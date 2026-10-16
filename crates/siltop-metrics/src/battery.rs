//! Battery and power source.
//!
//! Three sources, all readable without sudo:
//! - `AppleSmartBattery` in the `IORegistry`: flags, voltage, amperage,
//!   cycles, capacities, adapter and charger details. macOS 27 moved the
//!   capacities into a `BatteryData` sub-dictionary and dropped the
//!   top-level `Temperature`; both layouts are read, `BatteryData` first.
//! - `IOPowerSources`: the providing source, the percent `pmset -g batt`
//!   prints, time to empty/full, Low Power Mode, and
//!   `IOPSGetTimeRemainingEstimate`.
//! - `IOPMCopyAssertionsByProcess`: which processes keep the Mac awake.
//!
//! `PowerTelemetryData` refreshes only about once a minute, so it is never
//! used for live numbers. Reads happen every [`PERIOD`]; ticks in between
//! keep the last value. Parsing is split from the FFI through [`Fields`] so
//! both dictionary layouts are tested from fixtures on every platform.

use std::time::{Duration, Instant};

use siltop_core::{
    Adapter, AwakeAssertion, BatterySnapshot, ChargeState, CollectorId, PowerSource, Result,
    Snapshot, TimeRemaining, health_percent,
};

use crate::collector::{Collector, Outcome};

/// How often the battery sources are re-read.
pub(crate) const PERIOD: Duration = Duration::from_secs(5);

/// `AvgTimeToEmpty/ToFull` (minutes) while there is no estimate.
const MINUTES_UNKNOWN: i64 = 65_535;

/// One scalar from a power dictionary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Val {
    Int(i64),
    Bool(bool),
    Text(String),
}

/// A power dictionary addressed by `/`-separated paths
/// (`BatteryData/DesignCapacity`).
pub(crate) trait Fields {
    fn get(&self, path: &str) -> Option<Val>;

    fn int(&self, path: &str) -> Option<i64> {
        match self.get(path)? {
            Val::Int(n) => Some(n),
            Val::Bool(b) => Some(i64::from(b)),
            Val::Text(_) => None,
        }
    }

    /// Booleans; registry flags are sometimes integers (`ChargerData`).
    fn flag(&self, path: &str) -> Option<bool> {
        match self.get(path)? {
            Val::Bool(b) => Some(b),
            Val::Int(n) => Some(n != 0),
            Val::Text(_) => None,
        }
    }

    fn text(&self, path: &str) -> Option<String> {
        match self.get(path)? {
            Val::Text(t) => {
                let t = t.trim();
                (!t.is_empty()).then(|| siltop_core::into_printable(t.to_owned()))
            }
            _ => None,
        }
    }

    fn first_int(&self, paths: &[&str]) -> Option<i64> {
        paths.iter().find_map(|p| self.int(p))
    }

    fn first_flag(&self, paths: &[&str]) -> Option<bool> {
        paths.iter().find_map(|p| self.flag(p))
    }
}

/// Registry amperage is a signed 64-bit value that some firmware reports
/// as a wrapped 32-bit unsigned one (4294966296 for -1000 mA).
pub(crate) fn signed_ma(raw: i64) -> i64 {
    if raw > i64::from(i32::MAX) && raw <= i64::from(u32::MAX) {
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let wrapped = raw as u32 as i32;
        i64::from(wrapped)
    } else {
        raw
    }
}

/// A capacity or count: positive and in range, else absent.
fn positive_u32(raw: i64) -> Option<u32> {
    u32::try_from(raw).ok().filter(|&n| n > 0)
}

/// Minutes with the 65535 sentinel (and negatives) mapped to absent.
pub(crate) fn minutes(raw: i64) -> Option<Duration> {
    if !(0..MINUTES_UNKNOWN).contains(&raw) {
        return None;
    }
    u64::try_from(raw).ok().map(|m| Duration::from_secs(m * 60))
}

/// State of charge in percent. Apple Silicon reports `MaxCapacity == 100`
/// and a percent in `CurrentCapacity`; older layouts report mAh for both.
pub(crate) fn percent_of(current: i64, max: i64) -> Option<u8> {
    if max <= 0 || current < 0 {
        return None;
    }
    let pct = if max == 100 {
        current
    } else {
        let (c, m) = (i128::from(current), i128::from(max));
        i64::try_from((c * 200 + m) / (m * 2)).unwrap_or(i64::MAX)
    };
    u8::try_from(pct.min(100)).ok()
}

/// Battery watts from millivolts and signed milliamps.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn watts(voltage_mv: u32, amperage_ma: i64) -> f32 {
    (f64::from(voltage_mv) * amperage_ma as f64 / 1.0e6) as f32
}

/// Everything `AppleSmartBattery` says, in either layout.
pub(crate) fn parse_registry(f: &impl Fields) -> BatterySnapshot {
    let external = f.first_flag(&["ExternalConnected", "AppleRawExternalConnected"]);
    let is_charging = f.first_flag(&["IsCharging", "ChargerData/IsCharging"]);
    let fully_charged = f.first_flag(&["FullyCharged", "BatteryData/FullyCharged"]);
    let state = match (external, is_charging) {
        (Some(ext), Some(chg)) => ChargeState::derive(ext, chg, fully_charged.unwrap_or(false)),
        _ => ChargeState::Unknown,
    };
    let percent = [
        ("CurrentCapacity", "MaxCapacity"),
        ("BatteryData/CurrentCapacity", "BatteryData/MaxCapacity"),
    ]
    .iter()
    .find_map(|(cur, max)| percent_of(f.int(cur)?, f.int(max)?));
    let voltage_mv = f
        .first_int(&["Voltage", "AppleRawBatteryVoltage", "BatteryData/Voltage"])
        .and_then(positive_u32);
    let amperage_ma = f.first_int(&["InstantAmperage", "Amperage"]).map(signed_ma);
    let design = f
        .first_int(&["BatteryData/DesignCapacity", "DesignCapacity"])
        .and_then(positive_u32);
    let nominal = f
        .first_int(&["BatteryData/NominalChargeCapacity", "NominalChargeCapacity"])
        .and_then(positive_u32);
    let full_charge = f
        .first_int(&[
            "BatteryData/FullChargeCapacity",
            "AppleRawMaxCapacity",
            "FullChargeCapacity",
        ])
        .and_then(positive_u32);
    let health_pct = design.and_then(|d| health_percent(nominal.or(full_charge)?, d));
    BatterySnapshot {
        present: true,
        percent,
        state,
        external_connected: external,
        is_charging,
        fully_charged,
        voltage_mv,
        amperage_ma,
        watts: voltage_mv.zip(amperage_ma).map(|(v, a)| watts(v, a)),
        cycle_count: f
            .first_int(&["CycleCount", "BatteryData/CycleCount"])
            .and_then(|n| u32::try_from(n).ok()),
        design_cycle_count: f.int("DesignCycleCount9C").and_then(positive_u32),
        design_capacity_mah: design,
        nominal_capacity_mah: nominal,
        full_charge_capacity_mah: full_charge,
        health_pct,
        time_to_empty: f
            .first_int(&["AvgTimeToEmpty", "BatteryData/AvgTimeToEmpty"])
            .and_then(minutes),
        time_to_full: f.int("AvgTimeToFull").and_then(minutes),
        adapter: parse_adapter(f, external),
        not_charging_reason: f
            .int("ChargerData/NotChargingReason")
            .and_then(|n| u64::try_from(n).ok()),
        thermally_limited: f
            .int("ChargerData/TimeChargingThermallyLimited")
            .and_then(|n| u64::try_from(n).ok()),
        temp_c: registry_temp(f),
        ..BatterySnapshot::default()
    }
}

/// The adapter is only reported while one is connected; `Watts == 0` and
/// an empty name mean none.
fn parse_adapter(f: &impl Fields, external: Option<bool>) -> Option<Adapter> {
    if external == Some(false) {
        return None;
    }
    let watts = f.int("AdapterDetails/Watts").and_then(positive_u32);
    let name = f.text("AdapterDetails/Name");
    (watts.is_some() || name.is_some()).then_some(Adapter { watts, name })
}

/// macOS 26 and earlier: top-level `Temperature` in centi-°C.
#[allow(clippy::cast_precision_loss)]
fn registry_temp(f: &impl Fields) -> Option<f32> {
    let celsius = f.int("Temperature")? as f32 / 100.0;
    (-20.0..=90.0).contains(&celsius).then_some(celsius)
}

/// Overlays the internal battery's `IOPowerSources` description: the
/// percent `pmset` prints, time estimates and Low Power Mode.
pub(crate) fn apply_power_source(snap: &mut BatterySnapshot, ps: &impl Fields) {
    if let Some(pct) = ps
        .int("Current Capacity")
        .zip(ps.int("Max Capacity"))
        .and_then(|(c, m)| percent_of(c, m))
    {
        snap.percent = Some(pct);
    }
    if let Some(state) = ps.text("Power Source State") {
        snap.source = PowerSource::from_iops(&state);
    }
    if snap.is_charging.is_none() {
        snap.is_charging = ps.flag("Is Charging");
    }
    // IOPS says -1 while it is still calculating and 0 when not relevant.
    let ps_minutes = |key: &str| ps.int(key).filter(|&m| m > 0).and_then(minutes);
    if let Some(t) = ps_minutes("Time to Empty") {
        snap.time_to_empty = Some(t);
    }
    if let Some(t) = ps_minutes("Time to Full Charge") {
        snap.time_to_full = Some(t);
    }
    if let Some(lpm) = ps.flag("LPM Active") {
        snap.low_power_mode = Some(lpm);
    }
}

/// Final consistency: the source follows the external flag when IOPS did
/// not say, and times only apply in the direction the battery is going.
pub(crate) fn finish(snap: &mut BatterySnapshot, estimate_secs: f64) {
    if snap.source == PowerSource::Unknown {
        snap.source = match snap.external_connected {
            Some(true) => PowerSource::Ac,
            Some(false) => PowerSource::Battery,
            None => PowerSource::Unknown,
        };
    }
    if snap.state != ChargeState::Discharging {
        snap.time_to_empty = None;
    }
    if snap.state != ChargeState::Charging {
        snap.time_to_full = None;
    }
    snap.time_remaining = TimeRemaining::from_estimate(estimate_secs);
    if snap.source == PowerSource::Ac && snap.time_remaining == TimeRemaining::Unknown {
        snap.time_remaining = TimeRemaining::Unlimited;
    }
}

/// One process's assertions, as `IOPMCopyAssertionsByProcess` lists them.
pub(crate) fn parse_assertions<F: Fields>(pid: i32, items: &[F]) -> Vec<AwakeAssertion> {
    items
        .iter()
        .filter(|a| a.int("AssertLevel") != Some(0))
        .filter_map(|a| {
            let kind = a.text("AssertType")?;
            let process = a
                .text("Process Name")
                .unwrap_or_else(|| format!("pid {pid}"));
            Some(AwakeAssertion {
                pid,
                process,
                kind,
                name: a.text("AssertName"),
            })
        })
        .collect()
}

/// Orders by pid, then kind and name, and drops exact duplicates.
pub(crate) fn tidy_awake(list: &mut Vec<AwakeAssertion>) {
    list.sort_by(|a, b| (a.pid, &a.kind, &a.name).cmp(&(b.pid, &b.kind, &b.name)));
    list.dedup();
}

/// The battery temperature from the heat collector's `battery` zone.
fn sensor_temp(snap: &Snapshot) -> Option<f32> {
    snap.sensors
        .readings
        .iter()
        .find(|r| r.name == "battery")
        .map(|r| r.celsius)
}

pub(crate) struct BatteryCollector {
    next_read: Option<Instant>,
    last: Outcome,
}

impl BatteryCollector {
    pub(crate) fn new() -> Self {
        Self {
            next_read: None,
            last: Outcome::Absent,
        }
    }

    /// `None` when this platform has no power-source API at all.
    #[cfg_attr(
        target_os = "macos",
        allow(clippy::unnecessary_wraps, reason = "None off macOS")
    )]
    fn read() -> Option<BatterySnapshot> {
        #[cfg(target_os = "macos")]
        {
            Some(macos::read())
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    fn due(&mut self, now: Instant) -> bool {
        if self
            .next_read
            .is_some_and(|at| now + crate::cadence::TICK_SLACK < at)
        {
            return false;
        }
        self.next_read = Some(now + PERIOD);
        true
    }
}

impl Collector for BatteryCollector {
    fn id(&self) -> CollectorId {
        CollectorId::Battery
    }

    /// A desktop still has a power source and assertions, so only a
    /// platform with nothing at all is absent.
    fn collect(&mut self, now: Instant, snap: &mut Snapshot) -> Result<Outcome> {
        if self.due(now) {
            let Some(battery) = Self::read() else {
                snap.battery = BatterySnapshot::default();
                self.last = Outcome::Absent;
                return Ok(self.last);
            };
            snap.battery = battery;
            self.last = Outcome::Fresh;
        }
        if snap.battery.present
            && let Some(celsius) = sensor_temp(snap)
        {
            snap.battery.temp_c = Some(celsius);
        }
        Ok(self.last)
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::RefCell;
    use std::ffi::CString;

    use siltop_core::{BatterySnapshot, PowerSource};

    use super::{Fields, Val, apply_power_source, finish, parse_assertions, parse_registry};
    use crate::iokit::{self, CfKey, CfOwned, CfRef, IoObject};

    fn key(segment: &str) -> Option<CfKey> {
        CfKey::new(&CString::new(segment).ok()?)
    }

    fn scalar(value: CfRef<'_>) -> Option<Val> {
        if let Some(n) = iokit::cf_i64(value) {
            return Some(Val::Int(n));
        }
        if let Some(b) = iokit::cf_bool(value) {
            return Some(Val::Bool(b));
        }
        iokit::cf_string(value).map(Val::Text)
    }

    fn walk<'a>(mut value: CfRef<'a>, rest: &mut std::str::Split<'_, char>) -> Option<CfRef<'a>> {
        for segment in rest {
            value = iokit::cf_dict_get(value, &key(segment)?)?;
        }
        Some(value)
    }

    /// A borrowed `CFDictionary`.
    pub(super) struct CfFields<'a>(pub(super) CfRef<'a>);

    impl Fields for CfFields<'_> {
        fn get(&self, path: &str) -> Option<Val> {
            scalar(walk(self.0, &mut path.split('/'))?)
        }
    }

    /// A registry entry; each top-level property is copied once.
    pub(super) struct RegistryFields {
        entry: IoObject,
        cache: RefCell<Vec<(String, Option<CfOwned>)>>,
    }

    impl RegistryFields {
        pub(super) fn new(entry: IoObject) -> Self {
            Self {
                entry,
                cache: RefCell::new(Vec::new()),
            }
        }
    }

    impl Fields for RegistryFields {
        fn get(&self, path: &str) -> Option<Val> {
            let mut parts = path.split('/');
            let top = parts.next()?;
            let mut cache = self.cache.borrow_mut();
            let index = if let Some(i) = cache.iter().position(|(k, _)| k == top) {
                i
            } else {
                let value = iokit::property(&self.entry, &key(top)?);
                cache.push((top.to_owned(), value));
                cache.len() - 1
            };
            let root = cache[index].1.as_ref()?;
            scalar(walk(root.borrow(), &mut parts)?)
        }
    }

    fn assertions() -> Vec<siltop_core::AwakeAssertion> {
        let Some(by_pid) = iokit::assertions_by_process() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (pid, list) in iokit::cf_dict_entries(by_pid.borrow()) {
            let Some(pid) = iokit::cf_i64(pid).and_then(|p| i32::try_from(p).ok()) else {
                continue;
            };
            let items: Vec<CfFields<'_>> = iokit::cf_array_items(list)
                .into_iter()
                .map(CfFields)
                .collect();
            out.extend(parse_assertions(pid, &items));
        }
        super::tidy_awake(&mut out);
        out
    }

    pub(super) fn read() -> BatterySnapshot {
        let mut snap = iokit::services(c"AppleSmartBattery")
            .into_iter()
            .next()
            .map(|entry| parse_registry(&RegistryFields::new(entry)))
            .unwrap_or_default();
        if let Some(blob) = iokit::power_sources_info() {
            for desc in iokit::power_source_descriptions(&blob) {
                let fields = CfFields(desc.borrow());
                if fields.text("Type").as_deref() == Some("InternalBattery") {
                    snap.present = true;
                    apply_power_source(&mut snap, &fields);
                }
            }
            if let Some(kind) = iokit::providing_power_source(&blob) {
                snap.source = PowerSource::from_iops(&kind);
            }
        }
        finish(&mut snap, iokit::time_remaining_estimate());
        snap.awake = assertions();
        snap
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use siltop_core::{HoldReason, NOT_CHARGING_CHARGE_LIMIT, TempReading};

    /// A flat `{path: value}` dictionary.
    #[derive(Default, Clone)]
    struct Map(HashMap<String, Val>);

    impl Fields for Map {
        fn get(&self, path: &str) -> Option<Val> {
            self.0.get(path).cloned()
        }
    }

    impl Map {
        fn with(mut self, path: &str, value: Val) -> Self {
            self.0.insert(path.to_owned(), value);
            self
        }

        fn without(mut self, path: &str) -> Self {
            self.0.remove(path);
            self
        }
    }

    const M5_PRO_AC_HELD: &str = include_str!("fixtures/battery_mac17_8_macos27_ac_held.tsv");

    /// Parses `path<TAB>type<TAB>value`; integers wider than i64 wrap the
    /// way `CFNumber` stores `ioreg`'s unsigned 64-bit output.
    fn fixture(text: &str) -> Map {
        let mut map = Map::default();
        for line in text
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
        {
            let mut cols = line.splitn(3, '\t');
            let (path, kind, value) = (
                cols.next().unwrap(),
                cols.next().unwrap(),
                cols.next().unwrap_or(""),
            );
            let value = match kind {
                "i" => {
                    let wide: i128 = value.parse().unwrap();
                    #[allow(clippy::cast_possible_truncation)]
                    Val::Int(wide as i64)
                }
                "b" => Val::Bool(value == "true"),
                "s" => Val::Text(value.to_owned()),
                other => panic!("bad type {other} in {line}"),
            };
            map.0.insert(path.to_owned(), value);
        }
        map
    }

    fn m5() -> Map {
        fixture(M5_PRO_AC_HELD)
    }

    /// The same battery in the macOS 26 layout: capacities at the top
    /// level, no `BatteryData`, and a `Temperature` key.
    fn macos26() -> Map {
        let mut map = Map::default();
        for (k, v) in &m5().0 {
            if !k.starts_with("BatteryData/") {
                map.0.insert(k.clone(), v.clone());
            }
        }
        map.with("DesignCapacity", Val::Int(8579))
            .with("NominalChargeCapacity", Val::Int(8731))
            .with("AppleRawMaxCapacity", Val::Int(8487))
            .with("AppleRawCurrentCapacity", Val::Int(6762))
            .with("Temperature", Val::Int(3056))
    }

    /// Unplugged and discharging at 1.5 A, as ioreg prints it (unsigned).
    fn on_battery() -> Map {
        m5().with("ExternalConnected", Val::Bool(false))
            .with("AppleRawExternalConnected", Val::Bool(false))
            .with("InstantAmperage", Val::Int(-1500))
            .with("Amperage", Val::Int(-1500))
            .with("AvgTimeToEmpty", Val::Int(412))
            .with("AdapterDetails/Watts", Val::Int(0))
            .with("ChargerData/NotChargingReason", Val::Int(0))
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    // --- the captured M5 Pro fixture (pmset: 80%; AC attached; not charging)

    #[test]
    fn fixture_matches_pmset_and_system_profiler() {
        let snap = parse_registry(&m5());
        assert!(snap.present);
        assert_eq!(snap.percent, Some(80), "pmset -g batt: 80%");
        assert_eq!(snap.state, ChargeState::Held, "AC attached; not charging");
        assert_eq!(snap.cycle_count, Some(14));
        assert_eq!(snap.design_cycle_count, Some(1000));
        assert_eq!(snap.health_pct, Some(100), "Maximum Capacity: 100%");
        assert_eq!(snap.external_connected, Some(true));
        assert_eq!(snap.is_charging, Some(false));
        assert_eq!(snap.fully_charged, Some(false));
    }

    #[test]
    fn fixture_reads_capacities_from_battery_data() {
        let snap = parse_registry(&m5());
        assert_eq!(snap.design_capacity_mah, Some(8579));
        assert_eq!(snap.nominal_capacity_mah, Some(8731));
        assert_eq!(snap.full_charge_capacity_mah, Some(8487));
    }

    #[test]
    fn fixture_adapter_is_the_140w_brick() {
        let adapter = parse_registry(&m5()).adapter.expect("adapter");
        assert_eq!(adapter.watts, Some(140));
        assert_eq!(adapter.name.as_deref(), Some("140W USB-C Power Adapter"));
    }

    #[test]
    fn fixture_is_held_at_the_charge_limit() {
        let snap = parse_registry(&m5());
        assert_eq!(snap.not_charging_reason, Some(NOT_CHARGING_CHARGE_LIMIT));
        assert_eq!(snap.hold_reason(), Some(HoldReason::ChargeLimit));
        assert_eq!(snap.thermally_limited, Some(0));
    }

    #[test]
    fn fixture_power_is_zero_watts_not_absent_while_held() {
        let snap = parse_registry(&m5());
        assert_eq!(snap.voltage_mv, Some(12_384));
        assert_eq!(snap.amperage_ma, Some(0));
        assert_eq!(snap.watts, Some(0.0), "a measured 0 A is a real reading");
    }

    #[test]
    fn fixture_65535_times_are_absent() {
        let snap = parse_registry(&m5());
        assert_eq!(snap.time_to_empty, None);
        assert_eq!(snap.time_to_full, None);
    }

    #[test]
    fn macos27_has_no_registry_temperature() {
        assert_eq!(parse_registry(&m5()).temp_c, None);
    }

    // --- layouts

    #[test]
    fn macos26_layout_reads_top_level_capacities() {
        let old = parse_registry(&macos26());
        let new = parse_registry(&m5());
        assert_eq!(old.design_capacity_mah, new.design_capacity_mah);
        assert_eq!(old.nominal_capacity_mah, new.nominal_capacity_mah);
        assert_eq!(old.full_charge_capacity_mah, Some(8487));
        assert_eq!(old.health_pct, Some(100));
        assert_eq!(old.percent, Some(80));
        assert_eq!(old.temp_c, Some(30.56));
    }

    #[test]
    fn battery_data_wins_over_top_level_keys() {
        let map = m5()
            .with("DesignCapacity", Val::Int(1))
            .with("NominalChargeCapacity", Val::Int(1));
        let snap = parse_registry(&map);
        assert_eq!(snap.design_capacity_mah, Some(8579));
        assert_eq!(snap.nominal_capacity_mah, Some(8731));
    }

    #[test]
    fn health_falls_back_to_full_charge_capacity() {
        let map = m5().without("BatteryData/NominalChargeCapacity");
        let snap = parse_registry(&map);
        assert_eq!(snap.nominal_capacity_mah, None);
        // 8487 / 8579 = 98.93%.
        assert_eq!(snap.health_pct, Some(99));
    }

    #[test]
    fn health_absent_without_design_capacity() {
        let map = m5().without("BatteryData/DesignCapacity");
        assert_eq!(parse_registry(&map).health_pct, None);
        let zero = m5().with("BatteryData/DesignCapacity", Val::Int(0));
        assert_eq!(parse_registry(&zero).health_pct, None, "0 mAh is no design");
    }

    #[test]
    fn worn_battery_health_rounds() {
        let map = m5().with("BatteryData/NominalChargeCapacity", Val::Int(7250));
        // 7250 / 8579 = 84.51%.
        assert_eq!(parse_registry(&map).health_pct, Some(85));
    }

    #[test]
    fn percent_falls_back_to_battery_data() {
        let map = m5().without("CurrentCapacity").without("MaxCapacity");
        assert_eq!(parse_registry(&map).percent, Some(80));
    }

    #[test]
    fn percent_in_mah_layout() {
        assert_eq!(percent_of(6762, 8487), Some(80));
        assert_eq!(percent_of(80, 100), Some(80));
        assert_eq!(percent_of(0, 100), Some(0));
        assert_eq!(percent_of(120, 100), Some(100), "clamped");
        assert_eq!(percent_of(9000, 8487), Some(100), "clamped");
        assert_eq!(percent_of(50, 0), None);
        assert_eq!(percent_of(50, -1), None);
        assert_eq!(percent_of(-5, 100), None);
        assert_eq!(percent_of(i64::MAX, i64::MAX), Some(100), "no overflow");
    }

    #[test]
    fn percent_sweep_is_monotonic_and_bounded() {
        for max in [100_i64, 4000, 8487, 65_535] {
            let mut prev = 0;
            for cur in (0..=max + max / 5).step_by(7) {
                let pct = percent_of(cur, max).unwrap();
                assert!(pct <= 100);
                assert!(pct >= prev, "{cur}/{max}");
                prev = pct;
            }
        }
    }

    // --- amperage sign and watts

    #[test]
    fn amperage_sign_survives_every_encoding() {
        assert_eq!(signed_ma(-1000), -1000, "SInt64 as CFNumber stores it");
        assert_eq!(signed_ma(4_294_966_296), -1000, "wrapped 32-bit");
        assert_eq!(signed_ma(1500), 1500);
        assert_eq!(signed_ma(0), 0);
        assert_eq!(signed_ma(i64::from(i32::MAX)), i64::from(i32::MAX));
        assert_eq!(signed_ma(i64::from(u32::MAX)), -1);
        assert_eq!(signed_ma(i64::MIN), i64::MIN);
    }

    #[test]
    fn ioreg_unsigned_64_bit_amperage_parses_negative() {
        // ioreg prints SInt64 -1000 as 18446744073709550616.
        let map = fixture("InstantAmperage\ti\t18446744073709550616\nVoltage\ti\t12000\n");
        let snap = parse_registry(&map);
        assert_eq!(snap.amperage_ma, Some(-1000));
        assert!(close(snap.watts.unwrap(), -12.0));
    }

    #[test]
    fn amperage_sweep_round_trips_through_every_encoding() {
        for ma in (-12_000_i64..=12_000).step_by(37) {
            assert_eq!(signed_ma(ma), ma);
            let wrapped = i64::from(u32::from_ne_bytes(i32::try_from(ma).unwrap().to_ne_bytes()));
            assert_eq!(signed_ma(wrapped), ma, "{ma} as u32 {wrapped}");
        }
    }

    #[test]
    fn discharging_watts_are_negative() {
        let snap = parse_registry(&on_battery());
        assert_eq!(snap.state, ChargeState::Discharging);
        assert_eq!(snap.amperage_ma, Some(-1500));
        // 12.384 V × -1.5 A.
        assert!(close(snap.watts.unwrap(), -18.576));
        assert_eq!(snap.time_to_empty, Some(Duration::from_mins(412)));
        assert_eq!(snap.adapter, None, "no adapter on battery");
        assert_eq!(snap.hold_reason(), None);
    }

    #[test]
    fn charging_watts_are_positive() {
        let map = m5()
            .with("IsCharging", Val::Bool(true))
            .with("InstantAmperage", Val::Int(3000))
            .with("AvgTimeToFull", Val::Int(45));
        let snap = parse_registry(&map);
        assert_eq!(snap.state, ChargeState::Charging);
        assert!(snap.watts.unwrap() > 37.0);
        assert_eq!(snap.time_to_full, Some(Duration::from_mins(45)));
    }

    #[test]
    fn instant_amperage_preferred_over_averaged() {
        let map = m5()
            .with("InstantAmperage", Val::Int(-900))
            .with("Amperage", Val::Int(-100));
        assert_eq!(parse_registry(&map).amperage_ma, Some(-900));
        let map = m5()
            .without("InstantAmperage")
            .with("Amperage", Val::Int(-100));
        assert_eq!(parse_registry(&map).amperage_ma, Some(-100));
    }

    #[test]
    fn zero_voltage_means_no_watts() {
        let map = m5()
            .with("Voltage", Val::Int(0))
            .with("AppleRawBatteryVoltage", Val::Int(0));
        let snap = parse_registry(&map);
        assert_eq!(snap.voltage_mv, None);
        assert_eq!(snap.watts, None);
    }

    // --- held derivation and state

    #[test]
    fn full_on_ac_is_full_not_held() {
        let map = m5().with("FullyCharged", Val::Bool(true));
        let snap = parse_registry(&map);
        assert_eq!(snap.state, ChargeState::Full);
        assert_eq!(snap.hold_reason(), None);
    }

    #[test]
    fn held_uses_battery_data_fully_charged_when_top_level_is_missing() {
        let map = m5()
            .without("FullyCharged")
            .with("BatteryData/FullyCharged", Val::Int(1));
        assert_eq!(parse_registry(&map).state, ChargeState::Full);
    }

    #[test]
    fn charger_data_integer_flag_is_a_fallback() {
        let map = m5()
            .without("IsCharging")
            .with("ChargerData/IsCharging", Val::Int(1));
        assert_eq!(parse_registry(&map).state, ChargeState::Charging);
    }

    #[test]
    fn missing_flags_leave_the_state_unknown() {
        let map = m5()
            .without("ExternalConnected")
            .without("AppleRawExternalConnected");
        assert_eq!(parse_registry(&map).state, ChargeState::Unknown);
        let map = m5().without("IsCharging").without("ChargerData/IsCharging");
        assert_eq!(parse_registry(&map).state, ChargeState::Unknown);
    }

    #[test]
    fn unknown_hold_reason_stays_raw() {
        let map = m5().with("ChargerData/NotChargingReason", Val::Int(0x40));
        assert_eq!(
            parse_registry(&map).hold_reason(),
            Some(HoldReason::Other(0x40))
        );
        let neg = m5().with("ChargerData/NotChargingReason", Val::Int(-1));
        assert_eq!(parse_registry(&neg).not_charging_reason, None);
    }

    // --- sentinels

    #[test]
    fn minute_sentinels() {
        assert_eq!(minutes(65_535), None);
        assert_eq!(minutes(65_536), None);
        assert_eq!(minutes(-1), None);
        assert_eq!(minutes(-2), None);
        assert_eq!(minutes(0), Some(Duration::ZERO));
        assert_eq!(minutes(65_534), Some(Duration::from_mins(65534)));
        assert_eq!(minutes(i64::MAX), None);
    }

    #[test]
    fn junk_types_are_ignored() {
        let map = m5()
            .with("CycleCount", Val::Text(String::from("fourteen")))
            .with("BatteryData/CycleCount", Val::Int(14))
            .with("AdapterDetails/Name", Val::Text(String::from("   ")))
            .with("AdapterDetails/Watts", Val::Int(-5));
        let snap = parse_registry(&map);
        assert_eq!(snap.cycle_count, Some(14), "falls through to BatteryData");
        assert_eq!(snap.adapter, None, "blank name and bad watts");
    }

    #[test]
    fn implausible_registry_temperature_is_dropped() {
        for raw in [-9000_i64, 9500, 65_535] {
            let map = macos26().with("Temperature", Val::Int(raw));
            assert_eq!(parse_registry(&map).temp_c, None, "{raw}");
        }
    }

    #[test]
    fn empty_registry_is_present_but_unknown() {
        let snap = parse_registry(&Map::default());
        assert!(snap.present);
        assert_eq!(snap.percent, None);
        assert_eq!(snap.state, ChargeState::Unknown);
        assert_eq!(snap.watts, None);
        assert_eq!(snap.health_pct, None);
        assert_eq!(snap.adapter, None);
    }

    // --- IOPowerSources overlay and finish

    fn ps_ac() -> Map {
        Map::default()
            .with("Type", Val::Text(String::from("InternalBattery")))
            .with("Power Source State", Val::Text(String::from("AC Power")))
            .with("Current Capacity", Val::Int(80))
            .with("Max Capacity", Val::Int(100))
            .with("Is Charging", Val::Bool(false))
            .with("Time to Empty", Val::Int(0))
            .with("Time to Full Charge", Val::Int(0))
            .with("LPM Active", Val::Bool(false))
    }

    #[test]
    fn iops_on_ac_held_matches_pmset() {
        let mut snap = parse_registry(&m5());
        apply_power_source(&mut snap, &ps_ac());
        finish(&mut snap, -2.0);
        assert_eq!(snap.source, PowerSource::Ac);
        assert_eq!(snap.percent, Some(80));
        assert_eq!(snap.low_power_mode, Some(false));
        assert_eq!(snap.time_remaining, TimeRemaining::Unlimited);
        assert_eq!(snap.time_to_empty, None);
        assert_eq!(snap.time_to_full, None);
    }

    #[test]
    fn iops_percent_overrides_the_registry() {
        let mut snap = parse_registry(&m5());
        apply_power_source(&mut snap, &ps_ac().with("Current Capacity", Val::Int(79)));
        assert_eq!(snap.percent, Some(79), "pmset reads IOPS");
    }

    #[test]
    fn iops_minus_one_is_still_calculating() {
        let mut snap = parse_registry(&on_battery().with("AvgTimeToEmpty", Val::Int(65_535)));
        let ps = ps_ac()
            .with(
                "Power Source State",
                Val::Text(String::from("Battery Power")),
            )
            .with("Time to Empty", Val::Int(-1));
        apply_power_source(&mut snap, &ps);
        finish(&mut snap, -1.0);
        assert_eq!(snap.source, PowerSource::Battery);
        assert_eq!(snap.time_to_empty, None);
        assert_eq!(snap.time_remaining, TimeRemaining::Unknown);
    }

    #[test]
    fn iops_time_to_empty_wins_on_battery() {
        let mut snap = parse_registry(&on_battery());
        let ps = ps_ac()
            .with(
                "Power Source State",
                Val::Text(String::from("Battery Power")),
            )
            .with("Time to Empty", Val::Int(192))
            .with("LPM Active", Val::Bool(true));
        apply_power_source(&mut snap, &ps);
        finish(&mut snap, 11_520.0);
        assert_eq!(snap.time_to_empty, Some(Duration::from_mins(192)));
        assert_eq!(
            snap.time_remaining,
            TimeRemaining::Known(Duration::from_mins(192))
        );
        assert_eq!(snap.low_power_mode, Some(true));
    }

    #[test]
    fn finish_derives_the_source_from_the_external_flag() {
        let mut ac = parse_registry(&m5());
        finish(&mut ac, -1.0);
        assert_eq!(ac.source, PowerSource::Ac);
        assert_eq!(
            ac.time_remaining,
            TimeRemaining::Unlimited,
            "AC is unlimited"
        );
        let mut bat = parse_registry(&on_battery());
        finish(&mut bat, -1.0);
        assert_eq!(bat.source, PowerSource::Battery);
        assert_eq!(bat.time_remaining, TimeRemaining::Unknown);
    }

    #[test]
    fn finish_drops_times_against_the_direction_of_charge() {
        let mut snap = parse_registry(&m5());
        snap.time_to_empty = Some(Duration::from_secs(60));
        snap.time_to_full = Some(Duration::from_secs(60));
        finish(&mut snap, -2.0);
        assert_eq!(snap.time_to_empty, None, "held: not emptying");
        assert_eq!(snap.time_to_full, None, "held: not filling");
    }

    #[test]
    fn desktop_is_absent_but_keeps_its_power_source() {
        let mut snap = BatterySnapshot::default();
        finish(&mut snap, -2.0);
        assert!(!snap.present);
        assert_eq!(snap.percent, None);
        assert_eq!(snap.watts, None);
        assert_eq!(snap.cycle_count, None);
        assert_eq!(snap.health_pct, None);
        assert_eq!(snap.time_remaining, TimeRemaining::Unlimited);
    }

    // --- assertions

    fn assertion(kind: &str, name: &str, process: &str) -> Map {
        Map::default()
            .with("AssertType", Val::Text(kind.to_owned()))
            .with("AssertName", Val::Text(name.to_owned()))
            .with("Process Name", Val::Text(process.to_owned()))
            .with("AssertLevel", Val::Int(255))
    }

    #[test]
    fn assertions_parse_like_pmset() {
        let items = [
            assertion(
                "PreventUserIdleSystemSleep",
                "caffeinate command-line tool",
                "caffeinate",
            ),
            assertion("UserIsActive", "Caffeine is Active", "Caffeine"),
            assertion("PreventUserIdleDisplaySleep", "x", "p").with("AssertLevel", Val::Int(0)),
            Map::default().with("AssertName", Val::Text(String::from("no type"))),
            assertion("BackgroundTask", "t", "p").without("Process Name"),
        ];
        let parsed = parse_assertions(57_518, &items);
        assert_eq!(parsed.len(), 3, "released and typeless ones dropped");
        assert_eq!(parsed[0].process, "caffeinate");
        assert_eq!(parsed[0].pid, 57_518);
        assert_eq!(
            parsed[0].name.as_deref(),
            Some("caffeinate command-line tool")
        );
        assert!(parsed[0].prevents_sleep());
        assert!(!parsed[1].prevents_sleep());
        assert_eq!(parsed[2].process, "pid 57518");
    }

    #[test]
    fn awake_list_is_sorted_and_deduplicated() {
        let mut list = parse_assertions(726, &[assertion("B", "n", "sharingd")]);
        list.extend(parse_assertions(358, &[assertion("B", "n", "powerd")]));
        list.extend(parse_assertions(358, &[assertion("A", "n", "powerd")]));
        list.extend(parse_assertions(358, &[assertion("A", "n", "powerd")]));
        tidy_awake(&mut list);
        let keys: Vec<(i32, &str)> = list.iter().map(|a| (a.pid, a.kind.as_str())).collect();
        assert_eq!(keys, [(358, "A"), (358, "B"), (726, "B")]);
    }

    #[test]
    fn empty_assertion_list() {
        assert!(parse_assertions::<Map>(1, &[]).is_empty());
    }

    // --- collector cadence and merging

    #[test]
    fn reads_are_throttled_to_the_period() {
        let mut c = BatteryCollector::new();
        let t0 = Instant::now();
        assert!(c.due(t0));
        assert!(!c.due(t0 + Duration::from_millis(500)));
        assert!(!c.due(t0 + Duration::from_millis(4_900)));
        assert!(c.due(t0 + PERIOD));
        assert!(!c.due(t0 + PERIOD + Duration::from_secs(1)));
    }

    #[test]
    fn a_tick_that_wakes_early_does_not_slip_the_read_a_whole_tick() {
        let mut c = BatteryCollector::new();
        let t0 = Instant::now();
        assert!(c.due(t0 + Duration::from_millis(3)));
        // The 5 s slot's wake came 2 ms earlier than the last one did.
        assert!(c.due(t0 + PERIOD + Duration::from_millis(1)));
    }

    #[test]
    fn collector_keeps_the_value_between_reads_and_merges_sensor_temp() {
        let mut c = BatteryCollector::new();
        let mut snap = Snapshot::empty();
        let t0 = Instant::now();
        let first = c.collect(t0, &mut snap).unwrap();
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(first, Outcome::Absent, "no source off macOS");
            assert!(!snap.battery.present);
        }
        #[cfg(target_os = "macos")]
        assert_eq!(first, Outcome::Fresh);
        snap.battery.present = true;
        snap.battery.percent = Some(42);
        snap.sensors.readings = vec![TempReading {
            name: String::from("battery"),
            celsius: 31.5,
        }];
        let again = c.collect(t0 + Duration::from_secs(1), &mut snap).unwrap();
        assert_eq!(again, first, "same outcome between reads");
        assert_eq!(snap.battery.percent, Some(42), "not re-read inside 5 s");
        assert_eq!(snap.battery.temp_c, Some(31.5));
    }

    #[test]
    fn sensor_temp_needs_a_battery_zone() {
        let mut snap = Snapshot::empty();
        assert_eq!(sensor_temp(&snap), None);
        snap.sensors.readings = vec![TempReading {
            name: String::from("nand"),
            celsius: 40.0,
        }];
        assert_eq!(sensor_temp(&snap), None);
    }

    // --- live checks on this machine

    #[cfg(target_os = "macos")]
    #[test]
    fn live_read_is_consistent() {
        let started = Instant::now();
        let snap = macos::read();
        let took = started.elapsed();
        eprintln!("battery read {took:?}: {snap:#?}");
        assert!(took < Duration::from_millis(500), "{took:?}");
        assert_ne!(snap.source, PowerSource::Unknown);
        if snap.present {
            let pct = snap.percent.expect("a battery reports a percent");
            assert!(pct <= 100);
            assert!(snap.cycle_count.is_some());
            assert!(snap.health_pct.is_some_and(|h| h <= 100));
            assert_ne!(snap.state, ChargeState::Unknown);
        } else {
            assert_eq!(snap.percent, None, "desktop: absent, never zero");
            assert_eq!(snap.watts, None);
        }
        for a in &snap.awake {
            assert!(a.pid >= 0);
            assert!(!a.kind.is_empty());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_cf_fields_walks_dictionaries() {
        let dict = crate::iokit::cf_dict_numbers(&[(c"Watts", 140), (c"Amps", -1000)]).unwrap();
        let fields = macos::CfFields(dict.borrow());
        assert_eq!(fields.int("Watts"), Some(140));
        assert_eq!(fields.int("Amps"), Some(-1000));
        assert_eq!(fields.flag("Watts"), Some(true));
        assert_eq!(fields.text("Watts"), None);
        assert_eq!(fields.int("Missing"), None);
        assert_eq!(fields.int("Watts/Deeper"), None, "a number is not a dict");
    }
}
