/// Memory pressure. Declared mildest first, so `Ord` is severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Pressure {
    #[default]
    Nominal,
    Warn,
    Critical,
}

impl Pressure {
    /// Every level, mildest first.
    pub const ALL: [Self; 3] = [Self::Nominal, Self::Warn, Self::Critical];

    /// The one spelling everywhere: panels, JSON, alert rules, verdict
    /// evidence.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Nominal => "nominal",
            Self::Warn => "warn",
            Self::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemorySnapshot {
    pub used_bytes: u64,
    pub total_bytes: u64,
    pub wired_bytes: u64,
    pub compressed_bytes: u64,
    pub cache_bytes: u64,
    pub swap_used_bytes: u64,
    pub swap_total_bytes: u64,
    pub pressure: Pressure,
    /// Bytes swapped in per second since the previous sample; `None` on the
    /// first sample or when the counter is unavailable.
    pub swap_in_bps: Option<u64>,
    /// Bytes swapped out per second since the previous sample.
    pub swap_out_bps: Option<u64>,
    /// `kern.memorystatus_level`: the kernel's free-memory percentage (0–100),
    /// the figure `memory_pressure` prints. `None` when unavailable.
    pub free_percent: Option<u8>,
}

impl MemorySnapshot {
    /// Used / total, 0..=1; `None` before memory was read (never a fake 0).
    /// The one definition every graph, report and line shares.
    #[must_use]
    #[expect(clippy::cast_precision_loss, reason = "a ratio")]
    pub fn used_ratio(&self) -> Option<f32> {
        (self.total_bytes > 0).then(|| self.used_bytes as f32 / self.total_bytes as f32)
    }
}

/// Where physical memory goes, in Activity Monitor's order. Each part is
/// clamped against what is left of `total`, so the four always sum to at
/// most the machine's RAM even when the kernel's counters race each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Composition {
    pub app: u64,
    pub wired: u64,
    pub compressed: u64,
    pub cached: u64,
    pub total: u64,
}

impl Composition {
    /// `(name, bytes)` in a fixed order, so colours never swap between frames.
    #[must_use]
    pub const fn parts(&self) -> [(&'static str, u64); 4] {
        [
            ("app", self.app),
            ("wired", self.wired),
            ("compressed", self.compressed),
            ("cached", self.cached),
        ]
    }

    /// A part's share of the total, `0.0` when the total is unknown.
    #[must_use]
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    pub fn ratio(&self, bytes: u64) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (bytes as f64 / self.total as f64) as f32
        }
    }
}

impl MemorySnapshot {
    /// Used memory split into app / wired / compressed, plus the file cache
    /// that sits beside it.
    #[must_use]
    pub fn composition(&self) -> Composition {
        let total = self.total_bytes;
        let mut left = total;
        let mut take = |bytes: u64| {
            let got = bytes.min(left);
            left -= got;
            got
        };
        let wired = take(self.wired_bytes);
        let compressed = take(self.compressed_bytes);
        let app = take(
            self.used_bytes
                .saturating_sub(self.wired_bytes)
                .saturating_sub(self.compressed_bytes),
        );
        let cached = take(self.cache_bytes);
        Composition {
            app,
            wired,
            compressed,
            cached,
            total,
        }
    }

    /// Swap is worth a word only once something is actually in it.
    #[must_use]
    pub const fn swap_in_use(&self) -> bool {
        self.swap_used_bytes > 0
    }

    /// Net swap direction this tick: `Some(true)` paging out (growing),
    /// `Some(false)` paging in, `None` when idle or unknown.
    #[must_use]
    pub fn swap_trend(&self) -> Option<bool> {
        let out = self.swap_out_bps.unwrap_or(0);
        let inn = self.swap_in_bps.unwrap_or(0);
        match out.cmp(&inn) {
            std::cmp::Ordering::Greater => Some(true),
            std::cmp::Ordering::Less => Some(false),
            std::cmp::Ordering::Equal => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: u64 = 1024 * 1024 * 1024;

    fn mem(used: u64, wired: u64, compressed: u64, cache: u64, total: u64) -> MemorySnapshot {
        MemorySnapshot {
            used_bytes: used,
            wired_bytes: wired,
            compressed_bytes: compressed,
            cache_bytes: cache,
            total_bytes: total,
            ..MemorySnapshot::default()
        }
    }

    #[test]
    fn composition_splits_used_into_app_wired_compressed() {
        let c = mem(18 * G, 4 * G, 2 * G, 6 * G, 36 * G).composition();
        assert_eq!(c.wired, 4 * G);
        assert_eq!(c.compressed, 2 * G);
        assert_eq!(c.app, 12 * G);
        assert_eq!(c.cached, 6 * G);
    }

    #[test]
    fn composition_sums_never_exceed_total_sweep() {
        for used in [0, 1, 8, 16, 40] {
            for wired in [0, 2, 9, 50] {
                for compressed in [0, 3, 20] {
                    for cache in [0, 5, 64] {
                        let c = mem(used * G, wired * G, compressed * G, cache * G, 16 * G)
                            .composition();
                        let sum: u64 = c.parts().iter().map(|(_, b)| b).sum();
                        assert!(sum <= c.total, "{used} {wired} {compressed} {cache}");
                    }
                }
            }
        }
    }

    #[test]
    fn composition_with_unknown_total_is_empty() {
        let c = mem(8 * G, 2 * G, G, G, 0).composition();
        assert_eq!(c.parts().iter().map(|(_, b)| b).sum::<u64>(), 0);
        assert!((c.ratio(G) - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn wired_larger_than_used_does_not_underflow_app() {
        let c = mem(2 * G, 3 * G, 0, 0, 8 * G).composition();
        assert_eq!(c.app, 0);
        assert_eq!(c.wired, 3 * G);
    }

    #[test]
    fn swap_token_only_when_swap_used() {
        let mut m = MemorySnapshot::default();
        assert!(!m.swap_in_use());
        m.swap_used_bytes = 1;
        assert!(m.swap_in_use());
    }

    #[test]
    fn swap_trend_follows_the_larger_rate() {
        let mut m = MemorySnapshot::default();
        assert_eq!(m.swap_trend(), None);
        m.swap_out_bps = Some(10);
        assert_eq!(m.swap_trend(), Some(true));
        m.swap_in_bps = Some(20);
        assert_eq!(m.swap_trend(), Some(false));
        m.swap_out_bps = Some(20);
        assert_eq!(m.swap_trend(), None);
    }
}
