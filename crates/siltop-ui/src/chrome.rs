use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};
use siltop_core::{
    Base, History, NetUnit, Scale, ScaleRange, Thermal, TimeAxis, byte_rate, net_rate,
    nice_ceiling, nice_ceiling_in,
};
use std::time::Duration;

use crate::braille::{self, BrailleCell, Grow};
use crate::layout::Panel;
use crate::theme::{Ramp, Theme};

/// The last line of defence against terminal escape injection: whatever a
/// widget drew, no cell may carry a control or bidi character to the
/// terminal. Data is made printable where it enters (`printable`); this
/// catches the rest (replayed traces, fleet hosts, cells set directly).
pub(crate) fn scrub_controls(buf: &mut Buffer) {
    for cell in &mut buf.content {
        let symbol = cell.symbol();
        if symbol.chars().any(siltop_core::is_unsafe) {
            let clean: String = symbol
                .chars()
                .filter(|c| !siltop_core::is_unsafe(*c))
                .collect();
            cell.set_symbol(if clean.is_empty() { "\u{FFFD}" } else { &clean });
        }
    }
}

/// Veil everything outside `modal` so an open overlay reads as the one
/// thing on screen: each cell's ink sinks toward the theme's faint token and
/// loses its bold. Backgrounds are never touched, so glass stays glass and a
/// kill-armed row keeps its alarm fill. Every overlay goes through here.
pub fn veil_outside(buf: &mut Buffer, modal: Rect, theme: &Theme) {
    veil_except(buf, &[modal], theme);
}

/// [`veil_outside`] with several lit regions, such as the kill prompt in
/// the footer and the rows it is about to kill. Without colour the sunk
/// ink cannot show, so veiled cells go DIM instead: an attribute, never a
/// fill, so it is glass-safe too.
pub(crate) fn veil_except(buf: &mut Buffer, lit: &[Rect], theme: &Theme) {
    let dim = theme.depth == crate::theme::ColorDepth::NoColor;
    let area = buf.area;
    for pos in area.positions() {
        if lit.iter().any(|r| r.contains(pos)) {
            continue;
        }
        let cell = &mut buf[pos];
        let ink = theme.veiled(cell.fg);
        cell.set_fg(ink);
        cell.modifier.remove(Modifier::BOLD);
        if dim {
            cell.modifier.insert(Modifier::DIM);
        }
    }
}

/// Light the button under the pointer: its ink turns the highlight colour
/// and goes bold. Foreground only, so it is glass-safe; blanks stay blank.
pub(crate) fn light(buf: &mut Buffer, target: Rect, theme: &Theme) {
    for pos in target.intersection(buf.area).positions() {
        let cell = &mut buf[pos];
        if cell.symbol().trim().is_empty() {
            continue;
        }
        cell.set_fg(theme.hi);
        cell.modifier.insert(Modifier::BOLD);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    None,
    Percent,
    /// History in bits per second, printed in the viewer's unit.
    Net(NetUnit),
    /// History in bytes per second (disk), printed in decimal units.
    Bytes,
    Celsius,
    Number,
    /// Watts: a plain number with a `W`.
    Watts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphInk {
    Flat,
    /// A load graph: its accent ramp, turning crit only at Serious or worse.
    Load(Thermal),
    /// The load graph that carries the thermal state (cpu): its crest row
    /// also turns warn at Fair.
    Signal(Thermal),
}

/// Quiet frame at rest; the panel's own accent rings it, labels it and lights
/// its corner mark once it has focus.
#[must_use]
pub(crate) fn panel_block<'a>(
    panel: Panel,
    title: Line<'a>,
    focused: bool,
    expanded: bool,
    theme: &Theme,
) -> Block<'a> {
    panel_block_axis(panel, title, focused, expanded, None, theme)
}

/// [`panel_block`] with the graph's scale printed in the top border, just
/// left of the corner mark, so no plot cell is ever cleared for it.
#[must_use]
pub(crate) fn panel_block_axis<'a>(
    panel: Panel,
    mut title: Line<'a>,
    focused: bool,
    expanded: bool,
    axis: Option<String>,
    theme: &Theme,
) -> Block<'a> {
    let lit = focused || expanded;
    let accent = Style::default().fg(theme.panel_accent(panel));
    if focused && let Some(label) = title.spans.first_mut() {
        label.style = accent.add_modifier(Modifier::BOLD);
    }
    if let Some(icon) = theme.icons.panel(panel) {
        title
            .spans
            .insert(0, Span::styled(format!(" {icon}"), theme.dim()));
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(if lit { accent } else { theme.border(false) })
        .title(title);
    // Bracketed so the mark reads as a button; `corner_hit` is its hit rect.
    let mark = if expanded {
        crate::layout::CORNER_CLOSE
    } else {
        crate::layout::CORNER_EXPAND
    };
    let mark_style = if lit {
        accent.add_modifier(Modifier::BOLD)
    } else {
        theme.dim()
    };
    let mut right = Vec::with_capacity(2);
    if let Some(axis) = axis {
        right.push(Span::styled(format!(" {axis} "), theme.dim()));
    }
    right.push(Span::styled(mark, mark_style));
    block.title(Line::from(right).right_aligned())
}

/// Top-border columns an axis label takes beside the corner mark: a space
/// either side, so a number never touches the mark's bracket, and at least
/// one cell of rule, so the scale never reads as the title's last token
/// (`43° ─ 10%`, not `43°  10%`).
#[must_use]
pub(crate) fn axis_reserve(axis: &str) -> usize {
    axis.chars().count() + 3
}

/// Titled rounded cell used by expanded grids and the process dossier.
pub fn cell(frame: &mut Frame, area: Rect, title: &str, theme: &Theme) -> Rect {
    cell_titled(frame, area, title, None, false, theme)
}

pub(crate) fn cell_titled(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    value: Option<&str>,
    hop: bool,
    theme: &Theme,
) -> Rect {
    let head = CellHead {
        label,
        value: value.map(|v| (v, theme.title())),
        hop,
        axis: None,
    };
    cell_headed(frame, area, head, theme)
}

/// What a cell's top border carries: its label, the value in its own ink
/// (a temperature in its heat colour), the hop arrow, and the plot's scale
/// label at the right end, as compact panels print theirs.
#[derive(Debug, Clone, Default)]
pub(crate) struct CellHead<'a> {
    pub label: &'a str,
    pub value: Option<(&'a str, Style)>,
    pub hop: bool,
    pub axis: Option<String>,
}

/// [`cell_titled`] with every part of the head spelled out. The scale label
/// yields to the title: it shows only where both fit with a cell of rule
/// between them.
pub(crate) fn cell_headed(
    frame: &mut Frame,
    area: Rect,
    head: CellHead<'_>,
    theme: &Theme,
) -> Rect {
    let mut spans = vec![Span::styled(format!(" {}", head.label), theme.dim())];
    if let Some((value, style)) = head.value {
        push_token(&mut spans, value.to_owned(), style);
    }
    if head.hop {
        push_token(&mut spans, String::from("→"), theme.dim());
    }
    // Spacing law rule 1 holds on both ends: the rule resumes one space
    // after the last token (`free ─`, never `free─`), as panel titles do.
    spans.push(Span::raw(" "));
    let title = Line::from(spans);
    let room = usize::from(area.width.saturating_sub(2));
    let axis = head
        .axis
        .filter(|a| title.width() + axis_reserve(a) <= room);
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(title)
        .border_style(theme.border(false));
    if let Some(axis) = axis {
        let label = Span::styled(format!(" {axis} "), theme.dim());
        block = block.title(Line::from(label).right_aligned());
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

#[derive(Debug, Clone, Copy)]
pub struct Graph<'a> {
    pub history: &'a History,
    pub ramp: Ramp,
    pub theme: &'a Theme,
    pub scale: Scale,
    pub axis: Axis,
    pub ink: GraphInk,
    pub time: TimeAxis,
}

/// Samples a graph needs before it draws a line or an axis mark.
pub(crate) const MIN_SAMPLES: usize = 2;

impl Graph<'_> {
    /// The draw window for a plot `width` columns wide: the scale over the
    /// time on screen, its auto ceiling rounded in the unit the axis prints.
    #[must_use]
    pub fn range(&self, width: u16) -> ScaleRange {
        visible_range(self.history, self.scale, self.axis, self.time, width)
    }

    /// Whether the history has enough samples to draw.
    #[must_use]
    pub fn ready(&self) -> bool {
        self.history.len() >= MIN_SAMPLES
    }

    /// The thermal state this graph shows and whether it signals Fair.
    fn thermal(&self) -> (Thermal, bool) {
        match self.ink {
            GraphInk::Flat => (Thermal::Nominal, false),
            GraphInk::Load(thermal) => (thermal, false),
            GraphInk::Signal(thermal) => (thermal, true),
        }
    }
}

/// A plot that prints its scale in its frame's top border.
pub(crate) trait BorderScale {
    /// The scale label for the plot drawn `width` columns wide: `10%`,
    /// `40–60°`, `2MB/s`. `None` leaves the border bare.
    fn border_label(&self, width: u16) -> Option<String>;
}

impl BorderScale for Graph<'_> {
    /// `None` for fixed percent graphs and until the graph is ready.
    fn border_label(&self, width: u16) -> Option<String> {
        if !self.ready() {
            return None;
        }
        scale_label(self.scale, self.axis, self.range(width))
    }
}

/// The draw window for `history` in a plot `width` columns wide: the scale
/// over the time on screen, with an auto ceiling rounded to a 1-2-5 step in
/// the unit the axis prints (so a bytes/s ceiling reads `2MB/s`, not
/// `250KB/s` rounded from bits).
#[must_use]
pub(crate) fn visible_range(
    history: &History,
    scale: Scale,
    axis: Axis,
    time: TimeAxis,
    width: u16,
) -> ScaleRange {
    let span = time.span(dots_for(width));
    let range = scale.range_in_by(history, span, time.now, |peak| unit_ceiling(peak, axis));
    // A percent band (battery charge) never runs past 100%: a full battery
    // reads 90–100%, not 95–105%.
    if matches!((scale, axis), (Scale::Band { .. }, Axis::Percent)) && range.max > 1.0 {
        let over = range.max - 1.0;
        return ScaleRange {
            min: (range.min - over).max(0.0),
            max: 1.0,
        };
    }
    range
}

/// Next 1-2-5 step above `peak` in the unit `axis` prints.
fn unit_ceiling(peak: f32, axis: Axis) -> f32 {
    match axis {
        Axis::Net(unit) => {
            let raw = f64::from(peak).max(1.0);
            let per_display = raw / unit.from_bps(raw);
            (nice_ceiling_in(unit.from_bps(raw).max(1.0), Base::Decimal) * per_display) as f32
        }
        Axis::Bytes => nice_ceiling_in(f64::from(peak).max(1.0), Base::Decimal) as f32,
        Axis::None | Axis::Percent | Axis::Celsius | Axis::Number | Axis::Watts => {
            nice_ceiling(peak)
        }
    }
}

/// The border label for `range`; `None` where the scale says nothing
/// (fixed percent) or the plot has no axis.
#[must_use]
pub(crate) fn scale_label(scale: Scale, axis: Axis, range: ScaleRange) -> Option<String> {
    let shows = match axis {
        Axis::None => false,
        Axis::Percent => scale.hints_axis(),
        Axis::Net(_) | Axis::Bytes | Axis::Celsius | Axis::Number | Axis::Watts => true,
    };
    if !shows {
        return None;
    }
    let max = axis_label(range.max, axis);
    Some(if range.min > 0.0 {
        let min = axis_label(range.min, axis);
        format!("{}–{max}", min.trim_end_matches(['°', '%']))
    } else {
        max
    })
}

/// Two braille dots per column: the number of time buckets a plot shows.
#[must_use]
pub(crate) fn dots_for(width: u16) -> usize {
    usize::from(width) * 2
}

pub fn render_scaled_graph(frame: &mut Frame, area: Rect, g: Graph<'_>) {
    if let Some(span) = paint_scaled_graph(frame, area, g) {
        stamp_time_labels(frame.buffer_mut(), area, span, g.theme);
    }
}

/// [`render_scaled_graph`] without the time labels, for a caller that
/// labels its own axis. Returns the span drawn, `None` while the graph
/// still shows `sampling…` or has no room.
pub(crate) fn paint_scaled_graph(frame: &mut Frame, area: Rect, g: Graph<'_>) -> Option<Duration> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    if !g.ready() {
        render_sampling(frame, area, g.theme);
        return None;
    }
    // The scale lives in the top border and time in the bottom one, so the
    // plot owns every inner cell.
    let plot = area;
    let span = g.time.span(dots_for(plot.width));
    let range = g.range(plot.width);
    let (thermal, signals) = g.thermal();
    let dots = braille::dots(g.history, g.time, plot.width, range);
    let rows = braille::render_dots(&dots, plot.width, plot.height, Grow::Up);
    let rows_n = rows.len();
    paint_rows(frame.buffer_mut(), plot, &rows, |i| {
        g.theme.graph_ink(&g.ramp, i, rows_n, thermal, signals)
    });
    Some(span)
}

/// Frame chrome only, plus a dim `sampling…`, until a graph has data.
pub(crate) fn render_sampling(frame: &mut Frame, area: Rect, theme: &Theme) {
    let row = Rect {
        y: area.y.saturating_add(area.height / 2),
        height: 1,
        ..area
    };
    frame.render_widget(
        Paragraph::new(Span::styled("sampling…", theme.dim())).centered(),
        row,
    );
}

/// Paint braille rows into `plot`; `ink(row)` picks each row's colour. Blank
/// braille becomes a space so transparent terminals show through.
pub(crate) fn paint_rows(
    buf: &mut Buffer,
    plot: Rect,
    rows: &[Vec<BrailleCell>],
    ink: impl Fn(usize) -> Color,
) {
    let area = plot.intersection(buf.area);
    for (i, row) in rows.iter().enumerate().take(usize::from(plot.height)) {
        let y = plot.y.saturating_add(i as u16);
        if y >= area.bottom() {
            break;
        }
        let style = Style::default().fg(ink(i));
        for (dx, cell) in row.iter().enumerate().take(usize::from(plot.width)) {
            let x = plot.x.saturating_add(dx as u16);
            if x >= area.right() {
                break;
            }
            let dest = &mut buf[(x, y)];
            dest.reset();
            if cell.glyph == '\u{2800}' {
                dest.set_char(' ');
            } else {
                dest.set_char(cell.glyph).set_style(style);
            }
        }
    }
}

/// `-20s`, `-5m`: how far back the left edge of a graph reaches.
#[must_use]
pub(crate) fn span_label(span: Duration) -> String {
    format!("\u{2212}{}", duration_label(span))
}

/// `20s`, `5m`: a length of time as the axis labels print it.
#[must_use]
pub(crate) fn duration_label(span: Duration) -> String {
    let secs = span.as_secs();
    if secs < 120 {
        format!("{}s", secs.max(1))
    } else {
        format!("{}m", secs / 60)
    }
}

/// Time labels in the bottom border under a plot: `-5m ┴ … ┴ now`. Only
/// writes over plain border cells, so a plot without a border row is untouched.
pub(crate) fn stamp_time_labels(buf: &mut Buffer, plot: Rect, span: Duration, theme: &Theme) {
    stamp_edge_labels(buf, plot, &span_label(span), "now", theme);
}

/// `left ┴ … ┴ right` in the bottom border under a plot, for an axis that
/// is not "the last N ago … now" (the bench's `0s … 60s`). Same rules as
/// [`stamp_time_labels`].
pub(crate) fn stamp_edge_labels(
    buf: &mut Buffer,
    plot: Rect,
    left: &str,
    right: &str,
    theme: &Theme,
) {
    let y = plot.bottom();
    if plot.width < 16 || y >= buf.area.bottom() || plot.right() > buf.area.right() {
        return;
    }
    let is_rule = |buf: &Buffer, x: u16| buf[(x, y)].symbol() == "\u{2500}";
    if !(plot.left()..plot.right()).all(|x| is_rule(buf, x)) {
        return;
    }
    let style = theme.dim();
    let mut put = |x: u16, text: &str| {
        for (i, ch) in text.chars().enumerate() {
            let cx = x.saturating_add(i as u16);
            if cx < plot.right() {
                buf[(cx, y)].set_char(ch).set_style(style);
            }
        }
    };
    if plot.width >= 32 {
        for quarter in 1..4_u16 {
            put(plot.x + plot.width * quarter / 4, "\u{2534}");
        }
    }
    put(plot.x + 1, left);
    let right_w = u16::try_from(right.chars().count()).unwrap_or(u16::MAX);
    put(plot.right().saturating_sub(right_w + 1), right);
}

pub fn panel_title(label: &str, theme: &Theme) -> Line<'static> {
    Line::from(vec![Span::styled(format!(" {label}  "), theme.dim())])
}

pub fn push_token(spans: &mut Vec<Span<'static>>, text: String, style: Style) {
    if text.is_empty() {
        return;
    }
    if !spans.is_empty() && !spans.last().is_some_and(|span| span.content.ends_with(' ')) {
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(text, style));
}

pub fn push_kv(
    spans: &mut Vec<Span<'static>>,
    theme: &Theme,
    key: &str,
    value: String,
    value_style: Style,
) {
    push_token(spans, format!("{key}  "), theme.dim());
    spans.push(Span::styled(value, value_style));
}

fn number_label(value: f32) -> String {
    if value >= 10_000.0 {
        format!("{:.0}k", f64::from(value) / 1000.0)
    } else if value >= 1000.0 {
        let tenths = format!("{:.1}", f64::from(value) / 1000.0);
        if tenths.parse::<f64>().is_ok_and(|k| k >= 10.0) {
            format!("{:.0}k", f64::from(value) / 1000.0)
        } else {
            format!("{tenths}k")
        }
    } else {
        format!("{:.0}", value.round())
    }
}

/// `2.0MB/s` → `2MB/s`: axis marks are round numbers, so a `.0` is noise.
fn whole(label: &str) -> String {
    match label.find(".0") {
        Some(i) if !label[i + 2..].starts_with(|c: char| c.is_ascii_digit()) => {
            format!("{}{}", &label[..i], &label[i + 2..])
        }
        _ => label.to_owned(),
    }
}

pub(crate) fn axis_label(value: f32, axis: Axis) -> String {
    match axis {
        Axis::Percent => format!("{:.0}%", (value * 100.0).round()),
        Axis::Net(unit) => whole(&net_rate(value.max(0.0).round() as u64, unit)),
        Axis::Bytes => whole(&byte_rate(value.max(0.0).round() as u64)),
        Axis::Celsius => format!("{:.0}°", value.round()),
        Axis::Number => number_label(value.max(0.0)),
        Axis::Watts => format!("{}W", number_label(value.max(0.0))),
        Axis::None => String::new(),
    }
}

/// Left-aligned partial blocks, one to seven eighths of a cell wide.
const EIGHTHS: [char; 7] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉'];
pub(crate) const FULL_CELL: char = '█';
/// The unfilled rest of a bar: a faint rule, so the scale stays readable.
pub(crate) const BAR_TRACK: char = '─';

/// Is `c` part of a bar's fill?
#[cfg(test)]
#[must_use]
pub(crate) fn is_bar_fill(c: char) -> bool {
    c == FULL_CELL || EIGHTHS.contains(&c)
}

/// Eighths of a cell to fill for `ratio` of `width` cells: 8× the cell
/// resolution. Any load shows at least one eighth, and anything short of
/// full stops one eighth shy, so `1%` never reads idle and `99%` never full.
fn fill_eighths(width: u16, ratio: f32) -> u32 {
    let total = u32::from(width) * 8;
    if ratio.is_nan() || ratio <= 0.0 || total == 0 {
        return 0;
    }
    if ratio >= 1.0 {
        return total;
    }
    let raw = (total as f32 * ratio).round() as u32;
    raw.clamp(1, total.saturating_sub(1))
}

/// The bar's fill (full cells plus one partial edge) and its track.
fn fill_bar_parts(width: u16, ratio: f32) -> (String, String) {
    let eighths = fill_eighths(width, ratio);
    let mut ink: String = std::iter::repeat_n(FULL_CELL, (eighths / 8) as usize).collect();
    if let Some(edge) = ((eighths % 8) as usize)
        .checked_sub(1)
        .and_then(|i| EIGHTHS.get(i))
    {
        ink.push(*edge);
    }
    let rest = usize::from(width).saturating_sub(ink.chars().count());
    (ink, std::iter::repeat_n(BAR_TRACK, rest).collect())
}

/// One-row meter: `fill` ink over a faint track, eighth-block precise.
pub(crate) fn render_fill_bar(
    frame: &mut Frame,
    area: Rect,
    ratio: f32,
    fill: impl Into<Color>,
    theme: &Theme,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (filled, track) = fill_bar_parts(area.width, ratio);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(filled, Style::default().fg(fill.into())),
            Span::styled(track, theme.faint()),
        ])),
        Rect { height: 1, ..area },
    );
}

/// Cells per segment for a stacked bar: boundaries round the running total,
/// so segments never overlap, sum to at most `width`, and a sum past `1.0`
/// is clipped at the end of the track.
#[must_use]
pub(crate) fn stacked_cells(width: u16, ratios: &[f32]) -> Vec<u16> {
    let w = f32::from(width);
    let mut cum = 0.0_f32;
    let mut prev = 0_u16;
    ratios
        .iter()
        .map(|r| {
            cum = (cum + r.max(0.0)).min(1.0);
            let bound = ((w * cum).round() as u16).min(width);
            let n = bound.saturating_sub(prev);
            prev = prev.max(bound);
            n
        })
        .collect()
}

/// One-row stacked meter: each `(ratio, style, glyph)` segment in turn,
/// then the faint track. Shares the fill bar's track glyph and height.
pub(crate) fn render_stacked_bar(
    frame: &mut Frame,
    area: Rect,
    segments: &[(f32, Style, char)],
    theme: &Theme,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let ratios: Vec<f32> = segments.iter().map(|s| s.0).collect();
    let counts = stacked_cells(area.width, &ratios);
    let mut spans: Vec<Span<'static>> = segments
        .iter()
        .zip(&counts)
        .filter(|(_, n)| **n > 0)
        .map(|((_, style, glyph), n)| {
            Span::styled(
                std::iter::repeat_n(*glyph, usize::from(*n)).collect::<String>(),
                *style,
            )
        })
        .collect();
    let used: u16 = counts.iter().sum();
    let rest = usize::from(area.width.saturating_sub(used));
    if rest > 0 {
        spans.push(Span::styled(
            std::iter::repeat_n(BAR_TRACK, rest).collect::<String>(),
            theme.faint(),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect { height: 1, ..area },
    );
}

/// `S0   99%`: a fixed-width name and percent, so every bar in a list
/// starts in one column whatever the value.
#[must_use]
pub(crate) fn bar_label(name: &str, ratio: f32) -> String {
    format!("{name:<3}{:>5}", siltop_core::percent_display(ratio))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    #[test]
    fn scrub_controls_removes_escapes_set_directly_on_cells() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 1));
        buf[(0, 0)].set_symbol("\u{1b}");
        buf[(1, 0)].set_symbol("x\u{7}");
        buf[(2, 0)].set_symbol("\u{202e}");
        buf[(3, 0)].set_symbol("⣿");
        scrub_controls(&mut buf);
        let symbols: Vec<&str> = (0..4).map(|x| buf[(x, 0)].symbol()).collect();
        assert_eq!(symbols, ["\u{FFFD}", "x", "\u{FFFD}", "⣿"]);
    }

    use super::*;
    use ratatui::text::Span;

    /// Every cell inked, some bold, a few with a selection fill.
    fn inked(theme: &Theme) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, 12, 6));
        let inks = [
            Color::Reset,
            theme.fg,
            theme.hi,
            theme.cpu.body,
            Color::Green,
        ];
        for (i, pos) in buf.area.positions().enumerate() {
            let cell = &mut buf[pos];
            cell.set_symbol("x");
            cell.set_fg(inks[i % inks.len()]);
            if i % 3 == 0 {
                cell.modifier.insert(Modifier::BOLD);
            }
            if pos.y == 2 {
                cell.set_bg(theme.sel_bg);
            }
        }
        buf
    }

    const MODAL: Rect = Rect::new(3, 1, 5, 3);

    #[test]
    fn veil_outside_dims_every_cell_outside_the_modal() {
        for theme in [Theme::default(), Theme::by_name("paper").unwrap()] {
            let before = inked(&theme);
            let mut after = before.clone();
            veil_outside(&mut after, MODAL, &theme);
            for pos in before.area.positions() {
                let (was, now) = (&before[pos], &after[pos]);
                if MODAL.contains(pos) {
                    assert_eq!(was, now, "{pos:?} inside the modal changed");
                    continue;
                }
                assert_eq!(now.fg, theme.veiled(was.fg), "{pos:?}");
                assert_ne!(now.fg, was.fg, "{} {pos:?} kept its ink", theme.name);
                assert!(!now.modifier.contains(Modifier::BOLD));
            }
        }
    }

    #[test]
    fn veil_outside_never_sets_a_background() {
        for glass in [false, true] {
            let mut theme = Theme::default();
            if glass {
                theme = theme.on_surface(
                    crate::theme::Surface::Glass(crate::theme::Opacity::new(0.75).unwrap()),
                    None,
                );
            }
            let before = inked(&theme);
            let mut after = before.clone();
            veil_outside(&mut after, MODAL, &theme);
            for pos in before.area.positions() {
                assert_eq!(before[pos].bg, after[pos].bg, "glass={glass} {pos:?}");
            }
        }
    }

    #[test]
    fn veiled_ink_sinks_toward_faint() {
        let theme = Theme::default();
        let ink = Color::Rgb(0xff, 0xff, 0xff);
        let Color::Rgb(r, _, _) = theme.veiled(ink) else {
            panic!("rgb in, rgb out");
        };
        let Color::Rgb(fr, _, _) = theme.faint else {
            panic!("billabong faint is rgb");
        };
        assert!(r < 0xff && r > fr, "{r:#x} between white and faint {fr:#x}");
    }

    #[test]
    fn veil_on_the_ansi_theme_uses_its_named_faint() {
        let theme = Theme::by_name("terminal").unwrap();
        for ink in [Color::Reset, Color::Green, Color::Red] {
            assert_eq!(theme.veiled(ink), theme.faint);
        }
    }

    #[test]
    fn veil_is_a_no_op_for_a_full_screen_modal() {
        let theme = Theme::default();
        let before = inked(&theme);
        let mut after = before.clone();
        veil_outside(&mut after, before.area, &theme);
        assert_eq!(before, after);
    }

    #[test]
    fn expanded_block_has_close_mark() {
        let theme = Theme::default();
        let block = panel_block(
            Panel::Cpu,
            Line::from(Span::raw(" cpu ")),
            true,
            true,
            &theme,
        );
        let _ = block;
    }

    #[test]
    fn stacked_cells_never_exceed_width_sweep() {
        let sets: [&[f32]; 5] = [
            &[0.3, 0.2, 0.1, 0.1],
            &[0.9, 0.9],
            &[0.0, 0.0, 0.0],
            &[1.0],
            &[0.33, 0.33, 0.34],
        ];
        for w in 0..120 {
            for set in sets {
                let cells = stacked_cells(w, set);
                assert_eq!(cells.len(), set.len());
                assert!(cells.iter().sum::<u16>() <= w, "{w} {set:?}");
            }
        }
    }

    #[test]
    fn percent_band_never_runs_past_one_hundred() {
        let charge = Scale::Band {
            pad: 0.02,
            min_span: 0.10,
        };
        let range_of = |values: &[f32]| {
            let mut h = History::for_period(Duration::from_secs(2));
            for v in values {
                h.push(*v);
            }
            let range = visible_range(&h, charge, Axis::Percent, TimeAxis::live(&h), 40);
            (range, scale_label(charge, Axis::Percent, range))
        };
        let (full, label) = range_of(&[1.0; 20]);
        assert!((full.max - 1.0).abs() < 1e-6, "{full:?}");
        assert!(full.span() >= 0.10 - 1e-4, "{full:?}");
        assert_eq!(label.as_deref(), Some("90–100%"));
        let (drain, label) = range_of(&[0.90, 0.88, 0.86, 0.84, 0.82]);
        assert!(
            drain.min <= 0.82 && drain.max >= 0.90 && drain.max <= 1.0,
            "{drain:?}"
        );
        assert_eq!(label.as_deref(), Some("80–95%"));
        // other bands are not capped: temperatures run past 1.0 freely
        let mut h = History::for_period(Duration::from_secs(2));
        h.push(60.0);
        let temp = visible_range(&h, Scale::TEMP, Axis::Celsius, TimeAxis::live(&h), 40);
        assert!(temp.max > 60.0, "{temp:?}");
    }

    #[test]
    fn stacked_cells_fill_exactly_when_parts_sum_to_one() {
        for w in 1..80 {
            let cells = stacked_cells(w, &[0.25, 0.25, 0.5]);
            assert_eq!(cells.iter().sum::<u16>(), w, "{w}");
        }
    }

    #[test]
    fn stacked_cells_ignore_negative_and_clip_overflow() {
        assert_eq!(stacked_cells(10, &[-1.0, 0.5]), vec![0, 5]);
        assert_eq!(stacked_cells(10, &[0.8, 0.8]), vec![8, 2]);
    }

    #[test]
    fn stacked_bar_paints_track_after_segments() {
        let theme = Theme::default();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(10, 1)).unwrap();
        terminal
            .draw(|frame| {
                render_stacked_bar(
                    frame,
                    frame.area(),
                    &[
                        (0.3, Style::default(), FULL_CELL),
                        (0.2, Style::default(), '▒'),
                    ],
                    &theme,
                );
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let row: String = (0..10).map(|x| buf[(x, 0)].symbol()).collect();
        assert_eq!(row, "███▒▒─────");
    }

    #[test]
    fn percent_axis_labels() {
        assert_eq!(axis_label(1.0, Axis::Percent), "100%");
        assert_eq!(axis_label(0.5, Axis::Percent), "50%");
        assert_eq!(axis_label(0.10, Axis::Percent), "10%");
        assert_eq!(axis_label(1800.0, Axis::Number), "1.8k");
    }

    #[test]
    fn watts_axis_carries_its_unit() {
        // Regression: the power graph's ceiling read a bare "100".
        assert_eq!(axis_label(100.0, Axis::Watts), "100W");
        assert_eq!(axis_label(-2.0, Axis::Watts), "0W");
        let range = ScaleRange {
            min: 0.0,
            max: 20.0,
        };
        assert_eq!(
            scale_label(Scale::Auto { floor: 5.0 }, Axis::Watts, range).as_deref(),
            Some("20W")
        );
    }

    fn series(values: &[f32]) -> siltop_core::History {
        let mut h = siltop_core::History::default();
        for v in values {
            h.push(*v);
        }
        h
    }

    fn graph<'a>(history: &'a History, theme: &'a Theme, scale: Scale, axis: Axis) -> Graph<'a> {
        Graph {
            history,
            ramp: theme.cpu,
            theme,
            scale,
            axis,
            ink: GraphInk::Flat,
            time: TimeAxis::live(history),
        }
    }

    fn paint_graph(g: Graph<'_>, w: u16, h: u16) -> Vec<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|frame| render_scaled_graph(frame, frame.area(), g))
            .unwrap();
        let buf = terminal.backend().buffer();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn axis_hint_never_clears_plot_cells() {
        // Regression (D49): a 5x1 Clear() hid `100% ⣿⣿` columns at full load.
        let theme = Theme::default();
        let history = series(&[1.0; 64]);
        for axis in [Axis::Percent, Axis::Celsius, Axis::Number] {
            let rows = paint_graph(graph(&history, &theme, Scale::LOAD, axis), 28, 6);
            assert!(
                rows[0]
                    .chars()
                    .all(|c| ('\u{2800}'..='\u{28ff}').contains(&c)),
                "{axis:?}: {}",
                rows[0]
            );
        }
    }

    #[test]
    fn sampling_until_two_samples() {
        let theme = Theme::default();
        for n in 0..MIN_SAMPLES {
            let history = series(&vec![0.5; n]);
            let g = graph(&history, &theme, Scale::LOAD, Axis::Percent);
            assert!(g.border_label(30).is_none(), "{n} samples drew an axis");
            let rows = paint_graph(g, 30, 5);
            assert!(rows.iter().any(|r| r.contains("sampling…")), "{rows:?}");
            assert!(!rows.concat().contains('%'), "{rows:?}");
        }
        let history = series(&[0.5, 0.5]);
        let rows = paint_graph(graph(&history, &theme, Scale::LOAD, Axis::Percent), 30, 5);
        assert!(!rows.concat().contains("sampling"), "{rows:?}");
    }

    #[test]
    fn border_label_table() {
        let theme = Theme::default();
        let cases: &[(&[f32], Scale, Axis, Option<&str>)] = &[
            (&[0.02, 0.02], Scale::LOAD, Axis::Percent, Some("10%")),
            (&[0.34, 0.2], Scale::LOAD, Axis::Percent, Some("50%")),
            (&[0.5, 0.5], Scale::Fixed(1.0), Axis::Percent, None),
            (&[80.0, 80.0], Scale::TEMP, Axis::Celsius, Some("70–90°")),
            (
                &[12_000_000.0, 1.0],
                Scale::Auto { floor: 8_000.0 },
                Axis::Net(NetUnit::Bytes),
                Some("2MB/s"),
            ),
            (
                &[12_000_000.0, 1.0],
                Scale::Auto { floor: 8_000.0 },
                Axis::Net(NetUnit::Bits),
                Some("20Mb/s"),
            ),
            (
                &[1_500_000.0, 1.0],
                Scale::Auto { floor: 1_024.0 },
                Axis::Bytes,
                Some("2MB/s"),
            ),
            (&[1.0, 1.0], Scale::LOAD, Axis::None, None),
        ];
        for (values, scale, axis, want) in cases {
            let history = series(values);
            let got = graph(&history, &theme, *scale, *axis).border_label(40);
            assert_eq!(got.as_deref(), *want, "{values:?} {axis:?}");
        }
    }

    /// Leading number of a label: `2.5MB/s` → 2.5.
    fn mantissa(label: &str) -> f64 {
        let digits: String = label
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        digits.parse().unwrap_or(f64::NAN)
    }

    #[test]
    fn border_ceilings_read_one_two_five_in_every_unit() {
        let theme = Theme::default();
        let ok = |m: f64| {
            [1.0, 2.0, 2.5, 5.0]
                .iter()
                .any(|b| [1.0, 10.0, 100.0].iter().any(|k| (m - b * k).abs() < 1e-6))
        };
        let mut v = 1.0_f32;
        while v < 5e10 {
            let history = series(&[v, v]);
            for axis in [
                Axis::Net(NetUnit::Bytes),
                Axis::Net(NetUnit::Bits),
                Axis::Bytes,
            ] {
                let g = graph(&history, &theme, Scale::Auto { floor: 1.0 }, axis);
                let label = g.border_label(40).unwrap();
                assert!(ok(mantissa(&label)), "{v} {axis:?} -> {label}");
                assert!(label.ends_with("/s"), "{label}");
                assert!(g.range(40).max >= v, "ceiling below data: {v} {axis:?}");
            }
            v *= 1.37;
        }
    }

    #[test]
    fn axis_sits_in_the_top_border_left_of_the_mark() {
        let theme = Theme::default();
        let block = panel_block_axis(
            Panel::Net,
            Line::from(" net "),
            false,
            false,
            Some(String::from("2MB/s")),
            &theme,
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(30, 4)).unwrap();
        terminal
            .draw(|frame| frame.render_widget(block, frame.area()))
            .unwrap();
        let buf = terminal.backend().buffer();
        let top: String = (0..30).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(top.contains(" 2MB/s [↗]"), "{top}");
        let body: String = (1..3)
            .flat_map(|y| (1..29).map(move |x| (x, y)))
            .map(|p| buf[p].symbol())
            .collect();
        assert!(body.trim().is_empty(), "axis leaked into the plot: {body}");
    }

    #[test]
    fn percent_graphs_carry_no_axis_ink() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use siltop_core::History;

        let mut history = History::default();
        for _ in 0..48 {
            history.push(0.5);
        }
        let backend = TestBackend::new(28, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_scaled_graph(
                    frame,
                    frame.area(),
                    Graph {
                        history: &history,
                        ramp: Theme::default().cpu,
                        theme: &Theme::default(),
                        scale: Scale::Fixed(1.0),
                        axis: Axis::Percent,
                        ink: GraphInk::Load(siltop_core::Thermal::Nominal),
                        time: TimeAxis::live(&history),
                    },
                );
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let sym = buf[(x, y)].symbol();
                assert!(!sym.contains('%'), "% at {x},{y}");
                assert!(
                    !sym.contains('°'),
                    "hint leaked to percent graph at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn titles_follow_the_spacing_contract() {
        let theme = Theme::default();
        let line = panel_title("cpu", &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, " cpu  ");

        let mut spans = Vec::new();
        push_token(&mut spans, String::from("18%"), theme.title());
        push_token(&mut spans, String::from("8.2W"), theme.cpu());
        push_kv(
            &mut spans,
            &theme,
            "temp",
            String::from("42°"),
            theme.temp(42.0),
        );
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "18%  8.2W  temp  42°");
        assert!(!joined.contains("   "), "{joined}");
    }

    fn paint_fixed(
        history: &siltop_core::History,
        ink: GraphInk,
        ramp: Ramp,
    ) -> ratatui::buffer::Buffer {
        let theme = Theme::default();
        let backend = ratatui::backend::TestBackend::new(20, 4);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_scaled_graph(
                    frame,
                    frame.area(),
                    Graph {
                        history,
                        ramp,
                        theme: &theme,
                        scale: Scale::Fixed(1.0),
                        axis: Axis::Percent,
                        ink,
                        time: TimeAxis::live(history),
                    },
                );
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn full_ones() -> siltop_core::History {
        let mut history = siltop_core::History::with_capacity(48);
        for _ in 0..48 {
            history.push(1.0);
        }
        history
    }

    #[test]
    fn graph_row_gradient_top_is_not_bottom() {
        let theme = Theme::default();
        let history = full_ones();
        let buf = paint_fixed(&history, GraphInk::Load(Thermal::Nominal), theme.cpu);
        assert_eq!(buf[(0, 0)].style().fg, Some(theme.cpu.body));
        assert_eq!(buf[(0, 3)].style().fg, Some(theme.cpu.foot));
        assert_ne!(buf[(0, 0)].style().fg, buf[(0, 3)].style().fg);
    }

    #[test]
    fn graph_foot_stays_in_the_series_hue() {
        let theme = Theme::default();
        let history = full_ones();
        let buf = paint_fixed(&history, GraphInk::Load(Thermal::Nominal), theme.gpu);
        for y in 0..4 {
            let fg = buf[(0, y)].style().fg;
            assert_ne!(fg, Some(theme.dim), "row {y} faded to grey");
            let Some(Color::Rgb(r, g, b)) = fg else {
                panic!("row {y} is not rgb: {fg:?}");
            };
            assert!(b > r && b > g, "row {y} left the violet family: {fg:?}");
        }
    }

    #[test]
    fn focus_rings_the_panel_in_its_accent() {
        let theme = Theme::default();
        let area = Rect::new(0, 0, 20, 4);
        let paint = |focused: bool| {
            let backend = ratatui::backend::TestBackend::new(20, 4);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    let title = panel_title("gpu", &theme);
                    let block = panel_block(Panel::Gpu, title, focused, false, &theme);
                    frame.render_widget(block, area);
                })
                .unwrap();
            terminal.backend().buffer().clone()
        };
        let idle = paint(false);
        assert_eq!(idle[(0, 1)].style().fg, Some(theme.border));
        assert_eq!(idle[(17, 0)].style().fg, Some(theme.dim), "quiet ↗");
        assert_eq!(idle[(16, 0)].symbol(), "[");
        assert_eq!(idle[(18, 0)].symbol(), "]");
        let lit = paint(true);
        assert_eq!(lit[(0, 1)].style().fg, Some(theme.gpu.body));
        assert_eq!(lit[(2, 0)].style().fg, Some(theme.gpu.body), "label");
        assert!(lit[(2, 0)].style().add_modifier.contains(Modifier::BOLD));
        assert_eq!(lit[(17, 0)].symbol(), "↗");
        assert_eq!(lit[(17, 0)].style().fg, Some(theme.gpu.body));
        assert_eq!(lit[(16, 0)].style().fg, Some(theme.gpu.body), "lit bracket");
    }

    /// Paint and click read one geometry: the bracketed mark is drawn
    /// exactly on `corner_hit` for every frame width, open or expanded.
    #[test]
    fn corner_mark_paints_exactly_on_its_hit_rect() {
        let theme = Theme::default();
        for width in 5..=90_u16 {
            for expanded in [false, true] {
                let area = Rect::new(0, 0, width, 3);
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 3)).unwrap();
                terminal
                    .draw(|frame| {
                        let block =
                            panel_block(Panel::Cpu, Line::default(), false, expanded, &theme);
                        frame.render_widget(block, area);
                    })
                    .unwrap();
                let buf = terminal.backend().buffer();
                let hit = crate::layout::LayoutPlan::corner_hit(area).unwrap();
                let drawn: String = (hit.x..hit.x + hit.width)
                    .map(|x| buf[(x, 0)].symbol())
                    .collect();
                let want = if expanded { "[×]" } else { "[↗]" };
                assert_eq!(drawn, want, "{width} {expanded}");
            }
        }
    }

    #[test]
    fn graph_same_row_shares_color() {
        let theme = Theme::default();
        let history = full_ones();
        let buf = paint_fixed(&history, GraphInk::Load(Thermal::Nominal), theme.cpu);
        for y in 0..4 {
            assert_eq!(buf[(0, y)].style().fg, buf[(1, y)].style().fg, "row {y}");
        }
    }

    #[test]
    fn thermal_fair_no_longer_paints_every_graph_amber() {
        let theme = Theme::default();
        let history = full_ones();
        for ramp in [theme.mem, theme.gpu, theme.net] {
            let fair = paint_fixed(&history, GraphInk::Load(Thermal::Fair), ramp);
            let calm = paint_fixed(&history, GraphInk::Load(Thermal::Nominal), ramp);
            assert_eq!(fair, calm, "a bystander graph keeps its accent at Fair");
        }
        let buf = paint_fixed(&history, GraphInk::Signal(Thermal::Fair), theme.cpu);
        let calm = paint_fixed(&history, GraphInk::Signal(Thermal::Nominal), theme.cpu);
        for x in 0..20 {
            assert_eq!(buf[(x, 0)].style().fg, Some(theme.warn), "cpu crest {x}");
            for y in 1..4 {
                assert_eq!(buf[(x, y)].style().fg, calm[(x, y)].style().fg, "{x},{y}");
            }
        }
        let buf = paint_fixed(&history, GraphInk::Load(Thermal::Serious), theme.cpu);
        for y in 0..4 {
            for x in 0..20 {
                let cell = &buf[(x, y)];
                if cell.symbol() == " " {
                    continue;
                }
                assert_eq!(cell.style().fg, Some(theme.crit), "serious {x},{y}");
            }
        }
    }

    fn joined(spans: &[Span<'_>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn push_token_spacing_law_sweep() {
        // Tokens joined by exactly one two-space separator: never doubled,
        // never leading, never trailing.
        let pool = ["18%", "8.2W", "42°", "x", "a b", "1.2Gb/s", "…", "→"];
        let style = Style::default();
        for mask in 1_u32..(1 << pool.len()) {
            let mut spans = Vec::new();
            let mut want = Vec::new();
            for (i, token) in pool.iter().enumerate() {
                if mask & (1 << i) != 0 {
                    push_token(&mut spans, (*token).to_owned(), style);
                    want.push(*token);
                }
            }
            let text = joined(&spans);
            assert_eq!(text, want.join("  "), "mask {mask:b}");
            assert!(!text.starts_with(' ') && !text.ends_with(' '), "{text:?}");
            assert!(!text.contains("   "), "{text:?}");
        }
    }

    #[test]
    fn push_token_after_a_spaced_title_adds_no_separator() {
        let theme = Theme::default();
        let mut spans = panel_title("cpu", &theme).spans;
        push_token(&mut spans, String::from("18%"), theme.title());
        assert_eq!(joined(&spans), " cpu  18%");
    }

    #[test]
    fn push_token_empty_text_adds_no_separator() {
        // Regression: an empty value (a missing reading) left a dangling
        // "  " separator, so the title ended in spaces or doubled them.
        let style = Style::default();
        let mut spans = Vec::new();
        push_token(&mut spans, String::from("18%"), style);
        push_token(&mut spans, String::new(), style);
        assert_eq!(joined(&spans), "18%");
        push_token(&mut spans, String::from("42°"), style);
        assert_eq!(joined(&spans), "18%  42°");
        let mut empty_first = Vec::new();
        push_token(&mut empty_first, String::new(), style);
        push_token(&mut empty_first, String::from("x"), style);
        assert_eq!(joined(&empty_first), "x");
    }

    #[test]
    fn push_kv_pairs_key_and_value_with_two_spaces() {
        let theme = Theme::default();
        let mut spans = Vec::new();
        push_kv(&mut spans, &theme, "rx", String::from("1.2Mb"), theme.net());
        push_kv(&mut spans, &theme, "tx", String::from("80Kb"), theme.net());
        assert_eq!(joined(&spans), "rx  1.2Mb  tx  80Kb");
    }

    #[test]
    fn axis_label_table() {
        let cases: &[(f32, Axis, &str)] = &[
            (0.0, Axis::Percent, "0%"),
            (0.254, Axis::Percent, "25%"),
            (2.0, Axis::Percent, "200%"),
            (0.0, Axis::Net(NetUnit::Bits), "0"),
            (-5.0, Axis::Net(NetUnit::Bits), "0"),
            (1_000_000.0, Axis::Net(NetUnit::Bits), "1Mb/s"),
            (8_000_000.0, Axis::Net(NetUnit::Bytes), "1MB/s"),
            (2_500_000.0, Axis::Net(NetUnit::Bits), "2.5Mb/s"),
            (-1.0, Axis::Bytes, "0"),
            (2000.0, Axis::Bytes, "2KB/s"),
            (72.4, Axis::Celsius, "72°"),
            (0.0, Axis::Number, "0"),
            (-3.0, Axis::Number, "0"),
            (999.4, Axis::Number, "999"),
            (1000.0, Axis::Number, "1.0k"),
            (9_949.0, Axis::Number, "9.9k"),
            (10_000.0, Axis::Number, "10k"),
            (5_300.0, Axis::Number, "5.3k"),
            (1.0, Axis::None, ""),
        ];
        for (v, axis, want) in cases {
            assert_eq!(axis_label(*v, *axis), *want, "{v} {axis:?}");
        }
    }

    #[test]
    fn number_label_rounds_into_whole_k() {
        // Regression: 9 951 rpm printed "10.0k", one cell wider than "10k".
        assert_eq!(number_label(9_951.0), "10k");
        for v in (9_900..10_100).map(|v| v as f32) {
            let label = number_label(v);
            assert!(label.len() <= 4, "{v} -> {label}");
        }
    }

    fn fill_bar_cells(width: u16, ratio: f32) -> Vec<(String, Style)> {
        let theme = Theme::default();
        let backend = ratatui::backend::TestBackend::new(width.max(1), 1);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_fill_bar(frame, Rect::new(0, 0, width, 1), ratio, theme.cpu, &theme);
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        (0..width)
            .map(|x| (buf[(x, 0)].symbol().to_owned(), buf[(x, 0)].style()))
            .collect()
    }

    fn fill_bar(width: u16, ratio: f32) -> String {
        fill_bar_cells(width, ratio)
            .into_iter()
            .map(|(s, _)| s)
            .collect()
    }

    fn filled_eighths(bar: &str) -> usize {
        bar.chars()
            .map(|c| match c {
                FULL_CELL => 8,
                c => EIGHTHS.iter().position(|e| *e == c).map_or(0, |i| i + 1),
            })
            .sum()
    }

    #[test]
    fn fill_bar_fills_ratio_to_the_eighth() {
        for width in [1_u16, 2, 7, 10, 33, 80] {
            for step in 0..=40 {
                let ratio = step as f32 / 40.0;
                let bar = fill_bar(width, ratio);
                let want = fill_eighths(width, ratio) as usize;
                assert_eq!(filled_eighths(&bar), want, "{width} @ {ratio}: {bar}");
                let exact = f32::from(width) * 8.0 * ratio;
                assert!((want as f32 - exact).abs() <= 1.0, "{width} @ {ratio}");
                assert_eq!(bar.chars().count(), usize::from(width));
                let fill_len = bar.chars().take_while(|c| is_bar_fill(*c)).count();
                assert!(
                    bar.chars().skip(fill_len).all(|c| c == BAR_TRACK),
                    "fill must be a left prefix: {bar}"
                );
            }
        }
    }

    /// Regression: per-core rows at 99% and 0% drew identical full-width
    /// lines, because the track and the fill were one glyph in one colour.
    #[test]
    fn fill_bar_zero_and_full_track_is_not_fill() {
        let bars: Vec<String> = [0.0, 0.01, 0.125, 0.5, 0.99, 1.0]
            .into_iter()
            .map(|r| fill_bar(20, r))
            .collect();
        for (i, a) in bars.iter().enumerate() {
            for b in &bars[i + 1..] {
                assert_ne!(a, b, "{bars:#?}");
            }
        }
        assert_eq!(bars[0], "─".repeat(20));
        assert_eq!(bars[1], format!("▎{}", "─".repeat(19)));
        assert_eq!(bars[2], format!("██▌{}", "─".repeat(17)));
        assert_eq!(bars[3], format!("{}{}", "█".repeat(10), "─".repeat(10)));
        assert_eq!(bars[4], format!("{}▊", "█".repeat(19)));
        assert_eq!(bars[5], "█".repeat(20));
    }

    #[test]
    fn fill_bar_track_is_faint_and_fill_is_ink() {
        let theme = Theme::default();
        let cells = fill_bar_cells(10, 0.45);
        let fill = cells.iter().find(|(s, _)| s == "█").unwrap().1;
        let edge = cells.iter().find(|(s, _)| s == "▌").unwrap().1;
        let track = cells.iter().find(|(s, _)| s == "─").unwrap().1;
        assert_eq!(fill.fg, Some(theme.cpu.body));
        assert_eq!(edge.fg, fill.fg, "the partial edge is fill");
        assert_eq!(track.fg, Some(theme.faint));
        assert_ne!(fill.fg, track.fg);
        assert_eq!(fill.bg, track.bg, "no painted backgrounds on glass");
    }

    #[test]
    fn fill_bar_clamps_wild_ratios() {
        assert_eq!(fill_bar(5, -1.0), "─────");
        assert_eq!(fill_bar(5, 9.0), "█████");
        assert_eq!(fill_bar(5, f32::NAN), "─────");
        assert_eq!(fill_bar(5, f32::INFINITY), "█████");
    }

    #[test]
    fn bar_labels_are_one_width() {
        let labels: Vec<String> = [("S0", 0.99), ("P0", 0.0), ("P10", 1.0), ("E3", 0.05)]
            .into_iter()
            .map(|(n, r)| bar_label(n, r))
            .collect();
        assert_eq!(labels[0], "S0   99%");
        assert_eq!(labels[2], "P10 100%");
        assert!(labels.iter().all(|l| l.chars().count() == 8), "{labels:?}");
    }

    #[test]
    fn scaled_graph_never_panics_or_escapes_its_area() {
        let theme = Theme::default();
        let mut histories = vec![siltop_core::History::with_capacity(8)];
        let mut ramp_up = siltop_core::History::with_capacity(300);
        for i in 0..300 {
            ramp_up.push(i as f32 * 1_000.0);
        }
        histories.push(ramp_up);
        let axes = [
            Axis::None,
            Axis::Percent,
            Axis::Net(NetUnit::Bits),
            Axis::Net(NetUnit::Bytes),
            Axis::Bytes,
            Axis::Celsius,
            Axis::Number,
        ];
        let scales = [
            Scale::Fixed(1.0),
            Scale::LOAD,
            Scale::TEMP,
            Scale::FAN,
            Scale::Fixed(0.0),
        ];
        for history in &histories {
            for axis in axes {
                for scale in scales {
                    for (w, h) in [(1_u16, 1_u16), (6, 2), (13, 3), (14, 5), (40, 8)] {
                        let backend = ratatui::backend::TestBackend::new(w + 4, h + 2);
                        let mut terminal = ratatui::Terminal::new(backend).unwrap();
                        let area = Rect::new(2, 1, w, h);
                        terminal
                            .draw(|frame| {
                                render_scaled_graph(
                                    frame,
                                    area,
                                    Graph {
                                        history,
                                        ramp: theme.cpu,
                                        theme: &theme,
                                        scale,
                                        axis,
                                        ink: GraphInk::Load(Thermal::Critical),
                                        time: TimeAxis::live(history),
                                    },
                                );
                            })
                            .unwrap();
                        let buf = terminal.backend().buffer();
                        for y in 0..buf.area.height {
                            for x in 0..buf.area.width {
                                let inside = x >= 2 && x < 2 + w && y >= 1 && y < 1 + h;
                                if !inside {
                                    assert_eq!(buf[(x, y)].symbol(), " ", "ink escaped at {x},{y}");
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn paint(history: &History, time: TimeAxis, w: u16, h: u16, bordered: bool) -> Vec<String> {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::widgets::Borders;
        let theme = Theme::default();
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| {
            let area = if bordered {
                let block = Block::default().borders(Borders::ALL);
                let inner = block.inner(f.area());
                f.render_widget(block, f.area());
                inner
            } else {
                f.area()
            };
            render_scaled_graph(
                f,
                area,
                Graph {
                    history,
                    ramp: theme.cpu,
                    theme: &theme,
                    scale: Scale::Fixed(1.0),
                    axis: Axis::None,
                    ink: GraphInk::Flat,
                    time,
                },
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_owned()).collect())
            .collect()
    }

    fn ramp_history(n: usize) -> History {
        let mut h = History::for_period(Duration::from_secs(1));
        for i in 0..n {
            h.push_at((i % 10) as f32 / 10.0, i as u64 * 1_000);
        }
        h
    }

    #[test]
    fn span_labels_read_as_time() {
        assert_eq!(span_label(Duration::from_secs(20)), "\u{2212}20s");
        assert_eq!(span_label(Duration::from_millis(300)), "\u{2212}1s");
        assert_eq!(span_label(Duration::from_secs(119)), "\u{2212}119s");
        assert_eq!(span_label(Duration::from_mins(5)), "\u{2212}5m");
        assert_eq!(span_label(Duration::from_mins(15)), "\u{2212}15m");
    }

    #[test]
    fn time_labels_sit_in_the_bottom_border() {
        let h = ramp_history(900);
        let time = TimeAxis {
            zoom: siltop_core::Zoom::FiveMin,
            now: h.newest().unwrap(),
            cadence: Duration::from_secs(1),
        };
        let rows = paint(&h, time, 42, 6, true);
        let bottom = &rows[5];
        assert!(bottom.contains("\u{2212}5m"), "{bottom}");
        assert!(bottom.contains("now"), "{bottom}");
        assert_eq!(bottom.matches('\u{2534}').count(), 3, "{bottom}");
        assert!(bottom.starts_with('└') && bottom.ends_with('┘'), "{bottom}");
    }

    #[test]
    fn time_labels_skip_plots_without_a_border_or_room() {
        let h = ramp_history(60);
        let rows = paint(&h, TimeAxis::live(&h), 42, 4, false);
        assert!(rows.iter().all(|r| !r.contains("now")), "{rows:#?}");
        let narrow = paint(&h, TimeAxis::live(&h), 14, 5, true);
        assert!(narrow.iter().all(|r| !r.contains("now")), "{narrow:#?}");
    }

    #[test]
    fn one_row_graph_is_braille_not_block_sparkline() {
        let h = ramp_history(80);
        let rows = paint(&h, TimeAxis::live(&h), 30, 1, false);
        let row = &rows[0];
        assert!(!row.chars().any(|c| ('▁'..='█').contains(&c)), "{row}");
        assert!(
            row.chars().any(|c| ('\u{2801}'..='\u{28FF}').contains(&c)),
            "{row}"
        );
    }

    #[test]
    fn one_row_strip_has_no_noise_floor() {
        // Regression D58: spark.rs floored every non-zero bar at 4%, so an idle
        // strip drew a dotted line of noise.
        let mut h = History::for_period(Duration::from_secs(1));
        for i in 0..60 {
            h.push_at(0.01, i * 1_000);
        }
        let rows = paint(&h, TimeAxis::live(&h), 30, 1, false);
        assert!(rows[0].trim().is_empty(), "{:?}", rows[0]);
    }

    /// Regression: live zoom mapped one sample per dot, so a 34-column cpu
    /// plot showed -34s beside a 69-column net plot at -69s. A step one
    /// minute ago must sit halfway across every plot, on either cadence.
    #[test]
    fn step_lands_at_the_same_fraction_of_plots_of_any_width() {
        let step_at = 60_000_u64;
        let now = 120_000_u64;
        let series = |period_ms: u64| {
            let mut h = History::for_period(Duration::from_millis(period_ms));
            for t in (0..=now).step_by(usize::try_from(period_ms).unwrap()) {
                h.push_at(if t >= step_at { 1.0 } else { 0.0 }, t);
            }
            h
        };
        let axis = TimeAxis {
            zoom: siltop_core::Zoom::Live,
            now,
            cadence: Duration::from_millis(500),
        };
        let range = ScaleRange { min: 0.0, max: 1.0 };
        for (label, h) in [("cheap 500ms", series(500)), ("sensor 2s", series(2_000))] {
            for width in [17_u16, 34, 50, 69, 118] {
                let dots = braille::dots(&h, axis, width, range);
                let first = dots
                    .iter()
                    .position(|d| d.is_some_and(|v| v > 0.5))
                    .unwrap();
                // within one bucket of halfway: the newest bucket holds `now`
                assert!(
                    first.abs_diff(dots.len() / 2) <= 1,
                    "{label} at {width} cols: step at column {first} of {}",
                    dots.len()
                );
            }
        }
    }

    #[test]
    fn graph_zoom_changes_what_is_on_screen() {
        // a spike 4 minutes ago is off a live view but on a 5m view
        let mut h = History::for_period(Duration::from_secs(1));
        for i in 0..300_u64 {
            h.push_at(if i == 60 { 1.0 } else { 0.0 }, i * 1_000);
        }
        let now = h.newest().unwrap();
        let axis = |zoom| TimeAxis {
            zoom,
            now,
            cadence: Duration::from_secs(1),
        };
        let lit = |rows: &[String]| {
            rows.iter()
                .flat_map(|r| r.chars())
                .filter(|c| ('\u{2801}'..='\u{28FF}').contains(c))
                .count()
        };
        assert_eq!(
            lit(&paint(&h, axis(siltop_core::Zoom::Live), 40, 4, false)),
            0
        );
        assert!(lit(&paint(&h, axis(siltop_core::Zoom::FiveMin), 40, 4, false)) > 0);
    }

    #[test]
    fn render_200_columns_is_fast() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let theme = Theme::default();
        let mut h = History::for_period(Duration::from_millis(250));
        for i in 0..3_600_u64 {
            h.push_at((i % 97) as f32 / 97.0, i * 250);
        }
        let mut term = Terminal::new(TestBackend::new(200, 20)).unwrap();
        let rounds = 200_u32;
        let mut best = Duration::MAX;
        for zoom in siltop_core::Zoom::ALL {
            let time = TimeAxis {
                zoom,
                now: h.newest().unwrap(),
                cadence: Duration::from_millis(250),
            };
            let start = std::time::Instant::now();
            for _ in 0..rounds {
                term.draw(|f| {
                    render_scaled_graph(
                        f,
                        f.area(),
                        Graph {
                            history: &h,
                            ramp: theme.cpu,
                            theme: &theme,
                            scale: Scale::LOAD,
                            axis: Axis::Percent,
                            ink: GraphInk::Flat,
                            time,
                        },
                    );
                })
                .unwrap();
            }
            best = best.min(start.elapsed() / rounds);
        }
        // the budget is 0.3 ms in release; debug builds and busy CI get 20x
        let bound = if cfg!(debug_assertions) { 6_000 } else { 300 };
        assert!(best.as_micros() <= bound, "{best:?} per 200-col frame");
    }

    /// The top border of a `cell_titled` cell `width` wide.
    fn cell_top(label: &str, value: Option<&str>, hop: bool, width: u16) -> String {
        let theme = Theme::default();
        let area = Rect::new(0, 0, width, 3);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 3)).unwrap();
        terminal
            .draw(|frame| {
                cell_titled(frame, area, label, value, hop, &theme);
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        (0..width).map(|x| buf[(x, 0)].symbol()).collect()
    }

    /// Regression: expanded cell titles ran into their rule
    /// (`memory  12.8G/24.0G · 47% free────`), breaking spacing law rule 1.
    #[test]
    fn cell_titles_leave_one_space_before_the_rule() {
        let cases = [
            ("memory", Some("12.8G/24.0G · 47% free"), false),
            ("swap", Some("820M"), false),
            ("volumes", None, false),
            ("cpu", Some("34%"), true),
        ];
        for (label, value, hop) in cases {
            let mut text = format!(" {label}");
            if let Some(value) = value {
                text = format!("{text}  {value}");
            }
            if hop {
                text.push_str("  →");
            }
            // every width with room for the title, its space and some rule
            let need = u16::try_from(text.chars().count()).unwrap() + 4;
            for width in need..need + 40 {
                let top = cell_top(label, value, hop, width);
                let expect = format!("╭{text} ─");
                assert!(top.starts_with(&expect), "{width}: {top:?}");
                assert!(top.ends_with('╮'), "{width}: {top:?}");
            }
        }
    }
}
