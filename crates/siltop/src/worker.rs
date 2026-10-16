use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use siltop_core::{Error, LowPower, Result, Snapshot};
use siltop_metrics::{Needs, Sampler, tick_interval};

use crate::msg::AppMsg;

#[derive(Debug, Clone, Copy)]
pub enum Cmd {
    Interval(Duration),
    Paused(bool),
    /// What the visible views read; the sampler parks the rest.
    Needs(Needs),
    /// The battery saver setting: follow the battery, or force it.
    LowPower(LowPower),
    Quit,
}

pub trait Tick {
    fn tick(&mut self) -> Result<Snapshot>;

    /// Parks any background sampling while frozen.
    fn set_paused(&self, _paused: bool) {}

    /// The snapshot interval, so background sampling can match it.
    fn set_interval(&self, _interval: Duration) {}

    /// Low-power cadences for the collectors (on battery).
    fn set_low_power(&self, _low: bool) {}

    /// What the visible views read, so costly sources nobody sees park.
    fn set_needs(&self, _needs: Needs) {}

    /// Whether `snap` says this Mac runs from its battery. A canned
    /// session's battery is not this Mac's, so it may say no.
    fn on_battery(&self, snap: &Snapshot) -> bool {
        siltop_metrics::on_battery(snap)
    }
}

/// So a sampler chosen at runtime (`--demo` or this Mac) can run here too.
impl<T: Tick + ?Sized> Tick for Box<T> {
    fn tick(&mut self) -> Result<Snapshot> {
        (**self).tick()
    }

    fn set_paused(&self, paused: bool) {
        (**self).set_paused(paused);
    }

    fn set_interval(&self, interval: Duration) {
        (**self).set_interval(interval);
    }

    fn set_low_power(&self, low: bool) {
        (**self).set_low_power(low);
    }

    fn set_needs(&self, needs: Needs) {
        (**self).set_needs(needs);
    }

    fn on_battery(&self, snap: &Snapshot) -> bool {
        (**self).on_battery(snap)
    }
}

impl Tick for Sampler {
    fn tick(&mut self) -> Result<Snapshot> {
        Sampler::tick(self)
    }

    fn set_paused(&self, paused: bool) {
        Sampler::set_paused(self, paused);
    }

    fn set_interval(&self, interval: Duration) {
        Sampler::set_interval(self, interval);
    }

    fn set_low_power(&self, low: bool) {
        Sampler::set_low_power(self, low);
    }

    fn set_needs(&self, needs: Needs) {
        Sampler::set_needs(self, needs);
    }
}

pub struct Handle {
    cmds: Sender<Cmd>,
    join: Option<JoinHandle<()>>,
}

impl Handle {
    pub fn send(&self, cmd: Cmd) {
        let _ = self.cmds.send(cmd);
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = self.cmds.send(Cmd::Quit);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

pub fn spawn(interval: Duration, low_power: LowPower, out: Sender<AppMsg>) -> Result<Handle> {
    spawn_power(interval, low_power, out, Sampler::new)
}

/// [`spawn_power`] with the battery saver on auto.
pub fn spawn_with<S, F>(interval: Duration, out: Sender<AppMsg>, make: F) -> Result<Handle>
where
    S: Tick,
    F: FnOnce() -> Result<S> + Send + 'static,
{
    spawn_power(interval, LowPower::Auto, out, make)
}

/// Starts the sampler thread on `make`'s sampler; `low_power` is the
/// battery saver setting it starts with, so the first tick already obeys it.
pub fn spawn_power<S, F>(
    interval: Duration,
    low_power: LowPower,
    out: Sender<AppMsg>,
    make: F,
) -> Result<Handle>
where
    S: Tick,
    F: FnOnce() -> Result<S> + Send + 'static,
{
    let (cmd_tx, cmd_rx) = channel();
    let join = thread::Builder::new()
        .name(String::from("siltop-sampler"))
        .spawn(move || {
            let start = PowerFollow::with_mode(interval, low_power);
            if let Err(why) = crate::tui::guard_thread(|| run(&cmd_rx, &out, start, make)) {
                let err = Error::system(format!("sampler stopped: {why}; restart siltop"));
                let _ = out.send(AppMsg::Stopped(err));
            }
        })
        .map_err(|err| Error::system(format!("spawn sampler thread: {err}")))?;
    Ok(Handle {
        cmds: cmd_tx,
        join: Some(join),
    })
}

fn run<S: Tick>(
    cmds: &Receiver<Cmd>,
    out: &Sender<AppMsg>,
    mut power: PowerFollow,
    make: impl FnOnce() -> Result<S>,
) {
    let mut sampler = match make() {
        Ok(sampler) => sampler,
        Err(err) => {
            let _ = out.send(AppMsg::Stopped(err));
            return;
        }
    };
    let mut cadence = Cadence::new(Instant::now(), power.effective());
    if power.low() {
        // Forced on: the app hears it before the first snapshot.
        if !follow_power(&power, &mut cadence, &sampler, out) {
            return;
        }
        cadence.resync(Instant::now());
    } else {
        sampler.set_interval(power.effective());
    }
    loop {
        if let Some(wait) = cadence.wait(Instant::now()) {
            match cmds.recv_timeout(wait) {
                Ok(cmd) => {
                    if !apply(cmd, &mut cadence, &mut power, cmds, &sampler, out) {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            continue;
        }
        let snapshot = sampler.tick();
        let battery = snapshot.as_ref().is_ok_and(|snap| sampler.on_battery(snap));
        if out.send(AppMsg::Snapshot(Box::new(snapshot))).is_err() {
            break;
        }
        cadence.advance(Instant::now());
        if power.observe(battery).is_some() && !follow_power(&power, &mut cadence, &sampler, out) {
            break;
        }
    }
}

/// Low-power mode flipped: the collectors, the tick and the app (which
/// labels the cadence it actually gets) all follow. False when the app
/// has gone.
fn follow_power(
    power: &PowerFollow,
    cadence: &mut Cadence,
    sampler: &impl Tick,
    out: &Sender<AppMsg>,
) -> bool {
    let low = power.low();
    sampler.set_low_power(low);
    set_interval(power.effective(), cadence, sampler);
    out.send(AppMsg::LowPower(low)).is_ok()
}

/// Follows the battery saver setting: on battery (auto) or always (on)
/// the tick stretches to [`siltop_metrics::LOW_POWER_TICK`] and the
/// collectors slow down; on the charger (auto) or always (off) everything
/// runs at the requested interval.
#[derive(Debug, Clone, Copy)]
struct PowerFollow {
    requested: Duration,
    mode: LowPower,
    battery: bool,
}

impl PowerFollow {
    #[cfg(test)]
    fn new(requested: Duration) -> Self {
        Self::with_mode(requested, LowPower::Auto)
    }

    fn with_mode(requested: Duration, mode: LowPower) -> Self {
        Self {
            requested,
            mode,
            battery: false,
        }
    }

    /// Whether the collectors run at their low-power cadences now.
    fn low(&self) -> bool {
        match self.mode {
            LowPower::Auto => self.battery,
            LowPower::On => true,
            LowPower::Off => false,
        }
    }

    /// The interval to run at now.
    fn effective(&self) -> Duration {
        tick_interval(self.requested, self.low())
    }

    /// The user asked for `interval`; returns what to run at.
    fn request(&mut self, interval: Duration) -> Duration {
        self.requested = interval;
        self.effective()
    }

    /// A snapshot said whether the Mac is on battery. Returns the new
    /// interval when low-power mode flipped, `None` when nothing changed.
    fn observe(&mut self, on_battery: bool) -> Option<Duration> {
        self.flip(|power| power.battery = on_battery)
    }

    /// The user changed the battery saver setting; returns the new interval
    /// when low-power mode flipped.
    fn set_mode(&mut self, mode: LowPower) -> Option<Duration> {
        self.flip(|power| power.mode = mode)
    }

    fn flip(&mut self, change: impl FnOnce(&mut Self)) -> Option<Duration> {
        let was = self.low();
        change(self);
        (self.low() != was).then(|| self.effective())
    }
}

/// Returns false when the worker should stop. A pause blocks here, so a
/// frozen app costs no sampler wakeups at all.
fn apply(
    cmd: Cmd,
    cadence: &mut Cadence,
    power: &mut PowerFollow,
    cmds: &Receiver<Cmd>,
    sampler: &impl Tick,
    out: &Sender<AppMsg>,
) -> bool {
    match cmd {
        Cmd::Interval(interval) => set_interval(power.request(interval), cadence, sampler),
        Cmd::Needs(needs) => sampler.set_needs(needs),
        Cmd::LowPower(mode) => {
            return power.set_mode(mode).is_none() || follow_power(power, cadence, sampler, out);
        }
        Cmd::Paused(false) => {}
        Cmd::Paused(true) => {
            sampler.set_paused(true);
            loop {
                match cmds.recv() {
                    Ok(Cmd::Paused(false)) => {
                        sampler.set_paused(false);
                        cadence.resync(Instant::now());
                        break;
                    }
                    Ok(Cmd::Interval(interval)) => {
                        set_interval(power.request(interval), cadence, sampler);
                    }
                    // Parked threads pick it up on thaw.
                    Ok(Cmd::Needs(needs)) => sampler.set_needs(needs),
                    Ok(Cmd::LowPower(mode)) => {
                        if power.set_mode(mode).is_some()
                            && !follow_power(power, cadence, sampler, out)
                        {
                            return false;
                        }
                    }
                    Ok(Cmd::Paused(true)) => {}
                    Ok(Cmd::Quit) | Err(_) => return false,
                }
            }
        }
        Cmd::Quit => return false,
    }
    true
}

fn set_interval(interval: Duration, cadence: &mut Cadence, sampler: &impl Tick) {
    cadence.set_interval(interval, Instant::now());
    sampler.set_interval(interval);
}

/// Fixed-rate schedule: the next slot is the previous slot plus one period,
/// so tick time never stretches the period. More than a whole period behind
/// (a stall, sleep/wake) resyncs to now and counts the slots skipped.
#[derive(Debug, Clone, Copy)]
pub struct Cadence {
    interval: Duration,
    next: Instant,
    skipped: u64,
}

impl Cadence {
    pub fn new(start: Instant, interval: Duration) -> Self {
        Self {
            interval: interval.max(Duration::from_millis(1)),
            next: start,
            skipped: 0,
        }
    }

    pub fn wait(&self, now: Instant) -> Option<Duration> {
        (now < self.next).then(|| self.next - now)
    }

    pub fn advance(&mut self, now: Instant) {
        self.next += self.interval;
        let Some(behind) = now.checked_duration_since(self.next) else {
            return;
        };
        if behind >= self.interval {
            let slots = behind.as_nanos() / self.interval.as_nanos();
            self.skipped += u64::try_from(slots).unwrap_or(u64::MAX);
            self.next = now;
        }
    }

    pub fn set_interval(&mut self, interval: Duration, now: Instant) {
        self.interval = interval.max(Duration::from_millis(1));
        self.next = now + self.interval;
    }

    pub fn resync(&mut self, now: Instant) {
        self.next = now;
    }

    #[cfg(test)]
    pub fn skipped(&self) -> u64 {
        self.skipped
    }
}

/// How often a [`Pacer`] wait looks at its stop flag.
pub const STOP_POLL: Duration = Duration::from_millis(100);

/// The `wait` of the headless loops (`record`, `--json-stream`, `agent
/// run`): fixed-rate on a [`Cadence`], so tick time never stretches the
/// period, and slept in slices of at most [`STOP_POLL`] so a stop request
/// ends it promptly instead of after a whole interval.
pub struct Pacer<'a> {
    cadence: Option<Cadence>,
    stop: &'a dyn Fn() -> bool,
}

impl<'a> Pacer<'a> {
    pub fn new(stop: &'a dyn Fn() -> bool) -> Self {
        Self {
            cadence: None,
            stop,
        }
    }

    /// Waits for the next slot `period` after the previous one (the first
    /// call counts from now), or until stop.
    pub fn wait(&mut self, period: Duration) {
        self.wait_on(period, Instant::now, &mut thread::sleep);
    }

    fn wait_on(
        &mut self,
        period: Duration,
        now: impl Fn() -> Instant,
        sleep: &mut impl FnMut(Duration),
    ) {
        let start = now();
        let cadence = match &mut self.cadence {
            Some(cadence) if cadence.interval == period.max(Duration::from_millis(1)) => cadence,
            slot => slot.insert(Cadence::new(start, period)),
        };
        cadence.advance(start);
        while !(self.stop)() {
            let Some(left) = cadence.wait(now()) else {
                break;
            };
            sleep(left.min(STOP_POLL));
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// Fake clock the pacer reads and its sleeps advance.
    fn pace(pacer: &mut Pacer, period: Duration, clock: &Cell<Instant>) -> Vec<Duration> {
        let mut slept = Vec::new();
        pacer.wait_on(period, || clock.get(), &mut |d| {
            slept.push(d);
            clock.set(clock.get() + d);
        });
        slept
    }

    /// Regression: `record`, `--json-stream` and `agent run` slept a full
    /// interval after each tick, so every tick's cost was added to the
    /// period (573 frames a minute at 100 ms instead of 600).
    #[test]
    fn pacer_holds_a_fixed_rate_whatever_the_tick_costs() {
        let t0 = Instant::now();
        let clock = Cell::new(t0);
        let never = || false;
        let mut pacer = Pacer::new(&never);
        let period = 500 * MS;
        for n in 1..=30_u32 {
            pace(&mut pacer, period, &clock);
            assert_eq!(clock.get(), t0 + period * n, "slot {n}");
            clock.set(clock.get() + 7 * MS); // the tick
        }
    }

    /// Regression: Ctrl-C during `record -i 60s` went unnoticed until the
    /// whole interval had passed.
    #[test]
    fn pacer_wait_ends_within_one_poll_of_a_stop_request() {
        let t0 = Instant::now();
        let clock = Cell::new(t0);
        let stop = || clock.get() >= t0 + 2 * Duration::from_secs(1);
        let mut pacer = Pacer::new(&stop);
        let slept = pace(&mut pacer, Duration::from_secs(60), &clock);
        assert!(slept.iter().all(|d| *d <= STOP_POLL));
        assert!(clock.get() <= t0 + Duration::from_secs(2) + STOP_POLL);
    }

    const MS: Duration = Duration::from_millis(1);

    /// Simulated clock: each tick starts at its slot and takes `cost`.
    fn run_ticks(interval: Duration, costs: &[Duration]) -> (Vec<Instant>, Cadence) {
        let start = Instant::now();
        let mut cadence = Cadence::new(start, interval);
        let mut now = start;
        let mut starts = Vec::new();
        for cost in costs {
            if let Some(wait) = cadence.wait(now) {
                now += wait;
            }
            starts.push(now);
            now += *cost;
            cadence.advance(now);
        }
        (starts, cadence)
    }

    #[test]
    fn slow_ticks_do_not_stretch_the_period() {
        // every 8th tick costs 90 ms, like the 2 s sensor sweep at 250 ms
        let costs: Vec<Duration> = (0..240)
            .map(|i| if i % 8 == 7 { 90 * MS } else { 5 * MS })
            .collect();
        let (starts, cadence) = run_ticks(250 * MS, &costs);
        let span = *starts.last().unwrap() - starts[0];
        let mean = span / u32::try_from(starts.len() - 1).unwrap();
        assert_eq!(mean, 250 * MS, "fixed rate keeps the mean period exact");
        assert_eq!(cadence.skipped(), 0);
    }

    #[test]
    fn a_tick_longer_than_the_period_catches_up_without_skipping() {
        let (starts, cadence) = run_ticks(250 * MS, &[300 * MS, 5 * MS, 5 * MS]);
        assert_eq!(starts[1] - starts[0], 300 * MS, "late slot runs at once");
        assert_eq!(starts[2] - starts[0], 500 * MS, "then back on the grid");
        assert_eq!(cadence.skipped(), 0);
    }

    #[test]
    fn a_stall_resyncs_and_counts_skips() {
        let (starts, cadence) = run_ticks(250 * MS, &[1100 * MS, 5 * MS, 5 * MS]);
        assert_eq!(cadence.skipped(), 3);
        assert_eq!(starts[1] - starts[0], 1100 * MS);
        assert_eq!(starts[2] - starts[1], 250 * MS);
    }

    #[test]
    fn interval_change_reschedules_from_now() {
        let start = Instant::now();
        let mut cadence = Cadence::new(start, 250 * MS);
        cadence.advance(start + MS);
        cadence.set_interval(1000 * MS, start + 10 * MS);
        assert_eq!(cadence.wait(start + 10 * MS), Some(1000 * MS));
    }

    struct Fake(u32);

    impl Tick for Fake {
        fn tick(&mut self) -> Result<Snapshot> {
            self.0 += 1;
            Ok(Snapshot::empty())
        }
    }

    fn next_snapshot(rx: &Receiver<AppMsg>) -> Result<Snapshot> {
        match rx.recv_timeout(Duration::from_secs(2)).expect("snapshot") {
            AppMsg::Snapshot(snap) => *snap,
            AppMsg::Stopped(err) => Err(err),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn worker_streams_pauses_and_stops() {
        let (tx, rx) = channel();
        let handle = spawn_with(20 * MS, tx, || Ok(Fake(0))).expect("spawn");
        assert!(next_snapshot(&rx).is_ok());
        assert!(next_snapshot(&rx).is_ok());
        handle.send(Cmd::Paused(true));
        while rx.recv_timeout(60 * MS).is_ok() {}
        assert!(
            rx.recv_timeout(100 * MS).is_err(),
            "paused worker sends nothing"
        );
        handle.send(Cmd::Paused(false));
        assert!(next_snapshot(&rx).is_ok());
        drop(handle);
    }

    /// Records every interval the worker hands the sampler.
    struct Intervals(std::sync::Arc<std::sync::Mutex<Vec<Duration>>>);

    impl Tick for Intervals {
        fn tick(&mut self) -> Result<Snapshot> {
            Ok(Snapshot::empty())
        }

        fn set_interval(&self, interval: Duration) {
            self.0.lock().unwrap().push(interval);
        }
    }

    #[test]
    fn worker_hands_the_sampler_its_interval_at_start_and_on_change() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = std::sync::Arc::clone(&seen);
        let (tx, rx) = channel();
        let handle = spawn_with(20 * MS, tx, move || Ok(Intervals(log))).expect("spawn");
        assert!(next_snapshot(&rx).is_ok());
        handle.send(Cmd::Interval(40 * MS));
        handle.send(Cmd::Paused(true));
        handle.send(Cmd::Interval(60 * MS));
        handle.send(Cmd::Paused(false));
        drop(handle);
        assert_eq!(*seen.lock().unwrap(), vec![20 * MS, 40 * MS, 60 * MS]);
    }

    #[test]
    fn power_follow_stretches_on_battery_and_restores_on_the_charger() {
        let mut power = PowerFollow::new(500 * MS);
        assert_eq!(power.observe(false), None, "on AC nothing changes");
        assert_eq!(power.observe(true), Some(siltop_metrics::LOW_POWER_TICK));
        assert_eq!(power.observe(true), None, "no churn while on battery");
        assert_eq!(power.observe(false), Some(500 * MS));
    }

    #[test]
    fn power_follow_keeps_a_user_interval_change_on_battery() {
        let mut power = PowerFollow::new(500 * MS);
        power.observe(true);
        assert_eq!(power.request(250 * MS), siltop_metrics::LOW_POWER_TICK);
        assert_eq!(power.request(5000 * MS), 5000 * MS, "slower is kept");
        assert_eq!(power.observe(false), Some(5000 * MS));
    }

    /// A sampler on battery: records what the worker tells it.
    struct OnBattery(std::sync::Arc<std::sync::Mutex<Vec<(Duration, bool)>>>);

    impl Tick for OnBattery {
        fn tick(&mut self) -> Result<Snapshot> {
            let mut snap = Snapshot::empty();
            snap.battery.external_connected = Some(false);
            Ok(snap)
        }

        fn set_interval(&self, interval: Duration) {
            self.0.lock().unwrap().push((interval, false));
        }

        fn set_low_power(&self, low: bool) {
            self.0.lock().unwrap().push((Duration::ZERO, low));
        }
    }

    #[test]
    fn worker_enters_low_power_after_a_battery_snapshot() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = std::sync::Arc::clone(&seen);
        let (tx, rx) = channel();
        let handle = spawn_with(20 * MS, tx, move || Ok(OnBattery(log))).expect("spawn");
        assert!(next_snapshot(&rx).is_ok());
        let started = Instant::now();
        while seen.lock().unwrap().len() < 3 && started.elapsed() < Duration::from_secs(2) {
            thread::sleep(5 * MS);
        }
        drop(handle);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                (20 * MS, false),
                (Duration::ZERO, true),
                (siltop_metrics::LOW_POWER_TICK, false)
            ]
        );
    }

    #[test]
    fn failed_sampler_reports_once_and_exits() {
        let (tx, rx) = channel();
        let handle = spawn_with(20 * MS, tx, || -> Result<Fake> {
            Err(Error::system("no sampler"))
        })
        .expect("spawn");
        assert!(next_snapshot(&rx).is_err());
        drop(handle);
    }

    struct Exploding;

    impl Tick for Exploding {
        fn tick(&mut self) -> Result<Snapshot> {
            std::panic::panic_any("smc read out of bounds")
        }
    }

    #[test]
    fn rv_sampler_panic_becomes_an_error_status() {
        let (tx, rx) = channel();
        let handle = spawn_with(20 * MS, tx, || Ok(Exploding)).expect("spawn");
        let err = next_snapshot(&rx).expect_err("panic is reported");
        let text = err.to_string();
        assert!(text.contains("sampler stopped"), "{text}");
        assert!(text.contains("smc read out of bounds"), "{text}");
        drop(handle);
    }

    /// Records every hint the worker hands the sampler, in order.
    #[derive(Clone, Default)]
    struct Log(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

    impl Log {
        fn push(&self, entry: String) {
            self.0.lock().unwrap().push(entry);
        }

        fn entries(&self) -> Vec<String> {
            self.0.lock().unwrap().clone()
        }
    }

    impl Tick for Log {
        fn tick(&mut self) -> Result<Snapshot> {
            Ok(Snapshot::empty())
        }

        fn set_paused(&self, paused: bool) {
            self.push(format!("paused {paused}"));
        }

        fn set_low_power(&self, low: bool) {
            self.push(format!("low {low}"));
        }

        fn set_needs(&self, needs: Needs) {
            self.push(format!("needs {needs:?}"));
        }
    }

    /// Runs `cmds` through a worker on `Log` and returns what it saw; the
    /// drop joins the thread, so every command has been applied.
    fn route(cmds: &[Cmd]) -> Vec<String> {
        let log = Log::default();
        let seen = log.clone();
        let (tx, rx) = channel();
        let handle = spawn_with(20 * MS, tx, move || Ok(seen)).expect("spawn");
        assert!(next_snapshot(&rx).is_ok());
        for cmd in cmds {
            handle.send(*cmd);
        }
        drop(handle);
        log.entries()
    }

    #[test]
    fn worker_routes_needs_to_the_sampler_in_order() {
        let seen = route(&[Cmd::Needs(Needs::NONE), Cmd::Needs(Needs::ALL)]);
        assert_eq!(
            seen,
            vec![
                format!("needs {:?}", Needs::NONE),
                format!("needs {:?}", Needs::ALL)
            ]
        );
    }

    #[test]
    fn needs_sent_while_frozen_still_reach_the_sampler() {
        let seen = route(&[
            Cmd::Paused(true),
            Cmd::Needs(Needs::POWER),
            Cmd::Paused(false),
        ]);
        assert_eq!(
            seen,
            vec![
                String::from("paused true"),
                format!("needs {:?}", Needs::POWER),
                String::from("paused false"),
            ]
        );
    }

    #[test]
    fn needs_never_pause_or_unpause_the_worker() {
        let log = Log::default();
        let seen = log.clone();
        let (tx, rx) = channel();
        let handle = spawn_with(20 * MS, tx, move || Ok(seen)).expect("spawn");
        assert!(next_snapshot(&rx).is_ok());
        handle.send(Cmd::Needs(Needs::NONE));
        assert!(
            next_snapshot(&rx).is_ok(),
            "still streaming with nothing needed"
        );
        drop(handle);
        assert!(log.entries().iter().all(|e| !e.starts_with("paused")));
    }

    #[test]
    fn power_follow_obeys_the_battery_saver_setting() {
        let mut power = PowerFollow::new(500 * MS);
        power.observe(true);
        assert_eq!(
            power.set_mode(LowPower::Off),
            Some(500 * MS),
            "off: full rate"
        );
        assert_eq!(power.observe(false), None);
        assert_eq!(
            power.set_mode(LowPower::On),
            Some(siltop_metrics::LOW_POWER_TICK),
            "on: slow on the charger too"
        );
        assert_eq!(power.observe(true), None);
        assert_eq!(
            power.set_mode(LowPower::Auto),
            None,
            "auto on battery: still low"
        );
        assert_eq!(power.observe(false), Some(500 * MS));
    }

    #[test]
    fn rv_battery_saver_off_keeps_full_rate_on_battery() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = std::sync::Arc::clone(&seen);
        let (tx, rx) = channel();
        let handle =
            spawn_power(20 * MS, LowPower::Off, tx, move || Ok(OnBattery(log))).expect("spawn");
        for _ in 0..3 {
            assert!(next_snapshot(&rx).is_ok(), "no LowPower message ever");
        }
        drop(handle);
        assert_eq!(*seen.lock().unwrap(), vec![(20 * MS, false)]);
    }

    #[test]
    fn rv_battery_saver_on_slows_on_the_charger_from_the_first_tick() {
        let log = Log::default();
        let seen = log.clone();
        let (tx, rx) = channel();
        let handle = spawn_power(20 * MS, LowPower::On, tx, move || Ok(seen)).expect("spawn");
        let first = rx.recv_timeout(Duration::from_secs(2)).expect("message");
        assert!(matches!(first, AppMsg::LowPower(true)), "{first:?}");
        assert!(next_snapshot(&rx).is_ok());
        drop(handle);
        assert_eq!(log.entries(), vec![String::from("low true")]);
    }

    #[test]
    fn rv_battery_saver_changes_reach_a_running_worker() {
        let seen = route(&[
            Cmd::LowPower(LowPower::On),
            Cmd::Paused(true),
            Cmd::LowPower(LowPower::Off),
            Cmd::Paused(false),
        ]);
        assert_eq!(
            seen,
            vec![
                String::from("low true"),
                String::from("paused true"),
                String::from("low false"),
                String::from("paused false"),
            ]
        );
    }

    #[test]
    fn rv_boxed_sampler_forwards_on_battery() {
        let mut snap = Snapshot::empty();
        snap.battery.external_connected = Some(false);
        let real: Box<dyn Tick> = Box::new(Log::default());
        assert!(real.on_battery(&snap));
        let demo: Box<dyn Tick> = Box::new(crate::demo::Demo::new(20 * MS));
        assert!(
            !demo.on_battery(&snap),
            "the canned battery is not this Mac's"
        );
    }

    #[test]
    fn rv_boxed_sampler_forwards_low_power_and_needs() {
        // `serve`, `record` and headless run a `Box<dyn Tick>`; the box used
        // to drop `set_low_power`, so on battery their collectors never
        // slowed down.
        let log = Log::default();
        let boxed: Box<dyn Tick> = Box::new(log.clone());
        boxed.set_low_power(true);
        boxed.set_needs(Needs::GPU_CLOCKS);
        assert_eq!(
            log.entries(),
            vec![
                String::from("low true"),
                format!("needs {:?}", Needs::GPU_CLOCKS)
            ]
        );
    }
}
