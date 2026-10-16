use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use siltop_core::{ProcSort, Process, friendly_name, printable};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) use super::columns::UNKNOWN;
use super::columns::{
    Align, CellCtx, Placed, ProcColumn, Show, Tone, cell, column_at, cpu_label, layout, micro_spark,
};
use super::model::{Fold, ModelRow, ProcModel};
use crate::layout::Panel;
use crate::theme::Theme;
use crate::widgets::AppView;

/// Sort key under column `x` (relative to the table's left edge).
#[must_use]
pub(super) fn header_sort(width: u16, show: Show, x: u16) -> Option<ProcSort> {
    column_at(width, show, x).and_then(|c| c.spec().sort)
}

/// Header row: numeric headers right-aligned over their numbers, with an
/// arrow on the sort column that points the way the rows run.
pub(super) fn render_header(frame: &mut Frame, area: Rect, view: &AppView<'_>, theme: &Theme) {
    let mut spans = Vec::new();
    let mut cursor = 0;
    for placed in layout(area.width, Show::for_view(view)) {
        pad_to(&mut spans, &mut cursor, placed.x);
        let spec = placed.col.spec();
        let mut label = if placed.col == ProcColumn::Cpu {
            cpu_label(view.config.cpu_unit)
        } else {
            spec.label
        }
        .to_owned();
        let active = spec.sort == Some(view.sort);
        if active {
            label.push(if descending(view.sort, view.sort_reversed) {
                '↓'
            } else {
                '↑'
            });
        }
        let text = aligned(&label, placed.width, spec.align);
        cursor = placed.x.saturating_add(text.width() as u16);
        let style = if active { theme.fg() } else { theme.dim() };
        spans.push(Span::styled(text, style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Whether the header at `width` shows the column `sort` orders by; when
/// it does not, the panel title names the sort instead.
#[must_use]
pub(super) fn sort_shown(width: u16, show: Show, sort: ProcSort) -> bool {
    layout(width, show)
        .iter()
        .any(|placed| placed.col.spec().sort == Some(sort))
}

#[must_use]
pub(super) fn descending(sort: ProcSort, reversed: bool) -> bool {
    sort.natural_descending() != reversed
}

/// Paints the visible window `offset..offset + area.height` only; the
/// selected row gets its mark in the `marks` gutter and the selection style
/// over the whole line, gutter included.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_rows(
    frame: &mut Frame,
    marks: Rect,
    area: Rect,
    view: &AppView<'_>,
    model: &ProcModel,
    offset: usize,
    selected: Option<usize>,
    theme: &Theme,
) {
    let cols = layout(area.width, Show::for_view(view));
    let ctx = CellCtx::for_view(view, now_unix());
    let needle = view.proc.filter.to_ascii_lowercase();
    let hit = theme.search_hit();
    let focused = view.is_focused(Panel::Processes);
    let processes = &view.snapshot.processes;
    for (line, row_at) in (offset..model.len())
        .take(usize::from(area.height))
        .enumerate()
    {
        let Some(row) = model.rows().get(row_at) else {
            break;
        };
        let Some(proc) = processes.get(row.index).filter(|p| p.pid == row.pid) else {
            continue;
        };
        let rect = Rect {
            x: area.x,
            y: area.y.saturating_add(line as u16),
            width: area.width,
            height: 1,
        };
        let spans = row_spans(&row.shown(proc), row, &cols, &needle, hit, ctx, theme);
        frame.render_widget(Paragraph::new(Line::from(spans)), rect);
        let armed = armed(view, proc.pid);
        let tagged = view.proc.tags.contains(proc.pid);
        let is_selected = selected == Some(row_at);
        let mark = Rect { y: rect.y, ..marks };
        if tagged && !armed {
            let glyph = tag_mark(theme.icons, is_selected);
            frame.render_widget(Line::from(Span::styled(glyph, theme.title())), mark);
        }
        if armed || is_selected {
            if !tagged || armed {
                let glyph = if armed {
                    Span::raw(armed_mark(theme.icons))
                } else {
                    theme.selection_mark(focused)
                };
                frame.render_widget(Line::from(glyph), mark);
            }
            let line = Rect {
                x: mark.x,
                width: mark.width.saturating_add(rect.width),
                ..rect
            };
            theme.paint_selection(frame.buffer_mut(), line, focused, armed);
        }
    }
}

/// Whether a pending kill confirmation targets `pid`.
#[must_use]
pub(super) fn armed(view: &AppView<'_>, pid: u32) -> bool {
    view.confirm_kill && (view.confirm_pid == Some(pid) || view.proc.armed.contains(&pid))
}

/// The gutter glyph of the row a kill is armed for: a shape cue that
/// survives 256- and 16-colour terminals, where the alarm fill can land on
/// the same palette slot as a plain selection.
#[must_use]
pub(super) const fn armed_mark(icons: crate::theme::Icons) -> &'static str {
    match icons {
        crate::theme::Icons::None => "x",
        crate::theme::Icons::Unicode | crate::theme::Icons::Nerd => "✕",
    }
}

/// The gutter glyph of a tagged row: a shape, so tags read without
/// colour. A tagged row that is also selected gets a heavier shape.
#[must_use]
pub(super) const fn tag_mark(icons: crate::theme::Icons, selected: bool) -> &'static str {
    match (icons, selected) {
        (crate::theme::Icons::None, false) => "*",
        (crate::theme::Icons::None, true) => "#",
        (_, false) => "•",
        (_, true) => "◆",
    }
}

/// Before the name: `▸` for folded members, `▾` for an open app group
/// (open tree rows already show their children's guides).
const fn fold_mark(row: &ModelRow) -> &'static str {
    match (row.fold, row.agg.is_some()) {
        (Fold::Closed, _) => "▸ ",
        (Fold::Open, true) => "▾ ",
        _ => "",
    }
}

/// Wall-clock seconds, read once per frame by the table and the dossier.
pub(super) fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

fn tone_style(tone: Tone, theme: &Theme) -> Style {
    match tone {
        Tone::Fg => theme.fg(),
        Tone::Dim => theme.dim(),
        Tone::Cpu => theme.cpu(),
        Tone::Gpu => theme.gpu(),
        Tone::Warn => theme.warn_style(),
    }
}

/// The name cell: tree guides, the name (search hits lit), then the
/// daemon's friendly name dim when the column has room for it.
fn name_spans(
    proc: &Process,
    row: &ModelRow,
    width: u16,
    needle: &str,
    hit: Style,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let prefix = format!("{}{}", row.guide.prefix(row.depth), fold_mark(row));
    // `+N`: members the row stands for beyond itself, kept whole.
    let more = row
        .agg
        .map(|agg| format!(" +{}", agg.hidden()))
        .filter(|m| m.width() + prefix.width() + 2 <= usize::from(width))
        .unwrap_or_default();
    let name_w = u16::try_from(usize::from(width).saturating_sub(more.width())).unwrap_or(width);
    // An app group is labelled by its bundle (`Ghostty`), not by whichever
    // member heads it. Names from an old trace predate collection-time
    // scrubbing.
    let label = row
        .app_group
        .then_some(proc)
        .and_then(super::model::app_bundle)
        .map_or(proc.name.as_str(), super::model::bundle_name);
    let name = fit(&format!("{prefix}{}", printable(label)), name_w);
    let used = name.width();
    let mut spans = Vec::new();
    match name.strip_prefix(prefix.as_str()) {
        Some(rest) if !prefix.is_empty() => {
            spans.push(Span::styled(prefix.clone(), theme.faint()));
            spans.extend(matched_spans(rest, needle, theme.fg(), hit));
        }
        _ => spans.extend(matched_spans(&name, needle, theme.fg(), hit)),
    }
    if !more.is_empty() {
        // Right-aligned at the cell's edge, so the counts of every group
        // and folded node line up in one column like the numbers beside it.
        let pad = usize::from(width).saturating_sub(used + more.width());
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(more, theme.dim()));
        return spans;
    }
    // Whole or not at all: a cut-off gloss reads worse than none.
    if let Some(friendly) = friendly_name(&proc.name)
        && used + 2 + friendly.width() <= usize::from(width)
    {
        spans.push(Span::styled(format!("  {friendly}"), theme.dim()));
    }
    spans
}

fn row_spans(
    proc: &Process,
    row: &ModelRow,
    cols: &[Placed],
    needle: &str,
    hit: Style,
    ctx: CellCtx,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut cursor = 0;
    for placed in cols {
        pad_to(&mut spans, &mut cursor, placed.x);
        let w = placed.width;
        let start = spans.len();
        match placed.col {
            ProcColumn::Name => spans.extend(name_spans(proc, row, w, needle, hit, theme)),
            ProcColumn::Spark if proc.cpu_spark.is_empty() => {
                spans.push(Span::styled(right(UNKNOWN, w), theme.dim()));
            }
            ProcColumn::Spark => {
                spans.push(Span::styled(micro_spark(&proc.cpu_spark, w), theme.cpu()));
            }
            col => {
                let cell = cell(col, proc, ctx);
                let text = aligned(&cell.text, w, col.spec().align);
                let style = tone_style(cell.tone, theme);
                if col == ProcColumn::Pid {
                    spans.extend(matched_spans(&text, needle, style, hit));
                } else {
                    spans.push(Span::styled(text, style));
                }
            }
        }
        let drawn: usize = spans[start..].iter().map(|s| s.content.width()).sum();
        cursor = placed.x.saturating_add(u16::try_from(drawn).unwrap_or(w));
    }
    spans
}

fn aligned(text: &str, width: u16, align: Align) -> String {
    match align {
        Align::Left => fit(text, width),
        Align::Right => right(text, width),
    }
}

fn pad_to(spans: &mut Vec<Span<'static>>, cursor: &mut u16, x: u16) {
    if x > *cursor {
        spans.push(Span::raw(" ".repeat(usize::from(x - *cursor))));
        *cursor = x;
    }
}

fn right(text: &str, width: u16) -> String {
    let fitted = fit(text, width);
    let pad = usize::from(width).saturating_sub(fitted.width());
    format!("{}{fitted}", " ".repeat(pad))
}

/// Truncates to `width` display cells, ending in `…` when anything was cut.
/// Cuts between graphemes, the unit the terminal draws: an emoji with its
/// variation selector (`☁\u{fe0f}`) is two cells as a whole but one as
/// chars, so a per-char cut ran past the column.
#[must_use]
pub(crate) fn fit(text: &str, width: u16) -> String {
    let width = usize::from(width);
    if text.width() <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for g in text.graphemes(true) {
        let w = g.width();
        if used + w + 1 > width {
            break;
        }
        out.push_str(g);
        used += w;
    }
    out.push('…');
    out
}

#[must_use]
pub(super) fn matched_spans(
    text: &str,
    needle: &str,
    base: Style,
    hit: Style,
) -> Vec<Span<'static>> {
    if needle.is_empty() {
        return vec![Span::styled(text.to_owned(), base)];
    }
    let lower = text.to_ascii_lowercase();
    let Some(at) = lower.find(needle) else {
        return vec![Span::styled(text.to_owned(), base)];
    };
    let end = at + needle.len();
    match (text.get(..at), text.get(at..end), text.get(end..)) {
        (Some(head), Some(mid), Some(tail)) => vec![
            Span::styled(head.to_owned(), base),
            Span::styled(mid.to_owned(), hit),
            Span::styled(tail.to_owned(), base),
        ],
        _ => vec![Span::styled(text.to_owned(), base)],
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::widgets::processes::columns::cpu_text;
    use crate::widgets::processes::model::Guide;
    use crate::widgets::tests_support::process;

    fn plain(depth: u8) -> ModelRow {
        ModelRow {
            depth,
            ..ModelRow::default()
        }
    }

    fn row_text_at(proc: &Process, width: u16, show: Show) -> (String, Vec<Span<'static>>) {
        let cols = layout(width, show);
        let spans = row_spans(
            proc,
            &plain(0),
            &cols,
            "",
            Style::default(),
            CellCtx::at(0),
            &Theme::default(),
        );
        (spans.iter().map(|s| s.content.as_ref()).collect(), spans)
    }

    fn row_text(proc: &Process) -> (String, Vec<Span<'static>>) {
        let show = Show {
            threads: true,
            battery: false,
            present: ProcColumn::Threads.bit(),
        };
        row_text_at(proc, 70, show)
    }

    /// Regression: names with an emoji + VS16 or a control character drew
    /// wider or narrower than their column and pushed that row's numbers
    /// under the wrong headers.
    #[test]
    fn odd_names_keep_every_number_under_its_header() {
        let base = row_text(&process(1, "plain", 46.0)).0;
        let at = |row: &str| row.find("46.0").map(|i| row[..i].width());
        for name in [
            "\u{2601}\u{fe0f}".repeat(20),
            "\u{2764}\u{fe0f}".repeat(10),
            String::from("tab\tname"),
            String::from("esc\u{1b}[31mred"),
            String::from("del\u{7f}"),
        ] {
            let row = row_text(&process(1, &name, 46.0)).0;
            assert_eq!(at(&row), at(&base), "{name:?}\n{row}\n{base}");
            assert_eq!(row.width(), base.width(), "{name:?}\n{row}\n{base}");
        }
    }

    #[test]
    fn cpu_text_is_per_core_and_never_invents_zero() {
        let table = [
            (Some(0.0), "0.0"),
            (Some(12.345), "12.3"),
            (Some(99.96), "100.0"),
            (Some(412.0), "412.0"),
            (Some(-0.5), "0.0"),
            (None, UNKNOWN),
            (Some(f32::NAN), UNKNOWN),
            (Some(f32::INFINITY), UNKNOWN),
        ];
        for (cpu, want) in table {
            assert_eq!(cpu_text(cpu), want, "{cpu:?}");
        }
    }

    #[test]
    fn d23_restricted_row_renders_dashes_dimmed() {
        let proc = Process {
            pid: 400,
            user: String::from("_windowserver"),
            name: String::from("WindowServer"),
            restricted: true,
            ..Process::default()
        };
        let (text, spans) = row_text(&proc);
        assert_eq!(
            text.matches(UNKNOWN).count(),
            3,
            "cpu, mem, threads: {text}"
        );
        assert!(!text.contains("0.0") && !text.contains("0B"), "{text}");
        let theme = Theme::default();
        for span in spans.iter().filter(|s| s.content.contains(UNKNOWN)) {
            assert_eq!(span.style, theme.dim(), "unknown is dim");
        }
    }

    #[test]
    fn every_row_is_exactly_the_table_width_at_every_size() {
        let proc = Process {
            pid: 4242,
            name: String::from("mds_stores"),
            user: String::from("root"),
            cpu: Some(12.5),
            mem_bytes: Some(3 << 20),
            cpu_spark: vec![10.0, 50.0, 100.0],
            gpu: 3.0,
            ..Process::default()
        };
        for width in 21..=200_u16 {
            let (text, _) = row_text_at(&proc, width, Show::ALL);
            assert!(text.width() <= usize::from(width), "{width}: {text}");
            let last = layout(width, Show::ALL).last().copied().unwrap();
            assert_eq!(
                text.width(),
                usize::from(last.x + last.width),
                "{width}: {text}"
            );
            assert!(text.contains("mds_st"), "{width}: name kept: {text}");
        }
    }

    #[test]
    fn friendly_name_follows_dim_when_there_is_room() {
        let proc = Process {
            name: String::from("mds_stores"),
            cpu: Some(1.0),
            ..Process::default()
        };
        let (text, spans) = row_text_at(&proc, 200, Show::default());
        assert!(text.contains("mds_stores  Spotlight indexing"), "{text}");
        let friendly = spans
            .iter()
            .find(|s| s.content.contains("Spotlight"))
            .unwrap();
        assert_eq!(friendly.style, Theme::default().dim());
        let (narrow, _) = row_text_at(&proc, 24, Show::default());
        assert!(!narrow.contains("Spotlight"), "no room: {narrow}");
        assert!(narrow.contains("mds_stores"), "{narrow}");
    }

    #[test]
    fn tree_guides_prefix_the_name_faintly() {
        let proc = Process {
            name: String::from("child"),
            ..Process::default()
        };
        let row = ModelRow {
            depth: 2,
            guide: Guide {
                rails: 0b1,
                last: false,
            },
            ..plain(0)
        };
        let spans = name_spans(&proc, &row, 30, "", Style::default(), &Theme::default());
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "│ ├ child");
        assert_eq!(spans[0].style, Theme::default().faint());
    }

    #[test]
    fn readable_row_renders_values() {
        let proc = Process {
            pid: 42,
            name: String::from("rustc serde"),
            user: String::from("cloaky"),
            cpu: Some(250.0),
            mem_bytes: Some(3 << 20),
            threads: Some(12),
            ..Process::default()
        };
        let (text, _) = row_text(&proc);
        assert!(!text.contains(UNKNOWN), "{text}");
        assert!(text.contains("250.0") && text.contains("12"), "{text}");
    }

    #[test]
    fn fit_ends_cut_names_with_an_ellipsis() {
        assert_eq!(fit("WindowServer", 20), "WindowServer");
        assert_eq!(fit("com.apple.WebKit.WebContent", 12), "com.apple.W…");
        assert_eq!(fit("com.apple.WebKit.WebContent", 12).width(), 12);
        assert_eq!(fit("日本語のプロセス", 7).width(), 7);
        assert_eq!(right("0.0", 7), "    0.0");
    }

    #[test]
    fn arrow_points_the_way_rows_run() {
        assert!(descending(ProcSort::Cpu, false));
        assert!(!descending(ProcSort::Cpu, true));
        assert!(!descending(ProcSort::Pid, false));
        assert!(descending(ProcSort::Pid, true));
        assert!(!descending(ProcSort::Name, false));
        assert!(descending(ProcSort::Started, false));
    }

    #[test]
    fn header_hit_matches_painted_columns() {
        for width in [40_u16, 60, 100, 180] {
            for placed in layout(width, Show::ALL) {
                let mid = placed.x + placed.width / 2;
                assert_eq!(
                    header_sort(width, Show::ALL, mid),
                    placed.col.spec().sort,
                    "{width} {placed:?}"
                );
            }
        }
    }

    #[test]
    fn matches_are_highlighted() {
        let theme = Theme::default();
        let hit = Style::default().fg(theme.hi);
        let spans = matched_spans("WindowServer", "win", theme.fg(), hit);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[1].content.as_ref(), "Win");
        assert_eq!(spans[1].style.fg, Some(theme.hi));
        assert_eq!(spans[0].style.fg, Some(theme.fg));
        assert_eq!(matched_spans("Finder", "zzz", theme.fg(), hit).len(), 1);
    }
}
