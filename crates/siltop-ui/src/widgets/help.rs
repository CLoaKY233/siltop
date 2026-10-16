use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;
use crate::widgets::AppView;

/// One titled block of help rows: `(keys, what they do)`. The binary builds
/// these from its keymap, so help can never disagree with the keys.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HelpGroup {
    pub title: &'static str,
    pub rows: Vec<(String, &'static str)>,
}

const KEY_MIN: usize = 10;

/// Help text as `(line, is_section_title)`, one blank line between groups.
#[must_use]
pub(super) fn help_lines(groups: &[HelpGroup]) -> Vec<(String, bool)> {
    let key_w = groups
        .iter()
        .flat_map(|g| g.rows.iter().map(|(keys, _)| keys.width()))
        .max()
        .unwrap_or(0)
        .max(KEY_MIN);
    let mut out = Vec::new();
    for (i, group) in groups.iter().enumerate() {
        if i > 0 {
            out.push((String::new(), false));
        }
        out.push((format!(" {}", group.title), true));
        for (keys, help) in &group.rows {
            let pad = key_w.saturating_sub(keys.width());
            out.push((format!("   {keys}{}  {help}", " ".repeat(pad)), false));
        }
    }
    if out.is_empty() {
        out.push((String::from(" no key bindings"), false));
    }
    out
}

/// The help popup: as wide and tall as its text, never bigger than `area`.
#[must_use]
pub fn help_rect(area: Rect, groups: &[HelpGroup]) -> Rect {
    let lines = help_lines(groups);
    let widest = lines.iter().map(|(l, _)| l.width()).max().unwrap_or(0);
    let width = u16::try_from(widest + 3).unwrap_or(u16::MAX);
    let height = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX);
    centered(area, width, height)
}

/// Rows that can scroll off the popup; 0 when everything fits.
#[must_use]
pub fn help_max_scroll(area: Rect, groups: &[HelpGroup]) -> u16 {
    let lines = u16::try_from(help_lines(groups).len()).unwrap_or(u16::MAX);
    let visible = help_rect(area, groups).height.saturating_sub(2);
    lines.saturating_sub(visible)
}

pub(super) fn render(frame: &mut Frame, area: Rect, view: &AppView<'_>, theme: &Theme) {
    let rect = help_rect(area, view.help_groups);
    let all = help_lines(view.help_groups);
    let scroll = view
        .help_scroll
        .min(help_max_scroll(area, view.help_groups));
    let visible = usize::from(rect.height.saturating_sub(2));
    let lines: Vec<Line> = all
        .iter()
        .skip(usize::from(scroll))
        .take(visible)
        .map(|(text, section)| {
            let style = if *section { theme.title() } else { theme.fg() };
            Line::from(Span::styled(text.clone(), style))
        })
        .collect();
    let more = all.len() > visible;
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(" help ")
        .border_style(theme.border(true))
        .style(ratatui::style::Style::default().fg(theme.fg));
    if more {
        let last = (usize::from(scroll) + visible).min(all.len());
        block = block.title_bottom(Line::from(format!(
            " {}–{last} of {} · ↑↓ scroll ",
            usize::from(scroll) + 1,
            all.len()
        )));
    }
    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(lines).block(block), rect);
    crate::layout::paint_card_close(frame, rect, theme);
}

pub(crate) fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn groups(n: usize) -> Vec<HelpGroup> {
        (0..n)
            .map(|g| HelpGroup {
                title: "group",
                rows: (0..5)
                    .map(|r| (format!("k{g}{r}"), "settings (sampling, panes, processes)"))
                    .collect(),
            })
            .collect()
    }

    fn paint(view: &AppView<'_>, w: u16, h: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), view, &Theme::default()))
            .unwrap();
        let buf = terminal.backend().buffer();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn centered_stays_inside() {
        let r = centered(Rect::new(0, 0, 20, 10), 40, 20);
        assert_eq!(r, Rect::new(0, 0, 20, 10));
    }

    #[test]
    fn centered_sweep_never_escapes_the_area() {
        for aw in [0_u16, 1, 7, 40, 200] {
            for ah in [0_u16, 1, 9, 50] {
                for w in [0_u16, 3, 60, 400] {
                    for h in [0_u16, 2, 30, 400] {
                        let area = Rect::new(3, 4, aw, ah);
                        let r = centered(area, w, h);
                        assert!(r.x >= area.x && r.right() <= area.right(), "{area:?} {r:?}");
                        assert!(
                            r.y >= area.y && r.bottom() <= area.bottom(),
                            "{area:?} {r:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn help_is_content_sized() {
        let g = groups(1);
        let rect = help_rect(Rect::new(0, 0, 200, 60), &g);
        let lines = help_lines(&g);
        assert_eq!(usize::from(rect.height), lines.len() + 2);
        let widest = lines.iter().map(|(l, _)| l.width()).max().unwrap();
        assert_eq!(usize::from(rect.width), widest + 3);
    }

    #[test]
    fn long_help_line_is_never_clipped_when_there_is_room() {
        // regression: "settings (sampling, panes, processe" was cut at 62 cols
        let mut fx = crate::widgets::tests_support::fixture("");
        fx.help_groups = groups(1);
        let screen = paint(&fx.view(), 120, 30).join("\n");
        assert!(
            screen.contains("settings (sampling, panes, processes)"),
            "{screen}"
        );
    }

    #[test]
    fn help_fits_60x16_and_scrolls() {
        let mut fx = crate::widgets::tests_support::fixture("");
        fx.help_groups = groups(4);
        let area = Rect::new(0, 0, 60, 16);
        let max = help_max_scroll(area, &fx.help_groups);
        assert!(max > 0, "four groups cannot fit 16 rows");
        let rect = help_rect(area, &fx.help_groups);
        assert!(rect.width <= 60 && rect.height <= 16);
        let first = paint(&fx.view(), 60, 16).join("\n");
        assert!(first.contains("k00"), "{first}");
        assert!(first.contains("of"), "a scroll hint shows: {first}");
        fx.help_scroll = max;
        let last = paint(&fx.view(), 60, 16).join("\n");
        assert!(last.contains("k34"), "{last}");
        assert!(!last.contains("k00"), "{last}");
        fx.help_scroll = u16::MAX;
        assert_eq!(paint(&fx.view(), 60, 16).join("\n"), last, "scroll clamps");
    }

    #[test]
    fn help_without_groups_still_draws() {
        let fx = crate::widgets::tests_support::fixture("");
        let mut fx = fx;
        fx.help_groups = Vec::new();
        let screen = paint(&fx.view(), 40, 10).join("\n");
        assert!(screen.contains("help"), "{screen}");
    }

    #[test]
    fn help_lines_align_the_description_column() {
        let g = vec![HelpGroup {
            title: "general",
            rows: vec![
                (String::from("q"), "quit"),
                (String::from("ctrl-c"), "quit"),
                (String::from("↑ k"), "up"),
            ],
        }];
        let lines = help_lines(&g);
        let cols: Vec<usize> = lines
            .iter()
            .filter(|(_, s)| !s)
            .map(|(l, _)| {
                let at = l.rfind("  ").unwrap() + 2;
                l[..at].width()
            })
            .collect();
        assert!(cols.windows(2).all(|w| w[0] == w[1]), "{lines:?}");
        assert!(lines[0].1, "the group title is a section");
    }
}
