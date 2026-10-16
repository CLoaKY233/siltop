//! `siltop report today|week|month [--json]`: totals over saved history.

use std::io::{self, Write};
use std::path::Path;

use siltop_core::store::{HistoryStore, Report, Span, clock_label, duration};
use siltop_core::watts_display;

use crate::store_disk::{DiskStore, SIZE_CAP};

/// Apps listed in the energy leaderboard.
pub const LEADERBOARD: usize = 10;

/// What `report <word>` asks for.
#[must_use]
pub fn span_by_name(name: &str) -> Option<Span> {
    match name {
        "today" => Some(Span::Today),
        "week" => Some(Span::Week),
        "month" => Some(Span::Month),
        _ => None,
    }
}

/// The local zone's offset east of UTC at unix second `t`, from libc.
#[must_use]
pub fn utc_offset_s(t: u64) -> i64 {
    let time = libc::time_t::try_from(t).unwrap_or(0);
    // SAFETY: `libc::tm` is plain old data; all-zero is a valid value (the
    // `tm_zone` pointer is null, which nothing here dereferences).
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are to live locals on this frame; localtime_r is
    // the reentrant variant and writes only into `tm`.
    let ok = unsafe { !libc::localtime_r(&raw const time, &raw mut tm).is_null() };
    // `c_long` is `i64` on every 64-bit target siltop builds for.
    if ok { tm.tm_gmtoff } else { 0 }
}

/// Runs the report: a query of the disk store at `root` (nothing recorded
/// yet reads as an empty report, not an error).
pub fn run(
    span: Span,
    json: bool,
    root: Option<&Path>,
    now: u64,
    out: &mut dyn Write,
) -> io::Result<()> {
    let offset = utc_offset_s(now);
    let report = match root.filter(|r| r.exists()) {
        Some(root) => {
            let store = DiskStore::open(root, SIZE_CAP).map_err(io::Error::other)?;
            query(&store, span, now, offset)?
        }
        None => Report::default(),
    };
    if json {
        writeln!(out, "{}", to_json(&report, span))
    } else {
        out.write_all(text(&report, span, offset).as_bytes())
    }
}

fn query(store: &dyn HistoryStore, span: Span, now: u64, offset: i64) -> io::Result<Report> {
    Report::query(store, span, now, offset, LEADERBOARD).map_err(io::Error::other)
}

const fn span_label(span: Span) -> &'static str {
    match span {
        Span::Hour => "hour",
        Span::Today => "today",
        Span::Day => "day",
        Span::Week => "week",
        Span::Month => "month",
    }
}

/// Plain text; lines for readings the Mac lacks are left out, not zeroed.
#[must_use]
pub fn text(report: &Report, span: Span, offset: i64) -> String {
    use std::fmt::Write as _;
    let dated = span != Span::Today;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "siltop report · {} (since {}) · recorded {}",
        span_label(span),
        clock_label(report.from, offset, dated),
        duration(report.recorded_s)
    );
    if report.is_empty() {
        let _ = writeln!(s, "nothing recorded in this range yet");
        return s;
    }
    if !report.energy.is_empty() {
        let _ = writeln!(s, "\nenergy by app");
        let width = report
            .energy
            .iter()
            .map(|a| a.name.chars().count())
            .max()
            .unwrap_or(0);
        let other = report.energy_other_wh.filter(|wh| *wh >= 0.005);
        let width = width.max(if other.is_some() { OTHER.len() } else { 0 });
        for app in &report.energy {
            let _ = writeln!(s, "  {:<width$}  {:>8.2} Wh", app.name, app.wh);
        }
        if let Some(wh) = other {
            let _ = writeln!(s, "  {OTHER:<width$}  {wh:>8.2} Wh");
        }
    }
    let _ = writeln!(s);
    if let Some(wh) = report.system_wh {
        let _ = writeln!(s, "system energy   {wh:.1} Wh");
    }
    let at = |t| clock_label(t, offset, dated);
    if let Some(p) = report.peak_power_w {
        let _ = writeln!(
            s,
            "peak power      {} at {}",
            watts_display(p.value),
            at(p.t)
        );
    }
    if let Some(p) = report.peak_cpu_c {
        let _ = writeln!(s, "peak CPU temp   {:.0} °C at {}", p.value, at(p.t));
    }
    if let Some(p) = report.peak_gpu_c {
        let _ = writeln!(s, "peak GPU temp   {:.0} °C at {}", p.value, at(p.t));
    }
    if report.peak_cpu_c.is_some() {
        let _ = writeln!(
            s,
            "time hot        {} (CPU ≥ {:.0} °C)",
            duration(report.hot_s),
            siltop_core::store::HOT_C
        );
    }
    if let Some(secs) = report.throttled_s {
        let _ = writeln!(
            s,
            "time throttled  {} (thermal state serious or worse)",
            duration(secs)
        );
    }
    if let Some(d) = report.drain {
        let from = if d.unplugged {
            format!("since unplug at {}", at(d.since))
        } else {
            format!("on battery since {}", at(d.since))
        };
        let pct = match (d.from_pct, d.to_pct) {
            (Some(a), Some(b)) => format!(", {a:.0}% → {b:.0}%"),
            _ => String::new(),
        };
        let _ = writeln!(
            s,
            "battery drain   {:.1} Wh {from} (avg {}{pct})",
            d.wh,
            watts_display(d.avg_w as f32)
        );
    }
    if let Some(h) = report.health {
        let _ = writeln!(
            s,
            "battery health  {:.0}% → {:.0}% since {}",
            h.first.value,
            h.last.value,
            clock_label(h.first.t, offset, true)
        );
    }
    s
}

/// The leaderboard row that holds every app below it.
const OTHER: &str = "other";

#[must_use]
pub fn to_json(report: &Report, span: Span) -> serde_json::Value {
    let peak = |p: Option<siltop_core::store::Peak>| {
        p.map(|p| serde_json::json!({ "value": p.value, "t": p.t }))
    };
    serde_json::json!({
        "span": span_label(span),
        "from": report.from,
        "to": report.to,
        "recorded_s": report.recorded_s,
        "energy_by_app": report.energy.iter().map(|a| serde_json::json!({ "name": a.name, "wh": a.wh })).collect::<Vec<_>>(),
        "system_wh": report.system_wh,
        "peak_power_w": peak(report.peak_power_w),
        "peak_cpu_c": peak(report.peak_cpu_c),
        "peak_gpu_c": peak(report.peak_gpu_c),
        "hot_s": report.hot_s,
        "energy_other_wh": report.energy_other_wh,
        "throttled_s": report.throttled_s,
        "drain": report.drain.map(|d| serde_json::json!({
            "since": d.since,
            "unplugged": d.unplugged,
            "wh": d.wh,
            "avg_w": d.avg_w,
            "from_pct": d.from_pct,
            "to_pct": d.to_pct,
        })),
        "battery_health": report.health.map(|h| serde_json::json!({
            "first": { "pct": h.first.value, "t": h.first.t },
            "last": { "pct": h.last.value, "t": h.last.t },
        })),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use siltop_core::store::{Bucket, Series, Stat, TopProc};

    fn bucket(t: u64, watts: f32) -> Bucket {
        let mut b = Bucket::empty(t);
        b.samples = 6;
        b.series[Series::CpuTempC.index()] = Some(Stat {
            mean: 60.0,
            max: 70.0,
        });
        b.top_energy = vec![TopProc {
            pid: 1,
            name: "cargo".into(),
            value: watts,
        }];
        b
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "siltop-report-{tag}-{}-{}",
            std::process::id(),
            crate::store_disk::unix_now()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn run_to_string(span: Span, json: bool, root: Option<&Path>, now: u64) -> String {
        let mut out = Vec::new();
        run(span, json, root, now, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn span_names_table() {
        assert_eq!(span_by_name("today"), Some(Span::Today));
        assert_eq!(span_by_name("week"), Some(Span::Week));
        assert_eq!(span_by_name("month"), Some(Span::Month));
        assert_eq!(span_by_name("year"), None);
        assert_eq!(span_by_name("Today"), None);
    }

    #[test]
    fn no_history_reads_as_empty_not_an_error() {
        let root = scratch("empty");
        let text = run_to_string(Span::Today, false, Some(&root), 1_000_000);
        assert!(text.contains("nothing recorded"), "{text}");
        assert!(!root.exists(), "a report never creates the directory");
        let text = run_to_string(Span::Today, false, None, 1_000_000);
        assert!(text.contains("nothing recorded"), "{text}");
    }

    /// Fake clock + a real disk store: an hour of 6 s buckets at 4 W is 4 Wh.
    #[test]
    fn report_reads_the_disk_store() {
        let root = scratch("disk");
        let start = 20_000 * 86_400;
        {
            let mut store = DiskStore::open(&root, SIZE_CAP).unwrap();
            for i in 0..600 {
                store.append(bucket(start + i * 6, 4.0)).unwrap();
            }
        }
        let now = start + 3600;
        let json = run_to_string(Span::Week, true, Some(&root), now);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let wh = v["energy_by_app"][0]["wh"].as_f64().unwrap();
        assert!((wh - 4.0).abs() < 0.1, "{wh}");
        assert_eq!(v["energy_by_app"][0]["name"], "cargo");
        assert_eq!(v["system_wh"], serde_json::Value::Null, "absent, not 0");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn text_leaves_out_what_the_mac_lacks() {
        let buckets: Vec<Bucket> = (0..10).map(|i| bucket(i * 6, 2.0)).collect();
        let r = Report::build(&buckets, siltop_core::store::Tier::Day, 0, 60, 5);
        let s = text(&r, Span::Today, 0);
        assert!(s.contains("cargo"));
        assert!(s.contains("peak CPU temp   70 °C at 00:00"), "{s}");
        assert!(!s.contains("GPU"), "{s}");
        assert!(!s.contains("system energy"), "{s}");
        assert!(s.contains("time hot        0 s"), "{s}");
    }

    #[test]
    fn utc_offset_is_within_a_day() {
        let off = utc_offset_s(crate::store_disk::unix_now());
        assert!(off.abs() <= 14 * 3600, "{off}");
    }

    /// Fake clock: 30 min on AC, then 30 min on battery at 8 W, throttled
    /// for the last 6 minutes, health 90 % throughout.
    fn laptop_hour() -> Vec<Bucket> {
        let stat = |v: f32| Some(Stat { mean: v, max: v });
        (0..600_u64)
            .map(|i| {
                let mut b = bucket(i * 6, 3.0);
                let on_battery = i >= 300;
                b.series[Series::OnAc.index()] = stat(if on_battery { 0.0 } else { 1.0 });
                b.series[Series::BatteryW.index()] = stat(if on_battery { -8.0 } else { 20.0 });
                b.series[Series::BatteryPct.index()] = stat(if on_battery { 70.0 } else { 90.0 });
                b.series[Series::BatteryHealth.index()] = stat(90.0);
                let hot = i >= 540;
                b.series[Series::ThermalState.index()] = stat(if hot { 2.0 } else { 0.0 });
                b.series[Series::Throttled.index()] = stat(if hot { 1.0 } else { 0.0 });
                b
            })
            .collect()
    }

    #[test]
    fn a_laptop_report_prints_drain_health_and_throttled_time() {
        let r = Report::build(&laptop_hour(), siltop_core::store::Tier::Day, 0, 3600, 5);
        let s = text(&r, Span::Today, 0);
        assert!(
            s.contains("time throttled  6 min (thermal state serious or worse)"),
            "{s}"
        );
        assert!(
            s.contains("battery drain   4.0 Wh since unplug at 00:30 (avg 8.0W, 70% → 70%)"),
            "{s}"
        );
        assert!(
            s.contains("battery health  90% → 90% since Jan 1 00:00"),
            "{s}"
        );
        let v = to_json(&r, Span::Today);
        assert_eq!(v["throttled_s"], 360);
        assert_eq!(v["drain"]["since"], 1800);
        assert_eq!(v["drain"]["unplugged"], true);
        assert!((v["drain"]["wh"].as_f64().unwrap() - 4.0).abs() < 1e-6);
        assert_eq!(v["battery_health"]["last"]["pct"], 90.0);
    }

    /// Desktops (and v1 history) have no battery or thermal columns: the
    /// lines are left out and the JSON says null, never 0.
    #[test]
    fn a_desktop_report_leaves_battery_and_throttling_out() {
        let buckets: Vec<Bucket> = (0..10).map(|i| bucket(i * 6, 2.0)).collect();
        let r = Report::build(&buckets, siltop_core::store::Tier::Day, 0, 60, 5);
        let s = text(&r, Span::Today, 0);
        for gone in ["battery", "throttled", "other"] {
            assert!(!s.contains(gone), "{gone}: {s}");
        }
        let v = to_json(&r, Span::Today);
        for key in ["drain", "battery_health", "throttled_s", "energy_other_wh"] {
            assert_eq!(v[key], serde_json::Value::Null, "{key}");
        }
    }

    #[test]
    fn the_exact_leaderboard_ends_with_an_other_row() {
        let mut b = bucket(0, 0.0);
        b.top_energy.clear();
        b.apps = vec![siltop_core::store::Named {
            name: "cargo".into(),
            value: 3600.0,
            count: 2,
        }];
        b.apps_other = Some(1800.0);
        let r = Report::build(&[b], siltop_core::store::Tier::Day, 0, 6, 5);
        let s = text(&r, Span::Today, 0);
        assert!(s.contains("  cargo      6.00 Wh"), "{s}");
        assert!(s.contains("  other      3.00 Wh"), "{s}");
    }
}
