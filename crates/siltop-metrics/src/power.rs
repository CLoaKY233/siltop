//! Package power without sudo.
//!
//! - System watts from the SMC `PSTR` key, every sensor sweep.
//! - CPU watts from model-specific SMC rails (`smc_keys::cpu_rails`),
//!   smoothed over [`SMOOTH_READS`] sweeps.
//! - An [`EnergyHold`] per `IOReport` "Energy Model" channel. On macOS 27 the
//!   PMGR counters publish in batches 2–108 s apart, so a rate is computed
//!   between publications (by driver timestamp), held until the next one,
//!   and dropped once it is older than [`POWER_STALE_AFTER`]. The `IOReport`
//!   thread feeds an [`EnergyBank`] on its 1 s cadence and publishes
//!   [`RailReadings`]; the sampler lays them over the SMC figures with
//!   [`RailReadings::apply`].
//!
//! Nothing here reports 0 W for missing data: absent is `None`.

use std::time::Duration;

use siltop_core::{POWER_STALE_AFTER, PowerSnapshot, Reading, ReadingSource, Throttle};

use crate::smc::fourcc;
use crate::smc_keys;

/// Whole-system input power, `flt` watts.
const SYSTEM_KEY: &str = "PSTR";
/// CPU rails are averaged over this many sweeps.
pub(crate) const SMOOTH_READS: usize = 3;
/// A CPU (or any rail) reading may exceed system power by this much before
/// it is treated as a glitch; the rails and `PSTR` are not sampled at the
/// same instant.
const CEILING_SLACK_W: f32 = 5.0;
/// No Mac draws this much; anything above is a decoding error.
const MAX_PLAUSIBLE_W: f64 = 1_000.0;
/// Publications closer than this are ignored as a rate base: the division
/// would amplify timestamp jitter.
const MIN_SPAN_NS: u64 = 10_000_000;
/// A wall-clock gap longer than this many cadences means the Mac slept.
const SLEEP_CADENCES: u64 = 3;

fn key(name: &str) -> Option<u32> {
    let bytes: [u8; 4] = name.as_bytes().try_into().ok()?;
    Some(fourcc(bytes))
}

/// Watts as `f32`, or `None` for NaN, negative or absurd values.
pub(crate) fn plausible_watts(watts: f64) -> Option<f32> {
    (watts.is_finite() && (0.0..=MAX_PLAUSIBLE_W).contains(&watts)).then_some(watts as f32)
}

/// Rolling mean of the last [`SMOOTH_READS`] values.
#[derive(Debug, Clone, Default)]
pub(crate) struct Smoother {
    ring: [f32; SMOOTH_READS],
    len: usize,
    next: usize,
}

impl Smoother {
    pub(crate) fn push(&mut self, value: f32) -> f32 {
        if let Some(slot) = self.ring.get_mut(self.next) {
            *slot = value;
        }
        self.next = (self.next + 1) % SMOOTH_READS;
        self.len = (self.len + 1).min(SMOOTH_READS);
        let sum: f32 = self.ring.iter().take(self.len).sum();
        sum / self.len as f32
    }

    /// The largest value in the window, or `None` before the first push.
    pub(crate) fn max(&self) -> Option<f32> {
        self.ring.iter().take(self.len).copied().reduce(f32::max)
    }
}

/// SMC power keys for this machine, resolved once when the SMC opens.
#[derive(Debug, Clone)]
pub(crate) struct Rails {
    system: Option<u32>,
    cpu: Vec<u32>,
    smooth: Smoother,
    /// Recent `PSTR`, over the same window as the CPU mean: the SMC updates
    /// `PSTR` more slowly than the rails, so at a load step the rails lead
    /// it by up to 30 W for a sweep or two.
    recent_system: Smoother,
}

impl Rails {
    /// Keeps `PSTR` if the SMC has it, and the CPU rails only when this
    /// exact model is mapped and every rail key exists: a partial sum would
    /// under-report.
    pub(crate) fn new(model: &str, mut exists: impl FnMut(u32) -> bool) -> Self {
        let system = key(SYSTEM_KEY).filter(|&k| exists(k));
        let cpu: Vec<u32> = smc_keys::cpu_rails(model)
            .map(|names| names.iter().filter_map(|n| key(n)).collect())
            .unwrap_or_default();
        let cpu = if cpu.iter().all(|&k| exists(k)) {
            cpu
        } else {
            Vec::new()
        };
        Self {
            system,
            cpu,
            smooth: Smoother::default(),
            recent_system: Smoother::default(),
        }
    }

    /// One sweep. `read` returns a key's value in watts.
    pub(crate) fn sample(&mut self, mut read: impl FnMut(u32) -> Option<f64>) -> PowerSnapshot {
        let system = self
            .system
            .and_then(&mut read)
            .and_then(plausible_watts)
            .filter(|&w| w > 0.0)
            .map(Reading::measured);
        if let Some(s) = system {
            self.recent_system.push(s.watts);
        }
        let ceiling = self.recent_system.max().map(Reading::measured);
        let cpu = self.cpu_watts(&mut read).and_then(|raw| {
            let smoothed = self.smooth.push(raw);
            // Prefer the mean; fall back to this sweep's sum if the mean is
            // over the ceiling; report nothing if both are (a glitch).
            [smoothed, raw]
                .into_iter()
                .find(|&w| under_ceiling(w, ceiling))
                .map(Reading::measured)
        });
        PowerSnapshot {
            system,
            cpu,
            ..PowerSnapshot::default()
        }
    }

    fn cpu_watts(&mut self, read: &mut impl FnMut(u32) -> Option<f64>) -> Option<f32> {
        if self.cpu.is_empty() {
            return None;
        }
        let mut sum = 0.0_f32;
        let mut any_live = false;
        for &k in &self.cpu {
            let w = read(k).and_then(plausible_watts)?;
            any_live |= w > 0.0;
            sum += w;
        }
        // Every rail at exactly 0.0 is the SMC before it has populated them,
        // not an idle CPU (idle reads about 0.3 W).
        any_live.then_some(sum)
    }
}

fn under_ceiling(watts: f32, system: Option<Reading>) -> bool {
    system.is_none_or(|s| watts <= s.watts + CEILING_SLACK_W)
}

/// Energy unit of an `IOReport` channel, from its unit label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
pub(crate) enum EnergyUnit {
    MilliJoules,
    MicroJoules,
    NanoJoules,
}

impl EnergyUnit {
    pub(crate) fn parse(label: &str) -> Option<Self> {
        match label.trim() {
            "mJ" => Some(Self::MilliJoules),
            "uJ" | "µJ" | "μJ" => Some(Self::MicroJoules),
            "nJ" => Some(Self::NanoJoules),
            _ => None,
        }
    }

    pub(crate) fn joules(self, raw: u64) -> f64 {
        let per_joule = match self {
            Self::MilliJoules => 1e3,
            Self::MicroJoules => 1e6,
            Self::NanoJoules => 1e9,
        };
        raw as f64 / per_joule
    }
}

/// Mach absolute ticks to nanoseconds with the `mach_timebase_info` ratio,
/// without overflowing on 24 MHz tick counts.
pub(crate) fn ticks_to_ns(ticks: u64, numer: u32, denom: u32) -> Option<u64> {
    if denom == 0 {
        return None;
    }
    let ns = u128::from(ticks) * u128::from(numer) / u128::from(denom);
    u64::try_from(ns).ok()
}

/// What a channel reports: `IOReport` gives a running total from a raw
/// sample and a difference from a delta sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Counter {
    Cumulative(u64),
    #[cfg_attr(not(test), allow(dead_code))]
    Delta(u64),
}

/// One read of one energy channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Observation {
    pub(crate) counter: Counter,
    /// Driver timestamp of the latest publication, in ns on the same clock
    /// as `now_ns`.
    pub(crate) published_ns: u64,
    /// When the read happened (mach absolute time, ns).
    pub(crate) now_ns: u64,
    /// A clock that keeps running while the Mac sleeps (continuous time,
    /// ns); only its gaps matter.
    pub(crate) wall_ns: u64,
}

/// Turns a batched energy counter into watts that are held between
/// publications and dropped when stale.
#[derive(Debug, Clone)]
pub(crate) struct EnergyHold {
    unit: EnergyUnit,
    cadence_ns: u64,
    running: u64,
    /// Energy total and timestamp of the last publication used as a base.
    base: Option<(u64, u64)>,
    /// The last rate and the publication it was measured at.
    rate: Option<(f32, u64)>,
    last_wall_ns: Option<u64>,
}

impl EnergyHold {
    pub(crate) fn new(unit: EnergyUnit, cadence: Duration) -> Self {
        Self {
            unit,
            cadence_ns: u64::try_from(cadence.as_nanos()).unwrap_or(u64::MAX),
            running: 0,
            base: None,
            rate: None,
            last_wall_ns: None,
        }
    }

    fn forget(&mut self) {
        self.base = None;
        self.rate = None;
    }

    /// Feeds one read and returns the current reading: `Measured` when a new
    /// publication arrived, `Held` while the last one is younger than
    /// [`POWER_STALE_AFTER`], otherwise `None`.
    pub(crate) fn observe(&mut self, obs: Observation) -> Option<Reading> {
        let slept = self.last_wall_ns.is_some_and(|last| {
            obs.wall_ns < last
                || obs.wall_ns - last > self.cadence_ns.saturating_mul(SLEEP_CADENCES)
        });
        if slept {
            self.forget();
        }
        self.last_wall_ns = Some(obs.wall_ns);
        let total = match obs.counter {
            Counter::Cumulative(total) => total,
            Counter::Delta(delta) => self.running.saturating_add(delta),
        };
        self.running = total;
        let mut fresh = false;
        match self.base {
            None => self.base = Some((total, obs.published_ns)),
            Some((e0, t0)) if total < e0 || obs.published_ns < t0 => {
                // The counter or its clock restarted: rebase, trust nothing.
                self.forget();
                self.base = Some((total, obs.published_ns));
            }
            Some((e0, t0)) if obs.published_ns - t0 >= MIN_SPAN_NS => {
                let secs = (obs.published_ns - t0) as f64 / 1e9;
                self.rate = plausible_watts(self.unit.joules(total - e0) / secs)
                    .map(|w| (w, obs.published_ns));
                self.base = Some((total, obs.published_ns));
                fresh = self.rate.is_some();
            }
            // No new publication (or one too close to rate): keep holding.
            Some(_) => {}
        }
        let (watts, published) = self.rate?;
        let age = Duration::from_nanos(obs.now_ns.saturating_sub(published));
        if age > POWER_STALE_AFTER {
            return None;
        }
        let source = if fresh {
            ReadingSource::Measured
        } else {
            ReadingSource::Held { age }
        };
        Some(Reading { watts, source })
    }
}

/// The rails an "Energy Model" channel rolls up into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rail {
    Cpu,
    Gpu,
    Ane,
    Dram,
}

/// Exact-name roll-up. The per-cluster, per-core, `PACC`, `DTL` and `SRAM`
/// channels overlap the totals (summing them over-counts about 3×), so only
/// the totals, `ANE<n>` and `DRAM<n>` are accepted.
pub(crate) fn rollup(name: &str) -> Option<Rail> {
    let numbered = |prefix: &str| {
        name.strip_prefix(prefix)
            .is_some_and(|n| n.len() <= 2 && n.bytes().all(|b| b.is_ascii_digit()))
    };
    match name {
        "CPU Energy" => Some(Rail::Cpu),
        "GPU Energy" => Some(Rail::Gpu),
        _ if numbered("ANE") => Some(Rail::Ane),
        _ if numbered("DRAM") => Some(Rail::Dram),
        _ => None,
    }
}

/// Where a roll-up channel sits: the whole package, or one die of an
/// Ultra (`DIE_<n>_` prefix, the naming macmon and asitop report there).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    Package,
    Die(u8),
}

/// [`rollup`], also accepting a `DIE_<n>_` prefix.
pub(crate) fn rollup_scoped(name: &str) -> Option<(Rail, Scope)> {
    if let Some(rail) = rollup(name) {
        return Some((rail, Scope::Package));
    }
    let rest = name.strip_prefix("DIE_")?;
    let (die, channel) = rest.split_once('_')?;
    let die: u8 = (die.len() <= 2).then(|| die.parse().ok())??;
    Some((rollup(channel)?, Scope::Die(die)))
}

/// Sums readings of one rail: the watts add, and the result is only as
/// fresh as its oldest part.
pub(crate) fn combine(a: Reading, b: Reading) -> Reading {
    use ReadingSource::{Estimated, Held, Measured};
    let source = match (a.source, b.source) {
        (Estimated, _) | (_, Estimated) => Estimated,
        (Held { age: x }, Held { age: y }) => Held { age: x.max(y) },
        (Held { age }, Measured) | (Measured, Held { age }) => Held { age },
        (Measured, Measured) => Measured,
    };
    Reading {
        watts: a.watts + b.watts,
        source,
    }
}

/// One channel's read, as the `IOReport` collector hands it over.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ChannelRead<'a> {
    pub(crate) name: &'a str,
    pub(crate) unit: &'a str,
    pub(crate) obs: Observation,
}

/// Energy holds for the channels that roll up, created on first sight.
#[derive(Debug, Clone)]
pub(crate) struct EnergyBank {
    cadence: Duration,
    holds: Vec<(String, Rail, EnergyHold)>,
}

impl EnergyBank {
    pub(crate) fn new(cadence: Duration) -> Self {
        Self {
            cadence,
            holds: Vec::new(),
        }
    }

    /// Channels the bank is tracking.
    pub(crate) fn tracked(&self) -> usize {
        self.holds.len()
    }

    /// Feeds one sweep and rolls the channels up per rail.
    ///
    /// A rail made of several channels (`DIE_0_`/`DIE_1_` totals on an
    /// Ultra, `DRAM0..n`) is only as whole as its parts: when any channel
    /// ever seen for it has no reading this sweep (stale, rebased, or
    /// missing), the sum would pass off one die's watts as the chip's, so
    /// that rail is `None` instead.
    pub(crate) fn observe<'a>(
        &mut self,
        reads: impl IntoIterator<Item = ChannelRead<'a>>,
    ) -> RailReadings {
        // Per rail: the package total, and the sum over dies. A package
        // total wins, so a chip that exposes both is never counted twice.
        let mut rolled: [[Option<Reading>; 2]; 4] = [[None; 2]; 4];
        let mut partial = [[false; 2]; 4];
        let mut read_now = vec![false; self.holds.len()];
        for read in reads {
            let Some((rail, scope)) = rollup_scoped(read.name) else {
                continue;
            };
            let part = (rail as usize, usize::from(scope != Scope::Package));
            let Some(unit) = EnergyUnit::parse(read.unit) else {
                continue;
            };
            let index = self
                .holds
                .iter()
                .position(|(n, _, _)| n == read.name)
                .unwrap_or_else(|| {
                    let hold = EnergyHold::new(unit, self.cadence);
                    self.holds.push((read.name.to_string(), rail, hold));
                    self.holds.len() - 1
                });
            if let Some(seen) = read_now.get_mut(index) {
                *seen = true;
            }
            let Some(reading) = self
                .holds
                .get_mut(index)
                .and_then(|(_, _, hold)| hold.observe(read.obs))
            else {
                partial[part.0][part.1] = true;
                continue;
            };
            let slot = &mut rolled[part.0][part.1];
            *slot = Some(slot.map_or(reading, |sum| combine(sum, reading)));
        }
        // A known channel this sweep did not carry leaves its rail short.
        let missing = self.holds.iter().zip(&read_now).filter(|(_, seen)| !**seen);
        for ((name, _, _), _) in missing {
            if let Some((rail, scope)) = rollup_scoped(name) {
                partial[rail as usize][usize::from(scope != Scope::Package)] = true;
            }
        }
        let whole = |sum: Option<Reading>, short: bool| sum.filter(|_| !short);
        let [cpu, gpu, ane, dram] = std::array::from_fn(|rail| {
            let [package, dies] = rolled[rail];
            let [package_short, dies_short] = partial[rail];
            whole(package, package_short).or(whole(dies, dies_short))
        });
        RailReadings {
            cpu,
            gpu,
            ane,
            dram,
        }
    }

    /// [`observe`](Self::observe) then [`RailReadings::apply`].
    #[cfg(test)]
    pub(crate) fn sweep<'a>(
        &mut self,
        reads: impl IntoIterator<Item = ChannelRead<'a>>,
        power: &mut PowerSnapshot,
    ) {
        self.observe(reads).apply(power);
    }
}

/// Energy-counter watts per rail from one bank sweep. Each is `None` when
/// the channel is missing, has not published twice yet, or went stale.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct RailReadings {
    pub(crate) cpu: Option<Reading>,
    pub(crate) gpu: Option<Reading>,
    pub(crate) ane: Option<Reading>,
    pub(crate) dram: Option<Reading>,
}

impl RailReadings {
    const fn rails(&self) -> [Option<Reading>; 4] {
        [self.cpu, self.gpu, self.ane, self.dram]
    }

    /// The readings `elapsed` after the sweep that made them: a measured
    /// value stays measured for `fresh_for`, then counts as held; held ages
    /// grow; anything past [`POWER_STALE_AFTER`] is dropped.
    pub(crate) fn aged(self, elapsed: Duration, fresh_for: Duration) -> Self {
        let age = |r: Option<Reading>| {
            let r = r?;
            let source = match r.source {
                ReadingSource::Held { age } => ReadingSource::Held {
                    age: age.saturating_add(elapsed),
                },
                ReadingSource::Measured if elapsed > fresh_for => {
                    ReadingSource::Held { age: elapsed }
                }
                other => other,
            };
            Some(Reading { source, ..r }).filter(|r| !r.is_stale())
        };
        Self {
            cpu: age(self.cpu),
            gpu: age(self.gpu),
            ane: age(self.ane),
            dram: age(self.dram),
        }
    }

    /// Lays the rails over the SMC figures: `gpu`, `ane` and `dram` always,
    /// `cpu` only when the SMC rails left it empty (they are live and exact
    /// on mapped models). Rails over the system ceiling are dropped.
    pub(crate) fn apply(self, power: &mut PowerSnapshot) {
        let [cpu, gpu, ane, dram] = self
            .rails()
            .map(|r| r.filter(|r| under_ceiling(r.watts, power.system)));
        if power.cpu.is_none() {
            power.cpu = cpu;
        }
        power.gpu = gpu;
        power.ane = ane;
        power.dram = dram;
    }
}

// Throttle shares land with the throttle collector; only tests drive them.
/// Share of `residency` (state name, ticks) spent outside `unlimited`, or
/// `None` when the channel is empty or does not have those states.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn limited_share(residency: &[(&str, u64)], unlimited: &[&str]) -> Option<f32> {
    let total: u128 = residency.iter().map(|&(_, t)| u128::from(t)).sum();
    if total == 0 || !residency.iter().any(|(n, _)| unlimited.contains(n)) {
        return None;
    }
    let free: u128 = residency
        .iter()
        .filter(|(n, _)| unlimited.contains(n))
        .map(|&(_, t)| u128::from(t))
        .sum();
    Some((1.0 - free as f64 / total as f64).clamp(0.0, 1.0) as f32)
}

/// Throttle shares from the "GPU Stats" `GPU_CLTM` and `PWRCTRL` channels
/// and the "PPM Stats" `Sys1s` CPMS channel. Frequency alone is never used:
/// the Super cluster's all-core ceiling sits well below its single-core max.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn throttle(
    gpu_cltm: &[(&str, u64)],
    gpu_pwrctrl: &[(&str, u64)],
    cpms: &[(&str, u64)],
) -> Throttle {
    Throttle {
        gpu_cltm: limited_share(gpu_cltm, &["NO_CLTM"]),
        gpu_power_limited: limited_share(gpu_pwrctrl, &["PERF", "IDLE_OFF"]),
        cpms_reduction: limited_share(cpms, &["0%"]),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// Every "Energy Model" channel on the Mac17,8 (name, unit label).
    const CHANNELS: &str = include_str!("fixtures/energy_model_channels.tsv");
    /// 20 minutes of raw CPU/DRAM/GPU energy reads every 5 s (24 MHz ticks).
    const RAWTS: &str = include_str!("fixtures/energy_rawts_20min.txt");
    /// `IOReport` CPU/GPU energy beside the SMC rails and PSTR, every ~0.27 s,
    /// through idle and `load.sh 18` phases.
    const CALIB: &str = include_str!("fixtures/rails_calib.txt");

    const SEC: u64 = 1_000_000_000;
    const CADENCE: Duration = Duration::from_secs(1);

    fn channels() -> Vec<(&'static str, &'static str)> {
        CHANNELS
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .collect()
    }

    fn ticks(t: u64) -> u64 {
        ticks_to_ns(t, 125, 3).unwrap()
    }

    fn obs(total: u64, published_ns: u64, now_ns: u64) -> Observation {
        Observation {
            counter: Counter::Cumulative(total),
            published_ns,
            now_ns,
            wall_ns: now_ns,
        }
    }

    fn hold_mj() -> EnergyHold {
        EnergyHold::new(EnergyUnit::MilliJoules, CADENCE)
    }

    fn held_age(r: Option<Reading>) -> Option<Duration> {
        match r?.source {
            ReadingSource::Held { age } => Some(age),
            _ => None,
        }
    }

    fn close(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    // ---------- units and clocks ----------

    #[test]
    fn unit_labels_parse_including_both_micro_signs() {
        assert_eq!(EnergyUnit::parse("mJ"), Some(EnergyUnit::MilliJoules));
        assert_eq!(EnergyUnit::parse(" mJ "), Some(EnergyUnit::MilliJoules));
        assert_eq!(EnergyUnit::parse("uJ"), Some(EnergyUnit::MicroJoules));
        assert_eq!(EnergyUnit::parse("\u{b5}J"), Some(EnergyUnit::MicroJoules));
        assert_eq!(EnergyUnit::parse("\u{3bc}J"), Some(EnergyUnit::MicroJoules));
        assert_eq!(EnergyUnit::parse("nJ"), Some(EnergyUnit::NanoJoules));
        for bad in [
            "",
            "J",
            "mj",
            "MJ",
            "kJ",
            "pJ",
            "24Mticks",
            "mJ/s",
            "nanojoules",
        ] {
            assert_eq!(EnergyUnit::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn joules_scale_by_unit() {
        assert!((EnergyUnit::MilliJoules.joules(1_500) - 1.5).abs() < 1e-12);
        assert!((EnergyUnit::MicroJoules.joules(2_000_000) - 2.0).abs() < 1e-12);
        assert!((EnergyUnit::NanoJoules.joules(3_000_000_000) - 3.0).abs() < 1e-12);
        assert!(EnergyUnit::NanoJoules.joules(u64::MAX).is_finite());
        assert!(EnergyUnit::MilliJoules.joules(0).abs() < f64::EPSILON);
    }

    #[test]
    fn every_fixture_channel_has_a_known_unit() {
        let all = channels();
        assert_eq!(all.len(), 364);
        let count = |u| {
            all.iter()
                .filter(|(_, l)| EnergyUnit::parse(l) == Some(u))
                .count()
        };
        assert_eq!(count(EnergyUnit::MilliJoules), 358);
        assert_eq!(count(EnergyUnit::MicroJoules), 5);
        assert_eq!(count(EnergyUnit::NanoJoules), 1);
        let gpu = all.iter().find(|(n, _)| *n == "GPU Energy").unwrap();
        assert_eq!(gpu.1, "nJ", "GPU Energy is the one nJ channel");
    }

    #[test]
    fn timebase_converts_24mhz_ticks_without_overflow() {
        assert_eq!(ticks_to_ns(24_000_000, 125, 3), Some(SEC));
        assert_eq!(ticks_to_ns(0, 125, 3), Some(0));
        assert_eq!(ticks_to_ns(12_345, 1, 1), Some(12_345));
        assert_eq!(ticks_to_ns(1, 1, 0), None, "zero denominator");
        // 6.4e12 ticks (about three days of uptime) is fine; u64::MAX is not.
        assert_eq!(
            ticks_to_ns(6_423_836_162_747, 125, 3),
            Some(267_659_840_114_458)
        );
        assert_eq!(ticks_to_ns(u64::MAX, 125, 3), None);
        assert_eq!(ticks_to_ns(u64::MAX, 3, 125).map(|n| n > 0), Some(true));
    }

    // ---------- roll-up matcher ----------

    #[test]
    fn rollup_takes_exactly_four_totals_from_the_m5_pro_channel_list() {
        let mut hits: Vec<(&str, Rail)> = channels()
            .into_iter()
            .filter_map(|(n, _)| rollup(n).map(|r| (n, r)))
            .collect();
        hits.sort_by_key(|(n, _)| *n);
        assert_eq!(
            hits,
            vec![
                ("ANE0", Rail::Ane),
                ("CPU Energy", Rail::Cpu),
                ("DRAM0", Rail::Dram),
                ("GPU Energy", Rail::Gpu),
            ]
        );
    }

    #[test]
    fn rollup_never_sums_cluster_core_pacc_dtl_or_sram_channels() {
        let overlapping: Vec<&str> = channels()
            .into_iter()
            .map(|(n, _)| n)
            .filter(|n| {
                ["MCPU", "PCPU", "PACC", "DTL", "SRAM", "MCPM", "PCPM"]
                    .iter()
                    .any(|bad| n.contains(bad))
            })
            .collect();
        assert!(overlapping.len() >= 300, "{} names", overlapping.len());
        for name in overlapping {
            assert_eq!(rollup(name), None, "{name}");
        }
    }

    #[test]
    fn rollup_rejects_near_misses() {
        for name in [
            "cpu energy",
            "CPU Energy ",
            " CPU Energy",
            "CPU  Energy",
            "ECPU Energy",
            "GPU",
            "GPU0",
            "GPU Energy0",
            "ANE0_SRAM",
            "ANE0DTL00",
            "ANEX",
            "ANE123",
            "ANE-1",
            "DRAM0 ",
            "DRAM0_SRAM",
            "DRAMX",
            "AMCC0",
            "DCS0",
            "PCIe Port 0 Energy",
            "apciec0 Energy",
            "",
            "\u{410}NE0",
        ] {
            assert_eq!(rollup(name), None, "{name:?}");
        }
        assert_eq!(rollup("ANE"), Some(Rail::Ane), "M1-style bare name");
        assert_eq!(rollup("ANE1"), Some(Rail::Ane));
        assert_eq!(rollup("DRAM12"), Some(Rail::Dram));
    }

    #[test]
    fn rollup_sweep_over_generated_names() {
        let stems = [
            "ANE",
            "DRAM",
            "CPU Energy",
            "GPU Energy",
            "MCPU0",
            "PACC_",
            "PCPU",
        ];
        let tails = [
            "", "0", "1", "9", "10", "99", "100", "_SRAM", "DTL00", "a", " ", "_0",
        ];
        let mut accepted = 0;
        for stem in stems {
            for tail in tails {
                let name = format!("{stem}{tail}");
                let want = matches!(
                    (stem, tail),
                    ("ANE" | "DRAM", "" | "0" | "1" | "9" | "10" | "99")
                        | ("CPU Energy" | "GPU Energy", "")
                );
                assert_eq!(rollup(&name).is_some(), want, "{name:?}");
                accepted += usize::from(want);
            }
        }
        assert_eq!(accepted, 14);
    }

    // ---------- energy hold state machine ----------

    #[test]
    fn first_read_is_only_a_baseline() {
        let mut hold = hold_mj();
        assert_eq!(hold.observe(obs(5_000, SEC, SEC)), None);
        assert_eq!(
            hold.observe(obs(5_000, SEC, 2 * SEC)),
            None,
            "no second publication yet"
        );
    }

    #[test]
    fn measured_then_held_then_dropped_when_stale() {
        let mut hold = EnergyHold::new(EnergyUnit::MilliJoules, FIVE_S);
        hold.observe(obs(0, 0, 0));
        // 20 J over 4 s = 5 W, read 1 s after publication.
        let r = hold.observe(obs(20_000, 4 * SEC, 5 * SEC)).unwrap();
        assert!(close(r.watts, 5.0, 1e-6));
        assert_eq!(r.source, ReadingSource::Measured);
        // Awake reads every cadence (wall clock), 5 s after publication.
        let r = hold
            .observe(Observation {
                wall_ns: 6 * SEC,
                ..obs(20_000, 4 * SEC, 9 * SEC)
            })
            .unwrap();
        assert!(close(r.watts, 5.0, 1e-6));
        assert_eq!(
            r.source,
            ReadingSource::Held {
                age: Duration::from_secs(5)
            }
        );
        let r = hold.observe(Observation {
            wall_ns: 7 * SEC,
            ..obs(20_000, 4 * SEC, 15 * SEC)
        });
        assert_eq!(r, None, "11 s after publication");
    }

    #[test]
    fn stale_boundary_is_ten_seconds_after_publication() {
        let mut hold = hold_mj();
        hold.observe(obs(0, 0, 0));
        hold.observe(obs(1_000, SEC, SEC));
        let at = |now| {
            let mut h = hold.clone();
            h.observe(Observation {
                wall_ns: SEC,
                ..obs(1_000, SEC, now)
            })
        };
        assert_eq!(held_age(at(11 * SEC)), Some(Duration::from_secs(10)));
        assert_eq!(at(11 * SEC + 1), None);
    }

    #[test]
    fn a_counter_reset_rebases_instead_of_spiking() {
        let mut hold = hold_mj();
        hold.observe(obs(1_000_000, 0, 0));
        assert!(hold.observe(obs(1_002_000, SEC, SEC)).is_some());
        // Driver restarted: the total drops. No negative or huge reading.
        assert_eq!(hold.observe(obs(10, 2 * SEC, 2 * SEC)), None);
        let r = hold.observe(obs(3_010, 3 * SEC, 3 * SEC)).unwrap();
        assert!(close(r.watts, 3.0, 1e-6), "{}", r.watts);
    }

    #[test]
    fn a_timestamp_going_backwards_rebases() {
        let mut hold = hold_mj();
        hold.observe(obs(0, 10 * SEC, 10 * SEC));
        assert!(hold.observe(obs(1_000, 11 * SEC, 11 * SEC)).is_some());
        assert_eq!(hold.observe(obs(2_000, 5 * SEC, 11 * SEC)), None);
        let r = hold.observe(obs(4_000, 6 * SEC, 11 * SEC)).unwrap();
        assert!(close(r.watts, 2.0, 1e-6));
    }

    #[test]
    fn a_sleep_gap_resets_the_hold() {
        let mut hold = hold_mj();
        hold.observe(obs(0, 0, 0));
        assert!(hold.observe(obs(1_000, SEC, SEC)).is_some());
        // Exactly three cadences of wall time is still awake.
        let mut awake = hold.clone();
        let r = awake.observe(Observation {
            wall_ns: 4 * SEC,
            ..obs(1_000, SEC, 4 * SEC)
        });
        assert!(held_age(r).is_some());
        // Longer: the Mac slept. The next publication is only a new base,
        // even though the absolute clock (which stops in sleep) barely moved.
        let r = hold.observe(Observation {
            wall_ns: 60 * SEC,
            ..obs(50_000, 2 * SEC, 2 * SEC)
        });
        assert_eq!(r, None);
        let r = hold.observe(Observation {
            wall_ns: 61 * SEC,
            ..obs(52_000, 3 * SEC, 3 * SEC)
        });
        assert!(close(r.unwrap().watts, 2.0, 1e-6));
    }

    #[test]
    fn a_wall_clock_going_backwards_resets() {
        let mut hold = hold_mj();
        hold.observe(obs(0, 0, 10 * SEC));
        assert!(hold.observe(obs(1_000, SEC, 11 * SEC)).is_some());
        let r = hold.observe(Observation {
            wall_ns: 5 * SEC,
            ..obs(2_000, 2 * SEC, 12 * SEC)
        });
        assert_eq!(r, None);
    }

    #[test]
    fn delta_counters_match_cumulative_ones() {
        let totals = [0_u64, 0, 4_000, 4_000, 4_000, 9_000, 9_000, 30_000];
        let pubs = [0_u64, 0, 2, 2, 2, 3, 3, 8];
        let mut cumulative = hold_mj();
        let mut delta = hold_mj();
        let mut prev = 0;
        for (i, (&total, &p)) in totals.iter().zip(&pubs).enumerate() {
            let now = (i as u64 + 1) * SEC;
            let a = cumulative.observe(obs(total, p * SEC, now));
            let b = delta.observe(Observation {
                counter: Counter::Delta(total - prev),
                ..obs(0, p * SEC, now)
            });
            prev = total;
            assert_eq!(a, b, "step {i}");
        }
    }

    #[test]
    fn publications_too_close_together_wait_for_a_real_span() {
        let mut hold = hold_mj();
        hold.observe(obs(0, SEC, SEC));
        // 5 ms later: dividing by that would amplify jitter.
        assert_eq!(
            hold.observe(obs(100, SEC + 5_000_000, SEC + 5_000_000)),
            None
        );
        let r = hold.observe(obs(2_000, 2 * SEC, 2 * SEC)).unwrap();
        assert!(close(r.watts, 2.0, 1e-6), "measured over the full second");
    }

    #[test]
    fn an_implausible_rate_is_dropped_not_held() {
        let mut hold = hold_mj();
        hold.observe(obs(0, 0, 0));
        assert!(hold.observe(obs(5_000, SEC, SEC)).is_some());
        // 5 MJ in a second.
        assert_eq!(hold.observe(obs(5_000_000_000, 2 * SEC, 2 * SEC)), None);
        assert_eq!(hold.observe(obs(5_000_000_000, 2 * SEC, 3 * SEC)), None);
    }

    #[test]
    fn a_counter_that_did_not_move_is_a_measured_zero_not_missing() {
        let mut hold = EnergyHold::new(EnergyUnit::MilliJoules, CADENCE);
        hold.observe(obs(777, 0, 0));
        let r = hold.observe(obs(777, 2 * SEC, 2 * SEC)).unwrap();
        assert_eq!(r, Reading::measured(0.0), "an idle ANE really is 0 W");
    }

    #[test]
    fn energy_moving_without_a_new_timestamp_waits_for_the_next_publication() {
        let mut hold = hold_mj();
        hold.observe(obs(0, 0, 0));
        hold.observe(obs(1_000, SEC, SEC));
        let r = hold.observe(obs(1_500, SEC, 2 * SEC)).unwrap();
        assert_eq!(
            r.source,
            ReadingSource::Held {
                age: Duration::from_secs(1)
            }
        );
        assert!(close(r.watts, 1.0, 1e-6));
        let r = hold.observe(obs(3_000, 2 * SEC, 2 * SEC)).unwrap();
        assert!(close(r.watts, 2.0, 1e-6), "the whole 2 J lands in the span");
    }

    struct RawRow {
        now_ns: u64,
        cpu: (u64, u64),
        dram: (u64, u64),
        gpu: (u64, u64),
    }

    /// `(published_ns, value)` for CPU, DRAM and GPU per 5 s read.
    fn rawts() -> Vec<RawRow> {
        RAWTS
            .lines()
            .map(|line| {
                let field = |key: &str| -> Vec<&str> {
                    line.split_whitespace()
                        .filter_map(|w| w.strip_prefix(key))
                        .collect()
                };
                let ts: Vec<u64> = field("ts=").iter().map(|v| v.parse().unwrap()).collect();
                let val: Vec<u64> = field("val=").iter().map(|v| v.parse().unwrap()).collect();
                let age: Vec<f64> = field("age=")
                    .iter()
                    .map(|v| v.trim_end_matches('s').parse().unwrap())
                    .collect();
                assert_eq!((ts.len(), val.len(), age.len()), (3, 3, 3), "{line}");
                // GPU Energy is live (age 0.00 s), so it dates the read.
                let now_ticks = ts[2] + (age[2] * 24e6) as u64;
                RawRow {
                    now_ns: ticks(now_ticks),
                    cpu: (ticks(ts[0]), val[0]),
                    dram: (ticks(ts[1]), val[1]),
                    gpu: (ticks(ts[2]), val[2]),
                }
            })
            .collect()
    }

    const FIVE_S: Duration = Duration::from_secs(5);

    #[test]
    fn fixture_shows_macos_27_batching_between_2_and_108_seconds() {
        let rows = rawts();
        assert_eq!(rows.len(), 170);
        let mut pubs: Vec<u64> = rows.iter().map(|r| r.cpu.0).collect();
        pubs.dedup();
        let gaps: Vec<f64> = pubs
            .windows(2)
            .map(|w| (w[1] - w[0]) as f64 / 1e9)
            .collect();
        let min = gaps.iter().copied().fold(f64::MAX, f64::min);
        let max = gaps.iter().copied().fold(0.0, f64::max);
        eprintln!("{} gaps, {min:.1}..{max:.1} s", gaps.len());
        assert!(gaps.len() >= 20, "{} publications", gaps.len());
        assert!((0.5..=3.0).contains(&min), "min gap {min:.1}s");
        assert!((90.0..=120.0).contains(&max), "max gap {max:.1}s");
    }

    #[test]
    fn cpu_energy_replay_measures_on_publication_holds_then_goes_absent() {
        let mut hold = EnergyHold::new(EnergyUnit::MilliJoules, FIVE_S);
        let (mut measured, mut kept, mut absent) = (0, 0, 0);
        let mut prev_pub: Option<(u64, u64)> = None;
        for row in rawts() {
            let (published, total) = row.cpu;
            let r = hold.observe(Observation {
                counter: Counter::Cumulative(total),
                published_ns: published,
                now_ns: row.now_ns,
                wall_ns: row.now_ns,
            });
            let new_pub = prev_pub.is_some_and(|(p, _)| p != published);
            match r.map(|r| r.source) {
                Some(ReadingSource::Measured) => {
                    measured += 1;
                    assert!(new_pub, "Measured only on a new publication");
                    let (p0, e0) = prev_pub.unwrap();
                    let want = (total - e0) as f64 / 1e3 / ((published - p0) as f64 / 1e9);
                    assert!(close(r.unwrap().watts, want as f32, 1e-3));
                }
                Some(ReadingSource::Held { age }) => {
                    kept += 1;
                    assert!(!new_pub);
                    assert!(age <= POWER_STALE_AFTER);
                    assert_eq!(age.as_nanos(), u128::from(row.now_ns - published));
                }
                Some(ReadingSource::Estimated) => panic!("holds never estimate"),
                None => absent += 1,
            }
            if let Some(r) = r {
                assert!((0.0..=150.0).contains(&r.watts), "{}", r.watts);
            }
            prev_pub = Some((published, total));
        }
        eprintln!("measured {measured} held {kept} absent {absent}");
        assert!(measured >= 15, "{measured}");
        assert!(
            absent > measured,
            "long batches leave CPU energy stale most of the time"
        );
    }

    #[test]
    fn dram_publishes_in_the_same_batches_as_cpu() {
        for row in rawts() {
            let skew = row.dram.0.abs_diff(row.cpu.0);
            assert!(skew < 1_000_000, "DRAM and CPU share a batch ({skew} ns)");
        }
    }

    #[test]
    fn gpu_energy_replay_is_measured_every_read() {
        let mut hold = EnergyHold::new(EnergyUnit::NanoJoules, FIVE_S);
        let mut prev: Option<(u64, u64)> = None;
        for row in rawts() {
            let r = hold.observe(Observation {
                counter: Counter::Cumulative(row.gpu.1),
                published_ns: row.gpu.0,
                now_ns: row.now_ns,
                wall_ns: row.now_ns,
            });
            if let Some((p0, e0)) = prev {
                let r = r.expect("GPU Energy is live");
                assert_eq!(r.source, ReadingSource::Measured);
                let want = (row.gpu.1 - e0) as f64 / 1e9 / ((row.gpu.0 - p0) as f64 / 1e9);
                assert!(close(r.watts, want as f32, 1e-4));
                assert!((0.0..=60.0).contains(&r.watts), "{}", r.watts);
            } else {
                assert_eq!(r, None);
            }
            prev = Some(row.gpu);
        }
    }

    /// Tiny deterministic generator for property sweeps.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }
    }

    #[test]
    fn property_hold_invariants_over_random_batching() {
        let mut rng = Lcg(0x5eed);
        for run in 0..300 {
            let mut hold = hold_mj();
            let mut now = 0_u64;
            let mut published = 0_u64;
            let mut total = 0_u64;
            let mut pubs_seen = 0;
            let mut last_pub = None;
            for _ in 0..60 {
                now += SEC + rng.next() % SEC;
                if rng.next().is_multiple_of(4) {
                    // A batch arrives: 0–40 W over the span since the last one.
                    let span_s = (now - published) / SEC;
                    total += span_s * (rng.next() % 40_000);
                    published = now - rng.next() % (SEC / 2);
                    if rng.next().is_multiple_of(50) {
                        total /= 2; // counter reset
                    }
                }
                let r = hold.observe(obs(total, published, now));
                if last_pub != Some(published) {
                    pubs_seen += 1;
                    last_pub = Some(published);
                }
                let Some(r) = r else { continue };
                assert!(
                    pubs_seen >= 2,
                    "run {run}: a reading needs two publications"
                );
                assert!(r.watts.is_finite() && r.watts >= 0.0 && r.watts <= 1_000.0);
                if let ReadingSource::Held { age } = r.source {
                    assert!(age <= POWER_STALE_AFTER, "run {run}");
                }
                assert_ne!(r.source, ReadingSource::Estimated);
            }
        }
    }

    // ---------- bank and combine ----------

    fn read_all<'a>(names: &'a [(&'a str, &'a str)], obs: Observation) -> Vec<ChannelRead<'a>> {
        names
            .iter()
            .map(|&(name, unit)| ChannelRead { name, unit, obs })
            .collect()
    }

    // ---------- whole-family fixtures ----------
    //
    // Only the M5 Pro list above is captured from hardware. The lists below
    // encode the "Energy Model" naming macmon and asitop document for other
    // chips (M1–M4 `ECPU`/`PCPU` cluster channels beside the `CPU Energy`
    // total, bare `ANE`/`DRAM` on M1, `DIE_<n>_` prefixes on Ultra).

    type FamilyCase = (&'static str, Vec<(&'static str, &'static str)>, usize);

    /// (chip, channels, expected tracked roll-ups).
    fn family() -> Vec<FamilyCase> {
        vec![
            (
                "M1 (E+P)",
                vec![
                    ("ECPU", "mJ"),
                    ("PCPU", "mJ"),
                    ("CPU Energy", "mJ"),
                    ("GPU", "mJ"),
                    ("GPU Energy", "nJ"),
                    ("ANE", "mJ"),
                    ("DRAM", "mJ"),
                ],
                4,
            ),
            (
                "M1 Pro/Max (E+2P)",
                vec![
                    ("ECPU", "mJ"),
                    ("PCPU", "mJ"),
                    ("PCPU1", "mJ"),
                    ("CPU Energy", "mJ"),
                    ("GPU Energy", "nJ"),
                    ("ANE0", "mJ"),
                    ("DRAM0", "mJ"),
                ],
                4,
            ),
            (
                "M4 (E+P, ACC naming)",
                vec![
                    ("EACC_CPU", "mJ"),
                    ("PACC0_CPU", "mJ"),
                    ("CPU Energy", "mJ"),
                    ("GPU Energy", "nJ"),
                    ("ANE0", "mJ"),
                    ("DRAM0", "mJ"),
                ],
                4,
            ),
            (
                "M1/M2 Ultra (two dies)",
                vec![
                    ("DIE_0_ECPU", "mJ"),
                    ("DIE_0_PCPU", "mJ"),
                    ("DIE_0_PCPU1", "mJ"),
                    ("DIE_0_CPU Energy", "mJ"),
                    ("DIE_1_CPU Energy", "mJ"),
                    ("GPU Energy", "nJ"),
                    ("DIE_0_ANE0", "mJ"),
                    ("DIE_1_ANE0", "mJ"),
                    ("DIE_0_DRAM0", "mJ"),
                    ("DIE_1_DRAM0", "mJ"),
                ],
                7,
            ),
            (
                "unknown future chip",
                vec![("XPU Energy", "mJ"), ("NPU0", "kJ")],
                0,
            ),
        ]
    }

    #[test]
    fn family_channel_lists_track_only_their_totals() {
        for (chip, names, tracked) in family() {
            let mut bank = EnergyBank::new(CADENCE);
            let _ = bank.observe(read_all(&names, obs(0, 0, 0)));
            assert_eq!(bank.tracked(), tracked, "{chip}");
        }
    }

    #[test]
    fn family_rates_fill_every_rail_the_chip_has() {
        for (chip, names, tracked) in family() {
            let mut bank = EnergyBank::new(CADENCE);
            let _ = bank.observe(read_all(&names, obs(0, 0, 0)));
            // 1 s later every counter moved by 1000 mJ (or 1e9 nJ): 1 W each.
            let reads: Vec<ChannelRead<'_>> = names
                .iter()
                .map(|&(name, unit)| {
                    let step = if unit == "nJ" { 1_000_000_000 } else { 1_000 };
                    ChannelRead {
                        name,
                        unit,
                        obs: obs(step, SEC, SEC),
                    }
                })
                .collect();
            let rails = bank.observe(reads);
            if tracked == 0 {
                assert_eq!(rails, RailReadings::default(), "{chip}: absent, not 0 W");
                continue;
            }
            let per_die = if chip.contains("Ultra") { 2.0 } else { 1.0 };
            assert!(close(rails.cpu.unwrap().watts, per_die, 1e-4), "{chip}");
            assert!(close(rails.gpu.unwrap().watts, 1.0, 1e-4), "{chip}");
            assert!(close(rails.ane.unwrap().watts, per_die, 1e-4), "{chip}");
            assert!(close(rails.dram.unwrap().watts, per_die, 1e-4), "{chip}");
        }
    }

    #[test]
    fn die_prefixes_parse_and_near_misses_do_not() {
        assert_eq!(
            rollup_scoped("DIE_1_CPU Energy"),
            Some((Rail::Cpu, Scope::Die(1)))
        );
        assert_eq!(rollup_scoped("DIE_0_ANE"), Some((Rail::Ane, Scope::Die(0))));
        assert_eq!(
            rollup_scoped("CPU Energy"),
            Some((Rail::Cpu, Scope::Package))
        );
        for bad in [
            "DIE_CPU Energy",
            "DIE_x_CPU Energy",
            "DIE_123_CPU Energy",
            "DIE_0_PCPU",
            "DIE_0_",
            "DIE_0_DIE_1_CPU Energy",
            "die_0_CPU Energy",
        ] {
            assert_eq!(rollup_scoped(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_package_total_beside_die_totals_is_not_counted_twice() {
        let names = [
            ("CPU Energy", "mJ"),
            ("DIE_0_CPU Energy", "mJ"),
            ("DIE_1_CPU Energy", "mJ"),
        ];
        let mut bank = EnergyBank::new(CADENCE);
        let _ = bank.observe(read_all(&names, obs(0, 0, 0)));
        let rails = bank.observe(read_all(&names, obs(4_000, SEC, SEC)));
        assert!(close(rails.cpu.unwrap().watts, 4.0, 1e-4), "package wins");
    }

    #[test]
    fn an_ultra_with_one_stale_die_reports_no_cpu_rail_not_half_of_it() {
        let names = [("DIE_0_CPU Energy", "mJ"), ("DIE_1_CPU Energy", "mJ")];
        let mut bank = EnergyBank::new(CADENCE);
        let _ = bank.observe(read_all(&names, obs(0, 0, 0)));
        let rails = bank.observe(read_all(&names, obs(4_000, SEC, SEC)));
        assert!(close(rails.cpu.unwrap().watts, 8.0, 1e-4), "both dies");
        // Die 0 publishes every second; die 1 last published at 1 s, so
        // its hold ages until it goes stale past POWER_STALE_AFTER.
        let stale_at = 1 + POWER_STALE_AFTER.as_secs() + 1;
        let mut rails = RailReadings::default();
        for t in 2..=stale_at {
            rails = bank.observe([
                ChannelRead {
                    name: names[0].0,
                    unit: "mJ",
                    obs: obs(4_000 * t, t * SEC, t * SEC),
                },
                ChannelRead {
                    name: names[1].0,
                    unit: "mJ",
                    obs: obs(4_000, SEC, t * SEC),
                },
            ]);
            if t == 2 {
                assert!(close(rails.cpu.unwrap().watts, 8.0, 1e-4), "die 1 held");
            }
        }
        assert_eq!(rails.cpu, None, "die 0 alone is not the chip's CPU");
    }

    #[test]
    fn a_dram_channel_that_rebases_or_drops_out_blanks_the_dram_rail() {
        let names = [("DRAM0", "mJ"), ("DRAM1", "mJ")];
        let mut bank = EnergyBank::new(CADENCE);
        let _ = bank.observe(read_all(&names, obs(0, 0, 0)));
        let rails = bank.observe(read_all(&names, obs(1_000, SEC, SEC)));
        assert!(close(rails.dram.unwrap().watts, 2.0, 1e-4));
        // DRAM1's counter restarts: it rebases and has no rate yet.
        let rails = bank.observe([
            ChannelRead {
                name: "DRAM0",
                unit: "mJ",
                obs: obs(2_000, 2 * SEC, 2 * SEC),
            },
            ChannelRead {
                name: "DRAM1",
                unit: "mJ",
                obs: obs(10, 2 * SEC, 2 * SEC),
            },
        ]);
        assert_eq!(rails.dram, None, "one channel rebased");
        // DRAM1 missing from the sweep altogether.
        let rails = bank.observe(read_all(&names[..1], obs(3_000, 3 * SEC, 3 * SEC)));
        assert_eq!(rails.dram, None, "one channel missing");
    }

    // ---------- rail overlay ----------

    fn held_ms(watts: f32, ms: u64) -> Reading {
        Reading {
            watts,
            source: ReadingSource::Held {
                age: Duration::from_millis(ms),
            },
        }
    }

    #[test]
    fn aging_grows_held_ages_and_drops_stale_rails() {
        let rails = RailReadings {
            cpu: Some(held_ms(3.0, 4_000)),
            gpu: Some(Reading::measured(2.0)),
            ane: Some(held_ms(0.1, 9_000)),
            dram: None,
        };
        let aged = rails.aged(Duration::from_millis(1_500), Duration::from_secs(1));
        assert_eq!(aged.cpu, Some(held_ms(3.0, 5_500)));
        assert_eq!(
            aged.gpu,
            Some(held_ms(2.0, 1_500)),
            "measured goes held after the fresh window"
        );
        assert_eq!(aged.ane, None, "9 s + 1.5 s is past the stale limit");
        assert_eq!(aged.dram, None);
    }

    #[test]
    fn aging_inside_the_fresh_window_keeps_measured() {
        let rails = RailReadings {
            gpu: Some(Reading::measured(2.0)),
            ..RailReadings::default()
        };
        let aged = rails.aged(Duration::from_millis(400), Duration::from_secs(1));
        assert_eq!(aged.gpu, Some(Reading::measured(2.0)));
        assert_eq!(rails.aged(Duration::ZERO, Duration::ZERO), rails);
    }

    #[test]
    fn apply_fills_cpu_only_when_the_smc_rails_are_absent() {
        let rails = RailReadings {
            cpu: Some(held_ms(4.0, 3_000)),
            gpu: Some(held_ms(1.0, 3_000)),
            ane: None,
            dram: Some(held_ms(0.5, 3_000)),
        };
        // Mac17,8: live SMC rails stay primary.
        let mut mapped = PowerSnapshot {
            system: Some(Reading::measured(30.0)),
            cpu: Some(Reading::measured(6.0)),
            ..PowerSnapshot::default()
        };
        rails.apply(&mut mapped);
        assert_eq!(mapped.cpu, Some(Reading::measured(6.0)));
        assert_eq!(mapped.gpu, Some(held_ms(1.0, 3_000)));
        assert_eq!(mapped.ane, None, "absent, not 0 W");
        // Any other model (a desktop with PSTR but no mapped rails).
        let mut other = PowerSnapshot {
            system: Some(Reading::measured(30.0)),
            ..PowerSnapshot::default()
        };
        rails.apply(&mut other);
        assert_eq!(other.cpu, Some(held_ms(4.0, 3_000)));
        assert_eq!(other.dram, Some(held_ms(0.5, 3_000)));
    }

    #[test]
    fn apply_drops_rails_over_the_system_ceiling() {
        let rails = RailReadings {
            cpu: Some(Reading::measured(90.0)),
            gpu: Some(Reading::measured(4.0)),
            ..RailReadings::default()
        };
        let mut power = PowerSnapshot {
            system: Some(Reading::measured(20.0)),
            ..PowerSnapshot::default()
        };
        rails.apply(&mut power);
        assert_eq!(power.cpu, None);
        assert_eq!(power.gpu, Some(Reading::measured(4.0)));
        // No PSTR yet: no ceiling to judge by.
        let mut open = PowerSnapshot::default();
        rails.apply(&mut open);
        assert_eq!(open.cpu, Some(Reading::measured(90.0)));
    }

    #[test]
    fn apply_with_no_rails_clears_previous_energy_figures() {
        let mut power = PowerSnapshot {
            gpu: Some(Reading::measured(4.0)),
            ane: Some(Reading::measured(0.2)),
            ..PowerSnapshot::default()
        };
        RailReadings::default().apply(&mut power);
        assert_eq!(power, PowerSnapshot::default());
    }

    #[test]
    fn a_desktop_with_pstr_and_no_mapped_rails_reports_system_only() {
        // Mac mini / Studio: PSTR exists, the model is not in the rail table.
        let pstr = key("PSTR").unwrap();
        let mut rails = Rails::new("Mac16,11", |k| k == pstr);
        let p = rails.sample(|_| Some(18.0));
        assert_eq!(p.system, Some(Reading::measured(18.0)));
        assert_eq!(p.cpu, None, "IOReport fills CPU on this model");
    }

    #[test]
    fn a_mac_without_pstr_or_rails_reports_nothing() {
        let mut rails = Rails::new("Mac99,1", |_| false);
        assert_eq!(rails.sample(|_| Some(5.0)), PowerSnapshot::default());
    }

    #[test]
    fn the_bank_tracks_only_roll_up_channels_of_the_full_list() {
        let names = channels();
        let mut bank = EnergyBank::new(CADENCE);
        let mut power = PowerSnapshot::default();
        bank.sweep(read_all(&names, obs(0, 0, 0)), &mut power);
        assert_eq!(bank.tracked(), 4, "DTL, SRAM and per-core never tracked");
        assert_eq!(power, PowerSnapshot::default(), "baseline only");
        bank.sweep(read_all(&names, obs(2_000, SEC, SEC)), &mut power);
        // Each rail saw 2,000 units over 1 s: mJ rails 2 W, GPU (nJ) 2 µW.
        assert!(close(power.cpu.unwrap().watts, 2.0, 1e-6));
        assert!(close(power.ane.unwrap().watts, 2.0, 1e-6));
        assert!(close(power.dram.unwrap().watts, 2.0, 1e-6));
        assert!(close(power.gpu.unwrap().watts, 2e-6, 1e-9));
    }

    #[test]
    fn smc_cpu_watts_win_over_the_energy_counter() {
        let names = [("CPU Energy", "mJ"), ("ANE0", "mJ"), ("ANE1", "mJ")];
        let mut bank = EnergyBank::new(CADENCE);
        let mut power = PowerSnapshot {
            cpu: Some(Reading::measured(7.5)),
            ..PowerSnapshot::default()
        };
        bank.sweep(read_all(&names, obs(0, 0, 0)), &mut power);
        bank.sweep(read_all(&names, obs(3_000, SEC, SEC)), &mut power);
        assert_eq!(power.cpu, Some(Reading::measured(7.5)));
        assert!(close(power.ane.unwrap().watts, 6.0, 1e-6), "ANE0 + ANE1");
    }

    #[test]
    fn bank_drops_a_rail_above_system_power() {
        let names = [("GPU Energy", "nJ"), ("DRAM0", "mJ")];
        let mut bank = EnergyBank::new(CADENCE);
        let mut power = PowerSnapshot {
            system: Some(Reading::measured(10.0)),
            ..PowerSnapshot::default()
        };
        bank.sweep(read_all(&names, obs(0, 0, 0)), &mut power);
        // 20 W of GPU and 2 W of DRAM against a 10 W system.
        bank.sweep(
            [
                ChannelRead {
                    name: "GPU Energy",
                    unit: "nJ",
                    obs: obs(20_000_000_000, SEC, SEC),
                },
                ChannelRead {
                    name: "DRAM0",
                    unit: "mJ",
                    obs: obs(2_000, SEC, SEC),
                },
            ],
            &mut power,
        );
        assert_eq!(power.gpu, None, "20 W > 10 W + 5 W slack");
        assert!(close(power.dram.unwrap().watts, 2.0, 1e-6));
    }

    #[test]
    fn bank_skips_channels_with_unknown_units() {
        let mut bank = EnergyBank::new(CADENCE);
        let mut power = PowerSnapshot::default();
        let reads = [ChannelRead {
            name: "CPU Energy",
            unit: "24Mticks",
            obs: obs(0, 0, 0),
        }];
        bank.sweep(reads, &mut power);
        assert_eq!(bank.tracked(), 0);
    }

    #[test]
    fn combine_is_as_fresh_as_its_oldest_part() {
        let m = Reading::measured(1.0);
        let h = |s| Reading {
            watts: 2.0,
            source: ReadingSource::Held {
                age: Duration::from_secs(s),
            },
        };
        let e = Reading {
            watts: 4.0,
            source: ReadingSource::Estimated,
        };
        assert_eq!(combine(m, m), Reading::measured(2.0));
        assert_eq!(combine(m, h(3)).source, h(3).source);
        assert_eq!(combine(h(7), h(3)).source, h(7).source);
        assert_eq!(combine(h(7), e).source, ReadingSource::Estimated);
        assert!(close(combine(combine(m, h(1)), e).watts, 7.0, 1e-6));
    }

    // ---------- SMC rails ----------

    /// Every P* power key on the Mac17,8 SMC (all `flt`).
    const MAC17_8_P_KEYS: &str = "P3F2 P5SR PCMC PD3C PDBC PDBR PDTR PFLR PH0R PHCR PHPB PHPC \
        PHPM PHPS PIDR PINC PMVR PN00 PP0b PP1b PP1l PP2b PP3b PP4b PP5l PP7b PP7l PP9b PP9l \
        PPBR PPMC PPSC PPSM PPal PPbb PPdb PPeb PPkl PR0b PR1b PR2b PR3b PR3l PR4b PR7b PR8l \
        PR9b PR9l PRal PRbb PRdb PReb PS0b PS1b PS2b PS3b PS4b PS7b PS7l PS9b PS9l PSTR PSal \
        PSbb PSbl PSdb PSdl PSeb PTHS PW3C PZC0 PZC1 PZCB PZCU PZD1 PZT0 Pb0f";

    fn m5_pro_has(k: u32) -> bool {
        MAC17_8_P_KEYS.split_whitespace().any(|n| key(n) == Some(k))
    }

    #[test]
    fn rail_table_is_gated_by_exact_model() {
        let rails = smc_keys::cpu_rails("Mac17,8").unwrap();
        assert_eq!(rails, ["PP0b", "PP4b", "PP1b", "PP7b", "PR0b", "PR7b"]);
        for other in [
            "Mac17,9",
            "mac17,8",
            "Mac17,8 ",
            "Mac17",
            "",
            "Mac16,1",
            "MacBookPro18,1",
        ] {
            assert!(smc_keys::cpu_rails(other).is_none(), "{other:?}");
        }
    }

    #[test]
    fn every_mapped_rail_exists_on_the_m5_pro_smc() {
        assert_eq!(MAC17_8_P_KEYS.split_whitespace().count(), 77);
        for name in smc_keys::cpu_rails("Mac17,8").unwrap() {
            assert!(m5_pro_has(key(name).unwrap()), "{name}");
        }
        assert!(m5_pro_has(key(SYSTEM_KEY).unwrap()));
        let rails = Rails::new("Mac17,8", m5_pro_has);
        assert_eq!((rails.system.is_some(), rails.cpu.len()), (true, 6));
        assert_eq!(key("PST"), None, "keys are exactly four bytes");
        assert_eq!(key("PSTRX"), None);
    }

    #[test]
    fn a_missing_rail_key_disables_cpu_watts_rather_than_under_reporting() {
        let pr7b = key("PR7b").unwrap();
        let rails = Rails::new("Mac17,8", |k| k != pr7b && m5_pro_has(k));
        assert!(rails.cpu.is_empty());
        assert!(rails.system.is_some());
        let unknown = Rails::new("Mac99,1", m5_pro_has);
        assert!(unknown.cpu.is_empty() && unknown.system.is_some());
        let bare = Rails::new("Mac17,8", |_| false);
        assert!(bare.cpu.is_empty() && bare.system.is_none());
    }

    fn constant<'a>(values: &'a [(&'a str, f64)]) -> impl FnMut(u32) -> Option<f64> + 'a {
        move |k| {
            values
                .iter()
                .find(|(n, _)| key(n) == Some(k))
                .map(|&(_, v)| v)
        }
    }

    const IDLE: &[(&str, f64)] = &[
        ("PSTR", 8.5),
        ("PP0b", 0.06),
        ("PP4b", 0.013),
        ("PP1b", 0.37),
        ("PP7b", 0.012),
        ("PR0b", 0.0),
        ("PR7b", 0.0),
    ];

    #[test]
    fn missing_data_is_never_zero_watts() {
        let mut rails = Rails::new("Mac17,8", m5_pro_has);
        assert_eq!(rails.sample(|_| None), PowerSnapshot::default());
        // PSTR of 0 on a running Mac, and rails all exactly 0: not populated.
        let zeros: Vec<(&str, f64)> = IDLE.iter().map(|&(n, _)| (n, 0.0)).collect();
        assert_eq!(rails.sample(constant(&zeros)), PowerSnapshot::default());
        for bad in [f64::NAN, f64::INFINITY, -1.0, 5_000.0] {
            let vals: Vec<(&str, f64)> = IDLE.iter().map(|&(n, _)| (n, bad)).collect();
            assert_eq!(
                rails.sample(constant(&vals)),
                PowerSnapshot::default(),
                "{bad}"
            );
        }
        // One rail unreadable: no partial CPU sum, PSTR still reported.
        let partial: Vec<(&str, f64)> =
            IDLE.iter().copied().filter(|(n, _)| *n != "PP1b").collect();
        let p = rails.sample(constant(&partial));
        assert_eq!(p.system, Some(Reading::measured(8.5)));
        assert_eq!(p.cpu, None);
    }

    #[test]
    fn idle_rails_sum_to_cpu_watts() {
        let mut rails = Rails::new("Mac17,8", m5_pro_has);
        let p = rails.sample(constant(IDLE));
        assert!(close(p.cpu.unwrap().watts, 0.455, 1e-5));
        assert_eq!(p.cpu.unwrap().source, ReadingSource::Measured);
        assert_eq!(p.gpu, None, "P3F2 did not track GPU Energy; not mapped");
        assert_eq!(p.ane, None);
        assert_eq!(p.throttle, Throttle::default());
    }

    #[test]
    fn cpu_watts_are_a_three_read_mean() {
        let mut s = Smoother::default();
        assert!(close(s.push(3.0), 3.0, 1e-6));
        assert!(close(s.push(6.0), 4.5, 1e-6));
        assert!(close(s.push(9.0), 6.0, 1e-6));
        assert!(close(s.push(30.0), 15.0, 1e-6), "oldest read falls out");
        for _ in 0..3 {
            s.push(1.0);
        }
        assert!(close(s.push(1.0), 1.0, 1e-6), "settles fully after 3 reads");
    }

    #[test]
    fn the_smoothed_value_never_exceeds_system_power() {
        let mut rails = Rails::new("Mac17,8", m5_pro_has);
        let load = |pstr: f64, pp0b: f64| {
            let mut v = IDLE.to_vec();
            v[0].1 = pstr;
            v[1].1 = pp0b;
            v
        };
        for _ in 0..3 {
            let p = rails.sample(constant(&load(100.0, 75.0)));
            assert!(close(p.cpu.unwrap().watts, 75.395, 1e-3));
        }
        // Load stops. The CPU mean lags, and so does the PSTR window it is
        // checked against, so the mean is still reported.
        let p = rails.sample(constant(&load(9.0, 0.5)));
        let cpu = p.cpu.unwrap().watts;
        assert!(close(cpu, (2.0 * 75.395 + 0.895) / 3.0, 1e-3), "{cpu}");
        rails.sample(constant(&load(9.0, 0.5)));
        let p = rails.sample(constant(&load(9.0, 0.5)));
        assert!(close(p.cpu.unwrap().watts, 0.895, 1e-4), "settled");
        // A rail glitch far above recent system power reports nothing.
        let p = rails.sample(constant(&load(9.0, 60.0)));
        assert_eq!(p.cpu, None);
        assert!(p.system.is_some());
        // The next sane sweep: the mean still carries the glitch, so this
        // sweep's own sum is used.
        let p = rails.sample(constant(&load(9.0, 0.5)));
        assert!(close(p.cpu.unwrap().watts, 0.895, 1e-4));
    }

    #[test]
    fn d_rails_lead_pstr_at_a_load_step_and_are_not_dropped() {
        // Real rows from the calibration log: the rails jump to 79 W while
        // PSTR still reads 48 W from the previous update.
        let mut rails = Rails::new("Mac17,8", m5_pro_has);
        let mut row = |pstr: f64, pp0b: f64| {
            let mut v = IDLE.to_vec();
            v[0].1 = pstr;
            v[1].1 = pp0b;
            rails.sample(constant(&v)).cpu.map(|r| r.watts)
        };
        row(90.6, 70.0);
        row(62.0, 55.0);
        let step = row(48.1, 78.6);
        assert!(step.is_some(), "within the recent PSTR window");
    }

    struct CalibRow {
        now_ns: u64,
        cpu_e: u64,
        cpu_ts_ns: u64,
        values: Vec<(&'static str, f64)>,
    }

    fn calib() -> Vec<CalibRow> {
        let mut lines = CALIB.lines();
        let header: Vec<&'static str> = lines.next().unwrap().split_whitespace().collect();
        lines
            .map(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                let values = header
                    .iter()
                    .zip(&f)
                    .skip(4)
                    .map(|(&h, v)| (h, v.parse().unwrap()))
                    .collect();
                CalibRow {
                    now_ns: ticks(f[0].parse().unwrap()),
                    cpu_e: f[1].parse().unwrap(),
                    cpu_ts_ns: ticks(f[2].parse().unwrap()),
                    values,
                }
            })
            .collect()
    }

    #[test]
    fn calibration_replay_at_the_sensor_cadence_never_drops_or_zeroes() {
        let rows = calib();
        assert_eq!(rows.len(), 760);
        // Rows are about 0.27 s apart; the sensor thread sweeps every 2 s.
        let step = 7;
        let mut peak: f32 = 0.0;
        for offset in 0..step {
            let mut rails = Rails::new("Mac17,8", m5_pro_has);
            let mut recent = Smoother::default();
            for row in rows.iter().skip(offset).step_by(step) {
                let p = rails.sample(constant(&row.values));
                let system = p.system.expect("PSTR every sweep").watts;
                let cpu = p.cpu.expect("rails every sweep").watts;
                recent.push(system);
                assert!((5.0..=130.0).contains(&system), "{system}");
                let ceiling = recent.max().unwrap() + CEILING_SLACK_W;
                assert!(cpu > 0.0 && cpu <= ceiling, "{cpu} vs {ceiling}");
                peak = peak.max(cpu);
            }
        }
        assert!((60.0..=90.0).contains(&peak), "load.sh 18 peak {peak}");
    }

    #[test]
    fn calibration_replay_at_full_rate_drops_only_transients() {
        let rows = calib();
        let mut rails = Rails::new("Mac17,8", m5_pro_has);
        let dropped = rows
            .iter()
            .filter(|r| rails.sample(constant(&r.values)).cpu.is_none())
            .count();
        assert!(dropped <= 20, "{dropped} of {} sweeps", rows.len());
    }

    #[test]
    fn rails_track_ioreport_cpu_energy_within_tolerance() {
        let rows = calib();
        let mut rails = Rails::new("Mac17,8", m5_pro_has);
        let smoothed: Vec<Option<f32>> = rows
            .iter()
            .map(|r| rails.sample(constant(&r.values)).cpu.map(|c| c.watts))
            .collect();
        let mut pubs: Vec<(u64, u64)> = rows.iter().map(|r| (r.cpu_ts_ns, r.cpu_e)).collect();
        pubs.dedup();
        let mut checked = 0;
        for w in pubs.windows(2) {
            let ((t0, e0), (t1, e1)) = (w[0], w[1]);
            // Only spans the capture fully covers.
            if rows[0].now_ns > t0 {
                continue;
            }
            let span_s = (t1 - t0) as f64 / 1e9;
            let ioreport = (e1 - e0) as f64 / 1e3 / span_s;
            let inside = |pick: &dyn Fn(usize) -> Option<f64>| {
                let v: Vec<f64> = (0..rows.len())
                    .filter(|&i| (t0..=t1).contains(&rows[i].now_ns))
                    .filter_map(pick)
                    .collect();
                v.iter().sum::<f64>() / v.len() as f64
            };
            // The calibration claim: the raw rail sum tracks CPU Energy.
            let raw = inside(&|i| {
                let v = &rows[i].values;
                Some(
                    smc_keys::cpu_rails("Mac17,8")
                        .unwrap()
                        .iter()
                        .map(|k| v.iter().find(|(n, _)| n == k).unwrap().1)
                        .sum(),
                )
            });
            let shown = inside(&|i| smoothed[i].map(f64::from));
            eprintln!(
                "span {span_s:.1}s: ioreport {ioreport:.2} W rails {raw:.2} W shown {shown:.2} W"
            );
            assert!((0.85..=1.1).contains(&(raw / ioreport)), "raw {raw:.2}");
            // What we display is held to the plan's 60 s window.
            if span_s >= 60.0 {
                assert!((0.85..=1.1).contains(&(shown / ioreport)), "{shown:.2}");
            }
            checked += 1;
        }
        assert!(checked >= 4, "{checked} spans");
    }

    // ---------- throttle ----------

    #[test]
    fn cltm_share_from_synthetic_residencies() {
        let no = ["NO_CLTM"];
        assert_eq!(limited_share(&[("NO_CLTM", 1_000)], &no), Some(0.0));
        assert_eq!(
            limited_share(&[("NO_CLTM", 500), ("CLTM_1", 250), ("CLTM_2", 250)], &no),
            Some(0.5)
        );
        assert_eq!(
            limited_share(&[("NO_CLTM", 0), ("CLTM_1", 9)], &no),
            Some(1.0)
        );
        assert_eq!(limited_share(&[], &no), None, "empty channel");
        assert_eq!(limited_share(&[("NO_CLTM", 0)], &no), None, "no time");
        assert_eq!(limited_share(&[("X", 10)], &no), None, "unknown layout");
        let huge = [("NO_CLTM", u64::MAX), ("CLTM_1", u64::MAX)];
        assert_eq!(limited_share(&huge, &no), Some(0.5), "no overflow");
    }

    #[test]
    fn throttle_reads_each_channel_by_its_own_states() {
        let t = throttle(
            &[("NO_CLTM", 100)],
            &[("PERF", 60), ("IDLE_OFF", 20), ("LIMITED", 20)],
            &[("0%", 75), ("10%", 25)],
        );
        assert_eq!(t.gpu_cltm, Some(0.0), "full load on the test Mac reads 0");
        assert!(close(t.gpu_power_limited.unwrap(), 0.2, 1e-6));
        assert!(close(t.cpms_reduction.unwrap(), 0.25, 1e-6));
        assert!(t.any_above(0.2));
        assert_eq!(throttle(&[], &[], &[]), Throttle::default());
    }

    #[test]
    fn property_throttle_share_is_a_fraction() {
        let mut rng = Lcg(42);
        let names = ["NO_CLTM", "CLTM_1", "CLTM_2", "PERF", "IDLE_OFF"];
        for _ in 0..500 {
            let mut res: Vec<(&str, u64)> = Vec::new();
            for n in names {
                if rng.next().is_multiple_of(2) {
                    let shift = rng.next() % 30;
                    res.push((n, rng.next() << shift));
                }
            }
            if let Some(f) = limited_share(&res, &["NO_CLTM"]) {
                assert!((0.0..=1.0).contains(&f), "{f} from {res:?}");
                assert!(res.iter().any(|(n, _)| *n == "NO_CLTM"));
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_smc_power_is_sane() {
        let _io = crate::sampler::io_gate();
        let Some(mut client) = crate::smc::Client::open() else {
            return;
        };
        let model = crate::sys::sysctl_string(c"hw.model").unwrap_or_default();
        let mut rails = Rails::new(&model, |k| client.info(k).is_some());
        let p = rails.sample(|k| {
            let (info, bytes) = client.read(k)?;
            crate::smc::decode_numeric(info.data_type, &bytes, crate::smc::NATIVE_INTS)
        });
        eprintln!("live power on {model}: {p:?}");
        if model == "Mac17,8" {
            let system = p.system.expect("PSTR").watts;
            let cpu = p.cpu.expect("CPU rails").watts;
            assert!((3.0..=150.0).contains(&system), "{system}");
            assert!(cpu > 0.0 && cpu <= system + CEILING_SLACK_W, "{cpu}");
        }
    }
}
