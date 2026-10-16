//! Series registry: every time series the app records, declared once.
//!
//! A [`SeriesDef`] names how a value is pulled out of a [`Snapshot`], on
//! which [`Clock`] it moves, and how to label it. [`Histories`] owns one ring
//! per series and is the single place that enforces "never push a missing or
//! stale value": an `extract` that returns `None` adds nothing, and a sensor
//! series is only extended on ticks that actually read the sensors.

use std::time::Duration;

use crate::config::SENSOR_PERIOD;
use crate::history::{History, Stamp, capacity_for};
use crate::snapshot::{Cluster, Reading, Snapshot};

/// Fans the dashboard tracks history for; more are listed but not graphed.
pub const FAN_SERIES_MAX: usize = 4;

/// How often a series gains a sample, which sets its ring's period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Clock {
    /// Every tick, at the user's sampling interval (load, rates).
    Cheap,
    /// Only on ticks that read the SMC / HID sensors ([`SENSOR_PERIOD`]).
    Sensor,
    /// Every tick, but the source updates on its own clock (power at
    /// [`SENSOR_PERIOD`], battery every 5 s) and `extract` rejects values the
    /// snapshot marks held. Its gaps are judged at [`SENSOR_PERIOD`], but the
    /// ring is sized for every tick: a source that re-reports its last value
    /// each tick must not cut the window to 2 s × capacity.
    Slow,
}

impl Clock {
    /// Ring period for this clock when sampling every `interval`.
    #[must_use]
    pub const fn period(self, interval: Duration) -> Duration {
        match self {
            Self::Cheap => interval,
            Self::Sensor | Self::Slow => SENSOR_PERIOD,
        }
    }

    /// Ring capacity for this clock when sampling every `interval`: enough
    /// for the whole history window at the fastest rate it can gain samples.
    #[must_use]
    pub fn capacity(self, interval: Duration) -> usize {
        match self {
            Self::Cheap => capacity_for(interval),
            Self::Sensor => capacity_for(SENSOR_PERIOD),
            Self::Slow => capacity_for(interval.min(SENSOR_PERIOD)),
        }
    }

    /// Does this tick carry a value for this clock?
    #[must_use]
    pub const fn ticks(self, snap: &Snapshot) -> bool {
        match self {
            Self::Cheap | Self::Slow => true,
            Self::Sensor => snap.sampled.sensors,
        }
    }
}

/// One recorded series. The discriminant indexes [`SERIES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SeriesId {
    Cpu,
    Gpu,
    Mem,
    NetRx,
    NetTx,
    Disk,
    DiskRead,
    DiskWrite,
    CpuTemp,
    GpuTemp,
    ETemp,
    PTemp,
    STemp,
    ELoad,
    PLoad,
    SLoad,
    PowerSystem,
    BatteryPct,
    BatteryWatts,
}

/// How to record one series.
#[derive(Debug, Clone, Copy)]
pub struct SeriesDef {
    pub id: SeriesId,
    pub clock: Clock,
    /// The value this snapshot carries, or `None` when it is missing or stale.
    pub extract: fn(&Snapshot) -> Option<f32>,
    pub label: &'static str,
    pub unit: &'static str,
}

const fn def(
    id: SeriesId,
    clock: Clock,
    extract: fn(&Snapshot) -> Option<f32>,
    label: &'static str,
    unit: &'static str,
) -> SeriesDef {
    SeriesDef {
        id,
        clock,
        extract,
        label,
        unit,
    }
}

#[expect(clippy::cast_precision_loss, reason = "rates are graphed, not summed")]
const fn rate(bps: u64) -> f32 {
    bps as f32
}

fn cluster(cluster: Option<&Cluster>) -> Option<f32> {
    cluster.map(|c| c.scaled)
}

/// Every series, in [`SeriesId`] order.
pub const SERIES: [SeriesDef; SERIES_COUNT] = {
    use Clock::{Cheap, Sensor, Slow};
    use SeriesId as S;
    [
        def(S::Cpu, Cheap, |s| Some(s.cpu.scaled), "cpu", "%"),
        def(S::Gpu, Cheap, |s| s.gpu.map(|g| g.scaled), "gpu", "%"),
        def(S::Mem, Cheap, |s| s.memory.used_ratio(), "mem", "%"),
        def(
            S::NetRx,
            Cheap,
            |s| Some(rate(s.network.rx_bps)),
            "net rx",
            "bit/s",
        ),
        def(
            S::NetTx,
            Cheap,
            |s| Some(rate(s.network.tx_bps)),
            "net tx",
            "bit/s",
        ),
        def(
            S::Disk,
            Cheap,
            |s| Some(rate(s.disk.read_bps.saturating_add(s.disk.write_bps))),
            "disk",
            "B/s",
        ),
        def(
            S::DiskRead,
            Cheap,
            |s| Some(rate(s.disk.read_bps)),
            "disk read",
            "B/s",
        ),
        def(
            S::DiskWrite,
            Cheap,
            |s| Some(rate(s.disk.write_bps)),
            "disk write",
            "B/s",
        ),
        def(S::CpuTemp, Sensor, Snapshot::cpu_temp_c, "cpu temp", "°C"),
        def(S::GpuTemp, Sensor, Snapshot::gpu_temp_c, "gpu temp", "°C"),
        def(S::ETemp, Sensor, |s| s.sensors.e_c, "e-cluster temp", "°C"),
        def(S::PTemp, Sensor, |s| s.sensors.p_c, "p-cluster temp", "°C"),
        def(S::STemp, Sensor, |s| s.sensors.s_c, "s-cluster temp", "°C"),
        def(
            S::ELoad,
            Cheap,
            |s| cluster(s.cpu.e_cluster.as_ref()),
            "e-cluster",
            "%",
        ),
        def(
            S::PLoad,
            Cheap,
            |s| cluster(s.cpu.p_cluster.as_ref()),
            "p-cluster",
            "%",
        ),
        def(
            S::SLoad,
            Cheap,
            |s| cluster(s.cpu.s_cluster.as_ref()),
            "s-cluster",
            "%",
        ),
        def(
            S::PowerSystem,
            Slow,
            |s| s.power.system.and_then(Reading::fresh),
            "system power",
            "W",
        ),
        def(
            S::BatteryPct,
            Slow,
            |s| {
                s.battery
                    .present
                    .then_some(s.battery.percent)
                    .flatten()
                    .map(|p| f32::from(p) / 100.0)
            },
            "battery",
            "%",
        ),
        def(
            S::BatteryWatts,
            Slow,
            |s| {
                s.battery
                    .present
                    .then_some(s.battery.watts)
                    .flatten()
                    .filter(|w| w.is_finite())
                    .map(f32::abs)
            },
            "battery power",
            "W",
        ),
    ]
};

const SERIES_COUNT: usize = 19;

impl SeriesId {
    /// Every id, in registry order.
    pub const ALL: [Self; SERIES_COUNT] = {
        let mut ids = [Self::Cpu; SERIES_COUNT];
        let mut i = 0;
        while i < SERIES_COUNT {
            ids[i] = SERIES[i].id;
            i += 1;
        }
        ids
    };

    const fn index(self) -> usize {
        self as usize
    }

    /// This series' registry entry.
    #[must_use]
    pub const fn def(self) -> &'static SeriesDef {
        &SERIES[self.index()]
    }
}

/// One history ring per registered series, plus per-fan RPM rings.
#[derive(Debug, Clone)]
pub struct Histories {
    series: [History; SERIES_COUNT],
    fans: Vec<History>,
}

impl Default for Histories {
    /// Every ring at [`SENSOR_PERIOD`], same as `History::default()`.
    fn default() -> Self {
        Self::new(SENSOR_PERIOD)
    }
}

impl Histories {
    /// Empty rings sized for sampling every `interval`.
    #[must_use]
    pub fn new(interval: Duration) -> Self {
        Self {
            series: SERIES.map(|d| {
                let mut ring = History::for_period(d.clock.period(interval));
                ring.set_capacity(d.clock.capacity(interval));
                ring
            }),
            fans: Vec::new(),
        }
    }

    #[must_use]
    pub fn get(&self, id: SeriesId) -> &History {
        &self.series[id.index()]
    }

    pub fn get_mut(&mut self, id: SeriesId) -> &mut History {
        &mut self.series[id.index()]
    }

    /// Per-fan RPM, one ring per fan up to [`FAN_SERIES_MAX`].
    #[must_use]
    pub fn fans(&self) -> &[History] {
        &self.fans
    }

    pub fn fans_mut(&mut self) -> &mut Vec<History> {
        &mut self.fans
    }

    /// Adopt a new sampling interval: [`Clock::Cheap`] rings retime to it,
    /// and [`Clock::Slow`] rings grow to hold the window at the new rate.
    /// No ring drops samples it already holds.
    pub fn set_interval(&mut self, interval: Duration) {
        for (def, history) in SERIES.iter().zip(&mut self.series) {
            match def.clock {
                Clock::Cheap => history.set_period(interval),
                Clock::Slow => {
                    let want = def.clock.capacity(interval).max(history.len());
                    history.set_capacity(want);
                }
                Clock::Sensor => {}
            }
        }
    }

    /// Record `snap` at `at`. The one place that decides what is recorded:
    /// a series whose clock did not tick, or whose value is missing or stale,
    /// gains nothing (never a fake zero).
    pub fn push_snapshot(&mut self, snap: &Snapshot, at: Stamp) {
        for (def, history) in SERIES.iter().zip(&mut self.series) {
            if def.clock.ticks(snap)
                && let Some(value) = (def.extract)(snap)
            {
                history.push_at(value, at);
            }
        }
        if Clock::Sensor.ticks(snap) {
            let fans = &snap.fans.fans;
            let n = fans.len().min(FAN_SERIES_MAX);
            while self.fans.len() < n {
                self.fans.push(History::for_period(SENSOR_PERIOD));
            }
            for (fan, history) in fans.iter().zip(&mut self.fans) {
                history.push_at(f32::from(fan.rpm), at);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::config::INTERVAL_DEFAULT;
    use crate::snapshot::{Cluster, ClusterKind, FanMetric, GpuSnapshot, Reading, ReadingSource};

    fn live() -> Snapshot {
        let mut s = Snapshot::empty();
        s.sampled.sensors = true;
        s
    }

    fn fan(rpm: u16) -> FanMetric {
        FanMetric {
            name: String::from("fan"),
            rpm,
            max_rpm: 6000,
        }
    }

    #[test]
    fn registry_is_indexed_by_id() {
        for (i, def) in SERIES.iter().enumerate() {
            assert_eq!(def.id.index(), i, "{:?} out of order", def.id);
            assert_eq!(SeriesId::ALL[i], def.id);
            assert!(!def.label.is_empty() && !def.unit.is_empty());
        }
    }

    #[test]
    fn labels_are_unique() {
        let mut labels: Vec<_> = SERIES.iter().map(|d| d.label).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), SERIES_COUNT);
    }

    #[test]
    fn cheap_rings_follow_the_interval_sensor_rings_do_not() {
        let fast = Duration::from_millis(250);
        let h = Histories::new(fast);
        for def in &SERIES {
            let want = def.clock.period(fast);
            assert_eq!(h.get(def.id).period(), want, "{:?}", def.id);
            let cap = def.clock.capacity(fast);
            assert_eq!(h.get(def.id).capacity(), cap, "{:?}", def.id);
        }
        assert_eq!(h.get(SeriesId::Cpu).period(), fast);
        assert_eq!(h.get(SeriesId::CpuTemp).period(), SENSOR_PERIOD);
        assert_eq!(h.get(SeriesId::PowerSystem).period(), SENSOR_PERIOD);
    }

    #[test]
    fn set_interval_retimes_only_cheap_rings() {
        let mut h = Histories::new(INTERVAL_DEFAULT);
        let slow = Duration::from_secs(5);
        h.set_interval(slow);
        for def in &SERIES {
            let want = if def.clock == Clock::Cheap {
                slow
            } else {
                SENSOR_PERIOD
            };
            assert_eq!(h.get(def.id).period(), want, "{:?}", def.id);
        }
    }

    #[test]
    fn default_matches_history_default() {
        let h = Histories::default();
        for id in SeriesId::ALL {
            assert_eq!(h.get(id).period(), History::default().period());
            assert_eq!(h.get(id).capacity(), History::default().capacity());
            assert!(h.get(id).is_empty());
        }
        assert!(h.fans().is_empty());
    }

    #[test]
    fn empty_snapshot_records_only_always_present_series() {
        use SeriesId as S;
        let mut h = Histories::default();
        h.push_snapshot(&live(), 1);
        let recorded: Vec<_> = SeriesId::ALL
            .into_iter()
            .filter(|&id| !h.get(id).is_empty())
            .collect();
        assert_eq!(
            recorded,
            [
                S::Cpu,
                S::NetRx,
                S::NetTx,
                S::Disk,
                S::DiskRead,
                S::DiskWrite
            ]
        );
    }

    #[test]
    fn missing_gpu_is_never_zero_filled() {
        let mut h = Histories::default();
        h.push_snapshot(&live(), 1);
        assert!(h.get(SeriesId::Gpu).is_empty());
        assert!(h.get(SeriesId::GpuTemp).is_empty());
    }

    #[test]
    fn gpu_temp_prefers_gpu_reading_then_sensor() {
        let mut s = live();
        s.sensors.gpu_c = Some(40.0);
        let mut h = Histories::default();
        h.push_snapshot(&s, 1);
        assert_eq!(h.get(SeriesId::GpuTemp).last(), Some(40.0));
        s.gpu = Some(GpuSnapshot {
            temp_c: Some(55.0),
            scaled: 0.3,
            ..GpuSnapshot::default()
        });
        h.push_snapshot(&s, 2);
        assert_eq!(h.get(SeriesId::GpuTemp).last(), Some(55.0));
        assert_eq!(h.get(SeriesId::Gpu).last(), Some(0.3));
    }

    #[test]
    fn sensor_series_skip_ticks_without_a_sensor_read() {
        let mut s = live();
        s.sensors.p_c = Some(60.0);
        s.fans.fans = vec![fan(1200)];
        let mut h = Histories::default();
        h.push_snapshot(&s, 1);
        s.sampled.sensors = false;
        s.sensors.p_c = Some(99.0);
        s.fans.fans = vec![fan(5000)];
        h.push_snapshot(&s, 2);
        assert_eq!(h.get(SeriesId::PTemp).len(), 1);
        assert_eq!(h.get(SeriesId::PTemp).last(), Some(60.0));
        assert_eq!(h.fans()[0].last(), Some(1200.0));
        assert_eq!(h.fans()[0].len(), 1);
    }

    #[test]
    fn slow_series_ignore_held_power() {
        let mut s = live();
        s.power.system = Some(Reading {
            watts: 9.0,
            source: ReadingSource::Held {
                age: Duration::from_secs(2),
            },
        });
        let mut h = Histories::default();
        h.push_snapshot(&s, 1);
        assert!(h.get(SeriesId::PowerSystem).is_empty(), "held is not new");
        s.power.system = Some(Reading::measured(12.5));
        s.sampled.sensors = false;
        h.push_snapshot(&s, 2);
        assert_eq!(
            h.get(SeriesId::PowerSystem).last(),
            Some(12.5),
            "slow series record on non-sensor ticks"
        );
    }

    /// Regression: power and battery rings were sized for one sample per
    /// 2 s but gained one per tick, so at 500 ms the 5m/15m graphs held
    /// only the last 225 s (112 s at 250 ms).
    #[test]
    fn slow_rings_keep_the_whole_window_when_fed_every_tick() {
        let mut s = live();
        s.power.system = Some(Reading::measured(20.0));
        s.battery.present = true;
        s.battery.percent = Some(80);
        s.battery.watts = Some(-6.0);
        let window = crate::config::HISTORY_WINDOW.as_millis() as u64;
        for ms in [250_u64, 500, 1000, 2000, 5000] {
            let interval = Duration::from_millis(ms);
            let mut retimed = Histories::new(INTERVAL_DEFAULT);
            retimed.set_interval(interval);
            for mut h in [Histories::new(interval), retimed] {
                // One sample per tick across the whole window.
                let ticks = window / ms;
                for i in 0..ticks {
                    h.push_snapshot(&s, i * ms);
                }
                for id in [
                    SeriesId::PowerSystem,
                    SeriesId::BatteryPct,
                    SeriesId::BatteryWatts,
                ] {
                    assert_eq!(h.get(id).len() as u64, ticks, "{id:?} at {ms} ms");
                }
            }
        }
    }

    #[test]
    fn fresh_skips_held_and_broken_readings() {
        assert_eq!(Reading::measured(4.0).fresh(), Some(4.0));
        let held = Reading {
            watts: 4.0,
            source: ReadingSource::Held {
                age: Duration::from_secs(2),
            },
        };
        assert_eq!(held.fresh(), None);
        assert_eq!(Reading::measured(f32::NAN).fresh(), None);
        let est = Reading {
            watts: 1.5,
            source: ReadingSource::Estimated,
        };
        assert_eq!(est.fresh(), Some(1.5));
    }

    #[test]
    fn nan_power_is_never_recorded() {
        let mut s = live();
        s.power.system = Some(Reading::measured(f32::NAN));
        let mut h = Histories::default();
        h.push_snapshot(&s, 1);
        assert!(h.get(SeriesId::PowerSystem).is_empty());
    }

    #[test]
    fn battery_series_need_a_present_battery() {
        let mut s = live();
        s.battery.percent = Some(80);
        s.battery.watts = Some(-7.0);
        let mut h = Histories::default();
        h.push_snapshot(&s, 1);
        assert!(
            h.get(SeriesId::BatteryPct).is_empty(),
            "desktop: no battery"
        );
        assert!(h.get(SeriesId::BatteryWatts).is_empty());
        s.battery.present = true;
        h.push_snapshot(&s, 2);
        assert_eq!(h.get(SeriesId::BatteryPct).last(), Some(0.8));
        assert_eq!(h.get(SeriesId::BatteryWatts).last(), Some(7.0), "magnitude");
    }

    #[test]
    fn battery_watts_rejects_non_finite() {
        let mut s = live();
        s.battery.present = true;
        s.battery.watts = Some(f32::INFINITY);
        let mut h = Histories::default();
        h.push_snapshot(&s, 1);
        assert!(h.get(SeriesId::BatteryWatts).is_empty());
    }

    #[test]
    fn cluster_load_records_only_present_families() {
        let mut s = live();
        s.cpu.s_cluster = Some(Cluster {
            kind: ClusterKind::Super,
            scaled: 0.81,
            active: 0.81,
            freq_mhz: 3000,
        });
        let mut h = Histories::default();
        h.push_snapshot(&s, 1);
        assert_eq!(h.get(SeriesId::SLoad).last(), Some(0.81));
        assert!(h.get(SeriesId::ELoad).is_empty());
        assert!(h.get(SeriesId::PLoad).is_empty());
    }

    /// Unread memory (total 0) is neither NaN nor a made-up 0 %: the ring
    /// gains nothing, as stored history always did.
    #[test]
    fn zero_memory_total_records_nothing_not_nan_or_zero() {
        let mut h = Histories::default();
        h.push_snapshot(&live(), 1);
        assert!(h.get(SeriesId::Mem).is_empty());
    }

    #[test]
    fn memory_ratio_is_used_over_total() {
        let mut s = live();
        s.memory.total_bytes = 1000;
        s.memory.used_bytes = 250;
        let mut h = Histories::default();
        h.push_snapshot(&s, 1);
        assert_eq!(h.get(SeriesId::Mem).last(), Some(0.25));
    }

    #[test]
    fn disk_total_saturates_instead_of_overflowing() {
        let mut s = live();
        s.disk.read_bps = u64::MAX;
        s.disk.write_bps = 1;
        let mut h = Histories::default();
        h.push_snapshot(&s, 1);
        assert_eq!(h.get(SeriesId::Disk).last(), Some(u64::MAX as f32));
    }

    #[test]
    fn fans_cap_at_four_rings() {
        let mut s = live();
        s.fans.fans = (0..6).map(|i| fan(1000 + i)).collect();
        let mut h = Histories::default();
        h.push_snapshot(&s, 1);
        assert_eq!(h.fans().len(), FAN_SERIES_MAX);
        assert_eq!(h.fans()[3].last(), Some(1003.0));
    }

    #[test]
    fn fan_rings_grow_when_a_fan_appears_and_are_kept_when_it_goes() {
        let mut s = live();
        s.fans.fans = vec![fan(900)];
        let mut h = Histories::default();
        h.push_snapshot(&s, 1);
        s.fans.fans = vec![fan(900), fan(1100)];
        h.push_snapshot(&s, 2);
        assert_eq!(h.fans().len(), 2);
        s.fans.fans.clear();
        h.push_snapshot(&s, 3);
        assert_eq!(h.fans().len(), 2, "rings are not dropped");
        assert_eq!(h.fans()[1].len(), 1);
    }

    #[test]
    fn fanless_air_has_no_fan_rings() {
        let mut h = Histories::default();
        for t in 0..5 {
            h.push_snapshot(&live(), t);
        }
        assert!(h.fans().is_empty());
    }

    #[test]
    fn capacity_bounds_every_ring() {
        let mut s = live();
        s.sensors.cpu_c = Some(50.0);
        let mut h = Histories::new(Duration::from_secs(1));
        let cap = h.get(SeriesId::Cpu).capacity();
        for t in 0..(cap as u64 * 2) {
            h.push_snapshot(&s, t * 1000);
        }
        for id in SeriesId::ALL {
            assert!(h.get(id).len() <= h.get(id).capacity(), "{id:?}");
        }
        assert_eq!(h.get(SeriesId::Cpu).len(), cap);
    }

    #[test]
    fn cpu_temp_falls_back_to_best_sensor() {
        let mut s = live();
        s.sensors.cpu_c = Some(47.0);
        let mut h = Histories::default();
        h.push_snapshot(&s, 1);
        assert!(h.get(SeriesId::CpuTemp).last().is_some());
        s.cpu.temp_c = Some(61.0);
        h.push_snapshot(&s, 2);
        assert_eq!(h.get(SeriesId::CpuTemp).last(), Some(61.0));
    }
}
