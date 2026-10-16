//! Expanded memory: usage, composition and swap cells.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use siltop_core::{byte_rate, bytes_short};

use super::cells::{cluster_spec, graph_spec, pack_items};
use super::{ID_MEM, ID_MEM_PARTS, ID_SWAP};
use crate::chrome::{cell_titled, render_fill_bar, render_stacked_bar};
use crate::theme::Theme;
use crate::widgets::AppView;
use crate::widgets::grid::{Band, CellSpec};

pub(super) fn mem_bands(view: &AppView<'_>) -> Vec<Band> {
    let m = &view.snapshot.memory;
    let known = m.total_bytes > 0;
    vec![
        Band {
            take_leftover: true,
            ..Band::new(
                5,
                vec![graph_spec(ID_MEM, "memory", Some(mem_value(view)), true)],
            )
        },
        Band::new(
            5,
            vec![
                CellSpec {
                    min: (24, 4),
                    weight: 2,
                    ..cluster_spec(ID_MEM_PARTS, "composition", None, known)
                },
                cluster_spec(ID_SWAP, "swap", swap_value(m), m.swap_in_use()),
            ],
        ),
    ]
}

/// `18.0G/36.0G · 62% available`: used over total, then the kernel's free
/// level when it reports one. That level counts cache and purgeable pages
/// as reclaimable, so it is "available", never "free": `14.7G/24.0G · 62%
/// free` contradicted the used figure printed beside it.
pub(super) fn mem_value(view: &AppView<'_>) -> String {
    let m = &view.snapshot.memory;
    let used = format!(
        "{}/{}",
        bytes_short(m.used_bytes),
        bytes_short(m.total_bytes)
    );
    match m.free_percent {
        Some(free) => format!("{used} · {free}% available"),
        None => used,
    }
}

/// `0.8G/2.0G`: swap in use over the swap file's size.
pub(super) fn swap_value(m: &siltop_core::MemorySnapshot) -> Option<String> {
    m.swap_in_use().then(|| {
        if m.swap_total_bytes > 0 {
            format!(
                "{}/{}",
                bytes_short(m.swap_used_bytes),
                bytes_short(m.swap_total_bytes)
            )
        } else {
            bytes_short(m.swap_used_bytes)
        }
    })
}

/// The stacked composition bar, then a legend that wraps under it:
/// `█ app 12.0G  █ wired 4.0G  █ compressed 2.0G  ▒ cached 6.0G`.
pub(super) fn paint_mem_parts(frame: &mut Frame, area: Rect, view: &AppView<'_>, theme: &Theme) {
    let m = &view.snapshot.memory;
    let inner = cell_titled(frame, area, "composition", None, false, theme);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let segments = crate::widgets::mem::composition_segments(m, theme);
    render_stacked_bar(frame, Rect { height: 1, ..inner }, &segments, theme);
    let legend = Rect {
        y: inner.y.saturating_add(1),
        height: inner.height.saturating_sub(1),
        ..inner
    };
    if legend.height == 0 {
        return;
    }
    let items: Vec<Vec<Span<'static>>> = m
        .composition()
        .parts()
        .iter()
        .zip(segments)
        .filter(|((_, bytes), _)| *bytes > 0)
        .map(|((name, bytes), (_, style, glyph))| {
            vec![
                Span::styled(format!("{glyph} "), style),
                Span::styled(format!("{name} "), theme.dim()),
                Span::styled(bytes_short(*bytes), theme.title()),
            ]
        })
        .collect();
    frame.render_widget(Paragraph::new(pack_items(items, legend.width)), legend);
}

/// Swap fill over the swap file, then the page-in / page-out rates.
pub(super) fn paint_swap(
    frame: &mut Frame,
    cell: &crate::widgets::grid::Placed,
    view: &AppView<'_>,
    theme: &Theme,
) {
    let m = &view.snapshot.memory;
    let value = swap_value(m);
    let inner = cell_titled(frame, cell.rect, "swap", value.as_deref(), false, theme);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let ratio = if m.swap_total_bytes == 0 {
        0.0
    } else {
        (m.swap_used_bytes as f64 / m.swap_total_bytes as f64) as f32
    };
    render_fill_bar(frame, Rect { height: 1, ..inner }, ratio, theme.warn, theme);
    // Rates arrive from the second sample on; until then the row stays empty
    // rather than printing placeholders.
    let rates: Vec<Vec<Span<'static>>> = [("in ", m.swap_in_bps), ("out ", m.swap_out_bps)]
        .into_iter()
        .filter_map(|(label, bps)| {
            bps.map(|b| {
                vec![
                    Span::styled(label, theme.dim()),
                    Span::styled(byte_rate(b), theme.title()),
                ]
            })
        })
        .collect();
    if inner.height < 2 || rates.is_empty() {
        return;
    }
    let line = pack_items(rates, inner.width)
        .into_iter()
        .next()
        .unwrap_or_default();
    frame.render_widget(
        Paragraph::new(line),
        Rect {
            y: inner.y.saturating_add(1),
            height: 1,
            ..inner
        },
    );
}
