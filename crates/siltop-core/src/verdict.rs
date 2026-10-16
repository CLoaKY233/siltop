//! The verdict: one plain sentence that says whether the Mac is fine, and if
//! not, what is wrong and who is doing it. Pure logic over a [`Snapshot`].
//!
//! [`assess`] judges one snapshot. [`VerdictEngine`] adds hysteresis: a new
//! kind of verdict must hold for [`VERDICT_HOLD`] before it replaces the
//! shown one, so a one-tick spike never flips the line.

use std::time::{Duration, Instant};

use crate::format::bytes_short;
use crate::snapshot::{BatterySnapshot, PowerSource, Pressure, Process, Snapshot, Thermal};

/// How long a new verdict must persist before it is shown.
pub const VERDICT_HOLD: Duration = Duration::from_secs(5);
/// One process using at least this many cores makes the Mac "busy".
pub const BUSY_PROCESS_CORES: f32 = 1.5;
/// Whole-machine CPU activity (0..=1) that makes the Mac "busy".
pub const BUSY_TOTAL: f32 = 0.75;
/// Swap growing faster than this (bytes per second) means memory is tight.
pub const SWAP_GROWTH_BPS: f64 = 1024.0 * 1024.0;
/// Battery discharge above this many watts is a drain worth naming.
pub const BATTERY_DRAIN_W: f32 = 15.0;

/// The thermal state the verdict calls throttling (and at or above).
pub const THROTTLE_THERMAL: Thermal = Thermal::Serious;
/// The memory pressure the verdict calls tight (and at or above).
pub const TIGHT_PRESSURE: Pressure = Pressure::Warn;

/// The process using the most CPU, with its per-core percent.
#[must_use]
pub fn top_process(snap: &Snapshot) -> Option<(&Process, f32)> {
    snap.processes
        .iter()
        .filter_map(|p| p.cpu.filter(|cpu| cpu.is_finite()).map(|cpu| (p, cpu)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

/// Watts drawn from the battery while it powers the Mac; `None` on AC,
/// on desktops and when the gauge gives no reading.
#[must_use]
pub fn battery_drain_w(battery: &BatterySnapshot) -> Option<f32> {
    Some(battery)
        .filter(|b| b.present && b.source == PowerSource::Battery)
        .and_then(|b| b.watts)
        .filter(|w| w.is_finite())
        .map(f32::abs)
}

/// Kinds in order of severity: the first one that matches wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VerdictKind {
    Normal,
    Busy,
    BatteryDrain,
    MemoryTight,
    Throttling,
}

impl VerdictKind {
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Busy => "busy",
            Self::BatteryDrain => "battery_drain",
            Self::MemoryTight => "memory_tight",
            Self::Throttling => "throttling",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub kind: VerdictKind,
    /// The one line: "All normal", "Busy: Safari using 3.1 cores", ...
    pub headline: String,
    /// Why, one fact per line.
    pub evidence: Vec<String>,
    /// The process the verdict names, so a click can select it.
    pub pid: Option<u32>,
}

impl Verdict {
    #[must_use]
    pub fn normal() -> Self {
        Self {
            kind: VerdictKind::Normal,
            headline: String::from("All normal"),
            evidence: Vec::new(),
            pid: None,
        }
    }
}

/// Signals that are not in the snapshot itself: rates over time and data
/// other collectors add later. `None` means unknown, never zero.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Signals {
    /// Swap growth in bytes per second since the previous snapshot.
    pub swap_growth_bps: Option<f64>,
    /// Watts drawn from the battery while unplugged.
    pub battery_drain_w: Option<f32>,
}

fn finite(value: f32) -> Option<f32> {
    value.is_finite().then_some(value)
}

/// Judge one snapshot. Rate-based rules (busy) wait for a primed snapshot.
#[must_use]
pub fn assess(snap: &Snapshot, signals: &Signals) -> Verdict {
    throttling(snap)
        .or_else(|| memory_tight(snap, signals))
        .or_else(|| battery_drain(signals))
        .or_else(|| busy(snap))
        .unwrap_or_else(Verdict::normal)
}

fn throttling(snap: &Snapshot) -> Option<Verdict> {
    if snap.thermal < THROTTLE_THERMAL {
        return None;
    }
    let word = snap.thermal.word().unwrap_or("serious");
    let mut evidence = vec![format!("thermal pressure: {word}")];
    if let Some(temp) = snap.cpu.temp_c.and_then(finite) {
        evidence.push(format!("cpu temperature: {temp:.0} °C"));
    }
    if let Some(temp) = snap.gpu.and_then(|gpu| gpu.temp_c).and_then(finite) {
        evidence.push(format!("gpu temperature: {temp:.0} °C"));
    }
    if snap.fans.is_present() {
        evidence.push(format!(
            "fans: {:.0}% of max",
            snap.fans.peak_ratio() * 100.0
        ));
    }
    Some(Verdict {
        kind: VerdictKind::Throttling,
        headline: format!("Throttling: thermal pressure is {word}"),
        evidence,
        pid: None,
    })
}

fn memory_tight(snap: &Snapshot, signals: &Signals) -> Option<Verdict> {
    let mem = &snap.memory;
    let growing = signals
        .swap_growth_bps
        .is_some_and(|bps| bps.is_finite() && bps >= SWAP_GROWTH_BPS);
    let pressured = mem.pressure >= TIGHT_PRESSURE;
    if !pressured && !growing {
        return None;
    }
    let mut evidence = Vec::new();
    evidence.push(format!("memory pressure: {}", mem.pressure.label()));
    if mem.total_bytes > 0 {
        evidence.push(format!(
            "memory used: {} of {}",
            bytes_short(mem.used_bytes),
            bytes_short(mem.total_bytes)
        ));
    }
    if let Some(bps) = signals
        .swap_growth_bps
        .filter(|bps| bps.is_finite() && *bps > 0.0)
    {
        evidence.push(format!("swap growing: {}/s", bytes_short(bps as u64)));
    }
    let headline = if mem.swap_used_bytes > 0 {
        format!("Memory tight: {} swapped", bytes_short(mem.swap_used_bytes))
    } else {
        format!("Memory tight: pressure {}", mem.pressure.label())
    };
    Some(Verdict {
        kind: VerdictKind::MemoryTight,
        headline,
        evidence,
        pid: None,
    })
}

fn battery_drain(signals: &Signals) -> Option<Verdict> {
    let watts = signals
        .battery_drain_w
        .filter(|w| w.is_finite() && *w > BATTERY_DRAIN_W)?;
    Some(Verdict {
        kind: VerdictKind::BatteryDrain,
        headline: format!("Battery drain: {watts:.0} W"),
        evidence: vec![format!(
            "discharging at {watts:.1} W (threshold {BATTERY_DRAIN_W:.0} W)"
        )],
        pid: None,
    })
}

fn busy(snap: &Snapshot) -> Option<Verdict> {
    if !snap.primed {
        return None;
    }
    let top = top_process(snap);
    // `Process::cpu` is per-core percent: 100 means one fully busy core.
    let top_cores = top.map_or(0.0, |(_, cpu)| cpu / 100.0);
    let total = finite(snap.cpu.active).unwrap_or(0.0);
    if top_cores < BUSY_PROCESS_CORES && total < BUSY_TOTAL {
        return None;
    }
    let mut evidence = vec![format!("cpu active: {:.0}%", total * 100.0)];
    let (headline, pid) = match top {
        Some((p, _)) if top_cores >= 0.5 => {
            evidence.push(format!(
                "top process: {} (pid {}) at {:.1} cores",
                p.name, p.pid, top_cores
            ));
            (
                format!("Busy: {} using {top_cores:.1} cores", p.name),
                Some(p.pid),
            )
        }
        _ => (format!("Busy: CPU at {:.0}%", total * 100.0), None),
    };
    Some(Verdict {
        kind: VerdictKind::Busy,
        headline,
        evidence,
        pid,
    })
}

/// [`assess`] with hysteresis and swap-growth tracking across snapshots.
#[derive(Debug, Clone)]
pub struct VerdictEngine {
    hold: Duration,
    shown: Option<Verdict>,
    candidate: Option<(VerdictKind, Instant)>,
    last_swap: Option<(u64, Instant)>,
}

impl Default for VerdictEngine {
    fn default() -> Self {
        Self::new(VERDICT_HOLD)
    }
}

impl VerdictEngine {
    #[must_use]
    pub const fn new(hold: Duration) -> Self {
        Self {
            hold,
            shown: None,
            candidate: None,
            last_swap: None,
        }
    }

    /// Swap growth since the last call, in bytes per second.
    fn swap_growth(&mut self, swap: u64, now: Instant) -> Option<f64> {
        let previous = self.last_swap.replace((swap, now));
        let (before, then) = previous?;
        let secs = now.checked_duration_since(then)?.as_secs_f64();
        if secs <= 0.0 {
            return None;
        }
        // Swap shrinking is not growth; a reset reads as zero, not negative.
        Some(swap.saturating_sub(before) as f64 / secs)
    }

    /// Feed one snapshot taken at `now`; returns the verdict to show.
    pub fn update(
        &mut self,
        snap: &Snapshot,
        now: Instant,
        battery_drain_w: Option<f32>,
    ) -> &Verdict {
        let signals = Signals {
            swap_growth_bps: self.swap_growth(snap.memory.swap_used_bytes, now),
            battery_drain_w,
        };
        let fresh = assess(snap, &signals);
        let switch = match &self.shown {
            None => true,
            Some(shown) if shown.kind == fresh.kind => {
                self.candidate = None;
                true
            }
            Some(_) => match self.candidate {
                Some((kind, since)) if kind == fresh.kind => {
                    now.saturating_duration_since(since) >= self.hold
                }
                _ => {
                    self.candidate = Some((fresh.kind, now));
                    self.hold.is_zero()
                }
            },
        };
        if switch {
            self.candidate = None;
            self.shown = Some(fresh);
        }
        self.shown.get_or_insert_with(Verdict::normal)
    }

    #[must_use]
    pub fn shown(&self) -> Option<&Verdict> {
        self.shown.as_ref()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::snapshot::{ClusterKind, CoreSample, FanMetric, GpuSnapshot, Process};

    const GB: u64 = 1024 * 1024 * 1024;

    fn machine() -> Snapshot {
        let mut snap = Snapshot::empty();
        snap.primed = true;
        snap.soc.p_cores = 12;
        snap.soc.s_cores = 6;
        snap.cpu.cores = (0..18)
            .map(|i| CoreSample {
                kind: ClusterKind::Performance,
                index: i,
                scaled: 0.05,
                active: 0.05,
                freq_mhz: None,
            })
            .collect();
        snap.cpu.active = 0.05;
        snap.memory.total_bytes = 24 * GB;
        snap.memory.used_bytes = 12 * GB;
        snap
    }

    fn proc_at(name: &str, pid: u32, cpu: f32) -> Process {
        Process {
            pid,
            name: String::from(name),
            cpu: Some(cpu),
            ..Process::default()
        }
    }

    fn hog(cores: f32) -> Snapshot {
        let mut snap = machine();
        // Per-core percent: `cores` busy cores is cores × 100.
        snap.processes = vec![
            proc_at("idle helper", 10, 0.5),
            proc_at("Brave Browser", 42, cores * 100.0),
        ];
        snap
    }

    #[test]
    fn idle_machine_is_all_normal() {
        let verdict = assess(&machine(), &Signals::default());
        assert_eq!(verdict.kind, VerdictKind::Normal);
        assert_eq!(verdict.headline, "All normal");
        assert!(verdict.evidence.is_empty());
    }

    #[test]
    fn empty_snapshot_is_normal_not_a_crash() {
        let verdict = assess(&Snapshot::empty(), &Signals::default());
        assert_eq!(verdict.kind, VerdictKind::Normal);
    }

    #[test]
    fn cpu_hog_is_named_with_cores() {
        let verdict = assess(&hog(3.1), &Signals::default());
        assert_eq!(verdict.kind, VerdictKind::Busy);
        assert_eq!(verdict.headline, "Busy: Brave Browser using 3.1 cores");
        assert!(verdict.evidence.iter().any(|e| e.contains("pid 42")));
        assert_eq!(verdict.pid, Some(42), "a click on the footer selects it");
    }

    #[test]
    fn busy_without_a_culprit_names_no_pid() {
        let mut snap = machine();
        snap.cpu.active = 0.9;
        snap.processes = (0..40).map(|i| proc_at("worker", i, 2.0)).collect();
        assert_eq!(assess(&snap, &Signals::default()).pid, None);
    }

    #[test]
    fn one_core_process_is_not_busy() {
        let verdict = assess(&hog(1.0), &Signals::default());
        assert_eq!(verdict.kind, VerdictKind::Normal);
    }

    #[test]
    fn busy_threshold_is_inclusive_at_one_and_a_half_cores() {
        assert_eq!(
            assess(&hog(1.49), &Signals::default()).kind,
            VerdictKind::Normal
        );
        assert_eq!(
            assess(&hog(1.5), &Signals::default()).kind,
            VerdictKind::Busy
        );
    }

    #[test]
    fn unprimed_rates_never_call_busy() {
        let mut snap = hog(8.0);
        snap.primed = false;
        snap.cpu.active = 1.0;
        assert_eq!(assess(&snap, &Signals::default()).kind, VerdictKind::Normal);
    }

    #[test]
    fn spread_load_without_a_culprit_reports_cpu_percent() {
        let mut snap = machine();
        snap.cpu.active = 0.9;
        snap.processes = (0..40).map(|i| proc_at("worker", i, 2.0)).collect();
        let verdict = assess(&snap, &Signals::default());
        assert_eq!(verdict.kind, VerdictKind::Busy);
        assert_eq!(verdict.headline, "Busy: CPU at 90%");
    }

    #[test]
    fn nan_process_cpu_is_ignored() {
        let mut snap = machine();
        snap.processes = vec![proc_at("broken", 1, f32::NAN), proc_at("ok", 2, 1.0)];
        assert_eq!(assess(&snap, &Signals::default()).kind, VerdictKind::Normal);
    }

    #[test]
    fn swap_growth_is_memory_tight() {
        let mut snap = machine();
        snap.memory.swap_used_bytes = 2 * GB + GB / 10;
        let signals = Signals {
            swap_growth_bps: Some(8.0 * 1024.0 * 1024.0),
            ..Signals::default()
        };
        let verdict = assess(&snap, &signals);
        assert_eq!(verdict.kind, VerdictKind::MemoryTight);
        assert!(
            verdict.headline.starts_with("Memory tight: "),
            "{}",
            verdict.headline
        );
        assert!(
            verdict.headline.ends_with(" swapped"),
            "{}",
            verdict.headline
        );
        assert!(
            verdict
                .evidence
                .iter()
                .any(|e| e.starts_with("swap growing"))
        );
    }

    #[test]
    fn static_swap_alone_is_not_tight() {
        let mut snap = machine();
        snap.memory.swap_used_bytes = 4 * GB;
        let signals = Signals {
            swap_growth_bps: Some(0.0),
            ..Signals::default()
        };
        assert_eq!(assess(&snap, &signals).kind, VerdictKind::Normal);
    }

    #[test]
    fn pressure_warn_is_memory_tight_even_without_swap() {
        let mut snap = machine();
        snap.memory.pressure = Pressure::Warn;
        let verdict = assess(&snap, &Signals::default());
        assert_eq!(verdict.kind, VerdictKind::MemoryTight);
        assert_eq!(verdict.headline, "Memory tight: pressure warn");
    }

    #[test]
    fn thermal_serious_is_throttling_and_beats_everything() {
        let mut snap = hog(6.0);
        snap.thermal = Thermal::Serious;
        snap.memory.pressure = Pressure::Critical;
        snap.cpu.temp_c = Some(98.4);
        snap.gpu = Some(GpuSnapshot {
            temp_c: Some(91.0),
            ..GpuSnapshot::default()
        });
        snap.fans.fans = vec![FanMetric {
            name: String::from("Left"),
            rpm: 5400,
            max_rpm: 6000,
        }];
        let verdict = assess(&snap, &Signals::default());
        assert_eq!(verdict.kind, VerdictKind::Throttling);
        assert_eq!(verdict.headline, "Throttling: thermal pressure is serious");
        assert!(
            verdict
                .evidence
                .contains(&String::from("cpu temperature: 98 °C"))
        );
        assert!(verdict.evidence.contains(&String::from("fans: 90% of max")));
    }

    #[test]
    fn thermal_fair_is_not_throttling() {
        let mut snap = machine();
        snap.thermal = Thermal::Fair;
        assert_eq!(assess(&snap, &Signals::default()).kind, VerdictKind::Normal);
    }

    #[test]
    fn battery_drain_over_fifteen_watts() {
        let snap = machine();
        let at = |w: f32| {
            assess(
                &snap,
                &Signals {
                    battery_drain_w: Some(w),
                    ..Signals::default()
                },
            )
        };
        assert_eq!(at(15.0).kind, VerdictKind::Normal);
        let drain = at(18.2);
        assert_eq!(drain.kind, VerdictKind::BatteryDrain);
        assert_eq!(drain.headline, "Battery drain: 18 W");
        assert_eq!(at(f32::INFINITY).kind, VerdictKind::Normal);
    }

    /// Every combination of conditions resolves to the most severe one.
    #[test]
    fn verdict_matrix_severity_order() {
        for mask in 0_u8..32 {
            let mut snap = machine();
            let mut signals = Signals::default();
            let mut expected = VerdictKind::Normal;
            if mask & 1 != 0 {
                snap.processes = vec![proc_at("hog", 7, 900.0)];
                expected = expected.max(VerdictKind::Busy);
            }
            if mask & 2 != 0 {
                signals.battery_drain_w = Some(30.0);
                expected = expected.max(VerdictKind::BatteryDrain);
            }
            if mask & 4 != 0 {
                snap.memory.pressure = Pressure::Critical;
                expected = expected.max(VerdictKind::MemoryTight);
            }
            if mask & 8 != 0 {
                signals.swap_growth_bps = Some(1e9);
                snap.memory.swap_used_bytes = GB;
                expected = expected.max(VerdictKind::MemoryTight);
            }
            if mask & 16 != 0 {
                snap.thermal = Thermal::Critical;
                expected = expected.max(VerdictKind::Throttling);
            }
            let verdict = assess(&snap, &signals);
            assert_eq!(verdict.kind, expected, "mask {mask:05b}");
            assert!(!verdict.headline.is_empty());
            assert_eq!(
                verdict.evidence.is_empty(),
                expected == VerdictKind::Normal,
                "mask {mask:05b}: evidence {:?}",
                verdict.evidence
            );
        }
    }

    #[test]
    fn engine_first_verdict_is_immediate() {
        let mut engine = VerdictEngine::default();
        let t0 = Instant::now();
        assert_eq!(engine.update(&hog(4.0), t0, None).kind, VerdictKind::Busy);
    }

    #[test]
    fn engine_spike_shorter_than_hold_never_shows() {
        let mut engine = VerdictEngine::new(Duration::from_secs(5));
        let t0 = Instant::now();
        engine.update(&machine(), t0, None);
        for step in 1..=4 {
            let now = t0 + Duration::from_secs(step);
            assert_eq!(
                engine.update(&hog(4.0), now, None).kind,
                VerdictKind::Normal
            );
        }
        let back = t0 + Duration::from_secs(5);
        assert_eq!(
            engine.update(&machine(), back, None).kind,
            VerdictKind::Normal
        );
        // The candidate reset: a new spike starts its clock over.
        let again = t0 + Duration::from_secs(6);
        assert_eq!(
            engine.update(&hog(4.0), again, None).kind,
            VerdictKind::Normal
        );
    }

    #[test]
    fn engine_switches_after_hold_and_back_after_hold() {
        let mut engine = VerdictEngine::new(Duration::from_secs(5));
        let t0 = Instant::now();
        engine.update(&machine(), t0, None);
        let mut shown = Vec::new();
        for s in 1..=20_u64 {
            let snap = if (1..=10).contains(&s) {
                hog(4.0)
            } else {
                machine()
            };
            let kind = engine.update(&snap, t0 + Duration::from_secs(s), None).kind;
            shown.push(kind);
        }
        // Busy from s=1 shows at s=6 (held 5 s), Normal from s=11 shows at s=16.
        let busy: Vec<u64> = shown
            .iter()
            .zip(1_u64..)
            .filter(|(k, _)| **k == VerdictKind::Busy)
            .map(|(_, s)| s)
            .collect();
        assert_eq!(busy, (6..=15).collect::<Vec<_>>());
    }

    #[test]
    fn engine_same_kind_refreshes_numbers_immediately() {
        let mut engine = VerdictEngine::new(Duration::from_secs(5));
        let t0 = Instant::now();
        engine.update(&hog(2.0), t0, None);
        let later = engine.update(&hog(5.0), t0 + Duration::from_millis(250), None);
        assert_eq!(later.headline, "Busy: Brave Browser using 5.0 cores");
    }

    #[test]
    fn engine_tracks_swap_growth_between_snapshots() {
        let mut engine = VerdictEngine::new(Duration::ZERO);
        let t0 = Instant::now();
        let mut snap = machine();
        snap.memory.swap_used_bytes = GB;
        assert_eq!(engine.update(&snap, t0, None).kind, VerdictKind::Normal);
        snap.memory.swap_used_bytes = GB + 64 * 1024 * 1024;
        let verdict = engine.update(&snap, t0 + Duration::from_secs(2), None);
        assert_eq!(verdict.kind, VerdictKind::MemoryTight);
        // Swap shrinking is never growth.
        snap.memory.swap_used_bytes = 0;
        let verdict = engine.update(&snap, t0 + Duration::from_secs(4), None);
        assert_eq!(verdict.kind, VerdictKind::Normal);
    }

    #[test]
    fn engine_ignores_time_going_backwards() {
        let mut engine = VerdictEngine::new(Duration::ZERO);
        let t0 = Instant::now() + Duration::from_secs(100);
        let mut snap = machine();
        engine.update(&snap, t0, None);
        snap.memory.swap_used_bytes = 10 * GB;
        let verdict = engine.update(&snap, t0.checked_sub(Duration::from_secs(1)).unwrap(), None);
        assert_eq!(verdict.kind, VerdictKind::Normal);
    }

    /// Property sweep: whatever the inputs, a verdict has a headline and
    /// its kind's evidence rule holds; the engine never panics.
    #[test]
    fn property_sweep_never_panics_and_stays_consistent() {
        let mut engine = VerdictEngine::new(Duration::from_secs(1));
        let t0 = Instant::now();
        let mut seed = 0x2545_f491_u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for i in 0..500_u64 {
            let mut snap = machine();
            snap.cpu.active = (next() % 1200) as f32 / 1000.0;
            snap.memory.swap_used_bytes = u64::from(next()) * 4096;
            snap.memory.pressure = match next() % 3 {
                0 => Pressure::Nominal,
                1 => Pressure::Warn,
                _ => Pressure::Critical,
            };
            snap.thermal = match next() % 4 {
                0 => Thermal::Nominal,
                1 => Thermal::Fair,
                2 => Thermal::Serious,
                _ => Thermal::Critical,
            };
            snap.processes = (0..next() % 5)
                .map(|p| proc_at("p", p, (next() % 10_000) as f32 / 100.0))
                .collect();
            let drain = (next() % 2 == 0).then(|| (next() % 40) as f32);
            let verdict = engine
                .update(&snap, t0 + Duration::from_millis(i * 250), drain)
                .clone();
            assert!(!verdict.headline.is_empty());
            assert_eq!(
                verdict.kind == VerdictKind::Normal,
                verdict.headline == "All normal"
            );
        }
    }

    #[test]
    fn slugs_are_snake_case_and_unique() {
        let kinds = [
            VerdictKind::Normal,
            VerdictKind::Busy,
            VerdictKind::BatteryDrain,
            VerdictKind::MemoryTight,
            VerdictKind::Throttling,
        ];
        let mut slugs: Vec<&str> = kinds.iter().map(|k| k.slug()).collect();
        assert!(
            slugs
                .iter()
                .all(|s| s.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
        );
        slugs.sort_unstable();
        slugs.dedup();
        assert_eq!(slugs.len(), kinds.len());
    }
}
