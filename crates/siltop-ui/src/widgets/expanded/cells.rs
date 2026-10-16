//! Cell builders and painters shared by every expanded panel.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use siltop_core::{ClusterKind, History, Scale, byte_rate, net_rate, percent_display, ready_pct};

use super::cpu::cluster_value;
use super::gpu::gpu_load_value;
use super::mem::mem_value;
use super::power::power_sys_value;
use super::{
    ID_BAT_PCT, ID_BAT_WATTS, ID_CPU, ID_DISK_READ, ID_DISK_WRITE, ID_EFF_LOAD, ID_EFF_ZONE,
    ID_FAN0, ID_GPU_TEMP, ID_GPU_UTIL, ID_HOP_CPU, ID_HOP_DISK, ID_HOP_FAN, ID_HOP_GPU, ID_HOP_NET,
    ID_HOP_POWER, ID_HOP_SENS, ID_MEM, ID_MEM_PARTS, ID_NET_DOWN, ID_NET_UP, ID_PACKAGE,
    ID_PERF_LOAD, ID_PERF_ZONE, ID_POWER_FACTS, ID_POWER_SYS, ID_READINGS, ID_SUPER_LOAD,
    ID_SUPER_ZONE, ID_SWAP, ID_VOLUMES,
};
use crate::chrome::{
    Axis, BorderScale, CellHead, Graph, GraphInk, cell_headed, cell_titled, render_scaled_graph,
};
use crate::theme::Theme;
use crate::widgets::AppView;
use crate::widgets::grid::{CellKind, CellSpec, CellTitle};

pub(super) fn live(opt: Option<f32>, history: &History) -> bool {
    opt.is_some() || !history.is_empty()
}

pub(super) fn usage_spec(
    id: u8,
    label: &'static str,
    value: Option<String>,
    present: bool,
) -> CellSpec {
    CellSpec {
        min: (16, 4),
        ..graph_spec(id, label, value, present)
    }
}

pub(super) fn graph_spec(
    id: u8,
    label: &'static str,
    value: Option<String>,
    present: bool,
) -> CellSpec {
    CellSpec {
        id,
        kind: CellKind::Graph,
        title: CellTitle {
            label,
            value,
            hop: None,
        },
        min: (16, 5),
        weight: 1,
        present,
    }
}

pub(super) fn cluster_spec(
    id: u8,
    label: &'static str,
    value: Option<String>,
    present: bool,
) -> CellSpec {
    CellSpec {
        kind: CellKind::Cluster,
        min: (14, 5),
        ..graph_spec(id, label, value, present)
    }
}

pub(super) fn paint_dim_line(
    frame: &mut Frame,
    x: u16,
    y: u16,
    width: u16,
    text: String,
    theme: &Theme,
) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(text, theme.dim()))),
        Rect {
            x,
            y,
            width,
            height: 1,
        },
    );
}

/// `(shown, hidden)` items of `per_item` rows each in a list `rows` tall:
/// when they do not all fit, the last row becomes a `+N more` marker so
/// none vanish silently.
pub(super) fn overflow_rows(items: usize, rows: u16, per_item: u16) -> (usize, usize) {
    let rows = usize::from(rows);
    let per_item = usize::from(per_item.max(1));
    if items.saturating_mul(per_item) <= rows {
        (items, 0)
    } else {
        let shown = rows.saturating_sub(1) / per_item;
        (shown, items - shown)
    }
}

/// The dim `+N more` line under a list that ran out of rows.
pub(super) fn paint_more(
    frame: &mut Frame,
    x: u16,
    y: u16,
    width: u16,
    hidden: usize,
    theme: &Theme,
) {
    if hidden > 0 {
        paint_dim_line(frame, x, y, width, format!("+{hidden} more"), theme);
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn paint_series(
    frame: &mut Frame,
    cell: &crate::widgets::grid::Placed,
    view: &AppView<'_>,
    history: &History,
    ramp: crate::theme::Ramp,
    ink: GraphInk,
    scale: Scale,
    axis: Axis,
    theme: &Theme,
) {
    let (label, value) = series_title(cell.id, view);
    let head = CellHead {
        label,
        value: value.as_deref().map(|v| (v, theme.title())),
        hop: cell.hop.is_some(),
        axis: None,
    };
    let graph = Graph {
        history,
        ramp,
        theme,
        scale,
        axis,
        ink,
        time: view.time_for(history),
    };
    paint_graph_cell(frame, cell.rect, head, Some(graph), view, theme);
}

/// A cell holding one graph, its scale (`10%`, `70–90%`, `2MB/s`) at the
/// right end of the top border as compact panels print theirs, so a small
/// value never reads as a big one; zen drops it there too. `None` paints
/// the cell with no plot.
pub(super) fn paint_graph_cell(
    frame: &mut Frame,
    rect: Rect,
    mut head: CellHead<'_>,
    graph: Option<Graph<'_>>,
    view: &AppView<'_>,
    theme: &Theme,
) {
    // The plot fills the cell's inside, so its scale is the one for that width.
    head.axis = graph
        .filter(|_| !view.config.zen)
        .and_then(|g| g.border_label(rect.width.saturating_sub(2)));
    let inner = cell_headed(frame, rect, head, theme);
    if let Some(graph) = graph {
        render_scaled_graph(frame, inner, graph);
    }
}

pub(super) fn series_title(id: u8, view: &AppView<'_>) -> (&'static str, Option<String>) {
    let label = label_for(id);
    let value = match id {
        ID_CPU => Some(ready_pct(view.ready, view.snapshot.cpu.scaled)),
        ID_GPU_UTIL => view.snapshot.gpu.map(|g| gpu_load_value(view, g)),
        ID_SUPER_LOAD => Some(cluster_value(view, ClusterKind::Super)),
        ID_PERF_LOAD => Some(cluster_value(view, ClusterKind::Performance)),
        ID_EFF_LOAD => Some(cluster_value(view, ClusterKind::Efficiency)),
        ID_HOP_CPU => Some(percent_display(view.snapshot.cpu.scaled)),
        ID_HOP_GPU => view.snapshot.gpu.map(|g| percent_display(g.scaled)),
        ID_NET_DOWN => Some(net_rate(view.snapshot.network.rx_bps, view.net_unit)),
        ID_NET_UP => Some(net_rate(view.snapshot.network.tx_bps, view.net_unit)),
        ID_DISK_READ => Some(byte_rate(view.snapshot.disk.read_bps)),
        ID_HOP_DISK => Some(byte_rate(
            view.snapshot
                .disk
                .read_bps
                .saturating_add(view.snapshot.disk.write_bps),
        )),
        ID_DISK_WRITE => Some(byte_rate(view.snapshot.disk.write_bps)),
        ID_POWER_SYS | ID_HOP_POWER => power_sys_value(view),
        ID_BAT_PCT => {
            use crate::widgets::power::{Flow, battery_summary};
            battery_summary(&view.snapshot.battery, Flow::Hide)
        }
        ID_MEM => Some(mem_value(view)),
        _ => None,
    };
    (label, value)
}

pub(super) fn label_for(id: u8) -> &'static str {
    match id {
        ID_CPU | ID_HOP_CPU => "cpu",
        ID_GPU_UTIL | ID_HOP_GPU => "gpu",
        ID_SUPER_LOAD => "super",
        ID_PERF_LOAD => "performance",
        ID_EFF_LOAD => "efficiency",
        ID_SUPER_ZONE => "super zone",
        ID_PERF_ZONE => "perf zone",
        ID_EFF_ZONE => "eff zone",
        ID_PACKAGE => "package",
        ID_GPU_TEMP => "gpu temp",
        ID_HOP_SENS => "heat",
        ID_HOP_FAN => "fan",
        ID_HOP_DISK => "disk",
        ID_POWER_SYS => "system",
        ID_HOP_POWER => "power",
        ID_BAT_PCT => "battery",
        ID_BAT_WATTS => "in / out",
        ID_POWER_FACTS => "details",
        ID_HOP_NET => "net",
        ID_MEM => "memory",
        ID_NET_DOWN => "down",
        ID_NET_UP => "up",
        ID_DISK_READ => "read",
        ID_DISK_WRITE => "write",
        ID_VOLUMES => "volumes",
        ID_READINGS => "readings",
        ID_FAN0 => "Fan 1",
        x if x == ID_FAN0 + 1 => "Fan 2",
        x if x == ID_FAN0 + 2 => "Fan 3",
        x if x == ID_FAN0 + 3 => "Fan 4",
        ID_MEM_PARTS => "composition",
        ID_SWAP => "swap",
        _ => "",
    }
}

pub(super) fn paint_temp(
    frame: &mut Frame,
    cell: &crate::widgets::grid::Placed,
    view: &AppView<'_>,
    history: &History,
    theme: &Theme,
) {
    let label = label_for(cell.id);
    let last = history.last();
    let value = last.map(|c| format!("{c:.0}°"));
    // One reading, one colour: the heat ink the compact title uses (so a
    // crit-red 95° stays red here), bold like every other cell value.
    let ink = last.map(|c| theme.temp(c).add_modifier(Modifier::BOLD));
    let head = CellHead {
        label,
        value: value.as_deref().zip(ink),
        hop: cell.hop.is_some(),
        axis: None,
    };
    let graph = Graph {
        history,
        ramp: theme.temp,
        theme,
        scale: Scale::TEMP,
        axis: Axis::Celsius,
        ink: GraphInk::Flat,
        time: view.time_for(history),
    };
    paint_graph_cell(frame, cell.rect, head, Some(graph), view, theme);
}

/// A titled cell of dim fact lines, with `+N more` when they overflow.
pub(super) fn paint_lines(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    lines: &[String],
    theme: &Theme,
) {
    let inner = cell_titled(frame, area, label, None, false, theme);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let (shown, hidden) = overflow_rows(lines.len(), inner.height, 1);
    for (line, y) in lines.iter().take(shown).zip(inner.y..) {
        paint_dim_line(frame, inner.x, y, inner.width, format!(" {line}"), theme);
    }
    let marker_y = inner
        .y
        .saturating_add(u16::try_from(shown).unwrap_or(u16::MAX));
    paint_more(frame, inner.x, marker_y, inner.width, hidden, theme);
}

pub(super) fn paint_readings(
    frame: &mut Frame,
    area: Rect,
    extras: &[(String, f32)],
    theme: &Theme,
) {
    let inner = cell_titled(frame, area, "readings", None, false, theme);
    let lines: Vec<Line<'static>> = extras
        .iter()
        .take(usize::from(inner.height))
        .map(|(name, c)| Line::from(Span::styled(format!(" {name}  {c:.0}°"), theme.temp(*c))))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Greedy rows of whole items, two spaces apart: an item never splits
/// across lines the way word wrap would split `▒ cached 5.3G`.
pub(super) fn pack_items(items: Vec<Vec<Span<'static>>>, width: u16) -> Vec<Line<'static>> {
    const GAP: usize = 2;
    let width = usize::from(width);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut used = 0;
    for item in items {
        let w: usize = item.iter().map(Span::width).sum();
        match lines.last_mut() {
            Some(line) if used + GAP + w <= width => {
                line.spans.push(Span::raw(" ".repeat(GAP)));
                line.spans.extend(item);
                used += GAP + w;
            }
            _ => {
                lines.push(Line::from(item));
                used = w;
            }
        }
    }
    lines
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use siltop_core::History;

    use super::{Axis, BorderScale, Graph, GraphInk, Modifier, Rect, Scale};
    use super::{cluster_spec, graph_spec, live, paint_series, paint_temp, usage_spec};
    use crate::widgets::grid::{CellKind, Placed};
    use crate::widgets::tests_support::fixture;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn cluster_spec_is_graph_spec_with_cluster_kind() {
        let g = graph_spec(7, "load", Some(String::from("42%")), true);
        let c = cluster_spec(7, "load", Some(String::from("42%")), true);
        assert_eq!(g.kind, CellKind::Graph);
        assert_eq!(c.kind, CellKind::Cluster);
        assert_eq!(c.min, (14, 5));
        assert_eq!((c.id, c.weight, c.present), (g.id, g.weight, g.present));
        assert_eq!(c.title, g.title);
    }

    /// The top border of one cell painted `width` wide, as (text, buffer).
    fn top_border(
        width: u16,
        paint: impl FnOnce(&mut ratatui::Frame, &Placed),
    ) -> (String, ratatui::buffer::Buffer) {
        let cell = Placed {
            id: super::ID_PACKAGE,
            kind: CellKind::Graph,
            rect: Rect::new(0, 0, width, 8),
            hop: None,
        };
        let mut t = Terminal::new(TestBackend::new(width, 8)).unwrap();
        t.draw(|f| paint(f, &cell)).unwrap();
        let buf = t.backend().buffer().clone();
        let top = (0..width).map(|x| buf[(x, 0)].symbol()).collect();
        (top, buf)
    }

    fn history(values: &[f32]) -> History {
        let mut h = History::default();
        for &v in values {
            h.push(v);
        }
        h
    }

    #[test]
    fn expanded_temperature_is_drawn_in_its_heat_colour() {
        // Seen live: compact `68°` in heat ink, expanded `71°` in plain
        // white, so a crit-red 95° lost its alarm exactly where users look.
        let fx = fixture("");
        let view = fx.view();
        let h = history(&[90.0, 95.0]);
        let (top, buf) = top_border(40, |f, cell| paint_temp(f, cell, &view, &h, view.theme));
        let at = top.find("95°").expect("value in the border");
        let x = u16::try_from(top[..at].chars().count()).unwrap();
        assert_eq!(Some(buf[(x, 0)].fg), view.theme.temp(95.0).fg, "{top}");
        assert!(buf[(x, 0)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn expanded_graphs_print_their_scale_in_the_top_border() {
        // Seen live: `gpu 1%` drawn a fifth tall and battery 80% at under
        // half height, with no ceiling or band to read them against.
        let fx = fixture("");
        let view = fx.view();
        let h = history(&[5_000.0, 12_000.0, 9_000.0]);
        let (top, _) = top_border(40, |f, cell| {
            paint_series(
                f,
                cell,
                &view,
                &h,
                view.theme.net,
                GraphInk::Flat,
                Scale::Auto { floor: 1_024.0 },
                Axis::Bytes,
                view.theme,
            );
        });
        let graph = Graph {
            history: &h,
            ramp: view.theme.net,
            theme: view.theme,
            scale: Scale::Auto { floor: 1_024.0 },
            axis: Axis::Bytes,
            ink: GraphInk::Flat,
            time: view.time_for(&h),
        };
        let label = graph.border_label(38).expect("auto scale has a ceiling");
        assert!(top.ends_with(&format!("─ {label} ╮")), "{top}");
        // a cell too narrow for both keeps the title and drops the scale
        let (narrow, _) = top_border(14, |f, cell| paint_temp(f, cell, &view, &h, view.theme));
        assert!(narrow.starts_with("╭ package"), "{narrow}");
    }

    #[test]
    fn usage_spec_is_a_shorter_graph() {
        let g = graph_spec(1, "cpu", None, false);
        let u = usage_spec(1, "cpu", None, false);
        assert_eq!(u.kind, CellKind::Graph);
        assert_eq!(u.min, (16, 4));
        assert_eq!(g.min, (16, 5));
        assert!(!u.present);
    }

    #[test]
    fn live_needs_a_value_or_a_history() {
        let mut h = History::default();
        assert!(!live(None, &h), "absent hardware stays hidden");
        assert!(live(Some(0.0), &h), "a real zero is live");
        h.push(1.0);
        assert!(live(None, &h), "history keeps a cell after a dropout");
    }
}
