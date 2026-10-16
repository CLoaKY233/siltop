use std::collections::VecDeque;
use std::time::Duration;

use crate::config::{HISTORY_WINDOW, INTERVAL_DEFAULT, SENSOR_PERIOD};

/// Samples that fit `HISTORY_WINDOW` at `period`. Never zero.
#[must_use]
pub fn capacity_for(period: Duration) -> usize {
    let window = HISTORY_WINDOW.as_millis();
    let period = period.as_millis().max(1);
    usize::try_from(window.div_ceil(period)).unwrap_or(1).max(1)
}

/// How a history is mapped onto 0..=1 for drawing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scale {
    /// Always this max from 0 (MEM used-ratio stays 0..=1).
    Fixed(f32),
    /// Grow to a 1-2-5 ceiling of the window peak, never below `floor`.
    /// btop's percent rescale floors at 10% so a 1% series still has shape.
    Auto { floor: f32 },
    /// Window min/max plus pad, never thinner than `min_span`. Temps live here.
    Band { pad: f32, min_span: f32 },
}

impl Scale {
    /// Load / util: 10% floor, same rescale-down as btop.
    pub const LOAD: Self = Self::Auto { floor: 0.10 };
    /// Die / package °C: zoom to the recent band.
    pub const TEMP: Self = Self::Band {
        pad: 3.0,
        min_span: 12.0,
    };
    /// Fan RPM from 0, floor 500 so idle 0 stays a flat line.
    pub const FAN: Self = Self::Auto { floor: 500.0 };

    #[must_use]
    pub fn resolve(self, peak: f32) -> f32 {
        match self {
            Self::Fixed(max) => max.max(f32::EPSILON),
            Self::Auto { floor } => nice_ceiling(peak.max(floor).max(0.0)),
            Self::Band { pad, min_span } => nice_ceiling((peak + pad).max(min_span).max(0.0)),
        }
    }

    /// Scale over the whole ring.
    #[must_use]
    pub fn range(self, history: &History) -> ScaleRange {
        self.range_in(history, HISTORY_WINDOW, history.newest().unwrap_or(0))
    }

    /// Scale over what is on screen: `span` up to `now`. Auto follows the
    /// visible peak and eases a departed peak down over [`SCALE_EASE`].
    #[must_use]
    pub fn range_in(self, history: &History, span: Duration, now: Stamp) -> ScaleRange {
        self.range_in_by(history, span, now, nice_ceiling)
    }

    /// [`Scale::range_in`] with the auto ceiling rounded by `nice`, so a
    /// caller can round in the unit it prints (bytes/s for a bits history).
    #[must_use]
    pub fn range_in_by(
        self,
        history: &History,
        span: Duration,
        now: Stamp,
        nice: impl Fn(f32) -> f32,
    ) -> ScaleRange {
        let from = now.saturating_sub(millis(span));
        match self {
            Self::Fixed(max) => ScaleRange {
                min: 0.0,
                max: max.max(f32::EPSILON),
            },
            Self::Auto { floor } => {
                let visible = history.since(from).map(|(_, v)| v).fold(0.0, f32::max);
                let ceiling = nice(visible.max(floor).max(0.0));
                ScaleRange {
                    min: 0.0,
                    max: ceiling.max(history.eased_peak(from, floor, &nice)),
                }
            }
            Self::Band { pad, min_span } => {
                let (lo, hi) = history
                    .since(from)
                    .fold(None, |acc: Option<(f32, f32)>, (_, v)| {
                        Some(acc.map_or((v, v), |(lo, hi)| (lo.min(v), hi.max(v))))
                    })
                    .unwrap_or((0.0, 0.0));
                band_range(lo, hi, pad, min_span)
            }
        }
    }

    #[must_use]
    pub const fn hints_axis(self) -> bool {
        matches!(self, Self::Auto { .. } | Self::Band { .. })
    }
}

/// Inclusive draw window. Values map as `(v - min) / (max - min)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScaleRange {
    pub min: f32,
    pub max: f32,
}

impl ScaleRange {
    #[must_use]
    pub fn span(self) -> f32 {
        (self.max - self.min).max(f32::EPSILON)
    }
}

fn band_range(lo_v: f32, hi_v: f32, pad: f32, min_span: f32) -> ScaleRange {
    let mut lo = lo_v - pad;
    let mut hi = hi_v + pad;
    let span = hi - lo;
    if span < min_span {
        let extra = (min_span - span) / 2.0;
        lo -= extra;
        hi += extra;
    }
    lo = lo.max(0.0);
    // clamping at 0 must not eat into the minimum span
    hi = hi.max(lo + min_span);
    let step = band_step(min_span);
    lo = snap_down(lo, step);
    hi = snap_up(hi, step);
    if hi <= lo {
        hi = lo + min_span.max(step);
    }
    ScaleRange { min: lo, max: hi }
}

/// Band edges land on a 1-2-5 step near half the minimum span: 5 for a 12°
/// temperature band, 0.05 for a 0.10 (ten percent) charge band, so the edges
/// read round in the band's own unit instead of snapping a fraction to 0..5.
fn band_step(min_span: f32) -> f32 {
    let half = min_span / 2.0;
    if !half.is_finite() || half <= 0.0 {
        return 5.0;
    }
    let base = 10.0_f32.powf(half.log10().floor());
    // a hair of slack so 0.05 / 0.01 still reads as 5 in f32
    let n = half / base * 1.000_1;
    let nice = if n >= 5.0 {
        5.0
    } else if n >= 2.0 {
        2.0
    } else {
        1.0
    };
    nice * base
}

fn snap_down(value: f32, step: f32) -> f32 {
    let step = step.max(f32::EPSILON);
    (value / step).floor() * step
}

fn snap_up(value: f32, step: f32) -> f32 {
    let step = step.max(f32::EPSILON);
    (value / step).ceil() * step
}

/// Next 1 / 2 / 5 × 10^n above `value`. Never 0.
#[must_use]
pub fn nice_ceiling(value: f32) -> f32 {
    if !value.is_finite() || value <= 0.0 {
        return 1.0;
    }
    let exp = value.log10().floor();
    let base = 10.0_f32.powf(exp);
    let n = value / base;
    let nice = if n <= 1.0 {
        1.0
    } else if n <= 2.0 {
        2.0
    } else if n <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * base
}

/// Milliseconds on the app's monotonic sample clock.
pub type Stamp = u64;

/// How long auto-scale takes to ease down once a peak scrolls out of view.
pub const SCALE_EASE: Duration = Duration::from_secs(30);

/// One time bucket: the mean fills the graph, the max is the peak whisker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bucket {
    pub mean: f32,
    pub max: f32,
}

/// The window every compact graph on a surface shares at [`Zoom::Live`]:
/// panels of any width show the same span, a narrow one in coarser buckets.
pub const LIVE_WINDOW: Duration = Duration::from_mins(2);

/// Visible time span of a graph. `Live` is the shared [`LIVE_WINDOW`];
/// `Samples` is one sample per dot, for a standalone strip on its own clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Zoom {
    #[default]
    Live,
    OneMin,
    FiveMin,
    FifteenMin,
    /// One sample per dot at the cadence. Never in the `z` cycle.
    Samples,
}

impl Zoom {
    /// The `z` ladder, narrowest to widest.
    pub const ALL: [Self; 4] = [Self::OneMin, Self::Live, Self::FiveMin, Self::FifteenMin];

    /// The next wider zoom, wrapping from the widest to the narrowest.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Live | Self::Samples => Self::FiveMin,
            Self::FiveMin => Self::FifteenMin,
            Self::FifteenMin => Self::OneMin,
            Self::OneMin => Self::Live,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Live | Self::Samples => "live",
            Self::OneMin => "1m",
            Self::FiveMin => "5m",
            Self::FifteenMin => "15m",
        }
    }

    /// Time shown across `dots` horizontal dots, never past the history
    /// window. Only `Samples` depends on `dots`, so every other zoom gives
    /// plots of any width the same window.
    #[must_use]
    pub fn span(self, cadence: Duration, dots: usize) -> Duration {
        let span = match self {
            Self::Live => LIVE_WINDOW,
            Self::Samples => cadence.saturating_mul(u32::try_from(dots.max(1)).unwrap_or(u32::MAX)),
            Self::OneMin => Duration::from_mins(1),
            Self::FiveMin => Duration::from_mins(5),
            Self::FifteenMin => Duration::from_mins(15),
        };
        span.clamp(Duration::from_millis(1), HISTORY_WINDOW)
    }
}

/// Where a graph looks: its zoom, the newest shared stamp and the cadence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeAxis {
    pub zoom: Zoom,
    pub now: Stamp,
    pub cadence: Duration,
}

impl TimeAxis {
    /// One history on its own clock, one sample per dot.
    #[must_use]
    pub fn live(history: &History) -> Self {
        Self {
            zoom: Zoom::Samples,
            now: history.newest().unwrap_or(0),
            cadence: history.period(),
        }
    }

    #[must_use]
    pub fn span(self, dots: usize) -> Duration {
        self.zoom.span(self.cadence, dots)
    }
}

/// Stamps samples on a shared clock, snapped to the cadence so jitter never
/// splits or merges live dots. Stamps only move forward.
#[derive(Debug, Clone, Copy, Default)]
pub struct SampleClock {
    last: Option<Stamp>,
}

impl SampleClock {
    pub fn stamp(&mut self, elapsed: Duration, cadence: Duration) -> Stamp {
        let cadence = millis(cadence).max(1);
        let snapped = millis(elapsed).saturating_add(cadence / 2) / cadence * cadence;
        let stamp = match self.last {
            Some(last) if snapped <= last => last.saturating_add(cadence),
            _ => snapped,
        };
        self.last = Some(stamp);
        stamp
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// An in-progress bucket while samples stream past in time order.
#[derive(Debug, Clone, Copy)]
struct Run {
    index: u64,
    sum: f32,
    count: u32,
    max: f32,
    newest: Stamp,
}

impl Run {
    fn bucket(self) -> Bucket {
        Bucket {
            mean: self.sum / self.count as f32,
            max: self.max,
        }
    }
}

#[derive(Debug, Clone)]
pub struct History {
    samples: VecDeque<(Stamp, f32)>,
    capacity: usize,
    period: Duration,
}

impl Default for History {
    fn default() -> Self {
        Self::for_period(SENSOR_PERIOD)
    }
}

impl History {
    /// A ring of `capacity` samples whose period spreads them over the window.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let period = Duration::from_millis(millis(HISTORY_WINDOW) / capacity as u64);
        Self::build(capacity, period)
    }

    #[must_use]
    pub fn for_period(period: Duration) -> Self {
        Self::build(capacity_for(period), period)
    }

    fn build(capacity: usize, period: Duration) -> Self {
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity,
            period: period.max(Duration::from_millis(1)),
        }
    }

    #[must_use]
    pub fn cheap() -> Self {
        Self::for_period(INTERVAL_DEFAULT)
    }

    /// Push one period after the newest sample (the ring's own clock).
    pub fn push(&mut self, value: f32) {
        let at = self
            .newest()
            .map_or(0, |t| t.saturating_add(millis(self.period)));
        self.push_at(value, at);
    }

    /// Push at `at` on the shared clock. Stamps never go backwards; samples
    /// older than the history window fall off.
    pub fn push_at(&mut self, value: f32, at: Stamp) {
        if !value.is_finite() {
            return;
        }
        let at = self.newest().map_or(at, |t| at.max(t));
        let horizon = at.saturating_sub(millis(HISTORY_WINDOW));
        while self.samples.len() >= self.capacity
            || self.samples.front().is_some_and(|&(t, _)| t < horizon)
        {
            self.samples.pop_front();
        }
        self.samples.push_back((at, value.max(0.0)));
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    #[must_use]
    pub fn period(&self) -> Duration {
        self.period
    }

    /// Samples the ring holds before the oldest is dropped.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    #[must_use]
    pub fn newest(&self) -> Option<Stamp> {
        self.samples.back().map(|&(t, _)| t)
    }

    #[must_use]
    pub fn last(&self) -> Option<f32> {
        self.samples.back().map(|&(_, v)| v)
    }

    #[must_use]
    pub fn max(&self) -> Option<f32> {
        self.iter().reduce(f32::max)
    }

    #[must_use]
    pub fn min(&self) -> Option<f32> {
        self.iter().reduce(f32::min)
    }

    pub fn iter(&self) -> impl Iterator<Item = f32> + '_ {
        self.samples.iter().map(|&(_, v)| v)
    }

    /// Samples stamped at or after `from`, oldest first.
    fn since(&self, from: Stamp) -> impl Iterator<Item = (Stamp, f32)> + '_ {
        let start = self.samples.partition_point(|&(t, _)| t < from);
        self.samples.range(start..).copied()
    }

    /// Nice ceilings of peaks that left the view less than [`SCALE_EASE`] ago,
    /// each scaled down linearly by how long it has been gone.
    fn eased_peak(&self, from: Stamp, floor: f32, nice: &impl Fn(f32) -> f32) -> f32 {
        let ease = millis(SCALE_EASE).max(1);
        let start = self
            .samples
            .partition_point(|&(t, _)| t < from.saturating_sub(ease));
        self.samples
            .range(start..)
            .take_while(|&&(t, _)| t < from)
            .map(|&(t, v)| {
                let left = 1.0 - (from - t) as f32 / ease as f32;
                nice(v.max(floor).max(0.0)) * left.max(0.0)
            })
            .fold(0.0, f32::max)
    }

    /// `n` buckets covering `span` up to `now`, right-aligned. Boundaries sit
    /// at `floor(t / bucket)`, so an older column never re-bins and series at
    /// different cadences agree on which column a moment belongs to. An empty
    /// bucket holds the previous value for up to two periods, then stays blank.
    #[must_use]
    pub fn buckets(&self, span: Duration, n: usize, now: Stamp) -> Vec<Option<Bucket>> {
        let mut out = vec![None; n];
        if n == 0 || self.samples.is_empty() {
            return out;
        }
        let n64 = n as u64;
        let width = millis(span).div_ceil(n64).max(1);
        let last = now / width;
        // slot of bucket index `k` is `k + shift - (last + 1)`, left-padded when
        // the clock is younger than the view
        let shift = n64;
        let first = (last + 1).saturating_sub(n64);
        let from = first.saturating_mul(width);
        let end = (last + 1).saturating_mul(width);
        let slot = |index: u64| usize::try_from(index + shift - (last + 1)).ok();

        let start = self.samples.partition_point(|&(t, _)| t < from);
        let mut held: Option<Run> =
            start
                .checked_sub(1)
                .and_then(|i| self.samples.get(i))
                .map(|&(t, v)| Run {
                    index: t / width,
                    sum: v,
                    count: 1,
                    max: v,
                    newest: t,
                });
        let max_gap = millis(self.period).saturating_mul(2);
        let mut run: Option<Run> = None;
        let fill = |out: &mut [Option<Bucket>], upto: u64, held: Option<Run>| {
            let Some(held) = held else { return };
            for index in (held.index + 1).max(first)..upto {
                if index.saturating_mul(width).saturating_sub(held.newest) > max_gap {
                    break;
                }
                if let Some(cell) = slot(index).and_then(|i| out.get_mut(i)) {
                    *cell = Some(held.bucket());
                }
            }
        };
        for (t, v) in self.samples.range(start..).copied() {
            if t >= end {
                break;
            }
            let index = t / width;
            match run.as_mut() {
                Some(r) if r.index == index => {
                    r.sum += v;
                    r.count += 1;
                    r.max = r.max.max(v);
                    r.newest = t;
                }
                _ => {
                    if let Some(done) = run.take() {
                        if let Some(cell) = slot(done.index).and_then(|i| out.get_mut(i)) {
                            *cell = Some(done.bucket());
                        }
                        held = Some(done);
                    }
                    fill(&mut out, index, held);
                    run = Some(Run {
                        index,
                        sum: v,
                        count: 1,
                        max: v,
                        newest: t,
                    });
                }
            }
        }
        if let Some(done) = run {
            if let Some(cell) = slot(done.index).and_then(|i| out.get_mut(i)) {
                *cell = Some(done.bucket());
            }
            held = Some(done);
        }
        fill(&mut out, last + 1, held);
        out
    }

    /// Adopt a new sampling period. Samples keep their stamps, so the window
    /// stays time-correct; the ring grows if it must to keep them all.
    pub fn set_period(&mut self, period: Duration) {
        self.period = period.max(Duration::from_millis(1));
        self.capacity = capacity_for(period).max(self.samples.len());
    }

    pub fn set_capacity(&mut self, capacity: usize) {
        let capacity = capacity.max(1);
        while self.samples.len() > capacity {
            self.samples.pop_front();
        }
        self.capacity = capacity;
    }

    #[must_use]
    pub fn range(&self, mode: Scale) -> ScaleRange {
        mode.range(self)
    }

    #[must_use]
    pub fn scale(&self, mode: Scale) -> f32 {
        self.range(mode).max
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn floors_and_caps() {
        let mut h = History::with_capacity(3);
        h.push(-0.5);
        h.push(0.4);
        h.push(1.5);
        h.push(0.2);
        assert_eq!(h.len(), 3);
        let values: Vec<f32> = h.iter().collect();
        assert_eq!(values, vec![0.4, 1.5, 0.2]);
    }

    #[test]
    fn nice_ceiling_steps() {
        assert!((nice_ceiling(0.0) - 1.0).abs() < f32::EPSILON);
        assert!((nice_ceiling(12.4) - 20.0).abs() < f32::EPSILON);
        assert!((nice_ceiling(100.0) - 100.0).abs() < f32::EPSILON);
        assert!((nice_ceiling(101.0) - 200.0).abs() < f32::EPSILON);
        assert!((nice_ceiling(5_000_000.0) - 5_000_000.0).abs() < 1.0);
        assert!((nice_ceiling(5_100_000.0) - 10_000_000.0).abs() < 1.0);
    }

    #[test]
    fn auto_scale_uses_nice_peak() {
        let mut h = History::with_capacity(8);
        h.push(1_200_000.0);
        h.push(3_400_000.0);
        let scale = h.scale(Scale::Auto { floor: 1.0 });
        assert!((scale - 5_000_000.0).abs() < 1.0);
    }

    #[test]
    fn last_and_max() {
        let mut h = History::default();
        assert!(h.last().is_none());
        h.push(0.2);
        h.push(0.7);
        h.push(0.1);
        assert!((h.last().unwrap_or(0.0) - 0.1).abs() < f32::EPSILON);
        assert!((h.max().unwrap_or(0.0) - 0.7).abs() < f32::EPSILON);
        assert!((h.min().unwrap_or(0.0) - 0.1).abs() < f32::EPSILON);
    }

    #[test]
    fn load_auto_floors_at_ten_percent() {
        let mut h = History::with_capacity(8);
        h.push(0.012);
        h.push(0.018);
        let range = h.range(Scale::LOAD);
        assert!((range.min - 0.0).abs() < f32::EPSILON);
        assert!((range.max - 0.10).abs() < 1e-5);
    }

    #[test]
    fn temp_band_zooms_to_the_window() {
        let mut h = History::with_capacity(8);
        h.push(38.0);
        h.push(42.0);
        h.push(40.0);
        let range = h.range(Scale::TEMP);
        assert!(range.min <= 35.0, "{range:?}");
        assert!(range.max >= 45.0, "{range:?}");
        assert!(range.span() >= 12.0);
    }

    #[test]
    fn smooth_capacity_is_3600() {
        let mut h = History::for_period(crate::config::INTERVAL_FAST);
        for i in 0..3601 {
            h.push(i as f32);
        }
        assert_eq!(h.len(), 3600);
        assert!((h.last().unwrap_or(0.0) - 3600.0).abs() < f32::EPSILON);
        assert!((h.iter().next().unwrap_or(0.0) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn capacities_share_the_history_window() {
        use crate::config::{
            HISTORY_WINDOW, INTERVAL_DEFAULT, INTERVAL_FAST, INTERVAL_MID, SENSOR_PERIOD,
        };

        assert_eq!(capacity_for(INTERVAL_FAST), 3600);
        assert_eq!(capacity_for(INTERVAL_MID), 1800);
        assert_eq!(capacity_for(SENSOR_PERIOD), 450);
        assert_eq!(
            u128::from(capacity_for(INTERVAL_DEFAULT) as u32) * INTERVAL_DEFAULT.as_millis(),
            HISTORY_WINDOW.as_millis()
        );
        assert_eq!(
            u128::from(capacity_for(SENSOR_PERIOD) as u32) * SENSOR_PERIOD.as_millis(),
            HISTORY_WINDOW.as_millis()
        );
    }

    #[test]
    fn set_capacity_keeps_the_newest_samples() {
        let mut h = History::with_capacity(4);
        for v in [1.0, 2.0, 3.0, 4.0] {
            h.push(v);
        }
        h.set_capacity(2);
        assert_eq!(h.len(), 2);
        assert!((h.iter().next().unwrap_or(0.0) - 3.0).abs() < f32::EPSILON);
        assert!((h.last().unwrap_or(0.0) - 4.0).abs() < f32::EPSILON);
    }

    /// Deterministic LCG in 0..1 so sweeps are repeatable.
    fn noise(seed: &mut u64) -> f32 {
        *seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((*seed >> 40) % 10_000) as f32 / 10_000.0
    }

    fn filled(capacity: usize, len: usize, seed: u64) -> History {
        let mut seed = seed;
        let mut h = History::with_capacity(capacity);
        for _ in 0..len {
            h.push(noise(&mut seed) * 100.0);
        }
        h
    }

    #[test]
    fn capacity_is_never_exceeded() {
        for capacity in [1_usize, 2, 3, 7, 64, 450] {
            let mut h = History::with_capacity(capacity);
            for i in 0..capacity * 3 + 5 {
                h.push(i as f32);
                assert!(h.len() <= capacity);
                assert_eq!(h.len(), (i + 1).min(capacity));
                assert!((h.last().unwrap() - i as f32).abs() < f32::EPSILON);
            }
        }
    }

    #[test]
    fn zero_capacity_is_one() {
        let mut h = History::with_capacity(0);
        h.push(1.0);
        h.push(2.0);
        assert_eq!(h.len(), 1);
        assert_eq!(h.last(), Some(2.0));
        h.set_capacity(0);
        assert_eq!(h.len(), 1);
        h.push(3.0);
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn non_finite_samples_are_dropped() {
        let mut h = History::with_capacity(8);
        h.push(0.5);
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -f32::NAN] {
            h.push(bad);
        }
        assert_eq!(h.len(), 1);
        assert_eq!(h.max(), Some(0.5));
        let range = h.range(Scale::TEMP);
        assert!(range.min.is_finite() && range.max.is_finite());
    }

    #[test]
    fn negative_samples_floor_at_zero() {
        let mut h = History::with_capacity(4);
        h.push(-5.0);
        h.push(-0.0);
        h.push(f32::MIN);
        assert!(h.iter().all(|v| v >= 0.0 && v.is_finite()));
        assert_eq!(h.min(), Some(0.0));
    }

    #[test]
    fn empty_history_answers_none_and_empty() {
        let h = History::with_capacity(16);
        assert!(h.is_empty());
        assert_eq!(h.last(), None);
        assert_eq!(h.max(), None);
        assert_eq!(h.min(), None);
        assert!(
            h.buckets(Duration::from_secs(60), 10, 0)
                .iter()
                .all(Option::is_none)
        );
    }

    #[test]
    fn nice_ceiling_is_a_1_2_5_step_at_or_above() {
        let mut v = 1e-4_f32;
        while v < 1e9 {
            let c = nice_ceiling(v);
            assert!(c >= v * (1.0 - 1e-6), "{v} -> {c}");
            assert!(c <= v * 10.0 + f32::EPSILON, "{v} -> {c} overshoots");
            let exp = c.log10().floor();
            let mantissa = c / 10.0_f32.powf(exp);
            let near = |m: f32| (mantissa - m).abs() < 1e-3;
            assert!(
                near(1.0) || near(2.0) || near(5.0) || near(10.0),
                "{v} -> {c} mantissa {mantissa}"
            );
            v *= 1.13;
        }
    }

    #[test]
    fn nice_ceiling_of_a_nice_value_is_itself() {
        for exp in -3..=7 {
            for m in [1.0_f32, 2.0, 5.0] {
                let v = m * 10.0_f32.powi(exp);
                let c = nice_ceiling(v);
                assert!((c - v).abs() <= v * 1e-5, "{v} -> {c}");
            }
        }
    }

    #[test]
    fn nice_ceiling_degenerate_inputs_are_one() {
        for v in [0.0, -0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!((nice_ceiling(v) - 1.0).abs() < f32::EPSILON, "{v}");
        }
    }

    #[test]
    fn auto_scale_never_drops_below_the_floor() {
        for floor in [0.1_f32, 1.0, 500.0] {
            let mut h = History::with_capacity(8);
            assert!(h.range(Scale::Auto { floor }).max >= floor);
            h.push(0.0);
            assert!(h.range(Scale::Auto { floor }).max >= floor);
            h.push(floor * 3.3);
            let range = h.range(Scale::Auto { floor });
            assert!(range.max >= floor * 3.3, "{range:?}");
            assert!((range.min - 0.0).abs() < f32::EPSILON);
            assert!((Scale::Auto { floor }.resolve(0.0) - nice_ceiling(floor)).abs() < 1e-3);
        }
    }

    #[test]
    fn auto_scale_covers_every_sample_sweep() {
        for seed in 0..80_u64 {
            let mut s = seed;
            let mut h = History::with_capacity(64);
            let magnitude = 10.0_f32.powi((seed % 9) as i32 - 2);
            for _ in 0..64 {
                h.push(noise(&mut s) * magnitude);
            }
            let range = h.range(Scale::LOAD);
            assert!(range.max >= h.max().unwrap(), "seed {seed} {range:?}");
            assert!(range.max >= 0.10 - 1e-6);
        }
    }

    #[test]
    fn band_scale_invariants_sweep() {
        for seed in 0..120_u64 {
            let mut s = seed;
            let base = 20.0 + noise(&mut s) * 80.0;
            let swing = noise(&mut s) * 30.0;
            let mut h = History::with_capacity(32);
            for _ in 0..=(seed as usize % 32) {
                h.push(base + noise(&mut s) * swing);
            }
            let range = h.range(Scale::TEMP);
            assert!(range.min >= 0.0, "{range:?}");
            assert!(range.max > range.min, "{range:?}");
            assert!(range.span() >= 12.0 - 1e-3, "{range:?}");
            assert!(range.min <= h.min().unwrap(), "{range:?}");
            assert!(range.max >= h.max().unwrap(), "{range:?}");
            assert!((range.min % 5.0).abs() < 1e-3, "min snaps to 5: {range:?}");
            assert!((range.max % 5.0).abs() < 1e-3, "max snaps to 5: {range:?}");
        }
    }

    #[test]
    fn band_step_follows_the_band_unit() {
        assert!(
            (band_step(12.0) - 5.0).abs() < 1e-6,
            "temperatures keep fives"
        );
        assert!(
            (band_step(0.10) - 0.05).abs() < 1e-6,
            "a charge band steps 5%"
        );
        assert!((band_step(4.0) - 2.0).abs() < 1e-6);
        assert!((band_step(0.0) - 5.0).abs() < 1e-6);
        assert!((band_step(f32::NAN) - 5.0).abs() < 1e-6);
    }

    #[test]
    fn fractional_band_zooms_instead_of_snapping_to_five() {
        // Regression: a battery draining 90% -> 82% drew as a flat line under
        // a `500%` label, because the band snapped its fraction edges to 0..5.
        let charge = Scale::Band {
            pad: 0.02,
            min_span: 0.10,
        };
        let mut h = History::with_capacity(16);
        for i in 0..16 {
            h.push(0.90 - 0.08 * i as f32 / 15.0);
        }
        let range = h.range(charge);
        assert!(range.max <= 1.0 && range.min >= 0.70, "{range:?}");
        assert!(range.span() >= 0.10 - 1e-4, "{range:?}");
        assert!(range.min <= 0.82 && range.max >= 0.90, "{range:?}");
    }

    #[test]
    fn band_scale_clamped_at_zero_keeps_min_span() {
        // Regression: a band clamped at 0 °C came out 10 wide, under the 12 floor.
        let mut h = History::with_capacity(4);
        h.push(0.0);
        h.push(1.0);
        let range = h.range(Scale::TEMP);
        assert!((range.min - 0.0).abs() < f32::EPSILON);
        assert!(range.max >= 12.0);
        let empty = History::with_capacity(4).range(Scale::TEMP);
        assert!(empty.min >= 0.0 && empty.max > empty.min);
    }

    #[test]
    fn fixed_scale_ignores_the_data() {
        let h = filled(16, 16, 3);
        let range = h.range(Scale::Fixed(1.0));
        assert!((range.min - 0.0).abs() < f32::EPSILON);
        assert!((range.max - 1.0).abs() < f32::EPSILON);
        let zero = h.range(Scale::Fixed(0.0));
        assert!(zero.max > 0.0, "a zero max would divide by zero");
        assert!(zero.span() > 0.0);
        assert!(Scale::Fixed(0.0).resolve(5.0) > 0.0);
    }

    #[test]
    fn hints_axis_only_for_moving_scales() {
        assert!(!Scale::Fixed(1.0).hints_axis());
        assert!(Scale::LOAD.hints_axis());
        assert!(Scale::TEMP.hints_axis());
        assert!(Scale::FAN.hints_axis());
    }

    #[test]
    fn capacity_for_edges() {
        let window = HISTORY_WINDOW.as_millis() as usize;
        assert_eq!(capacity_for(Duration::ZERO), window);
        assert_eq!(capacity_for(Duration::from_micros(10)), window);
        assert_eq!(capacity_for(Duration::from_hours(24)), 1);
        assert_eq!(capacity_for(Duration::MAX), 1);
        assert_eq!(capacity_for(HISTORY_WINDOW), 1);
    }

    #[test]
    fn set_capacity_grow_keeps_everything() {
        let mut h = History::for_period(Duration::from_secs(1));
        h.set_capacity(3);
        for v in [1.0, 2.0, 3.0] {
            h.push(v);
        }
        h.set_capacity(10);
        assert_eq!(h.len(), 3);
        for v in 4..=12 {
            h.push(v as f32);
        }
        assert_eq!(h.len(), 10);
        assert_eq!(h.iter().next(), Some(3.0));
    }

    const MS250: Duration = Duration::from_millis(250);
    const S2: Duration = Duration::from_secs(2);

    fn stepped(period: Duration, until: Stamp, step_at: Stamp) -> History {
        let mut h = History::for_period(period);
        let step = millis(period);
        let mut t = 0;
        while t <= until {
            h.push_at(if t >= step_at { 1.0 } else { 0.0 }, t);
            t += step;
        }
        h
    }

    fn first_high(buckets: &[Option<Bucket>]) -> Option<usize> {
        buckets.iter().position(|b| b.is_some_and(|b| b.mean > 0.5))
    }

    #[test]
    fn step_lands_in_the_same_column_across_cadences() {
        for (span, n) in [
            (Duration::from_mins(1), 80_usize),
            (Duration::from_mins(5), 200),
            (Duration::from_mins(15), 120),
        ] {
            let now = 600_000;
            let fast = stepped(MS250, now, 570_000).buckets(span, n, now);
            let slow = stepped(S2, now, 570_000).buckets(span, n, now);
            let (a, b) = (first_high(&fast).unwrap(), first_high(&slow).unwrap());
            assert!(a.abs_diff(b) <= 1, "{span:?}/{n}: {a} vs {b}");
        }
    }

    #[test]
    fn one_more_bucket_shifts_every_older_column_by_one() {
        let mut seed = 7;
        let mut h = History::for_period(MS250);
        let span = Duration::from_mins(1);
        let n = 60;
        let width = millis(span) / n as u64;
        let mut t = 0;
        while t < 90_000 {
            h.push_at(noise(&mut seed), t);
            t += 250;
        }
        let now = h.newest().unwrap();
        let before = h.buckets(span, n, now);
        for _ in 0..width / 250 {
            h.push_at(noise(&mut seed), t);
            t += 250;
        }
        let after = h.buckets(span, n, now + width);
        assert_eq!(&after[..n - 1], &before[1..], "older columns re-binned");
    }

    #[test]
    fn live_push_shifts_left_without_rebinning() {
        let mut h = History::with_capacity(64);
        for v in [0.1, 0.5, 0.9, 0.3] {
            h.push(v);
        }
        let axis = TimeAxis::live(&h);
        let before = h.buckets(axis.span(8), 8, axis.now);
        h.push(0.7);
        let axis = TimeAxis::live(&h);
        let after = h.buckets(axis.span(8), 8, axis.now);
        assert_eq!(&after[..7], &before[1..]);
        assert_eq!(after[7].map(|b| b.mean), Some(0.7));
        assert_eq!(after.iter().flatten().count(), 5);
    }

    #[test]
    fn interval_change_keeps_the_window_time_correct() {
        // Regression D21: 250 -> 500 ms dropped half the history, 1000 -> 250
        // stretched the window to ~26 min.
        let mut h = History::for_period(MS250);
        let mut t = 0;
        while t < 60_000 {
            h.push_at(1.0, t);
            t += 250;
        }
        let kept = h.len();
        h.set_period(Duration::from_millis(500));
        assert_eq!(h.len(), kept, "changing the interval dropped history");
        while t < 120_000 {
            h.push_at(1.0, t);
            t += 500;
        }
        let now = h.newest().unwrap();
        let span = Duration::from_mins(5);
        let buckets = h.buckets(span, 100, now);
        let filled = buckets.iter().flatten().count();
        // 120 s of data in 3 s buckets
        assert!((39..=41).contains(&filled), "{filled}");
    }

    #[test]
    fn slower_interval_ages_out_by_time_not_count() {
        let mut h = History::for_period(Duration::from_secs(1));
        h.set_period(MS250);
        let mut t = 0;
        while t <= 20 * 60_000 {
            h.push_at(1.0, t);
            t += 250;
        }
        let oldest = h.newest().unwrap() - millis(HISTORY_WINDOW);
        assert!(h.since(0).all(|(t, _)| t >= oldest));
        assert_eq!(h.len(), capacity_for(MS250));
    }

    #[test]
    fn push_at_never_goes_backwards() {
        let mut h = History::for_period(MS250);
        h.push_at(1.0, 5_000);
        h.push_at(2.0, 1_000);
        assert_eq!(h.newest(), Some(5_000));
        assert_eq!(h.len(), 2);
    }

    #[test]
    fn auto_scale_follows_the_visible_peak() {
        // Regression D66: a 5% load drawn under a 15-minute-old 100% ceiling.
        let mut h = History::for_period(Duration::from_secs(1));
        h.push_at(1.0, 0);
        let mut t = 1_000;
        while t <= 600_000 {
            h.push_at(0.05, t);
            t += 1_000;
        }
        let now = h.newest().unwrap();
        let visible = Scale::LOAD.range_in(&h, Duration::from_mins(1), now);
        assert!((visible.max - 0.10).abs() < 1e-5, "{visible:?}");
        assert!((h.range(Scale::LOAD).max - 1.0).abs() < 1e-5);
    }

    #[test]
    fn auto_scale_eases_down_over_thirty_seconds() {
        let span = Duration::from_mins(1);
        let mut h = History::for_period(Duration::from_secs(1));
        h.push_at(0.9, 0);
        let mut t = 1_000;
        while t <= 200_000 {
            h.push_at(0.02, t);
            t += 1_000;
        }
        let at = |now: Stamp| Scale::LOAD.range_in(&h, span, now).max;
        let gone = millis(span);
        assert!((at(gone) - 1.0).abs() < 1e-5, "still in view");
        let mut prev = f32::MAX;
        for s in 0..=35 {
            let v = at(gone + 1 + s * 1_000);
            assert!(v <= prev + 1e-6, "ease must never climb: {s}s {v}");
            prev = v;
        }
        let half = at(gone + 15_000);
        assert!((0.4..=0.6).contains(&half), "halfway {half}");
        assert!((at(gone + 30_001) - 0.10).abs() < 1e-5);
    }

    #[test]
    fn band_scale_uses_the_visible_window() {
        let mut h = History::for_period(S2);
        h.push_at(90.0, 0);
        let mut t = 2_000;
        while t <= 400_000 {
            h.push_at(41.0, t);
            t += 2_000;
        }
        let r = Scale::TEMP.range_in(&h, Duration::from_mins(1), h.newest().unwrap());
        assert!(r.max < 60.0, "{r:?}");
        assert!(r.min <= 41.0 && r.max >= 41.0);
    }

    #[test]
    fn slow_series_holds_between_its_samples() {
        let mut h = History::for_period(S2);
        for (i, v) in [0.2, 0.4, 0.6].into_iter().enumerate() {
            h.push_at(v, i as u64 * 2_000);
        }
        let b = h.buckets(Duration::from_secs(5), 20, 4_000);
        // 250 ms buckets: every one from the first sample on is filled
        assert!(b[..3].iter().all(Option::is_none), "{b:?}");
        assert!(b[3..].iter().all(Option::is_some), "{b:?}");
        assert_eq!(b[3].map(|b| b.mean), Some(0.2));
        assert_eq!(b[19].map(|b| b.mean), Some(0.6));
    }

    #[test]
    fn a_silent_series_goes_blank_after_two_periods() {
        let mut h = History::for_period(Duration::from_secs(1));
        h.push_at(0.5, 0);
        let b = h.buckets(Duration::from_secs(10), 10, 9_999);
        let filled: Vec<bool> = b.iter().map(Option::is_some).collect();
        assert_eq!(
            filled,
            [
                true, true, true, false, false, false, false, false, false, false
            ]
        );
    }

    #[test]
    fn bucket_keeps_mean_and_max() {
        let mut h = History::for_period(MS250);
        for (i, v) in [0.2_f32, 1.0, 0.0, 0.2].into_iter().enumerate() {
            h.push_at(v, i as u64 * 250);
        }
        let b = h.buckets(Duration::from_secs(1), 1, 999);
        let bucket = b[0].unwrap();
        assert!((bucket.mean - 0.35).abs() < 1e-6);
        assert!((bucket.max - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn buckets_count_and_bounds_sweep() {
        for seed in 0..40_u64 {
            let mut s = seed;
            let mut h = History::for_period(MS250);
            let len = 1 + (seed as usize * 131) % 3_000;
            for i in 0..len {
                h.push_at(noise(&mut s), i as u64 * 250 + seed % 200);
            }
            let (lo, hi) = (h.min().unwrap(), h.max().unwrap());
            let now = h.newest().unwrap();
            for zoom in Zoom::ALL.into_iter().chain([Zoom::Samples]) {
                for n in [0_usize, 1, 7, 80, 400] {
                    let span = zoom.span(MS250, n);
                    let b = h.buckets(span, n, now);
                    assert_eq!(b.len(), n);
                    for bucket in b.iter().flatten() {
                        assert!(bucket.mean >= lo - 1e-5 && bucket.max <= hi + 1e-5);
                        assert!(bucket.mean <= bucket.max + 1e-5);
                    }
                    if n > 0 {
                        assert!(b[n - 1].is_some(), "newest column is never blank");
                    }
                }
            }
        }
    }

    #[test]
    fn young_clock_pads_on_the_left() {
        let mut h = History::for_period(Duration::from_secs(1));
        h.push_at(0.3, 0);
        h.push_at(0.6, 1_000);
        let b = h.buckets(Duration::from_secs(10), 10, 1_000);
        assert_eq!(b[9].map(|b| b.mean), Some(0.6));
        assert_eq!(b[8].map(|b| b.mean), Some(0.3));
        assert!(b[..8].iter().all(Option::is_none));
    }

    #[test]
    fn sample_clock_snaps_jitter_to_the_cadence() {
        let mut clock = SampleClock::default();
        let stamps: Vec<Stamp> = [240_u64, 510, 740, 1_010, 1_260]
            .into_iter()
            .map(|ms| clock.stamp(Duration::from_millis(ms), MS250))
            .collect();
        assert_eq!(stamps, [250, 500, 750, 1_000, 1_250]);
    }

    #[test]
    fn sample_clock_never_repeats_a_stamp() {
        let mut clock = SampleClock::default();
        let a = clock.stamp(Duration::from_millis(1_000), MS250);
        let b = clock.stamp(Duration::from_millis(1_010), MS250);
        let c = clock.stamp(Duration::from_millis(900), MS250);
        assert!(a < b && b < c, "{a} {b} {c}");
    }

    #[test]
    fn zoom_cycles_through_every_level() {
        let mut z = Zoom::Live;
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(z.label());
            z = z.next();
        }
        assert_eq!(z, Zoom::Live);
        assert_eq!(seen, ["live", "5m", "15m", "1m"]);
        assert_eq!(Zoom::Samples.next(), Zoom::FiveMin);
    }

    #[test]
    fn zoom_ladder_widens_until_it_wraps() {
        let spans: Vec<_> = Zoom::ALL.iter().map(|z| z.span(MS250, 80)).collect();
        assert!(spans.is_sorted(), "{spans:?}");
        assert_eq!(Zoom::ALL.map(Zoom::next)[..3], Zoom::ALL[1..]);
    }

    #[test]
    fn zoom_span_is_clamped_to_the_window() {
        assert_eq!(Zoom::Samples.span(MS250, 80), Duration::from_secs(20));
        assert_eq!(Zoom::Samples.span(S2, 100_000), HISTORY_WINDOW);
        assert_eq!(Zoom::FiveMin.span(MS250, 3), Duration::from_mins(5));
        assert!(Zoom::Samples.span(Duration::ZERO, 0) > Duration::ZERO);
    }

    #[test]
    fn live_zoom_is_one_window_at_any_width() {
        for dots in [1, 68, 138, 240, 10_000] {
            assert_eq!(Zoom::Live.span(MS250, dots), LIVE_WINDOW);
            assert_eq!(Zoom::Live.span(S2, dots), LIVE_WINDOW);
        }
    }

    #[test]
    fn with_capacity_spreads_over_the_window() {
        let h = History::with_capacity(450);
        assert_eq!(h.period(), SENSOR_PERIOD);
        let mut h = History::with_capacity(4);
        for v in 0..10 {
            h.push(v as f32);
        }
        assert_eq!(h.len(), 4);
    }
}
