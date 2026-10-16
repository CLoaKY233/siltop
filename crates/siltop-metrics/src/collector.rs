//! The cheap-tick collector contract and per-collector health.

use std::time::Instant;

use siltop_core::{CollectorId, Health, Result, Snapshot};

/// What a successful collection produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// New values were written into the snapshot.
    Fresh,
    /// Only a baseline was recorded; its rates need one more sample.
    Baseline,
    /// This machine has no source for it.
    Absent,
}

pub(crate) trait Collector {
    fn id(&self) -> CollectorId;

    /// Writes this collector's part of `snap`. On error nothing is written,
    /// so the previous value stays in place.
    fn collect(&mut self, now: Instant, snap: &mut Snapshot) -> Result<Outcome>;
}

/// A registered collector and the time of its last success.
pub(crate) struct Tracked {
    collector: Box<dyn Collector>,
    last_ok: Option<Instant>,
}

impl Tracked {
    pub(crate) fn new(collector: Box<dyn Collector>) -> Self {
        Self {
            collector,
            last_ok: None,
        }
    }

    /// Runs one collection, records its health, and reports whether the
    /// snapshot's rates for this collector are still unprimed.
    pub(crate) fn run(&mut self, now: Instant, snap: &mut Snapshot) -> bool {
        let result = self.collector.collect(now, snap);
        let health = health_after(&result, self.last_ok, now);
        if matches!(result, Ok(Outcome::Fresh | Outcome::Baseline)) {
            self.last_ok = Some(now);
        }
        snap.health.set(self.collector.id(), health);
        matches!(result, Ok(Outcome::Baseline))
    }
}

/// Health after one collection: a failure after a success is stale by the
/// age of that success; a failure with no success yet is a failure.
pub(crate) fn health_after(
    result: &Result<Outcome>,
    last_ok: Option<Instant>,
    now: Instant,
) -> Health {
    match result {
        Ok(Outcome::Fresh | Outcome::Baseline) => Health::Ok,
        Ok(Outcome::Absent) => Health::Absent,
        Err(err) => match last_ok {
            Some(at) => Health::Stale(now.saturating_duration_since(at)),
            None => Health::Failed(err.to_string()),
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use siltop_core::Error;
    use std::time::Duration;

    #[test]
    fn failures_become_stale_after_a_success() {
        let t0 = Instant::now();
        let later = t0 + Duration::from_secs(3);
        let err: Result<Outcome> = Err(Error::system("host_statistics64: 5"));
        assert_eq!(health_after(&Ok(Outcome::Fresh), None, t0), Health::Ok);
        assert_eq!(health_after(&Ok(Outcome::Baseline), None, t0), Health::Ok);
        assert_eq!(health_after(&Ok(Outcome::Absent), None, t0), Health::Absent);
        assert_eq!(
            health_after(&err, None, t0),
            Health::Failed(String::from("system: host_statistics64: 5"))
        );
        assert_eq!(
            health_after(&err, Some(t0), later),
            Health::Stale(Duration::from_secs(3))
        );
    }

    struct Flaky {
        calls: u32,
    }

    impl Collector for Flaky {
        fn id(&self) -> CollectorId {
            CollectorId::Memory
        }

        fn collect(&mut self, _now: Instant, snap: &mut Snapshot) -> Result<Outcome> {
            self.calls += 1;
            if self.calls == 2 {
                return Err(Error::system("flaky"));
            }
            snap.memory.used_bytes = u64::from(self.calls);
            Ok(if self.calls == 1 {
                Outcome::Baseline
            } else {
                Outcome::Fresh
            })
        }
    }

    #[test]
    fn a_failed_tick_keeps_the_last_value_and_marks_it_stale() {
        let mut tracked = Tracked::new(Box::new(Flaky { calls: 0 }));
        let mut snap = Snapshot::empty();
        let t0 = Instant::now();
        assert!(tracked.run(t0, &mut snap), "first run is a baseline");
        assert_eq!(snap.health.get(CollectorId::Memory), &Health::Ok);
        assert!(!tracked.run(t0 + Duration::from_secs(1), &mut snap));
        assert_eq!(snap.memory.used_bytes, 1, "last good value kept");
        assert_eq!(
            snap.health.get(CollectorId::Memory),
            &Health::Stale(Duration::from_secs(1))
        );
        assert!(!tracked.run(t0 + Duration::from_secs(2), &mut snap));
        assert_eq!(snap.memory.used_bytes, 3);
        assert!(snap.health.is_fresh(CollectorId::Memory));
    }
}
