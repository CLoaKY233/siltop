//! The Timeline page (`H`, or the main menu): long history on one shared
//! time axis, a scrub cursor, and a blame card saying who was busy under it.
//!
//! [`parts`] is the single geometry source: painting, range-chip clicks,
//! scrub clicks and the binning width the app asks for all read it, so a
//! column is clickable exactly where it is drawn.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span as TextSpan};
use ratatui::widgets::Paragraph;
use siltop_core::store::{Blame, Bucket, Event, Named, Series, Span, clock_label};
use siltop_core::{Thermal, bytes_short, nice_ceiling, watts_display};

use crate::braille::{Grow, render_dots};
use crate::chrome::{cell_titled, paint_rows, push_token};
use crate::theme::{Ramp, Theme};
use crate::widgets::power::MIN_WATTS;
use unicode_width::UnicodeWidthStr;

/// Processes listed per blame list.
pub(crate) const BLAME_N: usize = 3;
/// Card width beside the graphs on a wide window.
const CARD_W: u16 = 34;
/// Wide enough to put the card beside the graphs rather than below.
const SIDE_CARD_MIN_W: u16 = 100;
/// Card height below the graphs on a narrow window.
const BOTTOM_CARD_H: u16 = 8;
/// Rows a graph box needs: two borders and one plot row.
const MIN_GRAPH_H: u16 = 3;

/// Everything the page shows; the app refreshes it on open, on a range
/// change, on resize and as new buckets close.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TimelineView {
    pub span: Span,
    pub from: u64,
    pub to: u64,
    /// One merged bucket per plot column, `None` where nothing was recorded.
    pub columns: Vec<Option<Bucket>>,
    pub events: Vec<(usize, Event)>,
    /// The scrub cursor's column; `None` until there is data.
    pub cursor: Option<usize>,
    pub utc_offset_s: i64,
    /// Whether history comes from the disk store (else this session).
    pub saved: bool,
    /// Per column, the newest recorded bucket's own start: the moment the
    /// cursor label names, rather than the column's rounded start.
    pub stamps: Vec<Option<u64>>,
}

impl TimelineView {
    /// Moves the cursor by `delta` columns, clamped to the plot.
    pub fn scrub(&mut self, delta: i32) {
        let last = self.columns.len().saturating_sub(1);
        let at = self.cursor.unwrap_or(last);
        self.cursor = Some(
            at.saturating_add_signed(isize::try_from(delta).unwrap_or(0))
                .min(last),
        );
    }

    /// The blame card for the cursor's column.
    #[must_use]
    pub fn blame(&self) -> Blame {
        self.cursor
            .map(|c| Blame::at(&self.columns, &self.events, c, BLAME_N))
            .unwrap_or_default()
    }

    /// The moment column `c` shows: its newest bucket's time when it has
    /// one, else the column's start.
    #[must_use]
    pub fn time_at(&self, c: usize) -> u64 {
        self.stamps.get(c).copied().flatten().unwrap_or_else(|| {
            siltop_core::store::column_start(c, self.from, self.to, self.columns.len())
        })
    }

    /// Whether ranges show dates (anything past today).
    const fn dated(&self) -> bool {
        matches!(self.span, Span::Week | Span::Month)
    }
}

/// One graph row: what it plots and how.
#[derive(Debug, Clone, Copy)]
struct Row {
    label: &'static str,
    series: Series,
    /// `Some(max)` for a fixed scale (ratios); `None` scales to the peak.
    fixed: Option<f32>,
    /// A second series told beside the value at the cursor (not plotted).
    detail: Option<Series>,
}

impl Row {
    const fn plain(label: &'static str, series: Series, fixed: Option<f32>) -> Self {
        Self {
            label,
            series,
            fixed,
            detail: None,
        }
    }
}

/// In paint order; rows whose series was never recorded are hidden (the
/// battery row on desktops and in v1 history).
const ROWS: [Row; 6] = [
    Row::plain("cpu", Series::Cpu, Some(1.0)),
    Row::plain("gpu", Series::Gpu, Some(1.0)),
    Row::plain("memory", Series::Mem, Some(1.0)),
    Row::plain("power", Series::SysPowerW, None),
    Row {
        label: "battery",
        series: Series::BatteryPct,
        fixed: Some(100.0),
        detail: Some(Series::BatteryW),
    },
    Row::plain("cpu temp", Series::CpuTempC, None),
];

fn ramp(theme: &Theme, series: Series) -> Ramp {
    match series {
        Series::Gpu => theme.gpu,
        Series::Mem => theme.mem,
        Series::SysPowerW => theme.fan,
        Series::BatteryPct => theme.net,
        Series::CpuTempC => theme.temp,
        _ => theme.cpu,
    }
}

fn format_value(series: Series, v: f32) -> String {
    match series {
        Series::Cpu | Series::Gpu | Series::Mem => format!("{:.0}%", v * 100.0),
        Series::BatteryPct => format!("{v:.0}%"),
        Series::SysPowerW => watts_display(v),
        // The power panel's arrows: ▲ charging, ▼ draining.
        Series::BatteryW if v.abs() < MIN_WATTS => "idle".to_owned(),
        Series::BatteryW if v > 0.0 => format!("▲ {}", watts_display(v)),
        Series::BatteryW => format!("▼ {}", watts_display(-v)),
        Series::CpuTempC => format!("{v:.0}°"),
        _ => format!("{v:.0}"),
    }
}

/// The row's reading at column `col`: `64% ▼ 8.0W` for the battery.
fn value_at(row: Row, columns: &[Option<Bucket>], col: usize) -> Option<String> {
    let mean = |series| {
        columns
            .get(col)?
            .as_ref()?
            .get(series)
            .map(|s| format_value(series, s.mean))
    };
    let value = mean(row.series)?;
    Some(match row.detail.and_then(mean) {
        Some(detail) => format!("{value} {detail}"),
        None => value,
    })
}

/// The page's rectangles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parts {
    pub header: Rect,
    /// One per [`Span::ALL`], in the header.
    pub chips: Vec<(Rect, Span)>,
    /// The graph boxes' outer area.
    pub graphs: Rect,
    /// The shared plot columns: every graph's inner x range.
    pub plot_x: u16,
    pub plot_w: u16,
    /// Time labels under the graphs.
    pub axis: Rect,
    pub card: Option<Rect>,
    pub hint: Rect,
    /// The page's close button, `[×]` at the header's right end.
    pub close: Option<Rect>,
    /// The hint row's `esc close`, a button too.
    pub hint_close: Option<Rect>,
}

const TITLE: &str = " timeline ";

/// The hint row's buttons, in paint order.
const HINTS: [(&str, &str); 3] = [("←→", " scrub  "), ("↑↓", " range  "), ("esc", " close")];

/// The page's geometry inside `area` (the whole screen).
#[must_use]
pub fn parts(area: Rect) -> Parts {
    let header = Rect {
        height: 1.min(area.height),
        ..area
    };
    let hint = Rect {
        y: area.bottom().saturating_sub(1),
        height: 1.min(area.height.saturating_sub(1)),
        ..area
    };
    let body = Rect {
        y: area.y.saturating_add(1),
        height: area.height.saturating_sub(2),
        ..area
    };
    let mut chips = Vec::with_capacity(Span::ALL.len());
    let mut x = area
        .x
        .saturating_add(u16::try_from(TITLE.len()).unwrap_or(0) + 1);
    let close_w = crate::layout::CORNER_W;
    let close = (area.width > close_w + 1).then(|| {
        Rect::new(
            area.right().saturating_sub(close_w + 1),
            header.y,
            close_w,
            header.height,
        )
    });
    let chips_end = close.map_or(area.right(), |r| r.x.saturating_sub(1));
    for span in Span::ALL {
        let w = u16::try_from(span.label().len() + 2).unwrap_or(0);
        if x.saturating_add(w) > chips_end {
            break;
        }
        chips.push((Rect::new(x, header.y, w, 1), span));
        x = x.saturating_add(w + 1);
    }
    let (graphs_outer, card) = if body.width >= SIDE_CARD_MIN_W {
        let graphs = Rect {
            width: body.width - CARD_W - 1,
            ..body
        };
        let card = Rect {
            x: graphs.right() + 1,
            width: CARD_W,
            ..body
        };
        (graphs, Some(card))
    } else if body.height > BOTTOM_CARD_H + 2 * MIN_GRAPH_H {
        let graphs = Rect {
            height: body.height - BOTTOM_CARD_H,
            ..body
        };
        let card = Rect {
            y: graphs.bottom(),
            height: BOTTOM_CARD_H,
            ..body
        };
        (graphs, Some(card))
    } else {
        (body, None)
    };
    let graphs = Rect {
        height: graphs_outer.height.saturating_sub(1),
        ..graphs_outer
    };
    let axis = Rect {
        y: graphs.bottom(),
        height: 1.min(graphs_outer.height),
        ..graphs_outer
    };
    let hint_close = {
        let before: usize = HINTS[..2].iter().map(|(k, v)| k.width() + v.width()).sum();
        let w = HINTS[2].0.width() + HINTS[2].1.width();
        let x = hint
            .x
            .saturating_add(u16::try_from(before).unwrap_or(u16::MAX));
        (hint.height > 0 && usize::from(x) + w <= usize::from(hint.right()))
            .then(|| Rect::new(x, hint.y, u16::try_from(w).unwrap_or(0), 1))
    };
    Parts {
        header,
        chips,
        close,
        hint_close,
        graphs,
        plot_x: graphs.x.saturating_add(1),
        plot_w: graphs.width.saturating_sub(2),
        axis,
        card,
        hint,
    }
}

/// Columns the app should bin history into for a screen of `area`.
#[must_use]
pub fn plot_width(area: Rect) -> usize {
    usize::from(parts(area).plot_w)
}

/// Where a click on the page landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineHit {
    Range(Span),
    /// The `[×]` or the hint row's `esc close`.
    Close,
    /// A plot column: move the scrub cursor there.
    Scrub(usize),
}

#[must_use]
pub fn hit(area: Rect, col: u16, row: u16) -> Option<TimelineHit> {
    let p = parts(area);
    let inside = |r: Rect| col >= r.x && col < r.right() && row >= r.y && row < r.bottom();
    if p.close.into_iter().chain(p.hint_close).any(inside) {
        return Some(TimelineHit::Close);
    }
    if let Some((_, span)) = p.chips.iter().find(|(r, _)| inside(*r)) {
        return Some(TimelineHit::Range(*span));
    }
    let plot_rows = Rect {
        x: p.plot_x,
        width: p.plot_w,
        ..Rect {
            height: p.graphs.height + p.axis.height,
            ..p.graphs
        }
    };
    inside(plot_rows).then(|| TimelineHit::Scrub(usize::from(col - p.plot_x)))
}

/// The rows that fit, with their heights: hidden hardware first, then the
/// lowest rows once boxes would drop below [`MIN_GRAPH_H`].
fn visible_rows(view: &TimelineView, height: u16) -> Vec<(Row, u16)> {
    let mut rows: Vec<Row> = ROWS
        .into_iter()
        .filter(|r| siltop_core::store::present(&view.columns, r.series))
        .collect();
    let fits = usize::from(height / MIN_GRAPH_H);
    rows.truncate(fits);
    let n = u16::try_from(rows.len()).unwrap_or(1).max(1);
    let (each, extra) = (height / n, height % n);
    rows.into_iter()
        .enumerate()
        .map(|(i, r)| (r, each + u16::from(u16::try_from(i).unwrap_or(0) < extra)))
        .collect()
}

pub(crate) fn render(
    frame: &mut Frame,
    area: Rect,
    view: &TimelineView,
    status: Option<&str>,
    theme: &Theme,
) {
    let p = parts(area);
    render_header(frame, &p, view, theme);
    let rows = visible_rows(view, p.graphs.height);
    if rows.is_empty() {
        let msg = if view.saved {
            "nothing recorded in this range yet"
        } else {
            "history starts with this session: give it a minute"
        };
        let mid = Rect {
            y: p.graphs.y + p.graphs.height / 2,
            height: 1.min(p.graphs.height),
            ..p.graphs
        };
        frame.render_widget(
            Paragraph::new(TextSpan::styled(msg, theme.dim())).centered(),
            mid,
        );
    }
    let mut y = p.graphs.y;
    for (row, h) in rows {
        render_graph(
            frame,
            Rect {
                y,
                height: h,
                ..p.graphs
            },
            row,
            view,
            theme,
        );
        y += h;
    }
    render_axis(frame, &p, view, theme);
    if let Some(card) = p.card {
        render_card(frame, card, view, theme);
    }
    render_hint(frame, p.hint, status, theme);
}

fn render_header(frame: &mut Frame, p: &Parts, view: &TimelineView, theme: &Theme) {
    let mut spans = vec![TextSpan::styled(TITLE, theme.title())];
    let mut x = p.header.x + u16::try_from(TITLE.len()).unwrap_or(0);
    for (rect, span) in &p.chips {
        spans.push(TextSpan::raw(
            " ".repeat(usize::from(rect.x.saturating_sub(x))),
        ));
        let style = if *span == view.span {
            theme
                .title()
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            theme.dim()
        };
        spans.push(TextSpan::styled(format!(" {} ", span.label()), style));
        x = rect.right();
    }
    if let Some(close) = p.close {
        spans.push(TextSpan::raw(
            " ".repeat(usize::from(close.x.saturating_sub(x))),
        ));
        spans.push(TextSpan::styled(crate::layout::CORNER_CLOSE, theme.dim()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), p.header);
}

fn render_graph(frame: &mut Frame, area: Rect, row: Row, view: &TimelineView, theme: &Theme) {
    let means = siltop_core::store::values(&view.columns, row.series, false);
    let at_cursor = view.cursor.and_then(|c| value_at(row, &view.columns, c));
    let inner = cell_titled(frame, area, row.label, at_cursor.as_deref(), false, theme);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let peaks = siltop_core::store::values(&view.columns, row.series, true);
    let max = row.fixed.unwrap_or_else(|| {
        nice_ceiling(
            peaks
                .iter()
                .flatten()
                .copied()
                .fold(0.0_f32, f32::max)
                .max(1.0),
        )
    });
    // One bucket per cell column; both braille dots of a cell show it.
    let dots: Vec<Option<f32>> = means
        .iter()
        .take(usize::from(inner.width))
        .flat_map(|v| {
            let d = v.map(|v| (v / max).clamp(0.0, 1.0));
            [d, d]
        })
        .collect();
    let cells = render_dots(&dots, inner.width, inner.height, Grow::Up);
    let n = cells.len();
    let ramp = ramp(theme, row.series);
    paint_rows(frame.buffer_mut(), inner, &cells, |i| {
        theme.graph_ink(&ramp, i, n, Thermal::Nominal, false)
    });
    if row.fixed.is_none() {
        let label = format!(" {} ", format_value(row.series, max));
        let x = area
            .right()
            .saturating_sub(u16::try_from(label.len()).unwrap_or(0) + 1);
        if x > area.x + 12 {
            frame.buffer_mut().set_string(x, area.y, label, theme.dim());
        }
    }
    if let Some(c) = view.cursor.and_then(|c| u16::try_from(c).ok())
        && c < inner.width
    {
        let x = inner.x + c;
        let buf = frame.buffer_mut();
        for y in inner.y..inner.bottom() {
            let cell = &mut buf[(x, y)];
            if cell.symbol() == " " {
                cell.set_char('│').set_style(theme.faint());
            } else {
                cell.set_style(theme.title().add_modifier(Modifier::BOLD));
            }
        }
    }
}

fn render_axis(frame: &mut Frame, p: &Parts, view: &TimelineView, theme: &Theme) {
    if p.axis.height == 0 || p.plot_w < 12 {
        return;
    }
    let dated = view.dated();
    let left = clock_label(view.from, view.utc_offset_s, dated);
    let buf = frame.buffer_mut();
    buf.set_string(p.plot_x, p.axis.y, &left, theme.dim());
    let right = "now";
    let rx = p.plot_x + p.plot_w - u16::try_from(right.len()).unwrap_or(0);
    buf.set_string(rx, p.axis.y, right, theme.dim());
    if let Some(c) = view.cursor.and_then(|c| u16::try_from(c).ok()) {
        let t = view.time_at(usize::from(c));
        let label = format!("▲ {}", clock_label(t, view.utc_offset_s, dated));
        let w = u16::try_from(label.chars().count()).unwrap_or(0);
        let x = (p.plot_x + c).min(p.plot_x + p.plot_w.saturating_sub(w));
        buf.set_string(x, p.axis.y, label, theme.title());
    }
}

/// A blame list: its title, rows, and how its values print.
type BlameList<'a> = (&'static str, &'a [Named], fn(f32) -> String);

fn proc_line(p: &Named, value: &str, theme: &Theme) -> Line<'static> {
    let mut spans = vec![TextSpan::styled(format!("  {value:>6} "), theme.fg())];
    push_token(&mut spans, p.name.clone(), theme.dim());
    if p.count > 1 {
        push_token(&mut spans, format!("×{}", p.count), theme.muted());
    }
    Line::from(spans)
}

fn render_card(frame: &mut Frame, area: Rect, view: &TimelineView, theme: &Theme) {
    let when = view
        .cursor
        .map(|c| clock_label(view.time_at(c), view.utc_offset_s, view.dated()));
    let inner = cell_titled(frame, area, "blame", when.as_deref(), false, theme);
    let blame = view.blame();
    let mut lines: Vec<Line<'static>> = Vec::new();
    if blame.is_empty() {
        lines.push(Line::styled("nothing recorded here", theme.dim()));
    }
    for event in &blame.events {
        lines.push(Line::styled(
            format!("• {}", event.label()),
            theme.warn_style(),
        ));
    }
    let lists: [BlameList<'_>; 3] = [
        ("cpu", &blame.cpu, |v| format!("{v:.0}%")),
        ("energy", &blame.energy, watts_display),
        ("memory", &blame.mem, |v| bytes_short(v.max(0.0) as u64)),
    ];
    for (title, list, fmt) in lists {
        if list.is_empty() {
            continue;
        }
        lines.push(Line::styled(title, theme.muted()));
        lines.extend(list.iter().map(|p| proc_line(p, &fmt(p.value), theme)));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The key hints, or the current toast in their place (as the settings
/// page does): "history unreadable" belongs on the page it explains.
fn render_hint(frame: &mut Frame, area: Rect, status: Option<&str>, theme: &Theme) {
    if let Some(status) = status {
        let line = Line::from(TextSpan::styled(format!(" {status}"), theme.fg()));
        frame.render_widget(Paragraph::new(line), area);
        return;
    }
    let spans: Vec<TextSpan<'static>> = HINTS
        .iter()
        .flat_map(|(key, verb)| {
            [
                TextSpan::styled(*key, theme.title()),
                TextSpan::styled(*verb, theme.dim()),
            ]
        })
        .collect();
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use siltop_core::store::{Stat, Tier, TopProc, bin, events};

    fn bucket(t: u64, with_gpu: bool) -> Bucket {
        let mut b = Bucket::empty(t);
        b.samples = 6;
        let s = |v| Some(Stat { mean: v, max: v });
        b.series[Series::Cpu.index()] = s(0.4);
        b.series[Series::Mem.index()] = s(0.6);
        b.series[Series::CpuTempC.index()] = s(55.0);
        if with_gpu {
            b.series[Series::Gpu.index()] = s(0.2);
        }
        b.top_cpu = vec![TopProc {
            pid: 7,
            name: "cargo".into(),
            value: 180.0,
        }];
        b
    }

    fn view(width: usize, with_gpu: bool) -> TimelineView {
        let buckets: Vec<Bucket> = (0..600).map(|i| bucket(i * 6, with_gpu)).collect();
        let columns = bin(&buckets, Tier::Day, 0, 3600, width);
        let events = events(&columns);
        TimelineView {
            span: Span::Hour,
            from: 0,
            to: 3600,
            cursor: siltop_core::store::last_filled(&columns),
            columns,
            events,
            utc_offset_s: 0,
            saved: false,
            ..TimelineView::default()
        }
    }

    fn paint_buf(v: &TimelineView, w: u16, h: u16, status: Option<&str>) -> Buffer {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| render(f, f.area(), v, status, &Theme::default()))
            .unwrap();
        term.backend().buffer().clone()
    }

    fn paint(v: &TimelineView, w: u16, h: u16) -> String {
        let buf = paint_buf(v, w, h, None);
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn history_unreadable_toast_shows_on_the_timeline() {
        // Seen live: the store error toast was raised while the page was
        // open but never drawn; the page only said nothing was recorded.
        let v = view(plot_width(Rect::new(0, 0, 100, 30)), true);
        let text = |status| {
            let buf = paint_buf(&v, 100, 30, status);
            (0..100)
                .map(|x| buf[(x, 29)].symbol().to_owned())
                .collect::<String>()
        };
        assert!(text(None).contains("scrub"));
        let shown = text(Some("history unreadable: permission denied"));
        assert!(
            shown.contains("history unreadable: permission denied"),
            "{shown}"
        );
        assert!(!shown.contains("scrub"), "{shown}");
    }

    #[test]
    fn timeline_text_is_never_drawn_in_the_faint_ink() {
        // The faint token (1.5–1.7:1) is for tracks and corner marks: the
        // empty-state messages and `×N` were unreadable.
        let theme = Theme::default();
        let width = plot_width(Rect::new(0, 0, 100, 30));
        let empty = TimelineView {
            columns: bin(&[], Tier::Day, 0, 3600, width),
            events: Vec::new(),
            cursor: None,
            ..view(width, true)
        };
        for v in [view(width, true), empty] {
            let buf = paint_buf(&v, 100, 30, None);
            for pos in buf.area.positions() {
                let cell = &buf[pos];
                if cell.symbol().chars().any(char::is_alphanumeric) {
                    assert_ne!(cell.fg, theme.faint, "{pos:?} {:?}", cell.symbol());
                }
            }
        }
    }

    /// Property sweep: every size from tiny to huge paints without panic,
    /// and the plot never pokes outside the screen.
    #[test]
    fn every_size_paints_inside_the_screen() {
        for w in [1, 5, 20, 40, 60, 80, 99, 100, 120, 200] {
            for h in [1, 2, 3, 5, 8, 12, 20, 40, 60] {
                let area = Rect::new(0, 0, w, h);
                let p = parts(area);
                assert!(p.plot_x + p.plot_w <= w, "{w}x{h}");
                let v = view(plot_width(area), true);
                let _ = paint(&v, w, h);
            }
        }
    }

    #[test]
    fn range_chips_are_hit_where_painted() {
        let area = Rect::new(0, 0, 120, 40);
        let text = paint(&view(plot_width(area), true), 120, 40);
        let header = text.lines().next().unwrap();
        for (rect, span) in parts(area).chips {
            let drawn: String = header
                .chars()
                .skip(usize::from(rect.x))
                .take(usize::from(rect.width))
                .collect();
            assert_eq!(drawn.trim(), span.label());
            assert_eq!(hit(area, rect.x + 1, 0), Some(TimelineHit::Range(span)));
        }
    }

    #[test]
    fn a_plot_click_scrubs_to_that_column() {
        let area = Rect::new(0, 0, 90, 30);
        let p = parts(area);
        assert_eq!(
            hit(area, p.plot_x, p.graphs.y + 1),
            Some(TimelineHit::Scrub(0))
        );
        assert_eq!(
            hit(area, p.plot_x + 10, p.axis.y),
            Some(TimelineHit::Scrub(10))
        );
        assert_eq!(hit(area, p.plot_x.saturating_sub(1), p.graphs.y + 1), None);
        assert_eq!(hit(area, 5, p.hint.y), None);
    }

    #[test]
    fn scrub_clamps_to_the_plot() {
        let mut v = view(50, false);
        v.cursor = Some(2);
        v.scrub(-10);
        assert_eq!(v.cursor, Some(0));
        v.scrub(1000);
        assert_eq!(v.cursor, Some(49));
        let mut empty = TimelineView::default();
        empty.scrub(1);
        assert_eq!(empty.cursor, Some(0));
    }

    #[test]
    fn blame_card_names_the_busy_process() {
        let area = Rect::new(0, 0, 130, 36);
        let text = paint(&view(plot_width(area), true), 130, 36);
        assert!(text.contains("blame"), "{text}");
        assert!(text.contains("cargo"), "{text}");
        assert!(text.contains("180%"), "{text}");
    }

    /// Regression (UI-DESIGN §2): a Mac with no GPU reading gets no gpu box.
    #[test]
    fn absent_gpu_hides_its_row() {
        let area = Rect::new(0, 0, 130, 40);
        let with = paint(&view(plot_width(area), true), 130, 40);
        let without = paint(&view(plot_width(area), false), 130, 40);
        assert!(with.contains(" gpu"), "{with}");
        assert!(!without.contains(" gpu"), "{without}");
        assert!(without.contains(" cpu"));
    }

    #[test]
    fn empty_history_says_so_instead_of_drawing_zeros() {
        let v = TimelineView {
            columns: vec![None; 80],
            ..TimelineView::default()
        };
        let text = paint(&v, 120, 30);
        assert!(text.contains("history starts with this session"), "{text}");
        assert!(!text.contains("0%"), "{text}");
    }

    #[test]
    fn cursor_time_is_stamped_on_the_axis() {
        let area = Rect::new(0, 0, 130, 30);
        let mut v = view(plot_width(area), true);
        v.cursor = Some(0);
        let text = paint(&v, 130, 30);
        assert!(text.contains("▲ 00:00"), "{text}");
    }

    #[test]
    fn the_cursor_names_its_newest_sample_not_the_rounded_column_start() {
        let mut v = view(5, true);
        v.stamps = vec![None, None, None, None, Some(3594)];
        v.cursor = Some(4);
        assert_eq!(siltop_core::store::column_start(4, 0, 3600, 5), 2880);
        assert_eq!(v.time_at(4), 3594, "the right edge is the last sample");
        assert_eq!(
            v.time_at(0),
            0,
            "an unstamped column falls back to its start"
        );
        let text = paint(&v, 60, 30);
        assert!(text.contains("00:59"), "{text}");
        assert!(!text.contains("00:48"), "{text}");
    }

    /// A laptop hour: on AC at 90 % charging 20 W, then unplugged for the
    /// second half, draining 8 W down to 64 %.
    fn laptop_view(width: usize) -> TimelineView {
        let s = |v| Some(Stat { mean: v, max: v });
        let buckets: Vec<Bucket> = (0..600)
            .map(|i| {
                let mut b = bucket(i * 6, true);
                let off = i >= 300;
                b.series[Series::OnAc.index()] = s(if off { 0.0 } else { 1.0 });
                b.series[Series::BatteryPct.index()] = s(if off { 64.0 } else { 90.0 });
                b.series[Series::BatteryW.index()] = s(if off { -8.0 } else { 20.0 });
                b
            })
            .collect();
        let columns = bin(&buckets, Tier::Day, 0, 3600, width);
        let events = events(&columns);
        TimelineView {
            span: Span::Hour,
            from: 0,
            to: 3600,
            cursor: siltop_core::store::last_filled(&columns),
            columns,
            events,
            utc_offset_s: 0,
            saved: true,
            ..TimelineView::default()
        }
    }

    #[test]
    fn a_laptop_gets_a_battery_row_with_charge_and_drain_at_the_cursor() {
        let area = Rect::new(0, 0, 130, 40);
        let text = paint(&laptop_view(plot_width(area)), 130, 40);
        assert!(text.contains("battery"), "{text}");
        assert!(text.contains("64% ▼ 8.0W"), "{text}");
    }

    /// Desktops (and 1.0 history) never recorded a battery: no row, no 0 %.
    #[test]
    fn a_desktop_has_no_battery_row() {
        let area = Rect::new(0, 0, 130, 40);
        let text = paint(&view(plot_width(area), true), 130, 40);
        assert!(!text.contains("battery"), "{text}");
    }

    #[test]
    fn charging_and_idle_battery_values_read_plainly() {
        assert_eq!(format_value(Series::BatteryW, 20.0), "▲ 20W");
        assert_eq!(format_value(Series::BatteryW, -8.0), "▼ 8.0W");
        assert_eq!(format_value(Series::BatteryW, 0.01), "idle");
        assert_eq!(format_value(Series::BatteryPct, 63.6), "64%");
    }

    #[test]
    fn the_unplug_shows_in_the_blame_card_at_its_column() {
        let area = Rect::new(0, 0, 130, 40);
        let mut v = laptop_view(plot_width(area));
        let (col, _) = *v
            .events
            .iter()
            .find(|(_, e)| *e == Event::Unplugged)
            .unwrap();
        v.cursor = Some(col);
        let text = paint(&v, 130, 40);
        assert!(text.contains("unplugged, on battery"), "{text}");
        assert!(text.contains("╭ battery "), "{text}");
    }
}
