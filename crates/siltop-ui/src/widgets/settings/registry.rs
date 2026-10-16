//! Every setting as data. Painting, clicking and keys all walk this one
//! table, so a row can never read one way and change another.

use std::borrow::Cow;
use std::sync::LazyLock;
use std::time::Duration;

use siltop_core::{
    Config, CpuUnit, PROC_RATIO_MAX, PROC_RATIO_MIN, Preset, ProcSort, Switch, parse_surface,
    rule_enabled, set_rule_enabled, surface_label,
};

use crate::theme::{Icons, Theme};

/// Left-column groups, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Category {
    #[default]
    General,
    Appearance,
    Panels,
    Processes,
    Alerts,
    Keys,
    Privacy,
    About,
}

impl Category {
    pub const ALL: [Self; 8] = [
        Self::General,
        Self::Appearance,
        Self::Panels,
        Self::Processes,
        Self::Alerts,
        Self::Keys,
        Self::Privacy,
        Self::About,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Appearance => "appearance",
            Self::Panels => "panels",
            Self::Processes => "processes",
            Self::Alerts => "alerts",
            Self::Keys => "keys",
            Self::Privacy => "privacy",
            Self::About => "about",
        }
    }

    /// The neighbour `delta` steps away, wrapping at both ends.
    #[must_use]
    pub fn step(self, delta: i32) -> Self {
        let len = Self::ALL.len() as i32;
        let at = Self::ALL.iter().position(|c| *c == self).unwrap_or(0) as i32;
        let next = (at + delta).rem_euclid(len) as usize;
        Self::ALL.get(next).copied().unwrap_or_default()
    }
}

/// Which way a value moves: `←` / `h` go back, everything else forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Prev,
    Next,
}

/// A setting's value in the one shape its [`Kind`] understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    Bool(bool),
    /// Index into the kind's option list.
    Choice(usize),
    Number(u16),
    /// A sample interval in milliseconds, preset or not.
    Millis(u64),
}

#[derive(Debug, Clone, Copy)]
pub enum Kind {
    Toggle,
    Cycle(fn() -> &'static [&'static str]),
    Number {
        min: u16,
        max: u16,
        step: u16,
        unit: &'static str,
    },
    /// The sample interval: shows any value (`-i 750ms`) and steps to the
    /// nearest preset that way, clamped at both ends like `[` / `]`.
    Interval,
}

/// One row of the settings screen.
#[derive(Debug, Clone, Copy)]
pub struct SettingDef {
    /// Stable id, named after the config key it writes.
    pub id: &'static str,
    pub category: Category,
    pub label: &'static str,
    /// One plain-English line shown under the list for the focused row.
    pub description: &'static str,
    pub kind: Kind,
    pub get: fn(&Config) -> Value,
    pub set: fn(&mut Config, Value),
}

impl SettingDef {
    /// The value moved one step: toggles flip, cycles wrap, numbers clamp.
    #[must_use]
    pub fn stepped(&self, config: &Config, step: Step) -> Value {
        match (self.kind, (self.get)(config)) {
            (Kind::Toggle, Value::Bool(on)) => Value::Bool(!on),
            (Kind::Cycle(options), Value::Choice(at)) => {
                let len = options().len().max(1);
                Value::Choice(match step {
                    Step::Next => (at + 1) % len,
                    Step::Prev => at.checked_sub(1).unwrap_or(len - 1),
                })
            }
            (
                Kind::Number {
                    min, max, step: by, ..
                },
                Value::Number(n),
            ) => Value::Number(
                match step {
                    Step::Next => n.saturating_add(by),
                    Step::Prev => n.saturating_sub(by),
                }
                .clamp(min, max),
            ),
            (Kind::Interval, Value::Millis(ms)) => {
                let current = Duration::from_millis(ms);
                let next = config.clone().with_interval(current);
                let next = next.step_interval(step == Step::Prev);
                Value::Millis(u64::try_from(next.as_millis()).unwrap_or(ms))
            }
            (_, other) => other,
        }
    }

    /// Apply one step to `config`.
    pub fn step(&self, config: &mut Config, step: Step) {
        let next = self.stepped(config, step);
        (self.set)(config, next);
    }

    /// The value as the screen shows it.
    #[must_use]
    pub fn display(&self, config: &Config) -> Cow<'static, str> {
        if self.id == "preset" && Preset::matching(config).is_none() && default_look(config) {
            // Nothing picked and nothing changed: not a custom look.
            return Cow::Borrowed("default");
        }
        match (self.kind, (self.get)(config)) {
            (_, Value::Bool(on)) => Cow::Borrowed(if on { "on" } else { "off" }),
            (Kind::Cycle(options), Value::Choice(at)) => {
                Cow::Borrowed(options().get(at).copied().unwrap_or("?"))
            }
            (Kind::Number { unit, .. }, Value::Number(n)) => Cow::Owned(format!("{n}{unit}")),
            (_, Value::Millis(ms)) => Cow::Owned(crate::widgets::interval_label(ms)),
            (_, Value::Choice(at)) => Cow::Owned(at.to_string()),
            (_, Value::Number(n)) => Cow::Owned(n.to_string()),
        }
    }

    /// Case-insensitive match on the label, the id or the description.
    #[must_use]
    pub fn matches(&self, needle: &str) -> bool {
        let needle = needle.trim().to_ascii_lowercase();
        needle.is_empty()
            || [self.label, self.id, self.description]
                .iter()
                .any(|hay| hay.to_ascii_lowercase().contains(&needle))
    }
}

/// Position of `current` in `options`, forgiving case; unknown reads as the first.
fn choice(options: &[&str], current: &str) -> Value {
    Value::Choice(
        options
            .iter()
            .position(|o| o.eq_ignore_ascii_case(current.trim()))
            .unwrap_or(0),
    )
}

fn pick(options: &'static [&'static str], value: Value) -> Option<&'static str> {
    match value {
        Value::Choice(at) => options.get(at).copied(),
        _ => None,
    }
}

fn bool_of(value: Value) -> Option<bool> {
    match value {
        Value::Bool(on) => Some(on),
        _ => None,
    }
}

const SURFACES: [&str; 3] = ["auto", "work", "glance"];
const NET_UNITS: [&str; 2] = ["bytes/s", "bits/s"];
/// Every auto / on / off row ([`Switch`]) cycles these labels.
const SWITCH: [&str; 3] = ["auto", "on", "off"];
/// Every `ProcSort` label, in its cycle order (a test keeps them in step).
const SORTS: [&str; 14] = [
    "cpu", "mem", "pid", "name", "user", "state", "gpu", "rss", "threads", "power", "read",
    "write", "wakeups", "started",
];
const CPU_UNITS: [&str; 2] = ["core", "total"];
/// Keymap profiles; the binary's `Profile::by_name` reads these names.
pub const KEY_PROFILES: [&str; 3] = ["siltop", "htop", "btop"];

/// Read on first use, after the binary installed the user's theme files.
static THEMES: LazyLock<Vec<&'static str>> = LazyLock::new(Theme::catalog);
static ICONS: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| std::iter::once("auto").chain(Icons::NAMES).collect());

/// `custom` first, then every preset in menu order (`1`–`5`).
const PRESETS: [&str; 6] = ["custom", "silicon", "ml", "laptop", "sre", "ricer"];

fn presets() -> &'static [&'static str] {
    &PRESETS
}

/// The preset row's value: the preset the config matches, else `custom`.
fn preset_get(c: &Config) -> Value {
    choice(&PRESETS, siltop_core::preset_label(Preset::matching(c)))
}

/// The boxes, layout and sort a fresh config has: what the preset row
/// calls `default` rather than `custom`.
fn default_look(c: &Config) -> bool {
    let d = Config::default();
    (
        c.show_gpu,
        c.show_power,
        c.show_net,
        c.show_disk,
        c.show_fans,
        c.show_cores,
        c.surface,
        c.proc_sort,
        c.zen,
    ) == (
        d.show_gpu,
        d.show_power,
        d.show_net,
        d.show_disk,
        d.show_fans,
        d.show_cores,
        d.surface,
        d.proc_sort,
        d.zen,
    )
}

/// Picking a preset applies it. `custom` cannot be applied, so a step that
/// lands on it from either end wraps on to the preset past it: next from
/// ricer is silicon, back from silicon is ricer.
fn preset_set(c: &mut Config, v: Value) {
    let target = match pick(&PRESETS, v).and_then(Preset::parse) {
        Some(p) => Some(p),
        None => match Preset::matching(c) {
            Some(Preset::Ricer) => Some(Preset::Silicon),
            Some(Preset::Silicon) => Some(Preset::Ricer),
            _ => None,
        },
    };
    if let Some(p) = target {
        p.apply(c);
    }
}

fn surfaces() -> &'static [&'static str] {
    &SURFACES
}
fn net_units() -> &'static [&'static str] {
    &NET_UNITS
}
fn switch() -> &'static [&'static str] {
    &SWITCH
}
fn sorts() -> &'static [&'static str] {
    &SORTS
}
fn cpu_units() -> &'static [&'static str] {
    &CPU_UNITS
}
fn key_profiles() -> &'static [&'static str] {
    &KEY_PROFILES
}
/// `auto`, the core built-ins, the theme pack, then the user's themes.
#[must_use]
pub fn themes() -> &'static [&'static str] {
    THEMES.as_slice()
}
fn icons() -> &'static [&'static str] {
    &ICONS
}

/// A plain on/off row over one `Config` bool.
macro_rules! toggle {
    ($id:literal, $cat:ident, $label:literal, $desc:literal, $field:ident) => {
        SettingDef {
            id: $id,
            category: Category::$cat,
            label: $label,
            description: $desc,
            kind: Kind::Toggle,
            get: |c| Value::Bool(c.$field),
            set: |c, v| {
                if let Some(on) = bool_of(v) {
                    c.$field = on;
                }
            },
        }
    };
}

/// An on/off row for one built-in alert rule; the `[[alerts]]` table it
/// writes keeps every other key the user gave that rule.
macro_rules! alert_rule {
    ($rule:literal, $label:literal, $desc:literal) => {
        SettingDef {
            id: concat!("alerts.", $rule),
            category: Category::Alerts,
            label: $label,
            description: concat!(
                $desc,
                " Alerts run while siltop is open; siltop agent install keeps them running in the background."
            ),
            kind: Kind::Toggle,
            get: |c| Value::Bool(rule_enabled(&c.alerts, $rule)),
            set: |c, v| {
                if let Some(on) = bool_of(v) {
                    set_rule_enabled(&mut c.alerts, $rule, on);
                }
            },
        }
    };
}

/// The whole settings screen, in display order within each category.
pub static SETTINGS: &[SettingDef] = &[
    SettingDef {
        id: "interval_ms",
        category: Category::General,
        label: "sample every",
        description: "How often siltop samples. Faster is smoother; slower uses less power.",
        kind: Kind::Interval,
        get: |c| Value::Millis(u64::try_from(c.interval.as_millis()).unwrap_or(u64::MAX)),
        set: |c, v| {
            if let Value::Millis(ms) = v {
                c.interval = Duration::from_millis(ms);
            }
        },
    },
    SettingDef {
        id: "surface",
        category: Category::General,
        label: "layout",
        description: "work shows every box with the process list; glance is one compact column; auto picks by window size.",
        kind: Kind::Cycle(surfaces),
        get: |c| choice(&SURFACES, surface_label(c.surface)),
        set: |c, v| {
            if let Some(surface) = pick(&SURFACES, v).and_then(parse_surface) {
                c.surface = surface;
            }
        },
    },
    SettingDef {
        id: "units.net_bits",
        category: Category::General,
        label: "network units",
        description: "Show network speed in bytes per second (like Finder) or bits per second (like your ISP).",
        kind: Kind::Cycle(net_units),
        get: |c| Value::Choice(usize::from(c.net_bits)),
        set: |c, v| {
            if let Value::Choice(at) = v {
                c.net_bits = at == 1;
            }
        },
    },
    SettingDef {
        id: "low_power",
        category: Category::General,
        label: "battery saver",
        description: "auto samples less often on battery; on always does; off never does.",
        kind: Kind::Cycle(switch),
        get: |c| choice(&SWITCH, c.low_power.label()),
        set: |c, v| {
            if let Some(mode) = pick(&SWITCH, v).and_then(Switch::parse) {
                c.low_power = mode;
            }
        },
    },
    SettingDef {
        id: "preset",
        category: Category::General,
        label: "preset",
        description: "A ready-made look: which boxes show, the layout and the sort. Keys 1 to 5 pick one anywhere.",
        kind: Kind::Cycle(presets),
        get: preset_get,
        set: preset_set,
    },
    SettingDef {
        id: "theme",
        category: Category::Appearance,
        label: "theme",
        description: "Colour palette. auto follows your terminal's background, light or dark. T cycles it anywhere.",
        kind: Kind::Cycle(themes),
        get: |c| choice(themes(), &c.theme),
        set: |c, v| {
            if let Some(name) = pick(themes(), v) {
                name.clone_into(&mut c.theme);
            }
        },
    },
    SettingDef {
        id: "icons",
        category: Category::Appearance,
        label: "icons",
        description: "Glyphs beside each box title. nerd needs a Nerd Font; none keeps it plain ASCII.",
        kind: Kind::Cycle(icons),
        get: |c| choice(&ICONS, &c.icons),
        set: |c, v| {
            if let Some(name) = pick(icons(), v) {
                name.clone_into(&mut c.icons);
            }
        },
    },
    SettingDef {
        id: "glass",
        category: Category::Appearance,
        label: "glass",
        description: "For see-through terminal windows: no cell backgrounds, brighter frames. auto reads your terminal's opacity setting.",
        kind: Kind::Cycle(switch),
        get: |c| choice(&SWITCH, c.glass.label()),
        set: |c, v| {
            if let Some(mode) = pick(&SWITCH, v).and_then(Switch::parse) {
                c.glass = mode;
            }
        },
    },
    toggle!(
        "extra_contrast",
        Appearance,
        "extra contrast",
        "Brighter dim text, hints and box frames. Helps over a bright or busy wallpaper.",
        extra_contrast
    ),
    toggle!(
        "panels.gpu",
        Panels,
        "gpu box",
        "Graphics load, clock, power and temperature. Hidden anyway on Macs that report no GPU.",
        show_gpu
    ),
    toggle!(
        "panels.net",
        Panels,
        "network box",
        "Download and upload speed on the active connection.",
        show_net
    ),
    toggle!(
        "panels.disk",
        Panels,
        "disk box",
        "Disk read and write speed plus how full each volume is.",
        show_disk
    ),
    toggle!(
        "panels.fans",
        Panels,
        "heat box",
        "Fans and temperatures. Fanless Macs show temperatures only.",
        show_fans
    ),
    toggle!(
        "panels.power",
        Panels,
        "power box",
        "Whole-Mac watts, the battery and what keeps the Mac awake. Hidden on desktops that report no power.",
        show_power
    ),
    toggle!(
        "panels.cores",
        Panels,
        "per-core bars",
        "One bar per CPU core inside the cpu box, grouped by cluster.",
        show_cores
    ),
    toggle!(
        "zen",
        Appearance,
        "zen mode",
        "Graphs only: hides the footer and the scale labels. Z toggles it anywhere.",
        zen
    ),
    SettingDef {
        id: "proc.sort",
        category: Category::Processes,
        label: "sort by",
        description: "Which column orders the process list. Clicking a column header does the same.",
        kind: Kind::Cycle(sorts),
        get: |c| choice(&SORTS, c.proc_sort.label()),
        set: |c, v| {
            if let Some(sort) = pick(&SORTS, v).and_then(ProcSort::parse) {
                c.proc_sort = sort;
            }
        },
    },
    toggle!(
        "proc.sort.desc",
        Processes,
        "biggest first",
        "Each column's natural order: numbers highest first, names A to Z. Off reverses it.",
        sort_desc
    ),
    toggle!(
        "proc.tree",
        Processes,
        "tree view",
        "Nest each process under the one that started it.",
        show_tree
    ),
    toggle!(
        "proc.threads",
        Processes,
        "threads column",
        "Add a column with each process's thread count.",
        show_threads
    ),
    SettingDef {
        id: "proc.cpu_unit",
        category: Category::Processes,
        label: "cpu percent",
        description: "core: 100% is one full core, like Activity Monitor. total: 100% is the whole chip.",
        kind: Kind::Cycle(cpu_units),
        get: |c| choice(&CPU_UNITS, c.cpu_unit.label()),
        set: |c, v| {
            if let Some(unit) = pick(&CPU_UNITS, v).and_then(CpuUnit::parse) {
                c.cpu_unit = unit;
            }
        },
    },
    SettingDef {
        id: "proc.ratio",
        category: Category::Processes,
        label: "list width",
        description: "How much of the work screen the process list takes. You can also drag the divider.",
        kind: Kind::Number {
            min: PROC_RATIO_MIN,
            max: PROC_RATIO_MAX,
            step: 5,
            unit: "%",
        },
        get: |c| Value::Number(c.proc_ratio),
        set: |c, v| {
            if let Value::Number(n) = v {
                c.proc_ratio = n.clamp(PROC_RATIO_MIN, PROC_RATIO_MAX);
            }
        },
    },
    alert_rule!(
        "hot",
        "running hot",
        "Notify when thermal pressure stays serious or worse for 10 s: the Mac is slowing itself down."
    ),
    alert_rule!(
        "memory",
        "memory tight",
        "Notify when memory pressure stays at warn or worse for 30 s."
    ),
    alert_rule!(
        "runaway",
        "runaway process",
        "Notify when one process uses more than 1.5 cores for a minute, and name it."
    ),
    alert_rule!(
        "drain",
        "battery drain",
        "Notify when the battery drains faster than 15 W for a minute. Never fires on a desktop."
    ),
    alert_rule!(
        "disk",
        "disk nearly full",
        "Notify when the startup disk has less than 10 GB free."
    ),
    alert_rule!(
        "busy",
        "cpu busy",
        "Notify when the whole CPU stays above 75 % for two minutes."
    ),
    alert_rule!(
        "swap",
        "large swap",
        "Notify when more than 4 GB is swapped out for a minute."
    ),
    alert_rule!(
        "fans",
        "loud fans",
        "Notify when a fan runs above 5000 rpm for two minutes. Never fires on a fanless Mac."
    ),
    SettingDef {
        id: "keymap",
        category: Category::Keys,
        label: "key profile",
        description: "Key layout. htop and btop profiles match those tools' muscle memory; the list below follows it.",
        kind: Kind::Cycle(key_profiles),
        get: |c| {
            let name = if c.keymap.trim().eq_ignore_ascii_case("default") {
                KEY_PROFILES[0]
            } else {
                c.keymap.as_str()
            };
            choice(&KEY_PROFILES, name)
        },
        set: |c, v| {
            if let Some(name) = pick(&KEY_PROFILES, v) {
                name.clone_into(&mut c.keymap);
            }
        },
    },
    SettingDef {
        id: "updates_check",
        category: Category::Privacy,
        label: "check updates",
        description: "Once a day, ask GitHub for the latest release number. Nothing about you or this Mac is sent.",
        kind: Kind::Toggle,
        get: |c| Value::Bool(c.updates_check),
        set: |c, v| {
            if let Some(on) = bool_of(v) {
                c.updates_check = on;
            }
        },
    },
    SettingDef {
        id: "updates_auto_install",
        category: Category::Privacy,
        label: "auto install",
        description: "After you choose this once, a curl install downloads the next version in the background. A Homebrew install never replaces itself.",
        kind: Kind::Toggle,
        get: |c| Value::Bool(c.updates_auto_install),
        set: |c, v| {
            if let Some(on) = bool_of(v) {
                c.updates_auto_install = on;
            }
        },
    },
];

/// The rows on screen: the category's rows, or every row matching `filter`.
pub(crate) fn visible(
    category: Category,
    filter: &str,
) -> impl Iterator<Item = &'static SettingDef> {
    let searching = !filter.trim().is_empty();
    SETTINGS.iter().filter(move |def| {
        if searching {
            def.matches(filter)
        } else {
            def.category == category
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn sort_choices_list_every_sort_key_in_cycle_order() {
        let labels: Vec<&str> = ProcSort::ALL.iter().map(|s| s.label()).collect();
        assert_eq!(SORTS.as_slice(), labels.as_slice());
    }

    #[test]
    fn ids_are_unique() {
        let mut seen = HashSet::new();
        for def in SETTINGS {
            assert!(seen.insert(def.id), "{} twice", def.id);
        }
    }

    fn def(id: &str) -> &'static SettingDef {
        SETTINGS.iter().find(|d| d.id == id).unwrap()
    }

    #[test]
    fn glass_row_cycles_auto_on_off_and_back() {
        let glass = def("glass");
        assert_eq!(glass.category, Category::Appearance);
        let mut cfg = Config::default();
        let mut shown = vec![glass.display(&cfg).into_owned()];
        for _ in 0..3 {
            glass.step(&mut cfg, Step::Next);
            shown.push(glass.display(&cfg).into_owned());
        }
        assert_eq!(shown, ["auto", "on", "off", "auto"]);
        glass.step(&mut cfg, Step::Prev);
        assert_eq!(cfg.glass, Switch::Off);
    }

    #[test]
    fn extra_contrast_row_toggles_the_config_bool() {
        let row = def("extra_contrast");
        assert_eq!(row.category, Category::Appearance);
        let mut cfg = Config::default();
        assert_eq!(row.display(&cfg), "off");
        row.step(&mut cfg, Step::Next);
        assert!(cfg.extra_contrast);
        assert!(row.matches("wallpaper"), "findable by what it is for");
    }

    #[test]
    fn theme_row_cycles_through_auto() {
        let theme = def("theme");
        let mut cfg = Config::default();
        let last = *themes().last().unwrap();
        last.clone_into(&mut cfg.theme);
        theme.step(&mut cfg, Step::Next);
        assert_eq!(cfg.theme, "auto");
        theme.step(&mut cfg, Step::Prev);
        assert_eq!(cfg.theme, last);
        // The core ten run straight on into the pack.
        "gruvbox-dark".clone_into(&mut cfg.theme);
        theme.step(&mut cfg, Step::Next);
        assert_eq!(cfg.theme, "dracula");
    }

    #[test]
    fn every_setting_has_label_and_description() {
        for def in SETTINGS {
            assert!(!def.label.is_empty(), "{}", def.id);
            assert!(
                def.description.len() > 20,
                "{} needs a real sentence",
                def.id
            );
            assert!(
                def.label.len() <= 16,
                "{} label too long for the column",
                def.id
            );
        }
    }

    #[test]
    fn every_setting_round_trips_through_toml() {
        for def in SETTINGS {
            let mut cfg = Config::default();
            def.step(&mut cfg, Step::Next);
            let text = cfg.to_toml().unwrap();
            let loaded = Config::from_toml(&text).unwrap().config;
            assert_eq!((def.get)(&loaded), (def.get)(&cfg), "{}", def.id);
        }
    }

    #[test]
    fn next_changes_every_setting() {
        for def in SETTINGS {
            let mut cfg = Config::default();
            let before = (def.get)(&cfg);
            def.step(&mut cfg, Step::Next);
            assert_ne!((def.get)(&cfg), before, "{} did not move", def.id);
        }
    }

    /// Applying a preset overwrites several fields at once, so it is the one
    /// row that cannot step back to `custom`; its own tests cover it.
    const IRREVERSIBLE: [&str; 1] = ["preset"];

    #[test]
    fn next_then_prev_is_identity() {
        for def in SETTINGS.iter().filter(|d| !IRREVERSIBLE.contains(&d.id)) {
            let mut cfg = Config::default();
            let before = cfg.clone();
            def.step(&mut cfg, Step::Next);
            def.step(&mut cfg, Step::Prev);
            assert_eq!((def.get)(&cfg), (def.get)(&before), "{}", def.id);
        }
    }

    #[test]
    fn cycles_visit_every_option_and_wrap() {
        for def in SETTINGS.iter().filter(|d| !IRREVERSIBLE.contains(&d.id)) {
            let Kind::Cycle(options) = def.kind else {
                continue;
            };
            let mut cfg = Config::default();
            let mut seen = HashSet::new();
            for _ in 0..options().len() {
                seen.insert(def.display(&cfg).into_owned());
                def.step(&mut cfg, Step::Next);
            }
            assert_eq!(seen.len(), options().len(), "{}", def.id);
            assert_eq!(
                (def.get)(&cfg),
                (def.get)(&Config::default()),
                "{} wraps",
                def.id
            );
        }
    }

    #[test]
    fn numbers_clamp_at_both_ends() {
        let def = SETTINGS.iter().find(|d| d.id == "proc.ratio").unwrap();
        let mut cfg = Config::default();
        for _ in 0..40 {
            def.step(&mut cfg, Step::Next);
        }
        assert_eq!(cfg.proc_ratio, PROC_RATIO_MAX);
        for _ in 0..40 {
            def.step(&mut cfg, Step::Prev);
        }
        assert_eq!(cfg.proc_ratio, PROC_RATIO_MIN);
    }

    #[test]
    fn display_matches_the_config_default() {
        let cfg = Config::default();
        let shown = |id: &str| {
            SETTINGS
                .iter()
                .find(|d| d.id == id)
                .unwrap()
                .display(&cfg)
                .into_owned()
        };
        assert_eq!(shown("interval_ms"), "500 ms");
        assert_eq!(shown("surface"), "auto");
        assert_eq!(shown("theme"), "auto");
        assert_eq!(shown("keymap"), "siltop", "config says `default`");
        assert_eq!(shown("panels.disk"), "off");
        assert_eq!(shown("proc.ratio"), "55%");
    }

    /// Regression: `-i 750ms` read as `500 ms` here (an unknown interval
    /// fell back to the second preset), and ← at 250 ms wrapped to 5 s
    /// while `[` clamps.
    #[test]
    fn interval_row_shows_any_value_and_clamps_like_the_keys() {
        let def = SETTINGS.iter().find(|d| d.id == "interval_ms").unwrap();
        let mut cfg = Config::default().with_interval(Duration::from_millis(750));
        assert_eq!(def.display(&cfg), "750 ms");
        def.step(&mut cfg, Step::Prev);
        assert_eq!(def.display(&cfg), "500 ms");
        def.step(&mut cfg, Step::Prev);
        def.step(&mut cfg, Step::Prev);
        assert_eq!(def.display(&cfg), "250 ms", "clamped, no wrap to 5 s");
        let mut cfg = Config::default().with_interval(Duration::from_millis(750));
        def.step(&mut cfg, Step::Next);
        assert_eq!(def.display(&cfg), "1 s");
        for _ in 0..5 {
            def.step(&mut cfg, Step::Next);
        }
        assert_eq!(def.display(&cfg), "5 s", "clamped at the slow end");
    }

    #[test]
    fn theme_options_are_auto_then_every_builtin() {
        let options = themes();
        assert_eq!(options[0], "auto");
        assert_eq!(&options[..11], Theme::names());
        assert_eq!(&options[11..18], Theme::pack());
        for name in Theme::builtins() {
            assert!(Theme::by_name(name).is_some(), "{name}");
        }
    }

    #[test]
    fn every_category_but_about_has_rows() {
        for category in Category::ALL {
            let rows = visible(category, "").count();
            assert_eq!(rows == 0, category == Category::About, "{category:?}");
        }
    }

    #[test]
    fn alert_rows_cover_every_builtin_rule_and_toggle_it() {
        let ids: Vec<&str> = visible(Category::Alerts, "").map(|d| d.id).collect();
        let builtins: Vec<String> = siltop_core::DEFAULT_RULES
            .iter()
            .map(|b| format!("alerts.{}", b.id))
            .collect();
        assert_eq!(ids, builtins);
        let mut config = Config::default();
        for def in visible(Category::Alerts, "") {
            let before = (def.get)(&config);
            def.step(&mut config, Step::Next);
            assert_ne!((def.get)(&config), before, "{}", def.id);
            def.step(&mut config, Step::Next);
            assert_eq!((def.get)(&config), before, "{}", def.id);
        }
        assert!(config.alerts.is_empty(), "toggling back leaves no tables");
    }

    #[test]
    fn filter_searches_across_categories() {
        let hits: Vec<&str> = visible(Category::About, "box").map(|d| d.id).collect();
        let panels = hits.iter().filter(|id| id.starts_with("panels.")).count();
        assert_eq!(panels, visible(Category::Panels, "").count(), "{hits:?}");
        assert_eq!(
            visible(Category::General, "THEME").next().map(|d| d.id),
            Some("theme")
        );
        assert_eq!(visible(Category::General, "zzzz").count(), 0);
    }

    #[test]
    fn category_step_wraps_both_ways() {
        assert_eq!(Category::General.step(-1), Category::About);
        assert_eq!(Category::About.step(1), Category::General);
        assert_eq!(Category::General.step(2), Category::Panels);
    }

    fn preset_row() -> &'static SettingDef {
        SETTINGS.iter().find(|d| d.id == "preset").unwrap()
    }

    #[test]
    fn preset_row_reads_default_for_a_fresh_config_and_custom_once_changed() {
        assert_eq!(preset_row().display(&Config::default()), "default");
        let changed = Config {
            show_disk: !Config::default().show_disk,
            ..Config::default()
        };
        assert_ne!(preset_row().display(&changed), "default");
    }

    #[test]
    fn preset_row_walks_every_preset_and_wraps_past_custom() {
        let def = preset_row();
        let mut cfg = Config::default();
        let mut seen = Vec::new();
        for _ in 0..=Preset::ALL.len() {
            def.step(&mut cfg, Step::Next);
            seen.push(def.display(&cfg).into_owned());
        }
        assert_eq!(seen, ["silicon", "ml", "laptop", "sre", "ricer", "silicon"]);
    }

    #[test]
    fn preset_row_steps_back_from_silicon_to_ricer() {
        let def = preset_row();
        let mut cfg = Config::default();
        Preset::Silicon.apply(&mut cfg);
        def.step(&mut cfg, Step::Prev);
        assert_eq!(def.display(&cfg), "ricer");
        let mut custom = Config::default();
        def.step(&mut custom, Step::Prev);
        assert_eq!(def.display(&custom), "ricer");
    }

    #[test]
    fn preset_row_applies_the_whole_spec() {
        let def = preset_row();
        let mut cfg = Config::default();
        (def.set)(&mut cfg, Value::Choice(4));
        assert_eq!(Preset::matching(&cfg), Some(Preset::Sre));
        assert!(cfg.show_disk && cfg.show_net && !cfg.show_gpu);
    }

    #[test]
    fn zen_row_flips_the_config_flag() {
        let def = SETTINGS.iter().find(|d| d.id == "zen").unwrap();
        let mut cfg = Config::default();
        def.step(&mut cfg, Step::Next);
        assert!(cfg.zen);
    }

    /// Regression: the settings table spelled intervals by hand, beside
    /// two copies of the formatter the footer and the toast used.
    #[test]
    fn interval_labels_match_the_one_formatter() {
        let labels = ["250 ms", "500 ms", "1 s", "2 s", "5 s"];
        for (ms, label) in siltop_core::INTERVALS.iter().zip(labels) {
            let ms = u64::try_from(ms.as_millis()).unwrap();
            assert_eq!(crate::widgets::interval_label(ms), label);
        }
    }
}
