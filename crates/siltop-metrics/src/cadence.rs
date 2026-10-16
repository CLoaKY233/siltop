//! Low-power cadences: what the sampler slows down to on battery.
//!
//! On battery every sampler wake costs the user runtime, so the worker
//! stretches the snapshot tick to [`LOW_POWER_TICK`], the process table to
//! [`LOW_POWER_PROCS`] and the SMC sensor sweep to [`LOW_POWER_SENSORS`].
//! A longer interval the user chose is kept; a desktop (no battery) and a
//! Mac on its charger run at full cadence. One shared [`PowerMode`] flag
//! reaches every collector and thread that honours it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use siltop_core::Snapshot;

/// Snapshot interval floor on battery.
pub const LOW_POWER_TICK: Duration = Duration::from_secs(2);
/// Process-table refresh on battery: every other [`LOW_POWER_TICK`]. A
/// period that is not a multiple of the tick lands on whole ticks anyway.
pub(crate) const LOW_POWER_PROCS: Duration = Duration::from_secs(4);
/// SMC sensor sweep on battery.
pub(crate) const LOW_POWER_SENSORS: Duration = Duration::from_secs(10);

/// Whether the machine is running from its battery. `None`
/// (`external_connected` unknown, or no battery at all) is not.
pub fn on_battery(snap: &Snapshot) -> bool {
    snap.battery.external_connected == Some(false)
}

/// The snapshot interval to run at: the requested one, or
/// [`LOW_POWER_TICK`] on battery when the request is faster.
pub fn tick_interval(requested: Duration, low_power: bool) -> Duration {
    if low_power {
        requested.max(LOW_POWER_TICK)
    } else {
        requested
    }
}

/// `normal`, or `slow` when it is longer and low-power mode is on.
pub(crate) fn stretched(normal: Duration, slow: Duration, low_power: bool) -> Duration {
    if low_power { normal.max(slow) } else { normal }
}

/// How early a periodic check made on the sampler's tick may fire. The
/// worker ticks on a fixed grid, but each wake lands a few ms after its
/// slot, so two wakes one period apart can be 999 ms apart; without slack
/// that refresh slips a whole tick (a 1 s refresh took 1.5 s at a 500 ms
/// interval most of the time).
pub(crate) const TICK_SLACK: Duration = Duration::from_millis(50);

/// Periodic gate: due on the first call and whenever `every` has elapsed
/// since `last`, give or take [`TICK_SLACK`]. A clock that appears to run
/// backwards is not due.
pub(crate) fn due(last: Option<Instant>, now: Instant, every: Duration) -> bool {
    last.is_none_or(|t| now.saturating_duration_since(t) + TICK_SLACK >= every)
}

/// The low-power flag and the snapshot interval, shared by the sampler, its
/// collectors and threads.
/// Wake latency a [`Pace`] tolerates, capped at half the tick: far above
/// the few ms a wake normally lands after its slot, far below any period.
const WAKE_JITTER: Duration = Duration::from_millis(100);

/// A refresh paced slower than the snapshot tick: due on the first tick at
/// or past its period, whatever latency each wake landed with.
///
/// Each wake lands a variable latency after its fixed slot, so comparing
/// the raw time since the last run against the period made a 1 s refresh
/// on a 500 ms tick slip a whole tick whenever this wake came quicker than
/// the last one. [`WAKE_JITTER`] of slack (at most half the gap between
/// calls, so a whole tick early never passes) absorbs that.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Pace {
    ran: Option<Instant>,
    seen: Option<Instant>,
    gap: Duration,
}

impl Pace {
    /// Records a call at `now` and says whether work paced every `every`
    /// is due: the first call, or `every` has elapsed since the last run
    /// give or take wake jitter. A clock that runs backwards is not due.
    pub(crate) fn due(&mut self, now: Instant, every: Duration) -> bool {
        if let Some(seen) = self.seen {
            self.gap = now.saturating_duration_since(seen);
        }
        self.seen = Some(now);
        self.ran.is_none_or(|ran| {
            now.checked_duration_since(ran)
                .is_some_and(|since| since.saturating_add(self.slack()) >= every)
        })
    }

    /// Marks the work as done at `now`.
    pub(crate) fn ran(&mut self, now: Instant) {
        self.ran = Some(now);
    }

    /// The jitter tolerance: [`WAKE_JITTER`], or half the gap between the
    /// last two calls when that is less.
    pub(crate) fn slack(&self) -> Duration {
        WAKE_JITTER.min(self.gap / 2)
    }
}

/// The low-power flag shared by the sampler, its collectors and threads.
#[derive(Debug, Clone, Default)]
pub(crate) struct PowerMode(Arc<AtomicBool>, Arc<AtomicU64>);

impl PowerMode {
    pub(crate) fn set(&self, low: bool) {
        self.0.store(low, Ordering::Relaxed);
    }

    pub(crate) fn low(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Records the snapshot interval: a background sweep faster than it
    /// only makes readings nobody sees.
    pub(crate) fn set_interval(&self, interval: Duration) {
        let nanos = u64::try_from(interval.as_nanos()).unwrap_or(u64::MAX);
        self.1.store(nanos, Ordering::Relaxed);
    }

    /// The snapshot interval; zero until the sampler is told one.
    pub(crate) fn interval(&self) -> Duration {
        Duration::from_nanos(self.1.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn battery_stretches_a_fast_tick_to_two_seconds() {
        assert_eq!(tick_interval(500 * MS, true), LOW_POWER_TICK);
        assert_eq!(tick_interval(250 * MS, true), LOW_POWER_TICK);
        assert_eq!(tick_interval(500 * MS, false), 500 * MS);
    }

    #[test]
    fn a_slower_user_interval_is_kept_on_battery() {
        assert_eq!(tick_interval(5000 * MS, true), 5000 * MS);
    }

    #[test]
    fn jittered_ticks_keep_a_one_second_refresh_at_one_second() {
        // A 500 ms grid whose wakes land 0–9 ms late, in an order where
        // each refresh slot often wakes earlier than the one before.
        let t0 = Instant::now();
        let jitter = [7_u32, 1, 9, 2, 8, 0, 6, 3, 9, 1, 5, 4, 8, 2, 7, 0];
        let mut last = None;
        let mut refreshed = Vec::new();
        for (slot, late) in (0_u32..).zip(jitter) {
            let now = t0 + 500 * MS * slot + MS * late;
            if due(last, now, 1000 * MS) {
                last = Some(now);
                refreshed.push(slot);
            }
        }
        let gaps: Vec<u32> = refreshed.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps.iter().all(|g| *g == 2), "slots {refreshed:?}");
        assert!(
            !due(Some(t0), t0 + 400 * MS, 500 * MS),
            "never a tick early"
        );
    }

    #[test]
    fn stretched_only_ever_slows_down() {
        assert_eq!(stretched(1000 * MS, LOW_POWER_PROCS, true), LOW_POWER_PROCS);
        assert_eq!(stretched(1000 * MS, LOW_POWER_PROCS, false), 1000 * MS);
        assert_eq!(stretched(20_000 * MS, LOW_POWER_SENSORS, true), 20_000 * MS);
    }

    /// The ticks, in ms from the first, at which `pace` ran work paced
    /// every `every_ms` when each wake lands `jitter(i)` ms after its slot.
    fn runs(every_ms: u64, tick_ms: u64, jitter: impl Fn(u64) -> u64) -> Vec<u64> {
        let t0 = Instant::now();
        let mut pace = Pace::default();
        let mut out = Vec::new();
        for i in 0..40 {
            let now = t0 + Duration::from_millis(i * tick_ms + jitter(i));
            if pace.due(now, Duration::from_millis(every_ms)) {
                pace.ran(now);
                out.push(i);
            }
        }
        out
    }

    #[test]
    fn a_one_second_refresh_on_a_jittery_half_second_tick_never_slips() {
        // Wake latency alternating 30 ms / 2 ms: a quick wake after a slow
        // one used to fall 28 ms short of 1 s and wait a whole extra tick.
        let got = runs(1000, 500, |i| if i % 2 == 0 { 30 } else { 2 });
        let want: Vec<u64> = (0..40).step_by(2).collect();
        assert_eq!(got, want);
        let got = runs(1000, 500, |i| if i % 3 == 0 { 45 } else { i % 7 });
        assert!(got.windows(2).all(|w| w[1] - w[0] == 2), "{got:?}");
    }

    #[test]
    fn pace_waits_for_the_first_tick_at_or_past_its_period() {
        assert!(runs(1000, 250, |_| 0).windows(2).all(|w| w[1] - w[0] == 4));
        assert!(
            runs(1000, 100, |i| (i * 13) % 45)
                .windows(2)
                .all(|w| w[1] - w[0] == 10)
        );
        assert!(
            runs(2000, 1500, |i| i % 2 * 60)
                .windows(2)
                .all(|w| w[1] - w[0] == 2)
        );
        assert!(
            runs(1000, 1000, |i| i % 5)
                .windows(2)
                .all(|w| w[1] - w[0] == 1)
        );
        assert!(
            runs(6000, 1000, |i| (i * 7) % 40)
                .windows(2)
                .all(|w| w[1] - w[0] == 6)
        );
    }

    #[test]
    fn battery_procs_run_every_other_battery_tick() {
        let every = LOW_POWER_PROCS.as_millis() as u64;
        let tick = LOW_POWER_TICK.as_millis() as u64;
        assert_eq!(every % tick, 0, "a whole number of battery ticks");
        let got = runs(every, tick, |i| if (i / 2) % 2 == 0 { 40 } else { 3 });
        assert!(got.windows(2).all(|w| w[1] - w[0] == 2), "{got:?}");
    }

    #[test]
    fn pace_first_call_is_due_and_a_backwards_clock_is_not() {
        let t0 = Instant::now();
        let mut pace = Pace::default();
        assert!(pace.due(t0, Duration::from_secs(2)));
        pace.ran(t0 + Duration::from_secs(5));
        assert!(!pace.due(t0, Duration::from_secs(2)), "backwards clock");
        assert!(pace.due(t0 + Duration::from_secs(5), Duration::ZERO));
    }

    #[test]
    fn only_a_known_disconnected_charger_is_battery() {
        let mut snap = Snapshot::empty();
        assert!(!on_battery(&snap), "desktop or unknown runs at full speed");
        snap.battery.external_connected = Some(true);
        assert!(!on_battery(&snap));
        snap.battery.external_connected = Some(false);
        assert!(on_battery(&snap));
    }

    #[test]
    fn power_mode_is_shared_between_clones() {
        let mode = PowerMode::default();
        let seen = mode.clone();
        assert!(!seen.low());
        mode.set(true);
        assert!(seen.low());
        mode.set(false);
        assert!(!seen.low());
    }
}
