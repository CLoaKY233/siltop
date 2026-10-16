//! The signal picker ('X'): every signal with a plain description, one
//! per row. Paint and hit-testing share `menu_rect`/`row_at`.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;

/// One pickable signal. The UI never sees the metrics `Signal` type; the
/// bin maps rows back by index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalRow {
    pub label: &'static str,
    pub describe: &'static str,
    /// False when the kernel will refuse (another user's process).
    pub enabled: bool,
}

/// What the picker shows: whose process, the rows and the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalMenuView<'a> {
    /// "Safari (412)" or "3 tagged".
    pub target: &'a str,
    pub rows: &'a [SignalRow],
    pub selected: usize,
    /// Why every verb is dimmed, e.g. "owned by _windowserver: needs admin".
    pub note: Option<&'a str>,
}

const LABEL_W: usize = 5;

fn row_text(row: &SignalRow) -> String {
    format!(" {:<LABEL_W$} {}", row.label, row.describe)
}

/// The popup: as wide as its longest row, centred, clamped to `area`.
#[must_use]
pub fn menu_rect(area: Rect, menu: &SignalMenuView<'_>) -> Rect {
    let title = format!(" signal {} ", menu.target);
    let widest = menu
        .rows
        .iter()
        .map(|r| row_text(r).width() + 2)
        .chain([title.width(), menu.note.map_or(0, |n| n.width() + 2)])
        .max()
        .unwrap_or(0);
    let extra = usize::from(menu.note.is_some());
    let width = u16::try_from(widest + 2).unwrap_or(u16::MAX);
    let height = u16::try_from(menu.rows.len() + extra + 2).unwrap_or(u16::MAX);
    super::super::help::centered(area, width, height)
}

/// The row index under (`col`, `row`), from the same rect paint uses.
#[must_use]
pub fn row_at(area: Rect, menu: &SignalMenuView<'_>, col: u16, row: u16) -> Option<usize> {
    let rect = menu_rect(area, menu);
    let inner = rect.inner(ratatui::layout::Margin::new(1, 1));
    if !inner.contains(ratatui::layout::Position::new(col, row)) {
        return None;
    }
    let idx = usize::from(row - inner.y);
    (idx < menu.rows.len()).then_some(idx)
}

pub(crate) fn render(frame: &mut Frame, area: Rect, menu: &SignalMenuView<'_>, theme: &Theme) {
    let rect = menu_rect(area, menu);
    let mut lines: Vec<Line> = menu
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let mark = if i == menu.selected { "▸" } else { " " };
            let style = match (row.enabled, i == menu.selected) {
                (false, _) => theme.faint(),
                (true, true) => theme.title(),
                (true, false) => theme.fg(),
            };
            Line::from(vec![
                Span::styled(mark, theme.title()),
                Span::styled(row_text(row), style),
            ])
        })
        .collect();
    if let Some(note) = menu.note {
        lines.push(Line::from(Span::styled(
            format!(" {note}"),
            theme.warn_style(),
        )));
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(format!(" signal {} ", menu.target))
        .title_bottom(Line::from(" ↑↓ pick · enter send · esc close "))
        .border_style(theme.border(true))
        .style(ratatui::style::Style::default().fg(theme.fg));
    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(lines).block(block), rect);
    crate::layout::paint_card_close(frame, rect, theme);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    const ROWS: [SignalRow; 3] = [
        SignalRow {
            label: "TERM",
            describe: "ask to quit (lets it save and clean up)",
            enabled: true,
        },
        SignalRow {
            label: "STOP",
            describe: "pause the process (freeze it in place)",
            enabled: true,
        },
        SignalRow {
            label: "KILL",
            describe: "force quit now (no chance to save)",
            enabled: true,
        },
    ];

    fn menu(note: Option<&str>) -> SignalMenuView<'_> {
        SignalMenuView {
            target: "Safari (412)",
            rows: &ROWS,
            selected: 1,
            note,
        }
    }

    fn paint(m: &SignalMenuView<'_>, w: u16, h: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), m, &Theme::default()))
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_owned()).collect())
            .collect()
    }

    #[test]
    fn menu_lists_every_signal_with_its_description_and_cursor() {
        let text = paint(&menu(None), 80, 24).join("\n");
        assert!(text.contains("signal Safari (412)"), "{text}");
        assert!(text.contains("TERM  ask to quit"), "{text}");
        assert!(text.contains("▸ STOP  pause the process"), "{text}");
    }

    #[test]
    fn menu_shows_the_needs_admin_note() {
        let note = "owned by _windowserver: needs admin";
        let text = paint(&menu(Some(note)), 80, 24).join("\n");
        assert!(text.contains(note), "{text}");
    }

    #[test]
    fn row_at_matches_painted_rows() {
        let area = Rect::new(0, 0, 80, 24);
        let m = menu(None);
        let rect = menu_rect(area, &m);
        for (i, _) in ROWS.iter().enumerate() {
            let y = rect.y + 1 + u16::try_from(i).unwrap();
            assert_eq!(row_at(area, &m, rect.x + 2, y), Some(i));
        }
        assert_eq!(row_at(area, &m, rect.x, rect.y), None, "border");
        assert_eq!(row_at(area, &m, 0, 0), None, "outside");
    }

    #[test]
    fn menu_rect_never_escapes_any_area() {
        let m = menu(Some("owned by root: needs admin"));
        for w in 1..=120 {
            for h in 1..=40 {
                let area = Rect::new(0, 0, w, h);
                let r = menu_rect(area, &m);
                assert!(r.right() <= area.right() && r.bottom() <= area.bottom());
                // hit-testing never panics on a squeezed popup
                let _ = row_at(area, &m, r.x + 1, r.y + 1);
            }
        }
    }

    #[test]
    fn menu_fits_80x24() {
        let r = menu_rect(Rect::new(0, 0, 80, 24), &menu(Some("x")));
        assert!(r.width < 80 && r.height < 24, "{r:?}");
    }
}
