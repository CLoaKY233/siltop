//! Questions asked of long history: a picked range, graph columns on one
//! shared time axis, the scrub cursor's bucket and its blame card, notable
//! events, and the totals `siltop report` prints.
//!
//! Pure like the rest of the store: every call takes unix seconds (and the
//! local UTC offset where "today" matters), so tests drive time.

use std::collections::HashMap;

use super::bucket::{Bucket, TopProc};
use super::tier::{Tier, merge};
use super::{HistoryStore, Series};
use crate::day::Day;
use crate::error::Result;
use crate::format::{bytes_short, celsius};
use crate::snapshot::Thermal;

/// A time range the timeline and reports can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Span {
    #[default]
    Hour,
    /// Since local midnight.
    Today,
    Day,
    Week,
    Month,
}

impl Span {
    pub const ALL: [Self; 5] = [Self::Hour, Self::Today, Self::Day, Self::Week, Self::Month];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Hour => "1 h",
            Self::Today => "today",
            Self::Day => "24 h",
            Self::Week => "7 d",
            Self::Month => "30 d",
        }
    }

    /// The resolution that answers this span: Day (6 s) up to a day, Month
    /// (144 s) beyond, so a 30-day query reads ~18 000 buckets, not 432 000.
    #[must_use]
    pub const fn tier(self) -> Tier {
        match self {
            Self::Hour | Self::Today | Self::Day => Tier::Day,
            Self::Week | Self::Month => Tier::Month,
        }
    }

    /// `from..to` in unix seconds ending just after `now`; `utc_offset_s` is
    /// the local zone's offset east of UTC, used only by [`Span::Today`].
    #[must_use]
    pub fn bounds(self, now: u64, utc_offset_s: i64) -> (u64, u64) {
        let to = now.saturating_add(1);
        let back = |s: u64| now.saturating_sub(s);
        let from = match self {
            Self::Hour => back(3600),
            Self::Day => back(24 * 3600),
            Self::Week => back(7 * 24 * 3600),
            Self::Month => back(30 * 24 * 3600),
            Self::Today => {
                let local = i128::from(now) + i128::from(utc_offset_s);
                let midnight = local - local.rem_euclid(86_400) - i128::from(utc_offset_s);
                u64::try_from(midnight.clamp(0, i128::from(now))).unwrap_or(0)
            }
        };
        (from, to)
    }

    /// The neighbouring span, clamped at both ends (no wrap: → at 30 d stays).
    #[must_use]
    pub fn step(self, delta: i32) -> Self {
        let i = Self::ALL.iter().position(|s| *s == self).unwrap_or(0);
        let last = Self::ALL.len() - 1;
        let next = i
            .saturating_add_signed(isize::try_from(delta).unwrap_or(0))
            .min(last);
        Self::ALL[next]
    }
}

/// The column of `width` that unix second `t` falls in, on the shared axis
/// `from..to`; `None` outside it.
#[must_use]
pub fn column_of(t: u64, from: u64, to: u64, width: usize) -> Option<usize> {
    if width == 0 || t < from || t >= to {
        return None;
    }
    let col = u128::from(t - from) * width as u128 / u128::from(to - from);
    usize::try_from(col).ok()
}

/// The first unix second column `col` covers; the inverse of [`column_of`].
#[must_use]
pub fn column_start(col: usize, from: u64, to: u64, width: usize) -> u64 {
    if width == 0 {
        return from;
    }
    let span = u128::from(to.saturating_sub(from));
    // Ceiling, so column_of(column_start(c)) == c for every column.
    let off = (col as u128 * span).div_ceil(width as u128);
    from.saturating_add(u64::try_from(off).unwrap_or(u64::MAX))
}

/// `buckets` (oldest first, all of `tier`) merged into `width` columns on
/// `from..to`: one bucket per column holding samples, `None` where nothing
/// was recorded. Merging reuses the tier maths: weighted means, true maxes,
/// re-ranked tops. A column narrower than a bucket (today just after
/// midnight) that no bucket starts in shows the bucket still covering its
/// start, so contiguous history never reads as a comb of gaps.
#[must_use]
pub fn bin(
    buckets: &[Bucket],
    tier: Tier,
    from: u64,
    to: u64,
    width: usize,
) -> Vec<Option<Bucket>> {
    let mut groups: Vec<Vec<Bucket>> = vec![Vec::new(); width];
    for b in buckets {
        if let Some(group) = column_of(b.t, from, to, width).and_then(|c| groups.get_mut(c)) {
            group.push(b.clone());
        }
    }
    let span = tier.stored().span_s();
    groups
        .iter()
        .enumerate()
        .map(|(col, group)| {
            let start = column_start(col, from, to, width);
            if !group.is_empty() {
                return merge(group, start);
            }
            let covering = buckets.partition_point(|b| b.t <= start).checked_sub(1)?;
            let b = buckets.get(covering)?;
            (start < b.t.saturating_add(span))
                .then(|| merge(std::slice::from_ref(b), start))
                .flatten()
        })
        .collect()
}

/// One series' per-column values (mean, or max when `peak`), for a graph.
#[must_use]
pub fn values(columns: &[Option<Bucket>], series: Series, peak: bool) -> Vec<Option<f32>> {
    columns
        .iter()
        .map(|c| {
            c.as_ref()
                .and_then(|b| b.get(series))
                .map(|s| if peak { s.max } else { s.mean })
        })
        .collect()
}

/// Whether any column recorded `series`: absent hardware hides its row.
#[must_use]
pub fn present(columns: &[Option<Bucket>], series: Series) -> bool {
    columns.iter().flatten().any(|b| b.get(series).is_some())
}

/// The newest column holding data: where the scrub cursor starts.
#[must_use]
pub fn last_filled(columns: &[Option<Bucket>]) -> Option<usize> {
    columns.iter().rposition(Option::is_some)
}

/// Swap growing by at least this much between neighbouring columns is news.
pub const SWAP_STEP_BYTES: f32 = 256.0 * 1024.0 * 1024.0;
/// A CPU peak at or above this is "running hot".
pub const HOT_C: f32 = 95.0;

/// Something worth pointing at on the timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// Swap grew by this many bytes since the previous recorded column.
    SwapGrew(f32),
    /// The CPU crossed [`HOT_C`], peaking here.
    RanHot(f32),
    /// Recording resumed after a gap of this many seconds (app closed,
    /// display paused, Mac asleep).
    Resumed(u64),
    /// The Mac went on battery.
    Unplugged,
    /// The Mac went back on AC.
    PluggedIn,
    /// The thermal state moved to this level (0 nominal ..= 3 critical).
    Thermal(u8),
    /// The battery fell below [`LOW_BATTERY_PCT`], to this percent.
    BatteryLow(f32),
}

/// The battery charge "running low" crosses.
pub const LOW_BATTERY_PCT: f32 = 20.0;

impl Event {
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::SwapGrew(by) => format!("swap grew +{}", bytes_short(by.max(0.0) as u64)),
            Self::RanHot(c) => format!("CPU ran hot, {}", celsius(c)),
            Self::Resumed(gap) => format!("resumed after {}", duration(gap)),
            Self::Unplugged => "unplugged, on battery".to_owned(),
            Self::PluggedIn => "plugged in".to_owned(),
            Self::Thermal(0) => "thermal state back to nominal".to_owned(),
            Self::Thermal(level) => format!("thermal state {}", Thermal::from_level(level).name()),
            Self::BatteryLow(pct) => format!("battery below {LOW_BATTERY_PCT:.0}%, {pct:.0}%"),
        }
    }
}

/// Short human duration: `45 s`, `12 min`, `3 h 5 min`, `2 d 4 h`.
#[must_use]
pub fn duration(s: u64) -> String {
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min", s / 60),
        3600..86_400 => match (s % 3600) / 60 {
            0 => format!("{} h", s / 3600),
            m => format!("{} h {m} min", s / 3600),
        },
        _ => match (s % 86_400) / 3600 {
            0 => format!("{} d", s / 86_400),
            h => format!("{} d {h} h", s / 86_400),
        },
    }
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Local wall-clock `14:32`, or `Sep 21 14:32` with `dated`, for unix
/// second `t` in a zone `utc_offset_s` east of UTC.
#[must_use]
pub fn clock_label(t: u64, utc_offset_s: i64, dated: bool) -> String {
    let local = i128::from(t) + i128::from(utc_offset_s);
    let secs = local.rem_euclid(86_400);
    let hm = format!("{:02}:{:02}", secs / 3600, (secs % 3600) / 60);
    if !dated {
        return hm;
    }
    let days = i32::try_from(local.div_euclid(86_400)).unwrap_or(0);
    let (_, month, day) = Day::from_days(days).ymd();
    let name = MONTHS
        .get(month.saturating_sub(1) as usize)
        .copied()
        .unwrap_or("");
    format!("{name} {day} {hm}")
}

/// Events per column, oldest first. A gap is at least one empty column
/// between two recorded ones, so it scales with the range shown.
#[must_use]
pub fn events(columns: &[Option<Bucket>]) -> Vec<(usize, Event)> {
    let mut out = Vec::new();
    let mut prev: Option<(usize, &Bucket)> = None;
    for (col, bucket) in columns.iter().enumerate() {
        let Some(b) = bucket else { continue };
        if prev.is_some_and(|(prev_col, p)| col == prev_col + 1 && same_readings(p, b)) {
            // One bucket spread over narrow columns: nothing new happened.
            prev = Some((col, b));
            continue;
        }
        if let Some((prev_col, p)) = prev {
            if col > prev_col + 1 {
                out.push((col, Event::Resumed(b.t.saturating_sub(p.t))));
            }
            if let (Some(was), Some(now)) = (p.get(Series::SwapBytes), b.get(Series::SwapBytes)) {
                let grew = now.max - was.mean;
                if grew >= SWAP_STEP_BYTES {
                    out.push((col, Event::SwapGrew(grew)));
                }
            }
        }
        if let Some((_, p)) = prev {
            state_events(p, b, |e| out.push((col, e)));
        }
        if let Some(temp) = b.get(Series::CpuTempC) {
            let was_hot = prev
                .and_then(|(_, p)| p.get(Series::CpuTempC))
                .is_some_and(|s| s.max >= HOT_C);
            if temp.max >= HOT_C && !was_hot {
                out.push((col, Event::RanHot(temp.max)));
            }
        }
        prev = Some((col, b));
    }
    out
}

/// Whether two columns hold the same record (a bucket wider than a column
/// shown twice).
fn same_readings(a: &Bucket, b: &Bucket) -> bool {
    a.samples == b.samples && a.series == b.series
}

/// Whether a bucket was mostly on AC; `None` where not recorded.
fn on_ac(b: &Bucket) -> Option<bool> {
    b.get(Series::OnAc).map(|s| s.mean >= 0.5)
}

/// A bucket's thermal level: its worst moment, so a brief serious spell
/// inside a coarse column still shows.
fn thermal_of(b: &Bucket) -> Option<u8> {
    b.get(Series::ThermalState)
        .map(|s| s.max.round().clamp(0.0, 3.0) as u8)
}

/// Power-source, thermal-state and low-battery changes from `p` to `b`.
fn state_events(p: &Bucket, b: &Bucket, mut push: impl FnMut(Event)) {
    match (on_ac(p), on_ac(b)) {
        (Some(true), Some(false)) => push(Event::Unplugged),
        (Some(false), Some(true)) => push(Event::PluggedIn),
        _ => {}
    }
    if let (Some(was), Some(now)) = (thermal_of(p), thermal_of(b))
        && was != now
    {
        push(Event::Thermal(now));
    }
    if let (Some(was), Some(now)) = (p.get(Series::BatteryPct), b.get(Series::BatteryPct))
        && was.mean >= LOW_BATTERY_PCT
        && now.mean < LOW_BATTERY_PCT
    {
        push(Event::BatteryLow(now.mean));
    }
}

/// One app in a blame list: same-named processes (a build's many `rustc`)
/// folded into one row.
#[derive(Debug, Clone, PartialEq)]
pub struct Named {
    pub name: String,
    /// Summed over the folded processes.
    pub value: f32,
    /// How many processes were folded in.
    pub count: usize,
}

/// `list` folded by name, summed, highest first (ties by name), top `n`.
#[must_use]
pub fn by_name(list: &[TopProc], n: usize) -> Vec<Named> {
    let mut out: Vec<Named> = Vec::new();
    for p in list {
        match out.iter_mut().find(|a| a.name == p.name) {
            Some(a) => {
                a.value += p.value;
                a.count += 1;
            }
            None => out.push(Named {
                name: p.name.clone(),
                value: p.value,
                count: 1,
            }),
        }
    }
    out.sort_by(|a, b| {
        b.value
            .total_cmp(&a.value)
            .then_with(|| a.name.cmp(&b.name))
    });
    out.truncate(n);
    out
}

/// What the scrub cursor's column says: who was busy and what happened.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Blame {
    pub cpu: Vec<Named>,
    pub energy: Vec<Named>,
    pub mem: Vec<Named>,
    pub events: Vec<Event>,
}

impl Blame {
    /// The top `n` of each list in `column`, plus that column's events.
    #[must_use]
    pub fn at(columns: &[Option<Bucket>], events: &[(usize, Event)], col: usize, n: usize) -> Self {
        let Some(Some(b)) = columns.get(col) else {
            return Self::default();
        };
        let top = |list: &[TopProc]| by_name(list, n);
        Self {
            cpu: top(&b.top_cpu),
            energy: if b.apps_other.is_some() {
                b.apps.iter().take(n).cloned().collect()
            } else {
                top(&b.top_energy)
            },
            mem: top(&b.top_mem),
            events: events
                .iter()
                .filter(|(c, _)| *c == col)
                .map(|(_, e)| *e)
                .collect(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cpu.is_empty()
            && self.energy.is_empty()
            && self.mem.is_empty()
            && self.events.is_empty()
    }
}

/// One app's share of the energy leaderboard.
#[derive(Debug, Clone, PartialEq)]
pub struct AppEnergy {
    pub name: String,
    pub wh: f64,
}

/// A value and the unix second it happened.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Peak {
    pub value: f32,
    pub t: u64,
}

/// Battery drain over the latest stretch on battery.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drain {
    /// When that stretch began (the unplug, or the range start).
    pub since: u64,
    /// Whether the stretch began with an unplug seen in the range.
    pub unplugged: bool,
    pub wh: f64,
    pub avg_w: f64,
    /// Charge at the start and end of the stretch, when recorded.
    pub from_pct: Option<f32>,
    pub to_pct: Option<f32>,
}

/// Battery health (maximum capacity) at the start and end of the range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HealthTrend {
    pub first: Peak,
    pub last: Peak,
}

/// Totals over a span of history for `siltop report`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Report {
    pub from: u64,
    pub to: u64,
    /// Seconds actually recorded (each bucket's coverage): gaps and the
    /// unrecorded rest of a partly filled bucket are not counted.
    pub recorded_s: u64,
    /// Apps by energy, highest first. Exact where buckets carry the per-app
    /// sums (store v2); v1 buckets fall back to their top-5 lists.
    pub energy: Vec<AppEnergy>,
    /// Every app outside the leaderboard, so leaderboard + other is the
    /// whole process energy; `None` without per-app sums.
    pub energy_other_wh: Option<f64>,
    /// Whole-system energy from the input rail, when the Mac reports it.
    pub system_wh: Option<f64>,
    pub peak_cpu_c: Option<Peak>,
    pub peak_gpu_c: Option<Peak>,
    pub peak_power_w: Option<Peak>,
    /// Seconds spent with the CPU at or above [`HOT_C`]: the share of each
    /// bucket's samples that were hot × its coverage.
    pub hot_s: u64,
    /// Seconds with the thermal state serious or worse; `None` when the
    /// thermal state was never recorded (v1 history).
    pub throttled_s: Option<u64>,
    /// Drain over the latest stretch on battery; `None` on desktops or
    /// when the range never ran on battery.
    pub drain: Option<Drain>,
    pub health: Option<HealthTrend>,
}

/// Drain over the newest run of on-battery buckets. Buckets with no power
/// source recorded neither extend nor break the run; `hours` is a bucket's
/// recorded time.
fn drain_of(buckets: &[Bucket], hours: impl Fn(&Bucket) -> f64) -> Option<Drain> {
    let tagged: Vec<(&Bucket, bool)> = buckets
        .iter()
        .filter_map(|b| on_ac(b).map(|ac| (b, ac)))
        .collect();
    let end = tagged.iter().rposition(|(_, ac)| !ac)?;
    let start = tagged[..end]
        .iter()
        .rposition(|(_, ac)| *ac)
        .map_or(0, |i| i + 1);
    let run = &tagged[start..=end];
    let draw: Vec<(f64, f64)> = run
        .iter()
        .filter_map(|(b, _)| b.get(Series::BatteryW).map(|s| (s, hours(b))))
        .map(|(s, h)| (f64::from(-s.mean).max(0.0) * h, h))
        .collect();
    if draw.is_empty() {
        return None;
    }
    let wh: f64 = draw.iter().map(|(wh, _)| wh).sum();
    let drawn_h: f64 = draw.iter().map(|(_, h)| h).sum();
    let pct = |b: &Bucket| b.get(Series::BatteryPct).map(|s| s.mean);
    Some(Drain {
        since: run[0].0.t,
        unplugged: start > 0,
        wh,
        avg_w: if drawn_h > 0.0 { wh / drawn_h } else { 0.0 },
        from_pct: run.iter().find_map(|(b, _)| pct(b)),
        to_pct: run.iter().rev().find_map(|(b, _)| pct(b)),
    })
}

fn health_of(buckets: &[Bucket]) -> Option<HealthTrend> {
    let mut points = buckets.iter().filter_map(|b| {
        b.get(Series::BatteryHealth).map(|s| Peak {
            value: s.mean,
            t: b.t,
        })
    });
    let first = points.next()?;
    Some(HealthTrend {
        first,
        last: points.next_back().unwrap_or(first),
    })
}

/// The share of a bucket spent at or above [`HOT_C`]: its Hot mean, or, for
/// a record older than that column, all of it when its peak got there.
fn hot_share(b: &Bucket) -> f64 {
    match b.get(Series::Hot) {
        Some(hot) => f64::from(hot.mean.clamp(0.0, 1.0)),
        None => f64::from(u8::from(
            b.get(Series::CpuTempC).is_some_and(|s| s.max >= HOT_C),
        )),
    }
}

fn peak_of(buckets: &[Bucket], series: Series) -> Option<Peak> {
    buckets
        .iter()
        .filter_map(|b| {
            b.get(series).map(|s| Peak {
                value: s.max,
                t: b.t,
            })
        })
        .filter(|p| p.value.is_finite())
        .reduce(|best, p| if p.value > best.value { p } else { best })
}

impl Report {
    /// Aggregates `buckets` (all of `tier`) over `from..to`, keeping the top
    /// `n` apps. Energy per bucket is its mean watts × its recorded time.
    #[must_use]
    pub fn build(buckets: &[Bucket], tier: Tier, from: u64, to: u64, n: usize) -> Self {
        let span = tier.stored().span_s();
        let seconds = |b: &Bucket| b.covered(span) as f64;
        let hours = |b: &Bucket| seconds(b) / 3600.0;
        let mut by_app: HashMap<&str, f64> = HashMap::new();
        let mut other: Option<f64> = None;
        for b in buckets {
            let hours = hours(b);
            if let Some(rest) = b.apps_other {
                *other.get_or_insert(0.0) += f64::from(rest) * hours;
                for app in &b.apps {
                    *by_app.entry(app.name.as_str()).or_default() += f64::from(app.value) * hours;
                }
            } else {
                for p in &b.top_energy {
                    *by_app.entry(p.name.as_str()).or_default() += f64::from(p.value) * hours;
                }
            }
        }
        let mut energy: Vec<AppEnergy> = by_app
            .into_iter()
            .filter(|(_, wh)| *wh > 0.0)
            .map(|(name, wh)| AppEnergy {
                name: name.to_owned(),
                wh,
            })
            .collect();
        energy.sort_by(|a, b| b.wh.total_cmp(&a.wh).then_with(|| a.name.cmp(&b.name)));
        if other.is_some() {
            let tail: f64 = energy.iter().skip(n).map(|a| a.wh).sum();
            other = other.map(|o| o + tail);
        }
        energy.truncate(n);
        let throttle: Vec<f64> = buckets
            .iter()
            .filter_map(|b| b.get(Series::Throttled).map(|s| (s, seconds(b))))
            .map(|(s, secs)| f64::from(s.mean.clamp(0.0, 1.0)) * secs)
            .collect();
        let system: Vec<f64> = buckets
            .iter()
            .filter_map(|b| {
                b.get(Series::SysPowerW)
                    .map(|s| f64::from(s.mean) * hours(b))
            })
            .collect();
        let hot: f64 = buckets.iter().map(|b| hot_share(b) * seconds(b)).sum();
        Self {
            from,
            to,
            recorded_s: buckets.iter().map(|b| b.covered(span)).sum(),
            energy,
            system_wh: (!system.is_empty()).then(|| system.iter().sum()),
            peak_cpu_c: peak_of(buckets, Series::CpuTempC),
            peak_gpu_c: peak_of(buckets, Series::GpuTempC),
            peak_power_w: peak_of(buckets, Series::SysPowerW),
            hot_s: hot.round() as u64,
            energy_other_wh: other,
            throttled_s: (!throttle.is_empty())
                .then(|| throttle.iter().sum::<f64>().round() as u64),
            drain: drain_of(buckets, hours),
            health: health_of(buckets),
        }
    }

    /// Queries `store` for `span` ending at `now` and aggregates it.
    pub fn query(
        store: &dyn HistoryStore,
        span: Span,
        now: u64,
        utc_offset_s: i64,
        n: usize,
    ) -> Result<Self> {
        let (from, to) = span.bounds(now, utc_offset_s);
        let buckets = store.buckets(from, to, span.tier())?;
        Ok(Self::build(&buckets, span.tier(), from, to, n))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.recorded_s == 0
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::super::{MemoryStore, Stat};
    use super::*;

    #[allow(clippy::unnecessary_wraps)] // reads as a series slot at call sites
    fn stat(v: f32) -> Option<Stat> {
        Some(Stat { mean: v, max: v })
    }

    fn proc(pid: u32, name: &str, value: f32) -> TopProc {
        TopProc {
            pid,
            name: name.into(),
            value,
        }
    }

    fn bucket(t: u64) -> Bucket {
        let mut b = Bucket::empty(t);
        b.samples = 6;
        b.series[Series::Cpu.index()] = stat(0.5);
        b
    }

    #[test]
    fn fixed_spans_end_just_after_now() {
        let now = 100 * 86_400 + 5000;
        assert_eq!(Span::Hour.bounds(now, 0), (now - 3600, now + 1));
        assert_eq!(Span::Day.bounds(now, 0), (now - 86_400, now + 1));
        assert_eq!(Span::Week.bounds(now, 0).0, now - 7 * 86_400);
        assert_eq!(Span::Month.bounds(now, 0).0, now - 30 * 86_400);
    }

    #[test]
    fn spans_near_the_epoch_saturate_instead_of_wrapping() {
        assert_eq!(Span::Month.bounds(100, 0), (0, 101));
        assert_eq!(Span::Today.bounds(100, -7200).0, 0);
    }

    #[test]
    fn today_starts_at_local_midnight() {
        let midnight_utc = 20_000 * 86_400;
        // 10:00 UTC; UTC+5:30 → local 15:30, local midnight = 18:30 UTC the day before.
        let now = midnight_utc + 10 * 3600;
        let ist = 5 * 3600 + 1800;
        assert_eq!(Span::Today.bounds(now, 0).0, midnight_utc);
        assert_eq!(
            Span::Today.bounds(now, ist).0,
            midnight_utc - 5 * 3600 - 1800
        );
        // UTC-8 at 03:00 UTC is 19:00 the day before locally.
        let early = midnight_utc + 3 * 3600;
        assert_eq!(
            Span::Today.bounds(early, -8 * 3600).0,
            midnight_utc - 16 * 3600
        );
    }

    #[test]
    fn today_is_never_longer_than_a_day() {
        for offset in (-12..=14).map(|h| h * 3600) {
            for now in (0..86_400).step_by(977).map(|s| 30_000 * 86_400 + s) {
                let (from, to) = Span::Today.bounds(now, offset);
                assert!(to - from <= 86_401, "{offset} {now}");
            }
        }
    }

    #[test]
    fn span_tiers_answer_long_ranges_from_month() {
        assert_eq!(Span::Hour.tier(), Tier::Day);
        assert_eq!(Span::Today.tier(), Tier::Day);
        assert_eq!(Span::Week.tier(), Tier::Month);
        assert_eq!(Span::Month.tier(), Tier::Month);
    }

    #[test]
    fn span_step_clamps_at_both_ends() {
        assert_eq!(Span::Hour.step(-1), Span::Hour);
        assert_eq!(Span::Hour.step(1), Span::Today);
        assert_eq!(Span::Month.step(1), Span::Month);
        assert_eq!(Span::Week.step(-2), Span::Today);
        assert_eq!(Span::Hour.step(99), Span::Month);
    }

    #[test]
    fn column_mapping_round_trips_for_every_width() {
        for width in [1, 7, 40, 80, 211, 500] {
            for (from, to) in [(0, 3601), (1000, 87_401), (5, 2_592_006)] {
                for col in 0..width {
                    let t = column_start(col, from, to, width);
                    assert_eq!(column_of(t, from, to, width), Some(col), "{width} {col}");
                }
            }
        }
    }

    #[test]
    fn column_of_is_monotonic_and_bounded() {
        let (from, to, width) = (100, 3701, 73);
        let mut last = 0;
        for t in from..to {
            let c = column_of(t, from, to, width).unwrap();
            assert!(c >= last && c < width);
            last = c;
        }
        assert_eq!(column_of(from - 1, from, to, width), None);
        assert_eq!(column_of(to, from, to, width), None);
        assert_eq!(column_of(from, from, to, 0), None);
    }

    #[test]
    fn bin_merges_buckets_per_column_and_leaves_gaps_empty() {
        // An hour of 6 s buckets, but nothing in the middle third.
        let (from, to) = (0, 3600);
        let buckets: Vec<Bucket> = (0..600)
            .map(|i| i * 6)
            .filter(|t| !(1200..2400).contains(t))
            .map(bucket)
            .collect();
        let cols = bin(&buckets, Tier::Day, from, to, 60);
        assert_eq!(cols.len(), 60);
        assert_eq!(
            cols[0].as_ref().unwrap().samples,
            60,
            "ten 6-sample buckets"
        );
        assert!(cols[20..40].iter().all(Option::is_none));
        assert!(cols[40..].iter().all(Option::is_some));
        assert_eq!(last_filled(&cols), Some(59));
    }

    /// Regression: at 00:10 "today" spans 601 s over 150 columns, narrower
    /// than a 6 s bucket, so every other column sat empty and each raised
    /// "resumed after 8 s" though recording never stopped.
    #[test]
    fn narrow_columns_over_contiguous_buckets_leave_no_comb_of_gaps() {
        let midnight = 20_000 * 86_400;
        let now = midnight + 600;
        let (from, to) = Span::Today.bounds(now, 0);
        let buckets: Vec<Bucket> = (0..100).map(|i| bucket(midnight + i * 6)).collect();
        let cols = bin(&buckets, Tier::Day, from, to, 150);
        assert!(cols.iter().all(Option::is_some), "{cols:?}");
        assert_eq!(events(&cols), []);
        // A real gap still shows, and still says how long it was.
        let gapped: Vec<Bucket> = buckets
            .into_iter()
            .filter(|b| !(midnight + 300..midnight + 360).contains(&b.t))
            .collect();
        let cols = bin(&gapped, Tier::Day, from, to, 150);
        let ev = events(&cols);
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert!(matches!(ev[0].1, Event::Resumed(gap) if (60..=70).contains(&gap)));
    }

    /// Regression: buckets out of order (a clock stepped back) put one
    /// column in two runs, and the second overwrote the first.
    #[test]
    fn bin_merges_every_bucket_of_a_column_whatever_their_order() {
        let mut late = bucket(6);
        late.series[Series::Cpu.index()] = stat(0.8);
        let cols = bin(&[bucket(0), late, bucket(0)], Tier::Day, 0, 60, 1);
        let col = cols[0].as_ref().unwrap();
        assert_eq!(col.samples, 18);
        let cpu = col.get(Series::Cpu).unwrap();
        assert!((cpu.max - 0.8).abs() < 1e-6, "{cpu:?}");
    }

    #[test]
    fn bin_keeps_the_peak_of_merged_buckets() {
        let mut a = bucket(0);
        let mut b = bucket(6);
        a.series[Series::Cpu.index()] = Some(Stat {
            mean: 0.1,
            max: 0.2,
        });
        b.series[Series::Cpu.index()] = Some(Stat {
            mean: 0.3,
            max: 0.9,
        });
        let cols = bin(&[a, b], Tier::Day, 0, 60, 1);
        let s = cols[0].as_ref().unwrap().get(Series::Cpu).unwrap();
        assert!((s.mean - 0.2).abs() < 1e-6 && (s.max - 0.9).abs() < 1e-6);
        assert_eq!(values(&cols, Series::Cpu, true), [Some(0.9)]);
    }

    /// Regression (UI-DESIGN §2): a fanless Air's timeline has no fan row
    /// rather than a flat zero.
    #[test]
    fn absent_series_is_not_present() {
        let cols = bin(&[bucket(0)], Tier::Day, 0, 60, 4);
        assert!(present(&cols, Series::Cpu));
        assert!(!present(&cols, Series::Fan));
        assert_eq!(values(&cols, Series::Fan, false), [None; 4]);
    }

    #[test]
    fn scrub_column_maps_to_its_bucket_and_blame_takes_top_n() {
        let mut b = bucket(1800);
        b.top_cpu = (1..=5).map(|i| proc(i, "p", 100.0 - i as f32)).collect();
        b.top_energy = vec![proc(9, "Safari", 3.0)];
        let cols = bin(&[bucket(0), b], Tier::Day, 0, 3600, 10);
        let col = column_of(1800, 0, 3600, 10).unwrap();
        assert_eq!(col, 5);
        let blame = Blame::at(&cols, &[], col, 3);
        assert_eq!(blame.cpu.len(), 1, "five `p`s fold into one row");
        assert_eq!(blame.cpu[0].count, 5);
        assert_eq!(blame.energy[0].name, "Safari");
        assert!(
            Blame::at(&cols, &[], 7, 3).is_empty(),
            "a gap blames nobody"
        );
        assert!(Blame::at(&cols, &[], 99, 3).is_empty(), "off the end");
    }

    #[test]
    fn events_flag_swap_growth_hot_edges_and_gaps() {
        let gib = 1024.0 * 1024.0 * 1024.0;
        let mut cols: Vec<Option<Bucket>> = (0..7).map(|i| Some(bucket(i * 6))).collect();
        for (i, c) in cols.iter_mut().enumerate() {
            let c = c.as_mut().unwrap();
            c.series[Series::SwapBytes.index()] = stat(if i >= 2 { gib } else { 0.0 });
            c.series[Series::CpuTempC.index()] =
                stat(if (3..5).contains(&i) { 99.0 } else { 60.0 });
        }
        cols[5] = None;
        let ev = events(&cols);
        assert!(ev.contains(&(2, Event::SwapGrew(gib))));
        assert!(ev.contains(&(3, Event::RanHot(99.0))));
        assert!(!ev.iter().any(|(c, _)| *c == 4), "still hot is not news");
        assert_eq!(ev.last(), Some(&(6, Event::Resumed(36 - 24))));
        assert_eq!(ev.len(), 3, "{ev:?}");
        let blame = Blame::at(&cols, &ev, 3, 5);
        assert_eq!(blame.events, [Event::RanHot(99.0)]);
    }

    /// Regression: a build's eight `rustc` processes filled the whole card.
    #[test]
    fn blame_folds_same_named_processes() {
        let list = [
            proc(1, "rustc", 250.0),
            proc(2, "rustc", 240.0),
            proc(3, "Xcode", 300.0),
            proc(4, "rustc", 10.0),
            proc(5, "mds", 5.0),
        ];
        let named = by_name(&list, 2);
        assert_eq!(named.len(), 2);
        assert_eq!((named[0].name.as_str(), named[0].count), ("rustc", 3));
        assert!((named[0].value - 500.0).abs() < 1e-3);
        assert_eq!(named[1].name, "Xcode");
    }

    #[test]
    fn event_labels_read_plainly() {
        assert_eq!(Event::RanHot(97.4).label(), "CPU ran hot, 97°");
        assert_eq!(Event::Resumed(3700).label(), "resumed after 1 h 1 min");
        assert!(
            Event::SwapGrew(512.0 * 1024.0 * 1024.0)
                .label()
                .contains("512M")
        );
    }

    #[test]
    fn clock_labels_follow_the_local_zone() {
        // 2026-09-23 18:30:00 UTC.
        let t = 1_790_188_200;
        assert_eq!(clock_label(t, 0, false), "18:30");
        assert_eq!(clock_label(t, 5 * 3600 + 1800, false), "00:00");
        assert_eq!(clock_label(t, 5 * 3600 + 1800, true), "Sep 24 00:00");
        assert_eq!(clock_label(t, -7 * 3600, true), "Sep 23 11:30");
    }

    #[test]
    fn duration_table() {
        for (s, want) in [
            (0, "0 s"),
            (59, "59 s"),
            (60, "1 min"),
            (3600, "1 h"),
            (3660, "1 h 1 min"),
            (86_400, "1 d"),
            (90_000, "1 d 1 h"),
        ] {
            assert_eq!(duration(s), want);
        }
    }

    fn day_bucket(t: u64, watts: &[(&str, f32)], cpu_c: f32, sys_w: Option<f32>) -> Bucket {
        let mut b = bucket(t);
        b.top_energy = watts
            .iter()
            .enumerate()
            .map(|(i, (n, w))| proc(i as u32 + 1, n, *w))
            .collect();
        b.series[Series::CpuTempC.index()] = stat(cpu_c);
        b.series[Series::SysPowerW.index()] = sys_w.and_then(stat);
        b
    }

    #[test]
    fn report_sums_energy_by_app_over_buckets() {
        // 600 six-second buckets = 1 h: Xcode at 6 W → 6 Wh; Safari 1 W → 1 Wh.
        let buckets: Vec<Bucket> = (0..600)
            .map(|i| day_bucket(i * 6, &[("Xcode", 6.0), ("Safari", 1.0)], 50.0, Some(10.0)))
            .collect();
        let r = Report::build(&buckets, Tier::Day, 0, 3600, 5);
        assert_eq!(r.recorded_s, 3600);
        assert_eq!(r.energy[0].name, "Xcode");
        assert!((r.energy[0].wh - 6.0).abs() < 1e-3, "{}", r.energy[0].wh);
        assert!((r.energy[1].wh - 1.0).abs() < 1e-3);
        assert!((r.system_wh.unwrap() - 10.0).abs() < 1e-3);
    }

    #[test]
    fn report_merges_the_same_app_across_pids() {
        let mut a = day_bucket(0, &[("Chrome", 2.0)], 50.0, None);
        a.top_energy[0].pid = 10;
        let mut b = day_bucket(6, &[("Chrome", 2.0)], 50.0, None);
        b.top_energy[0].pid = 11;
        let r = Report::build(&[a, b], Tier::Day, 0, 12, 5);
        assert_eq!(r.energy.len(), 1);
        assert_eq!(r.system_wh, None, "no input rail: absent, not 0 Wh");
    }

    #[test]
    fn report_finds_peaks_and_hot_time() {
        let buckets = [
            day_bucket(0, &[], 70.0, None),
            day_bucket(6, &[], 99.0, None),
            day_bucket(12, &[], 96.0, None),
            day_bucket(18, &[], 80.0, None),
        ];
        let r = Report::build(&buckets, Tier::Day, 0, 24, 5);
        assert_eq!(r.peak_cpu_c, Some(Peak { value: 99.0, t: 6 }));
        assert_eq!(r.peak_gpu_c, None);
        assert_eq!(r.hot_s, 12);
    }

    #[test]
    fn report_truncates_the_leaderboard() {
        let names: Vec<String> = (0..9).map(|i| format!("app{i}")).collect();
        let watts: Vec<(&str, f32)> = names.iter().map(|n| (n.as_str(), 1.0)).collect();
        let r = Report::build(&[day_bucket(0, &watts, 50.0, None)], Tier::Day, 0, 6, 3);
        assert_eq!(r.energy.len(), 3);
        assert_eq!(r.energy[0].name, "app0", "ties break by name");
    }

    /// Fake clock: a store filled for two days answers `today` with only
    /// the buckets since local midnight.
    #[test]
    fn report_query_reads_only_the_span_from_the_store() {
        let mut store = MemoryStore::new();
        let midnight = 20_000 * 86_400;
        let start = midnight - 3600;
        for i in 0..1200 {
            store
                .append(day_bucket(start + i * 6, &[("mds", 1.0)], 50.0, None))
                .unwrap();
        }
        let now = start + 1200 * 6;
        let r = Report::query(&store, Span::Today, now, 0, 5).unwrap();
        assert_eq!(r.from, midnight);
        assert_eq!(r.recorded_s, 600 * 6);
        let empty = Report::query(&MemoryStore::new(), Span::Week, now, 0, 5).unwrap();
        assert!(empty.is_empty());
    }

    // ── battery, power source and thermal state (store v2) ──

    /// A Day bucket at `t` with the given v2 columns (`None` = unrecorded).
    fn power_bucket(t: u64, ac: Option<f32>, pct: Option<f32>, watts: Option<f32>) -> Bucket {
        let mut b = Bucket::empty(t);
        b.samples = 1;
        b.series[Series::OnAc.index()] = ac.and_then(stat);
        b.series[Series::BatteryPct.index()] = pct.and_then(stat);
        b.series[Series::BatteryW.index()] = watts.and_then(stat);
        b
    }

    fn thermal_bucket(t: u64, level: f32) -> Bucket {
        let mut b = Bucket::empty(t);
        b.samples = 1;
        b.series[Series::ThermalState.index()] = stat(level);
        b.series[Series::Throttled.index()] = stat(if level >= 2.0 { 1.0 } else { 0.0 });
        b
    }

    /// Per-column inputs and the events they must raise.
    type EventCase<T> = (&'static [T], &'static [(usize, Event)]);

    fn events_of(buckets: Vec<Bucket>) -> Vec<(usize, Event)> {
        let columns: Vec<Option<Bucket>> = buckets.into_iter().map(Some).collect();
        events(&columns)
    }

    #[test]
    fn power_source_event_table() {
        let cases: [EventCase<Option<f32>>; 6] = [
            (&[Some(1.0), Some(0.0)], &[(1, Event::Unplugged)]),
            (&[Some(0.0), Some(1.0)], &[(1, Event::PluggedIn)]),
            (&[Some(1.0), Some(1.0)], &[]),
            (&[Some(0.0), Some(0.0)], &[]),
            // A desktop (or v1 history) never records the source: silent.
            (&[None, None], &[]),
            (&[Some(1.0), None], &[]),
        ];
        for (ac, want) in cases {
            let buckets = ac
                .iter()
                .enumerate()
                .map(|(i, a)| power_bucket(i as u64 * 6, *a, None, None))
                .collect();
            assert_eq!(events_of(buckets), want, "{ac:?}");
        }
    }

    #[test]
    fn thermal_state_event_table() {
        let cases: [EventCase<f32>; 5] = [
            (&[0.0, 0.0, 0.0], &[]),
            (&[0.0, 1.0], &[(1, Event::Thermal(1))]),
            (
                &[1.0, 2.0, 3.0],
                &[(1, Event::Thermal(2)), (2, Event::Thermal(3))],
            ),
            (&[3.0, 0.0], &[(1, Event::Thermal(0))]),
            (&[2.0, 2.0, 0.0], &[(2, Event::Thermal(0))]),
        ];
        for (levels, want) in cases {
            let buckets = levels
                .iter()
                .enumerate()
                .map(|(i, l)| thermal_bucket(i as u64 * 6, *l))
                .collect();
            assert_eq!(events_of(buckets), want, "{levels:?}");
        }
    }

    #[test]
    fn low_battery_fires_once_on_the_way_down() {
        let pcts = [25.0, 21.0, 19.5, 18.0, 30.0, 15.0];
        let buckets = pcts
            .iter()
            .enumerate()
            .map(|(i, p)| power_bucket(i as u64 * 6, Some(0.0), Some(*p), None))
            .collect();
        assert_eq!(
            events_of(buckets),
            [(2, Event::BatteryLow(19.5)), (5, Event::BatteryLow(15.0))]
        );
    }

    #[test]
    fn battery_and_thermal_event_labels_read_plainly() {
        assert_eq!(Event::Unplugged.label(), "unplugged, on battery");
        assert_eq!(Event::PluggedIn.label(), "plugged in");
        assert_eq!(Event::Thermal(2).label(), "thermal state serious");
        assert_eq!(Event::Thermal(3).label(), "thermal state critical");
        assert_eq!(Event::Thermal(0).label(), "thermal state back to nominal");
        assert_eq!(Event::BatteryLow(18.4).label(), "battery below 20%, 18%");
    }

    /// Fake clock: 1 h on AC, then 1 h on battery drawing 6 W, 100 → 80 %.
    #[test]
    fn report_drain_since_the_last_unplug() {
        let hour = 600; // Day buckets per hour
        let mut buckets = Vec::new();
        for i in 0..hour {
            buckets.push(power_bucket(i * 6, Some(1.0), Some(100.0), Some(2.0)));
        }
        for i in 0..hour {
            let pct = 100.0 - 20.0 * i as f32 / (hour - 1) as f32;
            buckets.push(power_bucket(
                (hour + i) * 6,
                Some(0.0),
                Some(pct),
                Some(-6.0),
            ));
        }
        let r = Report::build(&buckets, Tier::Day, 0, 7200, 5);
        let d = r.drain.unwrap();
        assert_eq!(d.since, 3600);
        assert!(d.unplugged);
        assert!((d.wh - 6.0).abs() < 1e-6, "{}", d.wh);
        assert!((d.avg_w - 6.0).abs() < 1e-6, "{}", d.avg_w);
        assert_eq!((d.from_pct, d.to_pct), (Some(100.0), Some(80.0)));
    }

    /// Only the newest stretch on battery counts; charging rows never add.
    #[test]
    fn report_drain_takes_the_newest_stretch_and_ignores_charging() {
        let mut buckets = vec![
            power_bucket(0, Some(0.0), None, Some(-20.0)),
            power_bucket(6, Some(1.0), None, Some(30.0)),
            power_bucket(12, Some(0.0), None, Some(-3.0)),
            // No source recorded (a v1 bucket): neither breaks nor adds.
            power_bucket(18, None, None, None),
            power_bucket(24, Some(0.0), None, Some(-5.0)),
        ];
        let r = Report::build(&buckets, Tier::Day, 0, 30, 5);
        let d = r.drain.unwrap();
        assert_eq!(d.since, 12);
        let hours = 6.0 / 3600.0;
        assert!((d.wh - 8.0 * hours).abs() < 1e-9);
        assert!((d.avg_w - 4.0).abs() < 1e-6);
        // Back on AC at the end: the last stretch still reports.
        buckets.push(power_bucket(30, Some(1.0), None, Some(10.0)));
        assert_eq!(
            Report::build(&buckets, Tier::Day, 0, 36, 5)
                .drain
                .unwrap()
                .since,
            12
        );
    }

    #[test]
    fn a_desktop_report_has_no_drain_or_health() {
        let buckets: Vec<Bucket> = (0..10).map(|i| bucket(i * 6)).collect();
        let r = Report::build(&buckets, Tier::Day, 0, 60, 5);
        assert_eq!((r.drain, r.health, r.throttled_s), (None, None, None));
        assert_eq!(r.energy_other_wh, None);
    }

    /// Fake clock over Month buckets: health read once per 144 s bucket
    /// across 20 days, first and last kept.
    #[test]
    fn report_health_trend_spans_the_range() {
        let day = 86_400;
        let buckets: Vec<Bucket> = (0..20)
            .map(|d| {
                let mut b = Bucket::empty(d * day);
                b.samples = 24;
                b.series[Series::BatteryHealth.index()] = stat(98.0 - d as f32 * 0.1);
                b
            })
            .collect();
        let r = Report::build(&buckets, Tier::Month, 0, 20 * day, 5);
        let h = r.health.unwrap();
        assert_eq!((h.first.t, h.first.value), (0, 98.0));
        assert_eq!(h.last.t, 19 * day);
        assert!((h.last.value - 96.1).abs() < 1e-4);
    }

    /// Throttled time is the Throttled mean × bucket span: exact even when
    /// a coarse bucket was only partly throttled.
    #[test]
    fn report_time_throttled_counts_the_share_of_each_bucket() {
        let mut buckets: Vec<Bucket> = [0.0, 1.0, 2.0, 3.0, 2.0, 0.0]
            .iter()
            .enumerate()
            .map(|(i, l)| thermal_bucket(i as u64 * 6, *l))
            .collect();
        let r = Report::build(&buckets, Tier::Day, 0, 36, 5);
        assert_eq!(r.throttled_s, Some(18));
        let mut half = Bucket::empty(144);
        half.samples = 24;
        half.series[Series::Throttled.index()] = stat(0.5);
        buckets = vec![half];
        assert_eq!(
            Report::build(&buckets, Tier::Month, 0, 288, 5).throttled_s,
            Some(72)
        );
    }

    /// Regression: a Month bucket filled by two 6 s Day buckets counted as
    /// 144 s recorded, and 144 s of watts, so `report week` claimed many
    /// times what `report today` did for the same minutes.
    #[test]
    fn a_partly_filled_month_bucket_counts_only_its_recorded_seconds() {
        let mut store = MemoryStore::new();
        for t in [0, 6] {
            let mut b = day_bucket(t, &[("cargo", 36.0)], 50.0, Some(72.0));
            b.covered_s = Some(6);
            b.apps_other = Some(0.0);
            b.apps = vec![Named {
                name: "cargo".into(),
                value: 36.0,
                count: 1,
            }];
            store.append(b).unwrap();
        }
        let day = Report::query(&store, Span::Today, 60, 0, 5).unwrap();
        let week = Report::query(&store, Span::Week, 60, 0, 5).unwrap();
        assert_eq!(day.recorded_s, 12);
        assert_eq!(week.recorded_s, 12, "the open Month span is 12 s, not 144");
        for r in [&day, &week] {
            assert!((r.system_wh.unwrap() - 72.0 * 12.0 / 3600.0).abs() < 1e-6);
            assert!((r.energy[0].wh - 36.0 * 12.0 / 3600.0).abs() < 1e-6);
        }
    }

    /// Regression: one sample at 95 °C made the whole bucket "hot"; the Hot
    /// share counts only the hot part of each bucket's recorded time.
    #[test]
    fn hot_time_counts_the_hot_share_of_each_bucket() {
        let mut spike = Bucket::empty(0);
        spike.samples = 288;
        spike.covered_s = Some(144);
        spike.series[Series::CpuTempC.index()] = Some(Stat {
            mean: 93.0,
            max: 96.0,
        });
        spike.series[Series::Hot.index()] = Some(Stat {
            mean: 1.0 / 144.0,
            max: 1.0,
        });
        let r = Report::build(&[spike], Tier::Month, 0, 144, 5);
        assert_eq!(r.hot_s, 1);
    }

    /// Exact per-app sums win over the top-5 list; v1 buckets still count
    /// through their top lists, and the tail lands in "other".
    #[test]
    fn report_energy_is_exact_with_app_sums_and_falls_back_for_v1() {
        let hours = 6.0 / 3600.0;
        let mut v2 = Bucket::empty(0);
        v2.samples = 1;
        v2.apps = vec![
            Named {
                name: "cargo".into(),
                value: 10.0,
                count: 4,
            },
            Named {
                name: "Safari".into(),
                value: 2.0,
                count: 1,
            },
        ];
        v2.apps_other = Some(1.0);
        v2.top_energy = vec![proc(1, "cargo", 99.0)]; // ignored: sums win
        let v1 = day_bucket(6, &[("cargo", 4.0), ("mail", 3.0)], 50.0, None);
        let r = Report::build(&[v2, v1], Tier::Day, 0, 12, 2);
        let names: Vec<&str> = r.energy.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["cargo", "mail"]);
        assert!((r.energy[0].wh - 14.0 * hours).abs() < 1e-9);
        // Safari (2 W) fell off the 2-row board: other = 1 W + 2 W.
        assert!((r.energy_other_wh.unwrap() - 3.0 * hours).abs() < 1e-9);
    }
}
