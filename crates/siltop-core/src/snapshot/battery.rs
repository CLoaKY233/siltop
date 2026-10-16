use std::time::Duration;

/// `NotChargingReason` bit seen while macOS holds the charge at the
/// user's (or Optimized Charging's) limit, e.g. 80% on Mac17,8.
pub const NOT_CHARGING_CHARGE_LIMIT: u64 = 0x0100_0000;

/// Where the Mac is drawing power from right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PowerSource {
    /// Not reported (no power-source API on this platform).
    #[default]
    Unknown,
    Ac,
    Battery,
    Ups,
}

impl PowerSource {
    /// Parses the `IOPSGetProvidingPowerSourceType` / `Power Source State`
    /// strings.
    #[must_use]
    pub fn from_iops(text: &str) -> Self {
        match text {
            "AC Power" => Self::Ac,
            "Battery Power" => Self::Battery,
            "UPS Power" => Self::Ups,
            _ => Self::Unknown,
        }
    }

    #[must_use]
    pub const fn label(self) -> Option<&'static str> {
        match self {
            Self::Unknown => None,
            Self::Ac => Some("AC"),
            Self::Battery => Some("battery"),
            Self::Ups => Some("UPS"),
        }
    }
}

/// What the battery is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChargeState {
    /// Not known (no battery, or not reported yet).
    #[default]
    Unknown,
    Charging,
    Discharging,
    /// On external power and fully charged.
    Full,
    /// On external power, neither charging nor full: macOS is holding the
    /// charge (charge limit, Optimized Charging, heat). See
    /// [`BatterySnapshot::not_charging_reason`].
    Held,
}

impl ChargeState {
    /// Derives the state from the three `AppleSmartBattery` flags.
    #[must_use]
    pub const fn derive(external: bool, charging: bool, fully_charged: bool) -> Self {
        match (external, charging, fully_charged) {
            (_, true, _) => Self::Charging,
            (false, false, _) => Self::Discharging,
            (true, false, true) => Self::Full,
            (true, false, false) => Self::Held,
        }
    }

    #[must_use]
    pub const fn word(self) -> Option<&'static str> {
        match self {
            Self::Unknown => None,
            Self::Charging => Some("charging"),
            Self::Discharging => Some("discharging"),
            Self::Full => Some("charged"),
            Self::Held => Some("held"),
        }
    }
}

/// Why a held battery is not charging, decoded from `NotChargingReason`.
/// The table grows as evidence comes in; unknown codes stay raw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldReason {
    /// Held at the charge limit (`0x0100_0000`).
    ChargeLimit,
    /// A reason code we have not decoded yet.
    Other(u64),
}

impl HoldReason {
    /// `None` for 0, which means "nothing is stopping the charge".
    #[must_use]
    pub const fn decode(code: u64) -> Option<Self> {
        match code {
            0 => None,
            NOT_CHARGING_CHARGE_LIMIT => Some(Self::ChargeLimit),
            other => Some(Self::Other(other)),
        }
    }
}

/// How long the battery will last, from `IOPSGetTimeRemainingEstimate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeRemaining {
    /// Still being calculated, or not reported (-1).
    #[default]
    Unknown,
    /// On external power (-2).
    Unlimited,
    Known(Duration),
}

impl TimeRemaining {
    /// Maps the raw estimate in seconds: -1 unknown, -2 unlimited; other
    /// negatives and non-finite values are unknown.
    #[must_use]
    pub fn from_estimate(seconds: f64) -> Self {
        if !seconds.is_finite() {
            return Self::Unknown;
        }
        if (seconds - -2.0).abs() < f64::EPSILON {
            return Self::Unlimited;
        }
        if seconds < 0.0 {
            return Self::Unknown;
        }
        Self::Known(Duration::from_secs_f64(seconds.min(1.0e9)))
    }
}

/// The power adapter, from `AdapterDetails`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Adapter {
    pub watts: Option<u32>,
    pub name: Option<String>,
}

/// One process holding a power assertion (`IOPMCopyAssertionsByProcess`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwakeAssertion {
    pub pid: i32,
    pub process: String,
    /// The assertion type, e.g. `PreventUserIdleSystemSleep`.
    pub kind: String,
    /// The assertion's own name, e.g. `caffeinate command-line tool`.
    pub name: Option<String>,
}

impl AwakeAssertion {
    /// Whether this assertion stops the Mac (or its display) from sleeping,
    /// rather than only reporting user activity or a background task.
    #[must_use]
    pub fn prevents_sleep(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "PreventUserIdleSystemSleep"
                | "PreventSystemSleep"
                | "PreventUserIdleDisplaySleep"
                | "NoIdleSleepAssertion"
                | "NoDisplaySleepAssertion"
                | "PreventDiskIdle"
        )
    }
}

/// Battery and power source. On a desktop `present` is false and every
/// battery field is `None`; the power source and awake list still apply.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BatterySnapshot {
    /// A battery is installed.
    pub present: bool,
    pub source: PowerSource,
    /// State of charge, 0–100 (what `pmset -g batt` prints).
    pub percent: Option<u8>,
    pub state: ChargeState,
    pub external_connected: Option<bool>,
    pub is_charging: Option<bool>,
    pub fully_charged: Option<bool>,
    pub voltage_mv: Option<u32>,
    /// Positive while charging, negative while discharging.
    pub amperage_ma: Option<i64>,
    /// Battery power: positive in, negative out.
    pub watts: Option<f32>,
    pub cycle_count: Option<u32>,
    pub design_cycle_count: Option<u32>,
    pub design_capacity_mah: Option<u32>,
    pub nominal_capacity_mah: Option<u32>,
    pub full_charge_capacity_mah: Option<u32>,
    /// `min(100, round(nominal / design × 100))`, `system_profiler`'s
    /// "Maximum Capacity".
    pub health_pct: Option<u8>,
    pub time_to_empty: Option<Duration>,
    pub time_to_full: Option<Duration>,
    pub time_remaining: TimeRemaining,
    pub adapter: Option<Adapter>,
    /// Raw `ChargerData.NotChargingReason` (0 = not held).
    pub not_charging_reason: Option<u64>,
    /// Raw `ChargerData.TimeChargingThermallyLimited`.
    pub thermally_limited: Option<u64>,
    pub low_power_mode: Option<bool>,
    /// From the heat collector's battery zone (TB0T–TB2T).
    pub temp_c: Option<f32>,
    /// Processes holding power assertions, in pid order.
    pub awake: Vec<AwakeAssertion>,
}

impl BatterySnapshot {
    /// The decoded hold reason, only while the battery is held.
    #[must_use]
    pub fn hold_reason(&self) -> Option<HoldReason> {
        if self.state != ChargeState::Held {
            return None;
        }
        self.not_charging_reason.and_then(HoldReason::decode)
    }

    /// Assertions that actually keep the Mac or its display awake.
    pub fn sleep_blockers(&self) -> impl Iterator<Item = &AwakeAssertion> {
        self.awake.iter().filter(|a| a.prevents_sleep())
    }
}

/// `min(100, round(nominal / design × 100))`; `None` without a design
/// capacity.
#[must_use]
pub fn health_percent(nominal_mah: u32, design_mah: u32) -> Option<u8> {
    if design_mah == 0 {
        return None;
    }
    let pct = (u64::from(nominal_mah) * 200 + u64::from(design_mah)) / (u64::from(design_mah) * 2);
    Some(u8::try_from(pct.min(100)).unwrap_or(100))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn charge_state_truth_table() {
        use ChargeState::{Charging, Discharging, Full, Held};
        let cases = [
            ((false, false, false), Discharging),
            ((false, false, true), Discharging),
            ((false, true, false), Charging),
            ((false, true, true), Charging),
            ((true, false, false), Held),
            ((true, false, true), Full),
            ((true, true, false), Charging),
            ((true, true, true), Charging),
        ];
        for ((ext, chg, full), want) in cases {
            assert_eq!(
                ChargeState::derive(ext, chg, full),
                want,
                "{ext} {chg} {full}"
            );
        }
    }

    #[test]
    fn hold_reason_only_while_held() {
        let mut snap = BatterySnapshot {
            state: ChargeState::Held,
            not_charging_reason: Some(NOT_CHARGING_CHARGE_LIMIT),
            ..BatterySnapshot::default()
        };
        assert_eq!(snap.hold_reason(), Some(HoldReason::ChargeLimit));
        snap.not_charging_reason = Some(0x20);
        assert_eq!(snap.hold_reason(), Some(HoldReason::Other(0x20)));
        snap.not_charging_reason = Some(0);
        assert_eq!(snap.hold_reason(), None);
        snap.not_charging_reason = Some(NOT_CHARGING_CHARGE_LIMIT);
        snap.state = ChargeState::Charging;
        assert_eq!(snap.hold_reason(), None, "a charging battery is not held");
    }

    #[test]
    fn time_remaining_sentinels() {
        assert_eq!(TimeRemaining::from_estimate(-1.0), TimeRemaining::Unknown);
        assert_eq!(TimeRemaining::from_estimate(-2.0), TimeRemaining::Unlimited);
        assert_eq!(TimeRemaining::from_estimate(-3.0), TimeRemaining::Unknown);
        assert_eq!(
            TimeRemaining::from_estimate(f64::NAN),
            TimeRemaining::Unknown
        );
        assert_eq!(
            TimeRemaining::from_estimate(f64::INFINITY),
            TimeRemaining::Unknown
        );
        assert_eq!(
            TimeRemaining::from_estimate(0.0),
            TimeRemaining::Known(Duration::ZERO)
        );
        assert_eq!(
            TimeRemaining::from_estimate(11_520.0),
            TimeRemaining::Known(Duration::from_mins(192))
        );
        assert!(matches!(
            TimeRemaining::from_estimate(1.0e300),
            TimeRemaining::Known(_)
        ));
    }

    #[test]
    fn health_rounds_half_up_and_caps_at_100() {
        // The test machine: nominal 8731 over design 8579 is 101.8%.
        assert_eq!(health_percent(8731, 8579), Some(100));
        assert_eq!(health_percent(8579, 8579), Some(100));
        assert_eq!(health_percent(0, 8579), Some(0));
        assert_eq!(health_percent(100, 0), None);
        assert_eq!(health_percent(895, 1000), Some(90), "89.5 rounds up");
        assert_eq!(health_percent(894, 1000), Some(89));
        assert_eq!(health_percent(u32::MAX, 1), Some(100), "no overflow");
        assert_eq!(health_percent(u32::MAX, u32::MAX), Some(100));
    }

    #[test]
    fn health_sweep_matches_float_rounding() {
        for design in (1000..=9000).step_by(997) {
            for nominal in (0..=design * 11 / 10).step_by(13) {
                let got = health_percent(nominal, design).unwrap();
                let want = (f64::from(nominal) / f64::from(design) * 100.0)
                    .round()
                    .min(100.0);
                assert!(got <= 100);
                // Exact halves may differ from the float by representation.
                assert!(
                    (f64::from(got) - want).abs() <= 1.0,
                    "{nominal}/{design}: {got} vs {want}"
                );
            }
        }
    }

    #[test]
    fn power_source_strings() {
        assert_eq!(PowerSource::from_iops("AC Power"), PowerSource::Ac);
        assert_eq!(
            PowerSource::from_iops("Battery Power"),
            PowerSource::Battery
        );
        assert_eq!(PowerSource::from_iops("UPS Power"), PowerSource::Ups);
        assert_eq!(PowerSource::from_iops(""), PowerSource::Unknown);
        assert_eq!(PowerSource::from_iops("ac power"), PowerSource::Unknown);
        assert_eq!(PowerSource::Unknown.label(), None);
        assert_eq!(PowerSource::Ac.label(), Some("AC"));
    }

    #[test]
    fn desktop_default_reports_nothing() {
        let snap = BatterySnapshot::default();
        assert!(!snap.present);
        assert_eq!(snap.percent, None);
        assert_eq!(snap.watts, None);
        assert_eq!(snap.health_pct, None);
        assert_eq!(snap.state.word(), None);
        assert_eq!(snap.source.label(), None);
        assert_eq!(snap.hold_reason(), None);
        assert_eq!(snap.sleep_blockers().count(), 0);
    }

    #[test]
    fn sleep_blockers_skip_activity_only_assertions() {
        let a = |kind: &str| AwakeAssertion {
            pid: 1,
            process: String::from("p"),
            kind: kind.to_owned(),
            name: None,
        };
        let snap = BatterySnapshot {
            awake: vec![
                a("PreventUserIdleSystemSleep"),
                a("UserIsActive"),
                a("PreventUserIdleDisplaySleep"),
                a("BackgroundTask"),
                a("PreventSystemSleep"),
            ],
            ..BatterySnapshot::default()
        };
        let kinds: Vec<&str> = snap.sleep_blockers().map(|a| a.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "PreventUserIdleSystemSleep",
                "PreventUserIdleDisplaySleep",
                "PreventSystemSleep"
            ]
        );
    }
}
