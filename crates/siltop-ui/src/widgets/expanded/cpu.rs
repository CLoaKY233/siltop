//! Expanded CPU: load, cluster and per-core cells.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use siltop_core::{ClusterKind, CoreSample, SeriesId, percent_display, ready_pct};

use super::cells::{graph_spec, live, overflow_rows, paint_more, usage_spec};
use super::heat::any_cpu_zone;
use super::{
    ID_CPU, ID_EFF_LOAD, ID_EFF_ZONE, ID_PACKAGE, ID_PERF_LOAD, ID_PERF_ZONE, ID_SUPER_LOAD,
    ID_SUPER_ZONE,
};
use crate::chrome::{bar_label, render_fill_bar};
use crate::theme::Theme;
use crate::widgets::AppView;
use crate::widgets::grid::Band;

pub(super) fn cpu_has_spark_hops(view: &AppView<'_>) -> bool {
    (view.show_gpu && view.snapshot.gpu.is_some())
        || (view.show_fans && view.snapshot.fans.is_present())
}

pub(super) fn has_any_cluster(view: &AppView<'_>) -> bool {
    has_cluster(view, ClusterKind::Super)
        || has_cluster(view, ClusterKind::Performance)
        || has_cluster(view, ClusterKind::Efficiency)
}

#[allow(clippy::too_many_lines)]
pub(super) fn cpu_bands(view: &AppView<'_>) -> Vec<Band> {
    let s = has_cluster(view, ClusterKind::Super);
    let p = has_cluster(view, ClusterKind::Performance);
    let e = has_cluster(view, ClusterKind::Efficiency);
    let fallback = !s && !p && !e;
    vec![
        Band {
            take_leftover: true,
            ..Band::new(
                4,
                vec![
                    usage_spec(
                        ID_CPU,
                        "cpu",
                        Some(ready_pct(view.ready, view.snapshot.cpu.scaled)),
                        fallback,
                    ),
                    usage_spec(
                        ID_SUPER_LOAD,
                        "super",
                        Some(cluster_value(view, ClusterKind::Super)),
                        s,
                    ),
                    usage_spec(
                        ID_PERF_LOAD,
                        "performance",
                        Some(cluster_value(view, ClusterKind::Performance)),
                        p,
                    ),
                    usage_spec(
                        ID_EFF_LOAD,
                        "efficiency",
                        Some(cluster_value(view, ClusterKind::Efficiency)),
                        e,
                    ),
                ],
            )
        },
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
                            .cpu
                            .temp_c
                            .or(view.snapshot.sensors.best_cpu_c())
                            .map(|c| format!("{c:.0}°")),
                        !any_cpu_zone(view)
                            && live(
                                view.snapshot
                                    .cpu
                                    .temp_c
                                    .or(view.snapshot.sensors.best_cpu_c()),
                                view.history(SeriesId::CpuTemp),
                            ),
                    ),
                ],
            )
        },
    ]
}

pub(super) fn paint_cores(frame: &mut Frame, area: Rect, view: &AppView<'_>, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let mut cores: Vec<&CoreSample> = view.snapshot.cpu.cores.iter().collect();
    cores.sort_by_key(|c| (core_kind_rank(c.kind), c.index));
    let (shown, hidden) = overflow_rows(cores.len(), area.height, 1);
    let marker_y = area
        .y
        .saturating_add(u16::try_from(shown).unwrap_or(u16::MAX));
    paint_more(frame, area.x, marker_y, area.width, hidden, theme);
    // Fixed-width labels: every bar starts in one column, `P9` or `P10`.
    let ink = theme.stain(theme.cpu, 1.0, view.snapshot.thermal).fg;
    for (core, y) in cores.into_iter().take(shown).zip(area.y..) {
        let label = bar_label(&format!("{}{}", core.kind.tag(), core.index), core.scaled);
        let label_w = u16::try_from(label.chars().count())
            .unwrap_or(0)
            .min(area.width);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                label,
                theme.stain(theme.cpu, core.scaled, view.snapshot.thermal),
            ))),
            Rect {
                x: area.x,
                y,
                width: label_w,
                height: 1,
            },
        );
        let bar_x = area.x.saturating_add(label_w.saturating_add(1));
        let bar_w = area.x.saturating_add(area.width).saturating_sub(bar_x);
        if bar_w > 0 {
            render_fill_bar(
                frame,
                Rect {
                    x: bar_x,
                    y,
                    width: bar_w,
                    height: 1,
                },
                core.scaled,
                ink.unwrap_or(theme.cpu.body),
                theme,
            );
        }
    }
}

pub(super) fn core_kind_rank(kind: ClusterKind) -> u8 {
    match kind {
        ClusterKind::Super => 0,
        ClusterKind::Performance => 1,
        ClusterKind::Efficiency => 2,
    }
}

pub(super) fn has_cluster(view: &AppView<'_>, kind: ClusterKind) -> bool {
    let named = match kind {
        ClusterKind::Efficiency => view.snapshot.cpu.e_cluster.is_some(),
        ClusterKind::Performance => view.snapshot.cpu.p_cluster.is_some(),
        ClusterKind::Super => view.snapshot.cpu.s_cluster.is_some(),
    };
    named || view.snapshot.cpu.cores.iter().any(|c| c.kind == kind)
}

/// `42% 4.3G`: the cluster's load and, once `IOReport` has one, its clock.
pub(super) fn cluster_value(view: &AppView<'_>, kind: ClusterKind) -> String {
    let load = percent_display(cluster_load(view, kind));
    let cpu = &view.snapshot.cpu;
    let summary = match kind {
        ClusterKind::Efficiency => cpu.e_cluster.as_ref(),
        ClusterKind::Performance => cpu.p_cluster.as_ref(),
        ClusterKind::Super => cpu.s_cluster.as_ref(),
    };
    match summary.map(|c| c.freq_mhz).filter(|mhz| *mhz > 0) {
        Some(mhz) if view.ready => format!("{load} {}", crate::titles::ghz_display(mhz)),
        _ => load,
    }
}

pub(super) fn cluster_load(view: &AppView<'_>, kind: ClusterKind) -> f32 {
    let from = match kind {
        ClusterKind::Efficiency => view.snapshot.cpu.e_cluster.as_ref().map(|c| c.scaled),
        ClusterKind::Performance => view.snapshot.cpu.p_cluster.as_ref().map(|c| c.scaled),
        ClusterKind::Super => view.snapshot.cpu.s_cluster.as_ref().map(|c| c.scaled),
    };
    if let Some(load) = from {
        return load;
    }
    let mut sum = 0.0;
    let mut n = 0_u32;
    for core in &view.snapshot.cpu.cores {
        if core.kind == kind {
            sum += core.scaled;
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { sum / n as f32 }
}

pub(super) fn zone_value(view: &AppView<'_>, kind: ClusterKind) -> Option<String> {
    view.snapshot
        .sensors
        .zone_temp(kind)
        .or_else(|| view.zone_temp_history(kind).last())
        .map(|c| format!("{c:.0}°"))
}
