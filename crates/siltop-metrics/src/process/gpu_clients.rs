//! Per-process GPU share from the AGX user clients (no sudo).
//!
//! Every GPU client is a child of the `IOAccelerator` service with an
//! `IOUserClientCreator` of `"pid N, name"` and an `AppUsage` array whose
//! `accumulatedGPUTime` (ns) grows while that client's work runs. Summing
//! per pid and dividing the delta by wall time gives a GPU busy share.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::cadence::Pace;

/// The registry walk is refreshed at most this often.
pub(crate) const GPU_EVERY: Duration = Duration::from_secs(2);

/// The walk's cadence while the GPU is idle (each walk costs ~2.7 ms): all
/// shares are near zero, so an older walk shows the same thing.
pub(crate) const GPU_IDLE_EVERY: Duration = Duration::from_secs(6);

/// Parses `"pid 414, WindowServer"` into the pid.
pub(crate) fn parse_creator(text: &str) -> Option<u32> {
    let rest = text.trim_start().strip_prefix("pid ")?;
    let digits = rest.split(',').next()?.trim();
    digits.parse().ok()
}

/// GPU busy percent from two accumulated-time totals, capped at 100.
pub(crate) fn gpu_percent(prev_ns: u64, now_ns: u64, interval_ns: u128) -> Option<f32> {
    if interval_ns == 0 {
        return None;
    }
    let pct = now_ns.saturating_sub(prev_ns) as f64 / interval_ns as f64 * 100.0;
    Some(pct.clamp(0.0, 100.0) as f32)
}

/// Shares between the last two walks, per pid.
#[derive(Debug, Default)]
pub(crate) struct GpuClients {
    last: Option<(Instant, HashMap<u32, u64>)>,
    shares: HashMap<u32, f32>,
    pace: Pace,
}

impl GpuClients {
    /// Takes a fresh totals map (from [`read_totals`]) observed at `now`.
    pub(crate) fn update(&mut self, now: Instant, totals: HashMap<u32, u64>) {
        if let Some((at, prev)) = &self.last {
            let interval = now.saturating_duration_since(*at).as_nanos();
            self.shares = totals
                .iter()
                .filter_map(|(pid, &total)| {
                    let was = prev.get(pid).copied().unwrap_or(total);
                    gpu_percent(was, total, interval).map(|p| (*pid, p))
                })
                .collect();
        }
        self.last = Some((now, totals));
        self.pace.ran(now);
    }

    /// A walk is due every [`GPU_EVERY`], or [`GPU_IDLE_EVERY`] while the
    /// GPU is idle; in between, the last walk's shares stand.
    pub(crate) fn due(&mut self, now: Instant, gpu_idle: bool) -> bool {
        let every = if gpu_idle { GPU_IDLE_EVERY } else { GPU_EVERY };
        self.pace.due(now, every)
    }

    /// Share for `pid`; 0 when it holds no GPU client.
    pub(crate) fn share(&self, pid: u32) -> f32 {
        self.shares.get(&pid).copied().unwrap_or(0.0)
    }
}

/// Σ `accumulatedGPUTime` per creator pid across every GPU user client.
#[cfg(target_os = "macos")]
pub(crate) fn read_totals() -> HashMap<u32, u64> {
    use crate::iokit::{self, CfKey};
    let mut totals = HashMap::new();
    let (Some(creator_key), Some(usage_key), Some(time_key)) = (
        CfKey::new(c"IOUserClientCreator"),
        CfKey::new(c"AppUsage"),
        CfKey::new(c"accumulatedGPUTime"),
    ) else {
        return totals;
    };
    for accel in iokit::services(c"IOAccelerator") {
        for client in iokit::children(&accel, c"IOService") {
            let Some(pid) = iokit::property(&client, &creator_key)
                .and_then(|v| iokit::cf_string(v.borrow()))
                .and_then(|s| parse_creator(&s))
            else {
                continue;
            };
            let Some(usage) = iokit::property(&client, &usage_key) else {
                continue;
            };
            let sum: u64 = iokit::cf_array_items(usage.borrow())
                .into_iter()
                .filter_map(|entry| iokit::cf_dict_get(entry, &time_key))
                .filter_map(iokit::cf_i64)
                .filter_map(|t| u64::try_from(t).ok())
                .fold(0, u64::saturating_add);
            let slot = totals.entry(pid).or_insert(0_u64);
            *slot = slot.saturating_add(sum);
        }
    }
    totals
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn read_totals() -> HashMap<u32, u64> {
    HashMap::new()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn creator_string_table() {
        let table = [
            ("pid 414, WindowServer", Some(414)),
            ("pid 419, loginwindow", Some(419)),
            ("pid 0, kernel_task", Some(0)),
            ("pid 97961, Brave Browser Helper (GPU)", Some(97961)),
            ("  pid 7, x", Some(7)),
            ("pid 12", Some(12)),
            ("pid , x", None),
            ("pid -3, x", None),
            ("pid 99999999999, x", None),
            ("PID 5, x", None),
            ("", None),
            ("WindowServer", None),
        ];
        for (text, want) in table {
            assert_eq!(parse_creator(text), want, "{text:?}");
        }
    }

    #[test]
    fn gpu_percent_is_delta_over_wall_time() {
        assert_eq!(gpu_percent(0, 500_000_000, 1_000_000_000), Some(50.0));
        assert_eq!(
            gpu_percent(10, 5, 1_000_000_000),
            Some(0.0),
            "reset reads 0"
        );
        assert_eq!(gpu_percent(0, u64::MAX, 1), Some(100.0), "capped");
        assert_eq!(gpu_percent(0, 1, 0), None);
    }

    #[test]
    fn shares_need_two_walks_and_new_clients_start_at_zero() {
        let t0 = Instant::now();
        let mut g = GpuClients::default();
        assert!(g.due(t0, false));
        g.update(t0, HashMap::from([(414, 1_000_000_000)]));
        assert!(
            (g.share(414) - 0.0).abs() < f32::EPSILON,
            "one walk: no rate"
        );
        assert!(!g.due(t0 + Duration::from_millis(500), false));
        let t1 = t0 + GPU_EVERY;
        assert!(g.due(t1, false));
        g.update(
            t1,
            HashMap::from([(414, 1_500_000_000), (77, 9_000_000_000)]),
        );
        assert!((g.share(414) - 25.0).abs() < 1e-3, "{}", g.share(414));
        assert!((g.share(77) - 0.0).abs() < f32::EPSILON, "new client");
        assert!((g.share(5) - 0.0).abs() < f32::EPSILON, "no client");
    }

    #[test]
    fn an_idle_gpu_walks_every_six_seconds_and_keeps_the_last_shares() {
        let t0 = Instant::now();
        let mut g = GpuClients::default();
        assert!(g.due(t0, true), "the first walk is never deferred");
        g.update(t0, HashMap::from([(414, 0)]));
        let t1 = t0 + GPU_EVERY;
        assert!(g.due(t1, false), "busy: every 2 s");
        assert!(!g.due(t1, true), "idle: waits");
        assert!(
            !g.due(t1 + Duration::from_secs(3), true),
            "5 s after the walk"
        );
        assert!(g.due(t0 + GPU_IDLE_EVERY, true));
        g.update(t1, HashMap::from([(414, 20_000_000)]));
        assert!(!g.due(t1, true));
        assert!(g.share(414) > 0.0, "shares stand between idle walks");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_walk_finds_window_server_quickly() {
        let started = Instant::now();
        let totals = read_totals();
        let took = started.elapsed();
        assert!(!totals.is_empty(), "some process holds a GPU client");
        assert!(took < Duration::from_millis(50), "{took:?}");
    }
}
