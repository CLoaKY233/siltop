//! `IOReport`: CPU and GPU DVFS residency and GPU energy, without sudo.
//!
//! `libIOReport` is private (the SDK ships only a `.tbd`), so everything
//! degrades to "absent" when a call returns null. Two subscriptions run on
//! their own thread, because an `IOReport` subscription must stay on the
//! thread that made it:
//!
//! - GPU, once per snapshot interval (never faster than [`GPU_PERIOD`],
//!   never slower than [`GPU_MAX_PERIOD`]):
//!   "GPU Stats / GPU Performance States" (GPUPH), about 0.8–1 ms per
//!   sample on macOS 27. A faster cadence only threw windows away: each
//!   snapshot reads the newest one. While the GPU idles (GPUPH and the `IOAccelerator` hint both under
//!   [`GPU_IDLE_ACTIVE`]) it drops to [`GPU_IDLE_PERIOD`];
//! - PMGR, every [`PMGR_PERIOD`], or [`PMGR_STABLE_PERIOD`] while cluster
//!   clocks hold still ([`clocks_stable`]) and [`PMGR_DEEP_PERIOD`] once
//!   [`DEEP_AFTER`] windows in a row agreed, and at the next wake when the
//!   cheap tick sees the CPU load swing by [`LOAD_JUMP`]:
//!   "CPU Stats / CPU Core Performance States"
//!   merged with the roll-up "Energy Model" channels (`CPU Energy`,
//!   `GPU Energy`, `ANE<n>`, `DRAM<n>`). A PMGR sample costs a fixed
//!   6–8 ms of (mostly kernel) CPU however few channels it keeps, and one
//!   merged sample costs about the same as either half alone, so both ride
//!   one subscription. The delta gives per-core residency and GPU watts
//!   over the window; the raw sample's running totals and driver
//!   timestamps feed a [`power::EnergyBank`] (CPU, GPU, ANE and DRAM watts
//!   on any chip, held between the PMGR's batched publications, which come
//!   only every 2–108 s on macOS 27, so 2 s loses nothing).
//!
//! What the visible views read ([`Needs`], from the app through the worker)
//! gates both: parked GPU residency is not sampled; with CPU clocks and
//! watts parked, PMGR samples only until the clocks settle and again on a
//! load swing ([`pmgr_plan`]). Held clocks stand in; re-enabling a source
//! wakes the thread at once.
//!
//! `IOReportCopyAllChannels` is never called: it returns 12,043 channels.
//! The collectors read the newest [`FreqFrame`] without waiting.

use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::dvfs::{self, ClusterTable, CpuNode, Residency};
use crate::needs::Needs;
use crate::power::{self, EnergyUnit, RailReadings};

/// GPU residency cadence floor (each sample costs ~1 ms of CPU); the
/// thread follows the snapshot interval above it.
pub(crate) const GPU_PERIOD: Duration = Duration::from_millis(250);
/// GPU residency cadence ceiling: half of [`MAX_AGE`], so a reading is
/// always served at the next snapshot however long the interval (`-i 60s`
/// would otherwise leave only stale windows: no MHz, no GPU watts, and the
/// headline silently falling back to the `IOAccelerator` figure).
pub(crate) const GPU_MAX_PERIOD: Duration = Duration::from_secs(5);
/// The GPU cadence until the worker reports its interval (the default).
const GPU_DEFAULT_PERIOD: Duration = Duration::from_millis(500);
/// The GPU cadence while it is idle: a held idle reading is exact, and the
/// cheap `IOAccelerator` busy hint (read every snapshot) wakes it early.
pub(crate) const GPU_IDLE_PERIOD: Duration = Duration::from_secs(2);
/// Below this active share (GPUPH residency or the `IOAccelerator` hint)
/// the GPU counts as idle. The window compositor alone keeps GPUPH at
/// 2–5% on an idle desktop.
pub(crate) const GPU_IDLE_ACTIVE: f32 = 0.08;
/// CPU residency and energy cadence while clocks move (each sample costs
/// 6–8 ms of CPU).
pub(crate) const PMGR_PERIOD: Duration = Duration::from_secs(2);
/// The PMGR cadence while every cluster's clock and load hold still.
pub(crate) const PMGR_STABLE_PERIOD: Duration = Duration::from_secs(4);
/// The PMGR cadence once [`DEEP_AFTER`] windows in a row agreed (an idle
/// or pegged Mac), and for energy alone (the PMGR publishes rail totals
/// only every 2–108 s). A load swing still samples at the next wake.
pub(crate) const PMGR_DEEP_PERIOD: Duration = Duration::from_secs(8);
/// Agreeing PMGR windows in a row before the deep cadence.
pub(crate) const DEEP_AFTER: u32 = 3;
/// The first PMGR window, shorter so the first snapshot settles quickly.
pub(crate) const PMGR_FIRST_WINDOW: Duration = Duration::from_secs(1);
/// A CPU load swing this large (share of all cores, from the cheap tick)
/// since the last PMGR sample takes a new one at the next wake.
pub(crate) const LOAD_JUMP: f32 = 0.15;
/// Clusters whose MHz moved less than this share of their table maximum,
/// and whose active share moved less than this, count as stable.
pub(crate) const CLOCK_DRIFT: f32 = 0.10;
/// A cluster under this active share on both sides of a window counts as
/// stable whatever its clock does.
pub(crate) const QUIET_CLUSTER: f32 = 0.15;
/// An energy-rail reading counts as measured (not held) for this long.
pub(crate) const ENERGY_FRESH_FOR: Duration = Duration::from_secs(1);
/// Readings older than this are not used: the thread stalled. Longer than
/// [`PMGR_DEEP_PERIOD`], so a held reading never expires between samples.
pub(crate) const MAX_AGE: Duration = Duration::from_secs(10);
/// How long the thread sleeps when nothing on screen wants a source; a
/// [`FreqFeed::set_needs`] that re-enables one pokes it awake at once.
pub(crate) const PARKED_WAIT: Duration = Duration::from_secs(3600);
/// The thread never re-arms a wait shorter than this.
const WAKE_FLOOR: Duration = Duration::from_millis(1);

/// One channel of a sample delta, copied out of `CoreFoundation`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Channel {
    pub(crate) group: String,
    pub(crate) subgroup: String,
    pub(crate) name: String,
    pub(crate) unit: String,
    pub(crate) value: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    Simple(i64),
    States(Vec<(String, i64)>),
    Other,
}

/// One cluster's residency over the last CPU window.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ClusterFreq {
    pub(crate) name: String,
    pub(crate) id: Option<u8>,
    pub(crate) min_mhz: u32,
    pub(crate) max_mhz: u32,
    pub(crate) active: f32,
    pub(crate) scaled: f32,
    pub(crate) mhz: Option<u32>,
    /// Mach CPUs in this cluster, from the registry.
    pub(crate) cpus: Vec<usize>,
}

/// One core's residency over the last CPU window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CoreFreq {
    pub(crate) cpu: usize,
    pub(crate) cluster: usize,
    pub(crate) active: f32,
    pub(crate) mhz: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CpuFreq {
    pub(crate) at: Instant,
    pub(crate) clusters: Vec<ClusterFreq>,
    pub(crate) cores: Vec<CoreFreq>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GpuFreq {
    pub(crate) at: Instant,
    pub(crate) active: f32,
    pub(crate) scaled: f32,
    pub(crate) mhz: Option<u32>,
    pub(crate) max_mhz: u32,
    /// "GPU Energy" over the window; `None` when the channel is missing.
    pub(crate) watts: Option<f32>,
    /// Residency landed in a GPUPH state beyond the table (P14/P15).
    pub(crate) unmapped: bool,
}

/// Energy-counter watts from the last slow sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct EnergyFrame {
    pub(crate) at: Instant,
    pub(crate) rails: RailReadings,
}

/// The newest readings; any part may be missing.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct FreqFrame {
    pub(crate) cpu: Option<CpuFreq>,
    pub(crate) gpu: Option<GpuFreq>,
    /// "GPU Energy" averaged over the last CPU window.
    pub(crate) gpu_watts: Option<f32>,
    pub(crate) energy: Option<EnergyFrame>,
    /// The first CPU window has landed, or the thread found nothing to
    /// sample: waiting longer will not fill anything in.
    pub(crate) settled: bool,
}

impl FreqFrame {
    /// Sources just re-enabled serve their held reading as of `now`, so
    /// the first frame back shows the last value (not an absence) until
    /// the fresh sample lands, one thread wake later.
    pub(crate) fn hold_until_fresh(&mut self, added: Needs, now: Instant) {
        if added.contains(Needs::CPU_CLOCKS)
            && let Some(cpu) = self.cpu.as_mut()
        {
            cpu.at = now;
        }
        if added.contains(Needs::GPU_CLOCKS)
            && let Some(gpu) = self.gpu.as_mut()
        {
            gpu.at = now;
        }
    }
}

/// How long the `IOReport` thread sleeps: until the earlier gate among
/// the sources it runs (`None` = parked or absent), or [`PARKED_WAIT`]
/// when it runs none.
pub(crate) fn next_wait(gpu: Option<Duration>, pmgr: Option<Duration>) -> Duration {
    match (gpu, pmgr) {
        (Some(g), Some(p)) => g.min(p),
        (Some(one), None) | (None, Some(one)) => one,
        (None, None) => PARKED_WAIT,
    }
}

/// The GPU residency cadence for a snapshot interval: one window per
/// snapshot, within [`GPU_PERIOD`]..=[`GPU_MAX_PERIOD`].
pub(crate) fn gpu_period_for(interval: Duration) -> Duration {
    interval.clamp(GPU_PERIOD, GPU_MAX_PERIOD)
}

/// The GPU cadence: one window per snapshot while the GPU works, and
/// [`GPU_IDLE_PERIOD`] (or the interval, if longer) while it idles.
pub(crate) fn gpu_cadence(interval: Duration, busy: bool) -> Duration {
    let active = gpu_period_for(interval);
    if busy {
        active
    } else {
        active.max(GPU_IDLE_PERIOD)
    }
}

/// The GPU is busy when either the last GPUPH window or the cheap
/// `IOAccelerator` hint says so. Nothing known yet counts as busy, so the
/// first windows come at the snapshot cadence.
pub(crate) fn gpu_busy(residency: Option<f32>, hint: Option<f32>) -> bool {
    match (residency, hint) {
        (None, None) => true,
        (r, h) => [r, h].into_iter().flatten().any(|a| a >= GPU_IDLE_ACTIVE),
    }
}

/// Two CPU windows agree: the same clusters, and each one either quiet on
/// both sides (under [`QUIET_CLUSTER`] active: its clock is dither around
/// idle and moves nothing on screen) or with its clock within
/// [`CLOCK_DRIFT`] of its table maximum and its load within the same share.
/// A busy cluster that went idle or woke (MHz absent on one side) is a
/// change.
pub(crate) fn clocks_stable(prev: &CpuFreq, cur: &CpuFreq) -> bool {
    prev.clusters.len() == cur.clusters.len()
        && prev.clusters.iter().zip(&cur.clusters).all(|(a, b)| {
            if a.active < QUIET_CLUSTER && b.active < QUIET_CLUSTER {
                return true;
            }
            let span = a.max_mhz.max(b.max_mhz).max(1) as f32;
            let mhz = match (a.mhz, b.mhz) {
                (Some(x), Some(y)) => x.abs_diff(y) as f32 / span <= CLOCK_DRIFT,
                (None, None) => true,
                _ => false,
            };
            mhz && (a.active - b.active).abs() <= CLOCK_DRIFT
        })
}

/// The PMGR cadence after a window, from how many windows in a row agreed
/// ([`clocks_stable`]): slower the longer clocks hold still.
pub(crate) fn pmgr_cadence(streak: u32) -> Duration {
    match streak {
        0 => PMGR_PERIOD,
        n if n < DEEP_AFTER => PMGR_STABLE_PERIOD,
        _ => PMGR_DEEP_PERIOD,
    }
}

/// A planned PMGR period, never faster than snapshots are taken: at the
/// background agent's 5 s or a 10 s interval, a 2 s subscription read windows nobody
/// saw (over half the agent's CPU).
pub(crate) fn pmgr_paced(period: Duration, interval: Duration) -> Duration {
    period.max(interval)
}

/// The agreeing-window streak after one more PMGR sample.
pub(crate) fn next_streak(streak: u32, stable: bool) -> u32 {
    if stable { streak.saturating_add(1) } else { 0 }
}

/// The scheduled PMGR period for what the views need; `None` = no schedule
/// (a load swing, [`pmgr_urgent`], still samples at once).
///
/// - CPU clocks on screen: [`pmgr_cadence`] of the streak;
/// - watts alone: [`PMGR_DEEP_PERIOD`] (rail totals publish every 2–108 s);
/// - neither: sample until the clocks settle ([`DEEP_AFTER`] agreeing
///   windows), then stop. The held clocks still scale the live Mach busy
///   figure (the CPU history keeps recording), so they must be current: a
///   swing restarts the streak and sampling resumes until it settles again.
pub(crate) fn pmgr_plan(needs: Needs, streak: u32) -> Option<Duration> {
    if needs.contains(Needs::CPU_CLOCKS) {
        Some(pmgr_cadence(streak))
    } else if needs.contains(Needs::POWER) {
        Some(PMGR_DEEP_PERIOD)
    } else {
        (streak < DEEP_AFTER).then(|| pmgr_cadence(streak))
    }
}

/// A PMGR sample is worth taking now: the cheap tick saw the CPU load swing
/// by [`LOAD_JUMP`] since the last one, and that one is at least
/// [`PMGR_FIRST_WINDOW`] old (a window any shorter reads nothing useful).
pub(crate) fn pmgr_urgent(since_last: Duration, load_then: f32, load_now: f32) -> bool {
    since_last >= PMGR_FIRST_WINDOW && (load_now - load_then).abs() >= LOAD_JUMP
}

/// When a periodic sample is due. The first call is due at once (it primes
/// the subscription), the second after `first`, then every `period`. The
/// period may change between samples; the next due time follows it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Gate {
    period: Duration,
    first: Duration,
    last: Option<Instant>,
    /// Samples taken so far, saturating at 2 (primed, then regular).
    taken: u8,
    forced: bool,
}

impl Gate {
    pub(crate) const fn new(period: Duration, first: Duration) -> Self {
        Self {
            period,
            first,
            last: None,
            taken: 0,
            forced: false,
        }
    }

    /// When the next sample is due; `None` means now.
    fn next(&self) -> Option<Instant> {
        let last = self.last.filter(|_| !self.forced)?;
        Some(
            last + if self.taken < 2 {
                self.first
            } else {
                self.period
            },
        )
    }

    /// Whether a sample is due at `now`; if so, records it as taken.
    pub(crate) fn due(&mut self, now: Instant) -> bool {
        if self.next().is_some_and(|next| now < next) {
            return false;
        }
        self.last = Some(now);
        self.taken = self.taken.saturating_add(1).min(2);
        self.forced = false;
        true
    }

    /// How long until the next sample is due (zero when it already is).
    pub(crate) fn remaining(&self, now: Instant) -> Duration {
        self.next()
            .map_or(Duration::ZERO, |next| next.saturating_duration_since(now))
    }

    /// The regular period from here on; takes effect for the next sample.
    pub(crate) fn set_period(&mut self, period: Duration) {
        self.period = period;
    }

    /// Time since the last sample (`None` before the first).
    pub(crate) fn since_last(&self, now: Instant) -> Option<Duration> {
        self.last.map(|last| now.saturating_duration_since(last))
    }

    /// Makes the next [`due`](Self::due) fire at once, keeping the regular
    /// period after it (the subscription is already primed).
    pub(crate) fn restart(&mut self) {
        self.forced = true;
    }
}

pub(crate) fn energy_watts(channels: &[Channel], name: &str, dt_secs: f64) -> Option<f32> {
    if dt_secs <= 0.0 {
        return None;
    }
    channels.iter().find_map(|ch| match &ch.value {
        Value::Simple(v) if ch.group == "Energy Model" && ch.name == name => joules(*v, &ch.unit)
            .filter(|j| *j >= 0.0)
            .map(|j| (j / dt_secs) as f32),
        _ => None,
    })
}

/// CPU residency per cluster and per core from one delta. `None` when no
/// channel matched or the window failed the tick-count sanity check.
pub(crate) fn cpu_reading(
    channels: &[Channel],
    tables: &[ClusterTable],
    nodes: &[CpuNode],
    dt_secs: f64,
    at: Instant,
) -> Option<CpuFreq> {
    let mut sums = vec![Residency::default(); tables.len()];
    let mut cores = Vec::new();
    let mut matched = false;
    for ch in channels {
        let Value::States(states) = &ch.value else {
            continue;
        };
        if ch.subgroup != "CPU Core Performance States" {
            continue;
        }
        let Some((index, cpu)) = dvfs::map_core_channel(&ch.name, tables, nodes) else {
            continue;
        };
        let (Some(table), Some(sum)) = (tables.get(index), sums.get_mut(index)) else {
            continue;
        };
        let r = dvfs::residency(states, &table.mhz);
        if !dvfs::residency_is_sane(r.total, dt_secs) {
            return None;
        }
        matched = true;
        *sum = sum.add(r);
        if let Some(cpu) = cpu {
            cores.push(CoreFreq {
                cpu,
                cluster: index,
                active: r.active_ratio(),
                mhz: r.avg_mhz(),
            });
        }
    }
    if !matched {
        return None;
    }
    let clusters = tables
        .iter()
        .zip(&sums)
        .enumerate()
        .map(|(i, (table, sum))| ClusterFreq {
            name: table.name.clone(),
            id: table.id,
            min_mhz: table.mhz.first().copied().unwrap_or(0),
            max_mhz: table.max_mhz(),
            active: sum.active_ratio(),
            scaled: sum.scaled(&table.mhz),
            mhz: sum.avg_mhz(),
            cpus: cores
                .iter()
                .filter(|c| c.cluster == i)
                .map(|c| c.cpu)
                .collect(),
        })
        .collect();
    cores.sort_by_key(|c| c.cpu);
    Some(CpuFreq {
        at,
        clusters,
        cores,
    })
}

/// Joules from an energy channel's value and unit label; a negative delta
/// (a counter reset) is `None`.
pub(crate) fn joules(value: i64, unit: &str) -> Option<f64> {
    let raw = u64::try_from(value).ok()?;
    Some(EnergyUnit::parse(unit)?.joules(raw))
}

/// Channels the slow subscription keeps from "Energy Model": the roll-up
/// totals only, per die on an Ultra (see [`power::rollup_scoped`]).
pub(crate) fn keep_energy(name: &str) -> bool {
    power::rollup_scoped(name).is_some()
}

/// The driver timestamp of a raw `IOReport` element: the `u64` at byte 24
/// of `RawElements` (`provider_id`, `channel_id`, channel type, then the
/// timestamp in mach ticks). `None` for a short or zero stamp.
pub(crate) fn raw_timestamp(raw: &[u8]) -> Option<u64> {
    let bytes: [u8; 8] = raw.get(24..32)?.try_into().ok()?;
    Some(u64::from_ne_bytes(bytes)).filter(|&t| t > 0)
}

/// GPU residency (GPUPH, `P_k` → `table[k−1]`) and "GPU Energy" watts.
pub(crate) fn gpu_reading(
    channels: &[Channel],
    table: &[u32],
    dt_secs: f64,
    at: Instant,
) -> Option<GpuFreq> {
    if table.is_empty() || dt_secs <= 0.0 {
        return None;
    }
    let mut residency = None;
    for ch in channels {
        if let (Value::States(states), "GPUPH") = (&ch.value, ch.name.as_str()) {
            let r = dvfs::residency(states, table);
            if !dvfs::residency_is_sane(r.total, dt_secs) {
                return None;
            }
            residency = Some(r);
        }
    }
    let r = residency?;
    let watts = energy_watts(channels, "GPU Energy", dt_secs);
    Some(GpuFreq {
        at,
        active: r.active_ratio(),
        scaled: r.scaled(table),
        mhz: r.avg_mhz(),
        max_mhz: table.iter().copied().max().unwrap_or(0),
        watts,
        unmapped: r.unmapped > 0.0,
    })
}

/// Cheap per-snapshot readings the collectors hand the `IOReport` thread,
/// so it can sample slowly while nothing moves and at once when it does.
#[derive(Debug)]
pub(crate) struct Hints {
    /// The snapshot interval in nanoseconds.
    interval: AtomicU64,
    /// CPU active share of all cores (`f32` bits; NaN until known).
    cpu_load: AtomicU32,
    /// `IOAccelerator` device utilization (`f32` bits; NaN until known).
    gpu_busy: AtomicU32,
    /// [`Needs`] bits: what the visible views read.
    needs: AtomicU8,
}

impl Default for Hints {
    fn default() -> Self {
        Self {
            interval: AtomicU64::new(nanos(GPU_DEFAULT_PERIOD)),
            cpu_load: AtomicU32::new(f32::NAN.to_bits()),
            gpu_busy: AtomicU32::new(f32::NAN.to_bits()),
            needs: AtomicU8::new(Needs::ALL.bits()),
        }
    }
}

fn load_share(cell: &AtomicU32) -> Option<f32> {
    let value = f32::from_bits(cell.load(Ordering::Relaxed));
    value.is_finite().then_some(value)
}

impl Hints {
    pub(crate) fn interval(&self) -> Duration {
        Duration::from_nanos(self.interval.load(Ordering::Relaxed))
    }

    pub(crate) fn cpu_load(&self) -> Option<f32> {
        load_share(&self.cpu_load)
    }

    pub(crate) fn gpu_busy(&self) -> Option<f32> {
        load_share(&self.gpu_busy)
    }

    pub(crate) fn needs(&self) -> Needs {
        Needs::from_bits(self.needs.load(Ordering::Relaxed))
    }
}

/// The readings slot shared by the `IOReport` thread and the collectors.
#[derive(Clone, Default)]
pub(crate) struct FreqFeed {
    slot: Arc<Mutex<FreqFrame>>,
    thread: Option<Arc<crate::pacer::Background>>,
    /// Read by the thread every wake.
    hints: Arc<Hints>,
}

fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

impl FreqFeed {
    /// Starts the `IOReport` thread. The feed is empty (never zero) on other
    /// platforms or when `IOReport` or the DVFS tables are unavailable.
    pub(crate) fn spawn() -> Self {
        #[cfg(target_os = "macos")]
        {
            let slot = Arc::new(Mutex::new(FreqFrame::default()));
            let hints = Arc::new(Hints::default());
            let publisher = Arc::clone(&slot);
            let reader = Arc::clone(&hints);
            let thread = crate::pacer::Background::spawn("siltop-ioreport", move |pacer| {
                macos::run(&publisher, &reader, &pacer);
            })
            .map(Arc::new);
            Self {
                slot,
                thread,
                hints,
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self::default()
        }
    }

    /// A feed that only ever returns `frame` (tests).
    #[cfg(test)]
    pub(crate) fn fixed(frame: FreqFrame) -> Self {
        Self {
            slot: Arc::new(Mutex::new(frame)),
            ..Self::default()
        }
    }

    fn frame(&self) -> std::sync::MutexGuard<'_, FreqFrame> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Matches the GPU cadence to the snapshot interval; takes effect at
    /// the thread's next wake.
    pub(crate) fn set_interval(&self, interval: Duration) {
        self.hints
            .interval
            .store(nanos(interval), Ordering::Relaxed);
    }

    /// The CPU active share from this snapshot (cheap `host_processor_info`).
    pub(crate) fn note_cpu_load(&self, active: f32) {
        self.hints
            .cpu_load
            .store(active.to_bits(), Ordering::Relaxed);
    }

    /// The `IOAccelerator` utilization from this snapshot.
    pub(crate) fn note_gpu_busy(&self, active: f32) {
        self.hints
            .gpu_busy
            .store(active.to_bits(), Ordering::Relaxed);
    }

    /// The GPU cadence the thread would pick at its next wake, with no
    /// GPUPH window yet.
    #[cfg(test)]
    fn gpu_period(&self) -> Duration {
        gpu_cadence(self.hints.interval(), gpu_busy(None, self.hints.gpu_busy()))
    }

    /// What the visible views read. A parked source is not sampled at all
    /// and its last reading is held (never aged out) until it is needed
    /// again; re-enabling one wakes the thread at once.
    pub(crate) fn set_needs(&self, needs: Needs) {
        self.set_needs_at(needs, Instant::now());
    }

    pub(crate) fn set_needs_at(&self, needs: Needs, now: Instant) {
        let before = Needs::from_bits(self.hints.needs.swap(needs.bits(), Ordering::Relaxed));
        let added = needs.added_since(before);
        if added == Needs::NONE {
            return;
        }
        self.frame().hold_until_fresh(added, now);
        if let Some(thread) = &self.thread {
            thread.poke();
        }
    }

    /// Parks the `IOReport` thread while the app is frozen.
    pub(crate) fn set_paused(&self, paused: bool) {
        if let Some(thread) = &self.thread {
            thread.set_paused(paused);
        }
    }

    /// Nothing more is coming for the first snapshot: the first CPU window
    /// landed, or there is no `IOReport` thread to wait for.
    pub(crate) fn settled(&self) -> bool {
        self.thread.is_none() || self.frame().settled
    }

    /// Energy-rail watts aged to `now`; `None` when the last sweep is older
    /// than [`MAX_AGE`] (the thread stalled or is parked).
    pub(crate) fn energy(&self, now: Instant) -> Option<RailReadings> {
        let energy = self.frame().energy?;
        let elapsed = now.saturating_duration_since(energy.at);
        (elapsed <= MAX_AGE).then(|| energy.rails.aged(elapsed, ENERGY_FRESH_FOR))
    }

    /// The newest CPU reading, if one arrived in the last [`MAX_AGE`], or
    /// the held one while CPU clocks are parked.
    pub(crate) fn cpu(&self, now: Instant) -> Option<CpuFreq> {
        let held = !self.hints.needs().contains(Needs::CPU_CLOCKS);
        let frame = self.frame();
        frame
            .cpu
            .clone()
            .filter(|c| held || now.saturating_duration_since(c.at) <= MAX_AGE)
    }

    /// The newest GPU reading, if one arrived in the last [`MAX_AGE`], or
    /// the held one while GPU clocks are parked.
    pub(crate) fn gpu(&self, now: Instant) -> Option<GpuFreq> {
        let held = !self.hints.needs().contains(Needs::GPU_CLOCKS);
        let frame = self.frame();
        frame
            .gpu
            .filter(|g| held || now.saturating_duration_since(g.at) <= MAX_AGE)
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{CStr, c_void};
    use std::ptr;
    use std::sync::{Mutex, PoisonError};
    use std::time::Instant;

    use super::{
        Channel, CpuFreq, EnergyFrame, FreqFrame, GPU_DEFAULT_PERIOD, Gate, Hints,
        PMGR_FIRST_WINDOW, PMGR_PERIOD, PMGR_STABLE_PERIOD, Value, WAKE_FLOOR, clocks_stable,
        cpu_reading, energy_watts, gpu_busy, gpu_cadence, gpu_reading, keep_energy, next_streak,
        next_wait, pmgr_paced, pmgr_plan, pmgr_urgent, raw_timestamp,
    };
    use crate::dvfs::{self, ClusterTable, CpuNode};
    use crate::iokit::{self, CfKey, CfOwned, CfRef};
    use crate::needs::Needs;
    use crate::pacer::{Pacer, Wake};
    use crate::power::{ChannelRead, Counter, EnergyBank, Observation, RailReadings};
    use crate::sys;

    type CfTypeRef = *const c_void;

    const FORMAT_SIMPLE: i32 = 1;
    const FORMAT_STATE: i32 = 2;

    // libIOReport lives in the dyld shared cache; the SDK ships
    // usr/lib/libIOReport.tbd. Signatures follow the verified spike recipe.
    #[link(name = "IOReport", kind = "dylib")]
    unsafe extern "C" {
        fn IOReportCopyChannelsInGroup(
            group: CfTypeRef,
            subgroup: CfTypeRef,
            a: u64,
            b: u64,
            c: u64,
        ) -> CfTypeRef;
        fn IOReportMergeChannels(into: CfTypeRef, from: CfTypeRef, nil: CfTypeRef);
        fn IOReportCreateSubscription(
            a: *const c_void,
            desired: CfTypeRef,
            subbed: *mut CfTypeRef,
            channel_id: u64,
            b: CfTypeRef,
        ) -> CfTypeRef;
        fn IOReportCreateSamples(sub: CfTypeRef, subbed: CfTypeRef, a: CfTypeRef) -> CfTypeRef;
        fn IOReportCreateSamplesDelta(prev: CfTypeRef, cur: CfTypeRef, a: CfTypeRef) -> CfTypeRef;
        fn IOReportChannelGetGroup(ch: CfTypeRef) -> CfTypeRef;
        fn IOReportChannelGetSubGroup(ch: CfTypeRef) -> CfTypeRef;
        fn IOReportChannelGetChannelName(ch: CfTypeRef) -> CfTypeRef;
        fn IOReportChannelGetUnitLabel(ch: CfTypeRef) -> CfTypeRef;
        fn IOReportChannelGetFormat(ch: CfTypeRef) -> i32;
        fn IOReportSimpleGetIntegerValue(ch: CfTypeRef, index: i32) -> i64;
        fn IOReportStateGetCount(ch: CfTypeRef) -> i32;
        fn IOReportStateGetNameForIndex(ch: CfTypeRef, index: i32) -> CfTypeRef;
        fn IOReportStateGetResidency(ch: CfTypeRef, index: i32) -> i64;
    }

    fn text(value: CfTypeRef) -> String {
        CfRef::from_get(value)
            .and_then(iokit::cf_string)
            .unwrap_or_default()
    }

    fn channel_name(ch: CfRef<'_>) -> String {
        text(unsafe {
            // SAFETY: `ch` is a live channel dictionary; Get-rule string.
            IOReportChannelGetChannelName(ch.as_ptr())
        })
    }

    /// One (group, subgroup) request; `keep` filters channels by name
    /// (`None` keeps them all).
    pub(super) struct Group<'a> {
        pub(super) name: &'a CStr,
        pub(super) subgroup: Option<&'a CStr>,
        pub(super) keep: Option<fn(&str) -> bool>,
    }

    fn channels_in(request: &Group<'_>, list_key: &CfKey) -> Option<CfOwned> {
        let group = CfKey::new(request.name)?;
        let subgroup = match request.subgroup {
            Some(s) => Some(CfKey::new(s)?),
            None => None,
        };
        let sub_ptr = subgroup.as_ref().map_or(ptr::null(), CfKey::as_ptr);
        let dict = unsafe {
            // SAFETY: both strings are live (or the subgroup is null, meaning
            // the whole group); the call returns a +1 dictionary or null.
            IOReportCopyChannelsInGroup(group.as_ptr(), sub_ptr, 0, 0, 0)
        };
        // SAFETY: Copy-rule result.
        let dict = unsafe { CfOwned::from_create(dict) }?;
        let Some(keep) = request.keep else {
            return Some(dict);
        };
        let list = iokit::cf_dict_get(dict.borrow(), list_key)?;
        let kept: Vec<CfRef<'_>> = iokit::cf_array_items(list)
            .into_iter()
            .filter(|ch| keep(&channel_name(*ch)))
            .collect();
        if kept.is_empty() {
            return None;
        }
        let array = iokit::cf_array_of(&kept)?;
        let filtered = iokit::cf_mutable_copy(dict.borrow())?;
        iokit::cf_dict_set(&filtered, list_key, array.borrow());
        Some(filtered)
    }

    /// A live subscription and its previous sample; all released on drop.
    /// Not `Send`: it is made and used on the `IOReport` thread.
    pub(super) struct Subscription {
        sub: CfOwned,
        subbed: CfOwned,
        list_key: CfKey,
        raw_key: CfKey,
        prev: Option<(CfOwned, Instant)>,
    }

    /// One raw "Energy Model" read: running total and driver timestamp.
    pub(super) struct RawEnergy {
        pub(super) name: String,
        pub(super) unit: String,
        pub(super) total: u64,
        pub(super) stamp_ticks: u64,
    }

    impl Subscription {
        pub(super) fn new(groups: &[Group<'_>]) -> Option<Self> {
            let list_key = CfKey::new(c"IOReportChannels")?;
            let raw_key = CfKey::new(c"RawElements")?;
            let mut merged: Option<CfOwned> = None;
            for request in groups {
                let Some(dict) = channels_in(request, &list_key) else {
                    continue;
                };
                match &merged {
                    None => merged = iokit::cf_mutable_copy(dict.borrow()),
                    Some(into) => unsafe {
                        // SAFETY: both dictionaries are live; `into` is a
                        // mutable copy, merged in place.
                        IOReportMergeChannels(into.as_ptr(), dict.as_ptr(), ptr::null());
                    },
                }
            }
            let desired = merged?;
            let mut subbed: CfTypeRef = ptr::null();
            let sub = unsafe {
                // SAFETY: `desired` is a live mutable channel dictionary and
                // `subbed` a local out-pointer that receives a +1 dictionary.
                IOReportCreateSubscription(
                    ptr::null(),
                    desired.as_ptr(),
                    &raw mut subbed,
                    0,
                    ptr::null(),
                )
            };
            // SAFETY: both are Create-rule results (or null).
            let sub = unsafe { CfOwned::from_create(sub) };
            // SAFETY: as above.
            let subbed = unsafe { CfOwned::from_create(subbed) };
            Some(Self {
                sub: sub?,
                subbed: subbed?,
                list_key,
                raw_key,
                prev: None,
            })
        }

        /// The channels changed since the previous call and the seconds
        /// between them. The first call only primes.
        pub(super) fn sample(&mut self) -> Option<(Vec<Channel>, f64)> {
            let now = Instant::now();
            let current = unsafe {
                // SAFETY: `sub` and `subbed` are the live pair from
                // IOReportCreateSubscription; returns +1 or null.
                IOReportCreateSamples(self.sub.as_ptr(), self.subbed.as_ptr(), ptr::null())
            };
            // SAFETY: Create-rule result.
            let current = unsafe { CfOwned::from_create(current) }?;
            let (prev, then) = self.prev.replace((current, now))?;
            let (current, _) = self.prev.as_ref()?;
            let delta = unsafe {
                // SAFETY: both samples are live dictionaries from
                // IOReportCreateSamples; returns +1 or null.
                IOReportCreateSamplesDelta(prev.as_ptr(), current.as_ptr(), ptr::null())
            };
            // SAFETY: Create-rule result.
            let delta = unsafe { CfOwned::from_create(delta) }?;
            let list = iokit::cf_dict_get(delta.borrow(), &self.list_key)?;
            let channels = iokit::cf_array_items(list)
                .into_iter()
                .map(read_channel)
                .collect();
            Some((channels, now.duration_since(then).as_secs_f64()))
        }

        /// The "Energy Model" channels of the newest raw sample, with the
        /// running totals (not deltas) and driver timestamps the energy
        /// hold needs. Empty before the first sample.
        pub(super) fn raw_energy(&self) -> Vec<RawEnergy> {
            let Some((current, _)) = self.prev.as_ref() else {
                return Vec::new();
            };
            let Some(list) = iokit::cf_dict_get(current.borrow(), &self.list_key) else {
                return Vec::new();
            };
            iokit::cf_array_items(list)
                .into_iter()
                .filter_map(|ch| {
                    let p = ch.as_ptr();
                    let group = text(unsafe {
                        // SAFETY: `ch` is a live channel dictionary inside the
                        // sample; Get-rule string or null.
                        IOReportChannelGetGroup(p)
                    });
                    if group != "Energy Model" {
                        return None;
                    }
                    let stamp = iokit::cf_dict_get(ch, &self.raw_key)
                        .and_then(iokit::cf_data)
                        .and_then(raw_timestamp)?;
                    let format = unsafe {
                        // SAFETY: as above.
                        IOReportChannelGetFormat(p)
                    };
                    if format != FORMAT_SIMPLE {
                        return None;
                    }
                    let total = unsafe {
                        // SAFETY: simple-format channel; element 0 exists.
                        IOReportSimpleGetIntegerValue(p, 0)
                    };
                    Some(RawEnergy {
                        name: channel_name(ch),
                        unit: text(unsafe {
                            // SAFETY: as above.
                            IOReportChannelGetUnitLabel(p)
                        }),
                        total: u64::try_from(total).ok()?,
                        stamp_ticks: stamp,
                    })
                })
                .collect()
        }
    }

    fn read_channel(ch: CfRef<'_>) -> Channel {
        let p = ch.as_ptr();
        let group = text(unsafe {
            // SAFETY: `ch` is a live channel dictionary inside the delta;
            // Get-rule string or null.
            IOReportChannelGetGroup(p)
        });
        let subgroup = text(unsafe {
            // SAFETY: as above.
            IOReportChannelGetSubGroup(p)
        });
        let unit = text(unsafe {
            // SAFETY: as above.
            IOReportChannelGetUnitLabel(p)
        });
        let name = channel_name(ch);
        let format = unsafe {
            // SAFETY: as above.
            IOReportChannelGetFormat(p)
        };
        let value = match format {
            FORMAT_SIMPLE => Value::Simple(unsafe {
                // SAFETY: simple-format channel; element 0 always exists.
                IOReportSimpleGetIntegerValue(p, 0)
            }),
            FORMAT_STATE => {
                let count = unsafe {
                    // SAFETY: state-format channel.
                    IOReportStateGetCount(p)
                };
                Value::States(
                    (0..count.max(0))
                        .map(|i| {
                            // SAFETY: `i` is below the state count.
                            let state = text(unsafe { IOReportStateGetNameForIndex(p, i) });
                            let ticks = unsafe {
                                // SAFETY: as above.
                                IOReportStateGetResidency(p, i)
                            };
                            (state, ticks)
                        })
                        .collect(),
                )
            }
            _ => Value::Other,
        };
        Channel {
            group,
            subgroup,
            name,
            unit,
            value,
        }
    }

    pub(super) fn gpu_subscription() -> Option<Subscription> {
        Subscription::new(&[Group {
            name: c"GPU Stats",
            subgroup: Some(c"GPU Performance States"),
            keep: Some(|name| name == "GPUPH"),
        }])
    }

    /// CPU core residency plus the roll-up energy channels: one PMGR
    /// sample serves both. Either half alone still subscribes.
    pub(super) fn pmgr_subscription(cpu: bool) -> Option<Subscription> {
        let energy = Group {
            name: c"Energy Model",
            subgroup: None,
            keep: Some(keep_energy),
        };
        if !cpu {
            return Subscription::new(&[energy]);
        }
        Subscription::new(&[
            Group {
                name: c"CPU Stats",
                subgroup: Some(c"CPU Core Performance States"),
                keep: None,
            },
            energy,
        ])
    }

    pub(super) fn run(slot: &Mutex<FreqFrame>, hints: &Hints, pacer: &Pacer) {
        let (tables, nodes, gpu_table) = dvfs::discover();
        let mut gpu = (!gpu_table.is_empty()).then(gpu_subscription).flatten();
        // Energy needs no DVFS tables: an unknown future chip still gets
        // watts as long as its PMGR names the roll-up channels.
        let mut pmgr = pmgr_subscription(!tables.is_empty());
        if tables.is_empty() || pmgr.is_none() {
            slot.lock().unwrap_or_else(PoisonError::into_inner).settled = true;
        }
        if gpu.is_none() && pmgr.is_none() {
            return;
        }
        let mut bank = EnergyBank::new(PMGR_STABLE_PERIOD);
        let mut pmgr_gate = Gate::new(PMGR_PERIOD, PMGR_FIRST_WINDOW);
        let mut gpu_gate = Gate::new(GPU_DEFAULT_PERIOD, GPU_DEFAULT_PERIOD);
        let mut gpu_active = None;
        let mut last_cpu: Option<CpuFreq> = None;
        let mut pmgr_load = None;
        let mut streak = 0;
        loop {
            let now = Instant::now();
            let needs = hints.needs();
            let gpu_on = needs.contains(Needs::GPU_CLOCKS);
            let mut plan = pmgr_plan(needs, streak);
            let cadence = gpu_cadence(hints.interval(), gpu_busy(gpu_active, hints.gpu_busy()));
            gpu_gate.set_period(cadence);
            if let Some(sub) = gpu.as_mut().filter(|_| gpu_on)
                && gpu_gate.due(now)
                && let Some((channels, dt)) = sub.sample()
                && let Some(mut reading) = gpu_reading(&channels, &gpu_table, dt, Instant::now())
            {
                gpu_active = Some(reading.active);
                let mut frame = slot.lock().unwrap_or_else(PoisonError::into_inner);
                reading.watts = reading.watts.or(frame.gpu_watts);
                frame.gpu = Some(reading);
            }
            let load = hints.cpu_load();
            if let (Some(then), Some(load), Some(since)) =
                (pmgr_load, load, pmgr_gate.since_last(now))
                && pmgr_urgent(since, then, load)
            {
                pmgr_gate.restart();
                plan = plan.or(Some(PMGR_PERIOD));
            }
            if let Some(period) = plan {
                pmgr_gate.set_period(pmgr_paced(period, hints.interval()));
            }
            if let Some(sub) = pmgr.as_mut().filter(|_| plan.is_some())
                && pmgr_gate.due(Instant::now())
            {
                pmgr_load = load;
                let cpu = publish_pmgr(slot, sub, &tables, &nodes, &mut bank);
                let stable = match (&last_cpu, &cpu) {
                    (Some(prev), Some(cur)) => clocks_stable(prev, cur),
                    _ => false,
                };
                streak = next_streak(streak, stable);
                plan = pmgr_plan(needs, streak);
                if let Some(period) = plan {
                    pmgr_gate.set_period(pmgr_paced(period, hints.interval()));
                }
                if cpu.is_some() {
                    last_cpu = cpu;
                }
            }
            let now = Instant::now();
            let wait = next_wait(
                gpu.as_ref()
                    .filter(|_| gpu_on)
                    .map(|_| gpu_gate.remaining(now)),
                pmgr.as_ref().and(plan).map(|_| pmgr_gate.remaining(now)),
            );
            // Parked and settled: wake about once a second, only to catch a
            // load swing (two atomic loads, no IOReport call).
            let wait = if plan.is_none() {
                wait.min(hints.interval().max(PMGR_FIRST_WINDOW))
            } else {
                wait
            };
            match pacer.wait(wait.max(WAKE_FLOOR)) {
                Wake::Due => {}
                Wake::Resumed => {
                    pmgr_gate.restart();
                    gpu_gate.restart();
                }
                Wake::Stop => return,
            }
        }
    }

    /// One PMGR sample: CPU residency and GPU watts from the delta, rail
    /// watts from the raw running totals.
    fn publish_pmgr(
        slot: &Mutex<FreqFrame>,
        sub: &mut Subscription,
        tables: &[ClusterTable],
        nodes: &[CpuNode],
        bank: &mut EnergyBank,
    ) -> Option<CpuFreq> {
        let delta = sub.sample();
        let rails = observe_energy(sub, bank);
        let at = Instant::now();
        let mut frame = slot.lock().unwrap_or_else(PoisonError::into_inner);
        frame.energy = (bank.tracked() > 0).then_some(EnergyFrame { at, rails });
        let (channels, dt) = delta?;
        // The first window has landed: waiting longer fills in nothing new.
        frame.settled = true;
        let cpu = cpu_reading(&channels, tables, nodes, dt, at);
        if let Some(reading) = &cpu {
            frame.cpu = Some(reading.clone());
        }
        if let Some(watts) = energy_watts(&channels, "GPU Energy", dt) {
            frame.gpu_watts = Some(watts);
            // Hand the new average to the current GPU reading at once, so a
            // settled frame already carries GPU watts.
            if let Some(gpu) = frame.gpu.as_mut() {
                gpu.watts = Some(watts);
            }
        }
        cpu
    }

    fn observe_energy(sub: &Subscription, bank: &mut EnergyBank) -> RailReadings {
        let raw = sub.raw_energy();
        let now_ns = sys::mach_now_ns();
        let wall_ns = sys::mach_continuous_ns();
        bank.observe(raw.iter().map(|r| ChannelRead {
            name: &r.name,
            unit: &r.unit,
            obs: Observation {
                counter: Counter::Cumulative(r.total),
                published_ns: sys::mach_ticks_to_ns(r.stamp_ticks),
                now_ns,
                wall_ns,
            },
        }))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp
)]
mod tests {
    use super::*;
    use crate::dvfs::tests::m5pro_tables;
    use siltop_core::{Reading, ReadingSource};

    fn states(active_state: usize, ticks: i64, idle: i64, n: usize) -> Value {
        let mut s = vec![(String::from("DOWN"), 0), (String::from("IDLE"), idle)];
        for k in 0..n {
            s.push((
                format!("V{k}P{}", n - 1 - k),
                if k == active_state { ticks } else { 0 },
            ));
        }
        Value::States(s)
    }

    fn core(name: &str, value: Value) -> Channel {
        Channel {
            group: String::from("CPU Stats"),
            subgroup: String::from("CPU Core Performance States"),
            name: String::from(name),
            unit: String::from("24Mticks"),
            value,
        }
    }

    fn nodes() -> Vec<CpuNode> {
        (0..18)
            .map(|i| CpuNode {
                logical: i,
                cluster_id: (i / 6) as u8,
                core: (i % 6) as u8,
                letter: Some(if i < 12 { 'M' } else { 'P' }),
            })
            .collect()
    }

    /// `load.sh 18`: every MCPU core pinned at 4380 MHz, Super at 4104.
    fn full_load() -> Vec<Channel> {
        let mut out = Vec::new();
        for c in 0..2 {
            for i in 0..6 {
                out.push(core(&format!("MCPU{c}{i}"), states(14, 24_000_000, 0, 15)));
            }
        }
        for i in 0..6 {
            out.push(core(&format!("PCPU{i}"), states(11, 24_000_000, 0, 20)));
        }
        out
    }

    #[test]
    fn full_load_reads_table_max_on_mcpu_and_real_mhz_on_super() {
        let (tables, _) = m5pro_tables();
        let r = cpu_reading(&full_load(), &tables, &nodes(), 1.0, Instant::now()).unwrap();
        assert_eq!(r.clusters.len(), 3);
        for c in &r.clusters[..2] {
            assert_eq!(c.mhz, Some(4380));
            assert!((c.active - 1.0).abs() < 1e-6);
            assert!((c.scaled - 1.0).abs() < 1e-6);
            assert_eq!(c.cpus.len(), 6);
        }
        let s = &r.clusters[2];
        assert_eq!(s.name, "PCPU");
        assert_eq!(s.mhz, Some(4104));
        assert!((s.scaled - 4104.0 / 4608.0).abs() < 1e-4);
        assert_eq!(s.cpus, vec![12, 13, 14, 15, 16, 17]);
        assert_eq!(r.cores.len(), 18);
        assert!(
            r.cores.windows(2).all(|w| w[0].cpu < w[1].cpu),
            "sorted by cpu"
        );
    }

    #[test]
    fn independent_performance_clusters_keep_their_own_clocks() {
        let (tables, _) = m5pro_tables();
        let mut ch = full_load();
        // MCPU1 at the floor while MCPU0 runs flat out (seen live).
        for c in ch.iter_mut().filter(|c| c.name.starts_with("MCPU1")) {
            c.value = states(0, 2_400_000, 21_600_000, 15);
        }
        let r = cpu_reading(&ch, &tables, &nodes(), 1.0, Instant::now()).unwrap();
        assert_eq!(r.clusters[0].mhz, Some(4380));
        assert_eq!(r.clusters[1].mhz, Some(1344));
        assert!((r.clusters[1].active - 0.1).abs() < 1e-6);
    }

    #[test]
    fn a_bogus_window_drops_the_whole_cpu_reading() {
        let (tables, _) = m5pro_tables();
        let mut ch = full_load();
        ch[3].value = states(0, 2_000_000_000, 0, 15);
        assert!(cpu_reading(&ch, &tables, &nodes(), 0.5, Instant::now()).is_none());
    }

    #[test]
    fn unrelated_channels_give_no_cpu_reading() {
        let (tables, _) = m5pro_tables();
        let mut other = core("MCPU0", states(0, 100, 0, 15));
        other.subgroup = String::from("CPU Complex Performance States");
        let simple = Channel {
            value: Value::Simple(5),
            ..core("MCPU00", Value::Other)
        };
        let none = cpu_reading(&[other, simple], &tables, &nodes(), 1.0, Instant::now());
        assert!(none.is_none());
        assert!(cpu_reading(&[], &tables, &nodes(), 1.0, Instant::now()).is_none());
        assert!(cpu_reading(&full_load(), &[], &nodes(), 1.0, Instant::now()).is_none());
    }

    #[test]
    fn a_fully_idle_cluster_has_no_mhz_not_zero() {
        let (tables, _) = m5pro_tables();
        let mut ch = full_load();
        for c in ch.iter_mut().filter(|c| c.name.starts_with("PCPU")) {
            c.value = states(0, 0, 24_000_000, 20);
        }
        let r = cpu_reading(&ch, &tables, &nodes(), 1.0, Instant::now()).unwrap();
        assert_eq!(r.clusters[2].mhz, None);
        assert_eq!(r.clusters[2].active, 0.0);
        assert_eq!(r.clusters[2].scaled, 0.0);
        assert!(
            r.cores
                .iter()
                .filter(|c| c.cpu >= 12)
                .all(|c| c.mhz.is_none())
        );
    }

    fn gpuph(k: usize, ticks: i64, off: i64) -> Channel {
        let mut s = vec![(String::from("OFF"), off)];
        for i in 1..=15 {
            s.push((format!("P{i}"), if i == k { ticks } else { 0 }));
        }
        Channel {
            group: String::from("GPU Stats"),
            subgroup: String::from("GPU Performance States"),
            name: String::from("GPUPH"),
            unit: String::from("24Mticks"),
            value: Value::States(s),
        }
    }

    fn energy(nj: i64, unit: &str) -> Channel {
        Channel {
            group: String::from("Energy Model"),
            subgroup: String::new(),
            name: String::from("GPU Energy"),
            unit: String::from(unit),
            value: Value::Simple(nj),
        }
    }

    #[test]
    fn gpu_reading_is_frequency_weighted_with_watts() {
        let (_, table) = m5pro_tables();
        // 50% active at 589-ish MHz region: P3 = 636 MHz.
        let ch = [gpuph(3, 3_000_000, 3_000_000), energy(375_000_000, "nJ")];
        let g = gpu_reading(&ch, &table, 0.25, Instant::now()).unwrap();
        assert!((g.active - 0.5).abs() < 1e-6);
        assert_eq!(g.mhz, Some(636));
        assert!((g.scaled - 0.5 * 636.0 / 1620.0).abs() < 1e-5);
        assert_eq!(g.max_mhz, 1620);
        assert!((g.watts.unwrap() - 1.5).abs() < 1e-5);
        assert!(!g.unmapped);
    }

    #[test]
    fn energy_watts_matches_exact_names_only() {
        let ch = [
            energy(2_000_000_000, "nJ"),
            Channel {
                name: String::from("GPU Energy DTL"),
                ..energy(9_000_000_000, "nJ")
            },
            Channel {
                group: String::from("GPU Stats"),
                ..energy(7_000_000_000, "nJ")
            },
        ];
        assert_eq!(energy_watts(&ch, "GPU Energy", 1.0), Some(2.0));
        assert_eq!(energy_watts(&ch, "GPU Energy", 0.5), Some(4.0));
        assert_eq!(energy_watts(&ch, "GPU Energy", 0.0), None);
        assert_eq!(energy_watts(&ch, "CPU Energy", 1.0), None);
        assert_eq!(energy_watts(&ch[1..], "GPU Energy", 1.0), None);
    }

    #[test]
    fn gpu_p14_residency_is_flagged_not_counted() {
        let (_, table) = m5pro_tables();
        let g = gpu_reading(&[gpuph(14, 1_000, 0)], &table, 1.0, Instant::now()).unwrap();
        assert!(g.unmapped);
        assert_eq!(g.mhz, None);
        assert_eq!(g.watts, None, "no energy channel is absent, not 0 W");
    }

    #[test]
    fn gpu_reading_needs_gpuph_a_table_and_a_window() {
        let (_, table) = m5pro_tables();
        let now = Instant::now();
        assert!(gpu_reading(&[energy(5, "nJ")], &table, 1.0, now).is_none());
        assert!(gpu_reading(&[gpuph(1, 5, 5)], &[], 1.0, now).is_none());
        assert!(gpu_reading(&[gpuph(1, 5, 5)], &table, 0.0, now).is_none());
        assert!(gpu_reading(&[gpuph(1, 2_000_000_000, 0)], &table, 0.25, now).is_none());
    }

    #[test]
    fn energy_units_table() {
        let cases: &[(i64, &str, Option<f64>)] = &[
            (1_000, "mJ", Some(1.0)),
            (1_000_000, "uJ", Some(1.0)),
            (1_000_000, "µJ", Some(1.0)),
            (1_000_000_000, "nJ", Some(1.0)),
            (1_000_000_000, " nJ ", Some(1.0)),
            (5, "J", None),
            (5, "", None),
            (5, "24Mticks", None),
            (0, "nJ", Some(0.0)),
        ];
        for (v, unit, want) in cases {
            assert_eq!(joules(*v, unit), *want, "{v} {unit}");
        }
    }

    #[test]
    fn negative_energy_is_absent_not_negative_watts() {
        let (_, table) = m5pro_tables();
        let g = gpu_reading(
            &[gpuph(1, 5, 5), energy(-5, "nJ")],
            &table,
            1.0,
            Instant::now(),
        );
        assert_eq!(g.unwrap().watts, None);
    }

    // ---------- energy feed ----------

    /// An `IOReportElement`: provider id, channel id, channel type, then
    /// the driver timestamp at byte 24 and the value.
    fn element(stamp: u64) -> Vec<u8> {
        let mut raw = Vec::new();
        raw.extend_from_slice(&7_u64.to_ne_bytes());
        raw.extend_from_slice(&42_u64.to_ne_bytes());
        raw.extend_from_slice(&[1, 0, 0, 0, 1, 0, 0, 0]);
        raw.extend_from_slice(&stamp.to_ne_bytes());
        raw.extend_from_slice(&123_456_u64.to_ne_bytes());
        raw
    }

    #[test]
    fn raw_timestamp_reads_byte_24() {
        assert_eq!(
            raw_timestamp(&element(6_423_836_162_747)),
            Some(6_423_836_162_747)
        );
        assert_eq!(raw_timestamp(&element(0)), None, "zero is no stamp");
        assert_eq!(raw_timestamp(&element(9)[..31]), None, "short element");
        assert_eq!(raw_timestamp(&[]), None);
        assert_eq!(
            raw_timestamp(&element(9)[..32]),
            Some(9),
            "exactly 32 bytes"
        );
    }

    #[test]
    fn energy_subscription_keeps_roll_ups_on_every_family_naming() {
        for keep in [
            "CPU Energy",
            "GPU Energy",
            "ANE",
            "ANE0",
            "DRAM0",
            "DIE_0_CPU Energy",
            "DIE_1_DRAM0",
        ] {
            assert!(keep_energy(keep), "{keep}");
        }
        for drop in [
            "ECPU",
            "PCPU1",
            "PACC0_CPU",
            "GPU",
            "DCS0",
            "DISP0",
            "MCPU0_SRAM",
        ] {
            assert!(!keep_energy(drop), "{drop}");
        }
    }

    #[test]
    fn joules_rejects_a_negative_delta() {
        assert_eq!(joules(-1, "mJ"), None, "a counter reset is not energy");
        assert_eq!(joules(1_000_000, "μJ"), Some(1.0), "greek mu too");
    }

    fn energy_frame(at: Instant) -> FreqFrame {
        FreqFrame {
            energy: Some(EnergyFrame {
                at,
                rails: RailReadings {
                    gpu: Some(Reading::measured(2.5)),
                    dram: Some(Reading {
                        watts: 0.8,
                        source: ReadingSource::Held {
                            age: Duration::from_secs(9),
                        },
                    }),
                    ..RailReadings::default()
                },
            }),
            ..FreqFrame::default()
        }
    }

    #[test]
    fn energy_is_aged_to_the_reader_and_expires() {
        let t0 = Instant::now();
        let feed = FreqFeed::fixed(energy_frame(t0));
        let fresh = feed.energy(t0 + Duration::from_millis(300)).unwrap();
        assert_eq!(fresh.gpu, Some(Reading::measured(2.5)));
        assert!(fresh.dram.is_some());
        let later = feed.energy(t0 + Duration::from_secs(2)).unwrap();
        assert!(matches!(
            later.gpu.unwrap().source,
            ReadingSource::Held { .. }
        ));
        assert_eq!(later.dram, None, "9 s held + 2 s is stale");
        assert!(
            feed.energy(t0 + MAX_AGE + Duration::from_millis(1))
                .is_none()
        );
        assert!(FreqFeed::default().energy(t0).is_none(), "absent, not 0 W");
    }

    #[test]
    fn a_feed_without_a_thread_is_settled_and_a_fixed_one_follows_its_frame() {
        assert!(FreqFeed::default().settled(), "nothing to wait for");
        // `fixed` feeds have no thread either.
        assert!(FreqFeed::fixed(FreqFrame::default()).settled());
    }

    #[test]
    fn a_paused_or_threadless_feed_ignores_pause() {
        let feed = FreqFeed::default();
        feed.set_paused(true);
        feed.set_paused(false);
        assert!(feed.cpu(Instant::now()).is_none());
    }

    #[test]
    fn stale_readings_are_not_served() {
        let t0 = Instant::now();
        let feed = FreqFeed::fixed(FreqFrame {
            cpu: Some(CpuFreq {
                at: t0,
                clusters: Vec::new(),
                cores: Vec::new(),
            }),
            gpu: Some(GpuFreq {
                at: t0,
                active: 0.5,
                scaled: 0.2,
                mhz: Some(636),
                max_mhz: 1620,
                watts: None,
                unmapped: false,
            }),
            gpu_watts: None,
            ..FreqFrame::default()
        });
        assert!(feed.cpu(t0 + Duration::from_secs(1)).is_some());
        assert!(feed.gpu(t0 + MAX_AGE).is_some());
        assert!(feed.cpu(t0 + MAX_AGE + Duration::from_millis(1)).is_none());
        assert!(feed.gpu(t0 + Duration::from_secs(60)).is_none());
        let empty = FreqFeed::default();
        assert!(empty.cpu(t0).is_none() && empty.gpu(t0).is_none());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn linux_stub_reports_absent() {
        let feed = FreqFeed::spawn();
        std::thread::sleep(Duration::from_millis(20));
        assert!(feed.cpu(Instant::now()).is_none());
        assert!(feed.gpu(Instant::now()).is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_feed_fills_cpu_and_gpu() {
        let feed = FreqFeed::spawn();
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut cpu = None;
        let mut watts = None;
        while Instant::now() < deadline && (cpu.is_none() || watts.is_none()) {
            std::thread::sleep(Duration::from_millis(100));
            cpu = feed.cpu(Instant::now());
            watts = feed.gpu(Instant::now()).and_then(|g| g.watts);
        }
        let (tables, _, gpu_table) = dvfs::discover();
        if tables.is_empty() {
            return; // Not an Apple-Silicon machine with known tables.
        }
        let cpu = cpu.expect("a CPU reading within 4 s");
        assert_eq!(cpu.clusters.len(), tables.len());
        for c in &cpu.clusters {
            if let Some(mhz) = c.mhz {
                assert!(mhz >= c.min_mhz && mhz <= c.max_mhz, "{c:?}");
            }
            assert!(c.scaled <= c.active + 1e-6);
        }
        if !gpu_table.is_empty() {
            let gpu = feed.gpu(Instant::now()).expect("gpu reading");
            assert!((0.0..=1.0).contains(&gpu.scaled));
            assert!(gpu.watts.is_some_and(|w| (0.0..200.0).contains(&w)));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "manual: cargo test -p siltop-metrics -- --ignored --nocapture energy_live_trace"]
    fn energy_live_trace() {
        let mut sub = macos::pmgr_subscription(false).expect("energy sub");
        for _ in 0..8 {
            let _ = sub.sample();
            for r in sub.raw_energy() {
                eprintln!(
                    "{:<12} {:>3} total={} stamp={} now={}",
                    r.name,
                    r.unit,
                    r.total,
                    r.stamp_ticks,
                    crate::sys::mach_now_ns()
                );
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "manual: cargo test -p siltop-metrics --release -- --ignored --nocapture ioreport_cost"]
    fn ioreport_cost() {
        let (tables, nodes, gpu_table) = dvfs::discover();
        let mut gpu = macos::gpu_subscription().expect("gpu sub");
        let mut pmgr = macos::pmgr_subscription(true).expect("pmgr sub");
        let _ = gpu.sample();
        let _ = pmgr.sample();
        let rounds = 20;
        let t0 = crate::sys::thread_cpu_secs();
        for _ in 0..rounds {
            std::thread::sleep(Duration::from_millis(100));
            let (ch, dt) = gpu.sample().expect("gpu");
            let _ = gpu_reading(&ch, &gpu_table, dt, Instant::now());
        }
        let gpu_ms = (crate::sys::thread_cpu_secs() - t0) / f64::from(rounds) * 1e3;
        let t0 = crate::sys::thread_cpu_secs();
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(100));
            let (ch, dt) = pmgr.sample().expect("pmgr");
            let _ = cpu_reading(&ch, &tables, &nodes, dt, Instant::now());
            assert!(!pmgr.raw_energy().is_empty(), "roll-up channels present");
        }
        let pmgr_ms = (crate::sys::thread_cpu_secs() - t0) / 10.0 * 1e3;
        let share =
            (gpu_ms / GPU_DEFAULT_PERIOD.as_secs_f64() + pmgr_ms / PMGR_PERIOD.as_secs_f64()) / 1e3;
        eprintln!(
            "ioreport gpu {gpu_ms:.3} ms/sample, pmgr {pmgr_ms:.3} ms/sample, {:.2}% CPU",
            share * 100.0
        );
        assert!(share <= 0.01, "IOReport {:.2}% CPU", share * 100.0);
    }

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn a_slow_snapshot_interval_slows_the_pmgr_sample_to_match() {
        assert_eq!(pmgr_paced(PMGR_PERIOD, 500 * MS), PMGR_PERIOD);
        assert_eq!(pmgr_paced(PMGR_PERIOD, 5000 * MS), 5000 * MS);
        assert_eq!(
            pmgr_paced(PMGR_DEEP_PERIOD, 5000 * MS),
            PMGR_DEEP_PERIOD.max(5000 * MS)
        );
    }

    #[test]
    fn gate_primes_at_once_then_first_window_then_period() {
        let t0 = Instant::now();
        let mut gate = Gate::new(2000 * MS, 1000 * MS);
        assert!(gate.due(t0), "priming sample is immediate");
        assert!(!gate.due(t0 + 999 * MS));
        assert!(gate.due(t0 + 1000 * MS), "first window is the short one");
        assert!(!gate.due(t0 + 2999 * MS));
        assert!(gate.due(t0 + 3000 * MS), "then every period");
        assert!(!gate.due(t0 + 4000 * MS));
        assert!(gate.due(t0 + 5000 * MS));
    }

    #[test]
    fn gate_restart_fires_at_once_and_keeps_the_regular_period() {
        let t0 = Instant::now();
        let mut gate = Gate::new(2000 * MS, 1000 * MS);
        assert!(gate.due(t0));
        assert!(gate.due(t0 + 1000 * MS));
        gate.restart();
        assert!(gate.due(t0 + 1200 * MS), "a resume samples at once");
        assert!(
            !gate.due(t0 + 2500 * MS),
            "not the short first window again"
        );
        assert!(gate.due(t0 + 3200 * MS));
    }

    #[test]
    fn gate_late_wakes_schedule_from_the_actual_sample() {
        let t0 = Instant::now();
        let mut gate = Gate::new(2000 * MS, 1000 * MS);
        assert!(gate.due(t0));
        assert!(gate.due(t0 + 1500 * MS));
        assert!(!gate.due(t0 + 3499 * MS));
        assert!(gate.due(t0 + 3500 * MS));
    }

    #[test]
    fn pmgr_windows_fit_inside_max_age() {
        // A held CPU or energy reading must never expire between two PMGR
        // samples, even on the slow stable cadence.
        assert!(PMGR_FIRST_WINDOW <= PMGR_PERIOD);
        assert!(PMGR_PERIOD < PMGR_STABLE_PERIOD);
        assert!(PMGR_STABLE_PERIOD < PMGR_DEEP_PERIOD);
        assert!(PMGR_DEEP_PERIOD < MAX_AGE);
        assert!(
            GPU_IDLE_PERIOD < MAX_AGE,
            "an idle GPU reading stays served"
        );
    }

    #[test]
    fn a_long_snapshot_interval_still_serves_a_fresh_gpu_reading() {
        // `-i` reaches 60 s: every cadence must leave a reading younger
        // than MAX_AGE at the next snapshot, with a whole window to spare.
        for secs in [1, 5, 8, 10, 30, 60, 3600] {
            for busy in [true, false] {
                let cadence = gpu_cadence(Duration::from_secs(secs), busy);
                assert!(cadence <= GPU_MAX_PERIOD, "{secs}s busy={busy}");
                assert!(cadence * 2 <= MAX_AGE, "{secs}s busy={busy}");
            }
        }
        assert!(GPU_IDLE_PERIOD <= GPU_MAX_PERIOD);
    }

    #[test]
    fn gate_set_period_moves_the_next_sample() {
        let t0 = Instant::now();
        let mut gate = Gate::new(2000 * MS, 1000 * MS);
        assert!(gate.due(t0));
        assert!(gate.due(t0 + 1000 * MS));
        gate.set_period(4000 * MS);
        assert!(!gate.due(t0 + 3000 * MS), "stable cadence stretches");
        assert_eq!(gate.remaining(t0 + 3000 * MS), 2000 * MS);
        gate.set_period(2000 * MS);
        assert!(gate.due(t0 + 3000 * MS), "moving clocks pull it back in");
    }

    #[test]
    fn gate_remaining_is_zero_before_the_first_sample_and_when_forced() {
        let t0 = Instant::now();
        let mut gate = Gate::new(2000 * MS, 1000 * MS);
        assert_eq!(gate.remaining(t0), Duration::ZERO);
        assert_eq!(gate.since_last(t0), None);
        assert!(gate.due(t0));
        assert_eq!(gate.remaining(t0 + 400 * MS), 600 * MS);
        assert_eq!(gate.since_last(t0 + 400 * MS), Some(400 * MS));
        gate.restart();
        assert_eq!(gate.remaining(t0 + 400 * MS), Duration::ZERO);
    }

    #[test]
    fn idle_gpu_is_sampled_every_two_seconds_and_busy_follows_the_interval() {
        assert_eq!(gpu_cadence(500 * MS, true), 500 * MS);
        assert_eq!(gpu_cadence(500 * MS, false), GPU_IDLE_PERIOD);
        assert_eq!(gpu_cadence(100 * MS, true), GPU_PERIOD, "floor kept");
        assert_eq!(
            gpu_cadence(5000 * MS, false),
            5000 * MS,
            "never faster than the snapshot interval"
        );
    }

    #[test]
    fn gpu_busy_needs_either_signal_and_unknown_is_busy() {
        assert!(gpu_busy(None, None), "first windows at full cadence");
        assert!(!gpu_busy(Some(0.0), None));
        assert!(!gpu_busy(Some(0.01), Some(0.0)));
        assert!(
            !gpu_busy(Some(0.048), Some(0.02)),
            "rv: the compositor's 2-5% is idle"
        );
        assert!(
            gpu_busy(Some(0.0), Some(0.30)),
            "IOAccelerator hint wakes it"
        );
        assert!(gpu_busy(Some(0.5), Some(0.0)));
        assert!(gpu_busy(None, Some(GPU_IDLE_ACTIVE)));
    }

    fn clusters(mhz: [Option<u32>; 2], active: [f32; 2]) -> CpuFreq {
        let cluster = |i: usize| ClusterFreq {
            name: format!("C{i}"),
            id: None,
            min_mhz: 600,
            max_mhz: 4000,
            active: active[i],
            scaled: 0.0,
            mhz: mhz[i],
            cpus: Vec::new(),
        };
        CpuFreq {
            at: Instant::now(),
            clusters: vec![cluster(0), cluster(1)],
            cores: Vec::new(),
        }
    }

    #[test]
    fn clocks_stable_tolerates_small_drift_only() {
        let a = clusters([Some(1000), Some(2000)], [0.05, 0.30]);
        let near = clusters([Some(1300), Some(2100)], [0.08, 0.35]);
        assert!(clocks_stable(&a, &near), "300 MHz of 4000 is within 10%");
        let jump = clusters([Some(1000), Some(3600)], [0.05, 0.30]);
        assert!(!clocks_stable(&a, &jump), "a busy cluster ramped");
        let loaded = clusters([Some(1000), Some(2000)], [0.05, 0.60]);
        assert!(!clocks_stable(&a, &loaded), "load rose at the same clock");
    }

    #[test]
    fn rv_idle_cluster_clock_dither_is_stable() {
        // Found live: an idle E-cluster swings 1000 <-> 2000 MHz at 2-4%
        // active, which kept PMGR on its fast cadence all the time.
        let a = clusters([Some(1000), Some(900)], [0.03, 0.02]);
        let b = clusters([Some(2000), None], [0.04, 0.01]);
        assert!(clocks_stable(&a, &b));
        let woke = clusters([Some(2000), Some(3000)], [0.04, 0.40]);
        assert!(!clocks_stable(&a, &woke), "one cluster got busy");
    }

    #[test]
    fn a_cluster_waking_or_sleeping_is_a_change() {
        let asleep = clusters([None, Some(2000)], [0.0, 0.3]);
        let awake = clusters([Some(1000), Some(2000)], [0.2, 0.3]);
        assert!(!clocks_stable(&asleep, &awake));
        assert!(!clocks_stable(&awake, &asleep));
        assert!(clocks_stable(&asleep, &asleep.clone()));
        let fewer = CpuFreq {
            clusters: asleep.clusters[..1].to_vec(),
            ..asleep.clone()
        };
        assert!(!clocks_stable(&asleep, &fewer), "topology differs");
    }

    #[test]
    fn stable_clocks_stretch_pmgr_to_four_seconds() {
        assert_eq!(pmgr_cadence(1), PMGR_STABLE_PERIOD);
        assert_eq!(pmgr_cadence(0), PMGR_PERIOD);
    }

    #[test]
    fn a_long_stable_streak_stretches_pmgr_to_eight_seconds() {
        assert_eq!(pmgr_cadence(DEEP_AFTER - 1), PMGR_STABLE_PERIOD);
        assert_eq!(pmgr_cadence(DEEP_AFTER), PMGR_DEEP_PERIOD);
        assert_eq!(pmgr_cadence(u32::MAX), PMGR_DEEP_PERIOD);
    }

    #[test]
    fn one_moving_window_drops_the_streak_back_to_two_seconds() {
        let mut streak = 0;
        for _ in 0..10 {
            streak = next_streak(streak, true);
        }
        assert_eq!(pmgr_cadence(streak), PMGR_DEEP_PERIOD);
        streak = next_streak(streak, false);
        assert_eq!(streak, 0);
        assert_eq!(pmgr_cadence(streak), PMGR_PERIOD);
    }

    #[test]
    fn the_streak_saturates_instead_of_wrapping() {
        assert_eq!(next_streak(u32::MAX, true), u32::MAX);
    }

    #[test]
    fn energy_alone_runs_at_the_deep_cadence() {
        assert_eq!(pmgr_plan(Needs::POWER, 0), Some(PMGR_DEEP_PERIOD));
    }

    #[test]
    fn visible_clocks_follow_the_streak() {
        assert_eq!(pmgr_plan(Needs::ALL, 0), Some(PMGR_PERIOD));
        assert_eq!(pmgr_plan(Needs::CPU_CLOCKS, 1), Some(PMGR_STABLE_PERIOD));
        assert_eq!(pmgr_plan(Needs::ALL, DEEP_AFTER), Some(PMGR_DEEP_PERIOD));
    }

    #[test]
    fn parked_clocks_settle_then_stop_sampling() {
        let parked = Needs::GPU_CLOCKS;
        assert_eq!(pmgr_plan(parked, 0), Some(PMGR_PERIOD), "still moving");
        assert_eq!(pmgr_plan(parked, DEEP_AFTER - 1), Some(PMGR_STABLE_PERIOD));
        assert_eq!(pmgr_plan(parked, DEEP_AFTER), None, "settled: skipped");
        assert_eq!(pmgr_plan(Needs::NONE, u32::MAX), None);
    }

    #[test]
    fn a_swing_while_parked_restarts_the_settle() {
        // A settled, parked PMGR: the load swings, one sample disagrees.
        let streak = next_streak(DEEP_AFTER, false);
        assert_eq!(pmgr_plan(Needs::NONE, streak), Some(PMGR_PERIOD));
        assert!(pmgr_urgent(Duration::from_secs(30), 0.05, 0.60));
    }

    #[test]
    fn the_deep_cadence_never_reads_as_sleep_to_the_energy_bank() {
        // `EnergyBank` rebases after 3 of its cadences (`PMGR_STABLE_PERIOD`)
        // of wall-clock gap: a deep window plus a late wake must stay under.
        assert!(PMGR_DEEP_PERIOD + Duration::from_secs(2) < PMGR_STABLE_PERIOD * 3);
    }

    #[test]
    fn a_load_jump_makes_pmgr_urgent_but_not_twice_in_a_second() {
        assert!(pmgr_urgent(1500 * MS, 0.05, 0.40));
        assert!(pmgr_urgent(1500 * MS, 0.60, 0.10), "falling counts too");
        assert!(!pmgr_urgent(1500 * MS, 0.05, 0.10), "small swing");
        assert!(
            !pmgr_urgent(900 * MS, 0.05, 0.90),
            "a window under a second reads nothing useful"
        );
    }

    #[test]
    fn hints_start_unknown_and_carry_the_latest_value() {
        let feed = FreqFeed::fixed(FreqFrame::default());
        assert_eq!(feed.hints.cpu_load(), None);
        assert_eq!(feed.hints.gpu_busy(), None);
        assert_eq!(feed.hints.interval(), GPU_DEFAULT_PERIOD);
        feed.note_cpu_load(0.25);
        feed.note_gpu_busy(0.0);
        assert_eq!(feed.hints.cpu_load(), Some(0.25));
        assert_eq!(feed.hints.gpu_busy(), Some(0.0));
        feed.note_cpu_load(f32::NAN);
        assert_eq!(feed.hints.cpu_load(), None, "NaN is unknown, not 0");
    }

    #[test]
    fn an_idle_gpu_hint_slows_the_cadence_the_thread_would_pick() {
        let feed = FreqFeed::fixed(FreqFrame::default());
        feed.set_interval(500 * MS);
        assert_eq!(feed.gpu_period(), 500 * MS, "unknown counts as busy");
        feed.note_gpu_busy(0.0);
        assert_eq!(feed.gpu_period(), GPU_IDLE_PERIOD);
        feed.note_gpu_busy(0.5);
        assert_eq!(feed.gpu_period(), 500 * MS);
    }

    #[test]
    fn gpu_cadence_follows_the_snapshot_interval_above_the_floor() {
        assert_eq!(gpu_period_for(1000 * MS), 1000 * MS);
        assert_eq!(gpu_period_for(500 * MS), 500 * MS);
        assert_eq!(gpu_period_for(100 * MS), GPU_PERIOD, "floored");
    }

    #[test]
    fn set_interval_reaches_the_gpu_cadence() {
        let feed = FreqFeed::fixed(FreqFrame::default());
        feed.set_interval(2000 * MS);
        assert_eq!(feed.gpu_period(), 2000 * MS);
        assert_eq!(feed.hints.interval(), 2000 * MS);
        feed.set_interval(MS);
        assert_eq!(feed.gpu_period(), GPU_PERIOD);
    }

    fn held_frame(t0: Instant) -> FreqFrame {
        FreqFrame {
            cpu: Some(CpuFreq {
                at: t0,
                clusters: Vec::new(),
                cores: Vec::new(),
            }),
            gpu: Some(GpuFreq {
                at: t0,
                active: 0.5,
                scaled: 0.2,
                mhz: Some(636),
                max_mhz: 1620,
                watts: None,
                unmapped: false,
            }),
            ..energy_frame(t0)
        }
    }

    #[test]
    fn a_feed_needs_everything_by_default() {
        assert_eq!(FreqFeed::default().hints.needs(), Needs::ALL);
    }

    #[test]
    fn parked_clocks_hold_their_last_reading_instead_of_expiring() {
        let t0 = Instant::now();
        let feed = FreqFeed::fixed(held_frame(t0));
        let late = t0 + MAX_AGE + Duration::from_secs(30);
        assert!(feed.cpu(late).is_none(), "needed and stale: absent");
        feed.set_needs_at(Needs::NONE, t0);
        assert!(feed.cpu(late).is_some(), "parked: held");
        assert!(feed.gpu(late).is_some(), "parked: held");
    }

    #[test]
    fn parked_power_is_never_held_as_live_watts() {
        let t0 = Instant::now();
        let feed = FreqFeed::fixed(held_frame(t0));
        feed.set_needs_at(Needs::NONE, t0);
        assert!(
            feed.energy(t0 + MAX_AGE + Duration::from_secs(1)).is_none(),
            "watts go absent, not frozen"
        );
    }

    #[test]
    fn re_enabled_clocks_serve_the_held_value_until_the_fresh_sample() {
        let t0 = Instant::now();
        let feed = FreqFeed::fixed(held_frame(t0));
        feed.set_needs_at(Needs::NONE, t0);
        let back = t0 + Duration::from_secs(120);
        feed.set_needs_at(Needs::ALL, back);
        let cpu = feed.cpu(back + Duration::from_millis(10)).unwrap();
        assert_eq!(cpu.at, back, "restamped as of the resume");
        assert_eq!(feed.gpu(back).unwrap().mhz, Some(636));
        assert!(
            feed.cpu(back + MAX_AGE + Duration::from_millis(1))
                .is_none(),
            "no fresh sample after that: ages out as usual"
        );
    }

    #[test]
    fn only_the_re_enabled_source_is_restamped() {
        let t0 = Instant::now();
        let feed = FreqFeed::fixed(held_frame(t0));
        feed.set_needs_at(Needs::NONE, t0);
        let back = t0 + Duration::from_secs(60);
        feed.set_needs_at(Needs::CPU_CLOCKS, back);
        assert_eq!(feed.cpu(back).unwrap().at, back);
        // GPU is still parked: held, but keeps its own timestamp.
        assert_eq!(feed.gpu(back).unwrap().at, t0);
    }

    #[test]
    fn repeating_the_same_needs_restamps_nothing() {
        let t0 = Instant::now();
        let feed = FreqFeed::fixed(held_frame(t0));
        feed.set_needs_at(Needs::ALL, t0 + Duration::from_secs(1));
        assert_eq!(feed.cpu(t0).unwrap().at, t0, "ALL -> ALL adds nothing");
    }

    #[test]
    fn next_wait_takes_the_earlier_running_gate() {
        let ms = Duration::from_millis;
        assert_eq!(next_wait(Some(ms(250)), Some(ms(900))), ms(250));
        assert_eq!(next_wait(None, Some(ms(900))), ms(900));
        assert_eq!(next_wait(Some(ms(250)), None), ms(250));
    }

    #[test]
    fn next_wait_naps_long_when_every_source_is_parked() {
        assert_eq!(next_wait(None, None), PARKED_WAIT);
        assert!(PARKED_WAIT >= Duration::from_secs(60));
    }
}
