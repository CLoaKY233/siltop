//! Headless output: `--json`, `--json-stream` and `--diagnose`.
//!
//! The JSON is built from DTOs mapped out of a [`Snapshot`], so the core types
//! stay free of serde and the schema is pinned here. Schema 1 only grows:
//! fields are added, never renamed or removed. See `docs/json-schema.md`.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use siltop_core::{
    ClusterKind, CollectorId, Error, Health, Result, Snapshot, Verdict, VerdictEngine, bytes_short,
};

/// A schema word: a `&'static str` when this Mac writes it, owned when
/// `siltop fleet` reads another Mac's stream back.
pub type Word = std::borrow::Cow<'static, str>;

use crate::cli::Cli;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    pub unix_ms: u64,
    pub interval_ms: u64,
    /// `false` on a sample whose rates were not measured over an interval.
    pub primed: bool,
    pub soc: SocDto,
    pub cpu: CpuDto,
    pub gpu: Option<GpuDto>,
    pub memory: MemoryDto,
    pub network: NetworkDto,
    pub disk: DiskDto,
    pub temp: TempDto,
    pub fans: Vec<FanDto>,
    pub thermal_pressure: Word,
    /// macmon-compatible power rails in watts; `null` when unmeasured.
    pub sys_power: Option<f32>,
    pub all_power: Option<f32>,
    pub cpu_power: Option<f32>,
    pub gpu_power: Option<f32>,
    pub ane_power: Option<f32>,
    pub ram_power: Option<f32>,
    pub gpu_freq_mhz: Option<u32>,
    pub battery: Option<BatteryDto>,
    pub processes: Vec<ProcessDto>,
    pub verdict: VerdictDto,
    pub health: BTreeMap<Word, Word>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SocDto {
    pub name: String,
    pub e_cores: u8,
    pub p_cores: u8,
    pub s_cores: u8,
    pub gpu_cores: u8,
    pub memory_bytes: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CpuDto {
    /// Share of time not idle, 0..=1.
    pub usage: f32,
    /// Usage weighted by frequency, 0..=1.
    pub scaled: f32,
    pub freq_mhz: Option<u32>,
    pub temp_c: Option<f32>,
    pub clusters: Vec<ClusterDto>,
    pub cores: Vec<CoreDto>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClusterDto {
    pub kind: Word,
    pub cores: u32,
    pub usage: f32,
    pub scaled: f32,
    pub freq_mhz: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CoreDto {
    pub kind: Word,
    pub index: u16,
    pub usage: f32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GpuDto {
    pub usage: f32,
    pub scaled: f32,
    pub freq_mhz: Option<u32>,
    pub temp_c: Option<f32>,
    pub mem_in_use_bytes: Option<u64>,
    pub mem_alloc_bytes: Option<u64>,
}

/// macmon names `ram_total`, `ram_usage`, `swap_total` and `swap_usage`.
#[derive(Debug, Serialize, Deserialize)]
pub struct MemoryDto {
    pub ram_total: u64,
    pub ram_usage: u64,
    pub swap_total: u64,
    pub swap_usage: u64,
    pub wired_bytes: u64,
    pub compressed_bytes: u64,
    pub cache_bytes: u64,
    pub pressure: Word,
    /// Bytes per second; `null` until two samples exist.
    pub swap_in_bps: Option<u64>,
    pub swap_out_bps: Option<u64>,
    /// `kern.memorystatus_level`, 0–100, as `memory_pressure` prints it.
    pub free_percent: Option<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NetworkDto {
    pub iface: String,
    pub rx_bps: u64,
    pub tx_bps: u64,
    /// Interface carrying the default route; `null` without one.
    pub primary: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DiskDto {
    pub read_bps: u64,
    pub write_bps: u64,
    pub volumes: Vec<VolumeDto>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VolumeDto {
    pub name: String,
    pub mount: String,
    pub used_bytes: u64,
    pub total_bytes: u64,
}

/// macmon names `cpu_temp_avg` and `gpu_temp_avg`.
#[derive(Debug, Serialize, Deserialize)]
pub struct TempDto {
    pub cpu_temp_avg: Option<f32>,
    pub gpu_temp_avg: Option<f32>,
    pub hotspot_c: Option<f32>,
    pub e_cluster_c: Option<f32>,
    pub p_cluster_c: Option<f32>,
    pub s_cluster_c: Option<f32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FanDto {
    pub name: String,
    pub rpm: u16,
    pub max_rpm: u16,
}

/// Filled once the battery collector lands; every field may be `null`.
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct BatteryDto {
    pub percent: Option<f32>,
    pub charging: Option<bool>,
    pub on_ac: Option<bool>,
    pub watts: Option<f32>,
    pub minutes_remaining: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProcessDto {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    pub user: String,
    /// Per-core percent: 100 is one fully busy core. Null when the kernel
    /// withholds it (another user's process).
    pub cpu_pct: Option<f32>,
    /// The same load in cores.
    pub cpu_cores: Option<f32>,
    /// Physical footprint, as Activity Monitor reports memory.
    pub mem_bytes: Option<u64>,
    pub threads: Option<u32>,
    pub energy_w: Option<f32>,
    pub restricted: bool,
    pub status: Word,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VerdictDto {
    pub kind: Word,
    pub headline: String,
    pub evidence: Vec<String>,
}

const fn kind_word(kind: ClusterKind) -> &'static str {
    kind.word()
}

const fn collector_word(id: CollectorId) -> &'static str {
    match id {
        CollectorId::Cpu => "cpu",
        CollectorId::Memory => "memory",
        CollectorId::Gpu => "gpu",
        CollectorId::Network => "network",
        CollectorId::Disk => "disk",
        CollectorId::Processes => "processes",
        CollectorId::Sensors => "sensors",
        CollectorId::Fans => "fans",
        CollectorId::Thermal => "thermal",
        CollectorId::Battery => "battery",
    }
}

const fn health_word(health: &Health) -> &'static str {
    match health {
        Health::Ok => "ok",
        Health::Pending => "pending",
        Health::Stale(_) => "stale",
        Health::Failed(_) => "failed",
        Health::Absent => "absent",
    }
}

/// Non-finite floats become `null`, never a bogus number.
/// A process's per-core CPU percent, if the kernel shared a finite one.
fn proc_cpu(process: &siltop_core::Process) -> Option<f32> {
    process.cpu.and_then(num)
}

fn num(value: f32) -> Option<f32> {
    value.is_finite().then_some(value)
}

fn opt(value: Option<f32>) -> Option<f32> {
    value.and_then(num)
}

/// Everything a report needs besides the snapshot.
#[derive(Debug, Clone, Copy)]
pub struct Context {
    pub unix_ms: u64,
    pub interval: Duration,
    pub procs: usize,
}

/// [`siltop_core::into_printable`] for a schema word.
fn printable_word(word: Word) -> Word {
    match siltop_core::printable(&word) {
        std::borrow::Cow::Borrowed(_) => word,
        std::borrow::Cow::Owned(clean) => Word::Owned(clean),
    }
}

impl Report {
    /// Every string made [`siltop_core::printable`]. A report read back
    /// from another Mac (`siltop fleet`) is untrusted text: its process
    /// names, chip name or verdict could carry escape sequences.
    #[must_use]
    pub fn into_printable(mut self) -> Self {
        use siltop_core::into_printable as clean;
        let take = std::mem::take::<String>;
        self.soc.name = clean(take(&mut self.soc.name));
        self.thermal_pressure = printable_word(std::mem::take(&mut self.thermal_pressure));
        for cluster in &mut self.cpu.clusters {
            cluster.kind = printable_word(std::mem::take(&mut cluster.kind));
        }
        for core in &mut self.cpu.cores {
            core.kind = printable_word(std::mem::take(&mut core.kind));
        }
        self.memory.pressure = printable_word(std::mem::take(&mut self.memory.pressure));
        self.network.iface = clean(take(&mut self.network.iface));
        self.network.primary = self.network.primary.take().map(clean);
        for volume in &mut self.disk.volumes {
            volume.name = clean(take(&mut volume.name));
            volume.mount = clean(take(&mut volume.mount));
        }
        for fan in &mut self.fans {
            fan.name = clean(take(&mut fan.name));
        }
        for process in &mut self.processes {
            process.name = clean(take(&mut process.name));
            process.user = clean(take(&mut process.user));
            process.status = printable_word(std::mem::take(&mut process.status));
        }
        self.verdict.kind = printable_word(std::mem::take(&mut self.verdict.kind));
        self.verdict.headline = clean(take(&mut self.verdict.headline));
        self.verdict.evidence = std::mem::take(&mut self.verdict.evidence)
            .into_iter()
            .map(clean)
            .collect();
        self.health = std::mem::take(&mut self.health)
            .into_iter()
            .map(|(k, v)| (printable_word(k), printable_word(v)))
            .collect();
        self
    }

    #[must_use]
    #[allow(clippy::too_many_lines)] // one flat field-by-field mapping
    pub fn from_snapshot(snap: &Snapshot, verdict: &Verdict, ctx: Context) -> Self {
        let cpu = &snap.cpu;
        let clusters = [cpu.e_cluster, cpu.p_cluster, cpu.s_cluster]
            .into_iter()
            .flatten()
            .map(|cluster| ClusterDto {
                kind: kind_word(cluster.kind).into(),
                cores: cpu.cores.iter().filter(|c| c.kind == cluster.kind).count() as u32,
                usage: num(cluster.active).unwrap_or(0.0),
                scaled: num(cluster.scaled).unwrap_or(0.0),
                freq_mhz: (cluster.freq_mhz > 0).then_some(cluster.freq_mhz),
            })
            .collect();
        let cores = cpu
            .cores
            .iter()
            .map(|core| CoreDto {
                kind: kind_word(core.kind).into(),
                index: core.index,
                usage: num(core.active).unwrap_or(0.0),
            })
            .collect();
        let gpu = snap.gpu.map(|gpu| GpuDto {
            usage: num(gpu.active).unwrap_or(0.0),
            scaled: num(gpu.scaled).unwrap_or(0.0),
            freq_mhz: gpu.freq_mhz.filter(|mhz| *mhz > 0),
            temp_c: opt(gpu.temp_c),
            mem_in_use_bytes: gpu.mem_in_use_bytes,
            mem_alloc_bytes: gpu.mem_alloc_bytes,
        });
        let mut ranked: Vec<_> = snap.processes.iter().collect();
        ranked.sort_by(|a, b| {
            let (a, b) = (proc_cpu(a).unwrap_or(-1.0), proc_cpu(b).unwrap_or(-1.0));
            b.total_cmp(&a)
        });
        let processes = ranked
            .into_iter()
            .take(ctx.procs)
            .map(|p| {
                let pct = proc_cpu(p);
                ProcessDto {
                    pid: p.pid,
                    ppid: p.ppid,
                    name: p.name.clone(),
                    user: p.user.clone(),
                    cpu_pct: pct,
                    cpu_cores: pct.map(|pct| pct / 100.0),
                    mem_bytes: p.mem_bytes,
                    threads: p.threads,
                    energy_w: p.energy_w.and_then(num),
                    restricted: p.restricted,
                    status: p.status.into(),
                }
            })
            .collect();
        let rail = |r: Option<siltop_core::Reading>| r.and_then(siltop_core::Reading::usable);
        let power = &snap.power;
        let cpu_power = opt(cpu.watts).or(rail(power.cpu));
        let gpu_power = opt(snap.gpu.and_then(|g| g.watts)).or(rail(power.gpu));
        let ane_power = opt(snap.gpu.and_then(|g| g.ane_watts)).or(rail(power.ane));
        let battery = snap.battery.present.then(|| BatteryDto {
            percent: snap.battery.percent.map(f32::from),
            charging: snap.battery.is_charging,
            on_ac: snap.battery.external_connected,
            watts: opt(snap.battery.watts),
            minutes_remaining: snap
                .battery
                .time_to_empty
                .and_then(|d| u32::try_from(d.as_secs() / 60).ok()),
        });
        Self {
            schema_version: SCHEMA_VERSION,
            unix_ms: ctx.unix_ms,
            interval_ms: u64::try_from(ctx.interval.as_millis()).unwrap_or(u64::MAX),
            primed: snap.primed,
            soc: SocDto {
                name: snap.soc.name.clone(),
                e_cores: snap.soc.e_cores,
                p_cores: snap.soc.p_cores,
                s_cores: snap.soc.s_cores,
                gpu_cores: snap.soc.gpu_cores,
                memory_bytes: snap.soc.memory_bytes,
            },
            cpu: CpuDto {
                usage: num(cpu.active).unwrap_or(0.0),
                scaled: num(cpu.scaled).unwrap_or(0.0),
                freq_mhz: cpu.freq_mhz.filter(|mhz| *mhz > 0),
                temp_c: opt(cpu.temp_c),
                clusters,
                cores,
            },
            gpu,
            memory: MemoryDto {
                ram_total: snap.memory.total_bytes,
                ram_usage: snap.memory.used_bytes,
                swap_total: snap.memory.swap_total_bytes,
                swap_usage: snap.memory.swap_used_bytes,
                wired_bytes: snap.memory.wired_bytes,
                compressed_bytes: snap.memory.compressed_bytes,
                cache_bytes: snap.memory.cache_bytes,
                pressure: snap.memory.pressure.label().into(),
                swap_in_bps: snap.memory.swap_in_bps,
                swap_out_bps: snap.memory.swap_out_bps,
                free_percent: snap.memory.free_percent,
            },
            network: NetworkDto {
                iface: snap.network.iface.clone(),
                rx_bps: snap.network.rx_bps,
                tx_bps: snap.network.tx_bps,
                primary: snap.network.primary.clone(),
            },
            disk: DiskDto {
                read_bps: snap.disk.read_bps,
                write_bps: snap.disk.write_bps,
                volumes: snap
                    .disk
                    .volumes
                    .iter()
                    .map(|v| VolumeDto {
                        name: v.name.clone(),
                        mount: v.mount.clone(),
                        used_bytes: v.used_bytes,
                        total_bytes: v.total_bytes,
                    })
                    .collect(),
            },
            temp: TempDto {
                cpu_temp_avg: opt(cpu.temp_c.or_else(|| snap.sensors.best_cpu_c())),
                gpu_temp_avg: opt(snap
                    .gpu
                    .and_then(|g| g.temp_c)
                    .or_else(|| snap.sensors.named_gpu_c())),
                hotspot_c: opt(snap.sensors.hotspot_c),
                e_cluster_c: opt(snap.sensors.e_c),
                p_cluster_c: opt(snap.sensors.p_c),
                s_cluster_c: opt(snap.sensors.s_c),
            },
            fans: snap
                .fans
                .fans
                .iter()
                .map(|f| FanDto {
                    name: f.name.clone(),
                    rpm: f.rpm,
                    max_rpm: f.max_rpm,
                })
                .collect(),
            thermal_pressure: snap.thermal.name().into(),
            // `all_power` sums the known package rails.
            sys_power: rail(power.system),
            all_power: match (cpu_power, gpu_power, ane_power) {
                (None, None, None) => None,
                (c, g, a) => Some(c.unwrap_or(0.0) + g.unwrap_or(0.0) + a.unwrap_or(0.0)),
            },
            cpu_power,
            gpu_power,
            ane_power,
            ram_power: rail(power.dram),
            gpu_freq_mhz: snap.gpu.and_then(|g| g.freq_mhz).filter(|mhz| *mhz > 0),
            battery,
            processes,
            verdict: VerdictDto {
                kind: verdict.kind.slug().into(),
                headline: verdict.headline.clone(),
                evidence: verdict.evidence.clone(),
            },
            health: snap
                .health
                .iter()
                .map(|(id, health)| (collector_word(id).into(), health_word(health).into()))
                .collect(),
        }
    }
}

pub(crate) fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn io_err(err: &io::Error) -> Error {
    Error::system(format!("write output: {err}"))
}

/// A write that failed because the reader went away (`| head`) ends the
/// stream quietly; anything else is an error.
fn finish_write(result: io::Result<()>) -> Result<bool> {
    match result {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(false),
        Err(err) => Err(io_err(&err)),
    }
}

/// A one-shot report waits at most this long in total (the interval
/// included) for slow collectors' first readings.
const SETTLE_BUDGET: Duration = Duration::from_millis(2_500);
/// Re-tick step while waiting for them.
const SETTLE_STEP: Duration = Duration::from_millis(100);

/// Re-ticks until `snap` is settled (cluster MHz, power and battery have had
/// a chance to arrive) or the budget, less the `spent` interval, runs out.
fn settle<T, S>(tick: &mut T, wait: &mut S, mut snap: Snapshot, spent: Duration) -> Result<Snapshot>
where
    T: FnMut() -> Result<Snapshot>,
    S: FnMut(Duration),
{
    let mut waited = spent;
    while !snap.settled && waited + SETTLE_STEP <= SETTLE_BUDGET {
        wait(SETTLE_STEP);
        waited += SETTLE_STEP;
        snap = tick()?;
    }
    Ok(snap)
}

/// `--json` (one object) or `--json-stream` (one line per tick, `count`
/// lines if set). The first tick only primes rates and is never printed.
pub fn run_json<T, W, S>(
    tick: &mut T,
    wait: &mut S,
    out: &mut W,
    cli: &Cli,
    stream: bool,
) -> Result<()>
where
    T: FnMut() -> Result<Snapshot>,
    S: FnMut(Duration),
    W: Write + ?Sized,
{
    let interval = cli.headless_interval();
    let limit = if stream { cli.count } else { Some(1) };
    let mut engine = VerdictEngine::default();
    let _prime = tick()?;
    let mut printed = 0_u64;
    while limit.is_none_or(|n| printed < n) {
        wait(interval);
        let mut snap = tick()?;
        if !stream {
            snap = settle(tick, wait, snap, interval)?;
        }
        let verdict = engine.update(&snap, Instant::now(), None).clone();
        let ctx = Context {
            unix_ms: unix_ms_now(),
            interval,
            procs: cli.procs,
        };
        let report = Report::from_snapshot(&snap, &verdict, ctx);
        let line = serde_json::to_string(&report)
            .map_err(|err| Error::system(format!("encode json: {err}")))?;
        let written = writeln!(out, "{line}").and_then(|()| out.flush());
        if !finish_write(written)? {
            return Ok(());
        }
        printed += 1;
    }
    Ok(())
}

/// The `--diagnose` text: the verdict, its evidence, then what was checked.
#[must_use]
pub fn diagnose_text(snap: &Snapshot, verdict: &Verdict) -> String {
    use std::fmt::Write as _;
    let mut text = format!("{}\n", verdict.headline);
    for line in &verdict.evidence {
        let _ = writeln!(text, "  - {line}");
    }
    text.push_str("checked:\n");
    let _ = write!(
        text,
        "  cpu      {:.0}% active",
        num(snap.cpu.active).unwrap_or(0.0) * 100.0
    );
    if let Some((top, cpu)) = snap
        .processes
        .iter()
        .filter_map(|p| proc_cpu(p).map(|cpu| (p, cpu)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
    {
        let _ = write!(text, ", top {} at {:.1} cores", top.name, cpu / 100.0);
    }
    text.push('\n');
    let _ = writeln!(
        text,
        "  memory   {} of {} used, {} swapped, pressure {}",
        bytes_short(snap.memory.used_bytes),
        bytes_short(snap.memory.total_bytes),
        bytes_short(snap.memory.swap_used_bytes),
        snap.memory.pressure.label()
    );
    let _ = write!(text, "  thermal  {}", snap.thermal.name());
    if let Some(temp) = opt(snap.cpu.temp_c) {
        let _ = write!(text, ", cpu {temp:.0} °C");
    }
    text.push('\n');
    text
}

/// `snap` without the process `pid`: `--diagnose` judges the Mac it was
/// run on, and its own sampling is never the answer to "why is this slow"
/// (it read "top siltop at 0.0 cores" on an idle Mac).
fn without_pid(mut snap: Snapshot, pid: u32) -> Snapshot {
    snap.processes.retain(|p| p.pid != pid);
    snap
}

/// `--diagnose`: prime, wait one interval, judge, print.
pub fn run_diagnose<T, W, S>(tick: &mut T, wait: &mut S, out: &mut W, cli: &Cli) -> Result<()>
where
    T: FnMut() -> Result<Snapshot>,
    S: FnMut(Duration),
    W: Write + ?Sized,
{
    let _prime = tick()?;
    let interval = cli.headless_interval();
    wait(interval);
    let snap = tick()?;
    let snap = without_pid(settle(tick, wait, snap, interval)?, std::process::id());
    let mut engine = VerdictEngine::default();
    let verdict = engine.update(&snap, Instant::now(), None).clone();
    let text = diagnose_text(&snap, &verdict);
    finish_write(out.write_all(text.as_bytes()).and_then(|()| out.flush())).map(|_| ())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
pub(crate) mod tests {
    use super::*;
    use serde_json::Value;
    use siltop_core::{
        Cluster, CoreSample, DiskVolume, FanMetric, GpuSnapshot, Process, Signals, VerdictKind,
        assess,
    };

    const GB: u64 = 1024 * 1024 * 1024;

    /// Numbers captured on this Mac17,8 (M5 Pro, 6S + 12P, 20-core GPU).
    #[allow(clippy::too_many_lines)]
    pub(crate) fn fixture() -> Snapshot {
        let mut snap = Snapshot::empty();
        snap.primed = true;
        snap.settled = true;
        snap.soc.name = String::from("Apple M5 Pro");
        snap.soc.p_cores = 12;
        snap.soc.s_cores = 6;
        snap.soc.gpu_cores = 20;
        snap.soc.memory_bytes = 24 * GB;
        snap.cpu.cores = (0..2)
            .map(|i| CoreSample {
                kind: ClusterKind::Super,
                index: i,
                scaled: 0.25,
                active: 0.5,
                freq_mhz: None,
            })
            .chain((0..2).map(|i| CoreSample {
                kind: ClusterKind::Performance,
                index: i,
                scaled: 0.125,
                active: 0.25,
                freq_mhz: None,
            }))
            .collect();
        snap.cpu.active = 0.375;
        snap.cpu.scaled = 0.1875;
        snap.cpu.temp_c = Some(45.5);
        snap.cpu.s_cluster = Some(Cluster {
            kind: ClusterKind::Super,
            scaled: 0.25,
            active: 0.5,
            freq_mhz: 0,
        });
        snap.cpu.p_cluster = Some(Cluster {
            kind: ClusterKind::Performance,
            scaled: 0.125,
            active: 0.25,
            freq_mhz: 2748,
        });
        snap.gpu = Some(GpuSnapshot {
            scaled: 0.0625,
            active: 0.125,
            watts: None,
            freq_mhz: Some(338),
            ane_watts: None,
            temp_c: Some(41.0),
            mem_in_use_bytes: Some(512 * 1024 * 1024),
            mem_alloc_bytes: None,
            ..GpuSnapshot::default()
        });
        snap.memory.total_bytes = 24 * GB;
        snap.memory.used_bytes = 14 * GB;
        snap.memory.wired_bytes = 3 * GB;
        snap.memory.compressed_bytes = GB;
        snap.memory.cache_bytes = 5 * GB;
        snap.memory.swap_used_bytes = 0;
        snap.memory.swap_total_bytes = 0;
        snap.network.iface = String::from("en0");
        snap.network.rx_bps = 12_000;
        snap.network.tx_bps = 3_400;
        snap.disk.read_bps = 1_048_576;
        snap.disk.write_bps = 0;
        snap.disk.volumes = vec![DiskVolume {
            name: String::from("Macintosh HD"),
            mount: String::from("/"),
            used_bytes: 400 * GB,
            total_bytes: 994 * GB,
        }];
        snap.fans.fans = vec![FanMetric {
            name: String::from("Left"),
            rpm: 0,
            max_rpm: 6550,
        }];
        snap.sensors.s_c = Some(46.0);
        snap.sensors.p_c = Some(45.0);
        snap.processes = vec![
            Process {
                pid: 612,
                ppid: 1,
                name: String::from("WindowServer"),
                user: String::from("_windowserver"),
                cpu: Some(2.5),
                mem_bytes: Some(300 * 1024 * 1024),
                threads: Some(21),
                status: "sleep",
                ..Process::default()
            },
            Process {
                pid: 9001,
                ppid: 1,
                name: String::from("Brave Browser"),
                user: String::from("cloaky"),
                cpu: Some(12.5),
                mem_bytes: Some(GB),
                threads: Some(48),
                status: "run",
                ..Process::default()
            },
            Process {
                pid: 77,
                ppid: 1,
                name: String::from("mds"),
                user: String::from("root"),
                cpu: Some(0.0),
                mem_bytes: Some(64 * 1024 * 1024),
                threads: Some(5),
                status: "sleep",
                ..Process::default()
            },
        ];
        snap.health.set(CollectorId::Cpu, Health::Ok);
        snap.health.set(CollectorId::Gpu, Health::Ok);
        snap.health.set(CollectorId::Fans, Health::Absent);
        snap.health
            .set(CollectorId::Sensors, Health::Stale(Duration::from_secs(3)));
        snap
    }

    fn ctx(procs: usize) -> Context {
        Context {
            unix_ms: 1_790_000_000_000,
            interval: Duration::from_secs(1),
            procs,
        }
    }

    fn report_json(snap: &Snapshot, procs: usize) -> Value {
        let verdict = assess(snap, &Signals::default());
        serde_json::to_value(Report::from_snapshot(snap, &verdict, ctx(procs))).unwrap()
    }

    /// Also the line `fleet` tests parse back.
    pub(crate) const GOLDEN: &str = r#"{"schema_version":1,"unix_ms":1790000000000,"interval_ms":1000,"primed":true,"soc":{"name":"Apple M5 Pro","e_cores":0,"p_cores":12,"s_cores":6,"gpu_cores":20,"memory_bytes":25769803776},"cpu":{"usage":0.375,"scaled":0.1875,"freq_mhz":null,"temp_c":45.5,"clusters":[{"kind":"performance","cores":2,"usage":0.25,"scaled":0.125,"freq_mhz":2748},{"kind":"super","cores":2,"usage":0.5,"scaled":0.25,"freq_mhz":null}],"cores":[{"kind":"super","index":0,"usage":0.5},{"kind":"super","index":1,"usage":0.5},{"kind":"performance","index":0,"usage":0.25},{"kind":"performance","index":1,"usage":0.25}]},"gpu":{"usage":0.125,"scaled":0.0625,"freq_mhz":338,"temp_c":41.0,"mem_in_use_bytes":536870912,"mem_alloc_bytes":null},"memory":{"ram_total":25769803776,"ram_usage":15032385536,"swap_total":0,"swap_usage":0,"wired_bytes":3221225472,"compressed_bytes":1073741824,"cache_bytes":5368709120,"pressure":"nominal","swap_in_bps":null,"swap_out_bps":null,"free_percent":null},"network":{"iface":"en0","rx_bps":12000,"tx_bps":3400,"primary":null},"disk":{"read_bps":1048576,"write_bps":0,"volumes":[{"name":"Macintosh HD","mount":"/","used_bytes":429496729600,"total_bytes":1067299373056}]},"temp":{"cpu_temp_avg":45.5,"gpu_temp_avg":41.0,"hotspot_c":null,"e_cluster_c":null,"p_cluster_c":45.0,"s_cluster_c":46.0},"fans":[{"name":"Left","rpm":0,"max_rpm":6550}],"thermal_pressure":"nominal","sys_power":null,"all_power":null,"cpu_power":null,"gpu_power":null,"ane_power":null,"ram_power":null,"gpu_freq_mhz":338,"battery":null,"processes":[{"pid":9001,"ppid":1,"name":"Brave Browser","user":"cloaky","cpu_pct":12.5,"cpu_cores":0.125,"mem_bytes":1073741824,"threads":48,"energy_w":null,"restricted":false,"status":"run"},{"pid":612,"ppid":1,"name":"WindowServer","user":"_windowserver","cpu_pct":2.5,"cpu_cores":0.025,"mem_bytes":314572800,"threads":21,"energy_w":null,"restricted":false,"status":"sleep"}],"verdict":{"kind":"normal","headline":"All normal","evidence":[]},"health":{"cpu":"ok","fans":"absent","gpu":"ok","sensors":"stale"}}"#;

    #[test]
    fn json_golden_from_fixture() {
        let verdict = assess(&fixture(), &Signals::default());
        let text =
            serde_json::to_string(&Report::from_snapshot(&fixture(), &verdict, ctx(2))).unwrap();
        assert_eq!(text, GOLDEN);
    }

    fn key_paths(value: &Value, prefix: &str, out: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    let path = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    out.push(path.clone());
                    key_paths(child, &path, out);
                }
            }
            Value::Array(items) => {
                if let Some(first) = items.first() {
                    key_paths(first, &format!("{prefix}[]"), out);
                }
            }
            _ => {}
        }
    }

    /// Schema 1 may only grow. A removed or renamed field fails here; a new
    /// field must be added to this list and to `docs/json-schema.md`.
    #[test]
    #[allow(clippy::too_many_lines)] // one flat list of every schema path
    fn schema_field_set_is_stable() {
        let mut paths = Vec::new();
        key_paths(&report_json(&fixture(), 10), "", &mut paths);
        // `health` keys depend on which collectors reported, and `battery`
        // is null until its collector lands: neither is pinned here.
        paths.retain(|p| !p.starts_with("health."));
        paths.sort();
        let expected = [
            "all_power",
            "ane_power",
            "battery",
            "cpu",
            "cpu.clusters",
            "cpu.clusters[].cores",
            "cpu.clusters[].freq_mhz",
            "cpu.clusters[].kind",
            "cpu.clusters[].scaled",
            "cpu.clusters[].usage",
            "cpu.cores",
            "cpu.cores[].index",
            "cpu.cores[].kind",
            "cpu.cores[].usage",
            "cpu.freq_mhz",
            "cpu.scaled",
            "cpu.temp_c",
            "cpu.usage",
            "cpu_power",
            "disk",
            "disk.read_bps",
            "disk.volumes",
            "disk.volumes[].mount",
            "disk.volumes[].name",
            "disk.volumes[].total_bytes",
            "disk.volumes[].used_bytes",
            "disk.write_bps",
            "fans",
            "fans[].max_rpm",
            "fans[].name",
            "fans[].rpm",
            "gpu",
            "gpu.freq_mhz",
            "gpu.mem_alloc_bytes",
            "gpu.mem_in_use_bytes",
            "gpu.scaled",
            "gpu.temp_c",
            "gpu.usage",
            "gpu_freq_mhz",
            "gpu_power",
            "health",
            "interval_ms",
            "memory",
            "memory.cache_bytes",
            "memory.compressed_bytes",
            "memory.free_percent",
            "memory.pressure",
            "memory.ram_total",
            "memory.ram_usage",
            "memory.swap_in_bps",
            "memory.swap_out_bps",
            "memory.swap_total",
            "memory.swap_usage",
            "memory.wired_bytes",
            "network",
            "network.iface",
            "network.primary",
            "network.rx_bps",
            "network.tx_bps",
            "primed",
            "processes",
            "processes[].cpu_cores",
            "processes[].cpu_pct",
            "processes[].energy_w",
            "processes[].mem_bytes",
            "processes[].name",
            "processes[].pid",
            "processes[].ppid",
            "processes[].restricted",
            "processes[].status",
            "processes[].threads",
            "processes[].user",
            "ram_power",
            "schema_version",
            "soc",
            "soc.e_cores",
            "soc.gpu_cores",
            "soc.memory_bytes",
            "soc.name",
            "soc.p_cores",
            "soc.s_cores",
            "sys_power",
            "temp",
            "temp.cpu_temp_avg",
            "temp.e_cluster_c",
            "temp.gpu_temp_avg",
            "temp.hotspot_c",
            "temp.p_cluster_c",
            "temp.s_cluster_c",
            "thermal_pressure",
            "unix_ms",
            "verdict",
            "verdict.evidence",
            "verdict.headline",
            "verdict.kind",
        ];
        assert_eq!(paths, expected);
    }

    #[test]
    fn battery_dto_fields_are_pinned() {
        let mut paths = Vec::new();
        key_paths(
            &serde_json::to_value(BatteryDto::default()).unwrap(),
            "",
            &mut paths,
        );
        assert_eq!(
            paths,
            ["charging", "minutes_remaining", "on_ac", "percent", "watts"]
        );
    }

    #[test]
    fn empty_snapshot_serializes_without_nan_or_panic() {
        let value = report_json(&Snapshot::empty(), 10);
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["primed"], false);
        assert!(value["gpu"].is_null());
        assert_eq!(value["cpu"]["clusters"], Value::Array(Vec::new()));
        assert_eq!(value["processes"], Value::Array(Vec::new()));
        assert_eq!(value["temp"]["cpu_temp_avg"], Value::Null);
    }

    #[test]
    fn non_finite_numbers_become_null_or_zero() {
        let mut snap = fixture();
        snap.cpu.active = f32::NAN;
        snap.cpu.temp_c = Some(f32::INFINITY);
        snap.processes[0].cpu = Some(f32::NAN);
        if let Some(gpu) = snap.gpu.as_mut() {
            gpu.watts = Some(f32::NEG_INFINITY);
        }
        let value = report_json(&snap, 10);
        assert_eq!(value["cpu"]["usage"], 0.0);
        assert!(value["cpu"]["temp_c"].is_null());
        assert!(value["gpu_power"].is_null());
        let text = serde_json::to_string(&value).unwrap();
        assert!(!text.contains("NaN") && !text.contains("inf"));
    }

    #[test]
    fn procs_limit_is_top_n_by_cpu() {
        for n in 0..5 {
            let value = report_json(&fixture(), n);
            let procs = value["processes"].as_array().unwrap();
            assert_eq!(procs.len(), n.min(3), "n={n}");
            let cpus: Vec<f64> = procs
                .iter()
                .map(|p| p["cpu_pct"].as_f64().unwrap())
                .collect();
            assert!(cpus.windows(2).all(|w| w[0] >= w[1]), "{cpus:?}");
        }
    }

    #[test]
    fn zero_mhz_means_unknown_not_zero() {
        let mut snap = fixture();
        if let Some(gpu) = snap.gpu.as_mut() {
            gpu.freq_mhz = Some(0);
        }
        snap.cpu.freq_mhz = Some(0);
        let value = report_json(&snap, 0);
        assert!(value["gpu_freq_mhz"].is_null());
        assert!(value["gpu"]["freq_mhz"].is_null());
        assert!(value["cpu"]["freq_mhz"].is_null());
    }

    #[test]
    fn all_power_sums_known_rails_only() {
        let mut snap = fixture();
        snap.cpu.watts = Some(3.5);
        if let Some(gpu) = snap.gpu.as_mut() {
            gpu.watts = Some(1.25);
        }
        let value = report_json(&snap, 0);
        assert_eq!(value["cpu_power"], 3.5);
        assert_eq!(value["gpu_power"], 1.25);
        assert_eq!(value["all_power"], 4.75);
        assert!(
            value["sys_power"].is_null(),
            "no system rail yet: never a guess"
        );
    }

    /// Regression: `--json` kept printing a held watts figure long after
    /// the energy counter stopped publishing, while the dashboard hid it.
    #[test]
    fn stale_held_power_is_null_in_the_json() {
        let mut snap = fixture();
        let stale = siltop_core::Reading {
            watts: 21.5,
            source: siltop_core::ReadingSource::Held {
                age: siltop_core::POWER_STALE_AFTER * 2,
            },
        };
        snap.power.system = Some(stale);
        snap.power.dram = Some(stale);
        let value = report_json(&snap, 1);
        assert!(value["sys_power"].is_null(), "{value}");
        assert!(value["ram_power"].is_null(), "{value}");
    }

    #[test]
    fn power_rails_and_battery_reach_the_json() {
        let mut snap = fixture();
        snap.power.system = Some(siltop_core::Reading::measured(21.5));
        snap.power.dram = Some(siltop_core::Reading::measured(0.75));
        snap.power.ane = Some(siltop_core::Reading::measured(0.25));
        snap.battery.present = true;
        snap.battery.percent = Some(80);
        snap.battery.external_connected = Some(true);
        snap.battery.time_to_empty = Some(Duration::from_mins(95));
        let value = report_json(&snap, 1);
        assert_eq!(value["sys_power"], 21.5);
        assert_eq!(value["ram_power"], 0.75);
        assert_eq!(value["ane_power"], 0.25, "energy rail fills in");
        assert_eq!(value["battery"]["percent"], 80.0);
        assert_eq!(value["battery"]["on_ac"], true);
        assert_eq!(value["battery"]["minutes_remaining"], 95);
    }

    #[test]
    fn a_desktop_reports_battery_null_not_zero() {
        let snap = fixture();
        assert!(!snap.battery.present);
        let value = report_json(&snap, 1);
        assert!(value["battery"].is_null());
        assert!(value["sys_power"].is_null(), "no PSTR: null, not 0");
    }

    #[test]
    fn unicode_names_survive_round_trip() {
        let mut snap = fixture();
        snap.processes[1].name = String::from("Café \"quoted\" \u{1F980}\n");
        let value = report_json(&snap, 1);
        assert_eq!(value["processes"][0]["name"], "Café \"quoted\" \u{1F980}\n");
    }

    fn run_stream(count: Option<u64>, stream: bool) -> (String, usize, usize) {
        let mut ticks = 0;
        let mut waits = 0;
        let mut out = Vec::new();
        let cli = Cli {
            count,
            ..Cli::default()
        };
        let mut tick = || {
            ticks += 1;
            Ok(fixture())
        };
        let mut wait = |d: Duration| {
            assert_eq!(d, Duration::from_secs(1));
            waits += 1;
        };
        run_json(&mut tick, &mut wait, &mut out, &cli, stream).unwrap();
        (String::from_utf8(out).unwrap(), ticks, waits)
    }

    #[test]
    fn json_prints_exactly_one_object_after_priming() {
        let (text, ticks, waits) = run_stream(None, false);
        assert_eq!(text.lines().count(), 1);
        assert_eq!((ticks, waits), (2, 1), "prime, wait one interval, sample");
        let value: Value = serde_json::from_str(text.trim_end()).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert!(value["cpu"]["clusters"].is_array());
    }

    /// Ticks settle on call `settle_at` (1-based, counting the prime).
    fn run_settling(settle_at: Option<usize>, stream: bool) -> (Vec<Duration>, usize, Value) {
        let mut ticks = 0;
        let mut waits = Vec::new();
        let mut out = Vec::new();
        let cli = Cli {
            count: Some(1),
            ..Cli::default()
        };
        let mut tick = || {
            ticks += 1;
            let mut snap = fixture();
            snap.settled = settle_at.is_some_and(|n| ticks >= n);
            snap.cpu.temp_c = Some(ticks as f32);
            Ok(snap)
        };
        let mut wait = |d: Duration| waits.push(d);
        run_json(&mut tick, &mut wait, &mut out, &cli, stream).unwrap();
        let value = serde_json::from_str(String::from_utf8(out).unwrap().trim_end()).unwrap();
        (waits, ticks, value)
    }

    #[test]
    fn json_waits_for_slow_collectors_then_prints_the_settled_tick() {
        let (waits, ticks, value) = run_settling(Some(5), false);
        assert_eq!(ticks, 5);
        assert_eq!(waits[0], Duration::from_secs(1));
        assert!(waits[1..].iter().all(|&d| d == SETTLE_STEP));
        assert_eq!(value["cpu"]["temp_c"], 5.0, "the settled tick is printed");
    }

    #[test]
    fn json_settle_wait_is_bounded_by_the_budget() {
        let (waits, _, _) = run_settling(None, false);
        let total: Duration = waits.iter().sum();
        assert!(total <= SETTLE_BUDGET, "{total:?}");
        assert!(
            total + SETTLE_STEP > SETTLE_BUDGET,
            "used the budget: {total:?}"
        );
    }

    #[test]
    fn json_already_settled_does_not_wait_extra() {
        let (waits, ticks, _) = run_settling(Some(1), false);
        assert_eq!((waits.len(), ticks), (1, 2));
    }

    #[test]
    fn json_stream_never_waits_to_settle() {
        let (waits, ticks, _) = run_settling(None, true);
        assert_eq!((waits.len(), ticks), (1, 2));
    }

    #[test]
    fn diagnose_waits_to_settle_too() {
        let mut ticks = 0;
        let mut waits = 0;
        let mut tick = || {
            ticks += 1;
            let mut snap = fixture();
            snap.settled = ticks >= 3;
            Ok(snap)
        };
        let mut wait = |_: Duration| waits += 1;
        let mut out = Vec::new();
        run_diagnose(&mut tick, &mut wait, &mut out, &Cli::default()).unwrap();
        assert_eq!((ticks, waits), (3, 2));
    }

    #[test]
    fn ndjson_count_prints_exactly_n_lines() {
        for n in [1_u64, 2, 3, 17] {
            let (text, ticks, waits) = run_stream(Some(n), true);
            assert_eq!(text.lines().count() as u64, n);
            assert_eq!(ticks as u64, n + 1);
            assert_eq!(waits as u64, n);
            assert!(text.ends_with('\n'));
            for line in text.lines() {
                let value: Value = serde_json::from_str(line).unwrap();
                assert_eq!(value["schema_version"], 1);
            }
        }
    }

    #[test]
    fn output_has_no_escape_bytes() {
        let (text, _, _) = run_stream(Some(3), true);
        assert!(!text.contains('\x1b'));
    }

    struct Pipe {
        accepted: usize,
        kind: io::ErrorKind,
    }

    impl Write for Pipe {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.accepted == 0 {
                return Err(io::Error::from(self.kind));
            }
            self.accepted -= 1;
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn broken_pipe_ends_stream_quietly() {
        let mut pipe = Pipe {
            accepted: 2,
            kind: io::ErrorKind::BrokenPipe,
        };
        let mut ticks = 0;
        let mut tick = || {
            ticks += 1;
            Ok(fixture())
        };
        run_json(&mut tick, &mut |_| {}, &mut pipe, &Cli::default(), true).unwrap();
        assert!(ticks < 10, "stream stopped once the reader left");
    }

    #[test]
    fn other_write_errors_are_reported() {
        let mut pipe = Pipe {
            accepted: 0,
            kind: io::ErrorKind::PermissionDenied,
        };
        let result = run_json(
            &mut || Ok(fixture()),
            &mut |_| {},
            &mut pipe,
            &Cli::default(),
            false,
        );
        assert!(result.is_err());
    }

    #[test]
    fn sampler_error_propagates_before_any_output() {
        let mut out = Vec::new();
        let result = run_json(
            &mut || Err(Error::system("no sampler")),
            &mut |_| {},
            &mut out,
            &Cli::default(),
            true,
        );
        assert!(result.is_err());
        assert!(out.is_empty());
    }

    #[test]
    fn diagnose_prints_verdict_then_evidence() {
        let mut snap = fixture();
        snap.processes[1].cpu = Some(200.0);
        let verdict = assess(&snap, &Signals::default());
        assert_eq!(verdict.kind, VerdictKind::Busy);
        let text = diagnose_text(&snap, &verdict);
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("Busy: Brave Browser using 2.0 cores"));
        assert!(text.contains("  - top process: Brave Browser (pid 9001) at 2.0 cores"));
        assert!(text.contains("checked:"));
        assert!(text.contains("pressure nominal"));
        assert!(!text.contains('\x1b'));
    }

    /// Regression: on an idle Mac `--diagnose` named itself, "top siltop
    /// at 0.0 cores". Its own pid never ranks.
    #[test]
    fn diagnose_never_names_itself() {
        let me = std::process::id();
        let mut tick = || {
            let mut snap = fixture();
            snap.processes[0].pid = me;
            snap.processes[0].name = String::from("siltop");
            snap.processes[0].cpu = Some(900.0);
            Ok(snap)
        };
        let mut out = Vec::new();
        run_diagnose(&mut tick, &mut |_| {}, &mut out, &Cli::default()).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains("siltop"), "{text}");
        assert!(!text.contains(&format!("pid {me}")), "{text}");
    }

    #[test]
    fn diagnose_runs_prime_wait_sample() {
        let mut out = Vec::new();
        let mut ticks = 0;
        let mut tick = || {
            ticks += 1;
            Ok(fixture())
        };
        run_diagnose(&mut tick, &mut |_| {}, &mut out, &Cli::default()).unwrap();
        assert_eq!(ticks, 2);
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("All normal\n"), "{text}");
    }
}
