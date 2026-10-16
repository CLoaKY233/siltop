//! `--demo`: a canned, looping 60 s session for README captures and
//! screenshots. Every value is a pure function of the session clock (no
//! hardware, no RNG), so the same second always paints the same screen. The
//! wall clock is read once, when the demo starts, only to date its processes
//! (`up 2d04` must count from now); nothing of this Mac is ever shown.
//!
//! The session, on an M5 Pro laptop running on battery:
//!
//! | seconds | what happens                                                 |
//! |---------|--------------------------------------------------------------|
//! | 0–8     | idle browsing                                                |
//! | 6–11    | `cargo` downloads crates (network burst)                     |
//! | 8–28    | build burst: every core busy, `target/` writes, heat builds  |
//! | 31–34   | a model loads from disk                                      |
//! | 32–46   | GPU inference spike: GPU and ANE busy, unified memory grows  |
//! | 46–60   | cool-down; fans spin back down                               |
//!
//! The battery drains throughout. The process table is a Mac's worth of
//! apps and daemons (Safari, Brave, Slack, Xcode, Zed, Ghostty, Ollama), run
//! from `.app` bundles so `a` groups them, started minutes to days ago; the
//! build's `cargo`, `rustc` and `ld` come and go with the build, and the
//! model runner with the inference. Every process action is refused in the
//! demo (`demo: process actions are off`), so no pid here reaches this Mac.

use std::time::{Duration, Instant};

use siltop_core::{
    Adapter, BatterySnapshot, ChargeState, Cluster, ClusterKind, CollectorId, CoreSample,
    CpuCluster, DiskDevice, DiskSnapshot, DiskVolume, FanMetric, FanSnapshot, GpuSnapshot, Health,
    IfaceRate, MemorySnapshot, NetworkSnapshot, PowerSnapshot, PowerSource, Pressure, Process,
    Reading, Result, Sampled, SensorsSnapshot, Snapshot, SocInfo, TempReading, Thermal, Throttle,
    TimeRemaining,
};

use crate::worker::Tick;

/// One loop of the canned session.
pub const SESSION: Duration = Duration::from_secs(60);

const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;
const P_CORES: u16 = 12;
const S_CORES: u16 = 6;
const P_MAX_MHZ: u32 = 4_128;
const S_MAX_MHZ: u32 = 4_608;
const GPU_MAX_MHZ: u32 = 1_620;
const MEM_TOTAL: u64 = 24 * GIB;
const FAN_MAX: [u16; 2] = [6_550, 7_150];
/// Unix time [`frame`] dates processes from: a fixed moment, so a frame is
/// the same every run. [`Demo`] uses the moment it started instead.
const DEMO_EPOCH: i64 = 1_790_000_000;

/// A [`Tick`] that replays the session, one frame per worker interval.
pub struct Demo {
    /// The worker's cadence, so the session plays in real time.
    step: std::cell::Cell<Duration>,
    elapsed: Duration,
    /// Unix seconds when the session started: process ages count from it.
    epoch: i64,
}

impl Demo {
    #[must_use]
    pub fn new(step: Duration) -> Self {
        Self {
            step: std::cell::Cell::new(step),
            elapsed: Duration::ZERO,
            epoch: unix_now(),
        }
    }
}

/// Wall-clock unix seconds, read once per session.
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(DEMO_EPOCH, |d| {
            i64::try_from(d.as_secs()).unwrap_or(DEMO_EPOCH)
        })
}

impl Tick for Demo {
    fn tick(&mut self) -> Result<Snapshot> {
        let snap = frame_at(self.elapsed, Instant::now(), self.epoch);
        self.elapsed += self.step.get();
        Ok(snap)
    }

    fn set_interval(&self, interval: Duration) {
        self.step.set(interval);
    }

    /// The canned battery is not this Mac's: the battery saver (auto)
    /// follows the real power source, never the script.
    fn on_battery(&self, _snap: &Snapshot) -> bool {
        false
    }
}

/// Another Mac for `fleet --demo`: its own chip, memory, fans and battery,
/// and its own point in the session, so no two cards read alike and the
/// grid shows a desktop without a battery and a fanless laptop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Profile {
    pub chip: &'static str,
    memory: u64,
    /// Share of the M5 Pro's CPU busy and power this Mac runs at.
    load: f32,
    fans: bool,
    battery: bool,
    /// How far into the session this Mac starts.
    offset: Duration,
}

/// The demo fleet's remotes, handed out in turn after the local M5 Pro.
pub const FLEET: [Profile; 3] = [
    Profile {
        chip: "Apple M3 Ultra",
        memory: 96 * GIB,
        load: 0.55,
        fans: true,
        battery: false,
        offset: Duration::from_secs(20),
    },
    Profile {
        chip: "Apple M4",
        memory: 16 * GIB,
        load: 0.8,
        fans: true,
        battery: false,
        offset: Duration::from_secs(36),
    },
    Profile {
        chip: "Apple M2",
        memory: 8 * GIB,
        load: 0.35,
        fans: false,
        battery: true,
        offset: Duration::from_secs(50),
    },
];

impl Profile {
    /// Recasts an M5 Pro frame as this Mac. Older chips have Performance
    /// and Efficiency cores, so the frame's Super and Performance clusters
    /// become Performance and Efficiency (at an efficiency clock).
    fn apply(&self, snap: &mut Snapshot) {
        snap.soc.name = String::from(self.chip);
        snap.soc.memory_bytes = self.memory;
        let share = |part: u64| {
            u64::try_from(u128::from(part) * u128::from(self.memory) / u128::from(MEM_TOTAL))
                .unwrap_or(u64::MAX)
        };
        let mem = &mut snap.memory;
        mem.used_bytes = share(mem.used_bytes);
        mem.wired_bytes = share(mem.wired_bytes);
        mem.compressed_bytes = share(mem.compressed_bytes);
        mem.cache_bytes = share(mem.cache_bytes);
        mem.total_bytes = self.memory;

        let older = |kind| match kind {
            ClusterKind::Super => ClusterKind::Performance,
            _ => ClusterKind::Efficiency,
        };
        let efficiency_mhz = |mhz: u32| mhz * 3 / 5;
        let cpu = &mut snap.cpu;
        cpu.scaled *= self.load;
        cpu.active *= self.load;
        cpu.watts = cpu.watts.map(|w| w * self.load);
        for core in &mut cpu.cores {
            core.kind = older(core.kind);
            core.scaled *= self.load;
            core.active *= self.load;
            if core.kind == ClusterKind::Efficiency {
                core.freq_mhz = core.freq_mhz.map(efficiency_mhz);
            }
        }
        for cluster in &mut cpu.clusters {
            cluster.kind = older(cluster.kind);
            cluster.scaled *= self.load;
            cluster.active *= self.load;
            cluster.name = String::from(match cluster.kind {
                ClusterKind::Efficiency => "ECPU",
                _ => "PCPU",
            });
            if cluster.kind == ClusterKind::Efficiency {
                cluster.freq_mhz = cluster.freq_mhz.map(efficiency_mhz);
                cluster.max_mhz = efficiency_mhz(cluster.max_mhz);
            }
        }
        let mut e = cpu.p_cluster.take();
        if let Some(e) = &mut e {
            e.kind = ClusterKind::Efficiency;
            e.freq_mhz = efficiency_mhz(e.freq_mhz);
        }
        cpu.e_cluster = e;
        cpu.p_cluster = cpu.s_cluster.take().map(|p| Cluster {
            kind: ClusterKind::Performance,
            ..p
        });
        for cluster in [&mut cpu.e_cluster, &mut cpu.p_cluster]
            .into_iter()
            .flatten()
        {
            cluster.scaled *= self.load;
            cluster.active *= self.load;
        }
        snap.soc.core_kinds = snap.soc.core_kinds.iter().copied().map(older).collect();
        snap.soc.e_cores = snap.soc.p_cores;
        snap.soc.p_cores = snap.soc.s_cores;
        snap.soc.s_cores = 0;

        let scale = |r: Option<Reading>| r.map(|r| Reading::measured(r.watts * self.load));
        snap.power.system = scale(snap.power.system);
        snap.power.cpu = scale(snap.power.cpu);
        snap.power.gpu = scale(snap.power.gpu);
        if !self.fans {
            snap.fans.fans.clear();
            snap.health.set(CollectorId::Fans, Health::Absent);
        }
        if !self.battery {
            snap.battery = BatterySnapshot::default();
            snap.health.set(CollectorId::Battery, Health::Absent);
        }
    }
}

/// A [`Tick`] that replays the session as one of the [`FLEET`] Macs.
pub struct ProfiledDemo {
    demo: Demo,
    profile: Profile,
}

impl ProfiledDemo {
    #[must_use]
    pub fn new(step: Duration, profile: Profile) -> Self {
        Self {
            demo: Demo {
                elapsed: profile.offset,
                ..Demo::new(step)
            },
            profile,
        }
    }
}

impl Tick for ProfiledDemo {
    fn tick(&mut self) -> Result<Snapshot> {
        let mut snap = self.demo.tick()?;
        self.profile.apply(&mut snap);
        Ok(snap)
    }
}

/// 0 → 1 over `ramp` seconds from `start`, 1 until `end`, then back to 0
/// over `ramp` seconds.
fn envelope(t: f32, start: f32, end: f32, ramp: f32) -> f32 {
    let rise = ((t - start) / ramp).clamp(0.0, 1.0);
    let fall = ((end + ramp - t) / ramp).clamp(0.0, 1.0);
    rise.min(fall)
}

/// Deterministic wobble in 0..=1: two incommensurate sines, so no two
/// seeds or seconds line up.
fn wobble(t: f32, seed: f32) -> f32 {
    let a = (t * 1.7 + seed * 2.3).sin();
    let b = (t * 0.41 + seed * 5.1).sin();
    (0.5 + 0.35 * a + 0.15 * b).clamp(0.0, 1.0)
}

/// Where the session is at second `t`.
#[derive(Debug, Clone, Copy)]
struct Phase {
    t: f32,
    /// Share of the loop elapsed, 0..1.
    progress: f32,
    build: f32,
    infer: f32,
    fetch: f32,
    load_model: f32,
    /// Lags the load: rises through the build, peaks in inference.
    heat: f32,
}

impl Phase {
    fn at(elapsed: Duration) -> Self {
        let t = (elapsed.as_secs_f64() % SESSION.as_secs_f64()) as f32;
        Self {
            t,
            progress: t / SESSION.as_secs_f32(),
            build: envelope(t, 8.0, 27.0, 1.5),
            infer: envelope(t, 32.0, 45.0, 1.0),
            fetch: envelope(t, 6.0, 10.0, 1.0),
            load_model: envelope(t, 31.0, 33.5, 0.5),
            heat: envelope(t, 10.0, 44.0, 9.0),
        }
    }
}

/// The session frame at `elapsed` (wrapped to [`SESSION`]), its processes
/// dated from a fixed moment.
#[must_use]
pub fn frame(elapsed: Duration, collected_at: Instant) -> Snapshot {
    frame_at(elapsed, collected_at, DEMO_EPOCH)
}

/// The session frame at `elapsed` for a session that started at unix
/// second `epoch`.
#[must_use]
pub fn frame_at(elapsed: Duration, collected_at: Instant, epoch: i64) -> Snapshot {
    let mut snap = machine(elapsed, collected_at);
    snap.processes = processes(Phase::at(elapsed), elapsed, epoch);
    snap
}

/// Everything in a frame but the process table.
fn machine(elapsed: Duration, collected_at: Instant) -> Snapshot {
    let ph = Phase::at(elapsed);
    let mut snap = Snapshot::empty();
    snap.collected_at = collected_at;
    snap.soc = soc();
    snap.cpu = cpu(ph);
    snap.gpu = Some(gpu(ph));
    snap.memory = memory(ph);
    snap.network = network(ph);
    snap.disk = disk(ph);
    snap.sensors = sensors(ph);
    snap.fans = fans(ph);
    snap.thermal = thermal(ph);
    snap.power = power(ph, &snap);
    snap.battery = battery(ph, &snap.power, &snap.sensors);
    snap.process_ncpu = u32::from(P_CORES + S_CORES);
    snap.unattributed_cpu = Some(18.0 + 30.0 * ph.build);
    snap.sampled = Sampled::default();
    snap.primed = true;
    snap.settled = true;
    for id in [
        CollectorId::Cpu,
        CollectorId::Memory,
        CollectorId::Gpu,
        CollectorId::Network,
        CollectorId::Disk,
        CollectorId::Processes,
        CollectorId::Sensors,
        CollectorId::Fans,
        CollectorId::Thermal,
        CollectorId::Battery,
    ] {
        snap.health.set(id, Health::Ok);
    }
    snap
}

fn soc() -> SocInfo {
    SocInfo {
        name: String::from("Apple M5 Pro"),
        e_cores: 0,
        p_cores: P_CORES as u8,
        s_cores: S_CORES as u8,
        gpu_cores: 20,
        memory_bytes: MEM_TOTAL,
        core_kinds: std::iter::repeat_n(ClusterKind::Performance, usize::from(P_CORES))
            .chain(std::iter::repeat_n(
                ClusterKind::Super,
                usize::from(S_CORES),
            ))
            .collect(),
    }
}

/// Busy share for one core: idle chatter, the build on every core, and a
/// little of the inference feeding on the Super cores.
fn core_busy(ph: Phase, kind: ClusterKind, index: u16) -> f32 {
    let seed = f32::from(index)
        + if kind == ClusterKind::Super {
            40.0
        } else {
            0.0
        };
    let idle = 0.03 + 0.14 * wobble(ph.t, seed);
    let build = ph.build * (0.82 + 0.18 * wobble(ph.t * 2.0, seed + 7.0));
    let feed = if kind == ClusterKind::Super {
        ph.infer * 0.35 * wobble(ph.t, seed + 3.0)
    } else {
        0.0
    };
    (idle + build + feed).clamp(0.0, 1.0)
}

fn cluster_mhz(ph: Phase, kind: ClusterKind) -> u32 {
    let (idle, max) = match kind {
        ClusterKind::Super => (1_260.0, S_MAX_MHZ as f32),
        _ => (1_020.0, P_MAX_MHZ as f32),
    };
    // Heat trims the top clock a little late in the build.
    let drive = (ph.build + 0.4 * ph.infer).min(1.0) * (1.0 - 0.06 * ph.heat);
    (idle + (max - idle) * drive) as u32
}

fn cpu(ph: Phase) -> siltop_core::CpuSnapshot {
    let mut cores = Vec::new();
    let mut clusters = Vec::new();
    let mut summary = |kind, count: u16, id: u8, name: &str, max: u32| {
        let mhz = cluster_mhz(ph, kind);
        let ratio = mhz as f32 / max as f32;
        let busy: Vec<f32> = (0..count).map(|i| core_busy(ph, kind, i)).collect();
        let active = busy.iter().sum::<f32>() / f32::from(count);
        cores.extend(busy.iter().zip(0..).map(|(&b, index)| CoreSample {
            kind,
            index,
            scaled: b * ratio,
            active: b,
            freq_mhz: Some(mhz),
        }));
        clusters.push(CpuCluster {
            id,
            kind,
            name: String::from(name),
            active,
            scaled: active * ratio,
            freq_mhz: Some(mhz),
            max_mhz: max,
        });
        Cluster {
            kind,
            scaled: active * ratio,
            active,
            freq_mhz: mhz,
        }
    };
    let p = summary(ClusterKind::Performance, P_CORES, 0, "PCPU", P_MAX_MHZ);
    let s = summary(ClusterKind::Super, S_CORES, 1, "SCPU", S_MAX_MHZ);
    let total = f32::from(P_CORES + S_CORES);
    let weight = |c: Cluster, n: u16| f32::from(n) / total * c.active;
    let active = weight(p, P_CORES) + weight(s, S_CORES);
    let scaled = (f32::from(P_CORES) * p.scaled + f32::from(S_CORES) * s.scaled) / total;
    siltop_core::CpuSnapshot {
        scaled,
        active,
        watts: Some(cpu_watts(ph)),
        freq_mhz: Some(s.freq_mhz.max(p.freq_mhz)),
        temp_c: Some(cpu_temp(ph)),
        e_cluster: None,
        p_cluster: Some(p),
        s_cluster: Some(s),
        cores,
        clusters,
    }
}

fn cpu_watts(ph: Phase) -> f32 {
    0.9 + 1.4 * wobble(ph.t, 1.0) + 31.0 * ph.build + 3.5 * ph.infer
}

fn gpu_watts(ph: Phase) -> f32 {
    0.15 + 0.4 * wobble(ph.t, 2.0) + 17.5 * ph.infer * (0.9 + 0.1 * wobble(ph.t * 3.0, 4.0))
}

fn cpu_temp(ph: Phase) -> f32 {
    41.0 + 3.0 * wobble(ph.t, 5.0) + 34.0 * ph.heat
}

fn gpu_busy(ph: Phase) -> f32 {
    (0.03 + 0.06 * wobble(ph.t, 6.0) + 0.88 * ph.infer).clamp(0.0, 1.0)
}

fn gpu(ph: Phase) -> GpuSnapshot {
    let busy = gpu_busy(ph);
    let mhz = 338.0 + (GPU_MAX_MHZ as f32 - 338.0) * (0.1 + 0.9 * ph.infer).min(1.0);
    let ane = 2.6 * ph.infer;
    GpuSnapshot {
        scaled: busy * mhz / GPU_MAX_MHZ as f32,
        active: busy,
        watts: Some(gpu_watts(ph)),
        freq_mhz: Some(mhz as u32),
        max_mhz: Some(GPU_MAX_MHZ),
        renderer_busy: Some(busy),
        ane_watts: (ane >= 0.05).then_some(ane),
        temp_c: Some(38.0 + 36.0 * ph.infer.max(ph.heat * 0.6)),
        mem_in_use_bytes: Some(900 * MIB + (8.4 * ph.infer * GIB as f32) as u64),
        mem_alloc_bytes: Some(1_400 * MIB + (9.1 * ph.infer * GIB as f32) as u64),
    }
}

fn memory(ph: Phase) -> MemorySnapshot {
    let gib = |x: f32| (x * GIB as f32) as u64;
    let used = gib(12.6 + 0.3 * wobble(ph.t, 8.0) + 2.8 * ph.build + 7.2 * ph.infer);
    let pressure = match used * 100 / MEM_TOTAL {
        0..=84 => Pressure::Nominal,
        85..=94 => Pressure::Warn,
        _ => Pressure::Critical,
    };
    MemorySnapshot {
        used_bytes: used,
        total_bytes: MEM_TOTAL,
        wired_bytes: gib(3.1 + 0.8 * ph.infer),
        compressed_bytes: gib(1.2 + 1.9 * ph.infer),
        cache_bytes: gib(6.4 - 2.6 * ph.infer),
        swap_used_bytes: 820 * MIB + gib(0.6 * ph.infer),
        swap_total_bytes: 2 * GIB,
        pressure,
        swap_in_bps: Some(0),
        swap_out_bps: Some((ph.infer * 3.0 * MIB as f32) as u64),
        free_percent: Some((100 - used * 100 / MEM_TOTAL) as u8),
    }
}

fn network(ph: Phase) -> NetworkSnapshot {
    let rx = (180_000.0 + 1_600_000.0 * wobble(ph.t, 9.0) + 310_000_000.0 * ph.fetch) as u64;
    let tx = (40_000.0 + 220_000.0 * wobble(ph.t, 10.0) + 6_000_000.0 * ph.fetch) as u64;
    let en0 = IfaceRate {
        name: String::from("en0"),
        rx_bps: rx,
        tx_bps: tx,
        rx_bytes: 48_211_904_512 + (ph.t * 9_000_000.0) as u64,
        tx_bytes: 6_902_118_400 + (ph.t * 700_000.0) as u64,
        link_bps: Some(1_200_000_000),
        primary: true,
    };
    NetworkSnapshot {
        iface: String::from("en0"),
        rx_bps: rx,
        tx_bps: tx,
        primary: Some(String::from("en0")),
        ifaces: vec![en0],
    }
}

fn disk(ph: Phase) -> DiskSnapshot {
    let read = (400_000.0
        + 2_000_000.0 * wobble(ph.t, 11.0)
        + 40_000_000.0 * ph.build * wobble(ph.t * 2.0, 12.0)
        + 2_600_000_000.0 * ph.load_model) as u64;
    let write = (150_000.0
        + 600_000.0 * wobble(ph.t, 13.0)
        + 380_000_000.0 * ph.build * wobble(ph.t * 1.5, 14.0)) as u64;
    DiskSnapshot {
        volumes: vec![DiskVolume {
            name: String::from("Macintosh HD"),
            mount: String::from("/"),
            used_bytes: 512_400_000_000 + (ph.build * 1_900_000_000.0) as u64,
            total_bytes: 994_662_584_320,
        }],
        read_bps: read,
        write_bps: write,
        devices: vec![DiskDevice {
            registry_id: 0x1_0000_04d2,
            read_bps: read,
            write_bps: write,
        }],
    }
}

fn sensors(ph: Phase) -> SensorsSnapshot {
    let cpu = cpu_temp(ph);
    let reading = |name: &str, celsius: f32| TempReading {
        name: name.to_owned(),
        celsius,
    };
    SensorsSnapshot {
        cpu_c: Some(cpu),
        gpu_c: Some(38.0 + 36.0 * ph.infer.max(ph.heat * 0.6)),
        hotspot_c: Some(cpu + 3.0 + 3.0 * ph.build),
        e_c: None,
        p_c: Some(cpu - 2.0),
        s_c: Some(cpu + 3.0 * ph.build),
        readings: vec![
            reading("nand", 36.0 + 9.0 * ph.heat),
            reading("battery", 31.0 + 4.0 * ph.heat),
        ],
    }
}

/// Apple Silicon fans stop below a threshold; they spin up with the heat.
fn fans(ph: Phase) -> FanSnapshot {
    FanSnapshot {
        fans: FAN_MAX
            .iter()
            .enumerate()
            .map(|(i, &max_rpm)| {
                let rpm = if ph.heat < 0.12 {
                    0
                } else {
                    (1_200.0 + (f32::from(max_rpm) * 0.72 - 1_200.0) * ph.heat) as u16
                };
                FanMetric {
                    name: format!("Fan {}", i + 1),
                    rpm,
                    max_rpm,
                }
            })
            .collect(),
    }
}

fn thermal(ph: Phase) -> Thermal {
    if ph.heat > 0.85 {
        Thermal::Fair
    } else {
        Thermal::Nominal
    }
}

fn power(ph: Phase, snap: &Snapshot) -> PowerSnapshot {
    let cpu = cpu_watts(ph);
    let gpu = gpu_watts(ph);
    let ane = snap.gpu.and_then(|g| g.ane_watts);
    let dram = 0.5 + 0.4 * wobble(ph.t, 15.0) + 1.8 * ph.build.max(ph.infer);
    let system = cpu + gpu + ane.unwrap_or(0.0) + dram + 4.2 + 0.8 * wobble(ph.t, 16.0);
    PowerSnapshot {
        system: Some(Reading::measured(system)),
        cpu: Some(Reading::measured(cpu)),
        gpu: Some(Reading::measured(gpu)),
        ane: ane.map(Reading::measured),
        dram: Some(Reading::measured(dram)),
        throttle: Throttle {
            gpu_cltm: Some(0.0),
            gpu_power_limited: Some(0.0),
            cpms_reduction: Some(if ph.heat > 0.9 { 0.06 } else { 0.0 }),
        },
    }
}

/// On battery the whole loop: 79 % down to 73 %, drain following the load.
fn battery(ph: Phase, power: &PowerSnapshot, sensors: &SensorsSnapshot) -> BatterySnapshot {
    let percent = 79 - (ph.progress * 6.0) as u8;
    let draw = power.system.map_or(8.0, |r| r.watts);
    let voltage_mv = 12_420 - u32::from(79 - percent) * 18;
    let remaining_wh = 72.4 * f32::from(percent) / 100.0;
    let hours = remaining_wh / draw.max(1.0);
    BatterySnapshot {
        present: true,
        source: PowerSource::Battery,
        percent: Some(percent),
        state: ChargeState::Discharging,
        external_connected: Some(false),
        is_charging: Some(false),
        fully_charged: Some(false),
        voltage_mv: Some(voltage_mv),
        amperage_ma: Some(-(draw * 1_000_000.0 / voltage_mv as f32) as i64),
        watts: Some(-draw),
        cycle_count: Some(214),
        design_cycle_count: Some(1_000),
        design_capacity_mah: Some(6_075),
        nominal_capacity_mah: Some(5_742),
        full_charge_capacity_mah: Some(5_690),
        health_pct: siltop_core::health_percent(5_742, 6_075),
        time_to_empty: Some(Duration::from_secs_f32(hours * 3_600.0)),
        time_to_full: None,
        time_remaining: TimeRemaining::Known(Duration::from_secs_f32(hours * 3_600.0)),
        adapter: None::<Adapter>,
        not_charging_reason: Some(0),
        thermally_limited: Some(0),
        low_power_mode: Some(false),
        temp_c: sensors
            .readings
            .iter()
            .find(|r| r.name == "battery")
            .map(|r| r.celsius),
        awake: Vec::new(),
    }
}

/// The frames that fill the graphs before the first live one: the
/// session's last `window` at `step`, oldest first, ending where
/// [`Demo`] starts (a loop boundary), so `siltop --demo` opens on full
/// graphs instead of two empty minutes.
#[must_use]
pub fn backfill(window: Duration, step: Duration) -> Vec<Snapshot> {
    let step_ms = step.as_millis().max(1);
    let n = window.as_millis() / step_ms;
    let loops = window.as_secs() / SESSION.as_secs() + 1;
    let end = SESSION * u32::try_from(loops).unwrap_or(u32::MAX);
    let collected_at = Instant::now();
    (1..=n)
        .rev()
        .filter_map(|back| {
            let back = Duration::from_millis(u64::try_from(back * step_ms).ok()?);
            // Only the graphs read these; the table shows the live frame.
            Some(machine(end.checked_sub(back)?, collected_at))
        })
        .collect()
}

/// What drives a row's CPU beyond its idle figure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Idle,
    Build,
    Infer,
    Browse,
}

/// When a row's process started.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Born {
    /// This long before the demo started, in seconds.
    Ago(i64),
    /// At second `.0` of every loop, living until `.1`: the build's
    /// processes and the model runner come and go with the story.
    Loop(f32, f32),
}

/// One canned process.
struct Row {
    pid: u32,
    ppid: u32,
    name: &'static str,
    user: &'static str,
    /// CPU % when nothing in the story drives it.
    idle: f32,
    mem_mib: u64,
    threads: u32,
    role: Role,
    /// The executable's path.
    path: &'static str,
    /// The arguments after the program name.
    args: &'static [&'static str],
    born: Born,
}

const MIN: i64 = 60;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 24 * HOUR;
/// How long the demo Mac has been up.
const BOOT: i64 = 6 * DAY + 4 * HOUR + 12 * MIN;

#[allow(clippy::too_many_arguments, clippy::similar_names)]
const fn row(
    pid: u32,
    ppid: u32,
    name: &'static str,
    user: &'static str,
    idle: f32,
    mem_mib: u64,
    threads: u32,
    role: Role,
    path: &'static str,
    args: &'static [&'static str],
    born: Born,
) -> Row {
    Row {
        pid,
        ppid,
        name,
        user,
        idle,
        mem_mib,
        threads,
        role,
        path,
        args,
        born,
    }
}

use Born::{Ago, Loop};
use Role::{Browse, Build, Idle, Infer};

const BRAVE_RENDERER: &str = "/Applications/Brave Browser.app/Contents/Frameworks/Brave Browser Framework.framework/Versions/131.1.73.104/Helpers/Brave Browser Helper (Renderer).app/Contents/MacOS/Brave Browser Helper (Renderer)";
const RUSTC: &str = "/Users/dev/.rustup/toolchains/stable-aarch64-apple-darwin/bin/rustc";
const WEBCONTENT: &str = "/Applications/Safari.app/Contents/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent";

/// A Mac's worth of processes. Paths are the real layout of each app, so
/// `a` folds helpers into their app and the dossier reads like a real one.
#[rustfmt::skip]
const ROWS: &[Row] = &[
    // the system
    row(1, 0, "launchd", "root", 0.3, 14, 4, Idle, "/sbin/launchd", &[], Ago(BOOT)),
    row(92, 1, "logd", "root", 0.6, 31, 6, Idle, "/usr/libexec/logd", &[], Ago(BOOT - 20)),
    row(96, 1, "powerd", "root", 0.1, 6, 3, Idle, "/System/Library/CoreServices/powerd.bundle/powerd", &[], Ago(BOOT - 20)),
    row(118, 1, "mds", "root", 0.7, 48, 8, Idle, "/System/Library/Frameworks/CoreServices.framework/Frameworks/Metadata.framework/Support/mds", &[], Ago(BOOT - 25)),
    row(140, 1, "bluetoothd", "root", 0.4, 12, 7, Idle, "/usr/sbin/bluetoothd", &[], Ago(BOOT - 25)),
    row(160, 1, "WindowServer", "_windowserver", 11.2, 412, 24, Browse, "/System/Library/PrivateFrameworks/SkyLight.framework/Resources/WindowServer", &["-daemon"], Ago(BOOT - 30)),
    row(212, 1, "trustd", "root", 0.2, 9, 3, Idle, "/usr/libexec/trustd", &[], Ago(BOOT - 30)),
    row(301, 1, "mds_stores", "root", 2.1, 96, 9, Idle, "/System/Library/Frameworks/CoreServices.framework/Frameworks/Metadata.framework/Support/mds_stores", &[], Ago(BOOT - 40)),
    row(517, 1, "coreaudiod", "_coreaudiod", 1.1, 22, 11, Idle, "/usr/sbin/coreaudiod", &[], Ago(BOOT - 40)),
    row(611, 1, "cfprefsd", "dev", 0.2, 7, 3, Idle, "/usr/sbin/cfprefsd", &["agent"], Ago(BOOT - 60)),
    row(640, 1, "Dock", "dev", 0.3, 88, 6, Idle, "/System/Library/CoreServices/Dock.app/Contents/MacOS/Dock", &[], Ago(BOOT - 62)),
    row(660, 1, "ControlCenter", "dev", 0.4, 64, 7, Idle, "/System/Library/CoreServices/ControlCenter.app/Contents/MacOS/ControlCenter", &[], Ago(BOOT - 62)),
    row(705, 1, "Spotlight", "dev", 0.2, 54, 6, Idle, "/System/Library/CoreServices/Spotlight.app/Contents/MacOS/Spotlight", &[], Ago(BOOT - 64)),
    row(742, 1, "backupd", "root", 0.9, 51, 7, Idle, "/System/Library/CoreServices/backupd.bundle/Contents/Resources/backupd", &[], Ago(38 * MIN)),
    row(1180, 1, "Finder", "dev", 0.4, 132, 9, Idle, "/System/Library/CoreServices/Finder.app/Contents/MacOS/Finder", &[], Ago(BOOT - 65)),
    // Safari and its web content
    row(2210, 1, "Safari", "dev", 4.8, 612, 38, Browse, "/Applications/Safari.app/Contents/MacOS/Safari", &[], Ago(2 * DAY + 4 * HOUR)),
    row(2219, 2210, "Safari Networking", "dev", 0.6, 44, 9, Browse, "/Applications/Safari.app/Contents/XPCServices/com.apple.WebKit.Networking.xpc/Contents/MacOS/com.apple.WebKit.Networking", &[], Ago(2 * DAY + 4 * HOUR)),
    row(2240, 2210, "Safari Web Content", "dev", 7.6, 402, 22, Browse, WEBCONTENT, &[], Ago(2 * HOUR + 11 * MIN)),
    row(2251, 2210, "Safari Web Content", "dev", 1.3, 188, 14, Browse, WEBCONTENT, &[], Ago(26 * MIN)),
    // Brave and its helpers
    row(2800, 1, "Brave Browser", "dev", 2.6, 498, 41, Browse, "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser", &[], Ago(DAY + 3 * HOUR)),
    row(2812, 2800, "Brave Browser Helper (GPU)", "dev", 3.4, 236, 18, Browse, "/Applications/Brave Browser.app/Contents/Frameworks/Brave Browser Framework.framework/Versions/131.1.73.104/Helpers/Brave Browser Helper (GPU).app/Contents/MacOS/Brave Browser Helper (GPU)", &["--type=gpu-process"], Ago(DAY + 3 * HOUR)),
    row(2815, 2800, "Brave Browser Helper", "dev", 0.5, 62, 9, Idle, "/Applications/Brave Browser.app/Contents/Frameworks/Brave Browser Framework.framework/Versions/131.1.73.104/Helpers/Brave Browser Helper.app/Contents/MacOS/Brave Browser Helper", &["--type=utility", "--utility-sub-type=network.mojom.NetworkService"], Ago(DAY + 3 * HOUR)),
    row(2830, 2800, "Brave Browser Helper (Renderer)", "dev", 4.1, 322, 19, Browse, BRAVE_RENDERER, &["--type=renderer", "--lang=en-US"], Ago(3 * HOUR + 40 * MIN)),
    row(2847, 2800, "Brave Browser Helper (Renderer)", "dev", 1.2, 174, 15, Browse, BRAVE_RENDERER, &["--type=renderer", "--lang=en-US"], Ago(42 * MIN)),
    row(2901, 2800, "Brave Browser Helper (Renderer)", "dev", 0.4, 96, 13, Idle, BRAVE_RENDERER, &["--type=renderer", "--lang=en-US"], Ago(14 * MIN)),
    // Slack
    row(4501, 1, "Slack", "dev", 1.4, 356, 29, Idle, "/Applications/Slack.app/Contents/MacOS/Slack", &[], Ago(3 * DAY + 2 * HOUR)),
    row(4512, 4501, "Slack Helper (GPU)", "dev", 0.8, 118, 12, Idle, "/Applications/Slack.app/Contents/Frameworks/Slack Helper (GPU).app/Contents/MacOS/Slack Helper (GPU)", &["--type=gpu-process"], Ago(3 * DAY + 2 * HOUR)),
    row(4520, 4501, "Slack Helper (Renderer)", "dev", 1.9, 284, 17, Idle, "/Applications/Slack.app/Contents/Frameworks/Slack Helper (Renderer).app/Contents/MacOS/Slack Helper (Renderer)", &["--type=renderer"], Ago(3 * DAY + 2 * HOUR)),
    // the terminal, its shells, and the build
    row(3102, 1, "ghostty", "dev", 2.2, 188, 12, Idle, "/Applications/Ghostty.app/Contents/MacOS/ghostty", &[], Ago(9 * HOUR + 5 * MIN)),
    row(3150, 3102, "zsh", "dev", 0.0, 4, 1, Idle, "/bin/zsh", &["-l"], Ago(9 * HOUR + 5 * MIN)),
    row(3160, 3102, "zsh", "dev", 0.0, 4, 1, Idle, "/bin/zsh", &["-l"], Ago(47 * MIN)),
    row(7002, 3160, "siltop", "dev", 0.4, 15, 4, Idle, "/opt/homebrew/bin/siltop", &["--demo"], Ago(0)),
    row(50_120, 3150, "cargo", "dev", 0.6, 72, 12, Build, "/Users/dev/.rustup/toolchains/stable-aarch64-apple-darwin/bin/cargo", &["build", "--release"], Loop(6.0, 28.5)),
    row(51_234, 50_120, "rustc", "dev", 0.0, 1_210, 18, Build, RUSTC, &["--crate-name", "siltop_metrics", "--edition=2024", "-C", "opt-level=3"], Loop(8.0, 27.5)),
    row(51_240, 50_120, "rustc", "dev", 0.0, 942, 16, Build, RUSTC, &["--crate-name", "siltop_ui", "--edition=2024", "-C", "opt-level=3"], Loop(9.5, 27.5)),
    row(51_266, 50_120, "rustc", "dev", 0.0, 610, 14, Build, RUSTC, &["--crate-name", "siltop_core", "--edition=2024", "-C", "opt-level=3"], Loop(11.0, 27.0)),
    row(51_281, 50_120, "ld", "dev", 0.0, 380, 4, Build, "/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin/ld", &["-o", "target/release/siltop"], Loop(23.0, 28.5)),
    // editors
    row(4012, 1, "zed", "dev", 3.1, 739, 38, Idle, "/Applications/Zed.app/Contents/MacOS/zed", &[], Ago(3 * HOUR + 18 * MIN)),
    row(4040, 4012, "rust-analyzer", "dev", 6.4, 2_540, 26, Build, "/Users/dev/.rustup/toolchains/stable-aarch64-apple-darwin/bin/rust-analyzer", &[], Ago(3 * HOUR + 17 * MIN)),
    row(8250, 1, "Xcode", "dev", 1.8, 1_440, 34, Idle, "/Applications/Xcode.app/Contents/MacOS/Xcode", &[], Ago(5 * HOUR + 2 * MIN)),
    row(8261, 8250, "SourceKitService", "dev", 0.3, 212, 9, Idle, "/Applications/Xcode.app/Contents/SharedFrameworks/SourceKit.framework/Versions/A/XPCServices/SourceKitService.xpc/Contents/MacOS/SourceKitService", &[], Ago(5 * HOUR + MIN)),
    // the model server and its runner
    row(5990, 1, "Ollama", "dev", 0.1, 46, 8, Idle, "/Applications/Ollama.app/Contents/MacOS/Ollama", &[], Ago(2 * DAY + 6 * HOUR)),
    row(6001, 5990, "ollama", "dev", 0.2, 58, 12, Idle, "/Applications/Ollama.app/Contents/Resources/ollama", &["serve"], Ago(2 * DAY + 6 * HOUR)),
    row(6020, 6001, "ollama-runner", "dev", 0.5, 1_100, 20, Infer, "/Applications/Ollama.app/Contents/Resources/ollama", &["runner", "--model", "qwen3-coder:30b", "--ctx-size", "8192"], Loop(31.0, 60.0)),
];

/// The account a demo user name stands for.
fn uid_of(user: &str) -> u32 {
    match user {
        "dev" => 501,
        "_windowserver" => 88,
        "_coreaudiod" => 202,
        _ => 0,
    }
}

/// The processes alive at this point of the session, dated from `epoch`.
fn processes(ph: Phase, elapsed: Duration, epoch: i64) -> Vec<Process> {
    // Seconds from the demo's start to the start of this loop.
    let loop_start = (elapsed.as_secs_f64() - f64::from(ph.t)) as i64;
    ROWS.iter()
        .enumerate()
        .filter(|(_, r)| match r.born {
            Ago(_) => true,
            Loop(from, to) => (from..to).contains(&ph.t),
        })
        .map(|(i, r)| {
            let w = wobble(ph.t, 20.0 + i as f32);
            let (cpu, extra_mib, gpu) = match r.role {
                Idle => (r.idle * (0.6 + 0.8 * w), 0.0, 0.0),
                Browse => (r.idle * (0.7 + 0.6 * w), 0.0, r.idle.min(4.0) * w),
                Build => {
                    let heavy = match r.name {
                        "rustc" => 520.0,
                        "ld" => 160.0,
                        _ => 45.0,
                    };
                    let cpu = r.idle + ph.build * heavy * (0.7 + 0.3 * w);
                    (cpu, ph.build * r.mem_mib as f32 * 0.8, 0.0)
                }
                Infer => {
                    let cpu = r.idle + ph.infer * 160.0 * (0.8 + 0.2 * w);
                    (cpu, ph.infer * 8_400.0, 88.0 * ph.infer)
                }
            };
            let start_unix = match r.born {
                Ago(secs) => epoch - secs,
                Loop(from, _) => epoch + loop_start + from as i64,
            };
            let mem = (r.mem_mib as f32 + extra_mib) as u64 * MIB;
            let argv0 = r.path.rsplit('/').next().unwrap_or(r.name);
            Process {
                pid: r.pid,
                ppid: r.ppid,
                name: String::from(r.name),
                cpu: Some(cpu),
                mem_bytes: Some(mem),
                threads: Some(r.threads),
                gpu,
                user: String::from(r.user),
                uid: uid_of(r.user),
                command: Some(String::from(r.path)),
                args: std::iter::once(argv0)
                    .chain(r.args.iter().copied())
                    .map(String::from)
                    .collect(),
                status: "",
                start_unix,
                cpu_spark: (0..16_u8)
                    .map(|k| cpu * (0.85 + 0.15 * wobble(ph.t - f32::from(k), 30.0)))
                    .rev()
                    .collect(),
                restricted: false,
                rss_bytes: Some(mem + mem / 8),
                energy_w: Some(cpu / 100.0 * 1.4),
                disk_read_bps: Some(0.0),
                disk_write_bps: Some(if r.role == Build {
                    f64::from(ph.build) * 24_000_000.0
                } else {
                    0.0
                }),
                wakeups_per_s: Some(12.0 + 40.0 * w),
                child_cpu_ns: None,
            }
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn at(secs: f32) -> Snapshot {
        frame(Duration::from_secs_f32(secs), Instant::now())
    }

    /// Regression (fleet demo identical): every demo remote is its own
    /// Mac: other chips and memory, a desktop without a battery, a fanless
    /// laptop, and a different point in the session from the local M5 Pro.
    #[test]
    fn fleet_demo_profiles_differ_from_the_m5_pro_and_each_other() {
        let step = Duration::from_secs(1);
        let local = Demo::new(step).tick().unwrap();
        let snaps: Vec<Snapshot> = FLEET
            .iter()
            .map(|p| ProfiledDemo::new(step, *p).tick().unwrap())
            .collect();
        let chips: Vec<&str> = snaps.iter().map(|s| s.soc.name.as_str()).collect();
        assert_eq!(chips, ["Apple M3 Ultra", "Apple M4", "Apple M2"]);
        for s in &snaps {
            let apart = (s.cpu.active - local.cpu.active).abs();
            assert!(apart > 1e-3, "{} busy like the M5 Pro", s.soc.name);
            assert!(s.memory.used_bytes < s.memory.total_bytes, "{}", s.soc.name);
            assert!(s.cpu.s_cluster.is_none(), "{}: no Super cores", s.soc.name);
            assert!(s.cpu.e_cluster.is_some() && s.cpu.p_cluster.is_some());
            let kinds: Vec<ClusterKind> = s.cpu.clusters.iter().map(|c| c.kind).collect();
            assert!(!kinds.contains(&ClusterKind::Super), "{kinds:?}");
            for c in &s.cpu.clusters {
                assert!(c.freq_mhz.is_some_and(|f| f <= c.max_mhz), "{c:?}");
            }
            assert_eq!(s.soc.core_kinds.len(), s.cpu.cores.len());
        }
        let [studio, mini, air] = &snaps[..] else {
            panic!("three profiles")
        };
        assert!(!studio.battery.present && !mini.battery.present, "desktops");
        assert!(air.battery.present, "a laptop");
        assert!(air.fans.fans.is_empty(), "the Air is fanless");
        assert!(!studio.fans.fans.is_empty());
        let used: Vec<u64> = snaps.iter().map(|s| s.memory.total_bytes).collect();
        assert_eq!(used, [96 * GIB, 16 * GIB, 8 * GIB]);
    }

    /// Every whole second (and some fractions) of two full loops.
    fn seconds() -> impl Iterator<Item = f32> {
        (0..240).map(|q| q as f32 / 2.0)
    }

    /// `cargo test --release -p siltop -- --ignored --nocapture backfill_cost`
    #[test]
    #[ignore = "timing probe: run in release"]
    fn backfill_cost() {
        let start = Instant::now();
        let frames = backfill(siltop_core::HISTORY_WINDOW, Duration::from_millis(250));
        println!("{} frames in {:?}", frames.len(), start.elapsed());
    }

    #[test]
    fn same_second_is_the_same_frame() {
        let when = Instant::now();
        for secs in seconds() {
            let d = Duration::from_secs_f32(secs);
            assert_eq!(frame(d, when), frame(d, when), "{secs}");
        }
    }

    #[test]
    fn session_loops_every_sixty_seconds() {
        let when = Instant::now();
        for secs in [0.0_f32, 7.5, 18.0, 38.0, 59.0] {
            let mut a = frame(Duration::from_secs_f32(secs), when);
            let mut b = frame(Duration::from_secs_f32(secs + 60.0), when);
            // Only the start of a process born this loop moves on a loop.
            for (p, q) in a.processes.iter_mut().zip(&mut b.processes) {
                let shift = q.start_unix - p.start_unix;
                assert!(shift == 0 || shift == 60, "{} {shift}", p.name);
                q.start_unix = p.start_unix;
            }
            assert_eq!(a, b, "{secs}");
        }
    }

    #[test]
    fn demo_tick_advances_by_its_step() {
        let mut demo = Demo::new(Duration::from_secs(1));
        let first = demo.tick().unwrap();
        let second = demo.tick().unwrap();
        let when = first.collected_at;
        assert_eq!(frame_at(Duration::ZERO, when, demo.epoch), first);
        assert_eq!(
            frame_at(Duration::from_secs(1), second.collected_at, demo.epoch),
            second,
            "second tick is t = 1 s"
        );
    }

    #[test]
    fn rv_demo_plays_at_the_worker_cadence() {
        let mut demo = Demo::new(Duration::from_millis(500));
        let _ = demo.tick().unwrap();
        demo.set_interval(Duration::from_secs(2));
        let _ = demo.tick().unwrap();
        let third = demo.tick().unwrap();
        assert_eq!(
            frame_at(Duration::from_millis(2500), third.collected_at, demo.epoch),
            third,
            "0.5 s, then 2 s per tick"
        );
    }

    #[test]
    fn covers_every_panel_with_fresh_data() {
        for secs in seconds() {
            let s = at(secs);
            assert!(s.primed && s.settled, "{secs}");
            for (id, health) in s.health.iter() {
                assert_eq!(health, &Health::Ok, "{id:?} at {secs}");
            }
            assert_eq!(s.health.iter().count(), 10, "every collector reports");
            assert_eq!(s.cpu.cores.len(), usize::from(P_CORES + S_CORES));
            assert_eq!(s.cpu.clusters.len(), 2);
            assert!(s.gpu.is_some_and(|g| g.watts.is_some()));
            assert!(s.memory.total_bytes > 0 && s.memory.used_bytes < s.memory.total_bytes);
            assert!(!s.network.ifaces.is_empty());
            assert!(s.disk.primary().is_some());
            assert!(s.sensors.is_present());
            assert_eq!(s.fans.fans.len(), 2);
            assert!(s.battery.present && s.battery.percent.is_some());
            assert!(s.power.system.is_some() && s.power.cpu.is_some());
            assert!(s.processes.len() >= 20);
            assert!(s.unattributed_cpu.is_some());
        }
    }

    #[test]
    fn soc_counts_match_core_kinds() {
        let s = at(0.0);
        let kinds = &s.soc.core_kinds;
        assert_eq!(kinds.len(), s.cpu.cores.len());
        for (mach, core) in s.cpu.cores.iter().enumerate() {
            assert_eq!(
                s.soc.role_for(mach, kinds.len()),
                Some((core.kind, core.index)),
                "core {mach}"
            );
        }
    }

    #[test]
    fn every_ratio_stays_in_range_and_finite() {
        for secs in seconds() {
            let s = at(secs);
            let unit = |x: f32| x.is_finite() && (0.0..=1.0).contains(&x);
            assert!(unit(s.cpu.scaled) && unit(s.cpu.active), "{secs}");
            for c in &s.cpu.cores {
                assert!(unit(c.scaled) && unit(c.active), "{c:?} at {secs}");
                assert!(c.scaled <= c.active + f32::EPSILON);
            }
            for c in &s.cpu.clusters {
                assert!(c.freq_mhz.is_some_and(|f| f <= c.max_mhz), "{c:?}");
            }
            let g = s.gpu.unwrap();
            assert!(unit(g.scaled) && unit(g.active));
            assert!(g.freq_mhz.unwrap() <= GPU_MAX_MHZ);
            for f in &s.fans.fans {
                assert!(f.rpm <= f.max_rpm, "{f:?}");
            }
            for p in &s.processes {
                let cpu = p.cpu.unwrap();
                assert!(cpu.is_finite() && cpu >= 0.0);
                assert!(cpu <= 100.0 * s.process_ncpu as f32, "{} {cpu}", p.name);
            }
        }
    }

    #[test]
    fn build_burst_loads_the_cpu_and_heats_the_chip() {
        let idle = at(3.0);
        let build = at(20.0);
        assert!(idle.cpu.active < 0.25, "{}", idle.cpu.active);
        assert!(build.cpu.active > 0.8, "{}", build.cpu.active);
        assert!(build.sensors.cpu_c > idle.sensors.cpu_c);
        let rustc = |s: &Snapshot| {
            s.processes
                .iter()
                .filter(|p| p.name == "rustc")
                .filter_map(|p| p.cpu)
                .sum::<f32>()
        };
        assert!(rustc(&build) > 800.0, "{}", rustc(&build));
        assert!(build.disk.write_bps > 50 * idle.disk.write_bps);
    }

    #[test]
    fn inference_spike_owns_the_gpu() {
        let before = at(20.0);
        let infer = at(39.0);
        let g = infer.gpu.unwrap();
        assert!(g.active > 0.85, "{}", g.active);
        assert!(g.watts.unwrap() > 15.0);
        assert!(g.ane_watts.is_some());
        assert!(
            before.gpu.unwrap().ane_watts.is_none(),
            "idle ANE is absent"
        );
        assert!(infer.memory.used_bytes > before.memory.used_bytes);
        let top = infer
            .processes
            .iter()
            .max_by(|a, b| a.gpu.total_cmp(&b.gpu))
            .unwrap();
        assert_eq!(top.name, "ollama-runner");
    }

    #[test]
    fn battery_drains_across_the_loop() {
        let start = at(0.0).battery;
        let end = at(59.5).battery;
        assert_eq!(start.state, ChargeState::Discharging);
        assert!(end.percent < start.percent);
        assert!(start.watts.unwrap() < 0.0, "discharging is negative");
        assert!(matches!(start.time_remaining, TimeRemaining::Known(_)));
        assert!(start.adapter.is_none());
    }

    #[test]
    fn thermal_rises_then_recovers() {
        assert_eq!(at(2.0).thermal, Thermal::Nominal);
        assert!(
            (20..45).any(|s| at(s as f32).thermal == Thermal::Fair),
            "heat peaks mid-session"
        );
        assert_eq!(at(58.0).thermal, Thermal::Nominal);
        assert!(at(1.0).fans.is_idle(), "fans rest before the build");
        assert!(!at(30.0).fans.is_idle());
    }

    /// Hot, but the kind of hot a `MacBook` Pro really runs at under a build.
    #[test]
    fn temperatures_stay_plausible() {
        for secs in seconds() {
            let s = at(secs);
            let hot = s.sensors.hotspot_c.unwrap();
            assert!((35.0..=90.0).contains(&hot), "{hot} at {secs}");
            assert!(s.sensors.cpu_c.unwrap() <= hot);
        }
    }

    #[test]
    fn network_and_model_load_bursts_land_in_their_windows() {
        assert!(at(8.0).network.rx_bps > 100 * at(50.0).network.rx_bps / 2);
        assert!(at(32.5).disk.read_bps > 1_000_000_000);
        assert!(at(50.0).disk.read_bps < 100_000_000);
    }

    #[test]
    fn no_absent_value_is_faked_as_zero() {
        for secs in seconds() {
            let s = at(secs);
            if let Some(ane) = s.gpu.and_then(|g| g.ane_watts) {
                assert!(ane >= 0.05, "{ane} at {secs}");
            }
            for r in [s.power.system, s.power.cpu, s.power.gpu, s.power.dram] {
                assert!(r.unwrap().watts >= 0.05);
            }
        }
    }

    /// Regression: every row read `up 0s` (a start in the far future) and
    /// sorting by start did nothing. Rows started in the past, from minutes
    /// to days before the demo, and the build's processes during the loop.
    #[test]
    fn processes_started_minutes_to_days_ago() {
        let epoch = 1_800_000_000;
        let procs = frame_at(Duration::from_secs(80), Instant::now(), epoch).processes;
        let now = epoch + 80;
        let ages: Vec<i64> = procs.iter().map(|p| now - p.start_unix).collect();
        assert!(ages.iter().all(|&a| a >= 0), "{ages:?}");
        assert!(ages.iter().any(|&a| a < 60), "something just started");
        assert!(ages.iter().any(|&a| (60..3600).contains(&a)), "minutes");
        assert!(ages.iter().any(|&a| (3600..86_400).contains(&a)), "hours");
        assert!(ages.iter().any(|&a| a >= 86_400), "days");
        let rustc = procs.iter().find(|p| p.name == "rustc").unwrap();
        assert_eq!(
            now - rustc.start_unix,
            12,
            "rustc started with this loop's build"
        );
    }

    /// Regression: `a` grouped nothing ("grouped by app" over a flat list).
    #[test]
    fn gui_apps_and_their_helpers_share_a_bundle() {
        let procs = at(20.0).processes;
        let bundle = |name: &str| -> Vec<&str> {
            procs
                .iter()
                .filter_map(|p| p.command.as_deref())
                .filter(|c| c.starts_with(&format!("/Applications/{name}.app/")))
                .collect()
        };
        for (app, members) in [
            ("Safari", 4),
            ("Brave Browser", 6),
            ("Slack", 3),
            ("Xcode", 2),
            ("Zed", 1),
            ("Ghostty", 1),
            ("Ollama", 2),
        ] {
            assert_eq!(bundle(app).len(), members, "{app}");
        }
        assert!((38..=46).contains(&procs.len()), "{}", procs.len());
        for p in &procs {
            assert!(!p.args.is_empty(), "{} has an argv", p.name);
            assert!(["dev", "root"].contains(&p.user.as_str()) || p.user.starts_with('_'));
        }
    }

    #[test]
    fn build_processes_come_and_go_with_the_build() {
        let names =
            |secs: f32| -> Vec<String> { at(secs).processes.into_iter().map(|p| p.name).collect() };
        assert!(!names(3.0).contains(&String::from("rustc")));
        assert!(names(20.0).contains(&String::from("rustc")));
        assert!(names(20.0).contains(&String::from("cargo")));
        assert!(!names(40.0).contains(&String::from("cargo")));
        assert!(names(40.0).contains(&String::from("ollama-runner")));
    }

    /// The demo's first live frame continues the backfill: the graphs open
    /// full, ending on the loop boundary where [`Demo`] starts.
    #[test]
    fn backfill_fills_the_window_and_ends_where_the_demo_starts() {
        let step = Duration::from_millis(500);
        let frames = backfill(Duration::from_mins(2), step);
        assert_eq!(frames.len(), 240);
        let last = frames.last().unwrap();
        let when = last.collected_at;
        let mut want = frame(SESSION.checked_sub(step).unwrap(), when);
        want.processes.clear();
        assert_eq!(*last, want, "the frame just before t = 0");
        assert!(frames.iter().all(|f| f.processes.is_empty()));
        assert!(
            frames.iter().any(|f| f.cpu.active > 0.8),
            "the window holds a build burst"
        );
    }

    #[test]
    fn process_tree_is_closed() {
        let procs = at(15.0).processes;
        for p in &procs {
            assert!(
                p.ppid == 0 || procs.iter().any(|q| q.pid == p.ppid),
                "{} has no parent {}",
                p.name,
                p.ppid
            );
        }
        let mut pids: Vec<u32> = procs.iter().map(|p| p.pid).collect();
        pids.sort_unstable();
        pids.dedup();
        assert_eq!(pids.len(), procs.len(), "pids are unique");
    }

    #[test]
    fn envelope_shape() {
        assert!(envelope(0.0, 5.0, 10.0, 1.0).abs() < f32::EPSILON);
        assert!((envelope(7.0, 5.0, 10.0, 1.0) - 1.0).abs() < f32::EPSILON);
        assert!((envelope(5.5, 5.0, 10.0, 1.0) - 0.5).abs() < 1e-6);
        assert!((envelope(10.5, 5.0, 10.0, 1.0) - 0.5).abs() < 1e-6);
        assert!(envelope(12.0, 5.0, 10.0, 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn wobble_stays_in_unit_range() {
        for i in 0..2_000 {
            let w = wobble(i as f32 * 0.37, (i % 50) as f32);
            assert!((0.0..=1.0).contains(&w), "{w}");
        }
    }
}
