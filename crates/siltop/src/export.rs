//! `siltop export --csv`: the saved history store as CSV.
//!
//! One row per bucket, oldest first: the bucket's start (unix seconds and
//! UTC ISO time), how many snapshots it folds, then mean and max per
//! series and the busiest processes. An absent reading is an empty cell,
//! never a zero.

use std::io::{self, Write};

use siltop_core::Day;
use siltop_core::store::{Bucket, HistoryStore, Series, Tier, TopProc};

/// The CSV column stem for a series; units are in the name.
const fn column(series: Series) -> &'static str {
    match series {
        Series::Cpu => "cpu_ratio",
        Series::Gpu => "gpu_ratio",
        Series::Mem => "mem_ratio",
        Series::SwapBytes => "swap_bytes",
        Series::NetRxBps => "net_rx_bytes_per_s",
        Series::NetTxBps => "net_tx_bytes_per_s",
        Series::DiskReadBps => "disk_read_bytes_per_s",
        Series::DiskWriteBps => "disk_write_bytes_per_s",
        Series::CpuTempC => "cpu_temp_c",
        Series::GpuTempC => "gpu_temp_c",
        Series::SysPowerW => "sys_power_w",
        Series::CpuPowerW => "cpu_power_w",
        Series::GpuPowerW => "gpu_power_w",
        Series::Fan => "fan_rpm",
        Series::BatteryPct => "battery_pct",
        Series::BatteryW => "battery_w",
        Series::OnAc => "on_ac",
        Series::ThermalState => "thermal_state",
        Series::Throttled => "throttled_ratio",
        Series::BatteryHealth => "battery_health_pct",
        Series::Hot => "cpu_hot_ratio",
    }
}

/// A stored value in its column's unit: the history keeps network rates
/// in bits per second, as sampled, and the columns say bytes.
fn in_column_unit(series: Series, value: f32) -> f32 {
    match series {
        Series::NetRxBps | Series::NetTxBps => value / 8.0,
        _ => value,
    }
}

/// The header row.
#[must_use]
pub fn header() -> String {
    let mut cols = vec![
        String::from("unix_s"),
        String::from("utc"),
        String::from("samples"),
    ];
    for series in Series::ALL {
        cols.push(format!("{}_mean", column(series)));
        cols.push(format!("{}_max", column(series)));
    }
    cols.extend(["top_cpu", "top_energy", "top_mem"].map(String::from));
    cols.join(",")
}

/// RFC 4180 quoting, only where a cell needs it.
fn quote(cell: &str) -> String {
    if cell.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", cell.replace('"', "\"\""))
    } else {
        String::from(cell)
    }
}

/// `2026-09-23T14:05:06Z`.
fn utc(unix_s: u64) -> String {
    let day = Day::from_days(i32::try_from(unix_s / 86_400).unwrap_or(i32::MAX));
    let secs = unix_s % 86_400;
    format!(
        "{day}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

/// `name (pid) value; ...`, busiest first.
fn tops(list: &[TopProc]) -> String {
    let cells: Vec<String> = list
        .iter()
        .map(|p| format!("{} ({}) {}", p.name, p.pid, p.value))
        .collect();
    quote(&cells.join("; "))
}

/// One data row.
#[must_use]
pub fn row(bucket: &Bucket) -> String {
    let mut cells = vec![
        bucket.t.to_string(),
        utc(bucket.t),
        bucket.samples.to_string(),
    ];
    for series in Series::ALL {
        let stat = bucket.get(series);
        for value in [stat.map(|s| s.mean), stat.map(|s| s.max)] {
            cells.push(value.map_or_else(String::new, |v| in_column_unit(series, v).to_string()));
        }
    }
    cells.push(tops(&bucket.top_cpu));
    cells.push(tops(&bucket.top_energy));
    cells.push(tops(&bucket.top_mem));
    cells.join(",")
}

/// The finest stored tier that still holds `from`.
#[must_use]
pub fn tier_for(from: u64, now: u64) -> Tier {
    if Tier::Day.keeps(from, now) {
        Tier::Day
    } else {
        Tier::Month
    }
}

/// Writes `from..to` from `store` as CSV; returns the rows written.
pub fn write_csv<W: Write + ?Sized>(
    store: &dyn HistoryStore,
    from: u64,
    to: u64,
    now: u64,
    out: &mut W,
) -> siltop_core::Result<usize> {
    let buckets = store.buckets(from, to, tier_for(from, now))?;
    writeln!(out, "{}", header())?;
    for bucket in &buckets {
        writeln!(out, "{}", row(bucket))?;
    }
    out.flush()?;
    Ok(buckets.len())
}

/// Maps a closed pipe (`| head`) to success, like the JSON modes.
pub fn finish(result: siltop_core::Result<usize>) -> siltop_core::Result<()> {
    match result {
        Err(siltop_core::Error::Io(err)) if err.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        other => other.map(|_| ()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use siltop_core::store::{MemoryStore, Stat};

    fn bucket(t: u64) -> Bucket {
        let mut b = Bucket::empty(t);
        b.samples = 6;
        b.series[Series::Cpu.index()] = Some(Stat {
            mean: 0.25,
            max: 0.5,
        });
        b.series[Series::SysPowerW.index()] = Some(Stat {
            mean: 12.5,
            max: 30.0,
        });
        b.top_cpu = vec![
            TopProc {
                pid: 7,
                name: String::from("cargo"),
                value: 150.0,
            },
            TopProc {
                pid: 9,
                name: String::from("a,\"b\""),
                value: 2.5,
            },
        ];
        b
    }

    #[test]
    fn header_names_every_series_twice() {
        let header = header();
        assert!(header.starts_with("unix_s,utc,samples,cpu_ratio_mean,cpu_ratio_max,"));
        assert!(header.contains(",fan_rpm_mean,fan_rpm_max,battery_pct_mean,"));
        assert!(header.ends_with(
            ",battery_health_pct_mean,battery_health_pct_max,cpu_hot_ratio_mean,cpu_hot_ratio_max,top_cpu,top_energy,top_mem"
        ));
        assert_eq!(header.split(',').count(), 3 + 2 * Series::ALL.len() + 3);
    }

    #[test]
    fn row_matches_the_golden_line() {
        let want = concat!(
            "1790000004,2026-09-21T14:13:24Z,6,0.25,0.5,,,,,,,,,,,,,,,,,,,12.5,30,,,,,,,,,,,,,,,,,,,,,",
            "\"cargo (7) 150; a,\"\"b\"\" (9) 2.5\",,"
        );
        assert_eq!(row(&bucket(1_790_000_004)), want);
        assert_eq!(
            row(&bucket(0)).split(',').nth(1),
            Some("1970-01-01T00:00:00Z")
        );
    }

    /// The history stores the sampled bits; the column says bytes, and
    /// once printed the bits, eight times the real rate.
    #[test]
    fn network_columns_hold_bytes_not_the_stored_bits() {
        let mut b = Bucket::empty(0);
        b.series[Series::NetRxBps.index()] = Some(Stat {
            mean: 80_000_000.0,
            max: 160_000_000.0,
        });
        b.series[Series::DiskReadBps.index()] = Some(Stat {
            mean: 800.0,
            max: 800.0,
        });
        let header = header();
        let line = row(&b);
        let value = |name: &str| {
            let at = header.split(',').position(|c| c == name).unwrap();
            line.split(',').nth(at).unwrap().to_owned()
        };
        assert_eq!(value("net_rx_bytes_per_s_mean"), "10000000");
        assert_eq!(value("net_rx_bytes_per_s_max"), "20000000");
        assert_eq!(value("disk_read_bytes_per_s_mean"), "800", "already bytes");
    }

    #[test]
    fn utc_matches_known_instants() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc(1_790_121_599), "2026-09-22T23:59:59Z");
    }

    #[test]
    fn csv_export_of_a_store_is_header_then_rows_in_range() {
        let mut store = MemoryStore::new();
        let now = 1_790_000_000;
        for i in 0..5 {
            store.append(bucket(now - 60 + i * 6)).unwrap();
        }
        let mut out = Vec::new();
        let rows = write_csv(&store, now - 48, now, now, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(rows, 3);
        assert_eq!(text.lines().count(), 4);
        assert_eq!(text.lines().next(), Some(header().as_str()));
        assert!(text.lines().nth(1).unwrap().starts_with("1789999952,"));
    }

    #[test]
    fn empty_range_is_just_the_header() {
        let mut out = Vec::new();
        let rows = write_csv(&MemoryStore::new(), 0, 10, 10, &mut out).unwrap();
        assert_eq!(rows, 0);
        assert_eq!(String::from_utf8(out).unwrap(), format!("{}\n", header()));
    }

    #[test]
    fn old_ranges_come_from_the_month_tier() {
        let now = 40 * 86_400;
        assert_eq!(tier_for(now - 3600, now), Tier::Day);
        assert_eq!(tier_for(now - 3 * 86_400, now), Tier::Month);
    }

    #[test]
    fn broken_pipe_is_success() {
        let pipe = io::Error::from(io::ErrorKind::BrokenPipe);
        assert!(finish(Err(pipe.into())).is_ok());
        let other = io::Error::from(io::ErrorKind::PermissionDenied);
        assert!(finish(Err(other.into())).is_err());
    }
}
