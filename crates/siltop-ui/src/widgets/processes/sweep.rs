//! Size sweep (review wave 1): every UI state drawn and hit-tested at every
//! terminal size a user can make, including the absurd ones. Panics are
//! collected, not propagated, so one run lists every failing size.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Once;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use siltop_core::{Snapshot, Surface};

use crate::layout::{Panel, plan};
use crate::widgets::golden::m5_pro;
use crate::widgets::tests_support::{Fixture, fixture};
use crate::widgets::{
    Anchor, AppView, Focus, detail_actions, detail_rect, footer_hit, hit_at, hop_hit, render_app,
};

const PANELS: [Panel; 7] = [
    Panel::Cpu,
    Panel::Gpu,
    Panel::Mem,
    Panel::Net,
    Panel::Disk,
    Panel::Fans,
    Panel::Processes,
];

fn busiest(fx: &Fixture) -> u32 {
    fx.snap
        .processes
        .iter()
        .max_by(|a, b| {
            a.cpu
                .partial_cmp(&b.cpu)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map_or(1, |p| p.pid)
}

fn with(name: &str, edit: impl FnOnce(&mut Fixture)) -> (String, Fixture) {
    let mut fx = m5_pro();
    edit(&mut fx);
    (name.to_owned(), fx)
}

fn dossier(fx: &mut Fixture) {
    let pid = busiest(fx);
    fx.proc.anchor = Anchor::Pinned(pid);
    fx.detail_pid = Some(pid);
}

fn states() -> Vec<(String, Fixture)> {
    let mut out = Vec::new();
    for surface in [Surface::Work, Surface::Glance] {
        out.push(with(&format!("{surface:?}"), |fx| fx.surface = surface));
        for panel in PANELS {
            out.push(with(&format!("expanded {panel:?} {surface:?}"), |fx| {
                fx.surface = surface;
                fx.expanded = Some(panel);
                fx.focus = Focus::from_panel(panel);
            }));
        }
    }
    out.push(with("help", |fx| fx.help = true));
    out.push(with("settings", |fx| fx.settings = true));
    out.push(with("dossier", dossier));
    out.push(with("dossier expanded", |fx| {
        dossier(fx);
        fx.expanded = Some(Panel::Processes);
    }));
    out.push(with("confirm", |fx| {
        fx.confirm_kill = true;
        fx.confirm_pid = Some(busiest(fx));
    }));
    out.push(with("confirm in dossier", |fx| {
        dossier(fx);
        fx.confirm_kill = true;
        fx.confirm_pid = fx.detail_pid;
    }));
    out.push(with("zero matches", |fx| {
        fx.searching = true;
        fx.focus = Focus::Search;
        fx.proc.filter = String::from("zzzz");
    }));
    out.push(with("unicode filter", |fx| {
        fx.searching = true;
        fx.proc.filter = String::from("é日本🦀");
    }));
    out.push(with("wild values", |fx| {
        for p in &mut fx.snap.processes {
            p.name = "日本語🦀".repeat(20);
            p.cpu = Some(f32::NAN);
            p.mem_bytes = Some(u64::MAX);
            p.threads = Some(u32::MAX);
            p.start_unix = i64::MIN;
            p.command = Some("x".repeat(5000));
        }
        dossier(fx);
    }));
    out.push(with("no processes", |fx| fx.snap.processes.clear()));
    let mut empty = fixture("");
    empty.snap = Snapshot::empty();
    out.push((String::from("empty"), empty));
    out
}

/// Sizes that hit every layout break: the tiny ones, both sides of the
/// dossier's minimum, the surface fallbacks and a few wide screens.
const QUICK_WIDTHS: [u16; 20] = [
    1, 2, 3, 5, 8, 13, 20, 21, 22, 23, 24, 30, 40, 59, 60, 80, 100, 120, 160, 200,
];
const QUICK_HEIGHTS: [u16; 10] = [1, 2, 3, 4, 6, 9, 12, 16, 24, 40];
const FULL_HEIGHTS: [u16; 22] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 14, 16, 18, 20, 24, 27, 33, 40, 50, 60,
];

fn exercise(view: &AppView<'_>, w: u16, h: u16) {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|frame| render_app(frame, view)).unwrap();
    let area = Rect::new(0, 0, w, h);
    let planned = plan(area, view.surface, view.flags());
    let footer_view = AppView {
        surface: planned.surface,
        ..view.clone()
    };
    if let Some(pane) = planned.processes {
        let card = detail_rect(pane);
        assert_eq!(card.intersection(pane), card, "card leaves the pane");
        for (rect, _) in detail_actions(pane, view.read_only) {
            assert_eq!(rect.intersection(pane), rect, "chip leaves the pane");
        }
    }
    // every cell on small screens, a lattice on big ones
    let step = if u32::from(w) * u32::from(h) <= 400 {
        1
    } else {
        5
    };
    for row in (0..h).step_by(step) {
        for col in (0..w).step_by(step) {
            let _ = hit_at(view, area, col, row);
            let _ = footer_hit(&footer_view, planned.footer, col, row);
            if let Some(panel) = view.expanded
                && let Some(rect) = planned.panel(panel)
            {
                let _ = hop_hit(rect, view, col, row);
            }
        }
    }
}

thread_local! {
    /// Set while this thread's panics are expected and collected.
    static QUIET: Cell<bool> = const { Cell::new(false) };
}

/// Runs `f` with this thread's panic messages silenced. The hook is
/// process-wide and libtest runs every other test beside the sweep, so it
/// is wrapped once, never swapped out: other threads' panics still reach
/// the previous hook (their failure messages and insta diffs survive), and
/// a second sweep running alongside cannot restore a no-op for good.
fn quiet_panics<T>(f: impl FnOnce() -> T) -> T {
    static WRAP: Once = Once::new();
    struct Loud;
    impl Drop for Loud {
        fn drop(&mut self) {
            QUIET.with(|q| q.set(false));
        }
    }
    WRAP.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !QUIET.with(Cell::get) {
                previous(info);
            }
        }));
    });
    QUIET.with(|q| q.set(true));
    let _loud = Loud;
    f()
}

/// Every state at every size; the failing ones, named.
fn sweep(widths: &[u16], heights: &[u16]) -> Vec<String> {
    quiet_panics(|| sweep_loud(widths, heights))
}

fn sweep_loud(widths: &[u16], heights: &[u16]) -> Vec<String> {
    let mut failures = Vec::new();
    for (name, fx) in states() {
        let view = fx.view();
        for &w in widths {
            for &h in heights {
                if catch_unwind(AssertUnwindSafe(|| exercise(&view, w, h))).is_err() {
                    failures.push(format!("{name} {w}x{h}"));
                }
            }
        }
    }
    failures
}

#[test]
fn sweep_silences_only_its_own_thread() {
    // Seen: the sweep swapped in a process-wide no-op hook for ~25 s, so
    // every UI test failing meanwhile reported an empty message.
    let (inside, other, caught) = quiet_panics(|| {
        let other = std::thread::spawn(|| QUIET.with(Cell::get)).join().unwrap();
        let caught = catch_unwind(|| panic!("expected")).is_err();
        (QUIET.with(Cell::get), other, caught)
    });
    assert!(inside, "the sweep's own thread is quiet");
    assert!(!other, "every other test thread keeps its messages");
    assert!(caught);
    assert!(!QUIET.with(Cell::get), "quiet ends with the sweep");
    let unwound = catch_unwind(|| quiet_panics(|| panic!("escapes")));
    assert!(unwound.is_err());
    assert!(!QUIET.with(Cell::get), "quiet ends even when f unwinds");
}

fn assert_clean(failures: &[String]) {
    assert!(
        failures.is_empty(),
        "{} failing sizes, first: {:?}",
        failures.len(),
        &failures[..failures.len().min(40)]
    );
}

#[test]
fn rv_size_sweep_never_panics() {
    assert_clean(&sweep(&QUICK_WIDTHS, &QUICK_HEIGHTS));
}

/// The review's full grid (minutes in a debug build):
/// `cargo test -p siltop-ui --release rv_size_sweep_full -- --ignored`.
#[test]
#[ignore = "slow: full size grid"]
fn rv_size_sweep_full() {
    let widths: Vec<u16> = (1..=200).collect();
    assert_clean(&sweep(&widths, &FULL_HEIGHTS));
}

/// Regression (review P2-b): the expanded process panel with its dossier
/// open crashed at 3-20 columns.
#[test]
fn rv_dossier_expanded_narrow_sweep() {
    let mut fx = m5_pro();
    dossier(&mut fx);
    fx.expanded = Some(Panel::Processes);
    let view = fx.view();
    for w in 1..=40 {
        for h in [1, 2, 3, 4, 5, 6, 16, 24, 40, 70] {
            exercise(&view, w, h);
        }
    }
}

/// Regression: a start time far in the past overflowed `started_ago`.
#[test]
fn rv_wild_values_dossier_renders() {
    let (_, fx) = states()
        .into_iter()
        .find(|(name, _)| name == "wild values")
        .unwrap();
    exercise(&fx.view(), 60, 16);
}
