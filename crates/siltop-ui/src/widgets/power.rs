//! The `power` panel: whole-system watts with the rails that feed them, and
//! on laptops the battery. Facts are worded here once; the compact title,
//! Glance strip and expanded cells all read them. Absent readings stay
//! absent: no `0W`, no empty line.

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use siltop_core::{
    BatterySnapshot, ChargeState, History, HoldReason, PowerSnapshot, PowerSource, Reading,
    ReadingSource, Scale, SeriesId, Snapshot, TimeRemaining, watts_display,
};

use crate::chrome::{Axis, Graph, GraphInk, render_scaled_graph};
use crate::layout::Panel;
use crate::theme::Theme;
use crate::widgets::{AppView, frame_panel};

/// Graph floor for watts, so an idle Mac's 3 W is not drawn full height.
pub(crate) const WATTS_SCALE: Scale = Scale::Auto { floor: 5.0 };
/// Battery charge zooms to its recent band, so a slow drain has a slope
/// instead of filling the cell solid.
pub(crate) const CHARGE_SCALE: Scale = Scale::Band {
    pad: 0.02,
    min_span: 0.10,
};
/// Below this a rail or battery flow reads as absent rather than `0.0W`.
pub(crate) const MIN_WATTS: f32 = 0.05;

/// The one idle floor every watts token, cell and graph test uses: a
/// finite reading of at least [`MIN_WATTS`] (pass a flow's magnitude).
#[must_use]
pub(crate) fn shows_watts(w: f32) -> bool {
    w.is_finite() && w >= MIN_WATTS
}

/// Does this Mac have anything for the power box to show?
#[must_use]
pub fn has_power(snap: &Snapshot) -> bool {
    snap.battery.present || snap.power.system.is_some()
}

/// A shown power reading: `38W`, or `≈38W` when held or estimated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Watts {
    pub text: String,
    /// Held or estimated: drawn dim.
    pub approx: bool,
}

/// `None` for absent, non-finite, idle or stale readings.
#[must_use]
pub(crate) fn watts(reading: Option<Reading>) -> Option<Watts> {
    let r = reading?;
    if !shows_watts(r.usable()?) {
        return None;
    }
    let approx = !matches!(r.source, ReadingSource::Measured);
    let mut text = watts_display(r.watts);
    if approx {
        text.insert(0, '≈');
    }
    Some(Watts { text, approx })
}

/// `held 4s` for an aging reading; `None` when measured or estimated.
#[must_use]
pub(crate) fn held_age(reading: Option<Reading>) -> Option<String> {
    match reading?.source {
        ReadingSource::Held { age } if age >= Duration::from_secs(1) => {
            Some(format!("held {}s", age.as_secs()))
        }
        _ => None,
    }
}

/// `cpu 18W gpu 4W ane 1.2W dram 0.6W`, only the rails that read.
#[must_use]
pub(crate) fn breakdown(power: &PowerSnapshot) -> Vec<(&'static str, Watts)> {
    [
        ("cpu", power.cpu),
        ("gpu", power.gpu),
        ("ane", power.ane),
        ("dram", power.dram),
    ]
    .into_iter()
    .filter_map(|(name, r)| watts(r).map(|w| (name, w)))
    .collect()
}

/// `3h12m`, `45m`.
#[must_use]
pub(crate) fn duration_short(d: Duration) -> String {
    let mins = d.as_secs() / 60;
    match (mins / 60, mins % 60) {
        (0, m) => format!("{m}m"),
        (h, m) => format!("{h}h{m:02}m"),
    }
}

/// Signed battery flow worth showing, in watts.
#[must_use]
pub(crate) fn battery_flow(bat: &BatterySnapshot) -> Option<f32> {
    bat.watts.filter(|w| shows_watts(w.abs()))
}

/// Where the battery's flow watts have their one home (one fact, one home).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flow {
    /// The summary is the only place the flow shows: `74% ▼ 11W 3h12m`.
    Show,
    /// A system-watts token sits beside the summary. On battery the drain is
    /// that same draw, so only a charging flow still reads: `74% ▼ 3h12m`.
    HideDrain,
    /// Another cell owns the flow (the expanded `in / out` graph).
    Hide,
}

impl Flow {
    /// `Show` unless a system-watts reading is on screen beside the battery.
    #[must_use]
    pub(crate) fn beside_system(system_shown: bool) -> Self {
        if system_shown {
            Self::HideDrain
        } else {
            Self::Show
        }
    }
}

/// `80% held`, `74% ▼ 11W 3h12m`, `62% ▲ 30W`; `None` without a battery.
#[must_use]
pub(crate) fn battery_summary(bat: &BatterySnapshot, shown: Flow) -> Option<String> {
    if !bat.present {
        return None;
    }
    let mut parts: Vec<String> = bat.percent.map(|p| format!("{p}%")).into_iter().collect();
    let flow = battery_flow(bat).map(|w| watts_display(w.abs()));
    match bat.state {
        ChargeState::Discharging => {
            parts.push("▼".into());
            if shown == Flow::Show {
                parts.extend(flow);
            }
            if let TimeRemaining::Known(d) = bat.time_remaining {
                parts.push(duration_short(d));
            }
        }
        ChargeState::Charging => {
            parts.push("▲".into());
            if shown != Flow::Hide {
                parts.extend(flow);
            }
        }
        ChargeState::Held => parts.push("held".into()),
        ChargeState::Full => parts.push("full".into()),
        ChargeState::Unknown => {}
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// `on battery`, `on AC`: where the power comes from, on laptops.
#[must_use]
pub(crate) fn source_words(bat: &BatterySnapshot) -> Option<String> {
    if !bat.present {
        return None;
    }
    match bat.source {
        PowerSource::Battery => Some("on battery".to_owned()),
        other => other.label().map(|l| format!("on {l}")),
    }
}

/// Why a held battery is not charging, in plain English.
#[must_use]
pub(crate) fn hold_sentence(bat: &BatterySnapshot) -> Option<String> {
    Some(match bat.hold_reason()? {
        HoldReason::ChargeLimit => "held at the charge limit to spare the battery".to_owned(),
        HoldReason::Other(code) => format!("macOS is holding the charge (reason {code:#x})"),
    })
}

/// The expanded view's fact lines, most useful first. Every line states a
/// reading; absent facts leave no line.
#[must_use]
pub(crate) fn facts(snap: &Snapshot) -> Vec<String> {
    let bat = &snap.battery;
    let mut lines = Vec::new();
    if bat.present {
        match (bat.state, bat.time_remaining) {
            (ChargeState::Discharging, TimeRemaining::Known(d)) => {
                lines.push(format!("{} left on battery", duration_short(d)));
            }
            (ChargeState::Charging, _) => {
                if let Some(d) = bat.time_to_full {
                    lines.push(format!("full in {}", duration_short(d)));
                }
            }
            _ => {}
        }
        lines.extend(hold_sentence(bat));
        if let Some(h) = bat.health_pct {
            let cycles = match (bat.cycle_count, bat.design_cycle_count) {
                (Some(c), Some(rated)) if rated > 0 => format!(", {c} of {rated} cycles"),
                (Some(c), _) => format!(", {c} cycles"),
                _ => String::new(),
            };
            lines.push(format!("health {h}%{cycles}"));
        }
    }
    if let Some(adapter) = &bat.adapter
        && bat.source == PowerSource::Ac
    {
        let text = match (adapter.watts.filter(|w| *w > 0), &adapter.name) {
            // "140W USB-C Power Adapter" already names its watts.
            (Some(w), Some(name)) if name.contains(&format!("{w}W")) => format!("adapter {name}"),
            (Some(w), Some(name)) => format!("adapter {w}W {name}"),
            (Some(w), None) => format!("adapter {w}W"),
            (None, Some(name)) => format!("adapter {name}"),
            (None, None) => String::new(),
        };
        if !text.is_empty() {
            lines.push(text);
        }
    }
    if bat.low_power_mode == Some(true) {
        lines.push("Low Power Mode on".to_owned());
    }
    let blockers: Vec<&str> = bat
        .sleep_blockers()
        .map(|a| a.process.as_str())
        .filter(|p| !p.is_empty())
        .collect();
    if !blockers.is_empty() {
        let mut names = blockers;
        names.dedup();
        lines.push(format!("kept awake by {}", names.join(", ")));
    }
    lines
}

/// The throttle line for the cpu view, when any throttle channel reads.
#[must_use]
pub(crate) fn throttle_line(power: &PowerSnapshot) -> Option<String> {
    let t = power.throttle;
    if !t.any_above(0.0) {
        return None;
    }
    let parts: Vec<String> = [
        ("thermal", t.gpu_cltm),
        ("power cap", t.gpu_power_limited),
        ("cpu cut", t.cpms_reduction),
    ]
    .into_iter()
    .filter_map(|(name, v)| {
        v.filter(|v| *v > 0.0)
            .map(|v| format!("{name} {:.0}%", v * 100.0))
    })
    .collect();
    Some(format!("throttled: {}", parts.join(", ")))
}

/// The series the compact body draws: system watts, else battery charge.
fn compact_graph<'a>(view: &'a AppView<'_>) -> Option<(&'a History, Scale, Axis)> {
    if !view.history(SeriesId::PowerSystem).is_empty() {
        Some((
            view.history(SeriesId::PowerSystem),
            WATTS_SCALE,
            Axis::Watts,
        ))
    } else if !view.history(SeriesId::BatteryPct).is_empty() {
        Some((
            view.history(SeriesId::BatteryPct),
            CHARGE_SCALE,
            Axis::Percent,
        ))
    } else {
        None
    }
}

pub(crate) fn render(frame: &mut Frame, area: Rect, view: &AppView<'_>, theme: &Theme) {
    let graph = compact_graph(view).map(|(history, scale, axis)| Graph {
        history,
        ramp: theme.fan,
        theme,
        scale,
        axis,
        ink: GraphInk::Flat,
        time: view.time_for(history),
    });
    let inner = frame_panel(frame, area, Panel::Power, view, theme, graph.as_ref());
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if let Some(graph) = graph {
        render_scaled_graph(frame, inner, graph);
        return;
    }
    // Before the first sample lands the body states facts, never blank.
    let mut lines = facts(view.snapshot);
    if lines.is_empty() {
        lines.push("waiting for the first reading".to_owned());
    }
    let text: Vec<Line<'static>> = lines
        .into_iter()
        .map(|l| Line::from(Span::styled(format!(" {l}"), theme.dim())))
        .collect();
    frame.render_widget(Paragraph::new(text), inner);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::widgets::tests_support::fixture;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use siltop_core::{Adapter, AwakeAssertion, NOT_CHARGING_CHARGE_LIMIT, Throttle};

    fn held(watts: f32, secs: u64) -> Reading {
        Reading {
            watts,
            source: ReadingSource::Held {
                age: Duration::from_secs(secs),
            },
        }
    }

    fn laptop_on_ac() -> Snapshot {
        let mut s = Snapshot::empty();
        s.power.system = Some(Reading::measured(38.2));
        s.power.cpu = Some(Reading::measured(18.0));
        s.power.gpu = Some(Reading::measured(4.0));
        s.battery = BatterySnapshot {
            present: true,
            source: PowerSource::Ac,
            percent: Some(80),
            state: ChargeState::Held,
            not_charging_reason: Some(NOT_CHARGING_CHARGE_LIMIT),
            health_pct: Some(92),
            cycle_count: Some(210),
            design_cycle_count: Some(1000),
            adapter: Some(Adapter {
                watts: Some(96),
                name: Some("USB-C".into()),
            }),
            ..BatterySnapshot::default()
        };
        s
    }

    fn laptop_on_battery() -> Snapshot {
        let mut s = Snapshot::empty();
        s.power.system = Some(Reading::measured(11.0));
        s.battery = BatterySnapshot {
            present: true,
            source: PowerSource::Battery,
            percent: Some(74),
            state: ChargeState::Discharging,
            watts: Some(-11.24),
            time_remaining: TimeRemaining::Known(Duration::from_mins(192)),
            low_power_mode: Some(true),
            ..BatterySnapshot::default()
        };
        s
    }

    fn desktop() -> Snapshot {
        let mut s = Snapshot::empty();
        s.power.system = Some(Reading::measured(22.0));
        s
    }

    fn all_text(lines: &[String]) -> String {
        lines.join("\n")
    }

    #[test]
    fn measured_watts_are_plain() {
        let w = watts(Some(Reading::measured(38.2))).unwrap();
        assert!(!w.approx);
        assert!(!w.text.starts_with('≈'));
    }

    #[test]
    fn held_and_estimated_watts_read_approximate() {
        let w = watts(Some(held(9.0, 3))).unwrap();
        assert!(w.approx && w.text.starts_with('≈'), "{w:?}");
        let est = watts(Some(Reading {
            watts: 2.0,
            source: ReadingSource::Estimated,
        }))
        .unwrap();
        assert!(est.approx && est.text.starts_with('≈'));
    }

    #[test]
    fn stale_energy_is_absent_not_a_number() {
        assert_eq!(watts(Some(held(9.0, 11))), None);
    }

    #[test]
    fn idle_and_broken_rails_are_absent_not_zero() {
        for w in [0.0, 0.01, -3.0, f32::NAN, f32::INFINITY] {
            assert_eq!(watts(Some(Reading::measured(w))), None, "{w}");
        }
        assert_eq!(watts(None), None);
    }

    #[test]
    fn held_age_only_for_held_readings() {
        assert_eq!(held_age(Some(held(1.0, 4))).as_deref(), Some("held 4s"));
        assert_eq!(held_age(Some(Reading::measured(1.0))), None);
        assert_eq!(held_age(Some(held(1.0, 0))), None);
        assert_eq!(held_age(None), None);
    }

    #[test]
    fn breakdown_lists_only_reading_rails_in_order() {
        let s = laptop_on_ac();
        let names: Vec<&str> = breakdown(&s.power).iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["cpu", "gpu"]);
        assert!(breakdown(&PowerSnapshot::default()).is_empty());
    }

    #[test]
    fn battery_summary_held_on_ac() {
        assert_eq!(
            battery_summary(&laptop_on_ac().battery, Flow::Show).as_deref(),
            Some("80% held")
        );
    }

    #[test]
    fn battery_summary_discharging_has_flow_and_time() {
        let text = battery_summary(&laptop_on_battery().battery, Flow::Show).unwrap();
        assert!(text.starts_with("74% ▼ "), "{text}");
        assert!(text.ends_with("3h12m"), "{text}");
        assert!(!text.contains('-'), "draw is unsigned: {text}");
    }

    /// Regression: the power title read `6.9W  cpu 1.4W … 79% ▼ 6.9W 8h18m`,
    /// the system draw twice. Beside a system-watts token the drain drops.
    #[test]
    fn battery_summary_beside_system_watts_drops_the_drain() {
        let bat = laptop_on_battery().battery;
        assert_eq!(
            battery_summary(&bat, Flow::HideDrain).as_deref(),
            Some("74% ▼ 3h12m")
        );
        assert_eq!(
            battery_summary(&bat, Flow::Hide).as_deref(),
            Some("74% ▼ 3h12m")
        );
    }

    #[test]
    fn battery_summary_keeps_a_charge_flow_beside_system_watts() {
        let bat = BatterySnapshot {
            present: true,
            percent: Some(62),
            state: ChargeState::Charging,
            watts: Some(30.0),
            ..BatterySnapshot::default()
        };
        // charging watts are a different fact from the system draw
        assert_eq!(
            battery_summary(&bat, Flow::HideDrain).as_deref(),
            Some("62% ▲ 30W")
        );
        // the expanded `in / out` cell owns the flow outright
        assert_eq!(battery_summary(&bat, Flow::Hide).as_deref(), Some("62% ▲"));
    }

    #[test]
    fn flow_beside_system_table() {
        assert_eq!(Flow::beside_system(true), Flow::HideDrain);
        assert_eq!(Flow::beside_system(false), Flow::Show);
    }

    #[test]
    fn battery_summary_absent_on_desktop() {
        assert_eq!(battery_summary(&desktop().battery, Flow::Show), None);
    }

    #[test]
    fn battery_summary_charging_without_flow_has_no_zero_watts() {
        let bat = BatterySnapshot {
            present: true,
            percent: Some(40),
            state: ChargeState::Charging,
            watts: Some(0.0),
            ..BatterySnapshot::default()
        };
        assert_eq!(battery_summary(&bat, Flow::Show).as_deref(), Some("40% ▲"));
    }

    #[test]
    fn source_words_only_on_laptops() {
        assert_eq!(
            source_words(&laptop_on_ac().battery).as_deref(),
            Some("on AC")
        );
        assert_eq!(
            source_words(&laptop_on_battery().battery).as_deref(),
            Some("on battery")
        );
        assert_eq!(source_words(&desktop().battery), None);
    }

    #[test]
    fn duration_short_forms() {
        assert_eq!(duration_short(Duration::from_mins(45)), "45m");
        assert_eq!(duration_short(Duration::from_mins(192)), "3h12m");
        assert_eq!(duration_short(Duration::from_mins(60)), "1h00m");
    }

    #[test]
    fn facts_laptop_on_ac_explain_the_hold() {
        let text = all_text(&facts(&laptop_on_ac()));
        assert!(text.contains("charge limit"), "{text}");
        assert!(text.contains("health 92%, 210 of 1000 cycles"), "{text}");
        assert!(text.contains("adapter 96W USB-C"), "{text}");
        assert!(!text.contains("Low Power"), "{text}");
    }

    #[test]
    fn adapter_watts_are_not_repeated_when_the_name_has_them() {
        // Regression: live screen read "adapter 140W 140W USB-C Power Adapter".
        let mut s = laptop_on_ac();
        s.battery.adapter = Some(Adapter {
            watts: Some(140),
            name: Some("140W USB-C Power Adapter".into()),
        });
        let text = all_text(&facts(&s));
        assert!(text.contains("adapter 140W USB-C Power Adapter"), "{text}");
        assert!(!text.contains("140W 140W"), "{text}");
    }

    #[test]
    fn held_battery_with_no_flow_has_no_empty_in_out_cell() {
        // Regression: a held battery pushed 0 W samples and drew a blank cell.
        let mut fx = expanded(laptop_on_ac());
        *fx.histories.get_mut(SeriesId::BatteryWatts) = History::default();
        for _ in 0..30 {
            fx.histories.get_mut(SeriesId::BatteryWatts).push(0.0);
        }
        let out = screen(&fx, 120, 40);
        assert!(!out.contains("in / out"), "{out}");
    }

    #[test]
    fn facts_on_battery_show_time_left_and_low_power() {
        let text = all_text(&facts(&laptop_on_battery()));
        assert!(text.contains("3h12m left on battery"), "{text}");
        assert!(text.contains("Low Power Mode on"), "{text}");
        assert!(
            !text.contains("adapter"),
            "no adapter line on battery: {text}"
        );
    }

    #[test]
    fn facts_desktop_have_no_battery_lines() {
        assert!(facts(&desktop()).is_empty());
    }

    #[test]
    fn facts_never_contain_empty_lines_or_zero_watts() {
        for snap in [
            laptop_on_ac(),
            laptop_on_battery(),
            desktop(),
            Snapshot::empty(),
        ] {
            for line in facts(&snap) {
                assert!(!line.trim().is_empty());
                assert!(!line.contains(" 0W") && !line.contains("0.0W"), "{line}");
            }
        }
    }

    #[test]
    fn kept_awake_lists_only_real_sleep_blockers() {
        let mut s = laptop_on_ac();
        s.battery.awake = vec![
            AwakeAssertion {
                pid: 1,
                process: "caffeinate".into(),
                kind: "PreventUserIdleSystemSleep".into(),
                name: None,
            },
            AwakeAssertion {
                pid: 2,
                process: "Music".into(),
                kind: "UserIsActive".into(),
                name: None,
            },
        ];
        let text = all_text(&facts(&s));
        assert!(text.contains("kept awake by caffeinate"), "{text}");
        assert!(!text.contains("Music"), "{text}");
    }

    #[test]
    fn throttle_line_absent_when_channels_quiet() {
        assert_eq!(throttle_line(&PowerSnapshot::default()), None);
        let quiet = PowerSnapshot {
            throttle: Throttle {
                gpu_cltm: Some(0.0),
                gpu_power_limited: None,
                cpms_reduction: Some(0.0),
            },
            ..PowerSnapshot::default()
        };
        assert_eq!(throttle_line(&quiet), None);
    }

    #[test]
    fn throttle_line_names_active_channels() {
        let p = PowerSnapshot {
            throttle: Throttle {
                gpu_cltm: None,
                gpu_power_limited: Some(0.25),
                cpms_reduction: Some(0.1),
            },
            ..PowerSnapshot::default()
        };
        let line = throttle_line(&p).unwrap();
        assert_eq!(line, "throttled: power cap 25%, cpu cut 10%");
    }

    #[test]
    fn has_power_gates_desktops_without_readings() {
        assert!(has_power(&laptop_on_ac()));
        assert!(has_power(&desktop()));
        assert!(!has_power(&Snapshot::empty()), "no IOReport, no battery");
    }

    fn paint(fx: &crate::widgets::tests_support::Fixture, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| render(f, f.area(), &fx.view(), &Theme::default()))
            .unwrap();
        let buf = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn compact_title_carries_system_breakdown_and_battery() {
        let mut fx = fixture("");
        fx.ready = true;
        fx.snap = laptop_on_ac();
        let out = paint(&fx, 70, 5);
        let top = out.lines().next().unwrap();
        assert!(top.contains("power"), "{top}");
        assert!(top.contains("38W"), "{top}");
        assert!(top.contains("cpu 18W"), "{top}");
        assert!(top.contains("80% held"), "{top}");
    }

    #[test]
    fn compact_body_without_history_states_facts_not_blank() {
        // Regression: the golden sweep caught an empty power body on a
        // laptop before its first battery sample reached history.
        let mut fx = fixture("");
        fx.snap = laptop_on_battery();
        let out = paint(&fx, 50, 5);
        assert!(out.contains("left on battery"), "{out}");
        fx.snap = desktop();
        let out = paint(&fx, 50, 5);
        assert!(out.contains("waiting for the first reading"), "{out}");
    }

    #[test]
    fn compact_body_draws_system_watts_history() {
        let mut fx = fixture("");
        fx.ready = true;
        fx.snap = desktop();
        for i in 0..40 {
            fx.histories
                .get_mut(SeriesId::PowerSystem)
                .push(10.0 + (i % 7) as f32);
        }
        let out = paint(&fx, 40, 6);
        assert!(!out.contains("waiting"), "{out}");
        let body: String = out.lines().skip(1).take(4).collect();
        assert!(
            body.chars()
                .any(|c| ('\u{2800}'..='\u{28ff}').contains(&c) || "▁▂▃▄▅▆▇█".contains(c)),
            "{out}"
        );
    }

    #[test]
    fn compact_sweep_never_panics_or_prints_zero_watts() {
        for snap in [
            laptop_on_ac(),
            laptop_on_battery(),
            desktop(),
            Snapshot::empty(),
        ] {
            let mut fx = fixture("");
            fx.ready = true;
            fx.snap = snap;
            for w in [0u16, 1, 5, 12, 20, 33, 48, 80, 140] {
                for h in [0u16, 1, 2, 3, 5, 9] {
                    let out = paint(&fx, w, h);
                    assert!(!out.contains(" 0W"), "{w}x{h}: {out}");
                }
            }
        }
    }

    fn screen(fx: &crate::widgets::tests_support::Fixture, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| crate::widgets::render_app(f, &fx.view()))
            .unwrap();
        let buf = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn expanded(snap: Snapshot) -> crate::widgets::tests_support::Fixture {
        let mut fx = fixture("");
        fx.ready = true;
        fx.snap = snap;
        fx.expanded = Some(Panel::Power);
        fx.focus = crate::widgets::Focus::Power;
        for i in 0..60 {
            let t = (i % 9) as f32;
            if fx.snap.power.system.is_some() {
                fx.histories.get_mut(SeriesId::PowerSystem).push(12.0 + t);
            }
            if fx.snap.battery.present {
                fx.histories
                    .get_mut(SeriesId::BatteryPct)
                    .push(0.9 - 0.001 * i as f32);
                fx.histories.get_mut(SeriesId::BatteryWatts).push(8.0 + t);
            }
        }
        fx
    }

    #[test]
    fn expanded_laptop_on_ac_shows_system_battery_and_why_it_is_held() {
        let out = screen(&expanded(laptop_on_ac()), 120, 40);
        for want in [
            "system",
            "battery",
            "details",
            "charge limit",
            "adapter 96W",
        ] {
            assert!(out.contains(want), "missing {want}:\n{out}");
        }
    }

    #[test]
    fn expanded_on_battery_shows_flow_and_time_left() {
        let out = screen(&expanded(laptop_on_battery()), 120, 40);
        assert!(out.contains("in / out"), "{out}");
        assert!(out.contains("▼"), "{out}");
        assert!(out.contains("3h12m left on battery"), "{out}");
    }

    #[test]
    fn expanded_desktop_has_no_battery_cells() {
        let out = screen(&expanded(desktop()), 120, 40);
        assert!(out.contains("system"), "{out}");
        assert!(!out.contains("battery"), "{out}");
        assert!(!out.contains("in / out"), "{out}");
    }

    #[test]
    fn expanded_stale_energy_prints_no_watts() {
        let mut snap = laptop_on_ac();
        snap.power.system = Some(held(30.0, 60));
        snap.power.cpu = Some(held(10.0, 60));
        snap.power.gpu = None;
        let out = screen(&expanded(snap), 120, 40);
        assert!(!out.contains("30W") && !out.contains("10W"), "{out}");
        assert!(out.contains("80% held"), "{out}");
    }

    #[test]
    fn expanded_without_ioreport_still_shows_the_battery() {
        let mut snap = laptop_on_battery();
        snap.power = PowerSnapshot::default();
        let out = screen(&expanded(snap), 100, 30);
        assert!(out.contains("74%"), "{out}");
        assert!(!out.contains(" 0W") && !out.contains("0.0W"), "{out}");
    }

    #[test]
    fn expanded_sweep_never_prints_zero_watts_or_panics() {
        for snap in [laptop_on_ac(), laptop_on_battery(), desktop()] {
            let fx = expanded(snap);
            for (w, h) in [
                (40, 12),
                (60, 16),
                (80, 24),
                (100, 30),
                (120, 40),
                (200, 60),
            ] {
                let out = screen(&fx, w, h);
                assert!(
                    !out.contains(" 0W") && !out.contains("0.0W"),
                    "{w}x{h}:\n{out}"
                );
                assert!(out.contains("power"), "{w}x{h}:\n{out}");
            }
        }
    }

    #[test]
    fn expanded_power_hops_to_heat_and_cpu() {
        let mut fx = expanded(laptop_on_ac());
        fx.snap.sensors.cpu_c = Some(50.0);
        let area = Rect::new(0, 0, 120, 40);
        let view = fx.view();
        let mut seen = Vec::new();
        for row in 0..40 {
            for col in 0..120 {
                if let Some(p) = crate::widgets::hop_hit(area, &view, col, row)
                    && !seen.contains(&p)
                {
                    seen.push(p);
                }
            }
        }
        assert!(seen.contains(&Panel::Cpu), "{seen:?}");
        assert!(seen.contains(&Panel::Fans), "{seen:?}");
    }

    #[test]
    fn cpu_expanded_shows_throttle_line_only_when_throttled() {
        let mut fx = fixture("");
        fx.ready = true;
        fx.expanded = Some(Panel::Cpu);
        let calm = screen(&fx, 140, 40);
        assert!(!calm.contains("throttled"), "{calm}");
        fx.snap.power.throttle.cpms_reduction = Some(0.3);
        let hot = screen(&fx, 140, 40);
        let top = hot
            .lines()
            .find(|l| l.contains("╭ cpu"))
            .unwrap_or_default();
        assert!(top.contains("throttled: cpu cut 30%"), "{hot}");
    }

    #[test]
    fn gpu_expanded_shows_memory_in_use_against_allocation() {
        let mut fx = fixture("");
        fx.ready = true;
        fx.expanded = Some(Panel::Gpu);
        fx.snap.gpu = Some(siltop_core::GpuSnapshot {
            scaled: 0.2,
            mem_in_use_bytes: Some(1_288_490_189),
            mem_alloc_bytes: Some(5_690_831_667),
            ..siltop_core::GpuSnapshot::default()
        });
        let out = screen(&fx, 140, 40);
        assert!(
            out.contains("mem 1.2G of 5.3G") || out.contains("mem 1.2G"),
            "{out}"
        );
        fx.snap.gpu = Some(siltop_core::GpuSnapshot {
            scaled: 0.2,
            ..siltop_core::GpuSnapshot::default()
        });
        let out = screen(&fx, 140, 40);
        assert!(
            !out.contains("mem 0"),
            "absent GPU memory prints nothing: {out}"
        );
    }
}
