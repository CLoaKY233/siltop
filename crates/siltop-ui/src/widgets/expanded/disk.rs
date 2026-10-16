//! Expanded disk: read, write and volume cells.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use siltop_core::{byte_rate, bytes_short, printable};
use unicode_width::UnicodeWidthStr;

use super::cells::{graph_spec, overflow_rows, paint_more};
use super::{ID_DISK_READ, ID_DISK_WRITE, ID_VOLUMES};
use crate::chrome::{cell_titled, render_fill_bar};
use crate::theme::Theme;
use crate::widgets::AppView;
use crate::widgets::grid::{Band, CellKind, CellSpec, CellTitle};
use crate::widgets::processes::fit;

pub(super) fn disk_bands(view: &AppView<'_>) -> Vec<Band> {
    vec![
        Band {
            take_leftover: true,
            ..Band::new(
                5,
                vec![
                    graph_spec(
                        ID_DISK_READ,
                        "read",
                        Some(byte_rate(view.snapshot.disk.read_bps)),
                        true,
                    ),
                    graph_spec(
                        ID_DISK_WRITE,
                        "write",
                        Some(byte_rate(view.snapshot.disk.write_bps)),
                        true,
                    ),
                ],
            )
        },
        Band {
            // Room for every volume's name and bar (plus borders) before the
            // graphs take what is left; a fixed 4 rows only ever fit the
            // `+N more` marker once two volumes were mounted.
            grow_to: Some(volumes_height(view.snapshot.disk.volumes.len())),
            ..Band::new(
                4,
                vec![CellSpec {
                    id: ID_VOLUMES,
                    kind: CellKind::List,
                    title: CellTitle {
                        label: "volumes",
                        value: None,
                        hop: None,
                    },
                    min: (16, 4),
                    weight: 1,
                    present: !view.snapshot.disk.volumes.is_empty(),
                }],
            )
        },
    ]
}

/// Rows the volumes cell wants: two a volume and its borders, capped so a
/// long mount list never starves the graphs (the rest go to `+N more`).
fn volumes_height(volumes: usize) -> u16 {
    const MAX_LISTED: u16 = 6;
    let listed = u16::try_from(volumes).unwrap_or(u16::MAX).min(MAX_LISTED);
    listed * 2 + 2
}

pub(super) fn paint_volumes(frame: &mut Frame, area: Rect, view: &AppView<'_>, theme: &Theme) {
    let inner = cell_titled(frame, area, "volumes", None, false, theme);
    let volumes = &view.snapshot.disk.volumes;
    // Two rows a volume (the name line, then its capacity bar) when they
    // all fit; else one row each, so the list shows rather than only its
    // `+N more` marker.
    let bars = volumes.len().saturating_mul(2) <= usize::from(inner.height);
    let (shown, hidden) = overflow_rows(volumes.len(), inner.height, if bars { 2 } else { 1 });
    let mut y = inner.y;
    for vol in volumes.iter().take(shown) {
        let line = Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: 1,
        };
        // The numbers stay whole; the name gives up its cells first.
        let usage = format!(
            "  {} / {}",
            bytes_short(vol.used_bytes),
            bytes_short(vol.total_bytes)
        );
        let room = inner
            .width
            .saturating_sub(u16::try_from(usage.width()).unwrap_or(u16::MAX));
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(fit(&printable(&vol.name), room), theme.fg()),
                Span::styled(usage, theme.dim()),
            ])),
            line,
        );
        y = y.saturating_add(1);
        if bars {
            render_fill_bar(frame, Rect { y, ..line }, vol.ratio(), theme.disk, theme);
            y = y.saturating_add(1);
        }
    }
    paint_more(frame, inner.x, y, inner.width, hidden, theme);
}
