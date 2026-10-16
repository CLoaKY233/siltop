//! `siltop bench`: a sustained-performance test.
//!
//! One busy thread per logical core (half with `--light`) while the
//! ordinary sampler reads clocks, CPU power, temperatures, the thermal state
//! and the fans every [`STEP`]. The run ends on time, on Esc or Ctrl-C, or
//! at once when any temperature reaches [`SAFETY_C`]; the load threads are
//! joined on every path. Everything that judges the run is pure and works
//! on [`Sample`]s, so the tests feed synthetic curves and never spin a core.

use std::fmt::Write as _;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde::{Deserialize, Serialize};
use siltop_core::{CpuCluster, Day, History, Result, Snapshot, Thermal, watts_display};
use siltop_ui::{BenchView, Theme, render_bench};

use crate::cli::Cli;

/// How often the bench samples.
pub const STEP: Duration = Duration::from_millis(500);
/// `--duration` when none is given.
pub const DEFAULT_DURATION: Duration = Duration::from_secs(60);
/// The opening window whose median clock is the throttle baseline.
pub const BASELINE: Duration = Duration::from_secs(5);
/// Any temperature at or above this stops the run at once.
pub const SAFETY_C: f32 = 105.0;
/// A clock this far under its baseline counts as throttling.
pub const DROP_LIMIT: f64 = 0.15;
/// Consecutive low samples before a clock drop counts: one slow reading is
/// sampler noise, a second is the chip.
const DROP_RUN: usize = 2;
/// A cluster busy at least this share of the time (median over the run)
/// carried the load. A quieter one, such as the cores `--light` leaves
/// alone, picks its own clock, so its drop is shown but never judged.
pub const LOADED: f32 = 0.5;
/// A fan has responded once it turns this much faster than at the start.
const FAN_RAMP_RPM: u32 = 300;
/// Bumped when a field changes meaning; old saved runs are then skipped.
pub const SCHEMA: u32 = 1;

/// One cluster's clock in one sample.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClusterClock {
    pub name: String,
    pub mhz: u32,
    /// Cores in the cluster, to weight the efficiency figure.
    pub cores: u32,
    /// Share of the time the cluster was busy, 0..=1.
    pub busy: f32,
}

/// What the bench keeps from one snapshot.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sample {
    /// Time since the load started.
    pub at: Duration,
    pub clusters: Vec<ClusterClock>,
    pub cpu_w: Option<f32>,
    /// The hottest CPU reading: package, zones or hotspot.
    pub temp_c: Option<f32>,
    pub thermal: Thermal,
    /// The fastest fan; `None` on a fanless Mac.
    pub fan_rpm: Option<u32>,
}

impl Sample {
    #[must_use]
    pub fn from_snapshot(snap: &Snapshot, at: Duration) -> Self {
        let all = &snap.cpu.clusters;
        let clusters = all
            .iter()
            .filter_map(|c| {
                let mhz = c.freq_mhz.filter(|mhz| *mhz > 0)?;
                let siblings = all.iter().filter(|o| o.kind == c.kind).count().max(1);
                let cores = snap
                    .cpu
                    .cores
                    .iter()
                    .filter(|core| core.kind == c.kind)
                    .count()
                    / siblings;
                Some(ClusterClock {
                    name: cluster_label(all, c),
                    mhz,
                    cores: u32::try_from(cores).unwrap_or(u32::MAX).max(1),
                    busy: c.active,
                })
            })
            .collect();
        let sensors = &snap.sensors;
        let temp_c = [
            sensors.best_cpu_c(),
            sensors.e_c,
            sensors.p_c,
            sensors.s_c,
            sensors.hotspot_c,
        ]
        .into_iter()
        .flatten()
        .filter(|c| c.is_finite())
        .reduce(f32::max);
        Self {
            at,
            clusters,
            cpu_w: snap.power.cpu.map(|r| r.watts).or(snap.cpu.watts),
            temp_c,
            thermal: snap.thermal,
            fan_rpm: snap.fans.fans.iter().map(|f| u32::from(f.rpm)).max(),
        }
    }
}

/// Serious or worse: macOS is already holding the chip back.
const fn is_throttled_state(thermal: Thermal) -> bool {
    matches!(thermal, Thermal::Serious | Thermal::Critical)
}

/// Why the run ended.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Stop {
    Completed,
    /// Esc or Ctrl-C.
    Cancelled,
    /// A reading reached [`SAFETY_C`].
    TooHot {
        celsius: f32,
    },
}

/// One cluster's clocks over the run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClusterResult {
    pub name: String,
    /// The single fastest reading, a boost spike included.
    pub peak_mhz: u32,
    /// Median of the first [`BASELINE`]: the clock under load before heat.
    pub baseline_mhz: u32,
    /// Median of the last third of the run.
    pub sustained_mhz: u32,
    /// How far sustained fell under the baseline, in percent (never
    /// negative). Measured against the early clock under load, not
    /// [`ClusterResult::peak_mhz`], so one boost reading is not a drop.
    pub drop_pct: f64,
    /// Median busy share over the run, in percent; `None` in runs saved
    /// before it was kept.
    #[serde(default)]
    pub busy_pct: Option<f64>,
    /// Whether the load reached this cluster ([`LOADED`]). An unloaded
    /// cluster's drop never decides the verdict.
    #[serde(default = "loaded_by_default")]
    pub loaded: bool,
}

/// Runs saved before `loaded` was kept loaded every core.
const fn loaded_by_default() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FanResult {
    pub start_rpm: u32,
    pub peak_rpm: u32,
    /// Seconds until the fans sped up; `None` if they never did.
    pub ramp_s: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Throttled,
    /// Stopped early (Esc, Ctrl-C or the safety limit) before any throttle:
    /// too short a run to pass.
    Incomplete,
}

/// The report card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub schema: u32,
    pub chip: String,
    pub light: bool,
    pub threads: usize,
    /// UTC day the run started, `YYYY-MM-DD`.
    pub date: String,
    pub planned_s: f64,
    pub ran_s: f64,
    pub samples: usize,
    pub stop: Stop,
    pub clusters: Vec<ClusterResult>,
    pub verdict: Verdict,
    /// Seconds into the run of the first throttle.
    pub throttle_s: Option<f64>,
    /// `thermal state serious` or `P-cluster clock -18%`.
    pub throttle_cause: Option<String>,
    pub peak_temp_c: Option<f32>,
    pub avg_cpu_w: Option<f32>,
    /// Core-weighted sustained MHz per CPU watt.
    pub mhz_per_w: Option<f64>,
    pub fans: Option<FanResult>,
}

/// Facts about the run that are not in the samples.
#[derive(Debug, Clone, PartialEq)]
pub struct Meta {
    pub chip: String,
    pub light: bool,
    pub threads: usize,
    pub day: Day,
    pub planned: Duration,
}

fn median(mut values: Vec<u32>) -> Option<u32> {
    values.sort_unstable();
    let n = values.len();
    match n {
        0 => None,
        _ if n % 2 == 1 => Some(values[n / 2]),
        _ => Some(u32::midpoint(values[n / 2 - 1], values[n / 2])),
    }
}

/// Median of busy shares; NaN readings are skipped.
fn median_busy(mut values: Vec<f32>) -> Option<f32> {
    values.retain(|v| v.is_finite());
    values.sort_unstable_by(f32::total_cmp);
    let n = values.len();
    match n {
        0 => None,
        _ if n % 2 == 1 => Some(values[n / 2]),
        _ => Some(f32::midpoint(values[n / 2 - 1], values[n / 2])),
    }
}

fn pct_below(reference: u32, value: u32) -> f64 {
    if reference == 0 {
        return 0.0;
    }
    (f64::from(reference) - f64::from(value)) / f64::from(reference) * 100.0
}

/// Every cluster name in first-seen order.
fn cluster_names(samples: &[Sample]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for clock in samples.iter().flat_map(|s| &s.clusters) {
        if !names.contains(&clock.name) {
            names.push(clock.name.clone());
        }
    }
    names
}

fn clusters_of<'a>(
    samples: &'a [Sample],
    name: &'a str,
) -> impl Iterator<Item = (Duration, &'a ClusterClock)> + 'a {
    samples.iter().filter_map(move |s| {
        s.clusters
            .iter()
            .find(|c| c.name == name)
            .map(|c| (s.at, c))
    })
}

fn clocks_of<'a>(
    samples: &'a [Sample],
    name: &'a str,
) -> impl Iterator<Item = (Duration, u32)> + 'a {
    clusters_of(samples, name).map(|(at, c)| (at, c.mhz))
}

fn cluster_result(samples: &[Sample], name: &str) -> Option<ClusterResult> {
    let clocks: Vec<(Duration, u32)> = clocks_of(samples, name).collect();
    let peak = clocks.iter().map(|(_, mhz)| *mhz).max()?;
    let baseline = median(
        clocks
            .iter()
            .filter(|(at, _)| *at <= BASELINE)
            .map(|(_, mhz)| *mhz)
            .collect(),
    )
    .unwrap_or(peak);
    let tail = clocks.len() - clocks.len() / 3 * 2;
    let sustained = median(
        clocks[clocks.len() - tail..]
            .iter()
            .map(|(_, m)| *m)
            .collect(),
    )
    .unwrap_or(peak);
    let busy = median_busy(clusters_of(samples, name).map(|(_, c)| c.busy).collect());
    Some(ClusterResult {
        name: name.to_owned(),
        peak_mhz: peak,
        baseline_mhz: baseline,
        sustained_mhz: sustained,
        drop_pct: pct_below(baseline, sustained).max(0.0),
        busy_pct: busy.map(|b| (f64::from(b) * 100.0).round()),
        loaded: busy.is_none_or(|b| b >= LOADED),
    })
}

/// A clock under this is more than [`DROP_LIMIT`] under the baseline.
fn floor(cluster: &ClusterResult) -> f64 {
    f64::from(cluster.baseline_mhz) * (1.0 - DROP_LIMIT)
}

/// The first throttle: a serious thermal state, or, after the baseline
/// window, [`DROP_RUN`] samples in a row more than [`DROP_LIMIT`] under a
/// loaded cluster's baseline. Failing both, a loaded cluster whose
/// sustained clock ended more than [`DROP_LIMIT`] under it (dips that never
/// ran two in a row but held the median down) throttled at its first low
/// reading: a drop over the limit never sits next to a pass. Returns its
/// time and cause.
fn first_throttle(samples: &[Sample], clusters: &[ClusterResult]) -> Option<(Duration, String)> {
    let thermal = samples
        .iter()
        .find(|s| is_throttled_state(s.thermal))
        .map(|s| {
            let word = s.thermal.word().unwrap_or("serious");
            (s.at, format!("thermal state {word}"))
        });
    let loaded = || clusters.iter().filter(|c| c.loaded);
    let drops = loaded().filter_map(|c| {
        let floor = floor(c);
        let mut run = 0;
        clocks_of(samples, &c.name)
            .filter(|(at, _)| *at > BASELINE)
            .find_map(|(at, mhz)| {
                run = if f64::from(mhz) < floor { run + 1 } else { 0 };
                (run == DROP_RUN).then(|| {
                    let pct = pct_below(c.baseline_mhz, mhz).round();
                    (at, format!("{} clock -{pct:.0}%", c.name))
                })
            })
    });
    let mut sustained = loaded()
        .filter(|c| c.drop_pct > DROP_LIMIT * 100.0)
        .filter_map(|c| {
            let floor = floor(c);
            let low = |(_, mhz): &(Duration, u32)| f64::from(*mhz) < floor;
            // A median under the floor means some reading is under it too.
            let (at, _) = clocks_of(samples, &c.name)
                .filter(|(at, _)| *at > BASELINE)
                .find(low)
                .or_else(|| clocks_of(samples, &c.name).find(low))?;
            Some((at, format!("{} clock -{:.0}%", c.name, c.drop_pct.round())))
        });
    thermal
        .into_iter()
        .chain(drops)
        .min_by_key(|(at, _)| *at)
        .or_else(|| sustained.next())
}

fn fan_result(samples: &[Sample]) -> Option<FanResult> {
    let mut fans = samples
        .iter()
        .filter_map(|s| s.fan_rpm.map(|rpm| (s.at, rpm)));
    let (_, start) = fans.next()?;
    let mut peak = start;
    let mut ramp = None;
    for (at, rpm) in fans {
        peak = peak.max(rpm);
        if ramp.is_none() && rpm >= start + FAN_RAMP_RPM {
            ramp = Some(at.as_secs_f64());
        }
    }
    Some(FanResult {
        start_rpm: start,
        peak_rpm: peak,
        ramp_s: ramp,
    })
}

fn mean(values: impl Iterator<Item = f32>) -> Option<f32> {
    let (sum, n) = values
        .filter(|v| v.is_finite())
        .fold((0.0_f64, 0_u32), |(s, n), v| (s + f64::from(v), n + 1));
    (n > 0).then(|| (sum / f64::from(n)) as f32)
}

/// Judges a finished run.
#[must_use]
pub fn analyze(samples: &[Sample], meta: &Meta, stop: Stop) -> Report {
    let clusters: Vec<ClusterResult> = cluster_names(samples)
        .iter()
        .filter_map(|name| cluster_result(samples, name))
        .collect();
    let throttle = first_throttle(samples, &clusters);
    let avg_cpu_w = mean(samples.iter().filter_map(|s| s.cpu_w));
    let weights: Vec<(f64, f64)> = clusters
        .iter()
        .map(|c| {
            let cores = samples
                .iter()
                .flat_map(|s| &s.clusters)
                .find(|clock| clock.name == c.name)
                .map_or(1, |clock| clock.cores);
            (f64::from(c.sustained_mhz), f64::from(cores))
        })
        .collect();
    let total_cores: f64 = weights.iter().map(|(_, w)| w).sum();
    let mhz_per_w = avg_cpu_w
        .filter(|w| *w > 0.1 && total_cores > 0.0)
        .map(|w| weights.iter().map(|(m, c)| m * c).sum::<f64>() / total_cores / f64::from(w));
    Report {
        schema: SCHEMA,
        chip: meta.chip.clone(),
        light: meta.light,
        threads: meta.threads,
        date: meta.day.to_string(),
        planned_s: meta.planned.as_secs_f64(),
        ran_s: samples.last().map_or(0.0, |s| s.at.as_secs_f64()),
        samples: samples.len(),
        stop,
        clusters,
        // A throttle seen before an early stop still counts; a pass needs
        // the whole planned run.
        verdict: match (&throttle, stop) {
            (Some(_), _) => Verdict::Throttled,
            (None, Stop::Completed) => Verdict::Pass,
            (None, _) => Verdict::Incomplete,
        },
        throttle_s: throttle.as_ref().map(|(at, _)| at.as_secs_f64()),
        throttle_cause: throttle.map(|(_, cause)| cause),
        peak_temp_c: samples.iter().filter_map(|s| s.temp_c).reduce(f32::max),
        avg_cpu_w,
        mhz_per_w,
        fans: fan_result(samples),
    }
}

/// How this run's clocks compare with the last comparable one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comparison {
    pub previous_date: String,
    /// Sustained clocks relative to the previous run, in percent (negative
    /// is slower).
    pub clock_pct: f64,
}

fn weighted_sustained(report: &Report) -> Option<f64> {
    let n = report.clusters.len();
    (n > 0).then(|| {
        report
            .clusters
            .iter()
            .map(|c| f64::from(c.sustained_mhz))
            .sum::<f64>()
            / n as f64
    })
}

/// Same chip, same load, a run that went its full length.
fn comparable(now: &Report, before: &Report) -> bool {
    before.schema == SCHEMA
        && before.chip == now.chip
        && before.light == now.light
        && before.stop == Stop::Completed
}

#[must_use]
pub fn compare(now: &Report, before: &Report) -> Option<Comparison> {
    if !comparable(now, before) {
        return None;
    }
    let (a, b) = (weighted_sustained(now)?, weighted_sustained(before)?);
    (b > 0.0).then(|| Comparison {
        previous_date: before.date.clone(),
        clock_pct: (a - b) / b * 100.0,
    })
}

impl Comparison {
    /// `clocks 3% lower than your last run on 2026-09-20`.
    #[must_use]
    pub fn sentence(&self) -> String {
        let pct = self.clock_pct.abs().round();
        let date = &self.previous_date;
        if pct < 1.0 {
            format!("clocks match your last run on {date}")
        } else {
            let way = if self.clock_pct < 0.0 {
                "lower"
            } else {
                "higher"
            };
            format!("clocks {pct:.0}% {way} than your last run on {date}")
        }
    }
}

/// Saved runs: one JSON report per line, newest last.
#[must_use]
pub fn runs_path(state_root: &Path) -> PathBuf {
    state_root.join("bench.ndjson")
}

/// The newest saved run this one can be compared with. Unreadable lines
/// are skipped: a damaged file never stops a bench. The file is text from
/// disk, so every string in the run it returns is made printable.
#[must_use]
pub fn previous(path: &Path, now: &Report) -> Option<Report> {
    let file = std::fs::File::open(path).ok()?;
    io::BufReader::new(file)
        .lines()
        .map_while(std::result::Result::ok)
        .filter_map(|line| serde_json::from_str::<Report>(&line).ok())
        .map(Report::into_printable)
        .filter(|before| comparable(now, before))
        .last()
}

impl Report {
    /// Every string made [`siltop_core::printable`].
    #[must_use]
    pub fn into_printable(mut self) -> Self {
        use siltop_core::into_printable as clean;
        self.chip = clean(self.chip);
        self.date = clean(self.date);
        self.throttle_cause = self.throttle_cause.map(clean);
        for cluster in &mut self.clusters {
            cluster.name = clean(std::mem::take(&mut cluster.name));
        }
        self
    }
}

/// Appends a finished run.
pub fn save(path: &Path, report: &Report) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let line = serde_json::to_string(report).map_err(io::Error::other)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{line}")
}

/// `mm:ss`.
#[must_use]
pub fn mmss(secs: f64) -> String {
    let total = secs.max(0.0).round() as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

/// `PASS: no throttling`, `THROTTLED at 00:42 (thermal state serious)` or
/// `incomplete: stopped after 00:03, no verdict`.
#[must_use]
pub fn verdict_line(report: &Report) -> String {
    match (report.verdict, report.throttle_s) {
        (Verdict::Throttled, Some(at)) => {
            let cause = report.throttle_cause.as_deref().unwrap_or("clock drop");
            format!("THROTTLED at {} ({cause})", mmss(at))
        }
        (Verdict::Incomplete, _) => format!(
            "incomplete: stopped after {}, no verdict",
            mmss(report.ran_s)
        ),
        _ => String::from("PASS: no throttling"),
    }
}

/// `84°C`: the card is read outside the dashboard, so it names the unit.
fn degrees(c: f32) -> String {
    format!("{:.0}°C", c.round())
}

fn stop_line(stop: Stop) -> Option<String> {
    match stop {
        Stop::Completed => None,
        Stop::Cancelled => Some(String::from("early, by Esc or Ctrl-C")),
        Stop::TooHot { celsius: c } => Some(format!(
            "stopped at {} for safety (limit {})",
            degrees(c),
            degrees(SAFETY_C)
        )),
    }
}

/// `early 4590 → sustained 4420 MHz · drop 3.7% · peak 4608 MHz`: the drop
/// reads straight off the two clocks before it. A cluster the load never
/// reached says so instead of showing a drop that means nothing.
fn cluster_line(c: &ClusterResult) -> String {
    if !c.loaded {
        let busy = c.busy_pct.map_or_else(
            || String::from("not loaded"),
            |pct| format!("only {pct:.0}% busy"),
        );
        return format!(
            "sustained {} MHz · peak {} MHz · {busy}, drop not judged",
            c.sustained_mhz, c.peak_mhz
        );
    }
    format!(
        "early {} → sustained {} MHz · drop {:.1}% · peak {} MHz",
        c.baseline_mhz, c.sustained_mhz, c.drop_pct, c.peak_mhz
    )
}

/// `1200 → 4800 rpm, ramped at 00:12`; a fan at rest reads `off`, never
/// `0 rpm`.
fn fan_line(f: FanResult) -> String {
    let rpm = |rpm: u32| {
        if rpm == 0 {
            String::from("off")
        } else {
            format!("{rpm} rpm")
        }
    };
    match (f.peak_rpm, f.ramp_s) {
        (0, _) => String::from("stayed off"),
        (_, Some(at)) => format!(
            "{} → {}, ramped at {}",
            rpm(f.start_rpm),
            rpm(f.peak_rpm),
            mmss(at)
        ),
        (_, None) => format!("{} → {}, never ramped", rpm(f.start_rpm), rpm(f.peak_rpm)),
    }
}

/// The plain-text report card. Absent hardware (no fans, no power rail)
/// leaves its row out rather than printing a zero.
#[must_use]
pub fn render_text(report: &Report, comparison: Option<&Comparison>) -> String {
    let mut out = String::new();
    let load = if report.light { " light" } else { "" };
    let _ = writeln!(
        out,
        "siltop bench{load}  {} · {} threads · {} of {}",
        report.chip,
        report.threads,
        mmss(report.ran_s),
        mmss(report.planned_s)
    );
    // Wide enough for `performance 1` plus a gap; never narrower than the
    // fixed keys.
    let width = report
        .clusters
        .iter()
        .map(|c| c.name.chars().count() + 1)
        .fold(12, usize::max);
    let row = |out: &mut String, key: &str, value: String| {
        let _ = writeln!(out, "  {key:<width$}{value}");
    };
    for c in &report.clusters {
        row(&mut out, &c.name, cluster_line(c));
    }
    if let Some(t) = report.peak_temp_c {
        row(&mut out, "peak temp", degrees(t));
    }
    if let Some(w) = report.avg_cpu_w {
        let eff = report
            .mhz_per_w
            .map_or_else(String::new, |e| format!(" · {e:.0} MHz/W"));
        row(
            &mut out,
            "CPU power",
            format!("avg {}{eff}", watts_display(w)),
        );
    }
    if let Some(f) = report.fans {
        row(&mut out, "fans", fan_line(f));
    }
    if let Some(line) = stop_line(report.stop) {
        row(&mut out, "stopped", line);
    }
    row(&mut out, "verdict", verdict_line(report));
    if let Some(c) = comparison {
        row(&mut out, "vs last", c.sentence());
    }
    out
}

/// `--json`: the report with the comparison folded in.
#[derive(Serialize)]
struct JsonOut<'a> {
    #[serde(flatten)]
    report: &'a Report,
    previous: Option<&'a Comparison>,
}

pub fn render_json(report: &Report, comparison: Option<&Comparison>) -> Result<String> {
    serde_json::to_string_pretty(&JsonOut {
        report,
        previous: comparison,
    })
    .map_err(|e| siltop_core::Error::system(format!("bench json: {e}")))
}

/// Something that loads the CPU. The real one spins threads; tests count
/// calls.
pub trait Load {
    fn start(&mut self, threads: usize) -> Result<()>;
    /// Stops and joins every thread. Safe to call twice.
    fn stop(&mut self);
}

/// Busy threads that spin until told to stop; joined on drop too.
#[derive(Default)]
pub struct Spinners {
    stop: Arc<AtomicBool>,
    handles: Vec<JoinHandle<()>>,
}

impl Load for Spinners {
    fn start(&mut self, threads: usize) -> Result<()> {
        self.stop.store(false, Ordering::SeqCst);
        for i in 0..threads {
            let stop = Arc::clone(&self.stop);
            let spawned = std::thread::Builder::new()
                .name(format!("siltop-bench-{i}"))
                .spawn(move || spin(&stop));
            match spawned {
                Ok(handle) => self.handles.push(handle),
                Err(e) => {
                    self.stop();
                    return Err(siltop_core::Error::system(format!("bench thread: {e}")));
                }
            }
        }
        Ok(())
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

impl Drop for Spinners {
    fn drop(&mut self) {
        Load::stop(self);
    }
}

/// Integer and float work the optimiser cannot drop.
fn spin(stop: &AtomicBool) {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut f = 1.000_001_f64;
    while !stop.load(Ordering::Relaxed) {
        for _ in 0..4096 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            f = f.mul_add(1.000_000_1, 1e-9);
        }
        std::hint::black_box((x, f));
    }
}

/// Load threads for this Mac: every logical core, or half with `--light`.
#[must_use]
pub fn thread_count(logical: usize, light: bool) -> usize {
    let logical = logical.max(1);
    if light { logical.div_ceil(2) } else { logical }
}

/// What a run needs up front.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub duration: Duration,
    pub threads: usize,
}

/// Where the run is, for the live page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress<'a> {
    pub chip: &'a str,
    pub sample: &'a Sample,
    pub remaining: Duration,
}

/// The dashboard's word for a cluster (`super`, `performance`), numbered
/// in hardware order when a chip has several of one kind (`performance 0`,
/// `performance 1` on an M5 Pro). The `IOReport` channel name (`MCPU0`)
/// never reaches the user.
fn cluster_label(all: &[CpuCluster], cluster: &CpuCluster) -> String {
    let mut same: Vec<&CpuCluster> = all.iter().filter(|c| c.kind == cluster.kind).collect();
    let word = cluster.kind.word();
    if same.len() < 2 {
        return String::from(word);
    }
    same.sort_by_key(|c| c.id);
    let index = same
        .iter()
        .position(|c| c.id == cluster.id)
        .unwrap_or_default();
    format!("{word} {index}")
}

/// The finished run: samples and why it ended.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub chip: String,
    pub samples: Vec<Sample>,
    pub stop: Stop,
}

/// Runs the bench. Time is counted in steps, not read from a clock, so a
/// test's `wait` can return at once. The load is stopped and joined on
/// every path out, errors included.
pub fn run<T, W>(
    tick: &mut T,
    wait: &mut W,
    stop: &dyn Fn() -> bool,
    load: &mut dyn Load,
    plan: Plan,
    observe: &mut dyn FnMut(Progress<'_>),
) -> Result<Outcome>
where
    T: FnMut() -> Result<Snapshot>,
    W: FnMut(Duration),
{
    // Rates need one interval before they read true.
    let chip = siltop_core::into_printable(tick()?.soc.name);
    load.start(plan.threads)?;
    let result = sample_loop(tick, wait, stop, plan, &chip, observe);
    load.stop();
    let (samples, stop) = result?;
    Ok(Outcome {
        chip,
        samples,
        stop,
    })
}

fn sample_loop<T, W>(
    tick: &mut T,
    wait: &mut W,
    stop: &dyn Fn() -> bool,
    plan: Plan,
    chip: &str,
    observe: &mut dyn FnMut(Progress<'_>),
) -> Result<(Vec<Sample>, Stop)>
where
    T: FnMut() -> Result<Snapshot>,
    W: FnMut(Duration),
{
    let mut samples = Vec::new();
    let mut at = Duration::ZERO;
    loop {
        if stop() {
            return Ok((samples, Stop::Cancelled));
        }
        wait(STEP);
        if stop() {
            return Ok((samples, Stop::Cancelled));
        }
        at += STEP;
        let sample = Sample::from_snapshot(&tick()?, at);
        let hot = sample.temp_c.filter(|c| *c >= SAFETY_C);
        observe(Progress {
            chip,
            sample: &sample,
            remaining: plan.duration.saturating_sub(at),
        });
        samples.push(sample);
        if let Some(c) = hot {
            return Ok((samples, Stop::TooHot { celsius: c }));
        }
        if at >= plan.duration {
            return Ok((samples, Stop::Completed));
        }
    }
}

/// `bench --demo`: the canned M5 Pro under the bench's own load, not the
/// dashboard session (which idles between bursts and read as a 75% clock
/// drop beside PASS). From the first sample every loaded core is busy and
/// the clocks sit near the top of their tables, settling a few percent as
/// the chip warms; the fans wake and nothing reaches a serious thermal
/// state: a clean pass. With `--light` only half the threads run, so the
/// Super cores carry them and most Performance cores idle, as on a real Mac.
pub struct DemoRun {
    step: Duration,
    elapsed: Duration,
    light: bool,
}

impl DemoRun {
    #[must_use]
    pub const fn new(step: Duration, light: bool) -> Self {
        Self {
            step,
            elapsed: Duration::ZERO,
            light,
        }
    }
}

impl crate::worker::Tick for DemoRun {
    fn tick(&mut self) -> Result<Snapshot> {
        let snap = demo_frame(self.elapsed, self.light);
        self.elapsed += self.step;
        Ok(snap)
    }

    /// The canned battery is not this Mac's.
    fn on_battery(&self, _snap: &Snapshot) -> bool {
        false
    }
}

/// Deterministic wobble in 0..=1, as the demo session's.
fn wobble(t: f32, seed: f32) -> f32 {
    let a = (t * 1.7 + seed * 2.3).sin();
    let b = (t * 0.41 + seed * 5.1).sin();
    (0.5 + 0.35 * a + 0.15 * b).clamp(0.0, 1.0)
}

/// One `bench --demo` frame `elapsed` into the run: the session's calm
/// opening second (memory, disks, processes) with the CPU, its heat, power
/// and fans recast as a loaded run. Clusters, cores and fans keep the
/// session's own chip, so the card names the same Mac as `--demo`.
#[must_use]
pub fn demo_frame(elapsed: Duration, light: bool) -> Snapshot {
    use siltop_core::ClusterKind;
    let t = elapsed.as_secs_f32();
    // Warms toward a plateau over the first minute or so.
    let heat = 1.0 - (-t / 25.0).exp();
    let load = if light { 0.6 } else { 1.0 };
    let mut snap = crate::demo::frame(Duration::from_secs(2), Instant::now());
    // Threads land on the Super cores first; `--light` leaves all but three
    // Performance cores idle, so that cluster's clock wanders.
    let loaded = |kind: ClusterKind, index: u16| !light || kind == ClusterKind::Super || index < 3;
    let busy = |kind: ClusterKind, index: u16| {
        let seed = f32::from(index);
        if loaded(kind, index) {
            0.96 + 0.04 * wobble(t * 2.0, seed)
        } else {
            0.04 + 0.08 * wobble(t, seed + 9.0)
        }
    };
    let mhz = |kind: ClusterKind, max: u32| -> u32 {
        let (settle, seed) = if kind == ClusterKind::Super {
            (0.05, 1.0)
        } else {
            (0.035, 2.0)
        };
        let max = max as f32;
        if light && kind != ClusterKind::Super {
            return (max * (0.55 + 0.3 * wobble(t * 1.3, seed))) as u32;
        }
        let jitter = 0.006 * (wobble(t * 3.0, seed) - 0.5);
        (max * (0.99 - settle * load * heat + jitter)) as u32
    };
    let cpu = &mut snap.cpu;
    for cluster in &mut cpu.clusters {
        let clock = mhz(cluster.kind, cluster.max_mhz);
        let ratio = clock as f32 / cluster.max_mhz.max(1) as f32;
        let mut sum = 0.0;
        let mut n = 0_u16;
        for core in cpu
            .cores
            .iter_mut()
            .filter(|core| core.kind == cluster.kind)
        {
            core.active = busy(core.kind, core.index);
            core.scaled = core.active * ratio;
            core.freq_mhz = Some(clock);
            sum += core.active;
            n += 1;
        }
        cluster.active = sum / f32::from(n.max(1));
        cluster.scaled = cluster.active * ratio;
        cluster.freq_mhz = Some(clock);
        for summary in [&mut cpu.e_cluster, &mut cpu.p_cluster, &mut cpu.s_cluster]
            .into_iter()
            .flatten()
            .filter(|summary| summary.kind == cluster.kind)
        {
            summary.active = cluster.active;
            summary.scaled = cluster.scaled;
            summary.freq_mhz = clock;
        }
    }
    let cores = cpu.cores.len().max(1) as f32;
    cpu.active = cpu.cores.iter().map(|c| c.active).sum::<f32>() / cores;
    cpu.scaled = cpu.cores.iter().map(|c| c.scaled).sum::<f32>() / cores;
    cpu.freq_mhz = cpu.clusters.iter().filter_map(|c| c.freq_mhz).max();
    let watts = load * (36.0 - 3.0 * heat) + 0.8 * wobble(t, 3.0);
    let temp = 47.0 + load * 37.0 * heat + 1.5 * wobble(t, 5.0);
    cpu.watts = Some(watts);
    cpu.temp_c = Some(temp);
    snap.sensors.cpu_c = Some(temp);
    snap.sensors.p_c = Some(temp - 2.0);
    snap.sensors.s_c = Some(temp + 1.0);
    snap.sensors.hotspot_c = Some(temp + 3.0);
    let warm = ((heat * load - 0.3) / 0.7).clamp(0.0, 1.0);
    for fan in &mut snap.fans.fans {
        fan.rpm = if warm <= 0.0 {
            0
        } else {
            (1_200.0 + (f32::from(fan.max_rpm) * 0.7 - 1_200.0) * warm) as u16
        };
    }
    snap.thermal = if heat * load > 0.8 {
        Thermal::Fair
    } else {
        Thermal::Nominal
    };
    snap.power.cpu = Some(siltop_core::Reading::measured(watts));
    snap
}

/// `--demo`: canned numbers, so nothing to load.
struct NoLoad;

impl Load for NoLoad {
    fn start(&mut self, _threads: usize) -> Result<()> {
        Ok(())
    }
    fn stop(&mut self) {}
}

/// Keys that end a live bench early: Esc, q and Ctrl-C (raw mode turns
/// Ctrl-C into a key, not a signal).
#[must_use]
pub fn is_stop_key(key: KeyEvent) -> bool {
    key.kind != KeyEventKind::Release
        && match key.code {
            KeyCode::Esc | KeyCode::Char('q') => true,
            KeyCode::Char('c') => key.modifiers.contains(KeyModifiers::CONTROL),
            _ => false,
        }
}

/// Saved runs sit next to the history store.
fn runs_file() -> Option<PathBuf> {
    let history = crate::store_disk::default_root()?;
    history.parent().map(runs_path)
}

/// The histories the live page draws.
struct Live {
    clock: History,
    temp: History,
    watts: History,
}

impl Live {
    /// One dot per [`STEP`], so the time axis reads true.
    fn new() -> Self {
        Self {
            clock: History::for_period(STEP),
            temp: History::for_period(STEP),
            watts: History::for_period(STEP),
        }
    }

    fn push(&mut self, sample: &Sample) {
        let at = u64::try_from(sample.at.as_millis()).unwrap_or(u64::MAX);
        if let Some(mhz) = sample.clusters.iter().map(|c| c.mhz).max() {
            self.clock.push_at(mhz as f32, at);
        }
        if let Some(c) = sample.temp_c {
            self.temp.push_at(c, at);
        }
        if let Some(w) = sample.cpu_w {
            self.watts.push_at(w, at);
        }
    }
}

/// Waits `step` while reading keys; a stop key sets `stopped`.
fn wait_for_keys(step: Duration, stopped: &AtomicBool) {
    let deadline = Instant::now() + step;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || !event::poll(left).unwrap_or(false) {
            return;
        }
        if let Ok(Event::Key(key)) = event::read()
            && is_stop_key(key)
        {
            stopped.store(true, Ordering::SeqCst);
            return;
        }
    }
}

/// `siltop bench`: run, judge, save, print.
pub fn command(cli: &Cli, live: bool, out: &mut dyn Write, err: &mut dyn Write) -> Result<()> {
    let logical = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let plan = Plan {
        duration: cli.duration.unwrap_or(DEFAULT_DURATION),
        threads: thread_count(logical, cli.bench_light),
    };
    // The demo plays a canned loaded run, not the dashboard's session.
    let mut sampler: Box<dyn crate::worker::Tick> = if cli.demo {
        Box::new(DemoRun::new(STEP, cli.bench_light))
    } else {
        crate::sampler(false, STEP)?
    };
    let (tx, _rx) = std::sync::mpsc::channel();
    let shutdown = crate::signals::install(tx)?;
    let mut load: Box<dyn Load> = if cli.demo {
        Box::new(NoLoad)
    } else {
        Box::new(Spinners::default())
    };
    let mut tick = || sampler.tick();
    let live = live && !cli.report_json;
    let outcome = if live {
        let config = crate::cli_config(cli);
        let theme = Theme::by_name(&config.theme).unwrap_or_default();
        let stopped = AtomicBool::new(false);
        let mut screen = crate::tui::install()?;
        let mut graphs = Live::new();
        let mut drawn = Ok(());
        let result = run(
            &mut tick,
            &mut |step| wait_for_keys(step, &stopped),
            &|| stopped.load(Ordering::SeqCst) || shutdown.requested(),
            load.as_mut(),
            plan,
            &mut |progress| {
                graphs.push(progress.sample);
                let view = BenchView {
                    chip: progress.chip,
                    threads: plan.threads,
                    duration: plan.duration,
                    remaining: progress.remaining,
                    clock: &graphs.clock,
                    temp: Some(&graphs.temp),
                    watts: Some(&graphs.watts),
                };
                if drawn.is_ok() {
                    drawn = screen.draw(|frame| render_bench(frame, &view, &theme));
                }
            },
        );
        crate::tui::restore()?;
        drawn.map_err(|e| siltop_core::Error::terminal(e.to_string()))?;
        result?
    } else {
        let doing = if cli.demo {
            String::from("playing the demo (no load)")
        } else {
            format!("loading {} threads", plan.threads)
        };
        let _ = writeln!(
            err,
            "siltop: bench {doing} for {} (Ctrl-C stops)",
            mmss(plan.duration.as_secs_f64())
        );
        run(
            &mut tick,
            &mut std::thread::sleep,
            &|| shutdown.requested(),
            load.as_mut(),
            plan,
            &mut |_| {},
        )?
    };
    let meta = Meta {
        chip: outcome.chip,
        light: cli.bench_light,
        threads: plan.threads,
        day: Day::today(),
        planned: plan.duration,
    };
    let report = analyze(&outcome.samples, &meta, outcome.stop);
    // The demo's canned numbers never touch this Mac's saved runs.
    let file = runs_file().filter(|_| !cli.demo);
    let comparison = file
        .as_deref()
        .and_then(|path| previous(path, &report))
        .and_then(|before| compare(&report, &before));
    if let Some(path) = &file
        && let Err(e) = save(path, &report)
    {
        let _ = writeln!(err, "siltop: could not save the bench: {e}");
    }
    let text = if cli.report_json {
        render_json(&report, comparison.as_ref())? + "\n"
    } else {
        render_text(&report, comparison.as_ref())
    };
    out.write_all(text.as_bytes())?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    fn sample(at: f64, p: u32, e: u32, temp: f32, w: f32) -> Sample {
        Sample {
            at: secs(at),
            clusters: vec![
                ClusterClock {
                    name: String::from("P-cluster"),
                    mhz: p,
                    cores: 10,
                    busy: 1.0,
                },
                ClusterClock {
                    name: String::from("E-cluster"),
                    mhz: e,
                    cores: 4,
                    busy: 1.0,
                },
            ],
            cpu_w: Some(w),
            temp_c: Some(temp),
            thermal: Thermal::Nominal,
            fan_rpm: None,
        }
    }

    /// 60 s at 500 ms with `p(t)` as the P clock.
    fn curve(p: impl Fn(f64) -> u32) -> Vec<Sample> {
        (1..=120)
            .map(|i| {
                let t = f64::from(i) * 0.5;
                sample(t, p(t), 2600, 60.0 + t as f32 / 2.0, 30.0)
            })
            .collect()
    }

    fn meta() -> Meta {
        Meta {
            chip: String::from("Apple M4 Pro"),
            light: false,
            threads: 14,
            day: Day::from_ymd(2026, 9, 24).unwrap(),
            planned: Duration::from_secs(60),
        }
    }

    #[test]
    fn steady_clocks_pass_with_zero_drop() {
        let report = analyze(&curve(|_| 4500), &meta(), Stop::Completed);
        assert_eq!(report.verdict, Verdict::Pass);
        assert_eq!(report.throttle_s, None);
        let p = &report.clusters[0];
        assert_eq!(
            (p.peak_mhz, p.sustained_mhz, p.baseline_mhz),
            (4500, 4500, 4500)
        );
        assert!(p.drop_pct.abs() < f64::EPSILON);
        assert_eq!(verdict_line(&report), "PASS: no throttling");
    }

    #[test]
    fn sustained_is_the_last_third_and_drop_is_against_the_early_clock() {
        let report = analyze(
            &curve(|t| if t < 40.0 { 4500 } else { 4050 }),
            &meta(),
            Stop::Completed,
        );
        let p = &report.clusters[0];
        assert_eq!(p.peak_mhz, 4500);
        assert_eq!(p.sustained_mhz, 4050);
        assert!((p.drop_pct - 10.0).abs() < 1e-9, "{}", p.drop_pct);
        // 10% is under the throttle line.
        assert_eq!(report.verdict, Verdict::Pass);
    }

    #[test]
    fn clock_drop_over_15_percent_throttles_at_its_second_sample() {
        let report = analyze(
            &curve(|t| if t < 42.0 { 4500 } else { 3600 }),
            &meta(),
            Stop::Completed,
        );
        assert_eq!(report.verdict, Verdict::Throttled);
        assert_eq!(report.throttle_s, Some(42.5));
        assert_eq!(
            report.throttle_cause.as_deref(),
            Some("P-cluster clock -20%")
        );
        assert_eq!(
            verdict_line(&report),
            "THROTTLED at 00:43 (P-cluster clock -20%)"
        );
    }

    /// Regression: one slow reading from the sampler is noise, not a throttle.
    #[test]
    fn single_sample_dip_is_not_a_throttle() {
        let report = analyze(
            &curve(|t| if (t - 30.0).abs() < 0.1 { 2000 } else { 4500 }),
            &meta(),
            Stop::Completed,
        );
        assert_eq!(report.verdict, Verdict::Pass);
    }

    /// Regression: a boost that settles inside the first 5 s is the baseline,
    /// not a throttle.
    #[test]
    fn drop_inside_the_baseline_window_does_not_count() {
        let report = analyze(
            &curve(|t| if t <= 1.0 { 5000 } else { 3900 }),
            &meta(),
            Stop::Completed,
        );
        let p = &report.clusters[0];
        assert_eq!(p.baseline_mhz, 3900);
        assert_eq!(p.peak_mhz, 5000);
        // Regression: the drop was taken from the boost spike (22%) and
        // printed next to a pass.
        assert!(p.drop_pct.abs() < f64::EPSILON, "{}", p.drop_pct);
        assert_eq!(report.verdict, Verdict::Pass);
    }

    /// Regression: a cluster whose sustained clock ended over 15% under its
    /// early clock printed `drop 22%` beside `PASS`, because its dips never
    /// ran two samples in a row. Such a card is now always THROTTLED.
    #[test]
    fn a_sustained_drop_over_the_limit_never_passes() {
        let samples = curve(|t| {
            if t < 20.0 || ((t * 2.0) as u32).is_multiple_of(2) {
                4500
            } else {
                3000
            }
        });
        let report = analyze(&samples, &meta(), Stop::Completed);
        let p = &report.clusters[0];
        assert!(p.drop_pct > DROP_LIMIT * 100.0, "{}", p.drop_pct);
        assert_eq!(report.verdict, Verdict::Throttled);
        assert_eq!(report.throttle_s, Some(20.5));
        assert_eq!(
            report.throttle_cause.as_deref(),
            Some("P-cluster clock -17%")
        );
    }

    /// The card agrees with itself for any curve: a pass has no loaded
    /// cluster over the drop limit.
    #[test]
    fn a_pass_never_shows_a_judged_drop_over_the_limit() {
        for low in (2000..=4500).step_by(250) {
            for every in 1..=4_u32 {
                let samples = curve(|t| {
                    if t > 10.0 && ((t * 2.0) as u32).is_multiple_of(every) {
                        low
                    } else {
                        4500
                    }
                });
                let report = analyze(&samples, &meta(), Stop::Completed);
                if report.verdict == Verdict::Pass {
                    for c in report.clusters.iter().filter(|c| c.loaded) {
                        assert!(c.drop_pct <= DROP_LIMIT * 100.0, "{low} {every}: {c:?}");
                    }
                }
            }
        }
    }

    /// Regression: with `--light` the idle cluster's wandering clock showed
    /// as `drop 22.1%` beside `PASS`. It is labelled and never judged.
    #[test]
    fn a_cluster_the_load_never_reached_is_labelled_not_judged() {
        let mut samples = curve(|_| 4500);
        for (i, s) in samples.iter_mut().enumerate() {
            let e = &mut s.clusters[1];
            e.busy = 0.2;
            e.mhz = if i < 40 { 2600 } else { 1400 };
        }
        let report = analyze(&samples, &meta(), Stop::Completed);
        let e = &report.clusters[1];
        assert!(!e.loaded);
        assert_eq!(e.busy_pct, Some(20.0));
        assert_eq!(report.verdict, Verdict::Pass);
        let text = render_text(&report, None);
        assert!(
            text.contains(
                "E-cluster   sustained 1400 MHz · peak 2600 MHz · only 20% busy, drop not judged"
            ),
            "{text}"
        );
        assert!(
            text.contains("P-cluster   early 4500 → sustained 4500 MHz · drop 0.0%"),
            "{text}"
        );
    }

    /// Runs saved before `busy_pct` and `loaded` existed read as loaded.
    #[test]
    fn an_older_saved_cluster_reads_as_loaded() {
        let old = r#"{"name":"super","peak_mhz":4500,"baseline_mhz":4400,"sustained_mhz":4300,"drop_pct":2.3}"#;
        let c: ClusterResult = serde_json::from_str(old).unwrap();
        assert!(c.loaded);
        assert_eq!(c.busy_pct, None);
    }

    fn demo_samples(light: bool) -> Vec<Sample> {
        use crate::worker::Tick as _;
        let mut demo = DemoRun::new(STEP, light);
        let _prime = demo.tick().unwrap();
        (1..=120_u32)
            .map(|i| Sample::from_snapshot(&demo.tick().unwrap(), STEP * i))
            .collect()
    }

    /// Regression: `bench --demo` played the idle dashboard session, so its
    /// card read `drop 75%` beside `PASS`. The canned run is a loaded one.
    #[test]
    fn the_demo_card_is_a_loaded_run_that_passes() {
        let report = analyze(&demo_samples(false), &meta(), Stop::Completed);
        assert_eq!(report.verdict, Verdict::Pass, "{report:?}");
        assert_eq!(report.clusters.len(), 2);
        for c in &report.clusters {
            assert!(c.loaded, "{c:?}");
            assert!(c.drop_pct < 6.0, "{c:?}");
            assert!(
                f64::from(c.baseline_mhz) > f64::from(c.peak_mhz) * 0.97,
                "{c:?}"
            );
        }
        assert!(
            report
                .peak_temp_c
                .is_some_and(|t| (75.0..95.0).contains(&t))
        );
        assert!(report.fans.is_some_and(|f| f.ramp_s.is_some()));
        let light = analyze(&demo_samples(true), &meta(), Stop::Completed);
        assert_eq!(light.verdict, Verdict::Pass);
        let loaded: Vec<(&str, bool)> = light
            .clusters
            .iter()
            .map(|c| (c.name.as_str(), c.loaded))
            .collect();
        assert_eq!(loaded, [("performance", false), ("super", true)]);
    }

    #[test]
    fn serious_thermal_state_throttles_even_with_steady_clocks() {
        let mut samples = curve(|_| 4500);
        samples[39].thermal = Thermal::Fair;
        samples[59].thermal = Thermal::Serious;
        let report = analyze(&samples, &meta(), Stop::Completed);
        assert_eq!(report.verdict, Verdict::Throttled);
        assert_eq!(report.throttle_s, Some(30.0));
        assert_eq!(
            report.throttle_cause.as_deref(),
            Some("thermal state serious")
        );
    }

    #[test]
    fn earliest_cause_wins_between_thermal_and_clock() {
        let mut samples = curve(|t| if t < 20.0 { 4500 } else { 3000 });
        samples[99].thermal = Thermal::Critical;
        let report = analyze(&samples, &meta(), Stop::Completed);
        assert_eq!(report.throttle_s, Some(20.5));
        assert!(report.throttle_cause.unwrap().contains("clock"));
    }

    #[test]
    fn peak_temp_average_watts_and_efficiency() {
        let report = analyze(&curve(|_| 4200), &meta(), Stop::Completed);
        assert_eq!(report.peak_temp_c, Some(90.0));
        assert_eq!(report.avg_cpu_w, Some(30.0));
        // (4200×10 + 2600×4) / 14 / 30 W
        let expected = (4200.0 * 10.0 + 2600.0 * 4.0) / 14.0 / 30.0;
        assert!((report.mhz_per_w.unwrap() - expected).abs() < 1e-6);
    }

    #[test]
    fn no_power_rail_leaves_efficiency_out_not_zero() {
        let mut samples = curve(|_| 4200);
        for s in &mut samples {
            s.cpu_w = None;
        }
        let report = analyze(&samples, &meta(), Stop::Completed);
        assert_eq!(report.avg_cpu_w, None);
        assert_eq!(report.mhz_per_w, None);
        let text = render_text(&report, None);
        assert!(!text.contains("CPU power"), "{text}");
    }

    #[test]
    fn fans_ramp_time_and_peak() {
        let mut samples = curve(|_| 4500);
        for (i, s) in samples.iter_mut().enumerate() {
            s.fan_rpm = Some(if i < 20 {
                1200
            } else {
                1200 + 100 * (i as u32 - 19)
            });
        }
        let fans = analyze(&samples, &meta(), Stop::Completed).fans.unwrap();
        assert_eq!(fans.start_rpm, 1200);
        assert_eq!(fans.peak_rpm, 1200 + 100 * 100);
        assert_eq!(fans.ramp_s, Some(11.5));
    }

    /// Regression: a `MacBook` Pro's fans rest at 0 rpm; the card says `off`,
    /// never `0 rpm`.
    #[test]
    fn resting_fans_read_off_not_zero_rpm() {
        let off = FanResult {
            start_rpm: 0,
            peak_rpm: 0,
            ramp_s: None,
        };
        assert_eq!(fan_line(off), "stayed off");
        let woke = FanResult {
            start_rpm: 0,
            peak_rpm: 3400,
            ramp_s: Some(21.0),
        };
        assert_eq!(fan_line(woke), "off → 3400 rpm, ramped at 00:21");
        assert!(!fan_line(woke).contains("0 rpm →"));
    }

    #[test]
    fn fanless_mac_has_no_fan_row() {
        let report = analyze(&curve(|_| 4500), &meta(), Stop::Completed);
        assert_eq!(report.fans, None);
        assert!(!render_text(&report, None).contains("fans"));
    }

    #[test]
    fn fans_that_never_ramp_say_so() {
        let mut samples = curve(|_| 4500);
        for s in &mut samples {
            s.fan_rpm = Some(1500);
        }
        let report = analyze(&samples, &meta(), Stop::Completed);
        assert_eq!(report.fans.unwrap().ramp_s, None);
        assert!(render_text(&report, None).contains("1500 rpm → 1500 rpm, never ramped"));
    }

    /// Regression: Esc after 3 s printed "stopped early" and then
    /// "PASS: no throttling". A cut-short run gives no verdict, in the text
    /// and in `--json`; a throttle seen before the stop still stands.
    #[test]
    fn a_run_stopped_early_has_no_verdict() {
        let samples: Vec<Sample> = curve(|_| 4500).into_iter().take(6).collect();
        for stop in [Stop::Cancelled, Stop::TooHot { celsius: 105.0 }] {
            let report = analyze(&samples, &meta(), stop);
            assert_eq!(report.verdict, Verdict::Incomplete);
            assert_eq!(
                verdict_line(&report),
                "incomplete: stopped after 00:03, no verdict"
            );
            let json: serde_json::Value =
                serde_json::from_str(&render_json(&report, None).unwrap()).unwrap();
            assert_eq!(json["verdict"], "incomplete");
            assert!(!render_text(&report, None).contains("PASS"));
        }
        let mut hot = curve(|_| 4500);
        hot[20].thermal = Thermal::Serious;
        let report = analyze(&hot[..30], &meta(), Stop::Cancelled);
        assert_eq!(report.verdict, Verdict::Throttled);
    }

    /// The demo's live page and card are loaded from the very first sample.
    #[test]
    fn the_demo_is_loaded_from_its_first_sample() {
        let first = Sample::from_snapshot(&demo_frame(STEP, false), STEP);
        for c in &first.clusters {
            assert!(c.busy > 0.9 && c.mhz > 3900, "{c:?}");
        }
        assert!(first.cpu_w.is_some_and(|w| w > 30.0), "{first:?}");
    }

    #[test]
    fn empty_run_reports_without_panicking() {
        let report = analyze(&[], &meta(), Stop::Cancelled);
        assert!(report.clusters.is_empty());
        assert_eq!(report.verdict, Verdict::Incomplete);
        assert!(report.ran_s.abs() < f64::EPSILON);
        assert!(render_text(&report, None).contains("stopped     early, by Esc or Ctrl-C"));
    }

    #[test]
    fn median_handles_even_odd_and_empty() {
        assert_eq!(median(vec![]), None);
        assert_eq!(median(vec![3, 1, 2]), Some(2));
        assert_eq!(median(vec![4, 1, 3, 2]), Some(2));
        assert_eq!(median(vec![u32::MAX, u32::MAX]), Some(u32::MAX));
    }

    #[test]
    fn comparison_reads_lower_higher_and_match() {
        let before = analyze(&curve(|_| 4000), &meta(), Stop::Completed);
        let mut before = before;
        before.date = String::from("2026-09-20");
        // E stays 2600: mean(4000, 2600) = 3300 → mean(3802, 2600) = 3201.
        let slower = analyze(&curve(|_| 3802), &meta(), Stop::Completed);
        let c = compare(&slower, &before).unwrap();
        assert_eq!(
            c.sentence(),
            "clocks 3% lower than your last run on 2026-09-20"
        );
        let faster = analyze(&curve(|_| 4400), &meta(), Stop::Completed);
        assert!(
            compare(&faster, &before)
                .unwrap()
                .sentence()
                .contains("% higher")
        );
        let same = analyze(&curve(|_| 4000), &meta(), Stop::Completed);
        assert_eq!(
            compare(&same, &before).unwrap().sentence(),
            "clocks match your last run on 2026-09-20"
        );
    }

    #[test]
    fn comparison_skips_other_chips_light_runs_and_cut_short_runs() {
        let now = analyze(&curve(|_| 4000), &meta(), Stop::Completed);
        let mut other = now.clone();
        other.chip = String::from("Apple M1");
        let mut light = now.clone();
        light.light = true;
        let mut cut = now.clone();
        cut.stop = Stop::Cancelled;
        let mut old = now.clone();
        old.schema = 0;
        for before in [other, light, cut, old] {
            assert_eq!(compare(&now, &before), None, "{before:?}");
        }
    }

    #[test]
    fn saved_runs_round_trip_and_the_newest_comparable_wins() {
        let dir = std::env::temp_dir().join(format!("siltop-bench-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = runs_path(&dir);
        let now = analyze(&curve(|_| 4000), &meta(), Stop::Completed);
        assert_eq!(previous(&path, &now), None, "no file yet");
        let mut first = now.clone();
        first.date = String::from("2026-09-01");
        let mut second = now.clone();
        second.date = String::from("2026-09-20");
        let mut elsewhere = now.clone();
        elsewhere.chip = String::from("Apple M2");
        save(&path, &first).unwrap();
        save(&path, &second).unwrap();
        save(&path, &elsewhere).unwrap();
        // A damaged line is skipped, not fatal.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{not json\n")
            .unwrap();
        let found = previous(&path, &now).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(found, second);
    }

    #[test]
    fn a_saved_run_with_escapes_prints_nothing_that_acts_on_the_terminal() {
        let dir = std::env::temp_dir().join(format!("siltop-bench-esc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = runs_path(&dir);
        let now = analyze(&curve(|_| 4000), &meta(), Stop::Completed);
        let mut tampered = now.clone();
        tampered.date = String::from("2026-09-20\u{1b}]2;pwn\u{7}");
        save(&path, &tampered).unwrap();
        let found = previous(&path, &now).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let text = compare(&now, &found).unwrap().sentence();
        assert!(!text.chars().any(siltop_core::is_unsafe), "{text:?}");
        assert!(
            text.ends_with("2026-09-20\u{FFFD}]2;pwn\u{FFFD}"),
            "{text:?}"
        );
    }

    #[test]
    fn json_schema_keys_are_stable() {
        let mut samples = curve(|t| if t < 42.0 { 4500 } else { 3600 });
        for s in &mut samples {
            s.fan_rpm = Some(2000);
        }
        let report = analyze(&samples, &meta(), Stop::Completed);
        let cmp = Comparison {
            previous_date: String::from("2026-09-20"),
            clock_pct: -3.0,
        };
        let value: serde_json::Value =
            serde_json::from_str(&render_json(&report, Some(&cmp)).unwrap()).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "avg_cpu_w",
                "chip",
                "clusters",
                "date",
                "fans",
                "light",
                "mhz_per_w",
                "peak_temp_c",
                "planned_s",
                "previous",
                "ran_s",
                "samples",
                "schema",
                "stop",
                "threads",
                "throttle_cause",
                "throttle_s",
                "verdict",
            ]
        );
        assert_eq!(value["verdict"], "throttled");
        assert_eq!(value["stop"]["reason"], "completed");
        assert_eq!(value["previous"]["clock_pct"], -3.0);
        let cluster = value["clusters"][0].as_object().unwrap();
        for key in [
            "name",
            "peak_mhz",
            "baseline_mhz",
            "sustained_mhz",
            "drop_pct",
            "busy_pct",
            "loaded",
        ] {
            assert!(cluster.contains_key(key), "{key}");
        }
        let hot = render_json(
            &analyze(&[], &meta(), Stop::TooHot { celsius: 105.5 }),
            None,
        )
        .unwrap();
        let hot: serde_json::Value = serde_json::from_str(&hot).unwrap();
        assert_eq!(hot["stop"]["reason"], "too_hot");
        assert_eq!(hot["previous"], serde_json::Value::Null);
    }

    #[test]
    fn text_card_names_every_row() {
        let mut samples = curve(|t| if t < 42.0 { 4500 } else { 3600 });
        for s in &mut samples {
            s.fan_rpm = Some(2000);
        }
        let report = analyze(&samples, &meta(), Stop::Completed);
        let cmp = Comparison {
            previous_date: String::from("2026-09-20"),
            clock_pct: -3.2,
        };
        let text = render_text(&report, Some(&cmp));
        for needle in [
            "siltop bench  Apple M4 Pro · 14 threads · 01:00 of 01:00",
            "P-cluster   early 4500 → sustained 3600 MHz · drop 20.0% · peak 4500 MHz",
            "peak temp   90°C",
            "CPU power   avg 30W",
            "fans        2000 rpm → 2000 rpm, never ramped",
            "verdict     THROTTLED at 00:43",
            "vs last     clocks 3% lower than your last run on 2026-09-20",
        ] {
            assert!(text.contains(needle), "{needle:?} in\n{text}");
        }
        assert!(!text.contains('\x1b'));
    }

    #[test]
    fn esc_q_and_ctrl_c_stop_a_live_bench_other_keys_do_not() {
        let key = |code, mods| KeyEvent::new(code, mods);
        assert!(is_stop_key(key(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(is_stop_key(key(KeyCode::Char('q'), KeyModifiers::NONE)));
        assert!(is_stop_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)));
        assert!(!is_stop_key(key(KeyCode::Char('c'), KeyModifiers::NONE)));
        assert!(!is_stop_key(key(KeyCode::Enter, KeyModifiers::NONE)));
        let mut release = key(KeyCode::Esc, KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert!(!is_stop_key(release), "a key release is not a second press");
    }

    #[test]
    fn live_graphs_track_the_fastest_cluster_and_skip_absent_readings() {
        let mut live = Live::new();
        assert_eq!(live.clock.period(), STEP);
        let mut s = sample(0.5, 4300, 2600, 80.0, 20.0);
        s.cpu_w = None;
        live.push(&s);
        assert_eq!(live.clock.last(), Some(4300.0));
        assert_eq!(live.temp.last(), Some(80.0));
        assert!(live.watts.is_empty(), "no rail → no 0 W point");
    }

    #[test]
    fn mmss_formats_and_clamps() {
        assert_eq!(mmss(0.0), "00:00");
        assert_eq!(mmss(42.4), "00:42");
        assert_eq!(mmss(125.0), "02:05");
        assert_eq!(mmss(-3.0), "00:00");
    }

    #[test]
    fn light_mode_loads_half_the_cores_rounding_up() {
        assert_eq!(thread_count(14, false), 14);
        assert_eq!(thread_count(14, true), 7);
        assert_eq!(thread_count(9, true), 5);
        assert_eq!(thread_count(0, true), 1);
    }

    /// Counts starts and stops; never spins.
    #[derive(Default)]
    struct FakeLoad {
        started: Option<usize>,
        stops: usize,
    }

    impl Load for FakeLoad {
        fn start(&mut self, threads: usize) -> Result<()> {
            self.started = Some(threads);
            Ok(())
        }
        fn stop(&mut self) {
            self.stops += 1;
        }
    }

    fn snap(temp: f32) -> Snapshot {
        let mut s = Snapshot::empty();
        s.soc.name = String::from("Apple M4 Pro");
        s.sensors.cpu_c = Some(temp);
        s
    }

    fn plan(secs: u64) -> Plan {
        Plan {
            duration: Duration::from_secs(secs),
            threads: 8,
        }
    }

    #[test]
    fn run_completes_after_the_planned_steps_and_joins_the_load() {
        let mut load = FakeLoad::default();
        let waits = Cell::new(0);
        let mut seen = Vec::new();
        let outcome = run(
            &mut || Ok(snap(70.0)),
            &mut |d| {
                assert_eq!(d, STEP);
                waits.set(waits.get() + 1);
            },
            &|| false,
            &mut load,
            plan(3),
            &mut |p| seen.push(p.remaining),
        )
        .unwrap();
        assert_eq!(outcome.stop, Stop::Completed);
        assert_eq!(outcome.chip, "Apple M4 Pro");
        assert_eq!(outcome.samples.len(), 6);
        assert_eq!(waits.get(), 6);
        assert_eq!(seen.last(), Some(&Duration::ZERO));
        assert_eq!(seen[0], Duration::from_millis(2500));
        assert_eq!((load.started, load.stops), (Some(8), 1));
    }

    #[test]
    fn safety_stop_at_105_c_ends_the_run_at_once() {
        let mut load = FakeLoad::default();
        let temps = RefCell::new(vec![80.0, 95.0, 105.0, 70.0].into_iter());
        let outcome = run(
            &mut || Ok(snap(temps.borrow_mut().next().unwrap_or(60.0))),
            &mut |_| {},
            &|| false,
            &mut load,
            plan(60),
            &mut |_| {},
        )
        .unwrap();
        // Priming tick read 80; the samples read 95 then 105.
        assert_eq!(outcome.samples.len(), 2);
        assert_eq!(outcome.stop, Stop::TooHot { celsius: 105.0 });
        assert_eq!(load.stops, 1);
    }

    #[test]
    fn hot_zone_reading_trips_the_safety_stop_too() {
        let mut load = FakeLoad::default();
        let outcome = run(
            &mut || {
                let mut s = snap(60.0);
                s.sensors.p_c = Some(106.0);
                Ok(s)
            },
            &mut |_| {},
            &|| false,
            &mut load,
            plan(60),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(outcome.stop, Stop::TooHot { celsius: 106.0 });
    }

    #[test]
    fn cancel_stops_early_and_still_reports() {
        let mut load = FakeLoad::default();
        let ticks = Cell::new(0);
        let outcome = run(
            &mut || {
                ticks.set(ticks.get() + 1);
                Ok(snap(70.0))
            },
            &mut |_| {},
            &|| ticks.get() > 4,
            &mut load,
            plan(60),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(outcome.stop, Stop::Cancelled);
        assert_eq!(outcome.samples.len(), 4);
        assert_eq!(load.stops, 1);
    }

    /// Regression: a sampler error mid-run must still stop the load threads.
    #[test]
    fn sampler_error_still_joins_the_load() {
        let mut load = FakeLoad::default();
        let ticks = Cell::new(0);
        let result = run(
            &mut || {
                ticks.set(ticks.get() + 1);
                if ticks.get() == 3 {
                    Err(siltop_core::Error::system("gone"))
                } else {
                    Ok(snap(70.0))
                }
            },
            &mut |_| {},
            &|| false,
            &mut load,
            plan(60),
            &mut |_| {},
        );
        assert!(result.is_err());
        assert_eq!(load.stops, 1);
    }

    #[test]
    fn failed_priming_tick_never_starts_the_load() {
        let mut load = FakeLoad::default();
        let result = run(
            &mut || Err(siltop_core::Error::system("no sampler")),
            &mut |_| {},
            &|| false,
            &mut load,
            plan(60),
            &mut |_| {},
        );
        assert!(result.is_err());
        assert_eq!(load.started, None);
    }

    /// The real spinners with zero threads: start and stop without work.
    #[test]
    fn spinners_with_no_threads_start_and_stop_cleanly() {
        let mut spinners = Spinners::default();
        spinners.start(0).unwrap();
        Load::stop(&mut spinners);
        Load::stop(&mut spinners);
        assert!(spinners.handles.is_empty());
    }

    fn cpu_cluster(id: u8, kind: siltop_core::ClusterKind, name: &str) -> CpuCluster {
        CpuCluster {
            id,
            kind,
            name: String::from(name),
            active: 1.0,
            scaled: 1.0,
            freq_mhz: Some(4000 + u32::from(id)),
            max_mhz: 4608,
        }
    }

    /// Regression (bench cluster names): an M5 Pro's `MCPU0`, `MCPU1` and
    /// `PCPU` read as the dashboard's `performance 0`, `performance 1` and
    /// `super`, and each performance cluster weighs only its own cores.
    #[test]
    fn bench_cluster_names_use_the_dashboard_words_not_ioreport_channels() {
        use siltop_core::{ClusterKind, CoreSample};
        let mut s = snap(60.0);
        s.cpu.clusters = vec![
            cpu_cluster(2, ClusterKind::Performance, "MCPU1"),
            cpu_cluster(0, ClusterKind::Super, "PCPU"),
            cpu_cluster(1, ClusterKind::Performance, "MCPU0"),
        ];
        let core = |kind, index| CoreSample {
            kind,
            index,
            scaled: 1.0,
            active: 1.0,
            freq_mhz: None,
        };
        s.cpu.cores = (0..12)
            .map(|i| core(ClusterKind::Performance, i))
            .chain((0..6).map(|i| core(ClusterKind::Super, i)))
            .collect();
        let sample = Sample::from_snapshot(&s, Duration::from_secs(1));
        let named: Vec<(&str, u32)> = sample
            .clusters
            .iter()
            .map(|c| (c.name.as_str(), c.cores))
            .collect();
        assert_eq!(
            named,
            vec![("performance 1", 6), ("super", 6), ("performance 0", 6)]
        );
        let report = analyze(&[sample], &meta(), Stop::Completed);
        let text = render_text(&report, None);
        assert!(text.contains("  performance 1 early 4002 →"), "{text}");
        assert!(text.contains("  super         early 4000 →"), "{text}");
        assert!(text.contains("  verdict       PASS"), "{text}");
        for raw in ["MCPU", "PCPU"] {
            assert!(!text.contains(raw), "{raw} in\n{text}");
        }
    }

    #[test]
    fn sample_from_snapshot_keeps_hottest_reading_fastest_fan_and_cpu_watts() {
        let mut s = snap(71.0);
        s.sensors.hotspot_c = Some(88.0);
        s.sensors.e_c = Some(f32::NAN);
        s.fans.fans = vec![
            siltop_core::FanMetric {
                name: String::from("left"),
                rpm: 1800,
                max_rpm: 6000,
            },
            siltop_core::FanMetric {
                name: String::from("right"),
                rpm: 2100,
                max_rpm: 6000,
            },
        ];
        s.cpu.watts = Some(12.5);
        let sample = Sample::from_snapshot(&s, Duration::from_secs(1));
        assert_eq!(sample.temp_c, Some(88.0));
        assert_eq!(sample.fan_rpm, Some(2100));
        assert_eq!(sample.cpu_w, Some(12.5));
        assert!(sample.clusters.is_empty(), "no MHz → no cluster, never a 0");
    }
}
