//! Resolution tiers and the pure maths that moves buckets between them.
//!
//! Live (1 s, 15 min) is what the in-app rings already hold; the store keeps
//! Day (6 s, 24 h) and Month (144 s, 30 d). Each tier's span divides the
//! next one's, so a coarse bucket is always a whole number of fine ones.

use super::bucket::{Bucket, Stat, TopProc, merge_apps, merge_top};
use super::{SERIES_COUNT, TOP_N};

/// One resolution of stored history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tier {
    /// The cheap cadence the dashboard samples at; served by the live rings.
    Live,
    /// 6 s buckets kept for a day: what the writer appends.
    Day,
    /// 144 s buckets kept for thirty days, rolled up from Day.
    Month,
}

impl Tier {
    pub const ALL: [Self; 3] = [Self::Live, Self::Day, Self::Month];
    /// The tiers the store persists, finest first.
    pub const STORED: [Self; 2] = [Self::Day, Self::Month];

    /// Seconds one bucket covers.
    #[must_use]
    pub const fn span_s(self) -> u64 {
        match self {
            Self::Live => 1,
            Self::Day => 6,
            Self::Month => 144,
        }
    }

    /// Seconds of history this tier keeps.
    #[must_use]
    pub const fn retention_s(self) -> u64 {
        match self {
            Self::Live => 15 * 60,
            Self::Day => 24 * 3600,
            Self::Month => 30 * 24 * 3600,
        }
    }

    /// Buckets the tier holds when full.
    #[must_use]
    pub const fn capacity(self) -> usize {
        (self.retention_s() / self.span_s()) as usize
    }

    /// Stable one-byte id for file headers.
    #[must_use]
    pub const fn id(self) -> u8 {
        match self {
            Self::Live => 0,
            Self::Day => 1,
            Self::Month => 2,
        }
    }

    #[must_use]
    pub fn from_id(id: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|tier| tier.id() == id)
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Day => "day",
            Self::Month => "month",
        }
    }

    /// The stored tier that answers a query at this resolution.
    #[must_use]
    pub const fn stored(self) -> Self {
        match self {
            Self::Live | Self::Day => Self::Day,
            Self::Month => Self::Month,
        }
    }

    /// Start of the bucket holding unix second `t`.
    #[must_use]
    pub const fn floor(self, t: u64) -> u64 {
        t - t % self.span_s()
    }

    /// Whether a bucket starting at `t` is still inside retention at `now`.
    #[must_use]
    pub const fn keeps(self, t: u64, now: u64) -> bool {
        t + self.span_s() + self.retention_s() > now
    }
}

/// Folds consecutive buckets into one of a coarser `span_s`: means are
/// weighted by sample count, maxes are maxes, top lists are re-ranked.
/// `None` for an empty slice or one with no samples.
#[must_use]
pub fn merge(buckets: &[Bucket], t: u64) -> Option<Bucket> {
    let samples: u32 = buckets.iter().map(|b| b.samples).sum();
    if samples == 0 {
        return None;
    }
    let mut series = [None; SERIES_COUNT];
    for (i, slot) in series.iter_mut().enumerate() {
        let (mut weighted, mut weight, mut max) = (0.0_f64, 0_u64, f32::MIN);
        for b in buckets {
            if let Some(stat) = b.series[i] {
                weighted += f64::from(stat.mean) * f64::from(b.samples);
                weight += u64::from(b.samples);
                max = max.max(stat.max);
            }
        }
        if weight > 0 {
            *slot = Some(Stat {
                mean: (weighted / weight as f64) as f32,
                max,
            });
        }
    }
    let top = |pick: fn(&Bucket) -> &Vec<TopProc>| {
        merge_top(
            buckets.iter().map(|b| (pick(b).as_slice(), b.samples)),
            samples,
            TOP_N,
        )
    };
    let (apps, apps_other) = merge_apps(
        buckets
            .iter()
            .map(|b| (b.apps.as_slice(), b.apps_other, b.samples)),
        samples,
    );
    // Coverage adds up; one record without it leaves the merge unknown.
    let covered_s = buckets
        .iter()
        .try_fold(0_u32, |sum, b| Some(sum.saturating_add(b.covered_s?)));
    Some(Bucket {
        t,
        samples,
        covered_s,
        series,
        apps,
        apps_other,
        top_cpu: top(|b| &b.top_cpu),
        top_energy: top(|b| &b.top_energy),
        top_mem: top(|b| &b.top_mem),
    })
}

/// Groups `buckets` (oldest first) by `tier`'s span and merges each group.
#[must_use]
pub fn downsample(buckets: &[Bucket], tier: Tier) -> Vec<Bucket> {
    buckets
        .chunk_by(|a, b| tier.floor(a.t) == tier.floor(b.t))
        .filter_map(|group| merge(group, tier.floor(group[0].t)))
        .collect()
}

/// Collects fine buckets and emits the coarse one once a bucket from the
/// next coarse span arrives.
#[derive(Debug, Clone)]
pub struct Rollup {
    tier: Tier,
    pending: Vec<Bucket>,
}

impl Rollup {
    #[must_use]
    pub const fn new(tier: Tier) -> Self {
        Self {
            tier,
            pending: Vec::new(),
        }
    }

    /// Adds a fine bucket; returns the finished coarse bucket, if any.
    pub fn push(&mut self, bucket: Bucket) -> Option<Bucket> {
        let span = self.tier.floor(bucket.t);
        let done = match self.pending.first() {
            Some(first) if self.tier.floor(first.t) != span => {
                let t = self.tier.floor(first.t);
                let out = merge(&self.pending, t);
                self.pending.clear();
                out
            }
            _ => None,
        };
        self.pending.push(bucket);
        done
    }

    /// Fine buckets waiting for their coarse span to close.
    #[must_use]
    pub fn pending(&self) -> &[Bucket] {
        &self.pending
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::store::Series;

    fn bucket(t: u64, samples: u32, cpu: f32, cpu_max: f32) -> Bucket {
        let mut b = Bucket::empty(t);
        b.samples = samples;
        b.series[Series::Cpu.index()] = Some(Stat {
            mean: cpu,
            max: cpu_max,
        });
        b
    }

    #[test]
    fn spans_nest_and_capacities_match_the_plan() {
        assert_eq!(Tier::Day.span_s() % Tier::Live.span_s(), 0);
        assert_eq!(Tier::Month.span_s() % Tier::Day.span_s(), 0);
        assert_eq!(Tier::Live.capacity(), 900);
        assert_eq!(Tier::Day.capacity(), 14_400);
        assert_eq!(Tier::Month.capacity(), 18_000);
        for tier in Tier::ALL {
            assert_eq!(Tier::from_id(tier.id()), Some(tier));
        }
        assert_eq!(Tier::from_id(9), None);
        assert_eq!(Tier::Live.stored(), Tier::Day);
    }

    #[test]
    fn floor_aligns_to_span() {
        assert_eq!(Tier::Day.floor(1_000_003), 999_996 + 6);
        assert_eq!(Tier::Day.floor(12), 12);
        assert_eq!(Tier::Month.floor(145), 144);
        assert_eq!(Tier::Month.floor(143), 0);
    }

    #[test]
    fn merge_weights_means_by_samples_and_keeps_the_true_max() {
        let merged = merge(&[bucket(0, 1, 0.9, 1.0), bucket(6, 3, 0.1, 0.2)], 0).unwrap();
        let cpu = merged.series[Series::Cpu.index()].unwrap();
        assert!((cpu.mean - 0.3).abs() < 1e-6, "{cpu:?}");
        assert!((cpu.max - 1.0).abs() < f32::EPSILON);
        assert_eq!(merged.samples, 4);
    }

    #[test]
    fn merge_leaves_an_absent_series_absent_not_zero() {
        let merged = merge(&[bucket(0, 2, 0.5, 0.5)], 0).unwrap();
        assert_eq!(merged.series[Series::Gpu.index()], None);
        assert_eq!(merged.series[Series::Fan.index()], None);
    }

    #[test]
    fn merge_ignores_absent_samples_in_a_partly_present_series() {
        let mut a = bucket(0, 5, 0.0, 0.0);
        a.series[Series::Cpu.index()] = None;
        let merged = merge(&[a, bucket(6, 1, 0.8, 0.8)], 0).unwrap();
        let cpu = merged.series[Series::Cpu.index()].unwrap();
        assert!((cpu.mean - 0.8).abs() < 1e-6);
    }

    #[test]
    fn merge_of_nothing_is_none() {
        assert!(merge(&[], 0).is_none());
        assert!(merge(&[Bucket::empty(0)], 0).is_none());
    }

    #[test]
    fn downsample_groups_by_coarse_span() {
        let day: Vec<Bucket> = (0..48).map(|i| bucket(i * 6, 6, 0.5, 0.5)).collect();
        let month = downsample(&day, Tier::Month);
        assert_eq!(month.len(), 2);
        assert_eq!(month[0].t, 0);
        assert_eq!(month[1].t, 144);
        assert_eq!(month[0].samples, 24 * 6);
    }

    /// Property: downsampling preserves total samples and never invents a
    /// max above the finest max, for any stride.
    #[test]
    fn downsample_preserves_samples_and_bounds_for_every_stride() {
        for stride in 1..40_u64 {
            let day: Vec<Bucket> = (0..200)
                .map(|i| bucket(i * 6 * stride, 1 + (i % 5) as u32, (i % 7) as f32, 9.0))
                .collect();
            let total: u32 = day.iter().map(|b| b.samples).sum();
            let month = downsample(&day, Tier::Month);
            assert_eq!(month.iter().map(|b| b.samples).sum::<u32>(), total);
            for b in &month {
                let cpu = b.series[Series::Cpu.index()].unwrap();
                assert!(cpu.mean >= 0.0 && cpu.mean <= 6.0 && cpu.max <= 9.0);
                assert_eq!(b.t % 144, 0);
            }
        }
    }

    #[test]
    fn rollup_emits_only_when_the_next_span_starts() {
        let mut rollup = Rollup::new(Tier::Month);
        for i in 0..24 {
            assert!(rollup.push(bucket(i * 6, 1, 1.0, 1.0)).is_none());
        }
        let out = rollup.push(bucket(144, 1, 1.0, 1.0)).unwrap();
        assert_eq!((out.t, out.samples), (0, 24));
        assert_eq!(rollup.pending().len(), 1);
    }

    #[test]
    fn rollup_survives_a_sleep_gap() {
        let mut rollup = Rollup::new(Tier::Month);
        rollup.push(bucket(6, 1, 1.0, 1.0));
        let out = rollup.push(bucket(100_000, 1, 1.0, 1.0)).unwrap();
        assert_eq!(out.t, 0);
    }

    #[test]
    fn retention_boundary() {
        let now = 10_000_000;
        assert!(Tier::Day.keeps(now - 3600, now));
        assert!(!Tier::Day.keeps(now - 24 * 3600 - 6, now));
        assert!(Tier::Month.keeps(now - 29 * 86_400, now));
        assert!(!Tier::Month.keeps(now - 31 * 86_400, now));
    }
}
