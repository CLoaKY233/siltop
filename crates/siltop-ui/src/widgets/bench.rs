//! The live page `siltop bench` shows while it loads the CPU: a header
//! with the countdown, then one graph per reading this Mac has. A reading
//! the Mac lacks (no power rail, no temperature) has no graph at all.
//! Every graph spans exactly the planned run, `0s` at the left edge and the
//! full duration at the right, so it fills left to right as the run goes.
//! A run longer than the kept history shows its newest 15 minutes.

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use siltop_core::{HISTORY_WINDOW, History, Scale, Stamp, TimeAxis, Zoom, celsius, watts_display};

use crate::chrome::{
    Axis, Graph, GraphInk, cell_titled, dots_for, duration_label, paint_scaled_graph,
    stamp_edge_labels,
};
use crate::theme::{Ramp, Theme};

/// What the page shows; the bin owns the histories.
#[derive(Debug, Clone, Copy)]
pub struct BenchView<'a> {
    pub chip: &'a str,
    pub threads: usize,
    /// The planned run: the width of every graph.
    pub duration: Duration,
    pub remaining: Duration,
    /// The fastest cluster's clock in MHz.
    pub clock: &'a History,
    pub temp: Option<&'a History>,
    pub watts: Option<&'a History>,
}

struct Row<'a> {
    label: &'static str,
    value: String,
    history: &'a History,
    ramp: Ramp,
    scale: Scale,
    axis: Axis,
}

fn rows<'a>(view: &BenchView<'a>, theme: &Theme) -> Vec<Row<'a>> {
    let mut rows = Vec::with_capacity(3);
    if let Some(mhz) = view.clock.last() {
        rows.push(Row {
            label: "clock",
            value: format!("{mhz:.0}MHz"),
            history: view.clock,
            ramp: theme.cpu,
            scale: Scale::Auto { floor: 1000.0 },
            axis: Axis::Number,
        });
    }
    if let Some((history, c)) = view.temp.and_then(|h| h.last().map(|c| (h, c))) {
        rows.push(Row {
            label: "temp",
            value: celsius(c),
            history,
            ramp: theme.temp,
            scale: Scale::TEMP,
            axis: Axis::Celsius,
        });
    }
    if let Some((history, w)) = view.watts.and_then(|h| h.last().map(|w| (h, w))) {
        rows.push(Row {
            label: "cpu power",
            value: watts_display(w),
            history,
            ramp: theme.gpu,
            scale: Scale::Auto { floor: 5.0 },
            axis: Axis::Watts,
        });
    }
    rows
}

fn countdown(remaining: Duration) -> String {
    let secs = remaining.as_secs();
    format!("{:02}:{:02} left", secs / 60, secs % 60)
}

/// Paints the whole frame. Never paints a cell background, so a glass
/// terminal stays see-through.
pub fn render_bench(frame: &mut Frame, view: &BenchView<'_>, theme: &Theme) {
    paint(frame, view, theme);
    crate::chrome::scrub_controls(frame.buffer_mut());
    theme.finish(frame.buffer_mut());
}

fn paint(frame: &mut Frame, view: &BenchView<'_>, theme: &Theme) {
    let area = frame.area();
    if area.width < 4 || area.height < 3 {
        return;
    }
    let [head, body, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let title = Line::from(vec![
        Span::styled(" bench ", theme.title()),
        Span::styled(
            format!("{} · {} threads", view.chip, view.threads),
            theme.dim(),
        ),
    ]);
    frame.render_widget(title, head);
    let clock = Line::from(Span::styled(
        format!("{} ", countdown(view.remaining)),
        theme.title(),
    ))
    .right_aligned();
    frame.render_widget(clock, head);
    let hint = Line::from(Span::styled(
        " Esc stops early and still reports",
        theme.dim(),
    ));
    frame.render_widget(hint, foot);

    let rows = rows(view, theme);
    if rows.is_empty() {
        let wait = Line::from(Span::styled(" sampling…", theme.dim()));
        frame.render_widget(wait, body);
        return;
    }
    let areas = Layout::vertical(rows.iter().map(|_| Constraint::Fill(1))).split(body);
    for (row, area) in rows.iter().zip(areas.iter()) {
        render_row(frame, *area, row, view.duration, theme);
    }
}

/// The time axis that puts `0` (the load starting) at the left edge of a
/// plot `width` columns wide and `duration` at its right edge, with its
/// edge labels. Histories are stamped in milliseconds since the load
/// started. A run longer than the kept history ([`HISTORY_WINDOW`]) slides:
/// the plot shows the newest window, labelled from where it starts.
fn run_axis(duration: Duration, newest: Option<Stamp>, width: u16) -> (TimeAxis, String, String) {
    let window = duration.min(HISTORY_WINDOW);
    let millis = |d: Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
    let window_ms = millis(window);
    let now = if duration > HISTORY_WINDOW {
        newest.unwrap_or(0).max(window_ms)
    } else {
        millis(duration)
    };
    let left = match now - window_ms {
        0 => String::from("0s"),
        start => duration_label(Duration::from_millis(start)),
    };
    let dots = u32::try_from(dots_for(width).max(1)).unwrap_or(u32::MAX);
    let axis = TimeAxis {
        zoom: Zoom::Samples,
        now,
        cadence: window / dots,
    };
    (axis, left, duration_label(Duration::from_millis(now)))
}

fn render_row(frame: &mut Frame, area: Rect, row: &Row<'_>, duration: Duration, theme: &Theme) {
    let inner = cell_titled(frame, area, row.label, Some(&row.value), false, theme);
    let (time, left, right) = run_axis(duration, row.history.newest(), inner.width);
    let graph = Graph {
        history: row.history,
        ramp: row.ramp,
        theme,
        scale: row.scale,
        axis: row.axis,
        ink: GraphInk::Flat,
        time,
    };
    if paint_scaled_graph(frame, inner, graph).is_some() {
        stamp_edge_labels(frame.buffer_mut(), inner, &left, &right, theme);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn history(values: &[f32]) -> History {
        let mut h = History::with_capacity(64);
        for (i, v) in values.iter().enumerate() {
            h.push_at(*v, i as u64 * 500);
        }
        h
    }

    fn screen(view: &BenchView<'_>, theme: &Theme) -> String {
        let mut term = Terminal::new(TestBackend::new(60, 16)).unwrap();
        term.draw(|f| render_bench(f, view, theme)).unwrap();
        let buf = term.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn chip_escapes_never_reach_the_terminal() {
        let clock = history(&[3000.0]);
        let view = BenchView {
            chip: "Apple\u{1b}]2;pwn\u{7} M4",
            threads: 8,
            duration: Duration::from_secs(60),
            remaining: Duration::from_secs(5),
            clock: &clock,
            temp: None,
            watts: None,
        };
        let text = screen(&view, &Theme::default());
        assert!(
            text.lines()
                .all(|row| !row.chars().any(siltop_core::is_unsafe)),
            "{text}"
        );
        assert!(text.contains("]2;pwn"), "{text}");
    }

    #[test]
    fn page_shows_countdown_and_every_present_reading() {
        let clock = history(&[4400.0, 4500.0, 4480.0]);
        let temp = history(&[70.0, 80.0, 88.0]);
        let watts = history(&[25.0, 28.0, 30.0]);
        let view = BenchView {
            chip: "Apple M4 Pro",
            threads: 14,
            duration: Duration::from_secs(60),
            remaining: Duration::from_secs(42),
            clock: &clock,
            temp: Some(&temp),
            watts: Some(&watts),
        };
        let text = screen(&view, &Theme::default());
        for needle in [
            "bench Apple M4 Pro · 14 threads",
            "00:42 left",
            "clock",
            "4480MHz",
            "temp",
            "88°",
            "cpu power",
            "30W",
            "Esc stops early",
        ] {
            assert!(text.contains(needle), "{needle:?} in\n{text}");
        }
    }

    /// Regression (whole Mac family): a Mac without a power rail or a
    /// temperature gets no empty graph and no `0W`.
    #[test]
    fn absent_readings_have_no_graph() {
        let clock = history(&[3000.0, 3100.0]);
        let empty = History::with_capacity(4);
        let view = BenchView {
            chip: "Apple M1",
            threads: 8,
            duration: Duration::from_secs(60),
            remaining: Duration::from_secs(5),
            clock: &clock,
            temp: None,
            watts: Some(&empty),
        };
        let text = screen(&view, &Theme::default());
        assert!(text.contains("clock"));
        assert!(!text.contains("temp"), "{text}");
        assert!(!text.contains("cpu power"), "{text}");
        assert!(!text.contains("0W"), "{text}");
    }

    /// Regression (bench window): a 12 s run fills the graph from the left
    /// edge (`0s`) to the right edge (`12s`), not the dashboard's two
    /// minutes squashed against `now`.
    #[test]
    fn bench_window_graphs_span_the_planned_run() {
        // The bin keeps one sample per 500 ms step and stamps the first one
        // step after the load starts.
        let run = |n: u64| {
            let mut h = History::for_period(Duration::from_millis(500));
            for i in 1..=n {
                h.push_at(4000.0, i * 500);
            }
            h
        };
        let clock = run(24);
        let full = BenchView {
            chip: "Apple M5 Pro",
            threads: 9,
            duration: Duration::from_secs(12),
            remaining: Duration::ZERO,
            clock: &clock,
            temp: None,
            watts: None,
        };
        let text = screen(&full, &Theme::default());
        let rows: Vec<&str> = text.lines().collect();
        let bottom = rows[rows.len() - 2];
        assert!(bottom.contains("0s"), "{text}");
        assert!(bottom.contains("12s"), "{text}");
        assert!(
            !bottom.contains("now") && !bottom.contains('\u{2212}'),
            "{text}"
        );
        // Solid plot: the row just above the bottom border is inked from the
        // first sample (one 500 ms step in, about 4% of 12 s) to the last
        // inner column.
        let plot: Vec<char> = rows[rows.len() - 3].chars().collect();
        let inner = &plot[1..plot.len() - 1];
        let first_step = inner.len() / 24 + 1;
        assert!(inner[first_step..].iter().all(|c| *c != ' '), "{text}");

        // Half way through, the left half is drawn and the right half waits.
        let half = run(12);
        let view = BenchView {
            clock: &half,
            remaining: Duration::from_secs(6),
            ..full
        };
        let text = screen(&view, &Theme::default());
        let rows: Vec<&str> = text.lines().collect();
        let plot: Vec<char> = rows[rows.len() - 3].chars().collect();
        let width = plot.len() - 2;
        assert_ne!(plot[width / 2 - 2], ' ', "{text}");
        assert_eq!(plot[width * 3 / 4], ' ', "{text}");
    }

    /// A run longer than the kept history (15 min) never leaves the plot
    /// empty: it fills up to the window, then slides with the newest sample
    /// and says where its left edge is.
    #[test]
    fn bench_window_longer_than_the_history_slides() {
        let run = |secs: u64| {
            let mut h = History::for_period(Duration::from_millis(500));
            for i in 1..=secs * 2 {
                h.push_at(4000.0, i * 500);
            }
            h
        };
        let view = |clock: &History| {
            let text = screen(
                &BenchView {
                    chip: "Apple M5 Pro",
                    threads: 9,
                    duration: Duration::from_mins(30),
                    remaining: Duration::ZERO,
                    clock,
                    temp: None,
                    watts: None,
                },
                &Theme::default(),
            );
            let rows: Vec<String> = text.lines().map(String::from).collect();
            let plot: Vec<char> = rows[rows.len() - 3].chars().collect();
            (rows[rows.len() - 2].clone(), plot, text)
        };
        // Ten minutes in: two thirds of the 15-minute window are drawn.
        let (bottom, plot, text) = view(&run(600));
        assert!(bottom.contains("0s") && bottom.contains("15m"), "{text}");
        let width = plot.len() - 2;
        assert_ne!(plot[width / 2], ' ', "{text}");
        assert_eq!(plot[width * 7 / 8], ' ', "{text}");
        // Twenty minutes in: the newest 15 minutes, edge to edge.
        let (bottom, plot, text) = view(&run(1200));
        assert!(bottom.contains("5m") && bottom.contains("20m"), "{text}");
        assert!(plot[2..plot.len() - 2].iter().all(|c| *c != ' '), "{text}");
    }

    #[test]
    fn page_before_the_first_sample_says_sampling() {
        let empty = History::with_capacity(4);
        let view = BenchView {
            chip: "Apple M5",
            threads: 10,
            duration: Duration::from_secs(60),
            remaining: Duration::from_secs(60),
            clock: &empty,
            temp: None,
            watts: None,
        };
        let text = screen(&view, &Theme::default());
        assert!(text.contains("sampling…"), "{text}");
        assert!(text.contains("01:00 left"), "{text}");
    }

    #[test]
    fn tiny_terminal_draws_nothing_and_does_not_panic() {
        let clock = history(&[3000.0]);
        let view = BenchView {
            chip: "Apple M1",
            threads: 8,
            duration: Duration::from_secs(60),
            remaining: Duration::ZERO,
            clock: &clock,
            temp: None,
            watts: None,
        };
        let mut term = Terminal::new(TestBackend::new(3, 2)).unwrap();
        term.draw(|f| render_bench(f, &view, &Theme::default()))
            .unwrap();
    }

    /// Glass: the page never paints a background on any cell.
    #[test]
    fn glass_page_paints_no_backgrounds() {
        let clock = history(&[4400.0, 4500.0, 4480.0]);
        let temp = history(&[70.0, 80.0, 88.0]);
        let view = BenchView {
            chip: "Apple M4 Pro",
            threads: 14,
            duration: Duration::from_secs(60),
            remaining: Duration::from_secs(42),
            clock: &clock,
            temp: Some(&temp),
            watts: None,
        };
        let theme = Theme::default();
        let mut term = Terminal::new(TestBackend::new(60, 16)).unwrap();
        term.draw(|f| render_bench(f, &view, &theme)).unwrap();
        let buf = term.backend().buffer();
        for cell in buf.content() {
            assert_eq!(cell.bg, ratatui::style::Color::Reset);
        }
    }
}
