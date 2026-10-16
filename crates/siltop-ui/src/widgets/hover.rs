//! What lights up under the pointer. Every rect comes from the same
//! geometry the click handlers read (corner marks from the layout plan,
//! chips from the footer and dossier layouts, menu and settings rows from
//! their own `rows`/`target`), so what lights is exactly what a click hits.

use ratatui::layout::{Position, Rect};

use super::{AppView, detail_actions, footer, menu, menu_area, processes, settings, timeline};
use crate::layout::plan;

/// The rect of the button under `(col, row)`, or `None` over anything that
/// is not a button (graphs, text, rows that only select). Topmost first:
/// the same order the overlays stack in.
#[must_use]
pub fn hover_rect(view: &AppView<'_>, area: Rect, col: u16, row: u16) -> Option<Rect> {
    let on = |r: &Rect| r.contains(Position::new(col, row));
    if let Some(state) = view.settings {
        if view.help {
            return None;
        }
        return settings::target(area, state, col, row).map(|(r, _)| r);
    }
    if view.timeline.is_some() {
        if view.help {
            return None;
        }
        // The range chips and the close buttons; the plot only scrubs.
        let parts = timeline::parts(area);
        return parts
            .chips
            .into_iter()
            .map(|(r, _)| r)
            .chain(parts.close)
            .chain(parts.hint_close)
            .find(on);
    }
    if view.help {
        return crate::layout::card_close(super::help_rect(area, view.help_groups)).filter(on);
    }
    if let Some(popup) = &view.menu {
        let area = menu_area(area);
        return menu::rows(area, popup)
            .into_iter()
            .filter(|(_, i)| !popup.entries[*i].header)
            .map(|(r, _)| r)
            .chain(menu::close_rect(area, popup))
            .find(on);
    }
    if let Some(menu) = &view.signal_menu {
        let area = super::signal_menu_area(view, area);
        let card = processes::signal_menu::menu_rect(area, menu);
        if let Some(close) = crate::layout::card_close(card).filter(on) {
            return Some(close);
        }
        return processes::signal_menu::row_at(area, menu, col, row).map(|i| {
            let inner = card.inner(ratatui::layout::Margin::new(1, 1));
            Rect::new(
                inner.x,
                inner.y + u16::try_from(i).unwrap_or(0),
                inner.width,
                1,
            )
        });
    }
    let layout = plan(area, view.surface, view.flags());
    // the footer is painted (and clicked) for the surface the layout fell
    // back to, not the locked one
    let painted = AppView {
        surface: layout.surface,
        ..view.clone()
    };
    if let Some(found) = footer::footer_target(&painted, layout.footer, col, row) {
        return Some(found.0);
    }
    if view.detail_pid.is_some()
        && let Some(pane) = layout.processes
    {
        return detail_actions(pane, view.read_only)
            .into_iter()
            .map(|(r, _)| r)
            .chain(super::detail_tabs(pane, view).into_iter().map(|(r, _)| r))
            .chain(super::detail_close(pane))
            .find(on);
    }
    if let Some(pane) = layout.processes
        && let Some(clear) = processes::filter_clear(view, processes::search_row(pane)).filter(on)
    {
        return Some(clear);
    }
    if let Some(close) = layout.close_hit().filter(on) {
        return Some(close);
    }
    if layout.expanded.is_none() {
        return crate::layout::Panel::ALL
            .into_iter()
            .filter_map(|panel| layout.expand_hit(panel))
            .find(on);
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::layout::{Hit, Panel};
    use crate::widgets::tests_support::fixture;
    use crate::widgets::{MenuHit, render_app, settings_hit};
    use crate::widgets::{SettingsState, footer_hit, hit_at, menu_hit};
    use ratatui::{Terminal, backend::TestBackend};

    const SCREEN: Rect = Rect::new(0, 0, 120, 36);

    #[test]
    fn corner_marks_light_exactly_where_they_click() {
        let fx = fixture("");
        let view = fx.view();
        for pos in SCREEN.positions() {
            let lit = hover_rect(&view, SCREEN, pos.x, pos.y);
            let hit = hit_at(&view, SCREEN, pos.x, pos.y);
            if matches!(hit, Some(Hit::Expand(_) | Hit::ExpandClose)) {
                assert!(lit.is_some_and(|r| r.contains(pos)), "{pos:?} {hit:?}");
            }
        }
    }

    #[test]
    fn graphs_and_panel_bodies_never_light() {
        let fx = fixture("");
        let view = fx.view();
        let layout = plan(SCREEN, view.surface, view.flags());
        let cpu = layout.panel(Panel::Cpu).unwrap();
        let mid = Position::new(cpu.x + cpu.width / 2, cpu.y + cpu.height / 2);
        assert_eq!(hover_rect(&view, SCREEN, mid.x, mid.y), None);
    }

    #[test]
    fn every_footer_chip_lights_on_its_own_rect() {
        let fx = fixture("");
        let view = fx.view();
        let layout = plan(SCREEN, view.surface, view.flags());
        let y = layout.footer.y;
        let mut seen = 0;
        for x in 0..SCREEN.width {
            let hit = footer_hit(&view, layout.footer, x, y);
            let lit = hover_rect(&view, SCREEN, x, y);
            assert_eq!(hit.is_some(), lit.is_some(), "col {x}");
            if let Some(r) = lit {
                assert!(r.contains(Position::new(x, y)));
                seen += 1;
            }
        }
        assert!(seen > 0, "the footer has chips");
    }

    #[test]
    fn footer_lights_for_the_fallback_surface_when_work_is_locked() {
        // Seen live: a locked Work surface under 60 columns falls back to
        // Glance; hover lit blank space and `q quit` never lit.
        let fx = fixture("");
        let mut view = fx.view();
        view.surface = siltop_core::Surface::Work;
        for screen in [Rect::new(0, 0, 59, 30), Rect::new(0, 0, 45, 20)] {
            let layout = plan(screen, view.surface, view.flags());
            assert_ne!(layout.surface, view.surface, "{screen:?} falls back");
            let painted = AppView {
                surface: layout.surface,
                ..view.clone()
            };
            let y = layout.footer.y;
            for x in 0..screen.width {
                let hit = footer_hit(&painted, layout.footer, x, y);
                let lit = hover_rect(&view, screen, x, y);
                assert_eq!(hit.is_some(), lit.is_some(), "{screen:?} col {x}");
                assert!(lit.is_none_or(|r| r.contains(Position::new(x, y))));
            }
        }
    }

    #[test]
    fn menu_items_light_and_the_card_border_does_not() {
        let fx = fixture("");
        let mut fx = fx;
        fx.menu = Some(1);
        let view = fx.view();
        let menu = view.menu.unwrap();
        for pos in SCREEN.positions() {
            let lit = hover_rect(&view, SCREEN, pos.x, pos.y);
            match menu_hit(menu_area(SCREEN), &menu, pos.x, pos.y) {
                MenuHit::Item(_) | MenuHit::Close => assert!(lit.is_some(), "{pos:?}"),
                MenuHit::Card | MenuHit::Outside => assert_eq!(lit, None, "{pos:?}"),
            }
        }
    }

    #[test]
    fn settings_rows_light_where_they_click() {
        let fx = fixture("");
        let state = SettingsState::default();
        let mut view = fx.view();
        view.settings = Some(&state);
        for pos in SCREEN.positions() {
            let lit = hover_rect(&view, SCREEN, pos.x, pos.y);
            let hit = settings_hit(SCREEN, &state, pos.x, pos.y);
            assert_eq!(lit.is_some(), hit.is_some(), "{pos:?}");
        }
    }

    #[test]
    fn help_open_lights_only_its_close_button() {
        let fx = fixture("");
        let mut view = fx.view();
        view.help = true;
        let close = crate::layout::card_close(crate::widgets::help_rect(SCREEN, view.help_groups));
        for p in SCREEN.positions() {
            let lit = hover_rect(&view, SCREEN, p.x, p.y);
            assert_eq!(lit.is_some(), close.is_some_and(|r| r.contains(p)), "{p:?}");
        }
    }

    #[test]
    fn a_lit_target_is_drawn_in_the_highlight_ink_without_a_fill() {
        let fx = fixture("");
        let mut view = fx.view();
        let layout = plan(SCREEN, view.surface, view.flags());
        let corner = layout.expand_hit(Panel::Cpu).unwrap();
        view.hover = Some(corner);
        let mut t = Terminal::new(TestBackend::new(SCREEN.width, SCREEN.height)).unwrap();
        t.draw(|f| render_app(f, &view)).unwrap();
        let plain = {
            let mut t = Terminal::new(TestBackend::new(SCREEN.width, SCREEN.height)).unwrap();
            view.hover = None;
            t.draw(|f| render_app(f, &view)).unwrap();
            t.backend().buffer().clone()
        };
        let buf = t.backend().buffer();
        for pos in corner.positions() {
            assert_eq!(buf[pos].fg, view.theme.hi, "{pos:?}");
            assert_eq!(buf[pos].bg, plain[pos].bg, "{pos:?}");
        }
    }
}
