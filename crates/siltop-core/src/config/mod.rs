mod migrate;
mod v1;

use std::path::Path;
use std::time::Duration;

use crate::surface::Surface;

pub use migrate::{Loaded, ParseError};
pub use v1::{
    ConfigV1, CpuUnit, Glass, LowPower, PanelsV1, ProcV1, SortV1, Switch, UnitsV1, parse_surface,
    surface_label,
};

/// 250 ms is the opt-in "smooth" cadence; 500 ms is the default.
pub const INTERVAL_FAST: Duration = Duration::from_millis(250);
pub const INTERVAL_MID: Duration = Duration::from_millis(500);
pub const INTERVAL_SLOW: Duration = Duration::from_secs(1);
pub const INTERVAL_DEFAULT: Duration = INTERVAL_MID;
/// Selectable cheap-tick intervals, fastest first.
pub const INTERVALS: [Duration; 5] = [
    INTERVAL_FAST,
    INTERVAL_MID,
    INTERVAL_SLOW,
    Duration::from_secs(2),
    Duration::from_secs(5),
];

/// Visible history on every graph. Ring length is `window / series period`,
/// so changing a clock later cannot silently stretch the window.
pub const HISTORY_WINDOW: Duration = Duration::from_mins(15);
pub const SENSOR_PERIOD: Duration = Duration::from_secs(2);

/// Every key the process table sorts by: one per sortable column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ProcSort {
    #[default]
    Cpu,
    Mem,
    Pid,
    Name,
    User,
    State,
    Gpu,
    Rss,
    Threads,
    Power,
    DiskRead,
    DiskWrite,
    Wakeups,
    Started,
}

impl ProcSort {
    /// Cycle order for `<` / `>`: the everyday keys first.
    pub const ALL: [Self; 14] = [
        Self::Cpu,
        Self::Mem,
        Self::Pid,
        Self::Name,
        Self::User,
        Self::State,
        Self::Gpu,
        Self::Rss,
        Self::Threads,
        Self::Power,
        Self::DiskRead,
        Self::DiskWrite,
        Self::Wakeups,
        Self::Started,
    ];

    /// Position in [`Self::ALL`] (the declaration order).
    const fn at(self) -> usize {
        self as usize
    }

    #[must_use]
    pub const fn next(self) -> Self {
        Self::ALL[(self.at() + 1) % Self::ALL.len()]
    }

    #[must_use]
    pub const fn prev(self) -> Self {
        Self::ALL[(self.at() + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Mem => "mem",
            Self::Pid => "pid",
            Self::Name => "name",
            Self::User => "user",
            Self::State => "state",
            Self::Gpu => "gpu",
            Self::Rss => "rss",
            Self::Threads => "threads",
            Self::Power => "power",
            Self::DiskRead => "read",
            Self::DiskWrite => "write",
            Self::Wakeups => "wakeups",
            Self::Started => "started",
        }
    }

    /// The direction a fresh sort runs: numbers biggest first (the newest
    /// start first), pid and text columns ascending.
    #[must_use]
    pub const fn natural_descending(self) -> bool {
        !matches!(self, Self::Pid | Self::Name | Self::User | Self::State)
    }

    /// Inverse of `label`, ignoring case and surrounding space.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|sort| sort.label().eq_ignore_ascii_case(text.trim()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub interval: Duration,
    pub surface: Option<Surface>,
    pub show_gpu: bool,
    pub show_net: bool,
    pub show_disk: bool,
    pub show_fans: bool,
    /// Power box; shown only on Macs with a battery or a system power reading.
    pub show_power: bool,
    pub show_cores: bool,
    pub show_threads: bool,
    pub show_tree: bool,
    pub proc_sort: ProcSort,
    pub proc_ratio: u16,
    /// Theme name, or `"auto"` to follow the terminal background.
    pub theme: String,
    /// Icon set name, or `"auto"`.
    pub icons: String,
    /// Keymap profile name.
    pub keymap: String,
    pub low_power: LowPower,
    /// Glass surface: auto follows the terminal's opacity setting.
    pub glass: Glass,
    /// Raise the dim, muted and frame contrast floors (bright wallpapers).
    pub extra_contrast: bool,
    /// Persisted sort direction for `proc_sort`.
    pub sort_desc: bool,
    pub cpu_unit: CpuUnit,
    /// Network rates in bits per second instead of bytes.
    pub net_bits: bool,
    /// Zen: no footer, no corner marks, no axis labels — graphs only.
    pub zen: bool,
    /// App version that finished onboarding. Empty means it has not run.
    pub onboarding_completed: String,
    /// Ask GitHub once a day for a newer release. Off until chosen.
    pub updates_check: bool,
    /// Download and install a curl-install update without asking again.
    pub updates_auto_install: bool,
    /// `[[alerts]]` tables as written: overrides of the built-in rules
    /// and added rules, merged by [`crate::effective_rules`].
    pub alerts: Vec<crate::alerts::RuleSpec>,
}

pub const THEME_AUTO: &str = "auto";
pub const KEYMAP_DEFAULT: &str = "default";

pub const PROC_RATIO_MIN: u16 = 35;
pub const PROC_RATIO_MAX: u16 = 72;
pub const PROC_RATIO_DEFAULT: u16 = 55;

impl Default for Config {
    fn default() -> Self {
        Self {
            interval: INTERVAL_DEFAULT,
            surface: None,
            show_gpu: true,
            show_net: true,
            show_disk: false,
            show_fans: true,
            show_power: true,
            show_cores: true,
            show_threads: false,
            show_tree: false,
            proc_sort: ProcSort::Cpu,
            proc_ratio: PROC_RATIO_DEFAULT,
            theme: String::from(THEME_AUTO),
            icons: String::from(THEME_AUTO),
            keymap: String::from(KEYMAP_DEFAULT),
            low_power: LowPower::Auto,
            glass: Glass::Auto,
            extra_contrast: false,
            sort_desc: true,
            cpu_unit: CpuUnit::Core,
            net_bits: false,
            zen: false,
            onboarding_completed: String::new(),
            updates_check: false,
            updates_auto_install: false,
            alerts: Vec::new(),
        }
    }
}

impl Config {
    /// Next slower interval, wrapping from the slowest back to the fastest.
    #[must_use]
    pub fn cycle_interval(&self) -> Duration {
        let at = INTERVALS.iter().position(|i| *i == self.interval);
        at.and_then(|i| INTERVALS.get(i + 1))
            .copied()
            .unwrap_or(INTERVAL_FAST)
    }

    /// One step along `INTERVALS`, clamped at both ends.
    #[must_use]
    pub fn step_interval(&self, faster: bool) -> Duration {
        let at = INTERVALS
            .iter()
            .position(|i| *i == self.interval)
            .unwrap_or(1);
        let next = if faster {
            at.saturating_sub(1)
        } else {
            (at + 1).min(INTERVALS.len() - 1)
        };
        INTERVALS.get(next).copied().unwrap_or(INTERVAL_DEFAULT)
    }

    #[must_use]
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    /// Unknown keys are ignored, invalid values fall back per key; a missing,
    /// unreadable or unparseable file yields the default config.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| Self::from_toml(&text).ok())
            .map_or_else(Config::default, |loaded| loaded.config)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn proc_sort_labels_round_trip_and_cycle_visits_every_key() {
        for (i, sort) in ProcSort::ALL.into_iter().enumerate() {
            assert_eq!(ProcSort::parse(sort.label()), Some(sort));
            assert_eq!(sort.at(), i, "ALL follows declaration order");
            assert_eq!(sort.next().prev(), sort);
        }
        let mut seen = vec![ProcSort::Cpu];
        let mut at = ProcSort::Cpu.next();
        while at != ProcSort::Cpu {
            seen.push(at);
            at = at.next();
        }
        assert_eq!(seen, ProcSort::ALL);
        assert!(ProcSort::Cpu.natural_descending());
        assert!(!ProcSort::Name.natural_descending());
    }

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("siltop-test-{name}.toml"))
    }

    #[test]
    fn default_interval_is_500ms() {
        assert_eq!(Config::default().interval, Duration::from_millis(500));
    }

    #[test]
    fn interval_cycles_through_every_option() {
        let mut cfg = Config::default().with_interval(INTERVAL_FAST);
        let mut seen = vec![cfg.interval];
        for _ in 0..INTERVALS.len() {
            cfg = cfg.clone().with_interval(cfg.cycle_interval());
            seen.push(cfg.interval);
        }
        assert_eq!(&seen[..5], &INTERVALS[..]);
        assert_eq!(seen[5], INTERVAL_FAST, "wraps back to smooth");
    }

    #[test]
    fn bracket_steps_are_directional_and_clamped() {
        let cfg = Config::default();
        assert_eq!(cfg.step_interval(false), INTERVAL_SLOW);
        assert_eq!(cfg.step_interval(true), INTERVAL_FAST);
        let fastest = cfg.clone().with_interval(INTERVAL_FAST);
        assert_eq!(fastest.step_interval(true), INTERVAL_FAST);
        let slowest = cfg.with_interval(Duration::from_secs(5));
        assert_eq!(slowest.step_interval(false), Duration::from_secs(5));
        assert_eq!(slowest.step_interval(true), Duration::from_secs(2));
    }

    #[test]
    fn apply_interval_table() {
        let path = tmp("interval-table");
        std::fs::write(&path, "interval_ms = 250\n").unwrap();
        assert_eq!(Config::load(&path).interval, INTERVAL_FAST);
        std::fs::write(&path, "interval_ms = 500\n").unwrap();
        assert_eq!(Config::load(&path).interval, INTERVAL_MID);
        std::fs::write(&path, "interval_ms = 1000\n").unwrap();
        assert_eq!(Config::load(&path).interval, INTERVAL_SLOW);
        std::fs::write(&path, "interval_ms = 2000\n").unwrap();
        assert_eq!(Config::load(&path).interval, Duration::from_secs(2));
        std::fs::write(&path, "interval_ms = 5000\n").unwrap();
        assert_eq!(Config::load(&path).interval, Duration::from_secs(5));
        std::fs::write(&path, "interval_ms = 99999\n").unwrap();
        assert_eq!(Config::load(&path).interval, INTERVAL_DEFAULT);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn defaults_show_net_but_hide_disk() {
        let cfg = Config::default();
        assert!(cfg.show_net);
        assert!(!cfg.show_disk);
        assert_eq!(cfg.proc_ratio, PROC_RATIO_DEFAULT);
    }

    #[test]
    fn full_toml_then_load_roundtrips() {
        let path = tmp("roundtrip");
        let cfg = Config {
            interval: INTERVAL_FAST,
            show_gpu: false,
            surface: Some(Surface::Glance),
            proc_sort: ProcSort::Mem,
            proc_ratio: 66,
            ..Config::default()
        };
        std::fs::write(&path, cfg.to_toml().unwrap()).unwrap();

        let loaded = Config::load(&path);
        assert_eq!(loaded, cfg);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_is_defaults() {
        assert_eq!(Config::load(&tmp("definitely-missing")), Config::default());
    }

    #[test]
    fn garbage_values_fall_back_per_key() {
        let path = tmp("garbage");
        std::fs::write(
            &path,
            "interval_ms = 99999\nproc_ratio = 5\nshow_fans = \"maybe\"\nsort = \"nope\"\n",
        )
        .unwrap();
        let cfg = Config::load(&path);
        assert_eq!(cfg.interval, INTERVAL_DEFAULT);
        assert_eq!(cfg.proc_ratio, PROC_RATIO_MIN);
        assert!(cfg.show_fans);
        assert_eq!(cfg.proc_sort, ProcSort::Cpu);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn partial_file_overrides_only_named_keys() {
        let path = tmp("partial");
        std::fs::write(&path, "# comment\nshow_threads = true\n").unwrap();
        let cfg = Config::load(&path);
        assert!(cfg.show_threads);
        assert!(!cfg.show_disk);
        let _ = std::fs::remove_file(&path);
    }
}
