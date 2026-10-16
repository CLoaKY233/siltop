//! The Timeline page's state: open, refresh from the history store, scrub,
//! change range. Painting and hit geometry live in `siltop_ui`'s
//! `widgets/timeline.rs`; this only moves the numbers they read.

use siltop_core::store::{Span, bin, column_of, column_start, events, last_filled};
use siltop_ui::{TimelineHit, TimelineView, ToastKind, timeline_hit, timeline_plot_width};

use super::{App, STATUS_TTL};
use crate::keymap::Action;

/// Why `H` does nothing during `siltop replay`.
pub(super) const REPLAY_NO_TIMELINE: &str = "no timeline in a replay: ←/→ scrub the trace";

/// Seconds between refreshes while the page is open: one Day bucket.
const REFRESH_S: u64 = 6;

impl App {
    /// `H` and the menu's Timeline item: open the page, or close it.
    pub(super) fn toggle_timeline(&mut self) {
        // The store holds this Mac's history (or this session's), never the
        // trace's: a timeline over a replay would mislabel whose data it is.
        if self.timeline.is_none() && self.source.player().is_some() {
            self.set_toast(
                String::from(REPLAY_NO_TIMELINE),
                ToastKind::Info,
                STATUS_TTL,
            );
            return;
        }
        if self.timeline.take().is_none() {
            self.menu = None;
            self.settings = None;
            self.timeline = Some(TimelineView::default());
            self.refresh_timeline(crate::store_disk::unix_now());
        }
    }

    /// Re-reads the range from the store and re-bins it for the current
    /// width. A cursor on the newest column follows new data; one the user
    /// parked stays on the same moment while the window slides under it
    /// (the column index alone would drift to later times).
    pub(super) fn refresh_timeline(&mut self, now: u64) {
        let Some(span) = self.timeline.as_ref().map(|v| v.span) else {
            return;
        };
        let offset = crate::report::utc_offset_s(now);
        let (from, to) = span.bounds(now, offset);
        let width = timeline_plot_width(self.last_area);
        let buckets = match self.recorder.store().buckets(from, to, span.tier()) {
            Ok(buckets) => buckets,
            Err(err) => {
                self.set_toast(
                    format!("history unreadable: {err}"),
                    ToastKind::Warn,
                    STATUS_TTL,
                );
                Vec::new()
            }
        };
        let saved = self.recorder.persistent();
        let Some(view) = self.timeline.as_mut() else {
            return;
        };
        let columns = bin(&buckets, span.tier(), from, to, width);
        let mut stamps = vec![None; columns.len()];
        for b in &buckets {
            if let Some(slot) = column_of(b.t, from, to, width).and_then(|c| stamps.get_mut(c)) {
                *slot = Some(b.t.max(slot.unwrap_or(0)));
            }
        }
        let following = view.cursor.is_none() || view.cursor == last_filled(&view.columns);
        // The parked moment is remembered exactly: re-deriving it from the
        // column each refresh would round it by up to a column every 6 s and
        // walk the cursor into the past. A cursor the user moved since (its
        // column no longer holds the remembered moment) is re-read from its
        // column's middle, the moment the new column overlaps most.
        let old_width = view.columns.len();
        let parked = match view.cursor {
            Some(c) if !following => Some(
                self.timeline_parked
                    .filter(|&t| column_of(t, view.from, view.to, old_width) == Some(c))
                    .unwrap_or_else(|| column_mid(c, view.from, view.to, old_width)),
            ),
            _ => None,
        };
        self.timeline_parked = parked;
        view.cursor = match parked {
            // Slid off the left edge: hold the oldest column.
            Some(t) => Some(
                column_of(t, from, to, width)
                    .unwrap_or(0)
                    .min(width.saturating_sub(1)),
            ),
            None => last_filled(&columns),
        };
        view.stamps = stamps;
        view.events = events(&columns);
        view.columns = columns;
        view.from = from;
        view.to = to;
        view.utc_offset_s = offset;
        view.saved = saved;
    }

    /// Called per snapshot: refreshes once a new bucket can have closed.
    pub(super) fn tick_timeline(&mut self, now: u64) {
        let due = self
            .timeline
            .as_ref()
            .is_some_and(|v| now >= v.to.saturating_sub(1).saturating_add(REFRESH_S));
        if due {
            self.refresh_timeline(now);
        }
    }

    /// Picks `span` and re-reads the store for it.
    fn set_timeline_span(&mut self, span: Span) {
        if let Some(view) = self.timeline.as_mut() {
            view.span = span;
            view.cursor = None;
        }
        self.refresh_timeline(crate::store_disk::unix_now());
    }

    pub(super) fn handle_timeline(&mut self, action: Action) {
        let Some(view) = self.timeline.as_mut() else {
            return;
        };
        match action {
            Action::Scrub(delta) => view.scrub(delta),
            Action::Top => view.cursor = (!view.columns.is_empty()).then_some(0),
            Action::Bottom => view.cursor = last_filled(&view.columns),
            Action::Range(delta) => {
                let span = view.span.step(delta);
                self.set_timeline_span(span);
            }
            Action::Timeline => self.timeline = None,
            _ => {}
        }
    }

    /// A drag only scrubs: sweeping across a chip or `[×]` presses nothing.
    pub(super) fn drag_timeline(&mut self, col: u16, row: u16) {
        if let Some(TimelineHit::Scrub(c)) = timeline_hit(self.last_area, col, row)
            && let Some(view) = self.timeline.as_mut()
        {
            view.cursor = Some(c.min(view.columns.len().saturating_sub(1)));
        }
    }

    /// A click on the page: a range chip, a close button, or a plot column.
    pub(super) fn click_timeline(&mut self, col: u16, row: u16) {
        match timeline_hit(self.last_area, col, row) {
            Some(TimelineHit::Range(span)) => self.set_timeline_span(span),
            Some(TimelineHit::Close) => self.timeline = None,
            Some(TimelineHit::Scrub(c)) => {
                if let Some(view) = self.timeline.as_mut() {
                    view.cursor = Some(c.min(view.columns.len().saturating_sub(1)));
                }
            }
            None => {}
        }
    }
}

/// The middle second of column `col`: the moment a re-binned column
/// overlaps most, so a slide smaller than half a column never moves it.
fn column_mid(col: usize, from: u64, to: u64, width: usize) -> u64 {
    let start = column_start(col, from, to, width);
    let end = column_start(col.saturating_add(1), from, to, width);
    start + end.saturating_sub(start) / 2
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use ratatui::layout::Rect;
    use siltop_core::Snapshot;
    use siltop_core::store::Span;

    use super::super::tests::app;
    use super::App;
    use crate::keymap::{Action, Mode};

    fn open() -> App {
        let mut app = app();
        app.last_area = Rect::new(0, 0, 120, 40);
        app.reduce(Action::Timeline);
        app
    }

    #[test]
    fn h_opens_and_closes_the_page() {
        let mut app = open();
        assert!(app.timeline.is_some());
        assert_eq!(app.mode(), Mode::Timeline);
        app.reduce(Action::Close);
        assert!(app.timeline.is_none());
        app.reduce(Action::Timeline);
        app.reduce(Action::Cancel);
        assert!(app.timeline.is_none(), "esc closes it too");
    }

    #[test]
    fn the_menu_item_opens_the_page() {
        let mut app = app();
        app.reduce(Action::Menu);
        app.reduce(Action::Timeline);
        assert!(app.timeline.is_some());
        assert!(app.menu.is_none(), "the menu gives way to the page");
    }

    #[test]
    fn the_page_bins_to_the_plot_width() {
        let app = open();
        let view = app.timeline.as_ref().unwrap();
        assert_eq!(
            view.columns.len(),
            siltop_ui::timeline_plot_width(app.last_area)
        );
        assert_eq!(view.span, Span::Hour);
        assert!(!view.saved, "tests never touch the disk");
    }

    #[test]
    fn a_resize_rebins_to_the_new_width() {
        let mut app = open();
        app.reduce(Action::Resize { cols: 80, rows: 30 });
        let want = siltop_ui::timeline_plot_width(Rect::new(0, 0, 80, 30));
        assert_eq!(app.timeline.as_ref().unwrap().columns.len(), want);
    }

    #[test]
    fn range_keys_step_and_clicks_pick() {
        let mut app = app();
        app.last_area = Rect::new(0, 0, 120, 40);
        app.reduce(Action::Timeline);
        app.reduce(Action::Range(1));
        assert_eq!(app.timeline.as_ref().unwrap().span, Span::Today);
        app.reduce(Action::Range(-5));
        assert_eq!(app.timeline.as_ref().unwrap().span, Span::Hour);
        let (rect, span) = siltop_ui::timeline_parts(app.last_area).chips[4];
        app.reduce(Action::Click {
            col: rect.x + 1,
            row: rect.y,
        });
        assert_eq!(span, Span::Month);
        assert_eq!(app.timeline.as_ref().unwrap().span, span);
    }

    #[test]
    fn the_close_buttons_close_the_page_and_a_drag_never_presses_them() {
        let mut app = open();
        let parts = siltop_ui::timeline_parts(app.last_area);
        let close = parts.close.unwrap();
        app.reduce(Action::Drag {
            col: close.x + 1,
            row: close.y,
        });
        assert!(app.timeline.is_some(), "a drag only scrubs");
        app.reduce(Action::Click {
            col: close.x + 1,
            row: close.y,
        });
        assert!(app.timeline.is_none());
        let mut app = open();
        let esc = parts.hint_close.unwrap();
        app.reduce(Action::Click {
            col: esc.x,
            row: esc.y,
        });
        assert!(app.timeline.is_none(), "the hint's esc close is a button");
    }

    /// Session history shows up on the page without a disk store.
    #[test]
    fn session_snapshots_reach_the_page_and_scrub_moves_the_cursor() {
        let mut app = open();
        let mut snap = Snapshot::empty();
        snap.primed = true;
        snap.cpu.scaled = 0.5;
        let now = crate::store_disk::unix_now();
        for i in 0..20 {
            app.recorder.observe(&snap, now - 120 + i * 6);
        }
        app.refresh_timeline(now);
        let view = app.timeline.as_ref().unwrap();
        assert!(view.columns.iter().flatten().count() > 0);
        let before = view.cursor;
        assert!(before.is_some(), "the cursor starts on the newest data");
        app.reduce(Action::Scrub(-3));
        assert_eq!(
            app.timeline.as_ref().unwrap().cursor,
            before.map(|c| c.saturating_sub(3))
        );
        app.reduce(Action::Top);
        assert_eq!(app.timeline.as_ref().unwrap().cursor, Some(0));
    }

    #[test]
    fn a_parked_cursor_survives_a_refresh() {
        let mut app = open();
        app.reduce(Action::Top);
        app.refresh_timeline(crate::store_disk::unix_now());
        assert_eq!(app.timeline.as_ref().unwrap().cursor, Some(0));
    }

    /// Fake clock: the window slides two minutes under a parked cursor,
    /// which must keep pointing at the moment the user chose.
    #[test]
    fn a_parked_cursor_keeps_its_time_while_the_window_slides() {
        use siltop_core::store::column_of;
        let mut app = open();
        let now = 1_790_000_000;
        let mut snap = Snapshot::empty();
        snap.primed = true;
        snap.cpu.scaled = 0.5;
        for i in 0..400 {
            app.recorder.observe(&snap, now - 1_800 + i * 6);
        }
        app.refresh_timeline(now);
        app.reduce(Action::Scrub(-20));
        let view = app.timeline.as_ref().unwrap();
        let width = view.columns.len();
        let parked = super::column_mid(view.cursor.unwrap(), view.from, view.to, width);
        app.refresh_timeline(now + 120);
        let view = app.timeline.as_ref().unwrap();
        assert!(view.from > now - 3_600, "the window slid");
        assert_eq!(
            view.cursor,
            column_of(parked, view.from, view.to, view.columns.len()),
            "the cursor still covers the moment it was parked on"
        );
    }

    /// The real cadence: one refresh every 6 s. Mapping a column's start
    /// onto each new axis would lose most of a column per refresh and walk
    /// the cursor into the past; the parked moment must hold for minutes.
    #[test]
    fn a_parked_cursor_does_not_creep_across_many_small_refreshes() {
        use siltop_core::store::column_of;
        let mut app = open();
        let now = 1_790_000_000;
        let mut snap = Snapshot::empty();
        snap.primed = true;
        snap.cpu.scaled = 0.5;
        for i in 0..400 {
            app.recorder.observe(&snap, now - 1_800 + i * 6);
        }
        app.refresh_timeline(now);
        app.reduce(Action::Scrub(-20));
        let view = app.timeline.as_ref().unwrap();
        let parked =
            super::column_mid(view.cursor.unwrap(), view.from, view.to, view.columns.len());
        for step in 1..=20 {
            app.refresh_timeline(now + step * super::REFRESH_S);
        }
        let view = app.timeline.as_ref().unwrap();
        assert_eq!(
            view.cursor,
            column_of(parked, view.from, view.to, view.columns.len()),
            "twenty 6 s refreshes later the cursor still covers its moment"
        );
        app.reduce(Action::Scrub(-1));
        let moved = app.timeline.as_ref().unwrap().cursor;
        app.refresh_timeline(now + 21 * super::REFRESH_S);
        assert_eq!(
            app.timeline.as_ref().unwrap().cursor,
            moved,
            "a scrub re-parks the cursor on its new moment"
        );
    }

    #[test]
    fn a_plot_click_moves_the_cursor() {
        let mut app = open();
        let parts = siltop_ui::timeline_parts(app.last_area);
        app.reduce(Action::Click {
            col: parts.plot_x + 7,
            row: parts.graphs.y + 1,
        });
        assert_eq!(app.timeline.as_ref().unwrap().cursor, Some(7));
    }

    /// Fake clock: a laptop session that unplugs, then heats up to serious.
    /// The page gets a battery row and both events without a disk store.
    #[test]
    fn a_laptop_session_shows_its_unplug_and_thermal_events() {
        use siltop_core::store::{Event, Series, present};
        use siltop_core::{PowerSource, Thermal};
        let mut app = open();
        let now = 1_790_000_000;
        let mut snap = Snapshot::empty();
        snap.primed = true;
        snap.battery.present = true;
        snap.battery.percent = Some(80);
        snap.battery.watts = Some(15.0);
        snap.battery.source = PowerSource::Ac;
        for i in 0..60 {
            if i == 20 {
                snap.battery.source = PowerSource::Battery;
                snap.battery.watts = Some(-9.0);
            }
            if i == 40 {
                snap.thermal = Thermal::Serious;
            }
            app.recorder.observe(&snap, now - 360 + i * 6);
        }
        app.refresh_timeline(now);
        let view = app.timeline.as_ref().unwrap();
        assert!(present(&view.columns, Series::BatteryPct));
        let events: Vec<Event> = view.events.iter().map(|(_, e)| *e).collect();
        assert!(events.contains(&Event::Unplugged), "{events:?}");
        assert!(events.contains(&Event::Thermal(2)), "{events:?}");
    }
}
