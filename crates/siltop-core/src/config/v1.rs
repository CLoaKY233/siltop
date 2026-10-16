//! The on-disk schema, version 1. Every table and field carries
//! `#[serde(default)]`, so a file only needs the keys it changes.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{Config, INTERVAL_DEFAULT, INTERVALS, PROC_RATIO_MAX, PROC_RATIO_MIN, ProcSort};
use crate::surface::Surface;

pub const VERSION: i64 = 1;

/// A three-way setting: decide by what the machine or terminal reports, or
/// force it either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Switch {
    #[default]
    Auto,
    On,
    Off,
}

/// Battery saver: auto slows down on battery unless the user pinned a cadence.
pub type LowPower = Switch;

/// Glass (translucent window) handling: auto reads the terminal's own
/// opacity setting; on and off override it.
pub type Glass = Switch;

impl Switch {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
            Self::Off => "off",
        }
    }

    /// Accepts the labels and, leniently, `true` / `false` spelled as text.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "on" | "true" => Some(Self::On),
            "off" | "false" => Some(Self::Off),
            _ => None,
        }
    }
}

/// How process CPU % is scaled: 100 % per core (Activity Monitor) or
/// 100 % for the whole machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CpuUnit {
    #[default]
    Core,
    Total,
}

impl CpuUnit {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Total => "total",
        }
    }

    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "core" => Some(Self::Core),
            "total" => Some(Self::Total),
            _ => None,
        }
    }
}

#[must_use]
pub const fn surface_label(surface: Option<Surface>) -> &'static str {
    match surface {
        None => "auto",
        Some(Surface::Work) => "work",
        Some(Surface::Glance) => "glance",
    }
}

/// `Some(None)` is auto; `None` is not a surface name.
#[allow(clippy::option_option)]
#[must_use]
pub fn parse_surface(text: &str) -> Option<Option<Surface>> {
    match text.trim().to_ascii_lowercase().as_str() {
        "auto" | "" => Some(None),
        "work" => Some(Some(Surface::Work)),
        "glance" => Some(Some(Surface::Glance)),
        _ => None,
    }
}

/// Only the selectable cadences load; anything else is rejected.
#[must_use]
pub fn parse_interval_ms(ms: i64) -> Option<Duration> {
    let ms = u64::try_from(ms).ok()?;
    INTERVALS
        .into_iter()
        .find(|interval| interval.as_millis() == u128::from(ms))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelsV1 {
    pub gpu: bool,
    pub net: bool,
    pub disk: bool,
    pub fans: bool,
    pub power: bool,
    pub cores: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SortV1 {
    pub column: String,
    pub desc: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProcV1 {
    pub sort: SortV1,
    pub tree: bool,
    pub threads: bool,
    pub ratio: u16,
    pub cpu_unit: CpuUnit,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UnitsV1 {
    pub net_bits: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OnboardingV1 {
    pub completed: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdatesV1 {
    pub check: bool,
    pub auto_install: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigV1 {
    pub version: i64,
    pub interval_ms: u64,
    pub surface: String,
    pub theme: String,
    pub icons: String,
    pub keymap: String,
    pub low_power: LowPower,
    /// Scalars sit above the tables so TOML can emit them at the top level.
    pub zen: bool,
    pub glass: Glass,
    pub extra_contrast: bool,
    pub panels: PanelsV1,
    pub proc: ProcV1,
    pub units: UnitsV1,
    pub onboarding: OnboardingV1,
    pub updates: UpdatesV1,
}

impl Default for PanelsV1 {
    fn default() -> Self {
        ConfigV1::from(&Config::default()).panels
    }
}

impl Default for SortV1 {
    fn default() -> Self {
        ConfigV1::from(&Config::default()).proc.sort
    }
}

impl Default for ProcV1 {
    fn default() -> Self {
        ConfigV1::from(&Config::default()).proc
    }
}

impl Default for ConfigV1 {
    fn default() -> Self {
        Self::from(&Config::default())
    }
}

impl From<&Config> for ConfigV1 {
    fn from(cfg: &Config) -> Self {
        Self {
            version: VERSION,
            interval_ms: u64::try_from(cfg.interval.as_millis()).unwrap_or(500),
            surface: String::from(surface_label(cfg.surface)),
            theme: cfg.theme.clone(),
            icons: cfg.icons.clone(),
            keymap: cfg.keymap.clone(),
            low_power: cfg.low_power,
            zen: cfg.zen,
            glass: cfg.glass,
            extra_contrast: cfg.extra_contrast,
            panels: PanelsV1 {
                gpu: cfg.show_gpu,
                net: cfg.show_net,
                disk: cfg.show_disk,
                fans: cfg.show_fans,
                power: cfg.show_power,
                cores: cfg.show_cores,
            },
            proc: ProcV1 {
                sort: SortV1 {
                    column: String::from(cfg.proc_sort.label()),
                    desc: cfg.sort_desc,
                },
                tree: cfg.show_tree,
                threads: cfg.show_threads,
                ratio: cfg.proc_ratio,
                cpu_unit: cfg.cpu_unit,
            },
            units: UnitsV1 {
                net_bits: cfg.net_bits,
            },
            onboarding: OnboardingV1 {
                completed: cfg.onboarding_completed.clone(),
            },
            updates: UpdatesV1 {
                check: cfg.updates_check,
                auto_install: cfg.updates_auto_install,
            },
        }
    }
}

impl From<ConfigV1> for Config {
    /// Invalid values fall back to the default per field, never the whole file.
    fn from(v1: ConfigV1) -> Self {
        let base = Config::default();
        Self {
            interval: i64::try_from(v1.interval_ms)
                .ok()
                .and_then(parse_interval_ms)
                .unwrap_or(INTERVAL_DEFAULT),
            surface: parse_surface(&v1.surface).unwrap_or(base.surface),
            show_gpu: v1.panels.gpu,
            show_net: v1.panels.net,
            show_disk: v1.panels.disk,
            show_fans: v1.panels.fans,
            show_power: v1.panels.power,
            show_cores: v1.panels.cores,
            show_threads: v1.proc.threads,
            show_tree: v1.proc.tree,
            proc_sort: ProcSort::parse(&v1.proc.sort.column).unwrap_or(base.proc_sort),
            proc_ratio: v1.proc.ratio.clamp(PROC_RATIO_MIN, PROC_RATIO_MAX),
            theme: v1.theme,
            icons: v1.icons,
            keymap: v1.keymap,
            low_power: v1.low_power,
            glass: v1.glass,
            extra_contrast: v1.extra_contrast,
            sort_desc: v1.proc.sort.desc,
            cpu_unit: v1.proc.cpu_unit,
            net_bits: v1.units.net_bits,
            zen: v1.zen,
            onboarding_completed: v1.onboarding.completed,
            updates_check: v1.updates.check,
            updates_auto_install: v1.updates.auto_install,
            alerts: Vec::new(),
        }
    }
}

impl Config {
    /// The whole config as a v1 document, every key present: what
    /// `--print-config` shows.
    pub fn to_toml(&self) -> Result<String, toml_edit::ser::Error> {
        let text = toml_edit::ser::to_string_pretty(&ConfigV1::from(self))?;
        if self.alerts.is_empty() {
            return Ok(text);
        }
        let mut doc = toml_edit::ser::to_document(&ConfigV1::from(self))?;
        crate::alerts::write_specs(&mut doc, &self.alerts);
        Ok(doc.to_string())
    }
}
