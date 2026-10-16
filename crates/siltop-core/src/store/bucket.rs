//! One stored slice of time and the accumulator that builds it from snapshots.

use std::collections::HashMap;

use super::query::Named;
use super::tier::Tier;
use super::{APP_N, SERIES_COUNT, Series, TOP_N};
use crate::snapshot::{Process, Snapshot};

/// A series' mean and peak over one bucket.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stat {
    pub mean: f32,
    pub max: f32,
}

/// One process in a bucket's top list; `value` is its mean over the
/// bucket's process samples (a process seen in one of six counts a sixth).
#[derive(Debug, Clone, PartialEq)]
pub struct TopProc {
    pub pid: u32,
    pub name: String,
    pub value: f32,
}

/// A span of history: series means and maxes plus who was busiest.
#[derive(Debug, Clone, PartialEq)]
pub struct Bucket {
    /// Unix second the bucket starts at, aligned to its tier's span.
    pub t: u64,
    /// Snapshots folded in; weights the mean when buckets merge.
    pub samples: u32,
    /// Seconds of the span actually recorded: a Day bucket's whole 6 s, and
    /// the sum of its Day buckets' for a coarser one, so a Month bucket
    /// touched by one short session does not count as 144 s. `None` in
    /// records older than store v3; read those as the whole span.
    pub covered_s: Option<u32>,
    /// Indexed by [`Series::index`]; `None` = absent on this Mac or unread.
    pub series: [Option<Stat>; SERIES_COUNT],
    /// Per-core CPU percent, highest first, at most [`TOP_N`].
    pub top_cpu: Vec<TopProc>,
    /// Watts.
    pub top_energy: Vec<TopProc>,
    /// Footprint bytes.
    pub top_mem: Vec<TopProc>,
    /// Energy by app (processes folded by display name), mean watts over the
    /// bucket, highest first, at most [`APP_N`]. Summed from every process,
    /// not a top list: `apps + apps_other` is the bucket's whole process
    /// energy. Empty in version 1 records (use `top_energy` there).
    pub apps: Vec<Named>,
    /// Watts of every app below the top [`APP_N`]; `None` when no process
    /// energy was measured (a v1 record, an unsampled bucket).
    pub apps_other: Option<f32>,
}

impl Bucket {
    #[must_use]
    pub fn empty(t: u64) -> Self {
        Self {
            t,
            samples: 0,
            covered_s: None,
            series: [None; SERIES_COUNT],
            top_cpu: Vec::new(),
            top_energy: Vec::new(),
            top_mem: Vec::new(),
            apps: Vec::new(),
            apps_other: None,
        }
    }

    #[must_use]
    pub fn get(&self, series: Series) -> Option<Stat> {
        self.series[series.index()]
    }

    /// Seconds recorded in this bucket of a tier `span_s` wide: its coverage,
    /// capped at the span (duplicates merged after a clock step back), or
    /// the whole span for a record that predates coverage.
    #[must_use]
    pub fn covered(&self, span_s: u64) -> u64 {
        self.covered_s
            .map_or(span_s, |covered| u64::from(covered).min(span_s))
    }
}

/// The `n` largest `(pid, name, value)` rows, highest first; ties go to the
/// lower pid so the pick is deterministic. NaN values never rank.
#[must_use]
pub fn top_n<'a>(rows: impl Iterator<Item = (u32, &'a str, f32)>, n: usize) -> Vec<TopProc> {
    let mut all: Vec<TopProc> = rows
        .filter(|(_, _, value)| value.is_finite() && *value > 0.0)
        .map(|(pid, name, value)| TopProc {
            pid,
            name: name.to_owned(),
            value,
        })
        .collect();
    let rank = |a: &TopProc, b: &TopProc| b.value.total_cmp(&a.value).then(a.pid.cmp(&b.pid));
    if n < all.len() {
        all.select_nth_unstable_by(n, rank);
        all.truncate(n);
    }
    all.sort_unstable_by(rank);
    // Stored buckets live for days: keep n rows, not every candidate's slot.
    all.shrink_to_fit();
    all
}

/// Re-ranks several buckets' top lists: each entry weighs by its bucket's
/// samples, and the sum is spread over `samples` in total.
#[must_use]
pub fn merge_top<'a>(
    lists: impl Iterator<Item = (&'a [TopProc], u32)>,
    samples: u32,
    n: usize,
) -> Vec<TopProc> {
    let mut sums: HashMap<(u32, &str), f64> = HashMap::new();
    for (list, weight) in lists {
        for p in list {
            *sums.entry((p.pid, p.name.as_str())).or_default() +=
                f64::from(p.value) * f64::from(weight);
        }
    }
    let total = f64::from(samples.max(1));
    top_n(
        sums.into_iter()
            .map(|((pid, name), sum)| (pid, name, (sum / total) as f32)),
        n,
    )
}

/// Ranks `(name, watts, processes)` rows (unique names) highest first (ties
/// by name), keeps [`APP_N`], and folds the rest plus `other` into the
/// "other" watts, so nothing is lost: the total is preserved exactly.
#[must_use]
pub fn split_apps(rows: Vec<Named>, other: Option<f64>) -> (Vec<Named>, Option<f32>) {
    if rows.is_empty() && other.is_none() {
        return (Vec::new(), None);
    }
    let mut rows: Vec<Named> = rows.into_iter().filter(|r| r.value.is_finite()).collect();
    rows.sort_by(|a, b| {
        b.value
            .total_cmp(&a.value)
            .then_with(|| a.name.cmp(&b.name))
    });
    let rest: f64 = rows
        .drain(rows.len().min(APP_N)..)
        .map(|r| f64::from(r.value))
        .sum::<f64>()
        + other.unwrap_or(0.0);
    // Stored buckets live for days: keep APP_N rows, not every name's slot.
    rows.shrink_to_fit();
    (rows, Some(rest as f32))
}

/// Merges several buckets' app lists, each weighted by its samples and
/// spread over `samples` in total; the "other" rows add up the same way.
#[must_use]
pub fn merge_apps<'a>(
    lists: impl Iterator<Item = (&'a [Named], Option<f32>, u32)>,
    samples: u32,
) -> (Vec<Named>, Option<f32>) {
    let mut sums: HashMap<&str, (f64, usize)> = HashMap::new();
    let mut other: Option<f64> = None;
    for (list, rest, weight) in lists {
        let w = f64::from(weight);
        for app in list {
            let e = sums.entry(app.name.as_str()).or_default();
            e.0 += f64::from(app.value) * w;
            e.1 = e.1.max(app.count);
        }
        if let Some(rest) = rest {
            *other.get_or_insert(0.0) += f64::from(rest) * w;
        }
    }
    let total = f64::from(samples.max(1));
    let rows = sums
        .into_iter()
        .map(|(name, (sum, count))| Named {
            name: name.to_owned(),
            value: (sum / total) as f32,
            count,
        })
        .collect();
    split_apps(rows, other.map(|o| o / total))
}

/// Every process's watts in one bucket, folded by display name. Bounded by
/// the process table (a few hundred names) and emptied at each close.
#[derive(Debug, Clone, Default)]
struct AppSums {
    /// name → (summed watts over samples, most processes in one sample).
    by_name: HashMap<String, (f64, usize)>,
    measured: bool,
}

impl AppSums {
    fn add(&mut self, procs: &[Process]) {
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for p in procs {
            let Some(w) = p.energy_w.filter(|w| w.is_finite() && *w > 0.0) else {
                if p.energy_w.is_some() {
                    self.measured = true;
                }
                continue;
            };
            self.measured = true;
            *seen.entry(p.name.as_str()).or_default() += 1;
            match self.by_name.get_mut(p.name.as_str()) {
                Some(e) => e.0 += f64::from(w),
                None => {
                    self.by_name.insert(p.name.clone(), (f64::from(w), 0));
                }
            }
        }
        for (name, n) in seen {
            if let Some(e) = self.by_name.get_mut(name) {
                e.1 = e.1.max(n);
            }
        }
    }

    fn close(&mut self, samples: u32) -> (Vec<Named>, Option<f32>) {
        let sums = std::mem::take(&mut self.by_name);
        if !std::mem::take(&mut self.measured) {
            return (Vec::new(), None);
        }
        let total = f64::from(samples.max(1));
        let rows = sums
            .into_iter()
            .map(|(name, (sum, count))| Named {
                name,
                value: (sum / total) as f32,
                count,
            })
            .collect();
        split_apps(rows, Some(0.0))
    }
}

/// How many rows of each snapshot compete for a bucket's top list. Wider
/// than [`TOP_N`] so a steady sixth-place process can still win the bucket.
const TOP_CANDIDATES: usize = TOP_N * 2;

#[derive(Debug, Clone, Copy, Default)]
struct Running {
    sum: f64,
    count: u32,
    max: f32,
}

/// Folds snapshots into one Day bucket at a time. Cheap per tick: a dozen
/// adds and a partial sort of the process rows.
#[derive(Debug, Clone)]
pub struct Accumulator {
    tier: Tier,
    t: Option<u64>,
    samples: u32,
    /// Ticks whose process table was refreshed: the top lists' divisor, so
    /// a process slower than the tick is not diluted by the ticks between.
    proc_samples: u32,
    series: [Running; SERIES_COUNT],
    cpu: Vec<(TopProc, u32)>,
    energy: Vec<(TopProc, u32)>,
    mem: Vec<(TopProc, u32)>,
    apps: AppSums,
}

impl Default for Accumulator {
    fn default() -> Self {
        Self::new(Tier::Day)
    }
}

impl Accumulator {
    #[must_use]
    pub fn new(tier: Tier) -> Self {
        Self {
            tier,
            t: None,
            samples: 0,
            proc_samples: 0,
            series: [Running::default(); SERIES_COUNT],
            cpu: Vec::new(),
            energy: Vec::new(),
            mem: Vec::new(),
            apps: AppSums::default(),
        }
    }

    /// Adds one primed snapshot taken at unix second `now`. Returns the
    /// previous bucket once `now` falls in a new one.
    pub fn add(&mut self, snapshot: &Snapshot, now: u64) -> Option<Bucket> {
        let start = self.tier.floor(now);
        let done = match self.t {
            Some(t) if t != start => self.close(),
            _ => None,
        };
        self.t = Some(start);
        self.samples += 1;
        for series in Series::ALL {
            if let Some(value) = series.read(snapshot).filter(|v| v.is_finite()) {
                let run = &mut self.series[series.index()];
                run.max = if run.count == 0 {
                    value
                } else {
                    run.max.max(value)
                };
                run.sum += f64::from(value);
                run.count += 1;
            }
        }
        if snapshot.sampled.procs {
            self.proc_samples += 1;
            let procs = &snapshot.processes;
            fold_top(&mut self.cpu, procs, |p| p.cpu);
            fold_top(&mut self.energy, procs, |p| p.energy_w);
            fold_top(&mut self.mem, procs, |p| p.mem_bytes.map(|b| b as f32));
            self.apps.add(procs);
        }
        done
    }

    /// Snapshots folded into the open bucket so far.
    #[must_use]
    pub const fn samples(&self) -> u32 {
        self.samples
    }

    /// Closes the open bucket, if it holds anything.
    pub fn close(&mut self) -> Option<Bucket> {
        let t = self.t.take()?;
        let samples = std::mem::take(&mut self.samples);
        let proc_samples = std::mem::take(&mut self.proc_samples);
        let series = std::mem::replace(&mut self.series, [Running::default(); SERIES_COUNT]);
        let rank = |rows: Vec<(TopProc, u32)>| {
            top_n(
                rows.iter()
                    .map(|(p, _)| (p.pid, p.name.as_str(), p.value / proc_samples.max(1) as f32)),
                TOP_N,
            )
        };
        let (apps, apps_other) = self.apps.close(proc_samples);
        let bucket = Bucket {
            t,
            samples,
            covered_s: u32::try_from(self.tier.span_s()).ok(),
            series: series.map(|run| {
                (run.count > 0).then(|| Stat {
                    mean: (run.sum / f64::from(run.count)) as f32,
                    max: run.max,
                })
            }),
            top_cpu: rank(std::mem::take(&mut self.cpu)),
            top_energy: rank(std::mem::take(&mut self.energy)),
            top_mem: rank(std::mem::take(&mut self.mem)),
            apps,
            apps_other,
        };
        (samples > 0).then_some(bucket)
    }
}

/// Adds this snapshot's leading rows to the running per-pid sums.
fn fold_top(sums: &mut Vec<(TopProc, u32)>, procs: &[Process], value: fn(&Process) -> Option<f32>) {
    let leaders = top_n(
        procs
            .iter()
            .filter_map(|p| value(p).map(|v| (p.pid, p.name.as_str(), v))),
        TOP_CANDIDATES,
    );
    for lead in leaders {
        match sums
            .iter_mut()
            .find(|(p, _)| p.pid == lead.pid && p.name == lead.name)
        {
            Some((p, seen)) => {
                p.value += lead.value;
                *seen += 1;
            }
            None => sums.push((lead, 1)),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn proc(pid: u32, name: &str, cpu: f32, watts: f32, mem: u64) -> Process {
        Process {
            pid,
            name: name.to_owned(),
            cpu: Some(cpu),
            energy_w: Some(watts),
            mem_bytes: Some(mem),
            ..Process::default()
        }
    }

    fn snap(cpu: f32, procs: Vec<Process>) -> Snapshot {
        let mut s = Snapshot::empty();
        s.cpu.scaled = cpu;
        s.memory.total_bytes = 100;
        s.memory.used_bytes = 50;
        s.sampled.procs = true;
        s.primed = true;
        s.processes = procs;
        s
    }

    #[test]
    fn top_n_picks_the_largest_highest_first_and_breaks_ties_by_pid() {
        let rows = [(3, "c", 5.0), (1, "a", 9.0), (2, "b", 5.0), (4, "d", 1.0)];
        let top = top_n(rows.into_iter(), 3);
        let pids: Vec<u32> = top.iter().map(|p| p.pid).collect();
        assert_eq!(pids, [1, 2, 3]);
    }

    #[test]
    fn top_n_skips_zero_negative_and_nan() {
        let rows = [
            (1, "a", 0.0),
            (2, "b", f32::NAN),
            (3, "c", -1.0),
            (4, "d", 0.1),
        ];
        let top = top_n(rows.into_iter(), 5);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].pid, 4);
    }

    #[test]
    fn top_n_of_many_keeps_exactly_n() {
        let names: Vec<String> = (0..500).map(|i| format!("p{i}")).collect();
        let rows = names
            .iter()
            .enumerate()
            .map(|(i, n)| (i as u32, n.as_str(), i as f32));
        let top = top_n(rows, TOP_N);
        assert_eq!(top.len(), TOP_N);
        assert_eq!(top[0].pid, 499);
        assert_eq!(top[4].pid, 495);
    }

    #[test]
    fn accumulator_closes_on_the_next_span_with_means_and_maxes() {
        let mut acc = Accumulator::new(Tier::Day);
        assert!(acc.add(&snap(0.2, vec![]), 60).is_none());
        assert!(acc.add(&snap(0.6, vec![]), 62).is_none());
        let b = acc.add(&snap(0.1, vec![]), 66).unwrap();
        assert_eq!((b.t, b.samples), (60, 2));
        let cpu = b.get(Series::Cpu).unwrap();
        assert!((cpu.mean - 0.4).abs() < 1e-6 && (cpu.max - 0.6).abs() < 1e-6);
        assert!((b.get(Series::Mem).unwrap().mean - 0.5).abs() < 1e-6);
    }

    #[test]
    fn accumulator_absent_gpu_and_fans_stay_absent() {
        let mut acc = Accumulator::new(Tier::Day);
        acc.add(&snap(0.2, vec![]), 0);
        let b = acc.close().unwrap();
        assert_eq!(b.get(Series::Gpu), None);
        assert_eq!(b.get(Series::Fan), None);
        assert_eq!(b.get(Series::SysPowerW), None);
    }

    /// Regression: a process present in one of three samples must not rank
    /// as if it had run the whole bucket.
    #[test]
    fn a_brief_spike_is_averaged_over_the_bucket_not_its_lifetime() {
        let mut acc = Accumulator::new(Tier::Day);
        acc.add(&snap(0.1, vec![proc(1, "steady", 50.0, 1.0, 10)]), 0);
        acc.add(
            &snap(
                0.1,
                vec![
                    proc(1, "steady", 50.0, 1.0, 10),
                    proc(2, "blip", 120.0, 0.1, 5),
                ],
            ),
            1,
        );
        acc.add(&snap(0.1, vec![proc(1, "steady", 50.0, 1.0, 10)]), 2);
        let b = acc.close().unwrap();
        assert_eq!(b.top_cpu[0].name, "steady");
        assert!((b.top_cpu[0].value - 50.0).abs() < 1e-4);
        assert!((b.top_cpu[1].value - 40.0).abs() < 1e-4);
        assert_eq!(b.top_energy[0].name, "steady");
        assert_eq!(b.top_mem[0].pid, 1);
    }

    #[test]
    fn a_reused_pid_with_a_new_name_is_a_different_process() {
        let mut acc = Accumulator::new(Tier::Day);
        acc.add(&snap(0.1, vec![proc(7, "old", 10.0, 0.0, 1)]), 0);
        acc.add(&snap(0.1, vec![proc(7, "new", 10.0, 0.0, 1)]), 1);
        let b = acc.close().unwrap();
        assert_eq!(b.top_cpu.len(), 2);
    }

    #[test]
    fn unsampled_process_ticks_do_not_touch_the_top_lists() {
        let mut acc = Accumulator::new(Tier::Day);
        let mut s = snap(0.1, vec![proc(1, "a", 90.0, 1.0, 1)]);
        s.sampled.procs = false;
        acc.add(&s, 0);
        assert!(acc.close().unwrap().top_cpu.is_empty());
    }

    /// Regression: the process table refreshes slower than the tick (1 s
    /// vs 500 ms), so dividing by every tick halved each top list and app.
    #[test]
    fn top_lists_are_not_diluted_by_ticks_without_a_process_refresh() {
        let mut acc = Accumulator::new(Tier::Day);
        for i in 0..12 {
            let mut s = snap(0.1, vec![proc(1, "xcode", 100.0, 4.0, 2 << 30)]);
            s.sampled.procs = i % 2 == 0;
            acc.add(&s, 0);
        }
        let b = acc.close().unwrap();
        assert_eq!(b.samples, 12);
        assert!((b.top_cpu[0].value - 100.0).abs() < 1e-3);
        assert!((b.top_energy[0].value - 4.0).abs() < 1e-4);
        assert!((b.top_mem[0].value - (2u64 << 30) as f32).abs() < 1.0);
        assert!((b.apps[0].value - 4.0).abs() < 1e-4);
    }

    /// Regression: a stored bucket's lists kept every candidate's capacity
    /// (24 h of session history grew RSS by 15–134 MB).
    #[test]
    fn stored_top_and_app_lists_hold_no_spare_capacity() {
        let names: Vec<String> = (0..500).map(|i| format!("p{i}")).collect();
        let rows = names
            .iter()
            .enumerate()
            .map(|(i, n)| (i as u32, n.as_str(), i as f32 + 1.0));
        let top = top_n(rows, TOP_N);
        assert_eq!(top.capacity(), TOP_N);
        let apps = names
            .iter()
            .enumerate()
            .map(|(i, n)| Named {
                name: n.clone(),
                value: i as f32,
                count: 1,
            })
            .collect();
        let (apps, _) = split_apps(apps, None);
        assert_eq!(apps.len(), APP_N);
        assert_eq!(apps.capacity(), APP_N);
    }

    #[test]
    fn closing_an_empty_accumulator_yields_nothing() {
        assert!(Accumulator::default().close().is_none());
    }

    #[test]
    fn merge_top_weights_by_bucket_samples() {
        let a = [TopProc {
            pid: 1,
            name: "a".into(),
            value: 10.0,
        }];
        let b = [TopProc {
            pid: 2,
            name: "b".into(),
            value: 30.0,
        }];
        let merged = merge_top([(&a[..], 3), (&b[..], 1)].into_iter(), 4, TOP_N);
        // 10 W × 3 samples and 30 W × 1 sample tie at 7.5; lower pid first.
        assert_eq!(merged[0].pid, 1);
        assert!((merged[0].value - 7.5).abs() < 1e-6);
        assert!((merged[1].value - 7.5).abs() < 1e-6);
    }

    // ── exact per-app energy ──

    /// A tiny deterministic generator (no rand dep, same run every time).
    fn lcg(seed: &mut u64) -> u64 {
        *seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *seed >> 33
    }

    /// 40 apps × up to 3 processes over 7 samples, energy in whole mW so
    /// every sum is exact in f64.
    fn busy_snapshots() -> Vec<Snapshot> {
        let mut seed = 7;
        (0..7)
            .map(|_| {
                let mut procs = Vec::new();
                for app in 0..40_u32 {
                    for k in 0..(lcg(&mut seed) % 4) {
                        let mw = (lcg(&mut seed) % 5000) as f32;
                        procs.push(proc(
                            app * 10 + k as u32,
                            &format!("app{app}"),
                            1.0,
                            mw / 1000.0,
                            1,
                        ));
                    }
                }
                snap(0.1, procs)
            })
            .collect()
    }

    /// Accumulator exactness vs a brute-force sum over every process.
    #[test]
    fn app_energy_matches_a_brute_force_sum_over_every_process() {
        let snaps = busy_snapshots();
        let mut acc = Accumulator::new(Tier::Day);
        let mut brute: HashMap<String, f64> = HashMap::new();
        for (i, s) in snaps.iter().enumerate() {
            acc.add(s, i as u64 % 6);
            for p in &s.processes {
                *brute.entry(p.name.clone()).or_default() += f64::from(p.energy_w.unwrap());
            }
        }
        let b = acc.close().unwrap();
        let n = f64::from(b.samples);
        assert_eq!(b.apps.len(), APP_N);
        for app in &b.apps {
            let want = brute[&app.name] / n;
            assert!((f64::from(app.value) - want).abs() < 1e-4, "{}", app.name);
        }
        let mut ranked: Vec<f64> = brute.values().map(|v| v / n).collect();
        ranked.sort_by(|a, b| b.total_cmp(a));
        let listed: f64 = b.apps.iter().map(|a| f64::from(a.value)).sum();
        let total: f64 = ranked.iter().sum();
        assert!((listed - ranked[..APP_N].iter().sum::<f64>()).abs() < 1e-3);
        let other = f64::from(b.apps_other.unwrap());
        assert!(
            (listed + other - total).abs() < 1e-3,
            "{listed} + {other} vs {total}"
        );
    }

    /// Regression: the old leaderboard only saw each tick's top 10 rows, so
    /// an app spread over many small processes (a build's rustc swarm)
    /// vanished from reports.
    #[test]
    fn a_swarm_of_small_processes_outranks_one_big_one() {
        let mut procs: Vec<Process> = (0..30)
            .map(|i| proc(100 + i, "rustc", 1.0, 0.5, 1))
            .collect();
        procs.extend((0..12).map(|i| proc(i, &format!("solo{i}"), 1.0, 2.0, 1)));
        let mut acc = Accumulator::new(Tier::Day);
        acc.add(&snap(0.1, procs), 0);
        let b = acc.close().unwrap();
        assert_eq!(b.apps[0].name, "rustc");
        assert!((b.apps[0].value - 15.0).abs() < 1e-4);
        assert_eq!(b.apps[0].count, 30);
        assert!(b.top_energy.iter().all(|p| p.name != "rustc"));
    }

    #[test]
    fn an_app_seen_in_one_sample_is_averaged_over_the_bucket() {
        let mut acc = Accumulator::new(Tier::Day);
        acc.add(&snap(0.1, vec![proc(1, "a", 1.0, 3.0, 1)]), 0);
        acc.add(&snap(0.1, vec![]), 1);
        acc.add(&snap(0.1, vec![]), 2);
        let b = acc.close().unwrap();
        assert!((b.apps[0].value - 1.0).abs() < 1e-6);
        assert_eq!(b.apps_other, Some(0.0));
    }

    /// No energy reading anywhere (Linux, restricted rows): absent, not 0.
    #[test]
    fn unmeasured_energy_leaves_the_app_list_absent() {
        let mut acc = Accumulator::new(Tier::Day);
        let mut p = proc(1, "a", 1.0, 0.0, 1);
        p.energy_w = None;
        acc.add(&snap(0.1, vec![p]), 0);
        let b = acc.close().unwrap();
        assert!(b.apps.is_empty());
        assert_eq!(b.apps_other, None);
    }

    #[test]
    fn the_app_list_resets_between_buckets() {
        let mut acc = Accumulator::new(Tier::Day);
        acc.add(&snap(0.1, vec![proc(1, "first", 1.0, 1.0, 1)]), 0);
        let first = acc
            .add(&snap(0.1, vec![proc(2, "second", 1.0, 1.0, 1)]), 6)
            .unwrap();
        assert_eq!(first.apps[0].name, "first");
        let second = acc.close().unwrap();
        assert_eq!(second.apps.len(), 1);
        assert_eq!(second.apps[0].name, "second");
    }

    #[test]
    fn split_apps_keeps_the_top_and_folds_the_rest_exactly() {
        let rows = (0..20)
            .map(|i| Named {
                name: format!("a{i:02}"),
                value: i as f32,
                count: 1,
            })
            .collect();
        let (top, other) = split_apps(rows, Some(0.5));
        assert_eq!(top.len(), APP_N);
        assert_eq!(top[0].name, "a19");
        let tail: f32 = (0..20 - APP_N).map(|i| i as f32).sum();
        assert!((other.unwrap() - (tail + 0.5)).abs() < 1e-4);
        assert_eq!(split_apps(Vec::new(), None), (Vec::new(), None));
    }

    /// Rolling Day buckets up into Month keeps the whole energy: what one
    /// bucket's list dropped is in "other", never lost.
    #[test]
    fn merging_app_lists_preserves_the_total_energy() {
        let snaps = busy_snapshots();
        let mut buckets = Vec::new();
        for (i, s) in snaps.iter().enumerate() {
            let mut acc = Accumulator::new(Tier::Day);
            acc.add(s, i as u64 * 6);
            acc.add(s, i as u64 * 6 + 1);
            buckets.push(acc.close().unwrap());
        }
        let total = |b: &Bucket| {
            (b.apps.iter().map(|a| f64::from(a.value)).sum::<f64>()
                + f64::from(b.apps_other.unwrap()))
                * f64::from(b.samples)
        };
        let want: f64 = buckets.iter().map(total).sum();
        let merged = super::super::merge(&buckets, 0).unwrap();
        assert_eq!(merged.apps.len(), APP_N);
        assert!(
            (total(&merged) - want).abs() < 1e-2,
            "{} vs {want}",
            total(&merged)
        );
    }
}
