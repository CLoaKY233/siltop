//! Reading and editing the config document. One table maps every setting
//! to its v1 path and to the flat key the pre-v1 writer used, so old files
//! load without loss and a save only touches the keys that changed.

use toml_edit::{DocumentMut, InlineTable, Item, Table, TableLike, Value};

use super::v1::{self, CpuUnit, Switch, parse_interval_ms, parse_surface, surface_label};
use super::{Config, PROC_RATIO_MAX, PROC_RATIO_MIN, ProcSort};

/// One setting's value in file form.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Scalar {
    Bool(bool),
    Int(i64),
    Str(String),
}

impl Scalar {
    fn from_item(item: &Item) -> Option<Self> {
        let value = item.as_value()?;
        if let Some(b) = value.as_bool() {
            return Some(Self::Bool(b));
        }
        if let Some(i) = value.as_integer() {
            return Some(Self::Int(i));
        }
        value.as_str().map(|s| Self::Str(s.to_owned()))
    }

    fn to_value(&self) -> Value {
        match self {
            Self::Bool(b) => Value::from(*b),
            Self::Int(i) => Value::from(*i),
            Self::Str(s) => Value::from(s.as_str()),
        }
    }

    fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            Self::Str(s) => s.trim().parse().ok(),
            Self::Int(_) => None,
        }
    }

    fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            Self::Str(s) => s.trim().parse().ok(),
            Self::Bool(_) => None,
        }
    }

    fn as_text(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }
}

struct Field {
    path: &'static [&'static str],
    /// The flat key written before v1, when it differs from `path`.
    legacy: Option<&'static str>,
    get: fn(&Config) -> Scalar,
    /// Applies a file value; `false` means it was rejected.
    set: fn(&mut Config, &Scalar) -> bool,
}

fn set_bool(slot: &mut bool, value: &Scalar) -> bool {
    value.as_bool().map(|b| *slot = b).is_some()
}

fn set_text(slot: &mut String, value: &Scalar) -> bool {
    match value.as_text().map(str::trim) {
        Some(text) if !text.is_empty() => {
            text.clone_into(slot);
            true
        }
        _ => false,
    }
}

macro_rules! bool_field {
    ($path:expr, $legacy:expr, $field:ident) => {
        Field {
            path: $path,
            legacy: $legacy,
            get: |c| Scalar::Bool(c.$field),
            set: |c, v| set_bool(&mut c.$field, v),
        }
    };
}

macro_rules! text_field {
    ($path:expr, $field:ident) => {
        Field {
            path: $path,
            legacy: None,
            get: |c| Scalar::Str(c.$field.clone()),
            set: |c, v| set_text(&mut c.$field, v),
        }
    };
}

/// An auto / on / off key; a bare `true` or `false` reads as on or off.
fn set_switch(slot: &mut Switch, value: &Scalar) -> bool {
    let parsed = match value {
        Scalar::Bool(true) => Some(Switch::On),
        Scalar::Bool(false) => Some(Switch::Off),
        other => other.as_text().and_then(Switch::parse),
    };
    parsed.map(|switch| *slot = switch).is_some()
}

macro_rules! switch_field {
    ($path:expr, $field:ident) => {
        Field {
            path: $path,
            legacy: None,
            get: |c| Scalar::Str(c.$field.label().to_owned()),
            set: |c, v| set_switch(&mut c.$field, v),
        }
    };
}

const SORT_DESC: &[&str] = &["proc", "sort", "desc"];

const FIELDS: &[Field] = &[
    Field {
        path: &["interval_ms"],
        legacy: None,
        get: |c| Scalar::Int(i64::try_from(c.interval.as_millis()).unwrap_or(500)),
        set: |c, v| {
            v.as_int()
                .and_then(parse_interval_ms)
                .map(|interval| c.interval = interval)
                .is_some()
        },
    },
    Field {
        path: &["surface"],
        legacy: None,
        get: |c| Scalar::Str(surface_label(c.surface).to_owned()),
        set: |c, v| {
            v.as_text()
                .and_then(parse_surface)
                .map(|surface| c.surface = surface)
                .is_some()
        },
    },
    text_field!(&["theme"], theme),
    text_field!(&["icons"], icons),
    text_field!(&["keymap"], keymap),
    switch_field!(&["low_power"], low_power),
    switch_field!(&["glass"], glass),
    bool_field!(&["extra_contrast"], None, extra_contrast),
    bool_field!(&["panels", "gpu"], Some("show_gpu"), show_gpu),
    bool_field!(&["panels", "net"], Some("show_net"), show_net),
    bool_field!(&["panels", "disk"], Some("show_disk"), show_disk),
    bool_field!(&["panels", "fans"], Some("show_fans"), show_fans),
    bool_field!(&["panels", "power"], None, show_power),
    bool_field!(&["panels", "cores"], Some("show_cores"), show_cores),
    Field {
        path: &["proc", "sort", "column"],
        legacy: Some("sort"),
        get: |c| Scalar::Str(c.proc_sort.label().to_owned()),
        set: |c, v| {
            v.as_text()
                .and_then(ProcSort::parse)
                .map(|sort| c.proc_sort = sort)
                .is_some()
        },
    },
    bool_field!(SORT_DESC, None, sort_desc),
    bool_field!(&["proc", "tree"], Some("show_tree"), show_tree),
    bool_field!(&["proc", "threads"], Some("show_threads"), show_threads),
    Field {
        path: &["proc", "ratio"],
        legacy: Some("proc_ratio"),
        get: |c| Scalar::Int(i64::from(c.proc_ratio)),
        set: |c, v| {
            v.as_int()
                .map(|r| {
                    let clamped = r.clamp(i64::from(PROC_RATIO_MIN), i64::from(PROC_RATIO_MAX));
                    c.proc_ratio = u16::try_from(clamped).unwrap_or(c.proc_ratio);
                })
                .is_some()
        },
    },
    Field {
        path: &["proc", "cpu_unit"],
        legacy: None,
        get: |c| Scalar::Str(c.cpu_unit.label().to_owned()),
        set: |c, v| {
            v.as_text()
                .and_then(CpuUnit::parse)
                .map(|unit| c.cpu_unit = unit)
                .is_some()
        },
    },
    bool_field!(&["units", "net_bits"], None, net_bits),
    bool_field!(&["zen"], None, zen),
    // Empty is a real value: onboarding has not been finished.
    Field {
        path: &["onboarding", "completed"],
        legacy: None,
        get: |c| Scalar::Str(c.onboarding_completed.clone()),
        set: |c, v| {
            v.as_text()
                .map(|text| text.trim().clone_into(&mut c.onboarding_completed))
                .is_some()
        },
    },
    bool_field!(&["updates", "check"], None, updates_check),
    bool_field!(&["updates", "auto_install"], None, updates_auto_install),
];

/// A TOML syntax error: the file is left alone and never saved over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

/// A parsed file: the effective config, the editable document it came
/// from, and one line per value that was ignored.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    pub doc: DocumentMut,
    pub warnings: Vec<String>,
}

fn lookup<'a>(doc: &'a DocumentMut, path: &[&str]) -> Option<&'a Item> {
    let (first, rest) = path.split_first()?;
    let mut item = doc.get(first)?;
    for key in rest {
        item = item.get(key)?;
    }
    (!item.is_none()).then_some(item)
}

/// Keys the loader reads at the top level besides [`FIELDS`]: the format
/// version and the `[[alerts]]` tables, which check themselves.
const OTHER_TOP_KEYS: &[&str] = &["version", "alerts"];

/// Keys older builds wrote that no longer mean anything. They are read
/// past without a warning and left in the file as they are.
const RETIRED: &[&[&str]] = &[&["onboarding", "eula"]];

/// Whether `path` is a setting, the table holding one, a retired key or a
/// pre-v1 key.
fn known(path: &[&str]) -> Option<bool> {
    if let [only] = path
        && (OTHER_TOP_KEYS.contains(only) || FIELDS.iter().any(|f| f.legacy == Some(*only)))
    {
        return Some(true);
    }
    if RETIRED.contains(&path) {
        return Some(true);
    }
    FIELDS
        .iter()
        .filter(|field| field.path.starts_with(path))
        .map(|field| field.path.len() == path.len())
        .reduce(|leaf, other| leaf || other)
}

/// One "ignored unknown ..." line per key or table the loader has no use
/// for, so a typo such as `[ui] theme = 5` is named instead of silently
/// doing nothing. Known leaves are checked for type by [`FIELDS`].
fn unknown_keys(table: &dyn TableLike, prefix: &[&str], warnings: &mut Vec<String>) {
    for (key, item) in table.iter() {
        let mut path = prefix.to_vec();
        path.push(key);
        let name = path.join(".");
        match known(&path) {
            Some(true) => {}
            Some(false) => match item.as_table_like() {
                Some(inner) => unknown_keys(inner, &path, warnings),
                None => warnings.push(format!("ignored {name} = {}", item.to_string().trim())),
            },
            None if item.is_table_like() => {
                warnings.push(format!("ignored unknown table [{name}]"));
            }
            None => warnings.push(format!("ignored unknown key {name}")),
        }
    }
}

impl Config {
    /// Parses a v1 or pre-v1 file. The v1 path wins when both spellings of
    /// a setting are present. Wrong types and out-of-range values fall back
    /// per setting and are reported in `warnings`.
    pub fn from_toml(text: &str) -> Result<Loaded, ParseError> {
        let doc: DocumentMut = text
            .parse()
            .map_err(|err: toml_edit::TomlError| ParseError(err.message().to_owned()))?;
        let (config, warnings) = Self::from_document(&doc);
        Ok(Loaded {
            config,
            doc,
            warnings,
        })
    }

    #[must_use]
    pub fn from_document(doc: &DocumentMut) -> (Self, Vec<String>) {
        let mut config = Self::default();
        let mut warnings = Vec::new();
        for field in FIELDS {
            let found = lookup(doc, field.path)
                .map(|item| (field.path.join("."), item))
                .or_else(|| {
                    field
                        .legacy
                        .and_then(|key| doc.get(key).map(|item| (key.to_owned(), item)))
                });
            let Some((name, item)) = found else {
                continue;
            };
            let applied = Scalar::from_item(item).is_some_and(|v| (field.set)(&mut config, &v));
            if !applied {
                warnings.push(format!("ignored {name} = {}", item.to_string().trim()));
            }
        }
        if lookup(doc, SORT_DESC).is_none() {
            // no saved direction: the column's natural one (names A to Z)
            config.sort_desc = config.proc_sort.natural_descending();
        }
        unknown_keys(doc.as_table(), &[], &mut warnings);
        let (alerts, alert_warnings) = crate::alerts::read_specs(doc);
        config.alerts = alerts;
        warnings.extend(alert_warnings);
        if let Some(version) = doc.get("version").and_then(Item::as_integer)
            && version > v1::VERSION
        {
            warnings.push(format!("config version {version} is newer than this build"));
        }
        (config, warnings)
    }

    /// Writes into `doc` only the settings where `self` differs from
    /// `base`, keeping comments, formatting and unknown keys. A pre-v1 flat
    /// key is replaced by its v1 path when that setting is written.
    /// Returns how many settings were written.
    pub fn write_changes(&self, base: &Config, doc: &mut DocumentMut) -> usize {
        let mut written = 0;
        for field in FIELDS {
            let new = (field.get)(self);
            // an unwritten direction reads back as the column's natural one,
            // so a new column carries its direction along
            let coupled = field.path == SORT_DESC && self.proc_sort != base.proc_sort;
            if (field.get)(base) == new && !coupled {
                continue;
            }
            if let Some(legacy) = field.legacy {
                doc.remove(legacy);
            }
            set_path(doc.as_table_mut(), field.path, &new, false);
            written += 1;
        }
        if self.alerts != base.alerts {
            crate::alerts::write_specs(doc, &self.alerts);
            written += 1;
        }
        if written > 0 && doc.get("version").is_none() {
            doc.insert("version", toml_edit::value(v1::VERSION));
        }
        written
    }
}

/// Sets `path` under `table`, creating implicit tables on the way (inline
/// ones inside an inline table) and keeping the old value's comments.
fn set_path(table: &mut dyn TableLike, path: &[&str], value: &Scalar, inline: bool) {
    let Some((key, rest)) = path.split_first() else {
        return;
    };
    if rest.is_empty() {
        let mut new = value.to_value();
        if let Some(old) = table.get(key).and_then(Item::as_value) {
            *new.decor_mut() = old.decor().clone();
        }
        table.insert(key, Item::Value(new));
        return;
    }
    if !table.get(key).is_some_and(Item::is_table_like) {
        let child = if inline {
            Item::Value(Value::InlineTable(InlineTable::new()))
        } else {
            let mut fresh = Table::new();
            fresh.set_implicit(true);
            Item::Table(fresh)
        };
        table.insert(key, child);
    }
    let child_inline = inline || table.get(key).is_some_and(Item::is_inline_table);
    if let Some(child) = table.get_mut(key).and_then(Item::as_table_like_mut) {
        set_path(child, rest, value, child_inline);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::config::{
        ConfigV1, Glass, INTERVAL_DEFAULT, INTERVAL_FAST, INTERVALS, LowPower, PROC_RATIO_DEFAULT,
    };
    use crate::surface::Surface;

    /// Pre-v1 files captured from the harness XDG directories on this
    /// machine (M5 Pro), byte for byte.
    const LEGACY_DEFAULT_250: &str = "interval_ms = 250\n# surface = work\nshow_gpu = true\nshow_net = true\nshow_disk = false\nshow_fans = true\nshow_cores = true\nshow_threads = false\nshow_tree = false\nsort = \"cpu\"\nproc_ratio = 55\n";
    const LEGACY_ML_DEV: &str = "interval_ms = 1000\nsurface = \"work\"\nshow_gpu = true\nshow_net = false\nshow_disk = true\nshow_fans = true\nshow_cores = true\nshow_threads = false\nshow_tree = false\nsort = \"cpu\"\nproc_ratio = 55\n";
    const LEGACY_NO_GPU: &str = "interval_ms = 500\n# surface = work\nshow_gpu = false\nshow_net = true\nshow_disk = false\nshow_fans = true\nshow_cores = true\nshow_threads = false\nshow_tree = false\nsort = \"cpu\"\nproc_ratio = 55\n";
    const LEGACY_PID: &str = "interval_ms = 250\n# surface = work\nshow_gpu = true\nshow_net = true\nshow_disk = false\nshow_fans = true\nshow_cores = true\nshow_threads = false\nshow_tree = false\nsort = \"pid\"\nproc_ratio = 55\n";
    const LEGACY_TREE_THREADS: &str = "interval_ms = 250\n# surface = work\nshow_gpu = true\nshow_net = true\nshow_disk = false\nshow_fans = true\nshow_cores = true\nshow_threads = true\nshow_tree = true\nsort = \"cpu\"\nproc_ratio = 55\n";
    const LEGACY_MEM_GLANCE: &str = "interval_ms = 1000\nsurface = \"glance\"\nshow_gpu = true\nshow_net = true\nshow_disk = true\nshow_fans = true\nshow_cores = true\nshow_threads = false\nshow_tree = false\nsort = \"mem\"\nproc_ratio = 55\n";
    const CORRUPT: &[u8] = include_bytes!("fixtures/corrupt.toml");

    /// Parses a file whose every known value is valid. Unknown keys are
    /// allowed: several fixtures carry one to prove it survives a save.
    fn parse(text: &str) -> Config {
        let loaded = Config::from_toml(text).unwrap();
        assert!(
            loaded.warnings.iter().all(|w| w.contains(" unknown ")),
            "{:?}",
            loaded.warnings
        );
        loaded.config
    }

    #[test]
    fn unknown_keys_and_tables_warn_once_each() {
        let text = "version = 1\nthem = \"nord\"\n[ui]\ntheme = 5\n[panels]\ngpu = true\ncpuu = false\n[proc.sort]\ncolumn = \"mem\"\n[[alerts]]\nid = \"x\"\n";
        let loaded = Config::from_toml(text).unwrap();
        let unknown: Vec<_> = loaded
            .warnings
            .iter()
            .filter(|w| w.contains(" unknown "))
            .collect();
        assert_eq!(
            unknown,
            [
                "ignored unknown key them",
                "ignored unknown table [ui]",
                "ignored unknown key panels.cpuu",
            ],
            "{:?}",
            loaded.warnings
        );
        assert_eq!(loaded.config.proc_sort, ProcSort::Mem);
        // A known table given a plain value is a wrong type, not unknown.
        let loaded = Config::from_toml("panels = 3\nshow_gpu = false\n").unwrap();
        assert_eq!(loaded.warnings, ["ignored panels = 3"]);
        assert!(!loaded.config.show_gpu);
    }

    /// Files from before 1.2 carry `[onboarding] eula`; it means nothing
    /// now, so it loads without a warning and a save leaves it alone.
    #[test]
    fn a_retired_key_loads_silently_and_survives_a_save() {
        let text = "version = 1\n[onboarding]\ncompleted = \"1.1.0\"\neula = \"1.1\"\n";
        let loaded = Config::from_toml(text).unwrap();
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert_eq!(loaded.config.onboarding_completed, "1.1.0");
        let mut doc = loaded.doc;
        let changed = Config {
            zen: true,
            ..loaded.config.clone()
        };
        changed.write_changes(&loaded.config, &mut doc);
        assert!(doc.to_string().contains("eula = \"1.1\""), "{doc}");
    }

    fn edit(text: &str, change: impl FnOnce(&mut Config)) -> String {
        let loaded = Config::from_toml(text).unwrap();
        let mut next = loaded.config.clone();
        change(&mut next);
        let mut doc = loaded.doc;
        next.write_changes(&loaded.config, &mut doc);
        doc.to_string()
    }

    /// The pre-v1 hand parser, kept here as the oracle for migration.
    fn legacy_oracle(text: &str) -> Config {
        let mut cfg = Config::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if key.trim_start().starts_with('#') {
                continue;
            }
            let value = value.split('#').next().unwrap().trim().trim_matches('"');
            match key.trim() {
                "interval_ms" => {
                    if let Some(i) = value.parse().ok().and_then(parse_interval_ms) {
                        cfg.interval = i;
                    }
                }
                "surface" => match value {
                    "work" => cfg.surface = Some(Surface::Work),
                    "glance" => cfg.surface = Some(Surface::Glance),
                    _ => {}
                },
                "show_gpu" => cfg.show_gpu = value.parse().unwrap(),
                "show_net" => cfg.show_net = value.parse().unwrap(),
                "show_disk" => cfg.show_disk = value.parse().unwrap(),
                "show_fans" => cfg.show_fans = value.parse().unwrap(),
                "show_cores" => cfg.show_cores = value.parse().unwrap(),
                "show_threads" => cfg.show_threads = value.parse().unwrap(),
                "show_tree" => cfg.show_tree = value.parse().unwrap(),
                "sort" => cfg.proc_sort = ProcSort::parse(value).unwrap(),
                "proc_ratio" => cfg.proc_ratio = value.parse().unwrap(),
                _ => {}
            }
        }
        // flat files never saved a direction: the column's natural one
        cfg.sort_desc = cfg.proc_sort.natural_descending();
        cfg
    }

    #[test]
    fn missing_sort_desc_reads_as_the_columns_natural_direction() {
        assert!(!parse("[proc.sort]\ncolumn = \"name\"\n").sort_desc);
        assert!(parse("[proc.sort]\ncolumn = \"mem\"\n").sort_desc);
        assert!(parse("[proc.sort]\ncolumn = \"pid\"\ndesc = true\n").sort_desc);
        assert!(
            parse("").sort_desc,
            "the default cpu column runs high first"
        );
    }

    #[test]
    fn migration_table_matches_the_old_parser_on_captured_files() {
        for (name, text) in [
            ("default_250", LEGACY_DEFAULT_250),
            ("ml_dev", LEGACY_ML_DEV),
            ("no_gpu", LEGACY_NO_GPU),
            ("pid", LEGACY_PID),
            ("tree_threads", LEGACY_TREE_THREADS),
            ("mem_glance", LEGACY_MEM_GLANCE),
        ] {
            assert_eq!(parse(text), legacy_oracle(text), "{name}");
        }
    }

    #[test]
    fn migration_table_expected_values() {
        let ml = parse(LEGACY_ML_DEV);
        assert_eq!(ml.interval, Duration::from_secs(1));
        assert_eq!(ml.surface, Some(Surface::Work));
        assert!(!ml.show_net && ml.show_disk);
        let glance = parse(LEGACY_MEM_GLANCE);
        assert_eq!(glance.surface, Some(Surface::Glance));
        assert_eq!(glance.proc_sort, ProcSort::Mem);
        let commented = parse(LEGACY_DEFAULT_250);
        assert_eq!(commented.surface, None, "a commented surface stays auto");
        assert_eq!(commented.interval, INTERVAL_FAST);
        let tree = parse(LEGACY_TREE_THREADS);
        assert!(tree.show_tree && tree.show_threads);
        assert_eq!(parse(LEGACY_PID).proc_sort, ProcSort::Pid);
        assert!(!parse(LEGACY_NO_GPU).show_gpu);
    }

    #[test]
    fn legacy_defaults_file_equals_default_config() {
        assert_eq!(
            parse(&LEGACY_DEFAULT_250.replace("= 250", "= 500")),
            Config::default()
        );
    }

    #[test]
    fn v1_path_wins_over_legacy_key() {
        let cfg = parse("show_gpu = true\n[panels]\ngpu = false\n");
        assert!(!cfg.show_gpu);
        let cfg = parse("proc_ratio = 40\n[proc]\nratio = 60\n");
        assert_eq!(cfg.proc_ratio, 60);
    }

    #[test]
    fn empty_and_comment_only_files_are_defaults() {
        for text in ["", "\n\n", "# nothing here\n", "   \n# a\n#b = 1\n"] {
            assert_eq!(parse(text), Config::default(), "{text:?}");
        }
    }

    #[test]
    fn corrupt_capture_is_a_parse_error() {
        let text = String::from_utf8_lossy(CORRUPT);
        let err = Config::from_toml(&text).unwrap_err();
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn syntax_errors_are_rejected_not_guessed() {
        for text in [
            "sort = cpu\n",
            "show_gpu = \n",
            "[panels\n",
            "interval_ms = 500\ninterval_ms = 250\n",
            "= 3\n",
        ] {
            assert!(Config::from_toml(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn wrong_types_fall_back_per_key_with_warnings() {
        let loaded = Config::from_toml(
            "show_fans = 3\ninterval_ms = \"fast\"\nsurface = 4\nshow_gpu = false\n",
        )
        .unwrap();
        assert!(loaded.config.show_fans);
        assert_eq!(loaded.config.interval, INTERVAL_DEFAULT);
        assert_eq!(loaded.config.surface, None);
        assert!(!loaded.config.show_gpu, "good keys still apply");
        assert_eq!(loaded.warnings.len(), 3, "{:?}", loaded.warnings);
        assert!(loaded.warnings.iter().any(|w| w.contains("show_fans")));
    }

    #[test]
    fn stringly_typed_values_are_accepted() {
        let cfg = parse("show_gpu = \"false\"\nproc_ratio = \"60\"\ninterval_ms = \"250\"\n");
        assert!(!cfg.show_gpu);
        assert_eq!(cfg.proc_ratio, 60);
        assert_eq!(cfg.interval, INTERVAL_FAST);
    }

    #[test]
    fn interval_sweep_only_accepts_selectable_cadences() {
        for ms in -5_i64..=6000 {
            let loaded = Config::from_toml(&format!("interval_ms = {ms}\n")).unwrap();
            let listed = ms >= 0 && INTERVALS.iter().any(|i| i.as_millis() == ms as u128);
            if listed {
                assert_eq!(loaded.config.interval.as_millis(), ms as u128);
                assert!(loaded.warnings.is_empty());
            } else {
                assert_eq!(loaded.config.interval, INTERVAL_DEFAULT, "{ms}");
                assert_eq!(loaded.warnings.len(), 1, "{ms}");
            }
        }
    }

    #[test]
    fn ratio_sweep_is_clamped_including_extremes() {
        for ratio in [i64::MIN, -1, 0, 34, 35, 55, 72, 73, 100, 65_536, i64::MAX] {
            let cfg = parse(&format!("proc_ratio = {ratio}\n"));
            assert!((PROC_RATIO_MIN..=PROC_RATIO_MAX).contains(&cfg.proc_ratio));
            if (35..=72).contains(&ratio) {
                assert_eq!(i64::from(cfg.proc_ratio), ratio);
            }
        }
    }

    #[test]
    fn enum_values_are_case_insensitive() {
        let cfg = parse(
            "surface = \"GLANCE\"\nlow_power = \"On\"\n[proc]\ncpu_unit = \"Total\"\n[proc.sort]\ncolumn = \"MEM\"\n",
        );
        assert_eq!(cfg.surface, Some(Surface::Glance));
        assert_eq!(cfg.low_power, LowPower::On);
        assert_eq!(cfg.cpu_unit, CpuUnit::Total);
        assert_eq!(cfg.proc_sort, ProcSort::Mem);
    }

    #[test]
    fn glass_and_extra_contrast_read_from_the_file() {
        let cfg = parse("glass = \"off\"\nextra_contrast = true\n");
        assert_eq!(cfg.glass, Glass::Off);
        assert!(cfg.extra_contrast);
        assert_eq!(parse("glass = true\n").glass, Glass::On);
        let fresh = Config::default();
        assert_eq!((fresh.glass, fresh.extra_contrast), (Glass::Auto, false));
    }

    #[test]
    fn glass_edit_writes_only_its_key() {
        let out = edit("# mine\ntheme = \"nord\"\n", |c| c.glass = Glass::Off);
        assert!(out.contains("glass = \"off\""), "{out}");
        assert!(out.contains("# mine") && out.contains("theme = \"nord\""));
        assert!(!out.contains("extra_contrast"), "{out}");
    }

    #[test]
    fn low_power_accepts_bools() {
        assert_eq!(parse("low_power = true\n").low_power, LowPower::On);
        assert_eq!(parse("low_power = false\n").low_power, LowPower::Off);
        assert_eq!(parse("low_power = \"auto\"\n").low_power, LowPower::Auto);
    }

    #[test]
    fn surface_auto_is_none() {
        assert_eq!(parse("surface = \"auto\"\n").surface, None);
        assert_eq!(Config::default().surface, None);
    }

    #[test]
    fn unicode_theme_names_survive_and_blank_is_rejected() {
        let cfg = parse("theme = \"ドラキュラ 🦇\"\n");
        assert_eq!(cfg.theme, "ドラキュラ 🦇");
        let loaded = Config::from_toml("theme = \"   \"\n").unwrap();
        assert_eq!(loaded.config.theme, "auto");
        assert_eq!(loaded.warnings.len(), 1);
    }

    #[test]
    fn inline_tables_are_read() {
        let cfg = parse("proc = { ratio = 44, sort = { column = \"pid\", desc = false } }\n");
        assert_eq!(cfg.proc_ratio, 44);
        assert_eq!(cfg.proc_sort, ProcSort::Pid);
        assert!(!cfg.sort_desc);
    }

    #[test]
    fn dotted_keys_are_read() {
        let cfg = parse("panels.disk = true\nunits.net_bits = true\n");
        assert!(cfg.show_disk && cfg.net_bits);
    }

    #[test]
    fn newer_version_warns_but_loads() {
        let loaded = Config::from_toml("version = 9\ntheme = \"nord\"\n").unwrap();
        assert_eq!(loaded.config.theme, "nord");
        assert_eq!(loaded.warnings.len(), 1);
    }

    #[test]
    fn full_toml_round_trips_every_field() {
        let cfg = Config {
            interval: Duration::from_secs(2),
            surface: Some(Surface::Glance),
            show_gpu: false,
            show_net: false,
            show_disk: true,
            show_fans: false,
            show_power: true,
            show_cores: false,
            show_threads: true,
            show_tree: true,
            proc_sort: ProcSort::Pid,
            proc_ratio: 70,
            theme: String::from("nord"),
            icons: String::from("ascii"),
            keymap: String::from("vim"),
            low_power: LowPower::Off,
            glass: Glass::On,
            extra_contrast: true,
            sort_desc: false,
            cpu_unit: CpuUnit::Total,
            net_bits: true,
            zen: true,
            onboarding_completed: String::from("1.1.0"),
            updates_check: true,
            updates_auto_install: false,
            alerts: Vec::new(),
        };
        let text = cfg.to_toml().unwrap();
        assert_eq!(parse(&text), cfg, "{text}");
        let via_serde: ConfigV1 = toml_edit::de::from_str(&text).unwrap();
        assert_eq!(Config::from(via_serde), cfg);
    }

    fn quiet_hot() -> Vec<crate::alerts::RuleSpec> {
        vec![crate::alerts::RuleSpec {
            id: String::from("hot"),
            enabled: Some(false),
            ..crate::alerts::RuleSpec::default()
        }]
    }

    #[test]
    fn alerts_round_trip_through_to_toml() {
        let cfg = Config {
            alerts: quiet_hot(),
            ..Config::default()
        };
        let text = cfg.to_toml().unwrap();
        assert!(text.contains("[[alerts]]"), "{text}");
        assert_eq!(parse(&text), cfg, "{text}");
    }

    #[test]
    fn toggling_an_alert_writes_only_the_alerts_tables() {
        let text = "# my prefs\ntheme = \"nord\" # dark\n";
        let out = edit(text, |c| c.alerts = quiet_hot());
        assert!(
            out.starts_with("# my prefs\ntheme = \"nord\" # dark\n"),
            "{out}"
        );
        assert!(
            out.contains("[[alerts]]\nid = \"hot\"\nenabled = false\n"),
            "{out}"
        );
        assert_eq!(parse(&out).alerts, quiet_hot());
        let back = edit(&out, |c| c.alerts.clear());
        assert!(!back.contains("alerts"), "{back}");
    }

    #[test]
    fn a_broken_alert_table_warns_and_keeps_the_rest() {
        let loaded =
            Config::from_toml("theme = \"nord\"\n[[alerts]]\nid = \"x\"\nwhen = \"melt(9)\"\n")
                .unwrap();
        assert_eq!(loaded.config.theme, "nord");
        assert!(loaded.config.alerts.is_empty());
        assert_eq!(loaded.warnings.len(), 1, "{:?}", loaded.warnings);
    }

    #[test]
    fn default_toml_is_versioned_and_complete() {
        let text = Config::default().to_toml().unwrap();
        for key in [
            "version = 1",
            "interval_ms = 500",
            "surface = \"auto\"",
            "theme = \"auto\"",
            "[panels]",
            "[proc]",
            "ratio = 55",
            "[proc.sort]",
            "[units]",
            "net_bits = false",
            "low_power = \"auto\"",
        ] {
            assert!(text.contains(key), "{key} missing from\n{text}");
        }
    }

    #[test]
    fn serde_defaults_fill_missing_tables() {
        let v1: ConfigV1 = toml_edit::de::from_str("[proc]\ntree = true\n").unwrap();
        let cfg = Config::from(v1);
        assert!(cfg.show_tree);
        assert_eq!(cfg.proc_ratio, PROC_RATIO_DEFAULT);
        assert_eq!(cfg.interval, INTERVAL_DEFAULT);
    }

    #[test]
    fn unchanged_config_writes_nothing() {
        for text in [LEGACY_ML_DEV, LEGACY_DEFAULT_250, "", "# hi\nfuture = 1\n"] {
            let loaded = Config::from_toml(text).unwrap();
            let mut doc = loaded.doc.clone();
            assert_eq!(loaded.config.write_changes(&loaded.config, &mut doc), 0);
            assert_eq!(doc.to_string(), text, "byte-identical");
        }
    }

    #[test]
    fn comment_and_unknown_key_survive_a_toggle() {
        let text = "# my settings\nfuture_key = 42\n[proc]\n# keep this\nratio = 50 # wide\n";
        let out = edit(text, |c| c.show_gpu = !c.show_gpu);
        assert!(out.contains("# my settings"));
        assert!(out.contains("future_key = 42"));
        assert!(out.contains("# keep this"));
        assert!(out.contains("ratio = 50 # wide"));
        assert!(out.contains("gpu = false"));
        assert!(!parse(&out).show_gpu);
    }

    #[test]
    fn changed_value_keeps_its_trailing_comment() {
        let out = edit("[proc]\nratio = 50   # my split\n", |c| c.proc_ratio = 61);
        assert!(out.contains("ratio = 61   # my split"), "{out}");
    }

    #[test]
    fn only_changed_keys_are_written() {
        let out = edit("", |c| c.show_disk = true);
        let parsed: DocumentMut = out.parse().unwrap();
        assert_eq!(parsed.get("version").and_then(Item::as_integer), Some(1));
        assert_eq!(parsed["panels"]["disk"].as_bool(), Some(true));
        assert!(parsed["panels"].get("gpu").is_none());
        assert!(parsed.get("interval_ms").is_none());
        assert!(!out.contains("[proc]"), "{out}");
    }

    #[test]
    fn writing_a_legacy_setting_replaces_the_flat_key() {
        let out = edit(LEGACY_ML_DEV, |c| c.show_net = true);
        assert!(!out.contains("show_net"), "{out}");
        assert!(out.contains("show_gpu = true"), "other legacy keys stay");
        let cfg = parse(&out);
        assert!(cfg.show_net);
        let mut expected = parse(LEGACY_ML_DEV);
        expected.show_net = true;
        assert_eq!(cfg, expected, "nothing else moved");
    }

    #[test]
    fn nested_write_creates_implicit_parents() {
        let out = edit("", |c| c.sort_desc = false);
        assert!(out.contains("[proc.sort]"), "{out}");
        assert!(!out.contains("[proc]\n"), "no empty parent header: {out}");
        assert!(!parse(&out).sort_desc);
    }

    #[test]
    fn write_into_inline_table_stays_inline() {
        let out = edit("proc = { tree = true }\n", |c| c.proc_sort = ProcSort::Mem);
        let cfg = parse(&out);
        assert!(cfg.show_tree);
        assert_eq!(cfg.proc_sort, ProcSort::Mem);
        assert!(!out.contains("[proc"), "{out}");
    }

    #[test]
    fn write_over_a_scalar_parent_repairs_it() {
        let loaded = Config::from_toml("panels = 3\n").unwrap();
        let mut next = loaded.config.clone();
        next.show_fans = false;
        let mut doc = loaded.doc;
        next.write_changes(&loaded.config, &mut doc);
        assert!(!parse(&doc.to_string()).show_fans);
    }

    #[test]
    fn surface_auto_writes_auto_label() {
        let out = edit(LEGACY_ML_DEV, |c| c.surface = None);
        assert!(out.contains("surface = \"auto\""), "{out}");
        assert_eq!(parse(&out).surface, None);
    }

    #[test]
    fn existing_version_is_not_duplicated() {
        let out = edit("version = 1\n", |c| c.net_bits = true);
        assert_eq!(out.matches("version").count(), 1);
    }

    #[test]
    fn property_every_pair_of_changes_round_trips_from_every_fixture() {
        let changes: Vec<fn(&mut Config)> = vec![
            |c| c.interval = INTERVALS[4],
            |c| c.surface = Some(Surface::Glance),
            |c| c.surface = None,
            |c| c.theme = String::from("gruvbox"),
            |c| c.icons = String::from("nerd"),
            |c| c.keymap = String::from("vim"),
            |c| c.low_power = LowPower::On,
            |c| c.show_gpu = !c.show_gpu,
            |c| c.show_net = !c.show_net,
            |c| c.show_disk = !c.show_disk,
            |c| c.show_fans = !c.show_fans,
            |c| c.show_cores = !c.show_cores,
            |c| c.show_threads = !c.show_threads,
            |c| c.show_tree = !c.show_tree,
            |c| c.proc_sort = c.proc_sort.next(),
            |c| c.sort_desc = !c.sort_desc,
            |c| c.proc_ratio = 40,
            |c| c.cpu_unit = CpuUnit::Total,
            |c| c.net_bits = !c.net_bits,
        ];
        let fixtures = [
            "",
            LEGACY_DEFAULT_250,
            LEGACY_ML_DEV,
            LEGACY_NO_GPU,
            LEGACY_PID,
            LEGACY_TREE_THREADS,
            LEGACY_MEM_GLANCE,
            "# c\nx = 1\n[proc]\nratio = 50\n[zzz]\nq = \"r\"\n",
            "proc = { ratio = 44 }\n",
            "panels.gpu = false\n",
        ];
        for fixture in fixtures {
            for (i, first) in changes.iter().enumerate() {
                for (j, second) in changes.iter().enumerate() {
                    let base = parse(fixture);
                    let mut next = base.clone();
                    first(&mut next);
                    second(&mut next);
                    let mut doc: DocumentMut = fixture.parse().unwrap();
                    next.write_changes(&base, &mut doc);
                    let text = doc.to_string();
                    assert_eq!(parse(&text), next, "{fixture:?} change {i}+{j}\n{text}");
                    if fixture.contains("x = 1") {
                        assert!(text.contains("x = 1") && text.contains("# c"));
                    }
                }
            }
        }
    }
}
