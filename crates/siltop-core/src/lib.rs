mod alerts;
mod config;
mod day;
mod error;
mod format;
mod friendly;
mod history;
mod inspect;
mod presets;
mod series;
mod snapshot;
pub mod store;
mod surface;
mod text;
pub mod trace;
mod verdict;

pub use alerts::{
    AlertEngine, Builtin, Condition, DEFAULT_RULES, Fired, HYSTERESIS, Rule, RuleSpec, Sinks,
    effective_rules, format_secs, parse_condition, parse_secs, read_specs, rule_enabled,
    set_rule_enabled, write_specs,
};
pub use config::{
    Config, ConfigV1, CpuUnit, Glass, HISTORY_WINDOW, INTERVAL_DEFAULT, INTERVAL_FAST,
    INTERVAL_MID, INTERVAL_SLOW, INTERVALS, KEYMAP_DEFAULT, Loaded, LowPower, PROC_RATIO_DEFAULT,
    PROC_RATIO_MAX, PROC_RATIO_MIN, PanelsV1, ParseError, ProcSort, ProcV1, SENSOR_PERIOD, SortV1,
    Switch, THEME_AUTO, UnitsV1, parse_surface, surface_label,
};
pub use day::Day;
pub use error::{Error, Result};
pub use format::{
    Base, NetUnit, bits_per_sec, byte_rate, bytes_per_sec, bytes_short, celsius, fixed, net_rate,
    nice_ceiling_in, percent_display, ready_pct, storage, watts_display,
};
pub use friendly::friendly_name;
pub use history::{
    Bucket, History, LIVE_WINDOW, SCALE_EASE, SampleClock, Scale, ScaleRange, Stamp, TimeAxis,
    Zoom, capacity_for, nice_ceiling,
};
pub use inspect::{Inspection, OpenFile, Socket, ThreadInfo};
pub use presets::{Preset, PresetSpec, preset_label};
pub use series::{Clock, FAN_SERIES_MAX, Histories, SERIES, SeriesDef, SeriesId};
pub use snapshot::{
    Adapter, AwakeAssertion, BatterySnapshot, ChargeState, Cluster, ClusterKind, CollectorId,
    Composition, CoreSample, CpuCluster, CpuSnapshot, DiskDevice, DiskSnapshot, DiskVolume,
    FanMetric, FanSnapshot, GpuSnapshot, Health, HealthReport, HoldReason, IfaceRate,
    MemorySnapshot, NOT_CHARGING_CHARGE_LIMIT, NetworkSnapshot, POWER_STALE_AFTER, PowerSnapshot,
    PowerSource, Pressure, Process, Reading, ReadingSource, Sampled, SensorsSnapshot, Snapshot,
    SocInfo, TempReading, Thermal, Throttle, TimeRemaining, health_percent, linear_core_role,
    role_for_core,
};
pub use surface::{Surface, WORK_MIN_COLS, WORK_MIN_ROWS, auto_surface};
pub use text::{into_printable, is_unsafe, printable};
pub use trace::crc32;
pub use verdict::{
    BATTERY_DRAIN_W, BUSY_PROCESS_CORES, BUSY_TOTAL, SWAP_GROWTH_BPS, Signals, THROTTLE_THERMAL,
    TIGHT_PRESSURE, VERDICT_HOLD, Verdict, VerdictEngine, VerdictKind, assess, battery_drain_w,
    top_process,
};
