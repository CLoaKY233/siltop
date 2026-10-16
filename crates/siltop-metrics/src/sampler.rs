use std::thread;
use std::time::{Duration, Instant};

use siltop_core::{CollectorId, Health, PowerSnapshot, Reading, Result, Snapshot, SocInfo};

use crate::battery::BatteryCollector;
use crate::cadence::PowerMode;
use crate::collector::{Collector, Tracked};
use crate::cpu::CpuCollector;
use crate::disk::DiskCollector;
use crate::gpu::GpuCollector;
use crate::ioreport::FreqFeed;
use crate::memory::MemoryCollector;
use crate::net::NetCollector;
use crate::process::ProcessCollector;
use crate::sensors::{SensorFrame, SensorHub, SlowThread, sensor_period};
use crate::soc;

/// Test-only: live `Sampler`s in parallel tests contend for the SMC and
/// time out, so tests that open real hardware hold this gate. Production
/// never takes it: a second `Sampler` in one process just works.
#[cfg(test)]
static IO_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn io_gate() -> std::sync::MutexGuard<'static, ()> {
    IO_GATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The baseline taken in `Sampler::new` is at least this old before the
/// first snapshot, so its rates cover a real interval (10 stat ticks/core).
const PRIME_WINDOW: Duration = Duration::from_millis(100);

/// Slow-thread values older than this many periods are reported stale.
const STALE_PERIODS: u32 = 3;

const SLOW_IDS: [CollectorId; 3] = [
    CollectorId::Sensors,
    CollectorId::Fans,
    CollectorId::Thermal,
];

/// Cheap-tick collectors, in the order they run. A new collector is one
/// line here plus its `Collector` impl.
fn registered(soc: &SocInfo, freq: &FreqFeed, mode: &PowerMode) -> Vec<Box<dyn Collector>> {
    vec![
        Box::new(CpuCollector::new(soc.clone()).with_freq(freq.clone())),
        Box::new(MemoryCollector::new()),
        Box::new(GpuCollector::new().with_freq(freq.clone())),
        Box::new(NetCollector::new()),
        Box::new(DiskCollector::new()),
        Box::new(ProcessCollector::new().with_power(mode.clone())),
        Box::new(BatteryCollector::new()),
    ]
}

fn fill_missing_temps(snap: &mut Snapshot) {
    if snap.cpu.temp_c.is_none() {
        snap.cpu.temp_c = snap.sensors.best_cpu_c();
    }
    if let Some(gpu) = snap.gpu.as_mut()
        && gpu.temp_c.is_none()
    {
        gpu.temp_c = snap.sensors.named_gpu_c();
    }
}

/// Produces one [`Snapshot`] per call to [`Sampler::tick`].
///
/// Cheap collectors run inline on every tick; each failure keeps that
/// collector's last good value and marks it stale in `Snapshot::health`.
/// Sensors, fans and the thermal state come from a slow thread and are
/// merged without waiting.
pub struct Sampler {
    snap: Snapshot,
    collectors: Vec<Tracked>,
    slow: Option<SlowThread>,
    slow_period: Duration,
    slow_last_ok: [Option<Instant>; 3],
    /// A sensor frame (or its absence verdict) has landed.
    slow_seen: bool,
    /// SMC figures from the last sensor frame, before the energy rails are
    /// laid over them each tick.
    smc_power: PowerSnapshot,
    freq: FreqFeed,
    /// Low-power (on battery) cadences, shared with the collectors and the
    /// sensor thread.
    mode: PowerMode,
    prime_until: Option<Instant>,
}

impl Sampler {
    /// Starts the sensor thread and records baseline counters, so the first
    /// [`tick`](Self::tick) already reports real rates.
    pub fn new() -> Result<Self> {
        let soc = soc::info();
        let (e, p, s) = (soc.e_cores, soc.p_cores, soc.s_cores);
        let period = SensorHub::period();
        let mode = PowerMode::default();
        let slow = SlowThread::spawn_paced(period, mode.clone(), move || SensorHub::open(e, p, s));
        let freq = FreqFeed::spawn();
        let collectors = registered(&soc, &freq, &mode);
        let mut sampler = Self::assemble(soc, collectors, slow, period).with_freq(freq);
        sampler.mode = mode;
        Ok(sampler)
    }

    /// Low-power mode (on battery): the process table every 4 s and the
    /// SMC sweep every 10 s. The worker stretches the tick itself.
    pub fn set_low_power(&self, low: bool) {
        self.mode.set(low);
    }

    fn with_freq(mut self, freq: FreqFeed) -> Self {
        self.freq = freq;
        self
    }

    /// Tells the background threads the snapshot interval, so they sample
    /// no faster than snapshots are taken.
    pub fn set_interval(&self, interval: Duration) {
        self.mode.set_interval(interval);
        self.freq.set_interval(interval);
    }

    /// What the visible views read. With CPU clocks and watts both parked
    /// the PMGR sample (6–8 ms) stops once the clocks settle and runs again
    /// only on a load swing; parked clocks hold their last reading (they
    /// still scale the live busy figure), parked watts go absent after
    /// their usual age, and parked GPU residency is not sampled at all.
    /// Re-enabling a source wakes the thread, which samples it at once.
    pub fn set_needs(&self, needs: crate::Needs) {
        self.freq.set_needs(needs);
    }

    /// Parks the sensor and `IOReport` threads while the app is frozen, and
    /// wakes them on thaw. Held power readings age out on their own.
    pub fn set_paused(&self, paused: bool) {
        if let Some(slow) = &self.slow {
            slow.set_paused(paused);
        }
        self.freq.set_paused(paused);
    }

    fn assemble(
        soc: SocInfo,
        collectors: Vec<Box<dyn Collector>>,
        slow: Option<SlowThread>,
        slow_period: Duration,
    ) -> Self {
        let mut snap = Snapshot::empty();
        snap.soc = soc;
        if slow.is_none() {
            for id in SLOW_IDS {
                let reason = String::from("sensor thread did not start");
                snap.health.set(id, Health::Failed(reason));
            }
        }
        let mut sampler = Self {
            snap,
            collectors: collectors.into_iter().map(Tracked::new).collect(),
            slow,
            slow_period,
            slow_last_ok: [None; 3],
            slow_seen: false,
            smc_power: PowerSnapshot::default(),
            freq: FreqFeed::default(),
            mode: PowerMode::default(),
            prime_until: None,
        };
        let now = Instant::now();
        for collector in &mut sampler.collectors {
            collector.run(now, &mut sampler.snap);
        }
        sampler.prime_until = Some(now + PRIME_WINDOW);
        sampler
    }

    /// Never fails as a whole: a failed collector is reported through
    /// `Snapshot::health`.
    pub fn tick(&mut self) -> Result<Snapshot> {
        let primed_frame = self
            .prime_until
            .take()
            .and_then(|until| self.finish_priming(until));
        let now = Instant::now();
        let mut unprimed = false;
        for collector in &mut self.collectors {
            unprimed |= collector.run(now, &mut self.snap);
        }
        let frame = self
            .slow
            .as_ref()
            .and_then(SlowThread::latest)
            .or(primed_frame);
        self.merge_slow(frame, now);
        self.snap.power = self.smc_power;
        if let Some(rails) = self.freq.energy(now) {
            rails.apply(&mut self.snap.power);
        }
        fill_missing_temps(&mut self.snap);
        self.snap.cpu.watts = self.snap.power.cpu.and_then(Reading::usable);
        self.snap.collected_at = now;
        self.snap.primed = !unprimed;
        self.snap.settled = self.settled();
        Ok(self.snap.clone())
    }

    /// Every slow source that exists here has delivered its first reading.
    /// The battery needs no wait: its collector runs inline from the start.
    fn settled(&self) -> bool {
        (self.slow.is_none() || self.slow_seen) && self.freq.settled()
    }

    /// Lets the priming baseline age to [`PRIME_WINDOW`], spending the wait
    /// on the sensor thread's first frame rather than a blind sleep.
    fn finish_priming(&self, until: Instant) -> Option<SensorFrame> {
        let frame = self
            .slow
            .as_ref()
            .and_then(|slow| slow.wait(until.saturating_duration_since(Instant::now())));
        let rest = until.saturating_duration_since(Instant::now());
        if !rest.is_zero() {
            thread::sleep(rest);
        }
        frame
    }

    fn merge_slow(&mut self, frame: Option<SensorFrame>, now: Instant) {
        self.snap.sampled.sensors = false;
        let Some(frame) = frame else {
            self.age_slow(now);
            return;
        };
        let at = frame.at;
        self.slow_seen = true;
        let [sensors_ok, fans_ok, thermal_ok] = &mut self.slow_last_ok;
        let snap = &mut self.snap;
        let sensors = merge_part(
            &mut snap.sensors,
            frame.sensors,
            frame.sensors_health,
            sensors_ok,
            at,
        );
        snap.sampled.sensors = sensors.is_fresh();
        snap.health.set(CollectorId::Sensors, sensors);
        let fans = merge_part(&mut snap.fans, frame.fans, frame.fans_health, fans_ok, at);
        snap.health.set(CollectorId::Fans, fans);
        let thermal = merge_part(
            &mut snap.thermal,
            frame.thermal,
            frame.thermal_health,
            thermal_ok,
            at,
        );
        snap.health.set(CollectorId::Thermal, thermal);
        self.smc_power = frame.power;
    }

    /// Marks slow values stale once the thread has been silent too long.
    fn age_slow(&mut self, now: Instant) {
        let limit = sensor_period(self.slow_period, &self.mode) * STALE_PERIODS;
        for (id, last_ok) in SLOW_IDS.into_iter().zip(self.slow_last_ok) {
            if let Some(age) = last_ok.map(|t| now.saturating_duration_since(t))
                && age > limit
                && self.snap.health.is_fresh(id)
            {
                self.snap.health.set(id, Health::Stale(age));
            }
        }
    }
}

/// Takes a slow-thread value when it is fresh or absent; otherwise keeps
/// the previous one and reports how old it is.
fn merge_part<T>(
    slot: &mut T,
    value: T,
    health: Health,
    last_ok: &mut Option<Instant>,
    at: Instant,
) -> Health {
    match health {
        Health::Ok => {
            *slot = value;
            *last_ok = Some(at);
            Health::Ok
        }
        Health::Absent => {
            *slot = value;
            Health::Absent
        }
        failure => match *last_ok {
            Some(ok) => Health::Stale(at.saturating_duration_since(ok)),
            None => failure,
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::collector::Outcome;
    use crate::ioreport::{EnergyFrame, FreqFrame};
    use crate::power::RailReadings;
    use crate::sensors::testing::Scripted;
    use siltop_core::Error;
    use siltop_core::Reading;

    struct Fake {
        id: CollectorId,
        fail: bool,
    }

    impl Collector for Fake {
        fn id(&self) -> CollectorId {
            self.id
        }

        fn collect(&mut self, _now: Instant, snap: &mut Snapshot) -> Result<Outcome> {
            if self.fail {
                return Err(Error::system("deliberate failure"));
            }
            match self.id {
                CollectorId::Cpu => snap.cpu.active = 0.25,
                CollectorId::Network => snap.network.rx_bps = 4096,
                _ => {}
            }
            Ok(Outcome::Fresh)
        }
    }

    fn fake(id: CollectorId, fail: bool) -> Box<dyn Collector> {
        Box::new(Fake { id, fail })
    }

    fn scripted(delay: Duration, celsius: f32) -> Option<SlowThread> {
        SlowThread::spawn(Duration::from_millis(20), move || Scripted {
            delay,
            celsius,
        })
    }

    #[test]
    fn a_failing_collector_does_not_sink_the_tick() {
        let collectors = vec![
            fake(CollectorId::Cpu, false),
            fake(CollectorId::Memory, true),
            fake(CollectorId::Network, false),
        ];
        let mut sampler =
            Sampler::assemble(SocInfo::default(), collectors, None, Duration::from_secs(2));
        let snap = sampler.tick().expect("tick never fails");
        assert!((snap.cpu.active - 0.25).abs() < f32::EPSILON);
        assert_eq!(snap.network.rx_bps, 4096);
        assert!(snap.health.is_fresh(CollectorId::Cpu));
        assert!(snap.health.is_fresh(CollectorId::Network));
        assert_eq!(
            snap.health.get(CollectorId::Memory),
            &Health::Failed(String::from("system: deliberate failure"))
        );
        assert!(matches!(
            snap.health.get(CollectorId::Sensors),
            Health::Failed(_)
        ));
        assert!(snap.primed);
    }

    #[test]
    fn a_slow_sensor_source_never_delays_cheap_ticks() {
        let mut sampler = Sampler::assemble(
            SocInfo::default(),
            vec![fake(CollectorId::Cpu, false)],
            scripted(Duration::from_millis(200), 47.0),
            Duration::from_millis(20),
        );
        let _ = sampler.tick();
        let mut worst = Duration::ZERO;
        let mut merged = false;
        for _ in 0..30 {
            let started = Instant::now();
            let snap = sampler.tick().expect("tick");
            worst = worst.max(started.elapsed());
            merged |= snap.sampled.sensors;
            thread::sleep(Duration::from_millis(15));
        }
        assert!(worst < Duration::from_millis(50), "worst tick {worst:?}");
        assert!(merged, "frames still merge");
        assert_eq!(sampler.snap.sensors.gpu_c, Some(47.0));
    }

    #[test]
    fn settled_waits_for_the_first_sensor_frame() {
        let mut sampler = Sampler::assemble(
            SocInfo::default(),
            vec![fake(CollectorId::Cpu, false)],
            scripted(Duration::from_millis(250), 44.0),
            Duration::from_millis(20),
        );
        let first = sampler.tick().expect("tick");
        assert!(!first.settled, "the sensor thread has not reported yet");
        let started = Instant::now();
        let mut settled = false;
        while !settled && started.elapsed() < Duration::from_secs(3) {
            thread::sleep(Duration::from_millis(20));
            settled = sampler.tick().expect("tick").settled;
        }
        assert!(settled, "settles once the first frame lands");
    }

    #[test]
    fn low_power_sensor_sweeps_are_not_reported_stale_between_frames() {
        let mut sampler = Sampler::assemble(
            SocInfo::default(),
            vec![fake(CollectorId::Cpu, false)],
            None,
            Duration::from_secs(2),
        );
        let t0 = Instant::now();
        sampler.slow_last_ok = [Some(t0); 3];
        sampler.snap.health.set(CollectorId::Sensors, Health::Ok);
        sampler.set_low_power(true);
        let between = t0 + Duration::from_secs(8);
        sampler.age_slow(between);
        assert!(
            !matches!(
                sampler.snap.health.get(CollectorId::Sensors),
                Health::Stale(_)
            ),
            "8 s is inside one 10 s low-power sweep"
        );
        sampler.set_low_power(false);
        sampler.age_slow(between);
        assert!(matches!(
            sampler.snap.health.get(CollectorId::Sensors),
            Health::Stale(_)
        ));
    }

    #[test]
    fn a_mac_with_no_slow_sources_is_settled_at_once() {
        let mut sampler = Sampler::assemble(
            SocInfo::default(),
            vec![fake(CollectorId::Cpu, false)],
            None,
            Duration::from_secs(2),
        );
        assert!(sampler.tick().expect("tick").settled);
    }

    fn with_energy(rails: RailReadings) -> Sampler {
        let feed = FreqFeed::fixed(FreqFrame {
            energy: Some(EnergyFrame {
                at: Instant::now(),
                rails,
            }),
            ..FreqFrame::default()
        });
        Sampler::assemble(
            SocInfo::default(),
            vec![fake(CollectorId::Cpu, false)],
            None,
            Duration::from_secs(2),
        )
        .with_freq(feed)
    }

    #[test]
    fn energy_rails_fill_power_on_a_model_without_smc_rails() {
        let mut sampler = with_energy(RailReadings {
            cpu: Some(Reading::measured(5.0)),
            gpu: Some(Reading::measured(1.5)),
            ..RailReadings::default()
        });
        let snap = sampler.tick().expect("tick");
        assert_eq!(snap.power.cpu, Some(Reading::measured(5.0)));
        assert_eq!(snap.power.gpu, Some(Reading::measured(1.5)));
        assert_eq!(snap.power.ane, None, "absent, not 0 W");
        assert_eq!(snap.cpu.watts, Some(5.0), "CPU watts follow the fallback");
    }

    #[test]
    fn smc_cpu_rails_stay_primary_over_energy_every_tick() {
        let mut sampler = with_energy(RailReadings {
            cpu: Some(Reading::measured(5.0)),
            ..RailReadings::default()
        });
        sampler.smc_power = PowerSnapshot {
            cpu: Some(Reading::measured(7.25)),
            ..PowerSnapshot::default()
        };
        for _ in 0..3 {
            let snap = sampler.tick().expect("tick");
            assert_eq!(snap.power.cpu, Some(Reading::measured(7.25)));
        }
    }

    #[test]
    fn energy_overlay_does_not_stick_after_the_feed_goes_quiet() {
        // Regression: the overlay used to be applied onto the previous
        // tick's power, so a rail that vanished from the feed kept its
        // old value. Each tick now starts from the SMC figures.
        let mut sampler = with_energy(RailReadings {
            dram: Some(Reading::measured(0.7)),
            ..RailReadings::default()
        });
        assert!(sampler.tick().expect("tick").power.dram.is_some());
        sampler.freq = FreqFeed::default();
        assert_eq!(sampler.tick().expect("tick").power.dram, None);
    }

    #[test]
    fn samplers_do_not_block_each_other() {
        // Regression (wave-1 P2-f): production held a process-wide gate for
        // the Sampler's life, so a second one blocked forever.
        let a = Sampler::assemble(SocInfo::default(), Vec::new(), None, Duration::from_secs(2));
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let b = Sampler::assemble(SocInfo::default(), Vec::new(), None, Duration::from_secs(2));
            let _ = tx.send(b.snap.primed);
        });
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_ok());
        drop(a);
    }

    #[test]
    fn pause_reaches_the_sensor_thread() {
        let sampler = Sampler::assemble(
            SocInfo::default(),
            Vec::new(),
            scripted(Duration::ZERO, 40.0),
            Duration::from_millis(5),
        );
        sampler.set_paused(true);
        thread::sleep(Duration::from_millis(30));
        let slow = sampler.slow.as_ref().expect("thread");
        let _ = slow.latest();
        thread::sleep(Duration::from_millis(60));
        assert!(slow.latest().is_none(), "no frames while paused");
        sampler.set_paused(false);
        assert!(slow.wait(Duration::from_secs(2)).is_some(), "frames resume");
    }

    #[test]
    fn priming_ages_the_baseline_before_the_first_tick() {
        let started = Instant::now();
        let mut sampler = Sampler::assemble(
            SocInfo::default(),
            vec![fake(CollectorId::Cpu, false)],
            scripted(Duration::ZERO, 41.0),
            Duration::from_millis(20),
        );
        let first = sampler.tick().expect("tick");
        assert!(started.elapsed() >= PRIME_WINDOW);
        assert!(started.elapsed() < PRIME_WINDOW * 3);
        assert!(first.primed);
        assert!(first.sampled.sensors, "first frame caught during priming");
        assert_eq!(first.gpu, None);
        assert_eq!(first.sensors.gpu_c, Some(41.0));
    }

    #[test]
    fn stale_or_failed_slow_values_keep_the_last_good_one() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(2);
        let mut slot = 1;
        let mut last_ok = None;
        let failed = Health::Failed(String::from("no temperature readings"));
        assert_eq!(
            merge_part(&mut slot, 2, failed.clone(), &mut last_ok, t0),
            failed
        );
        assert_eq!(slot, 1);
        assert_eq!(
            merge_part(&mut slot, 3, Health::Ok, &mut last_ok, t0),
            Health::Ok
        );
        assert_eq!(
            merge_part(&mut slot, 4, failed, &mut last_ok, t1),
            Health::Stale(Duration::from_secs(2))
        );
        assert_eq!(slot, 3, "last good value kept");
        assert_eq!(
            merge_part(&mut slot, 0, Health::Absent, &mut last_ok, t1),
            Health::Absent
        );
    }

    #[test]
    fn a_silent_sensor_thread_turns_stale() {
        let mut sampler = Sampler::assemble(
            SocInfo::default(),
            Vec::new(),
            None,
            Duration::from_millis(10),
        );
        let t0 = Instant::now();
        sampler.merge_slow(
            Some(SensorFrame {
                at: t0,
                ..crate::sensors::testing::frame(40.0)
            }),
            t0,
        );
        assert!(sampler.snap.health.is_fresh(CollectorId::Sensors));
        sampler.age_slow(t0 + Duration::from_millis(20));
        assert!(sampler.snap.health.is_fresh(CollectorId::Sensors));
        sampler.age_slow(t0 + Duration::from_millis(50));
        assert_eq!(
            sampler.snap.health.get(CollectorId::Sensors),
            &Health::Stale(Duration::from_millis(50))
        );
        assert_eq!(sampler.snap.sensors.gpu_c, Some(40.0));
    }

    #[test]
    fn cpu_watts_follow_the_power_rails_and_are_never_zero_for_missing() {
        use siltop_core::{PowerSnapshot, Reading};
        let mut sampler =
            Sampler::assemble(SocInfo::default(), Vec::new(), None, Duration::from_secs(2));
        let t0 = Instant::now();
        let power = PowerSnapshot {
            system: Some(Reading::measured(9.5)),
            cpu: Some(Reading::measured(1.25)),
            ..PowerSnapshot::default()
        };
        sampler.merge_slow(
            Some(SensorFrame {
                at: t0,
                power,
                ..crate::sensors::testing::frame(40.0)
            }),
            t0,
        );
        let snap = sampler.tick().expect("tick");
        assert_eq!(snap.power, power);
        assert_eq!(snap.cpu.watts, Some(1.25));
        let later = t0 + Duration::from_secs(2);
        sampler.merge_slow(
            Some(SensorFrame {
                at: later,
                ..crate::sensors::testing::frame(40.0)
            }),
            later,
        );
        let snap = sampler.tick().expect("tick");
        assert_eq!(snap.cpu.watts, None, "no rails: absent, not 0 W");
        assert_eq!(snap.power.system, None);
    }

    #[test]
    fn sampler_ticks() {
        let _io = io_gate();
        let mut sampler = Sampler::new().expect("sampler");
        let snap = sampler.tick().expect("tick");
        assert!(!snap.soc.name.is_empty());
    }

    #[test]
    fn fills_gpu_temp_from_sensors() {
        let mut snap = Snapshot::empty();
        snap.gpu = Some(siltop_core::GpuSnapshot::default());
        snap.sensors.gpu_c = Some(47.0);
        fill_missing_temps(&mut snap);
        assert_eq!(snap.gpu.and_then(|g| g.temp_c), Some(47.0));
    }

    #[test]
    fn keeps_existing_gpu_temp() {
        let mut snap = Snapshot::empty();
        snap.gpu = Some(siltop_core::GpuSnapshot {
            temp_c: Some(51.0),
            ..siltop_core::GpuSnapshot::default()
        });
        snap.sensors.gpu_c = Some(47.0);
        fill_missing_temps(&mut snap);
        assert_eq!(snap.gpu.and_then(|g| g.temp_c), Some(51.0));
    }

    #[test]
    fn gpu_temp_falls_back_to_a_gpu_named_reading() {
        let mut snap = Snapshot::empty();
        snap.gpu = Some(siltop_core::GpuSnapshot::default());
        snap.sensors.readings = vec![
            siltop_core::TempReading {
                name: String::from("nand"),
                celsius: 38.0,
            },
            siltop_core::TempReading {
                name: String::from("GPU MTR Temp Sensor0"),
                celsius: 44.5,
            },
        ];
        fill_missing_temps(&mut snap);
        assert_eq!(snap.gpu.and_then(|g| g.temp_c), Some(44.5));
    }

    #[test]
    fn cpu_package_is_never_passed_off_as_gpu_temp() {
        let mut snap = Snapshot::empty();
        snap.gpu = Some(siltop_core::GpuSnapshot::default());
        snap.sensors.cpu_c = Some(61.0);
        snap.sensors.hotspot_c = Some(70.0);
        fill_missing_temps(&mut snap);
        // No zone or reading says GPU: honest absence beats a fake number.
        assert_eq!(snap.gpu.and_then(|g| g.temp_c), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_first_snapshot_is_primed_and_prompt() {
        let _io = io_gate();
        let started = Instant::now();
        let mut sampler = Sampler::new().expect("sampler");
        let first = sampler.tick().expect("tick1");
        let latency = started.elapsed();
        assert!(first.primed, "no 0% frame at start-up");
        // Typically 120-160 ms; the bound leaves room for a loaded CI host.
        assert!(latency < Duration::from_secs(1), "{latency:?}");
        assert!(first.sampled.procs);
        assert!(
            first
                .processes
                .iter()
                .any(|p| p.cpu.is_some_and(|c| c > 0.0)),
            "process CPU measured over the priming window"
        );
        assert_ne!(first.soc.name, "unknown");
        assert!(first.memory.total_bytes > 0);
        assert!(!first.cpu.cores.is_empty());
        assert!((0.0..=1.0).contains(&first.cpu.active));
        assert!(first.health.is_fresh(CollectorId::Cpu));
        let second = sampler.tick().expect("tick2");
        assert!(!second.sampled.procs, "process table reused inside 1 s");
        assert_eq!(first.processes.len(), second.processes.len());
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "manual: cargo test -p siltop-metrics -- --ignored --nocapture self_cpu_budget"]
    fn self_cpu_budget() {
        let _io = io_gate();
        let mut sampler = Sampler::new().expect("sampler");
        let start = Instant::now();
        let cpu0 = process_cpu_secs();
        thread::sleep(Duration::from_secs(2));
        for _ in 0..32 {
            let _ = sampler.tick();
            thread::sleep(Duration::from_millis(250));
        }
        let wall = start.elapsed().as_secs_f64().max(0.001);
        let cpu = (process_cpu_secs() - cpu0) / wall;
        eprintln!("self  {:.1}%  (250ms, {wall:.0}s window)", cpu * 100.0);
        assert!(cpu < 0.02, "self CPU {cpu:.3} exceeded 2%");
    }

    #[cfg(target_os = "macos")]
    fn process_cpu_secs() -> f64 {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        let rc = unsafe {
            // SAFETY: `usage` is a writable rusage out-parameter.
            libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr())
        };
        if rc != 0 {
            return 0.0;
        }
        let usage = unsafe {
            // SAFETY: getrusage filled the struct on success.
            usage.assume_init()
        };
        let user = usage.ru_utime.tv_sec as f64 + f64::from(usage.ru_utime.tv_usec) / 1_000_000.0;
        let sys = usage.ru_stime.tv_sec as f64 + f64::from(usage.ru_stime.tv_usec) / 1_000_000.0;
        user + sys
    }
}
