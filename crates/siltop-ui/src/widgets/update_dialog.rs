//! The update dialog. Painting and clicks share [`buttons`].

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;

pub(crate) const CHOICES: [&str; 4] = [
    "Update now",
    "Always update automatically",
    "Skip this version",
    "Later",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct View<'a> {
    pub version: &'a str,
    pub notes: &'a str,
    pub focus: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Button {
    pub label: &'static str,
    pub rect: Rect,
}

fn card(area: Rect) -> Rect {
    let width = area.width.clamp(36, 68);
    let height = area.height.clamp(10, 14);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

#[must_use]
pub(crate) fn buttons(area: Rect) -> Vec<Button> {
    let card = card(area);
    let mut y = card.y + 5;
    let mut out = Vec::with_capacity(CHOICES.len());
    for label in CHOICES {
        let width = u16::try_from(label.width())
            .unwrap_or(u16::MAX)
            .saturating_add(4);
        out.push(Button {
            label,
            rect: Rect {
                x: card.x + 2,
                y,
                width,
                height: 1,
            },
        });
        y = y.saturating_add(1);
    }
    out
}

/// The dialog's `[×]`: closes it, as Esc and Later do.
#[must_use]
pub fn close_rect(area: Rect) -> Option<Rect> {
    crate::layout::card_close(card(area))
}

#[must_use]
pub fn hit(area: Rect, col: u16, row: u16) -> Option<usize> {
    buttons(area).into_iter().position(|button| {
        col >= button.rect.x
            && col < button.rect.x.saturating_add(button.rect.width)
            && row == button.rect.y
    })
}

pub fn render(frame: &mut Frame, area: Rect, view: &View<'_>, theme: &Theme) {
    let card = card(area);
    frame.render_widget(Clear, card);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(format!(" update {} ", view.version))
        .style(theme.fg());
    frame.render_widget(block, card);
    crate::layout::paint_card_close(frame, card, theme);
    let notes = if view.notes.is_empty() {
        "A newer siltop is available."
    } else {
        view.notes
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(notes, theme.dim()))),
        Rect {
            x: card.x + 2,
            y: card.y + 2,
            width: card.width.saturating_sub(4),
            height: 2,
        },
    );
    for (i, button) in buttons(area).into_iter().enumerate() {
        let style = if i == view.focus {
            theme.title()
        } else {
            theme.dim()
        };
        frame.render_widget(
            Paragraph::new(Span::styled(format!("[ {} ]", button.label), style)),
            button.rect,
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn every_choice_is_drawn_and_clickable() {
        let area = Rect::new(0, 0, 80, 24);
        let buttons = buttons(area);
        assert_eq!(buttons.len(), CHOICES.len());
        for (i, choice) in CHOICES.iter().enumerate() {
            assert_eq!(buttons[i].label, *choice);
            let rect = buttons[i].rect;
            assert_eq!(hit(area, rect.x + 1, rect.y), Some(i));
        }
        assert_eq!(hit(area, 0, 0), None);
    }
}
