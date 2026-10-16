//! Long history: buckets of series means/maxes plus the busiest processes,
//! kept at coarser resolutions the further back they go.
//!
//! This module is pure: the in-memory store here backs a session with no
//! history directory; the binary supplies the on-disk store. Nothing in
//! here reads a clock: callers pass unix seconds, so tests drive time.

mod bucket;
mod query;
mod tier;

use std::collections::VecDeque;

pub use bucket::{Accumulator, Bucket, Stat, TopProc, merge_apps, merge_top, split_apps, top_n};
pub use query::{
    AppEnergy, Blame, Drain, Event, HOT_C, HealthTrend, LOW_BATTERY_PCT, Named, Peak, Report,
    SWAP_STEP_BYTES, Span, bin, by_name, clock_label, column_of, column_start, duration, events,
    last_filled, present, values,
};
pub use tier::{Rollup, Tier, downsample, merge};

use crate::error::Result;
use crate::snapshot::{PowerSource, Reading, Snapshot, Thermal};

/// Processes kept per bucket in each top list.
pub const TOP_N: usize = 5;
/// Apps kept per bucket in the exact energy list; the rest fold into "other".
pub const APP_N: usize = 8;

/// A stored series. The order is the on-disk column order: append only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Series {
    /// Machine CPU, 0..=1.
    Cpu,
    /// GPU busy, 0..=1; absent without a GPU reading.
    Gpu,
    /// Memory used / total, 0..=1.
    Mem,
    SwapBytes,
    /// Headline interface, in bits per second as sampled.
    NetRxBps,
    /// Headline interface, in bits per second as sampled.
    NetTxBps,
    DiskReadBps,
    DiskWriteBps,
    CpuTempC,
    GpuTempC,
    /// Whole-system input watts (PSTR); absent on Macs without it.
    SysPowerW,
    CpuPowerW,
    GpuPowerW,
    /// The fastest fan's RPM; absent on fanless Macs.
    Fan,
    // ── v2 (store record version 2) ──
    /// State of charge, 0..=100; absent on desktops.
    BatteryPct,
    /// Battery power, signed: + charging, − draining; absent on desktops.
    BatteryW,
    /// 1 on AC (or a UPS), 0 on battery; absent on desktops, whose AC is a
    /// given, and wherever the power source is not reported.
    OnAc,
    /// macOS thermal state, 0 nominal ..= 3 critical.
    ThermalState,
    /// 1 while the thermal state is serious or worse: its mean is the share
    /// of the bucket spent throttled, so reports can total it exactly.
    Throttled,
    /// Battery health (maximum capacity), 0..=100.
    BatteryHealth,
    // ── v3 ──
    /// 1 while the CPU reads at or above [`HOT_C`]: its mean is the share of
    /// the bucket spent hot, so one brief spike is not a whole bucket hot.
    Hot,
}

/// Columns in a bucket.
pub const SERIES_COUNT: usize = Series::ALL.len();

impl Series {
    pub const ALL: [Self; 21] = [
        Self::Cpu,
        Self::Gpu,
        Self::Mem,
        Self::SwapBytes,
        Self::NetRxBps,
        Self::NetTxBps,
        Self::DiskReadBps,
        Self::DiskWriteBps,
        Self::CpuTempC,
        Self::GpuTempC,
        Self::SysPowerW,
        Self::CpuPowerW,
        Self::GpuPowerW,
        Self::Fan,
        Self::BatteryPct,
        Self::BatteryW,
        Self::OnAc,
        Self::ThermalState,
        Self::Throttled,
        Self::BatteryHealth,
        Self::Hot,
    ];

    /// Columns a version 1 record holds: everything before [`Series::BatteryPct`].
    pub const V1_COUNT: usize = Self::BatteryPct as usize;
    /// Columns a version 2 record holds: everything before [`Series::Hot`].
    pub const V2_COUNT: usize = Self::Hot as usize;

    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// This series' value in one snapshot; `None` when the Mac lacks it or
    /// the snapshot has not measured it (never a made-up zero).
    #[must_use]
    pub fn read(self, s: &Snapshot) -> Option<f32> {
        let rate = |v: u64| s.primed.then_some(v as f32);
        let battery = s.battery.present.then_some(&s.battery);
        match self {
            Self::Cpu => s.primed.then_some(s.cpu.scaled),
            Self::Gpu => s.gpu.map(|g| g.scaled),
            Self::Mem => s.memory.used_ratio(),
            Self::SwapBytes => {
                (s.memory.total_bytes > 0).then_some(s.memory.swap_used_bytes as f32)
            }
            Self::NetRxBps => rate(s.network.rx_bps),
            Self::NetTxBps => rate(s.network.tx_bps),
            Self::DiskReadBps => rate(s.disk.read_bps),
            Self::DiskWriteBps => rate(s.disk.write_bps),
            Self::CpuTempC => s.cpu_temp_c(),
            Self::GpuTempC => s.gpu_temp_c(),
            Self::SysPowerW => s.power.system.and_then(Reading::usable),
            Self::CpuPowerW => s.power.cpu.and_then(Reading::usable),
            Self::GpuPowerW => s.power.gpu.and_then(Reading::usable),
            Self::Fan => s
                .fans
                .fans
                .iter()
                .map(|f| f32::from(f.rpm))
                .reduce(f32::max),
            Self::BatteryPct => battery.and_then(|b| b.percent).map(f32::from),
            Self::BatteryW => battery.and_then(|b| b.watts),
            Self::OnAc => battery.and_then(|b| match b.source {
                PowerSource::Ac | PowerSource::Ups => Some(1.0),
                PowerSource::Battery => Some(0.0),
                PowerSource::Unknown => None,
            }),
            Self::ThermalState => s.primed.then_some(f32::from(s.thermal.level())),
            Self::Throttled => s.primed.then_some(if s.thermal >= Thermal::Serious {
                1.0
            } else {
                0.0
            }),
            Self::BatteryHealth => battery.and_then(|b| b.health_pct).map(f32::from),
            Self::Hot => Self::CpuTempC
                .read(s)
                .map(|c| if c >= HOT_C { 1.0 } else { 0.0 }),
        }
    }
}

/// One point of a queried series.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub t: u64,
    pub stat: Stat,
}

/// Where long history lives. `append` takes Day buckets; the store rolls
/// them up into Month itself and drops what falls out of retention.
pub trait HistoryStore {
    fn append(&mut self, bucket: Bucket) -> Result<()>;

    /// Buckets starting in `from..to` at `tier` (Live answers from Day),
    /// oldest first. The still-open Month span is included, rolled up.
    fn buckets(&self, from: u64, to: u64, tier: Tier) -> Result<Vec<Bucket>>;

    /// Whether this store outlives the session.
    fn persistent(&self) -> bool;

    /// One series over `from..to`, skipping buckets where it was absent.
    fn range(&self, series: Series, from: u64, to: u64, tier: Tier) -> Result<Vec<Point>> {
        Ok(self
            .buckets(from, to, tier)?
            .iter()
            .filter_map(|b| b.get(series).map(|stat| Point { t: b.t, stat }))
            .collect())
    }
}

/// Session-only history: what a run without a history directory keeps,
/// and the fallback when the disk store cannot open.
#[derive(Debug, Clone)]
pub struct MemoryStore {
    day: VecDeque<Bucket>,
    month: VecDeque<Bucket>,
    rollup: Rollup,
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self {
            day: VecDeque::new(),
            month: VecDeque::new(),
            rollup: Rollup::new(Tier::Month),
        }
    }
}

impl MemoryStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Buckets held across both tiers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.day.len() + self.month.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn prune(ring: &mut VecDeque<Bucket>, tier: Tier, now: u64) {
    while ring.front().is_some_and(|b| !tier.keeps(b.t, now)) {
        ring.pop_front();
    }
}

/// `stored`'s buckets in `from..to`, plus a rolled-up open span for Month:
/// oldest first, one bucket per start time. Only in-range buckets are
/// cloned: a 1 h window over a full day ring copies an hour, not the day.
#[must_use]
pub fn window<'a>(
    stored: impl IntoIterator<Item = &'a Bucket>,
    pending: &[Bucket],
    from: u64,
    to: u64,
    tier: Tier,
) -> Vec<Bucket> {
    let open = if tier.stored() == Tier::Month {
        downsample(pending, Tier::Month)
    } else {
        Vec::new()
    };
    let within = |b: &Bucket| (from..to).contains(&b.t);
    let mut out: Vec<Bucket> = stored
        .into_iter()
        .filter(|b| within(b))
        .cloned()
        .chain(open.into_iter().filter(within))
        .collect();
    // A wall clock stepped back re-records times already stored, out of
    // order: sort (stably) and fold each repeated start into one bucket, so
    // graphs keep both runs and totals never count a span twice.
    out.sort_by_key(|b| b.t);
    if out.windows(2).all(|w| w[0].t < w[1].t) {
        return out;
    }
    out.chunk_by(|a, b| a.t == b.t)
        .filter_map(|group| match group {
            [one] => Some(one.clone()),
            many => merge(many, many[0].t),
        })
        .collect()
}

impl HistoryStore for MemoryStore {
    fn append(&mut self, bucket: Bucket) -> Result<()> {
        let now = bucket.t;
        if let Some(coarse) = self.rollup.push(bucket.clone()) {
            self.month.push_back(coarse);
        }
        self.day.push_back(bucket);
        prune(&mut self.day, Tier::Day, now);
        prune(&mut self.month, Tier::Month, now);
        Ok(())
    }

    fn buckets(&self, from: u64, to: u64, tier: Tier) -> Result<Vec<Bucket>> {
        let ring = match tier.stored() {
            Tier::Month => &self.month,
            _ => &self.day,
        };
        Ok(window(ring, self.rollup.pending(), from, to, tier))
    }

    fn persistent(&self) -> bool {
        false
    }
}

/// The settings → About line for how far back history reaches.
pub const RETENTION_LABEL: &str = "30 days";

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn bucket(t: u64, cpu: f32) -> Bucket {
        let mut b = Bucket::empty(t);
        b.samples = 1;
        b.series[Series::Cpu.index()] = Some(Stat {
            mean: cpu,
            max: cpu,
        });
        b
    }

    #[test]
    fn series_indices_are_dense_and_in_order() {
        for (i, s) in Series::ALL.iter().enumerate() {
            assert_eq!(s.index(), i);
        }
        assert_eq!(SERIES_COUNT, 21);
        assert_eq!(Series::V1_COUNT, 14);
        assert_eq!(Series::V2_COUNT, 20);
    }

    #[test]
    fn an_unprimed_snapshot_records_no_rates() {
        let mut s = Snapshot::empty();
        s.primed = false;
        s.cpu.scaled = 0.0;
        for series in [Series::Cpu, Series::NetRxBps, Series::DiskWriteBps] {
            assert_eq!(series.read(&s), None, "{series:?}");
        }
    }

    /// Regression (UI-DESIGN §2): a desktop with no battery rail, a fanless
    /// Air, a Mac without a GPU reading: absent, never 0.
    #[test]
    fn absent_hardware_reads_none_not_zero() {
        let s = Snapshot::empty();
        for series in [
            Series::Gpu,
            Series::Fan,
            Series::SysPowerW,
            Series::GpuTempC,
        ] {
            assert_eq!(series.read(&s), None, "{series:?}");
        }
    }

    #[test]
    fn memory_store_rolls_up_and_answers_both_tiers() {
        let mut store = MemoryStore::new();
        for i in 0..50 {
            store.append(bucket(i * 6, 0.5)).unwrap();
        }
        let day = store.range(Series::Cpu, 0, u64::MAX, Tier::Day).unwrap();
        assert_eq!(day.len(), 50);
        let month = store.buckets(0, u64::MAX, Tier::Month).unwrap();
        // 0..144 and 144..288 closed; 288.. still open and rolled up on read.
        let ts: Vec<u64> = month.iter().map(|b| b.t).collect();
        assert_eq!(ts, [0, 144, 288]);
        assert!(!store.persistent());
    }

    #[test]
    fn live_queries_answer_from_day() {
        let mut store = MemoryStore::new();
        store.append(bucket(6, 0.1)).unwrap();
        assert_eq!(store.buckets(0, 100, Tier::Live).unwrap().len(), 1);
    }

    #[test]
    fn range_is_half_open() {
        let mut store = MemoryStore::new();
        for t in [0, 6, 12] {
            store.append(bucket(t, 0.1)).unwrap();
        }
        let ts: Vec<u64> = store
            .range(Series::Cpu, 6, 12, Tier::Day)
            .unwrap()
            .iter()
            .map(|p| p.t)
            .collect();
        assert_eq!(ts, [6]);
    }

    /// Fake clock: a day and a bit of appends leave only a day in Day.
    #[test]
    fn memory_store_drops_day_buckets_past_retention() {
        let mut store = MemoryStore::new();
        let end = Tier::Day.retention_s() + 600;
        let mut t = 0;
        while t <= end {
            store.append(bucket(t, 0.2)).unwrap();
            t += 6;
        }
        let day = store.buckets(0, u64::MAX, Tier::Day).unwrap();
        assert!(day.len() <= Tier::Day.capacity() + 1, "{}", day.len());
        assert!(day.iter().all(|b| Tier::Day.keeps(b.t, end)));
        assert!(!store.buckets(0, u64::MAX, Tier::Month).unwrap().is_empty());
    }

    #[test]
    fn range_skips_absent_series() {
        let mut store = MemoryStore::new();
        store.append(bucket(0, 0.3)).unwrap();
        assert!(
            store
                .range(Series::Gpu, 0, 10, Tier::Day)
                .unwrap()
                .is_empty()
        );
    }

    /// Regression: a held power reading past its stale age was folded into
    /// history means and maxes as if measured.
    #[test]
    fn stale_held_power_is_not_recorded() {
        let mut s = Snapshot::empty();
        s.primed = true;
        let held = Reading {
            watts: 21.5,
            source: crate::snapshot::ReadingSource::Held {
                age: crate::snapshot::POWER_STALE_AFTER * 2,
            },
        };
        s.power.system = Some(held);
        s.power.cpu = Some(held);
        s.power.gpu = Some(Reading::measured(1.5));
        assert_eq!(Series::SysPowerW.read(&s), None);
        assert_eq!(Series::CpuPowerW.read(&s), None);
        assert_eq!(Series::GpuPowerW.read(&s), Some(1.5));
    }

    fn laptop(source: PowerSource) -> Snapshot {
        let mut s = Snapshot::empty();
        s.primed = true;
        s.battery.present = true;
        s.battery.source = source;
        s.battery.percent = Some(64);
        s.battery.watts = Some(-8.5);
        s.battery.health_pct = Some(91);
        s
    }

    /// Regression (UI-DESIGN §2): a desktop has AC by definition and no
    /// battery; recording "on AC = 1" or "0 %" would fake a laptop.
    #[test]
    fn a_desktop_records_no_battery_columns() {
        let mut s = Snapshot::empty();
        s.primed = true;
        s.battery.source = PowerSource::Ac;
        for series in [
            Series::BatteryPct,
            Series::BatteryW,
            Series::OnAc,
            Series::BatteryHealth,
        ] {
            assert_eq!(series.read(&s), None, "{series:?}");
        }
    }

    #[test]
    fn a_laptop_records_signed_watts_percent_and_health() {
        let s = laptop(PowerSource::Battery);
        assert_eq!(Series::BatteryPct.read(&s), Some(64.0));
        assert_eq!(Series::BatteryW.read(&s), Some(-8.5));
        assert_eq!(Series::BatteryHealth.read(&s), Some(91.0));
    }

    #[test]
    fn on_ac_table() {
        let cases = [
            (PowerSource::Ac, Some(1.0)),
            (PowerSource::Ups, Some(1.0)),
            (PowerSource::Battery, Some(0.0)),
            (PowerSource::Unknown, None),
        ];
        for (source, want) in cases {
            assert_eq!(Series::OnAc.read(&laptop(source)), want, "{source:?}");
        }
    }

    /// Regression: the live graphs and stored history read memory and
    /// temperatures under separate rules (live drew unread memory as 0 %).
    /// Both now go through the same snapshot accessors.
    #[test]
    fn live_and_stored_series_agree_on_shared_values() {
        use crate::series::SeriesId;
        let mut s = Snapshot::empty();
        s.sampled.sensors = true;
        let pairs = [
            (SeriesId::Mem, Series::Mem),
            (SeriesId::CpuTemp, Series::CpuTempC),
            (SeriesId::GpuTemp, Series::GpuTempC),
        ];
        for (live, stored) in pairs {
            assert_eq!((live.def().extract)(&s), None, "{live:?}: absent, not 0");
            assert_eq!(stored.read(&s), None, "{stored:?}");
        }
        s.memory.total_bytes = 4;
        s.memory.used_bytes = 1;
        s.sensors.p_c = Some(70.0);
        s.sensors.gpu_c = Some(55.0);
        for (live, stored) in pairs {
            assert_eq!((live.def().extract)(&s), stored.read(&s), "{live:?}");
            assert!(stored.read(&s).is_some(), "{stored:?}");
        }
    }

    /// Regression: "time at 95 °C+" counted a whole bucket hot for one
    /// sample; the Hot column records each sample, absent without a sensor.
    #[test]
    fn hot_marks_each_sample_at_or_above_the_threshold() {
        let mut s = Snapshot::empty();
        assert_eq!(Series::Hot.read(&s), None, "no sensor: absent, not cool");
        for (c, want) in [(94.9, 0.0), (HOT_C, 1.0), (101.0, 1.0)] {
            s.cpu.temp_c = Some(c);
            assert_eq!(Series::Hot.read(&s), Some(want), "{c}");
        }
    }

    #[test]
    fn thermal_state_and_throttled_table() {
        let cases = [
            (Thermal::Nominal, 0.0, 0.0),
            (Thermal::Fair, 1.0, 0.0),
            (Thermal::Serious, 2.0, 1.0),
            (Thermal::Critical, 3.0, 1.0),
        ];
        for (thermal, level, throttled) in cases {
            let mut s = Snapshot::empty();
            s.primed = true;
            s.thermal = thermal;
            assert_eq!(Series::ThermalState.read(&s), Some(level), "{thermal:?}");
            assert_eq!(Series::Throttled.read(&s), Some(throttled), "{thermal:?}");
            s.primed = false;
            assert_eq!(Series::ThermalState.read(&s), None);
        }
    }

    /// The v1 columns keep their on-disk order; v2 and v3 only append.
    #[test]
    fn v2_series_are_appended_after_the_v1_columns() {
        assert_eq!(Series::Fan.index(), Series::V1_COUNT - 1);
        assert_eq!(Series::BatteryPct.index(), Series::V1_COUNT);
        assert_eq!(Series::BatteryHealth.index(), Series::V2_COUNT - 1);
        assert_eq!(Series::Hot.index(), SERIES_COUNT - 1);
    }
}
