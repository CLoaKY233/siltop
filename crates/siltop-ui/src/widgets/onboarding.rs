//! The first-run screens. Painting and clicks share [`buttons`], so a
//! choice is clickable exactly where it is drawn.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
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
    /// Every screen, in the order the flow shows them.
    pub const ALL: [Self; 3] = [Self::Welcome, Self::Privacy, Self::Updates];

    /// This screen's place in the flow, from 1.
    #[must_use]
    pub const fn step(self) -> usize {
        match self {
            Self::Welcome => 1,
            Self::Privacy => 2,
            Self::Updates => 3,
        }
    }

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

/// Columns between the card's border and its text, buttons and hint.
const PAD: u16 = 2;
/// The card's widest; narrower terminals get the whole width.
const CARD_MAX_W: u16 = 64;
/// Rows the card spends around the body: the border, the wordmark, the
/// gaps above and below the body, the buttons and the hint...
const CHROME_ROWS: u16 = 2 + 1 + 1 + 1 + 1 + 1;
/// ...and the breathing room above the wordmark and below the hint, the
/// first thing a short terminal gives up.
const AIR_ROWS: u16 = 2;

/// Where everything on the card sits.
struct Layout {
    card: Rect,
    wordmark: u16,
    body: Rect,
    buttons: u16,
    hint: u16,
}

/// The card, sized to its content and centred: as wide as the terminal
/// allows up to [`CARD_MAX_W`], as tall as the wrapped body needs. A short
/// terminal loses the air rows first, then the body's last lines.
fn layout(area: Rect, view: &View<'_>) -> Layout {
    let width = area.width.clamp(20, CARD_MAX_W);
    let text_w = width.saturating_sub(2 * PAD);
    let lines = u16::try_from(wrap(&body(view), usize::from(text_w)).len()).unwrap_or(u16::MAX);
    let want = lines.saturating_add(CHROME_ROWS);
    let air = u16::from(want.saturating_add(AIR_ROWS) <= area.height);
    let height = want
        .saturating_add(2 * air)
        .min(area.height)
        .max(CHROME_ROWS + 1);
    let card = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    let wordmark = card.y + 1 + air;
    let hint = card.bottom().saturating_sub(2 + air);
    let buttons = hint.saturating_sub(1);
    let body_y = wordmark + 2;
    Layout {
        card,
        wordmark,
        body: Rect {
            x: card.x + PAD,
            y: body_y,
            width: text_w,
            height: buttons.saturating_sub(1).saturating_sub(body_y),
        },
        buttons,
        hint,
    }
}

/// The choice buttons, in order, inside `area`.
#[must_use]
pub fn buttons(area: Rect, view: &View<'_>) -> Vec<Button> {
    let at = layout(area, view);
    let choices = view.screen.choices();
    let y = at.buttons;
    let mut x = at.card.x + PAD;
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
pub fn hit(area: Rect, view: &View<'_>, col: u16, row: u16) -> Option<usize> {
    buttons(area, view).into_iter().position(|button| {
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
///
/// ```text
/// ╭ Welcome ───────────────────────────╮
/// │                                    │
/// │  siltop                      1 / 3 │
/// │                                    │
/// │  A system monitor for Apple …      │
/// │                                    │
/// │  [ Continue ]                      │
/// │  Enter chooses  ·  Esc skips       │
/// ╰────────────────────────────────────╯
/// ```
pub fn render(frame: &mut Frame, area: Rect, view: &View<'_>, theme: &Theme) {
    frame.render_widget(Clear, area);
    let at = layout(area, view);
    let card = at.card;
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme.border(true))
        .title(Span::styled(
            format!(" {} ", view.screen.title()),
            theme.dim(),
        ))
        .style(theme.fg());
    frame.render_widget(block, card);
    let row = |y: u16| Rect {
        height: 1,
        y,
        ..at.body
    };
    // The wordmark, with where this screen sits in the flow on the right.
    let step = format!("{} / {}", view.screen.step(), Screen::ALL.len());
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "siltop",
            theme.title().add_modifier(Modifier::BOLD),
        ))),
        row(at.wordmark),
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(step, theme.dim()))).right_aligned(),
        row(at.wordmark),
    );
    let mut text = wrap(&body(view), usize::from(at.body.width));
    let room = usize::from(at.body.height);
    if text.len() > room {
        // Cut, and say so: the last line that fits ends in `…`.
        text.truncate(room);
        if let Some(last) = text.last_mut() {
            *last = crate::widgets::processes::fit(
                &format!("{last} …"),
                at.body
                    .width
                    .min(u16::try_from(last.width() + 1).unwrap_or(u16::MAX)),
            );
        }
    }
    let lines: Vec<Line> = text
        .into_iter()
        .map(|text| Line::from(Span::styled(text, theme.fg())))
        .collect();
    frame.render_widget(Paragraph::new(lines), at.body);
    for (i, button) in buttons(area, view).into_iter().enumerate() {
        let focused = i == view.focus;
        let style = if focused { theme.title() } else { theme.dim() };
        let label = format!("[ {} ]", button.label);
        let rect = Rect {
            width: u16::try_from(label.width()).unwrap_or(button.rect.width),
            ..button.rect
        };
        frame.render_widget(Paragraph::new(Span::styled(label, style)), rect);
    }
    frame.render_widget(
        Paragraph::new(Span::styled(HINT, theme.dim())),
        row(at.hint),
    );
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
        let view = View {
            screen: Screen::Updates,
            focus: 0,
            keys: "",
        };
        let buttons = buttons(area, &view);
        assert_eq!(buttons[0].label, "Yes, check");
        assert_eq!(buttons[1].label, "No");
        let no = buttons[1].rect;
        assert_eq!(hit(area, &view, no.x + 1, no.y), Some(1));
        assert_eq!(hit(area, &view, 0, 0), None);
    }

    /// Regression: the body sat against the left border, a column left of
    /// the buttons, under ~10 empty rows. The card is sized to its text,
    /// carries the wordmark, and on the smallest window keeps the whole body.
    #[test]
    fn card_fits_its_content_with_the_buttons_padding() {
        for screen in Screen::ALL {
            for (w, h) in [(40, 12), (80, 24), (160, 45)] {
                let view = View {
                    screen,
                    focus: 0,
                    keys: "m menu · ? keys · T themes",
                };
                let painted = text(&paint(&view, w, h));
                assert!(painted.contains("siltop"), "{screen:?} {w}x{h}\n{painted}");
                assert!(!painted.contains('…'), "{screen:?} {w}x{h}: cut\n{painted}");
                let rows: Vec<&str> = painted.lines().collect();
                let blank = rows
                    .iter()
                    .filter(|r| {
                        r.contains('│')
                            && r.trim_matches(|c| c == '|' || c == ' ' || c == '│')
                                .is_empty()
                    })
                    .count();
                assert!(
                    blank <= 5,
                    "{screen:?} {w}x{h}: {blank} empty rows\n{painted}"
                );
                let at = layout(Rect::new(0, 0, w, h), &view);
                let button = buttons(Rect::new(0, 0, w, h), &view)[0].rect;
                assert_eq!(
                    at.body.x, button.x,
                    "{screen:?} {w}x{h}: text lines up with the buttons"
                );
                assert_eq!(at.body.x, at.card.x + 2);
            }
        }
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
