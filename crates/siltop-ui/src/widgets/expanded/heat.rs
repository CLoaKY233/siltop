//! Expanded sensors: zone temperatures, fans and extra readings.

use ratatui::Frame;
use siltop_core::{ClusterKind, History, Scale, SeriesId};

use super::cells::{graph_spec, live, paint_graph_cell};
use super::cpu::zone_value;
use super::gpu::gpu_temp_live;
use super::{
    ID_EFF_ZONE, ID_FAN0, ID_GPU_TEMP, ID_PACKAGE, ID_PERF_ZONE, ID_READINGS, ID_SUPER_ZONE,
};
use crate::chrome::{Axis, CellHead, Graph, GraphInk};
use crate::theme::Theme;
use crate::titles::rpm_display;
use crate::widgets::AppView;
use crate::widgets::grid::{Band, CellKind, CellSpec, CellTitle};

pub(super) fn any_cpu_zone(view: &AppView<'_>) -> bool {
    live(view.snapshot.sensors.s_c, view.history(SeriesId::STemp))
        || live(view.snapshot.sensors.p_c, view.history(SeriesId::PTemp))
        || live(view.snapshot.sensors.e_c, view.history(SeriesId::ETemp))
}

pub(super) fn any_sens_graph(view: &AppView<'_>) -> bool {
    any_cpu_zone(view)
        || live(
            view.snapshot.sensors.best_cpu_c(),
            view.history(SeriesId::CpuTemp),
        )
        || gpu_temp_live(view)
}

#[allow(clippy::too_many_lines)]
pub(super) fn sens_bands(view: &AppView<'_>) -> Vec<Band> {
    let mut fans = Vec::new();
    for (i, fan) in view.snapshot.fans.fans.iter().take(4).enumerate() {
        if fan.rpm == 0 && fan.max_rpm == 0 {
            continue;
        }
        fans.push(graph_spec(
            ID_FAN0.saturating_add(i as u8),
            "fan",
            Some(rpm_display(fan.rpm)),
            true,
        ));
        if let Some(cell) = fans.last_mut() {
            cell.title.label = match i {
                0 => "Fan 1",
                1 => "Fan 2",
                2 => "Fan 3",
                _ => "Fan 4",
            };
        }
    }
    vec![
        Band {
            take_leftover: true,
            ..Band::new(
                5,
                vec![
                    graph_spec(
                        ID_SUPER_ZONE,
                        "super zone",
                        zone_value(view, ClusterKind::Super),
                        live(view.snapshot.sensors.s_c, view.history(SeriesId::STemp)),
                    ),
                    graph_spec(
                        ID_PERF_ZONE,
                        "perf zone",
                        zone_value(view, ClusterKind::Performance),
                        live(view.snapshot.sensors.p_c, view.history(SeriesId::PTemp)),
                    ),
                    graph_spec(
                        ID_EFF_ZONE,
                        "eff zone",
                        zone_value(view, ClusterKind::Efficiency),
                        live(view.snapshot.sensors.e_c, view.history(SeriesId::ETemp)),
                    ),
                    graph_spec(
                        ID_PACKAGE,
                        "package",
                        view.snapshot
                            .sensors
                            .best_cpu_c()
                            .map(|c| format!("{c:.0}°")),
                        live(
                            view.snapshot.sensors.best_cpu_c(),
                            view.history(SeriesId::CpuTemp),
                        ),
                    ),
                    graph_spec(
                        ID_GPU_TEMP,
                        "gpu temp",
                        view.snapshot
                            .sensors
                            .gpu_c
                            .or(view.snapshot.gpu.and_then(|g| g.temp_c))
                            .map(|c| format!("{c:.0}°")),
                        live(
                            view.snapshot
                                .sensors
                                .gpu_c
                                .or(view.snapshot.gpu.and_then(|g| g.temp_c)),
                            view.history(SeriesId::GpuTemp),
                        ),
                    ),
                ],
            )
        },
        Band {
            take_leftover: true,
            ..Band::new(5, fans)
        },
        Band::new(
            3,
            vec![CellSpec {
                id: ID_READINGS,
                kind: CellKind::List,
                title: CellTitle {
                    label: "readings",
                    value: None,
                    hop: None,
                },
                min: (16, 3),
                weight: 1,
                present: !extra_readings(view).is_empty(),
            }],
        ),
    ]
}

pub(super) fn paint_fan(
    frame: &mut Frame,
    cell: &crate::widgets::grid::Placed,
    view: &AppView<'_>,
    fan: &siltop_core::FanMetric,
    history: Option<&History>,
    theme: &Theme,
) {
    // Lowercase like every other cell label (`perf zone`, `gpu temp`),
    // whatever case the hardware name arrives in.
    let name = if fan.name.is_empty() {
        String::from("fan")
    } else {
        fan.name.to_lowercase()
    };
    let value = rpm_display(fan.rpm);
    let head = CellHead {
        label: &name,
        value: Some((&value, theme.title())),
        ..CellHead::default()
    };
    let graph = history.and_then(|h| fan_graph(view, h, theme));
    paint_graph_cell(frame, cell.rect, head, graph, view, theme);
}

/// A fan's rpm plot; `None` until its history has a sample.
fn fan_graph<'a>(view: &AppView<'_>, history: &'a History, theme: &'a Theme) -> Option<Graph<'a>> {
    (!history.is_empty()).then(|| Graph {
        history,
        ramp: theme.fan,
        theme,
        scale: Scale::FAN,
        axis: Axis::Number,
        ink: GraphInk::Flat,
        time: view.time_for(history),
    })
}

pub(super) fn peak_fan_index(view: &AppView<'_>) -> Option<usize> {
    if !view.snapshot.fans.is_present() {
        return None;
    }
    view.snapshot
        .fans
        .fans
        .iter()
        .enumerate()
        .min_by(|a, b| b.1.rpm.cmp(&a.1.rpm).then(a.0.cmp(&b.0)))
        .map(|(i, _)| i)
}

pub(super) fn paint_fan_hop(
    frame: &mut Frame,
    cell: &crate::widgets::grid::Placed,
    view: &AppView<'_>,
    theme: &Theme,
) {
    let Some(i) = peak_fan_index(view) else {
        return;
    };
    let Some(fan) = view.snapshot.fans.fans.get(i) else {
        return;
    };
    let value = if view.snapshot.fans.fans.len() >= 2 {
        format!("max {}", rpm_display(fan.rpm))
    } else {
        rpm_display(fan.rpm)
    };
    let head = CellHead {
        label: "fan",
        value: Some((&value, theme.title())),
        hop: cell.hop.is_some(),
        axis: None,
    };
    let graph = view
        .fan_histories()
        .get(i)
        .and_then(|h| fan_graph(view, h, theme));
    paint_graph_cell(frame, cell.rect, head, graph, view, theme);
}

pub(super) fn extra_readings(view: &AppView<'_>) -> Vec<(String, f32)> {
    view.snapshot
        .sensors
        .readings
        .iter()
        .filter(|r| {
            !matches!(
                r.name.as_str(),
                "cpu" | "gpu" | "efficiency" | "performance" | "super"
            )
        })
        .map(|r| (r.name.clone(), r.celsius))
        .collect()
}
