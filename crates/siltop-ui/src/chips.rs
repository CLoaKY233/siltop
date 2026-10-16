use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use siltop_core::Surface;
use unicode_width::UnicodeWidthStr;

use crate::layout::{Hit, Panel};
use crate::theme::Theme;
use crate::widgets::AppView;

const GAP: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChipStyle {
    Plain,
    /// A state the user should notice, such as `f paused`.
    State,
}

/// One clickable `key verb` hint. The same list paints the footer and the
/// dossier action row and answers their clicks, so the two cannot drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Chip {
    pub key: &'static str,
    pub verb: &'static str,
    pub action: Hit,
    pub style: ChipStyle,
    /// `? help` and `q quit` survive every width; the rest drop from the right.
    pub essential: bool,
}

impl Chip {
    const fn new(key: &'static str, verb: &'static str, action: Hit) -> Self {
        Self {
            key,
            verb,
            action,
            style: ChipStyle::Plain,
            essential: false,
        }
    }

    const fn essential(mut self) -> Self {
        self.essential = true;
        self
    }

    const fn state(mut self) -> Self {
        self.style = ChipStyle::State;
        self
    }

    #[must_use]
    pub(crate) fn width(&self) -> u16 {
        u16::try_from(self.key.width() + 1 + self.verb.width()).unwrap_or(u16::MAX)
    }
}

/// The key each footer hint names, from the session's key profile, so an
/// htop hand reads `F1 help` and `F10 quit`. An empty key hides its chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyHints {
    pub help: &'static str,
    pub quit: &'static str,
    pub search: &'static str,
    pub kill: &'static str,
    pub settings: &'static str,
    pub pause: &'static str,
    pub work: &'static str,
}

impl KeyHints {
    /// The default profile's keys.
    pub const SILTOP: Self = Self {
        help: "?",
        quit: "q",
        search: "f",
        kill: "x",
        settings: "s",
        pause: "p",
        work: "v",
    };
}

impl Default for KeyHints {
    fn default() -> Self {
        Self::SILTOP
    }
}

pub(crate) const HELP: Chip = Chip::new("?", "help", Hit::Help).essential();
pub(crate) const QUIT: Chip = Chip::new("q", "quit", Hit::Quit).essential();
/// Home screens only; the first to drop, since the `≡` menu also has it.
pub(crate) const SETTINGS: Chip = Chip::new("s", "settings", Hit::Settings);
const RESUME: Chip = Chip::new("p", "resume", Hit::Pause).state();

/// Closes the dossier; never dropped, so the card always shows its way out.
const CLOSE: Chip = Chip::new("esc", "close", Hit::DetailClose).essential();

/// Actions inside the process dossier, in paint order. The signals drop
/// from the right as the card narrows; `esc close` stays.
pub(crate) const DOSSIER: [Chip; 4] = [
    Chip::new("x", "quit", Hit::DetailTerm),
    Chip::new("K", "force quit", Hit::DetailKill),
    Chip::new("i", "interrupt", Hit::DetailInterrupt),
    CLOSE,
];

/// The dossier's actions; a replay keeps only `esc close`, since its pids
/// are history and nothing may be signalled.
pub(crate) fn dossier(read_only: bool) -> Vec<Chip> {
    DOSSIER
        .into_iter()
        .filter(|c| !read_only || c.action == Hit::DetailClose)
        .collect()
}

/// `siltop fleet`'s footer on the card grid.
pub(crate) const FLEET_GRID: [Chip; 2] = [
    Chip::new("enter", "processes", Hit::Expand(Panel::Processes)),
    QUIT,
];
/// `siltop fleet`'s footer over one host's processes.
pub(crate) const FLEET_PROCS: [Chip; 2] = [Chip::new("esc", "back", Hit::DetailClose), QUIT];

/// Footer chips for the current context. `? help` leads and `q quit` ends
/// every context except the kill confirmation and the dossier, whose card
/// row carries its actions and where `q` closes the card, not the app.
/// `footer` is the frame's last row, so it also says whether Work fits.
#[must_use]
pub(crate) fn footer_chips(view: &AppView<'_>, footer: Rect) -> Vec<Chip> {
    if view.confirm_kill {
        return vec![
            Chip::new("y", "yes", Hit::ConfirmYes).essential(),
            Chip::new("n", "cancel", Hit::ConfirmNo).essential(),
        ];
    }
    let keys = view.keys;
    let help = Chip {
        key: keys.help,
        ..HELP
    };
    let quit = Chip {
        key: keys.quit,
        ..QUIT
    };
    let resume = Chip {
        key: keys.pause,
        ..RESUME
    };
    let mut chips = vec![help];
    if view.detail_pid.is_some() {
        chips.push(CLOSE);
        if view.frozen {
            chips.push(resume);
        }
        return chips;
    }
    if let Some(panel) = view.expanded {
        if crate::layout::hop_ring(view.flags(), panel).len() > 1 {
            chips.push(Chip::new("tab", "related", Hit::HopNext));
        }
        chips.push(Chip::new("esc", "home", Hit::ExpandClose));
    } else {
        let screen = Rect::new(0, 0, footer.right(), footer.bottom());
        if view.surface == Surface::Glance && crate::layout::work_fits(screen) {
            chips.push(Chip::new(keys.work, "work", Hit::Work));
        }
        if process_actions_visible(view) {
            chips.push(Chip::new(keys.search, "filter", Hit::Search));
            if !view.read_only {
                chips.push(Chip::new(keys.kill, "kill", Hit::Kill));
            }
        }
        chips.push(Chip {
            key: keys.settings,
            ..SETTINGS
        });
    }
    if view.frozen {
        chips.push(resume);
    }
    chips.push(quit);
    chips.retain(|c| !c.key.is_empty());
    chips
}

fn process_actions_visible(view: &AppView<'_>) -> bool {
    !view.searching
        && view.expanded.is_none()
        && view.surface != Surface::Glance
        && view.focus.panel() == Panel::Processes
}

/// Lays chips out left to right from `x`, each preceded by a two-cell gap.
/// Chips that do not fit are dropped, rightmost non-essential first, so
/// what is painted is exactly what can be clicked.
#[must_use]
pub(crate) fn place(chips: &[Chip], area: Rect, x: u16) -> Vec<(Rect, Chip)> {
    let room = area.x.saturating_add(area.width).saturating_sub(x);
    let mut kept: Vec<Chip> = chips.to_vec();
    while total(&kept) > room {
        let Some(drop) = kept.iter().rposition(|c| !c.essential) else {
            break;
        };
        kept.remove(drop);
    }
    let right = area.x.saturating_add(area.width);
    let mut cursor = x;
    let mut out = Vec::new();
    for chip in kept {
        let start = cursor.saturating_add(GAP);
        let end = start.saturating_add(chip.width());
        if end > right {
            break;
        }
        out.push((
            Rect {
                x: start,
                y: area.y,
                width: chip.width(),
                height: 1,
            },
            chip,
        ));
        cursor = end;
    }
    out
}

/// The cells `chips` take when placed, gaps included.
pub(crate) fn total(chips: &[Chip]) -> u16 {
    chips
        .iter()
        .map(|c| c.width().saturating_add(GAP))
        .fold(0, u16::saturating_add)
}

/// Spans for placed chips, padded so each lands on its own rect.
#[must_use]
pub(crate) fn spans(placed: &[(Rect, Chip)], from: u16, theme: &Theme) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut cursor = from;
    for (rect, chip) in placed {
        if rect.x > cursor {
            out.push(Span::raw(" ".repeat(usize::from(rect.x - cursor))));
        }
        let key = match chip.style {
            ChipStyle::Plain => theme.title(),
            ChipStyle::State => theme.warn_style(),
        };
        out.push(Span::styled(chip.key.to_owned(), key));
        out.push(Span::styled(format!(" {}", chip.verb), theme.dim()));
        cursor = rect.x.saturating_add(rect.width);
    }
    out
}

/// The chip under the pointer with its rect, for clicks and hover alike.
#[must_use]
pub(crate) fn target(placed: &[(Rect, Chip)], col: u16, row: u16) -> Option<(Rect, Hit)> {
    placed
        .iter()
        .find(|(rect, _)| row == rect.y && col >= rect.x && col < rect.x.saturating_add(rect.width))
        .map(|(rect, chip)| (*rect, chip.action))
}

#[must_use]
pub(crate) fn line(placed: &[(Rect, Chip)], from: u16, theme: &Theme) -> Line<'static> {
    Line::from(spans(placed, from, theme))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The last row of a 120x40 window.
    const BIG: Rect = Rect::new(0, 39, 120, 1);

    fn keys_of(chips: &[Chip]) -> Vec<(&'static str, &'static str)> {
        chips.iter().map(|c| (c.key, c.verb)).collect()
    }

    #[test]
    fn small_window_regression_hides_v_work_where_work_cannot_show() {
        let mut fx = crate::widgets::tests_support::fixture("");
        fx.surface = Surface::Glance;
        let small = Rect::new(0, 19, 55, 1);
        assert!(!keys_of(&footer_chips(&fx.view(), small)).contains(&("v", "work")));
        let short = Rect::new(0, 14, 120, 1);
        assert!(!keys_of(&footer_chips(&fx.view(), short)).contains(&("v", "work")));
        assert!(keys_of(&footer_chips(&fx.view(), BIG)).contains(&("v", "work")));
    }

    #[test]
    fn dossier_regression_footer_offers_close_not_app_quit() {
        let mut fx = crate::widgets::tests_support::fixture("");
        fx.detail_pid = Some(904);
        let keys = keys_of(&footer_chips(&fx.view(), BIG));
        assert_eq!(keys, vec![("?", "help"), ("esc", "close")]);
    }

    #[test]
    fn dossier_regression_card_row_keeps_close_at_every_width() {
        for width in 0..=80_u16 {
            let placed = place(&DOSSIER, Rect::new(0, 0, width, 1), 0);
            let keys: Vec<&str> = placed.iter().map(|(_, c)| c.key).collect();
            if width >= CLOSE.width() + GAP {
                assert!(keys.contains(&"esc"), "{width}: {keys:?}");
            }
            if keys.contains(&"K") {
                assert!(keys.contains(&"x"), "{width}: signals drop from the right");
            }
        }
    }

    #[test]
    fn tab_related_regression_only_when_the_ring_has_somewhere_to_go() {
        let fx = crate::widgets::tests_support::fixture("");
        let mut view = fx.view();
        view.expanded = Some(Panel::Net);
        view.show_disk = false;
        assert!(!keys_of(&footer_chips(&view, BIG)).contains(&("tab", "related")));
        view.expanded = Some(Panel::Mem);
        assert!(keys_of(&footer_chips(&view, BIG)).contains(&("tab", "related")));
    }

    #[test]
    fn narrow_rows_keep_help_and_quit() {
        let chips = [HELP, DOSSIER[0], DOSSIER[1], DOSSIER[2], DOSSIER[3], QUIT];
        let placed = place(&chips, Rect::new(0, 0, 30, 1), 0);
        let keys: Vec<&str> = placed.iter().map(|(_, c)| c.key).collect();
        assert_eq!(keys.first(), Some(&"?"));
        assert_eq!(keys.last(), Some(&"q"));
        let end = placed.last().map(|(r, _)| r.x + r.width).unwrap();
        assert!(end <= 30);
    }

    #[test]
    fn hit_covers_exactly_the_chip_text() {
        let placed = place(&[HELP, QUIT], Rect::new(0, 5, 40, 1), 0);
        assert_eq!(placed[0].0, Rect::new(2, 5, 6, 1));
        assert_eq!(target(&placed, 2, 5).map(|(_, a)| a), Some(Hit::Help));
        assert_eq!(target(&placed, 7, 5).map(|(_, a)| a), Some(Hit::Help));
        assert_eq!(
            target(&placed, 8, 5).map(|(_, a)| a),
            None,
            "the gap is not a chip"
        );
        assert_eq!(target(&placed, 10, 5).map(|(_, a)| a), Some(Hit::Quit));
        assert_eq!(target(&placed, 10, 4).map(|(_, a)| a), None);
    }

    #[test]
    fn place_never_overflows_and_keeps_order_for_any_width() {
        let chips = [HELP, DOSSIER[0], DOSSIER[1], DOSSIER[2], DOSSIER[3], QUIT];
        for width in 0..=120_u16 {
            for x in [0_u16, 3, 17] {
                let area = Rect::new(0, 0, width, 1);
                let placed = place(&chips, area, x);
                for pair in placed.windows(2) {
                    assert!(pair[0].0.x + pair[0].0.width < pair[1].0.x, "{width}");
                }
                if let Some((r, _)) = placed.last() {
                    assert!(r.x + r.width <= width, "{width} {x}");
                }
                let order: Vec<usize> = placed
                    .iter()
                    .map(|(_, c)| chips.iter().position(|k| k == c).unwrap())
                    .collect();
                assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
            }
        }
    }

    #[test]
    fn dossier_chips_use_mac_wording_and_keymap_keys() {
        let pairs: Vec<(&str, &str, Hit)> =
            DOSSIER.iter().map(|c| (c.key, c.verb, c.action)).collect();
        assert_eq!(
            pairs,
            vec![
                ("x", "quit", Hit::DetailTerm),
                ("K", "force quit", Hit::DetailKill),
                ("i", "interrupt", Hit::DetailInterrupt),
                ("esc", "close", Hit::DetailClose),
            ]
        );
    }

    #[test]
    fn footer_chips_per_context_use_current_keys() {
        let mut fx = crate::widgets::tests_support::fixture("");
        fx.surface = Surface::Glance;
        fx.frozen = true;
        let keys: Vec<(&str, Hit)> = footer_chips(&fx.view(), BIG)
            .iter()
            .map(|c| (c.key, c.action))
            .collect();
        assert!(keys.contains(&("v", Hit::Work)), "{keys:?}");
        assert!(keys.contains(&("p", Hit::Pause)), "{keys:?}");
        assert!(
            !keys.iter().any(|(k, _)| *k == "w" || *k == "f"),
            "{keys:?}"
        );
        let mut fx = crate::widgets::tests_support::fixture("");
        fx.confirm_kill = true;
        let keys: Vec<(&str, Hit)> = footer_chips(&fx.view(), BIG)
            .iter()
            .map(|c| (c.key, c.action))
            .collect();
        assert_eq!(keys, vec![("y", Hit::ConfirmYes), ("n", Hit::ConfirmNo)]);
        let fx = crate::widgets::tests_support::fixture("");
        let keys: Vec<(&str, Hit)> = footer_chips(&fx.view(), BIG)
            .iter()
            .map(|c| (c.key, c.action))
            .collect();
        assert!(
            keys.contains(&("f", Hit::Search)),
            "filter chip on the table: {keys:?}"
        );
        assert!(keys.contains(&("x", Hit::Kill)), "{keys:?}");
    }

    #[test]
    fn replay_footer_offers_no_kill_chips() {
        let mut fx = crate::widgets::tests_support::fixture("");
        let replay = |fx: &crate::widgets::tests_support::Fixture| -> Vec<Hit> {
            let view = AppView {
                read_only: true,
                ..fx.view()
            };
            footer_chips(&view, BIG).iter().map(|c| c.action).collect()
        };
        let home = replay(&fx);
        assert!(home.contains(&Hit::Search), "{home:?}");
        assert!(!home.contains(&Hit::Kill), "{home:?}");
        fx.detail_pid = Some(1);
        let dossier = replay(&fx);
        assert!(dossier.contains(&Hit::DetailClose), "{dossier:?}");
        for hit in [Hit::DetailTerm, Hit::DetailKill, Hit::DetailInterrupt] {
            assert!(!dossier.contains(&hit), "{dossier:?}");
        }
    }
}
