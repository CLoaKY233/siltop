//! Popup menus: the main menu (every command, grouped, each with its key
//! on the right so a mouse user learns the keys) and the right-click menu
//! on a process row. The app supplies the entries; [`rows`] is the one
//! geometry painting, clicks and hover all read, so an item is clickable
//! exactly where it is drawn.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::layout::{CORNER_CLOSE, CORNER_W};
use crate::theme::Theme;

/// One menu line: a command with the key that also runs it, or a group
/// header (not selectable).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuEntry {
    pub label: String,
    /// The key that runs the command outside the menu; empty for none.
    pub key: String,
    pub header: bool,
}

impl MenuEntry {
    #[must_use]
    pub fn item(label: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            key: key.into(),
            header: false,
        }
    }

    #[must_use]
    pub fn header(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            key: String::new(),
            header: true,
        }
    }
}

/// An open menu, as the view paints it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuView<'a> {
    pub title: &'a str,
    pub entries: &'a [MenuEntry],
    pub selected: usize,
    /// The first entry shown: long menus scroll.
    pub top: usize,
    /// Where a right-click opened it; `None` centres it.
    pub anchor: Option<(u16, u16)>,
}

/// Columns between the label and its right-aligned key.
const KEY_GAP: usize = 4;

/// Where a click on a menu landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuHit {
    Item(usize),
    /// The `[×]` in the card's title.
    Close,
    /// Inside the card but on no item (its border, a header).
    Card,
    Outside,
}

fn inner_width(view: &MenuView<'_>) -> u16 {
    let widest = view
        .entries
        .iter()
        .map(|e| e.label.width() + 3 + KEY_GAP + e.key.width())
        .chain([view.title.width() + usize::from(CORNER_W) + 4])
        .max()
        .unwrap_or(0);
    // a cell of padding either side of every row
    u16::try_from(widest + 2).unwrap_or(u16::MAX)
}

/// The card inside `area` (the body above the footer): at the pointer for
/// a right-click, else centred; always clamped to `area`.
#[must_use]
pub(super) fn rect(area: Rect, view: &MenuView<'_>) -> Rect {
    let width = inner_width(view).saturating_add(2).min(area.width);
    let items = u16::try_from(view.entries.len()).unwrap_or(u16::MAX);
    let height = items.saturating_add(2).min(area.height);
    let (x, y) = match view.anchor {
        Some((col, row)) => {
            let x = col.min(area.right().saturating_sub(width)).max(area.x);
            // below the pointer when it fits, else as low as it can sit
            let y = row
                .saturating_add(1)
                .min(area.bottom().saturating_sub(height))
                .max(area.y);
            (x, y)
        }
        None => (
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
        ),
    };
    Rect {
        x,
        y,
        width,
        height,
    }
}

/// Entries that fit inside the card at once.
#[must_use]
pub fn page(area: Rect, view: &MenuView<'_>) -> usize {
    usize::from(rect(area, view).height.saturating_sub(2))
}

/// The first entry shown: `top`, moved just enough to keep the selection
/// on screen. The app stores it back so the wheel scrolls from there.
#[must_use]
pub fn first(area: Rect, view: &MenuView<'_>) -> usize {
    let page = page(area, view).max(1);
    let top = view.top.min(view.entries.len().saturating_sub(page));
    if view.selected < top {
        view.selected
    } else if view.selected >= top + page {
        view.selected + 1 - page
    } else {
        top
    }
}

/// Each visible entry's row inside the card, in order, headers included.
#[must_use]
pub(super) fn rows(area: Rect, view: &MenuView<'_>) -> Vec<(Rect, usize)> {
    let card = rect(area, view);
    if card.width < 3 || card.height < 3 {
        return Vec::new();
    }
    let top = first(area, view);
    (top..view.entries.len())
        .take(usize::from(card.height - 2))
        .enumerate()
        .map(|(line, i)| {
            let y = card.y + 1 + u16::try_from(line).unwrap_or(0);
            (Rect::new(card.x + 1, y, card.width - 2, 1), i)
        })
        .collect()
}

/// The `[×]` on the card's top border.
#[must_use]
pub(super) fn close_rect(area: Rect, view: &MenuView<'_>) -> Option<Rect> {
    let card = rect(area, view);
    (card.width > CORNER_W + 4 && card.height >= 3).then(|| {
        Rect::new(
            card.right().saturating_sub(CORNER_W + 1),
            card.y,
            CORNER_W,
            1,
        )
    })
}

#[must_use]
pub fn hit(area: Rect, view: &MenuView<'_>, col: u16, row: u16) -> MenuHit {
    let inside = |r: Rect| {
        col >= r.x && col < r.x.saturating_add(r.width) && row >= r.y && row < r.y + r.height
    };
    if close_rect(area, view).is_some_and(inside) {
        return MenuHit::Close;
    }
    if let Some((_, i)) = rows(area, view)
        .into_iter()
        .find(|(r, i)| inside(*r) && !view.entries[*i].header)
    {
        return MenuHit::Item(i);
    }
    if inside(rect(area, view)) {
        MenuHit::Card
    } else {
        MenuHit::Outside
    }
}

/// Moves the highlight by `delta` over the selectable entries, wrapping.
#[must_use]
pub fn step(entries: &[MenuEntry], selected: usize, delta: i32) -> usize {
    let items: Vec<usize> = (0..entries.len()).filter(|&i| !entries[i].header).collect();
    if items.is_empty() {
        return 0;
    }
    let at = items.iter().position(|&i| i >= selected).unwrap_or(0);
    let n = i32::try_from(items.len()).unwrap_or(1);
    let next = (i32::try_from(at).unwrap_or(0) + delta).rem_euclid(n);
    items[usize::try_from(next).unwrap_or(0)]
}

/// The first selectable entry.
#[must_use]
pub fn first_item(entries: &[MenuEntry]) -> usize {
    entries.iter().position(|e| !e.header).unwrap_or(0)
}

pub(super) fn render(frame: &mut Frame, area: Rect, view: &MenuView<'_>, theme: &Theme) {
    let card = rect(area, view);
    if card.width < 3 || card.height < 3 {
        return;
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme.border(true))
        .title(Span::styled(format!(" {} ", view.title), theme.title()));
    frame.render_widget(Clear, card);
    frame.render_widget(block, card);
    if let Some(close) = close_rect(area, view) {
        frame
            .buffer_mut()
            .set_string(close.x, close.y, CORNER_CLOSE, theme.dim());
    }
    let shown = rows(area, view);
    for (row, i) in &shown {
        let entry = &view.entries[*i];
        if entry.header {
            let line = Line::from(Span::styled(format!(" {}", entry.label), theme.muted()));
            frame.render_widget(Paragraph::new(line), *row);
            continue;
        }
        let on = *i == view.selected;
        let pad = usize::from(row.width)
            .saturating_sub(4 + entry.label.width() + entry.key.width())
            .max(1);
        let (label_style, key_style) = if on {
            (theme.selection(true, false), theme.selection(true, false))
        } else {
            (theme.fg(), theme.dim())
        };
        let mark = if on {
            theme.selection_mark(true)
        } else {
            Span::raw(" ")
        };
        let line = Line::from(vec![
            mark,
            Span::styled(format!("  {}{}", entry.label, " ".repeat(pad)), label_style),
            Span::styled(format!("{} ", entry.key), key_style),
        ]);
        frame.render_widget(Paragraph::new(line), *row);
    }
    // more above or below: say so on the border
    let buf = frame.buffer_mut();
    if shown.first().is_some_and(|(_, i)| *i > 0) && card.width > 12 {
        buf.set_string(card.x + 2, card.y, "↑ more", theme.dim());
    }
    if shown
        .last()
        .is_some_and(|(_, i)| i + 1 < view.entries.len())
        && card.width > 12
    {
        let y = card.bottom().saturating_sub(1);
        buf.set_string(card.x + 2, y, "↓ more", theme.dim());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn entries() -> Vec<MenuEntry> {
        let mut out = vec![MenuEntry::header("pages")];
        for (label, key) in [
            ("Timeline", "H"),
            ("Settings", "s"),
            ("Themes", "t"),
            ("Keys & help", "?"),
            ("About", "a"),
        ] {
            out.push(MenuEntry::item(label, key));
        }
        out.push(MenuEntry::header("app"));
        out.push(MenuEntry::item("Quit", "q"));
        out
    }

    fn view(entries: &[MenuEntry], selected: usize) -> MenuView<'_> {
        MenuView {
            title: "siltop",
            entries,
            selected,
            top: 0,
            anchor: None,
        }
    }

    fn paint(width: u16, height: u16, view: &MenuView<'_>) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), view, &Theme::default()))
            .unwrap();
        let buf = terminal.backend().buffer();
        (0..height)
            .map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn every_drawn_item_is_clicked_where_it_is_drawn() {
        let entries = entries();
        for (w, h) in [(60, 15), (80, 23), (120, 39), (30, 7), (40, 6)] {
            let area = Rect::new(0, 0, w, h);
            let v = view(&entries, 1);
            let screen = paint(w, h, &v);
            let placed = rows(area, &v);
            assert!(!placed.is_empty(), "{w}x{h}");
            for (r, i) in placed {
                let line = &screen[usize::from(r.y)];
                assert!(line.contains(&entries[i].label), "{w}x{h}: {line}");
                let at = line.find(&entries[i].label).unwrap();
                let col = u16::try_from(line[..at].chars().count()).unwrap();
                let want = if entries[i].header {
                    MenuHit::Card
                } else {
                    MenuHit::Item(i)
                };
                assert_eq!(hit(area, &v, col, r.y), want, "{w}x{h}");
            }
        }
    }

    #[test]
    fn card_never_leaves_the_area_centred_or_anchored() {
        let entries = entries();
        for w in (0..=120_u16).step_by(7) {
            for h in 0..=40_u16 {
                let area = Rect::new(0, 0, w, h);
                for anchor in [None, Some((0, 0)), Some((w, h)), Some((w / 2, h / 2))] {
                    let v = MenuView {
                        anchor,
                        ..view(&entries, 1)
                    };
                    let card = rect(area, &v);
                    assert!(
                        card.x + card.width <= w && card.y + card.height <= h,
                        "{w}x{h} {anchor:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_short_window_scrolls_to_the_selection_and_says_more() {
        let entries = entries();
        let area = Rect::new(0, 0, 60, 6);
        let v = view(&entries, 7);
        let shown: Vec<usize> = rows(area, &v).into_iter().map(|(_, i)| i).collect();
        assert!(shown.contains(&7), "{shown:?}");
        let screen = paint(60, 6, &view(&entries, 1)).join("\n");
        assert!(screen.contains("↓ more"), "{screen}");
    }

    #[test]
    fn the_close_button_is_a_hit_and_outside_is_outside() {
        let entries = entries();
        let area = Rect::new(0, 0, 80, 23);
        let v = view(&entries, 1);
        let close = close_rect(area, &v).unwrap();
        assert_eq!(hit(area, &v, close.x + 1, close.y), MenuHit::Close);
        assert_eq!(hit(area, &v, 0, 0), MenuHit::Outside);
        let screen = paint(80, 23, &v).join("\n");
        assert!(screen.contains(CORNER_CLOSE), "{screen}");
    }

    #[test]
    fn step_skips_headers_and_wraps_both_ways() {
        let entries = entries();
        assert_eq!(first_item(&entries), 1);
        assert_eq!(step(&entries, 1, -1), 7, "wraps past the header to Quit");
        assert_eq!(step(&entries, 5, 1), 7, "skips the app header");
        assert_eq!(step(&entries, 7, 1), 1);
    }
}
