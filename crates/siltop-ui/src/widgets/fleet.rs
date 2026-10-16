//! `siltop fleet`: one card per Mac in a responsive grid, and the top
//! processes of the card Enter opened.
//!
//! The binary maps each host's latest reading into a [`FleetCard`]; this
//! module only lays out and paints. Hardware a host lacks (a GPU reading, a
//! battery, power rails) drops its line rather than printing a zero.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};
use siltop_core::{Pressure, Thermal, bytes_short, percent_display, watts_display};

use crate::braille::{Grow, render_dots};
use crate::chips::{self, Chip};
use crate::chrome::{paint_rows, push_kv, push_token};
use crate::theme::Theme;
use crate::titles::ghz_display;

/// Narrowest card that still fits `cpu 100%  P 4.5G · E 2.9G`.
pub(crate) const CARD_MIN_WIDTH: u16 = 34;
/// A card squeezed below this shows nothing useful and is not laid out.
const CARD_MIN_HEIGHT: u16 = 4;

/// Where a host's stream stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardLink {
    /// Readings arriving on time.
    Live,
    /// Started, nothing parsed yet.
    Connecting,
    /// Had readings, but none lately, or the link dropped: shown dim.
    Reconnecting,
}

/// One host's card, borrowed from the binary's per-host state.
#[derive(Debug, Clone)]
pub struct FleetCard<'a> {
    pub host: &'a str,
    pub link: CardLink,
    pub chip: Option<&'a str>,
    /// Share of time busy, 0..=1.
    pub cpu: Option<f32>,
    /// Each cluster kind's clock in MHz, fastest kind first: `("P", 3100)`.
    pub clocks: Vec<(&'a str, u32)>,
    pub gpu: Option<f32>,
    /// `(used, total)` bytes.
    pub mem: Option<(u64, u64)>,
    pub pressure: Option<Pressure>,
    pub power_w: Option<f32>,
    pub peak_c: Option<f32>,
    /// `(percent, charging)`; `None` on desktops.
    pub battery: Option<(f32, bool)>,
    pub verdict: Option<&'a str>,
    /// CPU share history, oldest first, 0..=1.
    pub cpu_history: &'a [f32],
}

/// One row of a host's process list.
#[derive(Debug, Clone, Copy)]
pub struct FleetProc<'a> {
    pub pid: u32,
    pub name: &'a str,
    pub cpu_pct: Option<f32>,
    pub mem_bytes: Option<u64>,
}

/// Card rects for `n` hosts in `area`: as many columns as fit
/// `CARD_MIN_WIDTH`, and rows that share the whole height, so a few hosts
/// get tall cards with tall CPU graphs. Rows never shrink below
/// `CARD_MIN_HEIGHT`; hosts that do not fit then are left out, so the
/// result may be shorter than `n`. Rects never overlap or leave `area`.
#[must_use]
pub fn fleet_grid(area: Rect, n: usize) -> Vec<Rect> {
    if n == 0 || area.width == 0 || area.height < CARD_MIN_HEIGHT {
        return Vec::new();
    }
    let fit_cols = usize::from((area.width / CARD_MIN_WIDTH).max(1));
    let cols = fit_cols.min(n);
    let rows_wanted = n.div_ceil(cols);
    let row_h = (area.height / u16::try_from(rows_wanted).unwrap_or(u16::MAX))
        .max(CARD_MIN_HEIGHT.min(area.height));
    let rows_fit = usize::from(area.height / row_h.max(1));
    let rows = rows_wanted.min(rows_fit).max(1);
    let shown = n.min(rows * cols);
    let (xs, widths) = spread(area.x, area.width, cols);
    let (ys, heights) = spread(area.y, area.height, rows);
    (0..shown)
        .map(|i| {
            let (row, col) = (i / cols, i % cols);
            Rect::new(xs[col], ys[row], widths[col], heights[row])
        })
        .collect()
}

/// Splits `len` cells from `start` into `parts` runs that cover it exactly,
/// the first `len % parts` one cell longer. Returns the starts and lengths.
fn spread(start: u16, len: u16, parts: usize) -> (Vec<u16>, Vec<u16>) {
    let parts = u16::try_from(parts.max(1)).unwrap_or(u16::MAX);
    let base = len / parts;
    let extra = len % parts;
    (0..parts)
        .map(|i| (start + i * base + i.min(extra), base + u16::from(i < extra)))
        .unzip()
}

/// The grid's share of the fleet screen: all of it but the footer row.
fn grid_area(area: Rect) -> Rect {
    Rect {
        height: area.height.saturating_sub(1),
        ..area
    }
}

/// Index of the card at column `x`, row `y` of the whole fleet screen
/// `area`, for mouse clicks.
#[must_use]
pub fn fleet_hit(area: Rect, n: usize, x: u16, y: u16) -> Option<usize> {
    fleet_grid(grid_area(area), n)
        .iter()
        .position(|r| r.contains(ratatui::layout::Position { x, y }))
}

/// The key chips on the bottom row, plus an optional note on its right.
fn render_footer(frame: &mut Frame, area: Rect, chips: &[Chip], note: Option<&str>, theme: &Theme) {
    if area.height == 0 {
        return;
    }
    let row = Rect::new(area.x, area.bottom() - 1, area.width, 1);
    let placed = chips::place(chips, row, row.x);
    frame.render_widget(chips::line(&placed, row.x, theme), row);
    if let Some(note) = note {
        let note = Line::from(Span::styled(format!("{note} "), theme.dim())).right_aligned();
        frame.render_widget(note, row);
    }
}

/// Every card, `selected` ringed in the highlight ink.
pub fn render_fleet(
    frame: &mut Frame,
    area: Rect,
    cards: &[FleetCard<'_>],
    selected: usize,
    theme: &Theme,
) {
    let rects = fleet_grid(grid_area(area), cards.len());
    for (i, (card, rect)) in cards.iter().zip(&rects).enumerate() {
        render_card(frame, *rect, card, i == selected, theme);
    }
    let hidden = cards.len().saturating_sub(rects.len());
    let note = (hidden > 0).then(|| format!("+{hidden} more: widen the terminal"));
    render_footer(frame, area, &chips::FLEET_GRID, note.as_deref(), theme);
    // Host, chip and process names come from other Macs: scrub last.
    crate::chrome::scrub_controls(frame.buffer_mut());
    theme.finish(frame.buffer_mut());
}

fn card_block<'a>(card: &FleetCard<'_>, selected: bool, theme: &Theme) -> Block<'a> {
    let live = card.link == CardLink::Live;
    let name_style = match (selected, live) {
        (true, _) => theme.title().add_modifier(Modifier::BOLD),
        (false, true) => theme.title(),
        (false, false) => theme.faint(),
    };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(if selected {
            theme.border(true)
        } else {
            theme.border(false)
        })
        .title(Line::from(Span::styled(
            format!(" {} ", card.host),
            name_style,
        )))
}

/// The text rows of one card, before dimming.
fn card_lines(card: &FleetCard<'_>, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(6);
    if let Some(chip) = card.chip {
        lines.push(Line::from(Span::styled(chip.to_owned(), theme.dim())));
    }
    if let Some(cpu) = card.cpu {
        let mut spans = Vec::new();
        push_kv(&mut spans, theme, "cpu", percent_display(cpu), theme.cpu());
        let clocks: Vec<String> = card
            .clocks
            .iter()
            .map(|(tag, mhz)| format!("{tag} {}", ghz_display(*mhz)))
            .collect();
        if !clocks.is_empty() {
            push_token(&mut spans, clocks.join(" · "), theme.dim());
        }
        lines.push(Line::from(spans));
    }
    if let Some(gpu) = card.gpu {
        let mut spans = Vec::new();
        push_kv(&mut spans, theme, "gpu", percent_display(gpu), theme.gpu());
        lines.push(Line::from(spans));
    }
    if let Some((used, total)) = card.mem {
        let mut spans = Vec::new();
        let value = format!("{}/{}", bytes_short(used), bytes_short(total));
        push_kv(&mut spans, theme, "mem", value, theme.mem());
        if let Some(pressure) = card.pressure.filter(|p| *p != Pressure::Nominal) {
            let word = match pressure {
                Pressure::Critical => "pressure critical",
                _ => "pressure warn",
            };
            push_token(&mut spans, word.to_owned(), theme.pressure(pressure));
        }
        lines.push(Line::from(spans));
    }
    let mut spans = Vec::new();
    if let Some(watts) = card.power_w {
        push_token(&mut spans, watts_display(watts), theme.fg());
    }
    if let Some(celsius) = card.peak_c {
        push_token(
            &mut spans,
            siltop_core::celsius(celsius),
            theme.temp(celsius),
        );
    }
    if let Some((percent, charging)) = card.battery {
        let mark = if charging { "+" } else { "" };
        push_token(&mut spans, format!("bat {percent:.0}%{mark}"), theme.dim());
    }
    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }
    let status = match card.link {
        CardLink::Live => card.verdict.map(str::to_owned),
        CardLink::Connecting => Some(String::from("connecting…")),
        CardLink::Reconnecting => Some(String::from("reconnecting…")),
    };
    if let Some(status) = status {
        lines.push(Line::from(Span::styled(status, theme.muted())));
    }
    lines
}

/// Everything in a stale card turns faint; the frame and name already are.
fn dim_line(line: Line<'static>, theme: &Theme) -> Line<'static> {
    let faint = theme.faint();
    Line::from(
        line.spans
            .into_iter()
            .map(|span| Span::styled(span.content, faint))
            .collect::<Vec<_>>(),
    )
}

fn render_card(frame: &mut Frame, area: Rect, card: &FleetCard<'_>, selected: bool, theme: &Theme) {
    let block = card_block(card, selected, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let live = card.link == CardLink::Live;
    let mut lines = card_lines(card, theme);
    if !live {
        lines = lines.into_iter().map(|l| dim_line(l, theme)).collect();
    }
    let text_rows = inner.height.saturating_sub(u16::from(inner.height > 1));
    lines.truncate(usize::from(text_rows));
    let used = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    frame.render_widget(Paragraph::new(lines), inner);
    // The CPU graph takes every row the text leaves, at least one.
    if inner.height > 1 && !card.cpu_history.is_empty() {
        let rows = inner.height.saturating_sub(used).max(1);
        let graph = Rect::new(inner.x, inner.bottom() - rows, inner.width, rows);
        render_history(frame, graph, card.cpu_history, live, theme);
    }
}

/// Lowest top of the graph's scale, so an idle Mac still reads as idle.
const STRIP_FLOOR: f32 = 0.25;

/// The newest values as a braille graph filling `area`, two per cell,
/// scaled to the busiest sample shown (never below [`STRIP_FLOOR`]) so
/// light loads show; inked like the dashboard's CPU graph, faint when stale.
fn render_history(frame: &mut Frame, area: Rect, values: &[f32], live: bool, theme: &Theme) {
    let shown = &values[values.len().saturating_sub(usize::from(area.width) * 2)..];
    let top = shown
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .fold(STRIP_FLOOR, f32::max)
        .min(1.0);
    let dots: Vec<Option<f32>> = shown
        .iter()
        .map(|v| v.is_finite().then(|| (v / top).clamp(0.0, 1.0)))
        .collect();
    let rows = render_dots(&dots, area.width, area.height, Grow::Up);
    let n = rows.len();
    let faint = theme.faint().fg.unwrap_or_default();
    paint_rows(frame.buffer_mut(), area, &rows, |i| {
        if live {
            theme.graph_ink(&theme.cpu, i, n, Thermal::Nominal, false)
        } else {
            faint
        }
    });
}

/// The top processes of one host, full screen; Esc returns to the grid.
pub fn render_fleet_procs(
    frame: &mut Frame,
    area: Rect,
    card: &FleetCard<'_>,
    procs: &[FleetProc<'_>],
    theme: &Theme,
) {
    let block = card_block(card, true, theme);
    let pane = grid_area(area);
    let inner = block.inner(pane);
    frame.render_widget(block, pane);
    let mut lines = vec![Line::from(Span::styled(
        format!("{:>7}  {:>6}  {:>7}  name", "pid", "cpu", "mem"),
        theme.dim(),
    ))];
    if procs.is_empty() {
        lines.push(Line::from(Span::styled(
            "no processes reported yet",
            theme.muted(),
        )));
    }
    for proc in procs {
        let cpu = proc
            .cpu_pct
            .map_or_else(|| String::from("—"), |p| format!("{p:.1}%"));
        let mem = proc
            .mem_bytes
            .map_or_else(|| String::from("—"), bytes_short);
        lines.push(Line::from(vec![
            Span::styled(format!("{:>7}  ", proc.pid), theme.dim()),
            Span::styled(format!("{cpu:>6}  "), theme.cpu()),
            Span::styled(format!("{mem:>7}  "), theme.mem()),
            Span::styled(proc.name.to_owned(), theme.fg()),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), inner);
    render_footer(frame, area, &chips::FLEET_PROCS, None, theme);
    // Host, chip and process names come from other Macs: scrub last.
    crate::chrome::scrub_controls(frame.buffer_mut());
    theme.finish(frame.buffer_mut());
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    const HISTORY: [f32; 8] = [0.1, 0.2, 0.4, 0.8, 1.0, 0.6, 0.3, 0.1];

    fn card(host: &str) -> FleetCard<'_> {
        FleetCard {
            host,
            link: CardLink::Live,
            chip: Some("Apple M4 Pro"),
            cpu: Some(0.42),
            clocks: vec![("P", 3900), ("E", 2600)],
            gpu: Some(0.1),
            mem: Some((12 << 30, 24 << 30)),
            pressure: Some(Pressure::Nominal),
            power_w: Some(8.5),
            peak_c: Some(61.0),
            battery: Some((87.0, true)),
            verdict: Some("All normal"),
            cpu_history: &HISTORY,
        }
    }

    fn draw(width: u16, height: u16, paint: impl FnOnce(&mut Frame)) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(paint).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Last line of defence: text another Mac sent that was not cleaned
    /// where it entered still never reaches the terminal as a control.
    #[test]
    fn remote_escapes_never_reach_the_terminal() {
        let mut evil = card("mini\u{1b}]2;pwn\u{7}");
        evil.chip = Some("Apple\u{9b}31m M4");
        evil.verdict = Some("\u{202e}lamron llA");
        let procs = [FleetProc {
            pid: 7,
            name: "x\u{1b}[2Jy",
            cpu_pct: Some(1.0),
            mem_bytes: None,
        }];
        let theme = Theme::default();
        let grid = draw(80, 12, |f| {
            render_fleet(f, f.area(), std::slice::from_ref(&evil), 0, &theme);
        });
        let list = draw(80, 12, |f| {
            render_fleet_procs(f, f.area(), &evil, &procs, &theme);
        });
        for text in [grid, list] {
            assert!(
                text.lines()
                    .all(|row| !row.chars().any(siltop_core::is_unsafe)),
                "{text}"
            );
        }
    }

    #[test]
    fn grid_layout_sweep_never_overlaps_or_leaves_area() {
        for width in [1u16, 20, 33, 34, 60, 80, 120, 200, 301] {
            for height in [0u16, 3, 4, 9, 20, 40, 60] {
                for n in 0..=12usize {
                    let area = Rect::new(2, 1, width, height);
                    let rects = fleet_grid(area, n);
                    assert!(rects.len() <= n, "{width}x{height} n={n}");
                    for (i, r) in rects.iter().enumerate() {
                        assert!(area.contains(r.as_position()), "{width}x{height} n={n}");
                        assert!(r.right() <= area.right() && r.bottom() <= area.bottom());
                        assert!(r.height >= CARD_MIN_HEIGHT.min(area.height));
                        for other in &rects[i + 1..] {
                            assert!(!r.intersects(*other), "{width}x{height} n={n}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn grid_fills_width_exactly_across_columns() {
        let area = Rect::new(0, 0, 104, 40);
        let rects = fleet_grid(area, 3);
        assert_eq!(rects.len(), 3);
        assert_eq!(rects.iter().map(|r| r.width).sum::<u16>(), 104);
        assert!(rects.iter().all(|r| r.y == 0 && r.height == 40));
    }

    #[test]
    fn grid_wraps_to_rows_when_narrow() {
        let rects = fleet_grid(Rect::new(0, 0, 70, 40), 5);
        assert_eq!(rects.len(), 5);
        // Three rows share 40 rows exactly, the first one row taller.
        assert_eq!(rects[2].y, 14);
        assert_eq!(rects[4].y, 27);
        assert_eq!(rects[4].bottom(), 40);
    }

    /// Regression (fleet layout): at 160x45 four hosts get cards the full
    /// height of the screen (no empty rows below), a CPU graph many rows
    /// tall, and a footer that says which keys work.
    #[test]
    fn fleet_layout_cards_fill_the_screen_with_tall_graphs_and_key_hints() {
        let theme = Theme::default();
        let busy: Vec<f32> = (0..160).map(|i| (i % 20) as f32 / 20.0).collect();
        let cards: Vec<_> = ["here", "studio", "mini", "air"]
            .into_iter()
            .map(|host| FleetCard {
                cpu_history: &busy,
                ..card(host)
            })
            .collect();
        let text = draw(160, 45, |f| render_fleet(f, f.area(), &cards, 0, &theme));
        let rows: Vec<&str> = text.lines().collect();
        assert!(rows[43].starts_with('╰'), "cards reach the footer:\n{text}");
        let graph_rows = rows
            .iter()
            .filter(|row| {
                row.chars()
                    .take(40)
                    .any(|c| ('\u{2801}'..='\u{28ff}').contains(&c))
            })
            .count();
        assert!(graph_rows >= 30, "{graph_rows} graph rows:\n{text}");
        let footer = rows[44];
        assert!(footer.contains("enter processes"), "{footer}");
        assert!(footer.contains("q quit"), "{footer}");
        let procs = draw(80, 20, |f| {
            render_fleet_procs(f, f.area(), &cards[1], &[], &theme);
        });
        let footer = procs.lines().last().unwrap();
        assert!(
            footer.contains("esc back") && footer.contains("q quit"),
            "{footer}"
        );
    }

    #[test]
    fn grid_squeezes_rows_on_short_terminal_then_drops_overflow() {
        let rects = fleet_grid(Rect::new(0, 0, 40, 12), 3);
        assert_eq!(rects.len(), 3, "three 4-row cards fit in 12 rows");
        assert!(rects.iter().all(|r| r.height == CARD_MIN_HEIGHT));
        let rects = fleet_grid(Rect::new(0, 0, 40, 12), 8);
        assert_eq!(rects.len(), 3);
    }

    #[test]
    fn hit_finds_the_card_under_the_mouse() {
        let area = Rect::new(0, 0, 80, 20);
        assert_eq!(fleet_hit(area, 2, 1, 1), Some(0));
        assert_eq!(fleet_hit(area, 2, 79, 3), Some(1));
        assert_eq!(fleet_hit(area, 2, 1, 19), None);
    }

    #[test]
    fn live_card_shows_every_present_reading() {
        let theme = Theme::default();
        let text = draw(40, 10, |f| {
            render_fleet(f, f.area(), &[card("studio")], 0, &theme);
        });
        for needle in [
            "studio",
            "Apple M4 Pro",
            "cpu  42%",
            "P 3.9G · E 2.6G",
            "gpu  10%",
            "12.0G/24.0G",
            "61°",
            "bat 87%+",
            "All normal",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in\n{text}");
        }
    }

    #[test]
    fn absent_hardware_hides_rather_than_printing_zero() {
        let theme = Theme::default();
        let desktop = FleetCard {
            gpu: None,
            battery: None,
            power_w: None,
            clocks: Vec::new(),
            ..card("mini")
        };
        let text = draw(40, 10, |f| render_fleet(f, f.area(), &[desktop], 0, &theme));
        assert!(!text.contains("gpu"), "{text}");
        assert!(!text.contains("bat"), "{text}");
        assert!(!text.contains(" 0%"), "{text}");
        assert!(!text.contains("0.0W"), "{text}");
    }

    #[test]
    fn stale_card_says_reconnecting_instead_of_verdict() {
        let theme = Theme::default();
        let stale = FleetCard {
            link: CardLink::Reconnecting,
            ..card("air")
        };
        let text = draw(40, 10, |f| render_fleet(f, f.area(), &[stale], 0, &theme));
        assert!(text.contains("reconnecting…"), "{text}");
        assert!(!text.contains("All normal"), "{text}");
    }

    #[test]
    fn stale_card_paints_every_reading_faint() {
        let theme = Theme::default();
        let stale = FleetCard {
            link: CardLink::Reconnecting,
            ..card("air")
        };
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        terminal
            .draw(|f| render_fleet(f, f.area(), &[stale], 1, &theme))
            .unwrap();
        let buf = terminal.backend().buffer();
        let faint = theme.faint().fg;
        // Row 2 is the cpu line: every inked cell is faint.
        for x in 1..39 {
            let cell = &buf[(x, 2)];
            if cell.symbol() != " " {
                assert_eq!(cell.fg, faint.unwrap_or_default(), "x={x}");
            }
        }
    }

    #[test]
    fn connecting_card_without_readings_has_no_fake_numbers() {
        let theme = Theme::default();
        let empty = FleetCard {
            host: "new",
            link: CardLink::Connecting,
            chip: None,
            cpu: None,
            clocks: Vec::new(),
            gpu: None,
            mem: None,
            pressure: None,
            power_w: None,
            peak_c: None,
            battery: None,
            verdict: None,
            cpu_history: &[],
        };
        let text = draw(40, 10, |f| render_fleet(f, f.area(), &[empty], 0, &theme));
        assert!(text.contains("connecting…"), "{text}");
        assert!(!text.contains('%'), "{text}");
    }

    #[test]
    fn pressure_word_only_when_not_nominal() {
        let theme = Theme::default();
        let warn = FleetCard {
            pressure: Some(Pressure::Warn),
            ..card("a")
        };
        let text = draw(50, 10, |f| render_fleet(f, f.area(), &[warn], 0, &theme));
        assert!(text.contains("pressure warn"), "{text}");
        let text = draw(50, 10, |f| {
            render_fleet(f, f.area(), &[card("a")], 0, &theme);
        });
        assert!(!text.contains("pressure"), "{text}");
    }

    #[test]
    fn history_strip_draws_braille() {
        let theme = Theme::default();
        let text = draw(40, 10, |f| {
            render_fleet(f, f.area(), &[card("a")], 0, &theme);
        });
        let strip = text.lines().nth(7).unwrap();
        assert!(
            strip
                .chars()
                .any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)),
            "{strip}"
        );
    }

    #[test]
    fn history_strip_shows_light_load_on_a_scaled_axis() {
        let theme = Theme::default();
        let light = [0.1f32; 16];
        let quiet = FleetCard {
            cpu_history: &light,
            ..card("a")
        };
        let text = draw(40, 10, |f| render_fleet(f, f.area(), &[quiet], 0, &theme));
        let strip = text.lines().nth(7).unwrap();
        assert!(
            strip
                .chars()
                .any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)),
            "{strip}"
        );
    }

    #[test]
    fn history_strip_ignores_non_finite_samples() {
        let theme = Theme::default();
        let odd = [f32::NAN, f32::INFINITY, 0.5];
        let card = FleetCard {
            cpu_history: &odd,
            ..card("a")
        };
        let text = draw(40, 10, |f| render_fleet(f, f.area(), &[card], 0, &theme));
        assert!(text.contains("cpu"), "{text}");
    }

    #[test]
    fn overflow_note_counts_hidden_hosts() {
        let theme = Theme::default();
        let cards: Vec<_> = ["a", "b", "c", "d"].into_iter().map(card).collect();
        let text = draw(40, 10, |f| render_fleet(f, f.area(), &cards, 0, &theme));
        assert!(text.contains("+2 more"), "{text}");
    }

    #[test]
    fn procs_view_lists_rows_and_dashes_for_withheld_cpu() {
        let theme = Theme::default();
        let procs = [
            FleetProc {
                pid: 42,
                name: "cargo",
                cpu_pct: Some(250.0),
                mem_bytes: Some(1 << 30),
            },
            FleetProc {
                pid: 1,
                name: "launchd",
                cpu_pct: None,
                mem_bytes: None,
            },
        ];
        let text = draw(50, 8, |f| {
            render_fleet_procs(f, f.area(), &card("studio"), &procs, &theme);
        });
        assert!(text.contains("cargo") && text.contains("250.0%"), "{text}");
        assert!(text.contains("launchd") && text.contains('—'), "{text}");
        assert!(text.contains("esc back"), "{text}");
    }
}
