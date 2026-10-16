//! Expanded power: system watts, battery and facts cells.

use siltop_core::{SeriesId, watts_display};

use super::cells::graph_spec;
use super::{ID_BAT_PCT, ID_BAT_WATTS, ID_POWER_FACTS, ID_POWER_SYS};
use crate::widgets::AppView;
use crate::widgets::grid::{Band, CellKind, CellSpec, CellTitle};

pub(super) fn power_bands(view: &AppView<'_>) -> Vec<Band> {
    use crate::widgets::power as p;
    let snap = view.snapshot;
    let bat = &snap.battery;
    let flow = p::battery_flow(bat);
    let facts = p::facts(snap);
    // Every fact on its own row inside the cell's frame, up to six.
    let facts_rows = u16::try_from(facts.len().min(6)).unwrap_or(6) + 2;
    vec![
        Band {
            take_leftover: true,
            ..Band::new(
                5,
                vec![graph_spec(
                    ID_POWER_SYS,
                    "system",
                    power_sys_value(view),
                    snap.power.system.is_some() || !view.history(SeriesId::PowerSystem).is_empty(),
                )],
            )
        },
        Band {
            take_leftover: true,
            ..Band::new(
                5,
                vec![
                    graph_spec(
                        ID_BAT_PCT,
                        "battery",
                        p::battery_summary(bat, p::Flow::Hide),
                        bat.present
                            && (bat.percent.is_some()
                                || !view.history(SeriesId::BatteryPct).is_empty()),
                    ),
                    graph_spec(
                        ID_BAT_WATTS,
                        "in / out",
                        flow.map(|w| {
                            let arrow = if w > 0.0 { "▲" } else { "▼" };
                            format!("{arrow} {}", watts_display(w.abs()))
                        }),
                        // A held battery records 0 W: no flow, no cell.
                        bat.present
                            && (flow.is_some()
                                || view
                                    .history(SeriesId::BatteryWatts)
                                    .max()
                                    .is_some_and(p::shows_watts)),
                    ),
                ],
            )
        },
        Band::new(
            facts_rows,
            vec![CellSpec {
                id: ID_POWER_FACTS,
                kind: CellKind::List,
                title: CellTitle {
                    label: "details",
                    value: None,
                    hop: None,
                },
                min: (16, facts_rows),
                weight: 1,
                present: !facts.is_empty(),
            }],
        ),
    ]
}

/// `38W  cpu 18W  gpu 4W  ane 1W`: the system cell names its rails.
pub(super) fn power_sys_value(view: &AppView<'_>) -> Option<String> {
    use crate::widgets::power as p;
    let system = p::watts(view.snapshot.power.system)?;
    let mut parts = vec![system.text];
    parts.extend(
        p::breakdown(&view.snapshot.power)
            .into_iter()
            .map(|(name, w)| format!("{name} {}", w.text)),
    );
    Some(parts.join("  "))
}
