use std::time::Duration;

/// A held energy reading goes stale this long after its publication.
pub const POWER_STALE_AFTER: Duration = Duration::from_secs(10);

/// Where a watts figure came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadingSource {
    /// Read this sweep: an SMC power key, or an energy counter that
    /// published since the last sample.
    Measured,
    /// The last published energy rate, repeated until the counter publishes
    /// again. `age` is the time since that publication.
    Held { age: Duration },
    /// Derived from a model rather than a counter.
    Estimated,
}

/// A power figure. Missing data is `None` in the owning `Option`, never a
/// `Reading` of 0 W.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    pub watts: f32,
    pub source: ReadingSource,
}

impl Reading {
    #[must_use]
    pub const fn measured(watts: f32) -> Self {
        Self {
            watts,
            source: ReadingSource::Measured,
        }
    }

    /// Watts worth recording in history: new this sample (measured or
    /// estimated, never held over) and finite. Missing stays missing.
    #[must_use]
    pub fn fresh(self) -> Option<f32> {
        (!matches!(self.source, ReadingSource::Held { .. }) && self.watts.is_finite())
            .then_some(self.watts)
    }

    /// Watts safe to show or export as the current draw: finite and not
    /// stale. A recently held value still counts (the counter publishes
    /// slower than the tick); one held past [`POWER_STALE_AFTER`] is a
    /// frozen number, not a measurement. The one policy every consumer
    /// (JSON, Prometheus, history, status line, panels) goes through.
    #[must_use]
    pub fn usable(self) -> Option<f32> {
        (!self.is_stale() && self.watts.is_finite()).then_some(self.watts)
    }

    /// True once a held value has outlived [`POWER_STALE_AFTER`].
    #[must_use]
    pub fn is_stale(&self) -> bool {
        matches!(self.source, ReadingSource::Held { age } if age > POWER_STALE_AFTER)
    }
}

/// Fractions in 0..=1 of the last interval spent limited. `None` means the
/// channel is not collected on this machine (or not yet).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Throttle {
    /// GPU time outside the `NO_CLTM` state (closed-loop thermal limiting).
    pub gpu_cltm: Option<f32>,
    /// GPU time in a power-controller state other than `PERF` or `IDLE_OFF`.
    pub gpu_power_limited: Option<f32>,
    /// CPU time with a CPMS power reduction above 0%.
    pub cpms_reduction: Option<f32>,
}

impl Throttle {
    /// Any channel reports limiting above `threshold`.
    #[must_use]
    pub fn any_above(&self, threshold: f32) -> bool {
        [self.gpu_cltm, self.gpu_power_limited, self.cpms_reduction]
            .into_iter()
            .flatten()
            .any(|f| f > threshold)
    }
}

/// Package power by rail. Every figure is optional: an unmapped model has no
/// CPU rails, and nothing is ever reported as 0 W for lack of data.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PowerSnapshot {
    /// Whole-system input power (`PSTR`).
    pub system: Option<Reading>,
    pub cpu: Option<Reading>,
    pub gpu: Option<Reading>,
    pub ane: Option<Reading>,
    pub dram: Option<Reading>,
    pub throttle: Throttle,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_power_is_absent_not_zero() {
        let p = PowerSnapshot::default();
        assert!(p.system.is_none() && p.cpu.is_none() && p.gpu.is_none());
        assert!(p.ane.is_none() && p.dram.is_none());
        assert_eq!(p.throttle, Throttle::default());
        assert!(!p.throttle.any_above(0.0));
    }

    #[test]
    fn staleness_is_strictly_after_ten_seconds() {
        let held = |secs| Reading {
            watts: 3.0,
            source: ReadingSource::Held {
                age: Duration::from_secs(secs),
            },
        };
        assert!(!held(0).is_stale());
        assert!(!held(10).is_stale());
        assert!(held(11).is_stale());
        assert!(!Reading::measured(3.0).is_stale());
        let est = Reading {
            watts: 1.0,
            source: ReadingSource::Estimated,
        };
        assert!(!est.is_stale());
    }

    /// Regression: JSON, Prometheus and history kept exporting a held
    /// value long after the energy counter stopped publishing.
    #[test]
    fn usable_watts_drop_stale_holds_and_non_finite_values() {
        let held = |secs| Reading {
            watts: 21.5,
            source: ReadingSource::Held {
                age: Duration::from_secs(secs),
            },
        };
        assert_eq!(held(2).usable(), Some(21.5));
        assert_eq!(held(11).usable(), None);
        assert_eq!(Reading::measured(4.0).usable(), Some(4.0));
        assert_eq!(Reading::measured(f32::NAN).usable(), None);
        assert_eq!(Reading::measured(f32::INFINITY).usable(), None);
    }

    #[test]
    fn throttle_threshold_ignores_absent_channels() {
        let t = Throttle {
            gpu_cltm: Some(0.0),
            gpu_power_limited: None,
            cpms_reduction: Some(0.2),
        };
        assert!(t.any_above(0.1));
        assert!(!t.any_above(0.2));
    }
}
