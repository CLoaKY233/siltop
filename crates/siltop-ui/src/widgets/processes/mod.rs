mod columns;
mod dossier;
mod model;
pub(super) mod signal_menu;
#[cfg(test)]
mod sweep;
mod table;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use siltop_core::{Process, Surface};
use unicode_width::UnicodeWidthStr;

use crate::chrome::panel_block;
use crate::layout::{Hit, LayoutFlags, Panel, plan};
use crate::theme::Theme;
use crate::widgets::{AppView, Focus};

pub use dossier::{DossierTab, detail_actions, detail_close, detail_rect, detail_tabs};
pub use model::{Anchor, ModelRow, Nesting, ProcModel, ProcView, SCROLL_MARGIN, Tags};
pub(crate) use table::fit;

/// Cells left of the columns for the selection mark.
const MARK_W: u16 = 1;

/// Inside the border: the search line, the header, then the body rows. The
/// header and body sit right of the mark gutter, so columns never shift.
struct Parts {
    search: Rect,
    header: Rect,
    marks: Rect,
    body: Rect,
}

fn parts(area: Rect) -> Parts {
    let inner = Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    let row = |dy: u16, height: u16| Rect {
        y: inner.y.saturating_add(dy),
        height,
        ..inner
    };
    let mark_w = MARK_W.min(inner.width);
    let columns = |r: Rect| Rect {
        x: r.x.saturating_add(mark_w),
        width: r.width.saturating_sub(mark_w),
        ..r
    };
    let body = row(2, inner.height.saturating_sub(2));
    Parts {
        search: row(0, inner.height.min(1)),
        header: columns(row(1, inner.height.saturating_sub(1).min(1))),
        marks: Rect {
            width: mark_w,
            ..body
        },
        body: columns(body),
    }
}

/// Body rows the table shows in the current layout; 0 without a table.
#[must_use]
pub fn proc_rows(area: Rect, surface: Surface, flags: LayoutFlags) -> usize {
    plan(area, surface, flags)
        .processes
        .map_or(0, |rect| usize::from(parts(rect).body.height))
}

pub(super) fn render(frame: &mut Frame, area: Rect, view: &AppView<'_>, theme: &Theme) {
    let rows = parts(area);
    let (model, offset) = window(view, rows.body);
    let mut title = crate::titles::proc(view, model.len(), offset, theme);
    if !table::sort_shown(rows.header.width, columns::Show::for_view(view), view.sort) {
        // sorted by a column this width hides: say what orders the rows
        let arrow = if table::descending(view.sort, view.sort_reversed) {
            '↓'
        } else {
            '↑'
        };
        title.push(
            crate::titles::Keep::Mid,
            format!("by {}{arrow}", view.sort.label()),
            theme.dim(),
        );
    }
    let title = title.fit(
        crate::titles::panel_budget(Panel::Processes, area.width, 0, theme),
        theme,
    );
    let block = panel_block(
        Panel::Processes,
        title,
        view.is_focused(Panel::Processes),
        view.is_expanded(Panel::Processes),
        theme,
    );
    frame.render_widget(block, area);
    if rows.search.height == 0 || rows.search.width == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(search_line(view, rows.search, theme)),
        rows.search,
    );
    if rows.header.height > 0 {
        table::render_header(frame, rows.header, view, theme);
    }
    if rows.body.height > 0 {
        let selected = view.proc.selected_row(&model);
        table::render_rows(
            frame, rows.marks, rows.body, view, &model, offset, selected, theme,
        );
    }
    if let Some(pid) = view.detail_pid {
        dossier::render_detail(frame, area, view, pid, theme);
    }
}

/// The current model and the scroll offset clamped to `body`, shared by
/// painting, clicks and the kill veil so they agree on which row is where.
fn window<'v>(view: &'v AppView<'_>, body: Rect) -> (std::borrow::Cow<'v, ProcModel>, usize) {
    let model = view.proc.current(
        &view.snapshot.processes,
        view.sort,
        view.sort_reversed,
        view.show_tree,
    );
    let offset = view
        .proc
        .offset
        .min(model::max_offset(model.len(), usize::from(body.height)));
    (model, offset)
}

/// The on-screen lines (gutter mark included) of every row a pending kill
/// targets, so the confirmation can keep them lit: the user must see what
/// they are about to end. Empty unless a kill is being confirmed.
#[must_use]
pub(super) fn armed_rows(view: &AppView<'_>, area: Rect) -> Vec<Rect> {
    if !view.confirm_kill {
        return Vec::new();
    }
    let rows = parts(area);
    let (model, offset) = window(view, rows.body);
    (0..rows.body.height)
        .filter(|line| {
            model
                .rows()
                .get(offset + usize::from(*line))
                .is_some_and(|row| table::armed(view, row.pid))
        })
        .map(|line| Rect {
            x: rows.marks.x,
            y: rows.body.y.saturating_add(line),
            width: rows.marks.width.saturating_add(rows.body.width),
            height: 1,
        })
        .collect()
}

/// What a click inside the process pane `area` means: the search line, a
/// sortable header column, or a model row (scroll offset included). The
/// bottom border and rows past the list are just the panel.
#[must_use]
pub(super) fn hit(view: &AppView<'_>, area: Rect, col: u16, row: u16) -> Option<Hit> {
    let rows = parts(area);
    let inside = |r: Rect| {
        col >= r.x
            && col < r.x.saturating_add(r.width)
            && row >= r.y
            && row < r.y.saturating_add(r.height)
    };
    if filter_clear(view, rows.search).is_some_and(inside) {
        return Some(Hit::FilterClear);
    }
    if inside(rows.search) {
        return Some(Hit::Search);
    }
    if inside(rows.header) {
        let show = columns::Show::for_view(view);
        return table::header_sort(rows.header.width, show, col - rows.header.x).map(Hit::SortBy);
    }
    if !inside(rows.body) && !inside(rows.marks) {
        return None;
    }
    let (model, offset) = window(view, rows.body);
    let index = offset + usize::from(row - rows.body.y);
    (index < model.len()).then_some(Hit::ProcRow(index))
}

const FILTER_LABEL: &str = " filter: ";
const FILTER_CLEAR: &str = "✕";
const TYPING_HINT: &str = "enter keep · esc clear";

/// Typing: ` search foo▌`, the input's end always in view, with its keys
/// on the right when they fit. A kept filter: ` filter: foo ✕`, whose `✕`
/// clears it. Idle: how to start one.
fn search_line(view: &AppView<'_>, area: Rect, theme: &Theme) -> Line<'static> {
    let width = usize::from(area.width);
    let typing = view.searching || matches!(view.focus, Focus::Search);
    if typing {
        let label = " search ";
        let room = width.saturating_sub(label.width() + 1);
        let mut spans = vec![
            Span::styled(label, theme.title()),
            Span::styled(tail(&view.proc.filter, room), theme.title()),
            Span::styled("▌", theme.title()),
        ];
        let used = label.width() + tail(&view.proc.filter, room).width() + 1;
        if used + 2 + TYPING_HINT.width() < width {
            let pad = width - used - TYPING_HINT.width() - 1;
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(TYPING_HINT, theme.dim()));
        }
        return Line::from(spans);
    }
    if view.proc.filter.is_empty() {
        return Line::from(vec![
            Span::styled(" search ", theme.dim()),
            Span::styled(" / to filter", theme.dim()),
        ]);
    }
    let room = width.saturating_sub(FILTER_LABEL.width() + 2);
    Line::from(vec![
        Span::styled(FILTER_LABEL, theme.warn_style()),
        Span::styled(tail(&view.proc.filter, room), theme.title()),
        Span::raw(" "),
        Span::styled(FILTER_CLEAR, theme.warn_style()),
    ])
}

/// The last `room` cells of `text`: a long filter keeps its end, where
/// the cursor is, in view.
fn tail(text: &str, room: usize) -> String {
    let mut out: Vec<char> = Vec::new();
    let mut used = 0;
    for c in text.chars().rev() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > room {
            break;
        }
        used += w;
        out.push(c);
    }
    out.into_iter().rev().collect()
}

/// The kept filter's `✕` on the search row `area`, where it is drawn.
#[must_use]
pub(crate) fn filter_clear(view: &AppView<'_>, area: Rect) -> Option<Rect> {
    let typing = view.searching || matches!(view.focus, Focus::Search);
    if typing || view.proc.filter.is_empty() || area.height == 0 {
        return None;
    }
    let room = usize::from(area.width).saturating_sub(FILTER_LABEL.width() + 2);
    let shown = FILTER_LABEL.width() + tail(&view.proc.filter, room).width() + 1;
    let x = area.x.saturating_add(u16::try_from(shown).ok()?);
    (x < area.right()).then(|| Rect::new(x, area.y, 1, 1))
}

/// The search row of the process pane `area`.
#[must_use]
pub(crate) fn search_row(area: Rect) -> Rect {
    parts(area).search
}

/// The listed processes in display order (tests and non-interactive callers).
#[must_use]
pub fn filtered<'a>(view: &'a AppView<'_>) -> Vec<&'a Process> {
    let model = view.proc.current(
        &view.snapshot.processes,
        view.sort,
        view.sort_reversed,
        view.show_tree,
    );
    model
        .rows()
        .iter()
        .filter_map(|row| view.snapshot.processes.get(row.index))
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::widgets::tests_support::{fixture, process};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn paint_with(view: &AppView<'_>, theme: &Theme, w: u16, h: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), view, theme))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn paint(view: &AppView<'_>, w: u16, h: u16) -> Vec<String> {
        let buf = paint_with(view, &Theme::default(), w, h);
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    fn armed_fixture(
        icons: crate::theme::Icons,
    ) -> (crate::widgets::tests_support::Fixture, Theme) {
        let mut fx = fixture("");
        fx.snap.processes = vec![process(7, "busy", 12.5), process(8, "idle", 0.0)];
        fx.confirm_kill = true;
        fx.confirm_pid = Some(7);
        (fx, Theme::default().with_icons(icons))
    }

    #[test]
    fn rv_armed_row_carries_a_shape_cue_at_every_depth() {
        use crate::theme::ColorDepth;
        for icons in [
            crate::theme::Icons::None,
            crate::theme::Icons::Unicode,
            crate::theme::Icons::Nerd,
        ] {
            for depth in [
                ColorDepth::TrueColor,
                ColorDepth::Ansi256,
                ColorDepth::Ansi16,
                ColorDepth::NoColor,
            ] {
                let (fx, theme) = armed_fixture(icons);
                let buf = paint_with(&fx.view(), &theme.with_depth(depth), 60, 10);
                let mark = buf[(1, 3)].symbol();
                assert_eq!(mark, table::armed_mark(icons), "{icons:?} {depth:?}");
                assert_ne!(mark, icons.select_mark(), "distinct from a selection");
                assert!(
                    buf[(10, 3)]
                        .modifier
                        .contains(ratatui::style::Modifier::BOLD),
                    "{icons:?} {depth:?}: the armed row is bold"
                );
                assert_eq!(buf[(1, 4)].symbol(), " ", "only the target is marked");
            }
        }
    }

    #[test]
    fn rv_armed_mark_follows_the_target_not_the_cursor() {
        let (mut fx, theme) = armed_fixture(crate::theme::Icons::Unicode);
        fx.confirm_pid = Some(8);
        let buf = paint_with(&fx.view(), &theme, 60, 10);
        assert_eq!(buf[(1, 4)].symbol(), "✕", "the dossier's target row");
        assert_eq!(buf[(1, 3)].symbol(), "▌", "the cursor keeps its mark");
    }

    #[test]
    fn rv_unarmed_selection_keeps_its_mark() {
        let (mut fx, theme) = armed_fixture(crate::theme::Icons::Unicode);
        fx.confirm_kill = false;
        let buf = paint_with(&fx.view(), &theme, 60, 10);
        assert_eq!(buf[(1, 3)].symbol(), "▌");
    }

    #[test]
    fn empty_filter_returns_all_sorted_by_cpu_desc() {
        let mut fx = fixture("");
        fx.snap.processes = vec![
            process(1, "WindowServer", 5.2),
            process(2, "Xcode", 48.1),
            process(3, "siltop", 0.6),
        ];
        let view = fx.view();
        let names: Vec<&str> = filtered(&view).iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Xcode", "WindowServer", "siltop"]);
    }

    #[test]
    fn filter_is_case_insensitive_on_name() {
        let mut fx = fixture("xCoDe");
        fx.snap.processes = vec![
            process(1, "WindowServer", 5.2),
            process(2, "Xcode", 48.1),
            process(3, "xcodebuild", 12.0),
        ];
        let view = fx.view();
        let names: Vec<&str> = filtered(&view).iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Xcode", "xcodebuild"]);
    }

    #[test]
    fn scrolled_rows_hit_by_model_index_and_border_is_not_a_row() {
        let mut fx = fixture("");
        fx.snap.processes = (1..=100)
            .map(|pid| process(pid, &format!("p{pid}"), 100.0 - pid as f32))
            .collect();
        fx.proc.offset = 40;
        let area = Rect::new(0, 0, 60, 20);
        let view = fx.view();
        let lines = paint(&view, 60, 20);
        let body_top = 3_u16;
        for visible in 0..16_u16 {
            let y = body_top + visible;
            let Some(Hit::ProcRow(index)) = hit(&view, area, 30, y) else {
                panic!("row {y} is not a process row");
            };
            let pid = view.snapshot.processes[view
                .proc
                .current(&view.snapshot.processes, view.sort, false, false)
                .rows()[index]
                .index]
                .pid;
            assert!(
                lines[usize::from(y)].contains(&format!("p{pid} ")),
                "row {y} shows {:?}, hit says p{pid}",
                lines[usize::from(y)]
            );
        }
        assert_eq!(hit(&view, area, 30, 19), None, "bottom border");
        assert!(lines[0].contains("↑ 40"), "{}", lines[0]);
    }

    #[test]
    fn selection_mark_sits_in_the_gutter_and_the_gutter_is_clickable() {
        let mut fx = fixture("");
        fx.snap.processes = vec![process(7, "busy", 12.5), process(8, "idle", 0.0)];
        let view = fx.view();
        let lines = paint(&view, 60, 10);
        assert!(lines[3].starts_with("│▌"), "{}", lines[3]);
        assert!(lines[4].starts_with("│ "), "{}", lines[4]);
        let area = Rect::new(0, 0, 60, 10);
        assert_eq!(hit(&view, area, 1, 4), Some(Hit::ProcRow(1)));
    }

    /// Regression: gpu% came and went with each 500 ms sample, shifting
    /// the header so a click on `mem` sorted by the `read` column that the
    /// next frame then hid.
    #[test]
    fn situational_columns_stay_once_seen() {
        let mut fx = fixture("");
        let mut busy = process(904, "Xcode", 48.1);
        busy.gpu = 12.0;
        busy.disk_read_bps = Some(4096.0);
        fx.snap.processes = vec![busy];
        fx.proc.prune(&fx.snap.processes);
        let before = paint(&fx.view(), 120, 20)[2].clone();
        assert!(
            before.contains("gpu%") && before.contains("read"),
            "{before}"
        );
        let mut idle = process(904, "Xcode", 48.1);
        idle.status = "stopped";
        fx.snap.processes = vec![idle];
        fx.proc.prune(&fx.snap.processes);
        let stopped = paint(&fx.view(), 120, 20)[2].clone();
        assert!(
            stopped.contains("gpu%") && stopped.contains("read"),
            "{stopped}"
        );
        fx.snap.processes = vec![process(904, "Xcode", 48.1)];
        fx.proc.prune(&fx.snap.processes);
        let after = paint(&fx.view(), 120, 20)[2].clone();
        assert_eq!(after, before, "the state column alone comes and goes");
    }

    #[test]
    fn header_clicks_sort_by_the_column_under_the_pointer() {
        let fx = fixture("");
        let view = fx.view();
        let area = Rect::new(0, 0, 60, 20);
        let lines = paint(&view, 60, 20);
        let header = &lines[2];
        for (label, want) in [
            ("pid", Some(siltop_core::ProcSort::Pid)),
            ("cpu%", Some(siltop_core::ProcSort::Cpu)),
            ("mem", Some(siltop_core::ProcSort::Mem)),
            ("name", Some(siltop_core::ProcSort::Name)),
        ] {
            let at = header
                .find(label)
                .unwrap_or_else(|| panic!("{label} in {header}"));
            let col = u16::try_from(header[..at].chars().count()).unwrap();
            assert_eq!(hit(&view, area, col, 2), want.map(Hit::SortBy), "{label}");
        }
        assert!(header.contains("cpu%↓"), "{header}");
    }

    #[test]
    fn known_name_survives_minimum_work_sizes() {
        use crate::widgets::render_app;
        for (w, h) in [(60_u16, 16_u16), (60, 20), (70, 60), (80, 24)] {
            let mut fx = fixture("");
            fx.snap.processes = vec![
                process(904, "Xcode", 48.1),
                process(1, "com.apple.WebKit.WebContent", 3.0),
            ];
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal
                .draw(|frame| render_app(frame, &fx.view()))
                .unwrap();
            let buf = terminal.backend().buffer();
            let text: String = (0..h)
                .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n")
                .collect();
            assert!(text.contains("Xcode"), "{w}x{h}:\n{text}");
            assert!(text.contains("com.apple."), "{w}x{h}:\n{text}");
        }
    }

    #[test]
    fn idle_cpu_is_dimmed_and_numbers_right_align() {
        let mut fx = fixture("");
        fx.snap.processes = vec![process(7, "busy", 12.5), process(8, "idle", 0.0)];
        let view = fx.view();
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &view, &Theme::default()))
            .unwrap();
        let buf = terminal.backend().buffer();
        let row = |y: u16| -> String { (0..60).map(|x| buf[(x, y)].symbol()).collect() };
        let header = row(2);
        let busy = row(3);
        let cpu_end = header.find("cpu%↓").unwrap() + "cpu%↓".len();
        let value_end = busy.find("12.5").unwrap() + "12.5".len();
        assert_eq!(
            header[..cpu_end].chars().count(),
            busy[..value_end].chars().count(),
            "header and value share a right edge:\n{header}\n{busy}"
        );
        let idle = row(4);
        let zero_x = u16::try_from(idle[..idle.find("0.0").unwrap()].chars().count()).unwrap();
        let theme = Theme::default();
        assert_eq!(buf[(zero_x, 4)].fg, theme.dim, "0.0 is dim");
    }

    #[test]
    fn tagged_rows_carry_a_shape_in_the_gutter_at_every_depth() {
        use crate::theme::{ColorDepth, Icons};
        for icons in [Icons::None, Icons::Unicode, Icons::Nerd] {
            for depth in [
                ColorDepth::TrueColor,
                ColorDepth::Ansi16,
                ColorDepth::NoColor,
            ] {
                let (mut fx, theme) = armed_fixture(icons);
                fx.confirm_kill = false;
                let procs = fx.snap.processes.clone();
                fx.proc.toggle_tags(&[7, 8], &procs);
                let buf = paint_with(&fx.view(), &theme.with_depth(depth), 60, 10);
                assert_eq!(
                    buf[(1, 3)].symbol(),
                    table::tag_mark(icons, true),
                    "{icons:?} {depth:?}: selected and tagged"
                );
                assert_eq!(
                    buf[(1, 4)].symbol(),
                    table::tag_mark(icons, false),
                    "{icons:?} {depth:?}: tagged"
                );
                assert_ne!(table::tag_mark(icons, true), icons.select_mark());
                assert_ne!(table::tag_mark(icons, false), table::armed_mark(icons));
            }
        }
    }

    #[test]
    fn untagged_rows_have_a_blank_gutter() {
        let (mut fx, theme) = armed_fixture(crate::theme::Icons::Unicode);
        fx.confirm_kill = false;
        let procs = fx.snap.processes.clone();
        fx.proc.toggle_tags(&[8], &procs);
        let buf = paint_with(&fx.view(), &theme, 60, 10);
        assert_eq!(buf[(1, 3)].symbol(), "▌", "selected, untagged");
        assert_eq!(buf[(1, 4)].symbol(), "•");
    }

    fn arc_fixture() -> crate::widgets::tests_support::Fixture {
        let mut fx = fixture("");
        let helper = |pid: u32, cpu: f32, cmd: &str| Process {
            command: Some(cmd.to_owned()),
            ..process(pid, "Arc Helper", cpu)
        };
        fx.snap.processes = vec![
            helper(20, 1.5, "/Applications/Arc.app/Contents/MacOS/Arc"),
            helper(
                21,
                40.0,
                "/Applications/Arc.app/Contents/Frameworks/H.app/Contents/MacOS/H",
            ),
            helper(
                22,
                2.5,
                "/Applications/Arc.app/Contents/Frameworks/G.app/Contents/MacOS/G",
            ),
            process(30, "zsh", 3.0),
        ];
        fx.snap.processes[0].name = String::from("Arc");
        fx.proc.grouped = true;
        fx
    }

    #[test]
    fn group_row_paints_its_sum_a_fold_mark_and_the_member_count() {
        let fx = arc_fixture();
        let lines = paint(&fx.view(), 80, 10);
        let row = &lines[3];
        assert!(row.contains("▸ Arc ") && row.contains(" +2 "), "{row}");
        assert!(row.contains("44.0"), "cpu is the group's sum: {row}");
        assert!(lines[4].contains("zsh"), "{}", lines[4]);
        assert_eq!(lines.iter().filter(|l| l.contains("Arc Helper")).count(), 0);
    }

    #[test]
    fn open_group_paints_helpers_under_an_open_mark() {
        let mut fx = arc_fixture();
        fx.proc
            .folds
            .open_apps
            .set(String::from("/Applications/Arc.app"), true);
        let lines = paint(&fx.view(), 80, 10);
        assert!(
            lines[3].contains("▾ Arc ") && lines[3].contains(" +2 "),
            "{}",
            lines[3]
        );
        assert!(lines[4].contains("├ Arc Helper"), "{}", lines[4]);
        assert!(lines[5].contains("╰ Arc Helper"), "{}", lines[5]);
    }

    #[test]
    fn collapsed_tree_node_paints_its_subtree_total() {
        let mut fx = fixture("");
        fx.snap.processes = vec![
            Process {
                ppid: 0,
                ..process(1, "launchd", 1.0)
            },
            process(2, "a", 10.0),
            process(3, "b", 20.0),
        ];
        fx.show_tree = true;
        fx.proc.folds.closed.set(1, true);
        let lines = paint(&fx.view(), 80, 8);
        assert!(
            lines[3].contains("▸ launchd ") && lines[3].contains(" +2 "),
            "{}",
            lines[3]
        );
        assert!(lines[3].contains("31.0"), "{}", lines[3]);
        assert!(!lines[4].contains(" a "), "children hidden: {}", lines[4]);
    }

    /// Regression: `+N` followed each name, so badges zig-zagged down the
    /// column. They end on the name cell's right edge now.
    #[test]
    fn group_badges_share_a_right_edge() {
        let mut fx = arc_fixture();
        let slack = |pid: u32, cpu: f32, cmd: &str| Process {
            command: Some(cmd.to_owned()),
            ..process(pid, "Slack Helper", cpu)
        };
        fx.snap.processes.extend([
            slack(40, 9.0, "/Applications/Slack.app/Contents/MacOS/Slack"),
            slack(
                41,
                1.0,
                "/Applications/Slack.app/Contents/Frameworks/S.app/Contents/MacOS/S",
            ),
        ]);
        fx.snap.processes[4].name = String::from("Slack");
        for w in [80_u16, 120, 160] {
            let lines = paint(&fx.view(), w, 10);
            let edge = |needle: &str| {
                let row = lines
                    .iter()
                    .find(|l| l.contains(needle))
                    .unwrap_or_else(|| panic!("{needle} at {w}"));
                let at = row.find(needle).unwrap();
                row[..at].chars().count() + needle.chars().count()
            };
            assert_eq!(edge(" +2"), edge(" +1"), "w={w}:\n{}", lines.join("\n"));
        }
    }

    #[test]
    fn member_count_is_whole_or_absent_at_every_width() {
        let fx = arc_fixture();
        for w in 24..=140 {
            let lines = paint(&fx.view(), w, 8);
            let row = &lines[3];
            let cut = row.contains(" +") && !row.contains(" +2");
            assert!(!cut, "w={w}: the +N cue was cut: {row}");
            assert!(row.contains('▸') || w < 30, "w={w}: fold mark kept: {row}");
        }
    }
}
