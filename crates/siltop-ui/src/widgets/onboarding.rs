//! The first-run screens. Painting and clicks share [`buttons`], so a
//! choice is clickable exactly where it is drawn.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;

/// Which screen is up. Esc skips the rest from any of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Welcome,
    Privacy,
    Updates,
}

impl Screen {
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Welcome => "Welcome",
            Self::Privacy => "Privacy",
            Self::Updates => "Updates",
        }
    }

    /// The choices, left to right. The first one is the default focus.
    #[must_use]
    pub const fn choices(self) -> &'static [&'static str] {
        match self {
            Self::Welcome | Self::Privacy => &["Continue"],
            Self::Updates => &["Yes, check", "No"],
        }
    }
}

/// What the screen is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct View<'a> {
    pub screen: Screen,
    /// Index into [`Screen::choices`].
    pub focus: usize,
    /// The welcome screen names these keys; other screens ignore it.
    pub keys: &'a str,
}

/// One choice and the cells it occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Button {
    pub label: &'static str,
    pub rect: Rect,
}

const WELCOME: &str = "A system monitor for Apple Silicon, in the terminal.";
const PRIVACY: &str = "siltop reads your Mac's sensors locally. It sends nothing anywhere, except the update check below if you turn it on.";
const UPDATES: &str = "Check for updates when siltop starts? It asks GitHub once a day for the latest release number. Nothing about you or your Mac is sent.";
const HINT: &str = "Enter chooses  ·  Esc skips";

fn body(view: &View<'_>) -> String {
    match view.screen {
        Screen::Welcome => {
            if view.keys.is_empty() {
                WELCOME.to_owned()
            } else {
                format!("{WELCOME}\n\n{}", view.keys)
            }
        }
        Screen::Privacy => PRIVACY.to_owned(),
        Screen::Updates => UPDATES.to_owned(),
    }
}

fn card(area: Rect) -> Rect {
    let width = area.width.clamp(20, 72);
    let height = area.height.clamp(8, 16);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect {
        x,
        y,
        width,
        height,
    }
}

/// The choice buttons, in order, inside `area`.
#[must_use]
pub fn buttons(area: Rect, screen: Screen) -> Vec<Button> {
    let card = card(area);
    let choices = screen.choices();
    let y = card.y + card.height.saturating_sub(3);
    let mut x = card.x + 2;
    let mut out = Vec::with_capacity(choices.len());
    for label in choices {
        let width = u16::try_from(label.width())
            .unwrap_or(u16::MAX)
            .saturating_add(4);
        let rect = Rect {
            x,
            y,
            width,
            height: 1,
        };
        out.push(Button { label, rect });
        x = x.saturating_add(width).saturating_add(2);
    }
    out
}

/// Which choice a click landed on, if it landed on one.
#[must_use]
pub fn hit(area: Rect, screen: Screen, col: u16, row: u16) -> Option<usize> {
    buttons(area, screen).into_iter().position(|button| {
        col >= button.rect.x
            && col < button.rect.x.saturating_add(button.rect.width)
            && row == button.rect.y
    })
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut line = String::new();
        for word in paragraph.split(' ') {
            let next = if line.is_empty() {
                word.to_owned()
            } else {
                format!("{line} {word}")
            };
            if next.width() > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                word.clone_into(&mut line);
            } else {
                line = next;
            }
        }
        lines.push(line);
    }
    lines
}

/// Full-screen. The dashboard is not drawn behind it.
pub fn render(frame: &mut Frame, area: Rect, view: &View<'_>, theme: &Theme) {
    frame.render_widget(Clear, area);
    let card = card(area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(format!(" siltop · {} ", view.screen.title()))
        .style(theme.fg());
    let inner = block.inner(card);
    frame.render_widget(block, card);
    let width = usize::from(inner.width);
    let lines: Vec<Line> = wrap(&body(view), width)
        .into_iter()
        .map(|text| Line::from(Span::styled(text, theme.fg())))
        .collect();
    let text_area = Rect {
        height: inner.height.saturating_sub(2),
        ..inner
    };
    frame.render_widget(Paragraph::new(lines), text_area);
    for (i, button) in buttons(area, view.screen).into_iter().enumerate() {
        let focused = i == view.focus;
        let style = if focused { theme.title() } else { theme.dim() };
        let label = format!("[ {} ]", button.label);
        let rect = Rect {
            width: u16::try_from(label.width()).unwrap_or(button.rect.width),
            ..button.rect
        };
        frame.render_widget(Paragraph::new(Span::styled(label, style)), rect);
    }
    let hint = Rect {
        x: card.x + 2,
        y: card.y + card.height.saturating_sub(2),
        width: card.width.saturating_sub(4),
        height: 1,
    };
    frame.render_widget(Paragraph::new(Span::styled(HINT, theme.dim())), hint);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    fn paint(view: &View<'_>, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let theme = Theme::by_name("billabong").unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), view, &theme))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn text(buf: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.area.height {
            out.push('|');
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push_str("|\n");
        }
        out
    }

    fn snap(name: &str, screen: Screen, width: u16, height: u16) {
        let view = View {
            screen,
            focus: 0,
            keys: "m menu · ? keys · T themes",
        };
        let mut settings = insta::Settings::clone_current();
        settings.set_omit_expression(true);
        settings.set_snapshot_path("snapshots");
        settings.bind(|| {
            insta::assert_snapshot!(name, text(&paint(&view, width, height)));
        });
    }

    #[test]
    fn welcome_80x24() {
        snap("onboard_welcome_80x24", Screen::Welcome, 80, 24);
    }

    #[test]
    fn welcome_160x45() {
        snap("onboard_welcome_160x45", Screen::Welcome, 160, 45);
    }

    #[test]
    fn privacy_80x24() {
        snap("onboard_privacy_80x24", Screen::Privacy, 80, 24);
    }

    #[test]
    fn privacy_160x45() {
        snap("onboard_privacy_160x45", Screen::Privacy, 160, 45);
    }

    #[test]
    fn updates_80x24() {
        snap("onboard_updates_80x24", Screen::Updates, 80, 24);
    }

    #[test]
    fn updates_160x45() {
        snap("onboard_updates_160x45", Screen::Updates, 160, 45);
    }

    #[test]
    fn updates_default_focus_is_yes_and_a_click_can_pick_no() {
        let area = Rect::new(0, 0, 80, 24);
        let buttons = buttons(area, Screen::Updates);
        assert_eq!(buttons[0].label, "Yes, check");
        assert_eq!(buttons[1].label, "No");
        let no = buttons[1].rect;
        assert_eq!(hit(area, Screen::Updates, no.x + 1, no.y), Some(1));
        assert_eq!(hit(area, Screen::Updates, 0, 0), None);
    }

    #[test]
    fn each_screen_names_its_choice() {
        for screen in [Screen::Welcome, Screen::Privacy, Screen::Updates] {
            let painted = text(&paint(
                &View {
                    screen,
                    focus: 0,
                    keys: "m menu · ? keys · T themes",
                },
                80,
                24,
            ));
            for choice in screen.choices() {
                assert!(painted.contains(choice), "{screen:?} lacks {choice}");
            }
        }
    }
}
