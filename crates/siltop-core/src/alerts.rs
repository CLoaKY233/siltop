//! Alerts: rules on sustained conditions, judged by a pure evaluator.
//!
//! A [`Rule`] names a [`Condition`], how long it must hold (`sustain`), how
//! long to stay quiet after firing (`cooldown`) and where to deliver it
//! ([`Sinks`]). [`AlertEngine`] walks snapshots and returns what fired.
//! Thresholds for the built-in rules come from the verdict module, so the
//! alert and the verdict line never disagree about what "busy" means.
//!
//! Rules live in the config as `[[alerts]]` tables. Every key is optional
//! except `id`: a table whose id matches a built-in rule changes only the
//! keys it names (`enabled = false` silences it), any other id adds a rule.
//! Hardware a Mac lacks (fans on an Air, a battery on a desktop) makes a
//! condition unknown, and an unknown condition never fires.

use std::fmt;
use std::time::{Duration, Instant};

use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, value};

use crate::format::bytes_short;
use crate::snapshot::{Pressure, Snapshot, Thermal};
use crate::verdict::{
    BATTERY_DRAIN_W, BUSY_PROCESS_CORES, BUSY_TOTAL, Signals, THROTTLE_THERMAL, TIGHT_PRESSURE,
    top_process,
};

/// A numeric condition clears only once its value is this fraction back
/// past the threshold, so a reading hovering on the line fires once.
pub const HYSTERESIS: f64 = 0.1;

const MB: u64 = 1024 * 1024;
const GB: u64 = 1024 * MB;

/// What a rule watches. Numbers are whole units so rules compare exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    /// The busiest process above `pct` per-core percent (100 = one core).
    ProcessCpuAbove {
        pct: u16,
    },
    /// Whole-machine CPU activity above `pct` percent.
    CpuTotalAbove {
        pct: u16,
    },
    ThermalAtLeast(Thermal),
    SwapAbove {
        bytes: u64,
    },
    BatteryDrainAbove {
        watts: u16,
    },
    DiskFreeBelow {
        bytes: u64,
    },
    FansAbove {
        rpm: u32,
    },
    MemoryPressureAtLeast(Pressure),
}

/// One condition's reading against its threshold.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Level {
    value: f64,
    threshold: f64,
    /// `true` when the condition trips above the threshold.
    above: bool,
    /// How far back past the threshold the value must go to clear.
    band: f64,
}

impl Level {
    fn above(value: f64, threshold: f64) -> Self {
        Self {
            value,
            threshold,
            above: true,
            band: threshold * HYSTERESIS,
        }
    }

    fn below(value: f64, threshold: f64) -> Self {
        Self {
            value,
            threshold,
            above: false,
            band: threshold * HYSTERESIS,
        }
    }

    /// Ordinal levels (thermal, pressure) clear as soon as they drop a step.
    fn rank(value: u8, threshold: u8) -> Self {
        Self {
            value: f64::from(value),
            threshold: f64::from(threshold) - 0.5,
            above: true,
            band: 0.0,
        }
    }

    fn holds(self) -> bool {
        if self.above {
            self.value >= self.threshold
        } else {
            self.value <= self.threshold
        }
    }

    fn cleared(self) -> bool {
        if self.above {
            self.value < self.threshold - self.band
        } else {
            self.value > self.threshold + self.band
        }
    }
}

fn parse_thermal(text: &str) -> Option<Thermal> {
    Thermal::ALL
        .into_iter()
        .find(|t| t.name().eq_ignore_ascii_case(text))
}

/// `nominal` as `--json` spells it; `normal`, the old rule spelling, too.
fn parse_pressure(text: &str) -> Option<Pressure> {
    if text.eq_ignore_ascii_case("normal") {
        return Some(Pressure::Nominal);
    }
    Pressure::ALL
        .into_iter()
        .find(|p| p.label().eq_ignore_ascii_case(text))
}

/// `30`, `30s`, `2m`, `1h` as whole seconds.
#[must_use]
pub fn parse_secs(text: &str) -> Option<u32> {
    let text = text.trim().to_ascii_lowercase();
    let (number, scale) = if let Some(n) = text.strip_suffix('h') {
        (n, 3600)
    } else if let Some(n) = text.strip_suffix("min") {
        (n, 60)
    } else if let Some(n) = text.strip_suffix('m') {
        (n, 60)
    } else if let Some(n) = text.strip_suffix('s') {
        (n, 1)
    } else {
        (text.as_str(), 1)
    };
    number.trim().parse::<u32>().ok()?.checked_mul(scale)
}

/// The shortest exact spelling: `1h`, `2m`, `45s`.
#[must_use]
pub fn format_secs(secs: u32) -> String {
    match secs {
        0 => String::from("0s"),
        s if s % 3600 == 0 => format!("{}h", s / 3600),
        s if s % 60 == 0 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

fn parse_bytes(text: &str) -> Option<u64> {
    let text = text.trim().to_ascii_uppercase();
    let digits = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(digits);
    let scale = match unit.trim().trim_end_matches("IB").trim_end_matches('B') {
        "" => 1,
        "K" => 1024,
        "M" => MB,
        "G" => GB,
        "T" => GB * 1024,
        _ => return None,
    };
    let value: f64 = number.parse().ok()?;
    (value.is_finite() && value >= 0.0).then(|| (value * scale as f64).round() as u64)
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= GB && bytes.is_multiple_of(GB) {
        format!("{}GB", bytes / GB)
    } else if bytes.is_multiple_of(MB) {
        format!("{}MB", bytes / MB)
    } else {
        format!("{bytes}B")
    }
}

fn parse_unit<T: std::str::FromStr>(text: &str, unit: &str) -> Option<T> {
    let lower = text.trim().to_ascii_lowercase();
    lower
        .strip_suffix(unit)
        .unwrap_or(&lower)
        .trim()
        .parse()
        .ok()
}

/// Parses `name(arg, ...)`. A trailing time argument on
/// `process_cpu_above` and `fans_above` is their sustain.
pub fn parse_condition(text: &str) -> Result<(Condition, Option<u32>), String> {
    let text = text.trim();
    let (name, args) = match text.split_once('(') {
        Some((name, rest)) => (
            name.trim(),
            rest.strip_suffix(')')
                .ok_or_else(|| format!("{text:?}: missing ')'"))?,
        ),
        None => (text, ""),
    };
    let args: Vec<&str> = args
        .split(',')
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .collect();
    let bad = |what: &str| format!("{text:?}: expected {what}");
    let arg = |i: usize| args.get(i).copied().unwrap_or("");
    let sustain = |i: usize| -> Result<Option<u32>, String> {
        args.get(i)
            .map(|a| parse_secs(a).ok_or_else(|| bad("a time such as 60s")))
            .transpose()
    };
    let cond = match name.to_ascii_lowercase().as_str() {
        "process_cpu_above" => {
            let pct = parse_unit(arg(0), "%").ok_or_else(|| bad("a percent such as 150%"))?;
            return Ok((Condition::ProcessCpuAbove { pct }, sustain(1)?));
        }
        "fans_above" => {
            let rpm = parse_unit(arg(0), "rpm").ok_or_else(|| bad("a speed such as 5000rpm"))?;
            return Ok((Condition::FansAbove { rpm }, sustain(1)?));
        }
        "cpu_total_above" => Condition::CpuTotalAbove {
            pct: parse_unit(arg(0), "%").ok_or_else(|| bad("a percent such as 90%"))?,
        },
        "thermal_at_least" => Condition::ThermalAtLeast(
            parse_thermal(arg(0)).ok_or_else(|| bad("fair, serious or critical"))?,
        ),
        "swap_above" => Condition::SwapAbove {
            bytes: parse_bytes(arg(0)).ok_or_else(|| bad("a size such as 4GB"))?,
        },
        "battery_drain_above" => Condition::BatteryDrainAbove {
            watts: parse_unit(arg(0), "w").ok_or_else(|| bad("watts such as 15W"))?,
        },
        "disk_free_below" => Condition::DiskFreeBelow {
            bytes: parse_bytes(arg(0)).ok_or_else(|| bad("a size such as 10GB"))?,
        },
        "memory_pressure_at_least" => Condition::MemoryPressureAtLeast(
            parse_pressure(arg(0)).ok_or_else(|| bad("warn or critical"))?,
        ),
        _ => return Err(format!("unknown condition {name:?}")),
    };
    if args.len() > 1 {
        return Err(bad("one argument"));
    }
    Ok((cond, None))
}

impl fmt::Display for Condition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::ProcessCpuAbove { pct } => write!(f, "process_cpu_above({pct}%)"),
            Self::CpuTotalAbove { pct } => write!(f, "cpu_total_above({pct}%)"),
            Self::ThermalAtLeast(t) => write!(f, "thermal_at_least({})", t.name()),
            Self::SwapAbove { bytes } => write!(f, "swap_above({})", format_bytes(bytes)),
            Self::BatteryDrainAbove { watts } => write!(f, "battery_drain_above({watts}W)"),
            Self::DiskFreeBelow { bytes } => write!(f, "disk_free_below({})", format_bytes(bytes)),
            Self::FansAbove { rpm } => write!(f, "fans_above({rpm}rpm)"),
            Self::MemoryPressureAtLeast(p) => {
                write!(f, "memory_pressure_at_least({})", p.label())
            }
        }
    }
}

fn disk_free(snap: &Snapshot) -> Option<u64> {
    snap.disk
        .primary()
        .filter(|v| v.total_bytes > 0)
        .map(|v| v.total_bytes.saturating_sub(v.used_bytes))
}

fn fan_peak(snap: &Snapshot) -> Option<u16> {
    snap.fans
        .is_present()
        .then(|| snap.fans.fans.iter().map(|f| f.rpm).max().unwrap_or(0))
}

impl Condition {
    /// The reading now; `None` while unknown or on hardware this Mac lacks.
    fn level(self, snap: &Snapshot, signals: &Signals) -> Option<Level> {
        match self {
            Self::ProcessCpuAbove { pct } => {
                let (_, cpu) = top_process(snap).filter(|_| snap.primed)?;
                Some(Level::above(f64::from(cpu), f64::from(pct)))
            }
            Self::CpuTotalAbove { pct } => snap
                .primed
                .then_some(snap.cpu.active)
                .filter(|a| a.is_finite())
                .map(|a| Level::above(f64::from(a) * 100.0, f64::from(pct))),
            Self::ThermalAtLeast(at) => Some(Level::rank(snap.thermal.level(), at.level())),
            Self::SwapAbove { bytes } => (snap.memory.total_bytes > 0)
                .then(|| Level::above(snap.memory.swap_used_bytes as f64, bytes as f64)),
            Self::BatteryDrainAbove { watts } => signals
                .battery_drain_w
                .filter(|w| w.is_finite())
                .map(|w| Level::above(f64::from(w), f64::from(watts))),
            Self::DiskFreeBelow { bytes } => {
                disk_free(snap).map(|free| Level::below(free as f64, bytes as f64))
            }
            Self::FansAbove { rpm } => {
                fan_peak(snap).map(|peak| Level::above(f64::from(peak), f64::from(rpm)))
            }
            Self::MemoryPressureAtLeast(at) => (snap.memory.total_bytes > 0)
                .then(|| Level::rank(snap.memory.pressure as u8, at as u8)),
        }
    }

    /// Who the reading is about when that can change under a pending rule:
    /// the top process's pid for [`Self::ProcessCpuAbove`]. A new subject
    /// restarts the sustain, so the rule means one process held the line.
    fn subject(self, snap: &Snapshot) -> Option<u32> {
        match self {
            Self::ProcessCpuAbove { .. } => top_process(snap).map(|(p, _)| p.pid),
            _ => None,
        }
    }

    /// The notification title: what is wrong, in plain words.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::ProcessCpuAbove { .. } => "A process is hogging the CPU",
            Self::CpuTotalAbove { .. } => "CPU is busy",
            Self::ThermalAtLeast(_) => "Mac is running hot",
            Self::SwapAbove { .. } => "Swap is large",
            Self::BatteryDrainAbove { .. } => "Battery is draining fast",
            Self::DiskFreeBelow { .. } => "Disk is nearly full",
            Self::FansAbove { .. } => "Fans are loud",
            Self::MemoryPressureAtLeast(_) => "Memory is tight",
        }
    }

    /// One line of evidence from the snapshot that fired it.
    #[must_use]
    pub fn describe(self, snap: &Snapshot, signals: &Signals) -> String {
        match self {
            Self::ProcessCpuAbove { pct } => top_process(snap)
                .map_or_else(String::new, |(p, cpu)| {
                    format!("{} (pid {}) at {cpu:.0}% (limit {pct}%)", p.name, p.pid)
                }),
            Self::CpuTotalAbove { pct } => {
                format!("cpu active {:.0}% (limit {pct}%)", snap.cpu.active * 100.0)
            }
            Self::ThermalAtLeast(_) => {
                format!("thermal pressure {}", snap.thermal.name())
            }
            Self::SwapAbove { bytes } => format!(
                "{} swapped (limit {})",
                bytes_short(snap.memory.swap_used_bytes),
                bytes_short(bytes)
            ),
            Self::BatteryDrainAbove { watts } => format!(
                "discharging at {:.1} W (limit {watts} W)",
                signals.battery_drain_w.unwrap_or(0.0)
            ),
            Self::DiskFreeBelow { bytes } => format!(
                "{} free (limit {})",
                bytes_short(disk_free(snap).unwrap_or(0)),
                bytes_short(bytes)
            ),
            Self::FansAbove { rpm } => format!(
                "fans at {} rpm (limit {rpm} rpm)",
                fan_peak(snap).unwrap_or(0)
            ),
            Self::MemoryPressureAtLeast(_) => {
                format!("memory pressure {}", snap.memory.pressure.label())
            }
        }
    }
}

/// Where a fired alert goes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sinks {
    /// A macOS notification (Notification Center).
    pub notify: bool,
    /// OSC 9 / 777 plus a bell, while a TUI owns the terminal.
    pub terminal: bool,
    /// A shell command run with `SILTOP_ALERT_*` in its environment.
    pub hook: Option<String>,
}

impl Sinks {
    const NAMES: [&'static str; 2] = ["notify", "terminal"];

    #[must_use]
    pub fn standard() -> Self {
        Self {
            notify: true,
            terminal: true,
            hook: None,
        }
    }

    fn names(&self) -> Vec<&'static str> {
        let on = [self.notify, self.terminal];
        Self::NAMES
            .into_iter()
            .zip(on)
            .filter_map(|(name, on)| on.then_some(name))
            .collect()
    }
}

/// An effective rule, after the config has been merged over the defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub id: String,
    pub condition: Condition,
    pub sustain: Duration,
    pub cooldown: Duration,
    pub sinks: Sinks,
    pub enabled: bool,
}

/// A rule as the config file spells it; `None` keeps the built-in value.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuleSpec {
    pub id: String,
    pub condition: Option<Condition>,
    pub sustain_s: Option<u32>,
    pub cooldown_s: Option<u32>,
    pub notify: Option<bool>,
    pub terminal: Option<bool>,
    pub hook: Option<String>,
    pub enabled: Option<bool>,
}

/// A built-in rule: id, condition, sustain, cooldown, on by default.
pub struct Builtin {
    pub id: &'static str,
    pub condition: Condition,
    pub sustain_s: u32,
    pub cooldown_s: u32,
    pub enabled: bool,
}

/// The rules every install starts with. Thresholds that the verdict line
/// also judges come from its constants.
pub const DEFAULT_RULES: [Builtin; 8] = [
    Builtin {
        id: "hot",
        condition: Condition::ThermalAtLeast(THROTTLE_THERMAL),
        sustain_s: 10,
        cooldown_s: 600,
        enabled: true,
    },
    Builtin {
        id: "memory",
        condition: Condition::MemoryPressureAtLeast(TIGHT_PRESSURE),
        sustain_s: 30,
        cooldown_s: 1800,
        enabled: true,
    },
    Builtin {
        id: "runaway",
        condition: Condition::ProcessCpuAbove {
            pct: (BUSY_PROCESS_CORES * 100.0) as u16,
        },
        sustain_s: 60,
        cooldown_s: 900,
        enabled: true,
    },
    Builtin {
        id: "drain",
        condition: Condition::BatteryDrainAbove {
            watts: BATTERY_DRAIN_W as u16,
        },
        sustain_s: 60,
        cooldown_s: 1800,
        enabled: true,
    },
    Builtin {
        id: "disk",
        condition: Condition::DiskFreeBelow { bytes: 10 * GB },
        sustain_s: 5,
        cooldown_s: 6 * 3600,
        enabled: true,
    },
    Builtin {
        id: "busy",
        condition: Condition::CpuTotalAbove {
            pct: (BUSY_TOTAL * 100.0) as u16,
        },
        sustain_s: 120,
        cooldown_s: 1800,
        enabled: false,
    },
    Builtin {
        id: "swap",
        condition: Condition::SwapAbove { bytes: 4 * GB },
        sustain_s: 60,
        cooldown_s: 3600,
        enabled: false,
    },
    Builtin {
        id: "fans",
        condition: Condition::FansAbove { rpm: 5000 },
        sustain_s: 120,
        cooldown_s: 1800,
        enabled: false,
    },
];

impl Builtin {
    fn rule(&self) -> Rule {
        Rule {
            id: String::from(self.id),
            condition: self.condition,
            sustain: Duration::from_secs(u64::from(self.sustain_s)),
            cooldown: Duration::from_secs(u64::from(self.cooldown_s)),
            sinks: Sinks::standard(),
            enabled: self.enabled,
        }
    }
}

impl RuleSpec {
    fn apply(&self, rule: &mut Rule) {
        if let Some(c) = self.condition {
            rule.condition = c;
        }
        if let Some(s) = self.sustain_s {
            rule.sustain = Duration::from_secs(u64::from(s));
        }
        if let Some(s) = self.cooldown_s {
            rule.cooldown = Duration::from_secs(u64::from(s));
        }
        if let Some(on) = self.notify {
            rule.sinks.notify = on;
        }
        if let Some(on) = self.terminal {
            rule.sinks.terminal = on;
        }
        if let Some(hook) = &self.hook {
            rule.sinks.hook = Some(hook.clone()).filter(|h| !h.trim().is_empty());
        }
        if let Some(on) = self.enabled {
            rule.enabled = on;
        }
    }

    /// Reads one `[[alerts]]` table; `Err` says why it was skipped.
    fn from_table(table: &Table) -> Result<Self, String> {
        let id = table
            .get("id")
            .and_then(Item::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| String::from("an [[alerts]] table has no id"))?;
        let time = |key: &str| -> Result<Option<u32>, String> {
            let Some(item) = table.get(key) else {
                return Ok(None);
            };
            item.as_integer()
                .and_then(|n| u32::try_from(n).ok())
                .or_else(|| item.as_str().and_then(parse_secs))
                .map(Some)
                .ok_or_else(|| format!("alert {id}: bad {key}"))
        };
        let flag = |key: &str| -> Result<Option<bool>, String> {
            table
                .get(key)
                .map(|item| {
                    item.as_bool()
                        .ok_or_else(|| format!("alert {id}: bad {key}"))
                })
                .transpose()
        };
        let mut spec = Self {
            id: String::from(id),
            sustain_s: time("sustain")?,
            cooldown_s: time("cooldown")?,
            enabled: flag("enabled")?,
            hook: table.get("hook").and_then(Item::as_str).map(String::from),
            ..Self::default()
        };
        if let Some(when) = table.get("when") {
            let text = when
                .as_str()
                .ok_or_else(|| format!("alert {id}: when must be text"))?;
            let (cond, sustain) = parse_condition(text).map_err(|e| format!("alert {id}: {e}"))?;
            spec.condition = Some(cond);
            spec.sustain_s = spec.sustain_s.or(sustain);
        }
        if let Some(sinks) = table.get("sinks") {
            let names: Vec<&str> = sinks
                .as_array()
                .ok_or_else(|| format!("alert {id}: sinks must be a list"))?
                .iter()
                .filter_map(toml_edit::Value::as_str)
                .collect();
            if let Some(unknown) = names
                .iter()
                .find(|n| !Sinks::NAMES.contains(n) && **n != "hook")
            {
                return Err(format!("alert {id}: unknown sink {unknown:?}"));
            }
            spec.notify = Some(names.contains(&"notify"));
            spec.terminal = Some(names.contains(&"terminal"));
        }
        Ok(spec)
    }

    /// The table keys [`Self::to_table`] writes, besides `id`.
    const KEYS: [&str; 6] = ["when", "sustain", "cooldown", "sinks", "hook", "enabled"];

    /// Rewrites in `table` (read back as `have`) only the keys whose value
    /// differs from `self`, keeping each value's comments. When that is not
    /// enough (`when` can also carry the sustain), every known key is
    /// synced, so the table always reads back as exactly `self`.
    fn update_table(&self, have: &Self, table: &mut Table) {
        if self == have {
            return;
        }
        let differs = [
            self.condition != have.condition,
            self.sustain_s != have.sustain_s,
            self.cooldown_s != have.cooldown_s,
            self.notify != have.notify || self.terminal != have.terminal,
            self.hook != have.hook,
            self.enabled != have.enabled,
        ];
        let fresh = self.to_table();
        let sync = |table: &mut Table, key: &str| match fresh.get(key) {
            Some(Item::Value(new)) => {
                let mut new = new.clone();
                if let Some(old) = table.get(key).and_then(Item::as_value) {
                    *new.decor_mut() = old.decor().clone();
                }
                table.insert(key, Item::Value(new));
            }
            Some(other) => {
                table.insert(key, other.clone());
            }
            None => {
                table.remove(key);
            }
        };
        for (key, _) in Self::KEYS.iter().zip(differs).filter(|(_, d)| *d) {
            sync(table, key);
        }
        if Self::from_table(table).as_ref() != Ok(self) {
            for key in Self::KEYS {
                sync(table, key);
            }
        }
    }

    fn to_table(&self) -> Table {
        let mut table = Table::new();
        table.insert("id", value(self.id.as_str()));
        if let Some(c) = self.condition {
            table.insert("when", value(c.to_string()));
        }
        if let Some(s) = self.sustain_s {
            table.insert("sustain", value(format_secs(s)));
        }
        if let Some(s) = self.cooldown_s {
            table.insert("cooldown", value(format_secs(s)));
        }
        if self.notify.is_some() || self.terminal.is_some() {
            let sinks = Sinks {
                notify: self.notify.unwrap_or(false),
                terminal: self.terminal.unwrap_or(false),
                hook: None,
            };
            table.insert("sinks", value(sinks.names().into_iter().collect::<Array>()));
        }
        if let Some(hook) = &self.hook {
            table.insert("hook", value(hook.as_str()));
        }
        if let Some(on) = self.enabled {
            table.insert("enabled", value(on));
        }
        table
    }
}

/// Every `[[alerts]]` table in `doc`, with one warning per table skipped.
#[must_use]
pub fn read_specs(doc: &DocumentMut) -> (Vec<RuleSpec>, Vec<String>) {
    let mut specs = Vec::new();
    let mut warnings = Vec::new();
    let Some(item) = doc.get("alerts") else {
        return (specs, warnings);
    };
    let Some(tables) = item.as_array_of_tables() else {
        warnings.push(String::from("ignored alerts: expected [[alerts]] tables"));
        return (specs, warnings);
    };
    for table in tables {
        match RuleSpec::from_table(table) {
            Ok(spec) => specs.push(spec),
            Err(why) => warnings.push(format!("ignored {why}")),
        }
    }
    (specs, warnings)
}

/// Brings the `[[alerts]]` tables in `doc` to `specs`, editing in place:
/// a table whose spec is unchanged is left byte for byte, a changed one
/// rewrites only the keys that changed (comments and unknown keys stay),
/// a table this build could not read is never touched, dropped specs lose
/// their table and new ones are appended.
pub fn write_specs(doc: &mut DocumentMut, specs: &[RuleSpec]) {
    let Some(tables) = doc.get_mut("alerts").and_then(Item::as_array_of_tables_mut) else {
        doc.remove("alerts");
        if !specs.is_empty() {
            let mut tables = ArrayOfTables::new();
            for spec in specs {
                tables.push(spec.to_table());
            }
            doc.insert("alerts", Item::ArrayOfTables(tables));
        }
        return;
    };
    let mut want = specs.iter().peekable();
    let mut dropped = Vec::new();
    for (at, table) in tables.iter_mut().enumerate() {
        let Ok(have) = RuleSpec::from_table(table) else {
            continue;
        };
        match want.next_if(|spec| spec.id == have.id) {
            Some(spec) => spec.update_table(&have, table),
            None => dropped.push(at),
        }
    }
    for at in dropped.into_iter().rev() {
        tables.remove(at);
    }
    for spec in want {
        tables.push(spec.to_table());
    }
    if tables.is_empty() {
        doc.remove("alerts");
    }
}

/// The built-in rules with `specs` merged over them, then added rules in
/// file order. A later spec for the same id wins key by key.
#[must_use]
pub fn effective_rules(specs: &[RuleSpec]) -> Vec<Rule> {
    let mut rules: Vec<Rule> = DEFAULT_RULES.iter().map(Builtin::rule).collect();
    for spec in specs {
        if let Some(rule) = rules.iter_mut().find(|r| r.id == spec.id) {
            spec.apply(rule);
        } else if let Some(condition) = spec.condition {
            let mut rule = Rule {
                id: spec.id.clone(),
                condition,
                sustain: Duration::ZERO,
                cooldown: Duration::from_secs(600),
                sinks: Sinks::standard(),
                enabled: true,
            };
            spec.apply(&mut rule);
            rules.push(rule);
        }
    }
    rules
}

/// Whether rule `id` is on after merging; unknown ids read as off. The
/// settings page asks every frame, so this walks the specs, not a merge.
#[must_use]
pub fn rule_enabled(specs: &[RuleSpec], id: &str) -> bool {
    let builtin = DEFAULT_RULES.iter().find(|b| b.id == id);
    let mut own = specs.iter().filter(|s| s.id == id);
    let first_real = own.clone().position(|s| s.condition.is_some());
    // An added rule exists from its first table with a condition on.
    let (skip, default) = match (builtin, first_real) {
        (Some(b), _) => (0, b.enabled),
        (None, Some(at)) => (at, true),
        (None, None) => return false,
    };
    own.nth(skip)
        .into_iter()
        .chain(own)
        .fold(default, |on, s| s.enabled.unwrap_or(on))
}

/// Turns rule `id` on or off, keeping every other key the user wrote. A
/// spec that ends up matching the built-in rule again is dropped.
pub fn set_rule_enabled(specs: &mut Vec<RuleSpec>, id: &str, on: bool) {
    let builtin = DEFAULT_RULES.iter().find(|b| b.id == id);
    match specs.iter_mut().rev().find(|s| s.id == id) {
        Some(spec) => spec.enabled = Some(on),
        None => specs.push(RuleSpec {
            id: String::from(id),
            enabled: Some(on),
            ..RuleSpec::default()
        }),
    }
    if let Some(b) = builtin {
        specs.retain(|s| {
            s.id != id
                || *s
                    != RuleSpec {
                        id: String::from(id),
                        enabled: Some(b.enabled),
                        ..RuleSpec::default()
                    }
        });
    }
}

/// One alert to deliver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fired {
    pub id: String,
    pub title: String,
    pub body: String,
    pub sinks: Sinks,
}

#[derive(Debug, Clone, Default)]
struct RuleState {
    /// When the condition started holding, while not yet fired.
    pending: Option<Instant>,
    /// Whom `pending` is timing ([`Condition::subject`]).
    subject: Option<u32>,
    /// Fired and not yet cleared past the hysteresis band.
    active: bool,
    last_fired: Option<Instant>,
}

/// Evaluates rules over successive snapshots: sustain, hysteresis, cooldown.
#[derive(Debug, Clone, Default)]
pub struct AlertEngine {
    rules: Vec<Rule>,
    states: Vec<RuleState>,
}

impl AlertEngine {
    #[must_use]
    pub fn new(rules: Vec<Rule>) -> Self {
        let states = vec![RuleState::default(); rules.len()];
        Self { rules, states }
    }

    #[must_use]
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Swaps in a new rule set (a settings toggle) and carries each rule's
    /// sustain, active and cooldown state across by id, so a rule that is
    /// still firing is not delivered again. A rule whose condition changed
    /// starts over.
    pub fn reconfigure(&mut self, rules: Vec<Rule>) {
        let mut old: Vec<(Rule, RuleState)> = std::mem::take(&mut self.rules)
            .into_iter()
            .zip(std::mem::take(&mut self.states))
            .collect();
        self.states = rules
            .iter()
            .map(|rule| {
                old.iter()
                    .position(|(o, _)| o.id == rule.id && o.condition == rule.condition)
                    .map(|at| old.swap_remove(at).1)
                    .unwrap_or_default()
            })
            .collect();
        self.rules = rules;
    }

    /// Ids of rules that fired and have not cleared yet.
    pub fn active(&self) -> impl Iterator<Item = &str> {
        self.rules
            .iter()
            .zip(&self.states)
            .filter(|(_, s)| s.active)
            .map(|(r, _)| r.id.as_str())
    }

    /// Feeds one snapshot taken at `now`; returns the alerts to deliver.
    pub fn update(&mut self, snap: &Snapshot, signals: &Signals, now: Instant) -> Vec<Fired> {
        let mut fired = Vec::new();
        for (rule, state) in self.rules.iter().zip(self.states.iter_mut()) {
            if !rule.enabled {
                *state = RuleState::default();
                continue;
            }
            // Unknown or not applicable (drain on AC) counts as cleared;
            // `last_fired` stays, so the cooldown still holds a re-fire.
            let Some(level) = rule.condition.level(snap, signals) else {
                state.active = false;
                state.pending = None;
                continue;
            };
            if state.active {
                if level.cleared() {
                    state.active = false;
                    state.pending = None;
                }
                continue;
            }
            if !level.holds() {
                state.pending = None;
                continue;
            }
            let subject = rule.condition.subject(snap);
            if state.subject != subject {
                state.subject = subject;
                state.pending = None;
            }
            let since = *state.pending.get_or_insert(now);
            let sustained = now.saturating_duration_since(since) >= rule.sustain;
            let rested = state
                .last_fired
                .is_none_or(|at| now.saturating_duration_since(at) >= rule.cooldown);
            if sustained && rested {
                state.active = true;
                state.pending = None;
                state.last_fired = Some(now);
                fired.push(Fired {
                    id: rule.id.clone(),
                    title: String::from(rule.condition.title()),
                    body: rule.condition.describe(snap, signals),
                    sinks: rule.sinks.clone(),
                });
            }
        }
        fired
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::snapshot::{DiskVolume, FanMetric, Process};

    fn machine() -> Snapshot {
        let mut snap = Snapshot::empty();
        snap.primed = true;
        snap.memory.total_bytes = 16 * GB;
        snap
    }

    fn with_cpu(active: f32) -> Snapshot {
        let mut snap = machine();
        snap.cpu.active = active;
        snap
    }

    fn rule(condition: Condition, sustain: u64, cooldown: u64) -> Rule {
        Rule {
            id: String::from("t"),
            condition,
            sustain: Duration::from_secs(sustain),
            cooldown: Duration::from_secs(cooldown),
            sinks: Sinks::standard(),
            enabled: true,
        }
    }

    fn secs(t0: Instant, s: u64) -> Instant {
        t0 + Duration::from_secs(s)
    }

    const NONE: Signals = Signals {
        swap_growth_bps: None,
        battery_drain_w: None,
    };

    #[test]
    fn fires_only_after_sustain() {
        let mut engine = AlertEngine::new(vec![rule(Condition::CpuTotalAbove { pct: 90 }, 30, 0)]);
        let t0 = Instant::now();
        let hot = with_cpu(0.95);
        assert!(engine.update(&hot, &NONE, t0).is_empty());
        assert!(engine.update(&hot, &NONE, secs(t0, 29)).is_empty());
        let fired = engine.update(&hot, &NONE, secs(t0, 30));
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].title, "CPU is busy");
        assert!(fired[0].body.contains("95%"), "{}", fired[0].body);
        assert_eq!(engine.active().collect::<Vec<_>>(), ["t"]);
    }

    #[test]
    fn a_dip_below_threshold_restarts_sustain() {
        let mut engine = AlertEngine::new(vec![rule(Condition::CpuTotalAbove { pct: 90 }, 30, 0)]);
        let t0 = Instant::now();
        engine.update(&with_cpu(0.95), &NONE, t0);
        engine.update(&with_cpu(0.50), &NONE, secs(t0, 20));
        assert!(
            engine
                .update(&with_cpu(0.95), &NONE, secs(t0, 31))
                .is_empty()
        );
        assert_eq!(engine.update(&with_cpu(0.95), &NONE, secs(t0, 61)).len(), 1);
    }

    #[test]
    fn hovering_on_the_line_fires_once_hysteresis() {
        let mut engine = AlertEngine::new(vec![rule(Condition::CpuTotalAbove { pct: 90 }, 0, 0)]);
        let t0 = Instant::now();
        let mut count = 0;
        // 92 → 85 → 92 ...: inside the 10% band, never clears.
        for i in 0..20 {
            let active = if i % 2 == 0 { 0.92 } else { 0.85 };
            count += engine.update(&with_cpu(active), &NONE, secs(t0, i)).len();
        }
        assert_eq!(count, 1);
        // Dropping past the band clears; the next crossing fires again.
        engine.update(&with_cpu(0.80), &NONE, secs(t0, 30));
        assert!(engine.active().next().is_none());
        assert_eq!(engine.update(&with_cpu(0.91), &NONE, secs(t0, 31)).len(), 1);
    }

    #[test]
    fn cooldown_holds_a_refire_until_it_expires() {
        let mut engine = AlertEngine::new(vec![rule(Condition::CpuTotalAbove { pct: 90 }, 0, 600)]);
        let t0 = Instant::now();
        assert_eq!(engine.update(&with_cpu(0.95), &NONE, t0).len(), 1);
        engine.update(&with_cpu(0.10), &NONE, secs(t0, 10));
        assert!(
            engine
                .update(&with_cpu(0.95), &NONE, secs(t0, 20))
                .is_empty()
        );
        assert!(
            engine
                .update(&with_cpu(0.95), &NONE, secs(t0, 599))
                .is_empty()
        );
        assert_eq!(
            engine.update(&with_cpu(0.95), &NONE, secs(t0, 600)).len(),
            1
        );
    }

    #[test]
    fn disabled_rules_never_fire() {
        let mut r = rule(Condition::CpuTotalAbove { pct: 10 }, 0, 0);
        r.enabled = false;
        let mut engine = AlertEngine::new(vec![r]);
        assert!(
            engine
                .update(&with_cpu(0.99), &NONE, Instant::now())
                .is_empty()
        );
    }

    #[test]
    fn fanless_air_never_fires_fan_rule() {
        let mut engine = AlertEngine::new(vec![rule(Condition::FansAbove { rpm: 1 }, 0, 0)]);
        assert!(engine.update(&machine(), &NONE, Instant::now()).is_empty());
        let mut loud = machine();
        loud.fans.fans = vec![FanMetric {
            name: String::from("left"),
            rpm: 6000,
            max_rpm: 7000,
        }];
        assert_eq!(engine.update(&loud, &NONE, Instant::now()).len(), 1);
    }

    #[test]
    fn desktop_without_battery_never_fires_drain_rule() {
        let mut engine =
            AlertEngine::new(vec![rule(Condition::BatteryDrainAbove { watts: 5 }, 0, 0)]);
        assert!(engine.update(&machine(), &NONE, Instant::now()).is_empty());
        let draining = Signals {
            battery_drain_w: Some(22.0),
            ..NONE
        };
        let fired = engine.update(&machine(), &draining, Instant::now());
        assert!(fired[0].body.contains("22.0 W"), "{}", fired[0].body);
    }

    /// Plugging in made the drain unknown, which once left `drain` active
    /// for good: a lit status line on AC, and no alert the next time the
    /// battery drained just as hard.
    #[test]
    fn drain_alert_clears_when_plugged_in_and_fires_again_on_battery() {
        let mut engine = AlertEngine::new(vec![rule(
            Condition::BatteryDrainAbove { watts: 15 },
            0,
            60,
        )]);
        let t0 = Instant::now();
        let draining = Signals {
            battery_drain_w: Some(20.0),
            ..NONE
        };
        assert_eq!(engine.update(&machine(), &draining, t0).len(), 1);
        assert!(engine.update(&machine(), &NONE, secs(t0, 10)).is_empty());
        assert!(engine.active().next().is_none(), "on AC: not draining");
        assert!(
            engine
                .update(&machine(), &draining, secs(t0, 30))
                .is_empty(),
            "the cooldown still holds"
        );
        assert_eq!(
            engine.update(&machine(), &draining, secs(t0, 3600)).len(),
            1
        );
        assert_eq!(engine.active().collect::<Vec<_>>(), ["t"]);
    }

    #[test]
    fn unprimed_snapshot_is_unknown_not_zero() {
        let mut engine = AlertEngine::new(vec![rule(Condition::CpuTotalAbove { pct: 1 }, 0, 0)]);
        let mut snap = with_cpu(0.99);
        snap.primed = false;
        assert!(engine.update(&snap, &NONE, Instant::now()).is_empty());
    }

    #[test]
    fn thermal_and_pressure_levels_clear_when_they_drop_a_step() {
        let mut engine = AlertEngine::new(vec![rule(
            Condition::ThermalAtLeast(Thermal::Serious),
            0,
            0,
        )]);
        let t0 = Instant::now();
        let mut snap = machine();
        snap.thermal = Thermal::Fair;
        assert!(engine.update(&snap, &NONE, t0).is_empty());
        snap.thermal = Thermal::Critical;
        assert_eq!(engine.update(&snap, &NONE, t0).len(), 1);
        snap.thermal = Thermal::Fair;
        engine.update(&snap, &NONE, t0);
        assert!(engine.active().next().is_none());

        let mut engine = AlertEngine::new(vec![rule(
            Condition::MemoryPressureAtLeast(Pressure::Warn),
            0,
            0,
        )]);
        snap.memory.pressure = Pressure::Critical;
        assert_eq!(engine.update(&snap, &NONE, t0).len(), 1);
    }

    #[test]
    fn disk_free_below_trips_under_and_clears_above_band() {
        let mut engine = AlertEngine::new(vec![rule(
            Condition::DiskFreeBelow { bytes: 10 * GB },
            0,
            0,
        )]);
        let mut snap = machine();
        let t0 = Instant::now();
        let volume = |free: u64| DiskVolume {
            name: String::from("Macintosh HD"),
            mount: String::from("/"),
            used_bytes: 500 * GB - free,
            total_bytes: 500 * GB,
        };
        snap.disk.volumes = vec![volume(8 * GB)];
        let fired = engine.update(&snap, &NONE, t0);
        assert!(fired[0].body.starts_with("8.0G free"), "{}", fired[0].body);
        snap.disk.volumes = vec![volume(10 * GB + GB / 2)];
        engine.update(&snap, &NONE, t0);
        assert!(engine.active().next().is_some(), "inside the band");
        snap.disk.volumes = vec![volume(12 * GB)];
        engine.update(&snap, &NONE, t0);
        assert!(engine.active().next().is_none());
    }

    #[test]
    fn runaway_process_names_the_culprit() {
        let mut engine =
            AlertEngine::new(vec![rule(Condition::ProcessCpuAbove { pct: 150 }, 0, 0)]);
        let mut snap = machine();
        snap.processes = vec![Process {
            pid: 42,
            name: String::from("mds_stores"),
            cpu: Some(310.0),
            ..Process::default()
        }];
        let fired = engine.update(&snap, &NONE, Instant::now());
        assert_eq!(fired[0].body, "mds_stores (pid 42) at 310% (limit 150%)");
    }

    /// Regression: a build's short-lived rustc processes took turns on top,
    /// and the rule fired on a 3 s old pid as if it had run away.
    #[test]
    fn runaway_sustain_restarts_when_the_top_process_changes() {
        let t0 = Instant::now();
        let mut engine =
            AlertEngine::new(vec![rule(Condition::ProcessCpuAbove { pct: 150 }, 60, 0)]);
        let hog = |pid: u32| {
            let mut snap = machine();
            snap.processes = vec![Process {
                pid,
                name: String::from("rustc"),
                cpu: Some(380.0),
                ..Process::default()
            }];
            snap
        };
        for (i, s) in (0..=60).step_by(10).enumerate() {
            let fired = engine.update(&hog(100 + i as u32), &NONE, secs(t0, s));
            assert!(fired.is_empty(), "fired at {s}s on a fresh pid");
        }
        engine.update(&hog(7), &NONE, secs(t0, 70));
        assert!(engine.update(&hog(7), &NONE, secs(t0, 120)).is_empty());
        let fired = engine.update(&hog(7), &NONE, secs(t0, 130));
        assert_eq!(fired.len(), 1);
        assert!(fired[0].body.contains("pid 7"), "{}", fired[0].body);
    }

    /// Regression: toggling any alert rebuilt the engine, so a rule still
    /// holding (disk nearly full, 6 h cooldown) fired again seconds later.
    #[test]
    fn reconfiguring_keeps_active_and_cooldown_state_by_id() {
        let t0 = Instant::now();
        let hot = |id: &str| Rule {
            id: String::from(id),
            ..rule(Condition::CpuTotalAbove { pct: 50 }, 0, 3600)
        };
        let mut engine = AlertEngine::new(vec![hot("a")]);
        let busy = with_cpu(0.9);
        assert_eq!(engine.update(&busy, &NONE, t0).len(), 1);
        engine.reconfigure(vec![hot("b"), hot("a")]);
        let fired = engine.update(&busy, &NONE, secs(t0, 5));
        assert_eq!(fired.len(), 1, "only the new rule fires");
        assert_eq!(fired[0].id, "b");
        assert_eq!(engine.active().collect::<Vec<_>>(), ["b", "a"]);
        // A changed condition is a new rule: it starts over.
        let mut stricter = hot("a");
        stricter.condition = Condition::CpuTotalAbove { pct: 60 };
        engine.reconfigure(vec![stricter]);
        assert_eq!(engine.update(&busy, &NONE, secs(t0, 10)).len(), 1);
    }

    #[test]
    fn condition_display_round_trips_through_parse() {
        let all = [
            Condition::ProcessCpuAbove { pct: 80 },
            Condition::CpuTotalAbove { pct: 90 },
            Condition::ThermalAtLeast(Thermal::Serious),
            Condition::SwapAbove { bytes: 4 * GB },
            Condition::SwapAbove { bytes: 512 * MB },
            Condition::BatteryDrainAbove { watts: 15 },
            Condition::DiskFreeBelow { bytes: 10 * GB },
            Condition::FansAbove { rpm: 5000 },
            Condition::MemoryPressureAtLeast(Pressure::Critical),
        ];
        for cond in all {
            assert_eq!(
                parse_condition(&cond.to_string()),
                Ok((cond, None)),
                "{cond}"
            );
        }
    }

    /// Regression: `--json` says `"pressure":"nominal"`, but a rule spelled
    /// that way failed to parse (alerts only knew `normal`).
    #[test]
    fn pressure_parses_as_json_spells_it_and_keeps_the_old_alias() {
        for text in [
            "memory_pressure_at_least(nominal)",
            "memory_pressure_at_least(Normal)",
        ] {
            assert_eq!(
                parse_condition(text),
                Ok((Condition::MemoryPressureAtLeast(Pressure::Nominal), None)),
                "{text}"
            );
        }
        assert_eq!(
            Condition::MemoryPressureAtLeast(Pressure::Nominal).to_string(),
            "memory_pressure_at_least(nominal)"
        );
    }

    #[test]
    fn condition_parse_table() {
        let ok: &[(&str, Condition, Option<u32>)] = &[
            (
                "process_cpu_above(80%, 60s)",
                Condition::ProcessCpuAbove { pct: 80 },
                Some(60),
            ),
            (
                "fans_above(5000rpm, 2m)",
                Condition::FansAbove { rpm: 5000 },
                Some(120),
            ),
            (
                " swap_above( 4 GB ) ",
                Condition::SwapAbove { bytes: 4 * GB },
                None,
            ),
            (
                "swap_above(1.5GiB)",
                Condition::SwapAbove { bytes: 3 * GB / 2 },
                None,
            ),
            (
                "THERMAL_AT_LEAST(Critical)",
                Condition::ThermalAtLeast(Thermal::Critical),
                None,
            ),
            (
                "battery_drain_above(15w)",
                Condition::BatteryDrainAbove { watts: 15 },
                None,
            ),
            (
                "cpu_total_above(90)",
                Condition::CpuTotalAbove { pct: 90 },
                None,
            ),
        ];
        for (text, cond, sustain) in ok {
            assert_eq!(parse_condition(text), Ok((*cond, *sustain)), "{text}");
        }
        for bad in [
            "",
            "melt(1)",
            "swap_above(lots)",
            "thermal_at_least(spicy)",
            "cpu_total_above(90",
            "cpu_total_above(90, 10s)",
            "process_cpu_above(80%, soon)",
        ] {
            assert!(parse_condition(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn secs_parse_and_format_table() {
        for (text, secs) in [
            ("30", 30),
            ("30s", 30),
            ("2m", 120),
            ("5min", 300),
            ("1h", 3600),
        ] {
            assert_eq!(parse_secs(text), Some(secs), "{text}");
        }
        assert_eq!(parse_secs("soon"), None);
        assert_eq!(parse_secs("99999999h"), None, "overflow is rejected");
        for (secs, text) in [
            (0, "0s"),
            (45, "45s"),
            (120, "2m"),
            (7200, "2h"),
            (90, "90s"),
        ] {
            assert_eq!(format_secs(secs), text);
            assert_eq!(parse_secs(text), Some(secs));
        }
    }

    fn doc(text: &str) -> DocumentMut {
        text.parse().unwrap()
    }

    #[test]
    fn specs_parse_merge_and_add_rules() {
        let (specs, warnings) = read_specs(&doc("[[alerts]]\nid = \"hot\"\nenabled = false\n\n\
             [[alerts]]\nid = \"build\"\nwhen = \"process_cpu_above(400%, 5m)\"\nsinks = [\"terminal\"]\nhook = \"say done\"\n"));
        assert!(warnings.is_empty(), "{warnings:?}");
        let rules = effective_rules(&specs);
        assert_eq!(rules.len(), DEFAULT_RULES.len() + 1);
        let hot = rules.iter().find(|r| r.id == "hot").unwrap();
        assert!(!hot.enabled);
        assert_eq!(hot.condition, Condition::ThermalAtLeast(Thermal::Serious));
        let build = rules.last().unwrap();
        assert_eq!(build.id, "build");
        assert_eq!(build.sustain, Duration::from_secs(300));
        assert!(!build.sinks.notify && build.sinks.terminal);
        assert_eq!(build.sinks.hook.as_deref(), Some("say done"));
    }

    #[test]
    fn bad_tables_are_skipped_with_a_warning_each() {
        let (specs, warnings) = read_specs(&doc("[[alerts]]\nwhen = \"swap_above(1GB)\"\n\n\
             [[alerts]]\nid = \"x\"\nwhen = \"melt(1)\"\n\n\
             [[alerts]]\nid = \"y\"\nsinks = [\"pager\"]\n\n\
             [[alerts]]\nid = \"z\"\nsustain = true\n\n\
             [[alerts]]\nid = \"ok\"\nwhen = \"swap_above(1GB)\"\n"));
        assert_eq!(specs.len(), 1);
        assert_eq!(warnings.len(), 4, "{warnings:?}");
        let (_, warnings) = read_specs(&doc("alerts = 3\n"));
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn specs_round_trip_through_the_document() {
        let specs = vec![
            RuleSpec {
                id: String::from("hot"),
                enabled: Some(false),
                ..RuleSpec::default()
            },
            RuleSpec {
                id: String::from("build"),
                condition: Some(Condition::ProcessCpuAbove { pct: 400 }),
                sustain_s: Some(300),
                cooldown_s: Some(3600),
                notify: Some(true),
                terminal: Some(false),
                hook: Some(String::from("say \"done\"")),
                enabled: Some(true),
            },
        ];
        let mut document = doc("theme = \"paper\" # mine\n");
        write_specs(&mut document, &specs);
        let text = document.to_string();
        assert!(text.starts_with("theme = \"paper\" # mine\n"), "{text}");
        let (back, warnings) = read_specs(&doc(&text));
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(back, specs);
        write_specs(&mut document, &[]);
        assert!(!document.to_string().contains("alerts"));
    }

    /// Regression: toggling any rule rewrote every `[[alerts]]` table, so a
    /// custom rule this build could not read (and its hook), comments,
    /// unknown keys and the user's spelling of values were all deleted.
    #[test]
    fn toggling_a_rule_keeps_unreadable_tables_comments_and_spelling() {
        let text = "[[alerts]]\nid = \"swap\"\nwhen = \"swap_above(1.1GB)\"\nsustain = 90 # 1.5 min\nnote = \"mine\"\n\n\
             # my build watcher\n[[alerts]]\nid = \"build\"\nwhen = \"process_cpu_above(400%, 1m30s)\"\nhook = \"say done\"\n\n\
             [[alerts]]\nid = \"hot\"\nenabled = true # loud\n";
        let mut document = doc(text);
        let (mut specs, warnings) = read_specs(&document);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        set_rule_enabled(&mut specs, "hot", false);
        write_specs(&mut document, &specs);
        let out = document.to_string();
        let untouched = text.split("[[alerts]]\nid = \"hot\"").next().unwrap();
        assert!(out.starts_with(untouched), "{out}");
        assert!(
            out.ends_with("id = \"hot\"\nenabled = false # loud\n"),
            "{out}"
        );
        assert_eq!(read_specs(&doc(&out)).0, specs);
        // Adding a rule appends; the unreadable table still survives.
        set_rule_enabled(&mut specs, "busy", true);
        write_specs(&mut document, &specs);
        let out = document.to_string();
        assert!(out.starts_with(untouched), "{out}");
        assert_eq!(read_specs(&doc(&out)).0, specs);
        // Dropping every readable spec keeps only the unreadable table.
        write_specs(&mut document, &[]);
        let out = document.to_string();
        assert!(out.contains("hook = \"say done\""), "{out}");
        assert!(!out.contains("swap_above"), "{out}");
    }

    /// Changing a condition whose `when` carried the sustain keeps the
    /// sustain: the table must read back as exactly the new spec.
    #[test]
    fn rewriting_a_when_with_an_inline_sustain_keeps_the_sustain() {
        let mut document =
            doc("[[alerts]]\nid = \"b\"\nwhen = \"process_cpu_above(400%, 5m)\" # keep\n");
        let (mut specs, _) = read_specs(&document);
        specs[0].condition = Some(Condition::ProcessCpuAbove { pct: 200 });
        write_specs(&mut document, &specs);
        let out = document.to_string();
        assert_eq!(read_specs(&doc(&out)).0, specs, "{out}");
        assert!(out.contains("# keep"), "{out}");
    }

    #[test]
    fn toggling_a_builtin_back_to_default_drops_the_table() {
        let mut specs = Vec::new();
        assert!(rule_enabled(&specs, "hot"));
        set_rule_enabled(&mut specs, "hot", false);
        assert!(!rule_enabled(&specs, "hot"));
        assert_eq!(specs.len(), 1);
        set_rule_enabled(&mut specs, "hot", true);
        assert!(specs.is_empty(), "{specs:?}");
        // A table with other keys the user wrote survives the toggle.
        let mut specs = vec![RuleSpec {
            id: String::from("busy"),
            sustain_s: Some(5),
            ..RuleSpec::default()
        }];
        set_rule_enabled(&mut specs, "busy", true);
        assert_eq!(specs[0].sustain_s, Some(5));
        assert!(rule_enabled(&specs, "busy"));
        assert!(!rule_enabled(&specs, "no-such-rule"));
    }

    #[test]
    fn rule_enabled_agrees_with_the_full_merge() {
        let spec = |id: &str, cond: bool, enabled: Option<bool>| RuleSpec {
            id: String::from(id),
            condition: cond.then_some(Condition::CpuTotalAbove { pct: 50 }),
            enabled,
            ..RuleSpec::default()
        };
        let cases: Vec<Vec<RuleSpec>> = vec![
            vec![],
            vec![spec("hot", false, Some(false))],
            vec![
                spec("hot", false, Some(false)),
                spec("hot", false, Some(true)),
            ],
            vec![spec("busy", true, None)],
            vec![spec("x", false, Some(true))],
            vec![spec("x", true, None)],
            vec![spec("x", true, Some(false))],
            vec![spec("x", false, Some(false)), spec("x", true, None)],
            vec![spec("x", true, None), spec("x", false, Some(false))],
        ];
        for specs in cases {
            let merged = effective_rules(&specs);
            for id in ["hot", "busy", "x", "y"] {
                let expect = merged.iter().any(|r| r.id == id && r.enabled);
                assert_eq!(rule_enabled(&specs, id), expect, "{id} in {specs:?}");
            }
        }
    }

    #[test]
    fn builtin_thresholds_follow_the_verdict() {
        let rules = effective_rules(&[]);
        let get = |id: &str| rules.iter().find(|r| r.id == id).unwrap().condition;
        assert_eq!(get("runaway"), Condition::ProcessCpuAbove { pct: 150 });
        assert_eq!(get("busy"), Condition::CpuTotalAbove { pct: 75 });
        assert_eq!(get("drain"), Condition::BatteryDrainAbove { watts: 15 });
        assert_eq!(get("hot"), Condition::ThermalAtLeast(THROTTLE_THERMAL));
        let ids: Vec<&str> = DEFAULT_RULES.iter().map(|b| b.id).collect();
        let mut unique = ids.clone();
        unique.dedup();
        assert_eq!(ids, unique, "ids are unique");
    }

    #[test]
    fn every_builtin_is_quiet_on_an_idle_mac() {
        let mut engine = AlertEngine::new(effective_rules(&[]));
        let t0 = Instant::now();
        for s in 0..400 {
            assert!(engine.update(&machine(), &NONE, secs(t0, s)).is_empty());
        }
    }
}
