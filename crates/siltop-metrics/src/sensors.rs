//! The slow sensor thread.
//!
//! SMC temperatures and fans, the HID fallback and the thermal state are
//! sampled every [`SENSOR_PERIOD`] on their own thread. The cheap tick only
//! merges the newest [`SensorFrame`] and never waits on `IOKit`. The SMC key
//! list is enumerated once when the thread starts.

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use siltop_core::{FanSnapshot, Health, PowerSnapshot, SENSOR_PERIOD, SensorsSnapshot, Thermal};

use crate::cadence::{LOW_POWER_SENSORS, PowerMode, stretched};
use crate::hid::HidSensors;
use crate::pacer::{Background, Pacer, Wake};
use crate::zones::{self, Plausibility, Tagged, TempZone};

/// One pass of the slow thread.
#[derive(Debug, Clone)]
pub(crate) struct SensorFrame {
    pub(crate) at: Instant,
    pub(crate) sensors: SensorsSnapshot,
    pub(crate) sensors_health: Health,
    pub(crate) fans: FanSnapshot,
    pub(crate) fans_health: Health,
    pub(crate) thermal: Thermal,
    pub(crate) thermal_health: Health,
    /// SMC system and CPU watts; absent parts are `None`.
    pub(crate) power: PowerSnapshot,
}

/// Something the slow thread samples once per period.
pub(crate) trait SlowSource {
    fn sample(&mut self) -> SensorFrame;
}

/// The newest unread frame. Publishing overwrites it, so a reader that
/// falls behind (a paused worker) never lets frames pile up.
#[derive(Default)]
struct Slot {
    frame: Mutex<Option<SensorFrame>>,
    ready: Condvar,
}

impl Slot {
    fn publish(&self, frame: SensorFrame) {
        let mut slot = self.frame.lock().unwrap_or_else(PoisonError::into_inner);
        *slot = Some(frame);
        self.ready.notify_all();
    }
}

/// Owns the slow thread; stops and joins it on drop.
pub(crate) struct SlowThread {
    slot: Arc<Slot>,
    thread: Background,
}

impl SlowThread {
    /// `make` runs on the new thread, so the source never crosses threads.
    #[cfg(test)]
    pub(crate) fn spawn<S, F>(period: Duration, make: F) -> Option<Self>
    where
        S: SlowSource,
        F: FnOnce() -> S + Send + 'static,
    {
        Self::spawn_paced(period, PowerMode::default(), make)
    }

    /// Starts the thread at `period`, stretched to
    /// [`LOW_POWER_SENSORS`] while `mode` is low.
    pub(crate) fn spawn_paced<S, F>(period: Duration, mode: PowerMode, make: F) -> Option<Self>
    where
        S: SlowSource,
        F: FnOnce() -> S + Send + 'static,
    {
        let slot = Arc::new(Slot::default());
        let publisher = Arc::clone(&slot);
        let thread = Background::spawn("siltop-sensors", move |pacer| {
            run(make(), || sensor_period(period, &mode), &publisher, &pacer);
        })?;
        Some(Self { slot, thread })
    }

    /// Parks the thread (no SMC or HID reads) while the app is frozen.
    pub(crate) fn set_paused(&self, paused: bool) {
        self.thread.set_paused(paused);
    }

    /// The newest frame published since the last call. Never waits: if the
    /// slow thread is mid-publish, the frame is picked up next tick.
    pub(crate) fn latest(&self) -> Option<SensorFrame> {
        self.slot.frame.try_lock().ok()?.take()
    }

    /// Waits up to `timeout` for a frame.
    pub(crate) fn wait(&self, timeout: Duration) -> Option<SensorFrame> {
        let guard = self
            .slot
            .frame
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (mut guard, _) = self
            .slot
            .ready
            .wait_timeout_while(guard, timeout, |frame| frame.is_none())
            .unwrap_or_else(PoisonError::into_inner);
        guard.take()
    }
}

/// The sensor sweep period under `mode`: never faster than snapshots are
/// taken (the background agent's 5 s, a 10 s interval the user chose).
pub(crate) fn sensor_period(normal: Duration, mode: &PowerMode) -> Duration {
    stretched(normal, LOW_POWER_SENSORS, mode.low()).max(mode.interval())
}

fn run<S: SlowSource>(mut source: S, period: impl Fn() -> Duration, slot: &Slot, pacer: &Pacer) {
    let mut next = Instant::now();
    loop {
        slot.publish(source.sample());
        let period = period();
        next += period;
        let now = Instant::now();
        if next <= now {
            next = now + period;
        }
        match pacer.wait(next - now) {
            Wake::Due => {}
            Wake::Resumed => next = Instant::now(),
            Wake::Stop => return,
        }
    }
}

/// How often an incomplete SMC discovery is retried, and how many
/// discoveries run at most (each walks every key: ~3,600 calls).
#[cfg(target_os = "macos")]
const REDISCOVER_EVERY: Duration = Duration::from_secs(30);
#[cfg(target_os = "macos")]
const DISCOVERIES: u8 = 5;

/// SMC temperatures and fans, with HID for zones the SMC lacks.
pub(crate) struct SensorHub {
    expected: Vec<TempZone>,
    hid: HidSensors,
    filter: Plausibility,
    ever_read: bool,
    #[cfg(target_os = "macos")]
    smc: Option<macos::Smc>,
    /// When SMC discovery last ran, and how many times it has.
    #[cfg(target_os = "macos")]
    discovered: (Option<Instant>, u8),
}

impl SensorHub {
    /// Opens the SMC and enumerates its keys. Runs on the slow thread.
    pub(crate) fn open(e_cores: u8, p_cores: u8, s_cores: u8) -> Self {
        Self {
            expected: zones::expected_zones(e_cores, p_cores, s_cores),
            hid: HidSensors::new(),
            filter: Plausibility::default(),
            ever_read: false,
            #[cfg(target_os = "macos")]
            smc: macos::Smc::open(),
            #[cfg(target_os = "macos")]
            discovered: (Some(Instant::now()), 1),
        }
    }

    /// Opens the SMC again when it failed to open or a call failed during
    /// discovery (another SMC poller made it time out), at most every
    /// [`REDISCOVER_EVERY`] and [`DISCOVERIES`] times in all: one timeout
    /// at launch must not hide the fans, temperatures or CPU rails for the
    /// whole session.
    #[cfg(target_os = "macos")]
    fn rediscover(&mut self, now: Instant) {
        let (at, runs) = self.discovered;
        if self.smc.as_ref().is_some_and(macos::Smc::complete)
            || runs >= DISCOVERIES
            || !crate::cadence::due(at, now, REDISCOVER_EVERY)
        {
            return;
        }
        self.discovered = (Some(now), runs + 1);
        // A retry that hit contention again never replaces a working client.
        if let Some(smc) = macos::Smc::open().filter(|s| s.complete() || self.smc.is_none()) {
            self.smc = Some(smc);
        }
    }

    pub(crate) fn period() -> Duration {
        SENSOR_PERIOD
    }

    fn temperatures(&mut self, now: Instant) -> Vec<Tagged> {
        #[cfg(target_os = "macos")]
        let mut tagged = self
            .smc
            .as_mut()
            .map(|smc| smc.temperatures(&mut self.filter))
            .unwrap_or_default();
        #[cfg(not(target_os = "macos"))]
        let mut tagged: Vec<Tagged> = Vec::new();
        let wanted = zones::missing_zones(&tagged, &self.expected);
        tagged.extend(self.hid.sample(&wanted, now, &mut self.filter));
        tagged
    }

    fn power(&mut self) -> PowerSnapshot {
        #[cfg(target_os = "macos")]
        if let Some(smc) = self.smc.as_mut() {
            return smc.power();
        }
        PowerSnapshot::default()
    }

    fn fans(&mut self) -> (FanSnapshot, Health) {
        #[cfg(target_os = "macos")]
        if let Some(smc) = self.smc.as_mut() {
            return smc.fans();
        }
        (FanSnapshot::default(), Health::Absent)
    }
}

impl SlowSource for SensorHub {
    fn sample(&mut self) -> SensorFrame {
        let now = Instant::now();
        #[cfg(target_os = "macos")]
        self.rediscover(now);
        let tagged = self.temperatures(now);
        let sensors_health = if !tagged.is_empty() {
            self.ever_read = true;
            Health::Ok
        } else if self.ever_read {
            Health::Failed(String::from("no temperature readings"))
        } else {
            Health::Absent
        };
        let (fans, fans_health) = self.fans();
        let (thermal, thermal_health) = if cfg!(target_os = "macos") {
            (crate::thermal::sample(), Health::Ok)
        } else {
            (Thermal::Nominal, Health::Absent)
        };
        SensorFrame {
            at: now,
            sensors: zones::assemble(&tagged),
            sensors_health,
            fans,
            fans_health,
            thermal,
            thermal_health,
            power: self.power(),
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use siltop_core::{FanSnapshot, Health, PowerSnapshot};

    use crate::fan::Fans;
    use crate::power::Rails;
    use crate::smc::{self, Client};
    use crate::smc_keys::{self, Chip};
    use crate::sys;
    use crate::zones::{Plausibility, SensorId, Tagged, TempZone};

    struct TempKey {
        key: u32,
        name: String,
        zone: TempZone,
    }

    pub(super) struct Smc {
        client: Client,
        temps: Vec<TempKey>,
        fans: Fans,
        power: Rails,
        complete: bool,
    }

    impl Smc {
        pub(super) fn open() -> Option<Self> {
            let mut client = Client::open()?;
            let model = sys::sysctl_string(c"hw.model").unwrap_or_default();
            let brand = sys::sysctl_string(c"machdep.cpu.brand_string").unwrap_or_default();
            let chip = smc_keys::chip_for(&model, &brand);
            let all = client.all_keys();
            let temps = temperature_keys(&mut client, &all, chip);
            let fans = Fans::discover(&mut client, &all);
            let power = Rails::new(&model, |key| client.info(key).is_some());
            Some(Self {
                complete: !client.had_transient_failure(),
                client,
                temps,
                fans,
                power,
            })
        }

        /// Discovery saw no call fail: the key set is final.
        pub(super) fn complete(&self) -> bool {
            self.complete
        }

        pub(super) fn temperatures(&mut self, filter: &mut Plausibility) -> Vec<Tagged> {
            let client = &mut self.client;
            self.temps
                .iter()
                .filter_map(|t| {
                    let (info, bytes) = client.read(t.key)?;
                    let raw = smc::decode_temp(info, &bytes)?;
                    let celsius = filter.admit(SensorId::Smc(t.key), raw)?;
                    Some(Tagged::new(t.zone, t.name.clone(), celsius))
                })
                .collect()
        }

        pub(super) fn power(&mut self) -> PowerSnapshot {
            let client = &mut self.client;
            self.power.sample(|key| {
                let (info, bytes) = client.read(key)?;
                smc::decode_numeric(info.data_type, &bytes, smc::NATIVE_INTS)
            })
        }

        pub(super) fn fans(&mut self) -> (FanSnapshot, Health) {
            if self.fans.is_empty() {
                return (FanSnapshot::default(), Health::Absent);
            }
            match self.fans.sample(&mut self.client) {
                Some(snap) => (snap, Health::Ok),
                None => (
                    FanSnapshot::default(),
                    Health::Failed(String::from("fan speeds unreadable")),
                ),
            }
        }
    }

    /// The display set: temperature-typed `T*` keys this chip's table (or
    /// the prefix fallback) gives a zone.
    fn temperature_keys(client: &mut Client, all: &[u32], chip: Option<&Chip>) -> Vec<TempKey> {
        all.iter()
            .filter(|key| key.to_be_bytes()[0] == b'T')
            .filter_map(|&key| {
                let name = smc::key_name(key);
                let zone = smc_keys::classify(chip, &name)?;
                client
                    .info(key)
                    .is_some_and(smc::KeyInfo::is_temperature)
                    .then_some(TempKey { key, name, zone })
            })
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::{SensorFrame, SlowSource};
    use crate::zones::{self, Tagged, TempZone};
    use siltop_core::{FanSnapshot, Health, PowerSnapshot, Thermal};
    use std::thread;
    use std::time::{Duration, Instant};

    /// A frame whose only reading is a GPU temperature.
    pub(crate) fn frame(gpu_c: f32) -> SensorFrame {
        SensorFrame {
            at: Instant::now(),
            sensors: zones::assemble(&[Tagged::new(TempZone::Gpu, "Tg08", gpu_c)]),
            sensors_health: Health::Ok,
            fans: FanSnapshot::default(),
            fans_health: Health::Absent,
            thermal: Thermal::Nominal,
            thermal_health: Health::Ok,
            power: PowerSnapshot::default(),
        }
    }

    /// A source that takes `delay` per sample.
    pub(crate) struct Scripted {
        pub(crate) delay: Duration,
        pub(crate) celsius: f32,
    }

    impl SlowSource for Scripted {
        fn sample(&mut self) -> SensorFrame {
            thread::sleep(self.delay);
            frame(self.celsius)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::testing::Scripted;
    use super::*;
    use std::thread;

    #[test]
    fn frames_arrive_and_the_thread_stops_on_drop() {
        let slow = SlowThread::spawn(Duration::from_millis(10), || Scripted {
            delay: Duration::ZERO,
            celsius: 40.0,
        })
        .expect("spawn");
        let frame = slow.wait(Duration::from_secs(2)).expect("frame");
        assert_eq!(frame.sensors.gpu_c, Some(40.0));
        let started = Instant::now();
        drop(slow);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "joined promptly"
        );
    }

    #[test]
    fn low_power_stretches_the_sensor_sweep_to_ten_seconds() {
        let mode = PowerMode::default();
        assert_eq!(sensor_period(SENSOR_PERIOD, &mode), SENSOR_PERIOD);
        mode.set(true);
        assert_eq!(sensor_period(SENSOR_PERIOD, &mode), LOW_POWER_SENSORS);
    }

    #[test]
    fn a_slow_snapshot_interval_slows_the_sensor_sweep_to_match() {
        let mode = PowerMode::default();
        mode.set_interval(Duration::from_millis(500));
        assert_eq!(sensor_period(SENSOR_PERIOD, &mode), SENSOR_PERIOD);
        mode.set_interval(Duration::from_secs(5));
        assert_eq!(sensor_period(SENSOR_PERIOD, &mode), Duration::from_secs(5));
        mode.set(true);
        assert_eq!(sensor_period(SENSOR_PERIOD, &mode), LOW_POWER_SENSORS);
    }

    #[test]
    fn a_low_power_thread_reads_its_period_every_sweep() {
        let mode = PowerMode::default();
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = std::sync::Arc::clone(&count);
        let period = Duration::from_millis(5);
        let slow =
            SlowThread::spawn_paced(period, mode.clone(), move || Counting(seen)).expect("spawn");
        thread::sleep(Duration::from_millis(60));
        mode.set(true);
        // One sweep may already be scheduled at the short period.
        thread::sleep(Duration::from_millis(30));
        let parked = count.load(std::sync::atomic::Ordering::SeqCst);
        thread::sleep(Duration::from_millis(100));
        assert!(
            count.load(std::sync::atomic::Ordering::SeqCst) <= parked + 1,
            "low power stopped the 5 ms sweeps"
        );
        drop(slow);
    }

    struct Counting(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl SlowSource for Counting {
        fn sample(&mut self) -> SensorFrame {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Scripted {
                delay: Duration::ZERO,
                celsius: 40.0,
            }
            .sample()
        }
    }

    #[test]
    fn an_unread_frame_is_replaced_not_queued() {
        let slow = SlowThread::spawn(Duration::from_millis(5), || Scripted {
            delay: Duration::ZERO,
            celsius: 40.0,
        })
        .expect("spawn");
        thread::sleep(Duration::from_millis(60));
        // Park the thread so no new frame lands between the two reads; under
        // a loaded test run a 5 ms period could otherwise refill the slot.
        slow.set_paused(true);
        thread::sleep(Duration::from_millis(20));
        assert!(slow.latest().is_some());
        assert!(slow.latest().is_none(), "one slot, drained by the read");
    }

    #[test]
    fn latest_never_blocks_on_a_slow_source() {
        let slow = SlowThread::spawn(Duration::from_millis(10), || Scripted {
            delay: Duration::from_millis(200),
            celsius: 40.0,
        })
        .expect("spawn");
        let started = Instant::now();
        for _ in 0..20 {
            let _ = slow.latest();
        }
        assert!(started.elapsed() < Duration::from_millis(50));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "manual: cargo test -p siltop-metrics --release -- --ignored --nocapture slow_sample_cost"]
    fn slow_sample_cost() {
        let _io = crate::sampler::io_gate();
        let mut hub = SensorHub::open(0, 12, 6);
        let _ = hub.sample();
        let rounds = 20;
        let wall = Instant::now();
        let cpu0 = crate::sys::thread_cpu_secs();
        for _ in 0..rounds {
            let _ = hub.sample();
        }
        let cpu_ms = (crate::sys::thread_cpu_secs() - cpu0) / f64::from(rounds) * 1e3;
        let wall_ms = wall.elapsed().as_secs_f64() / f64::from(rounds) * 1e3;
        let share = cpu_ms / SENSOR_PERIOD.as_secs_f64() / 10.0;
        eprintln!("slow sample {cpu_ms:.2} ms CPU, {wall_ms:.2} ms wall, {share:.3}% at 2 s");
        assert!(share <= 0.2, "slow thread {share:.3}% CPU");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_hub_reads_the_machine() {
        let _io = crate::sampler::io_gate();
        let started = Instant::now();
        let mut hub = SensorHub::open(0, 12, 6);
        let opened = started.elapsed();
        let frame = hub.sample();
        eprintln!(
            "hub open {opened:?}, first sample {:?}: {:?}",
            started.elapsed().saturating_sub(opened),
            frame.sensors
        );
        if let Some(c) = frame.sensors.hotspot_c {
            assert!(zones::PLAUSIBLE_C.contains(&c), "{c}");
        }
        if crate::sys::sysctl_string(c"hw.model").as_deref() == Some("Mac17,8") {
            assert!(frame.sensors.gpu_c.is_some(), "GPU °C on M5 Pro");
            assert!(frame.sensors.s_c.is_some() && frame.sensors.p_c.is_some());
            assert!(frame.sensors.e_c.is_none());
            let hot = frame.sensors.hotspot_c.expect("hotspot");
            assert!((hot - 51.82).abs() > 0.01, "not the tcal constant");
            assert!(frame.fans_health == Health::Ok, "{:?}", frame.fans_health);
            assert_eq!(frame.fans.fans.len(), 2);
            assert!(frame.fans.fans.iter().all(|f| f.max_rpm > 5000));
            assert_eq!(hub.hid.kept(), 0, "SMC covers every zone; HID untouched");
        }
    }
}
