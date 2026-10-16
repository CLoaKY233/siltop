use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use siltop_core::SeriesId;
use siltop_core::{MemorySnapshot, Scale};

use crate::chrome::{Axis, FULL_CELL, Graph, GraphInk, render_scaled_graph, render_stacked_bar};
use crate::layout::Panel;
use crate::theme::Theme;
use crate::widgets::{AppView, frame_panel};

/// Glyph for the file cache: lighter than the used parts, since the kernel
/// hands it back the moment an app asks.
const CACHE_CELL: char = '▒';

/// Rows the graph needs before the composition row is worth taking from it.
const BAR_MIN_ROWS: u16 = 3;

pub(super) fn render(frame: &mut Frame, area: Rect, view: &AppView<'_>, theme: &Theme) {
    let mem = &view.snapshot.memory;
    let graph = Graph {
        history: view.history(SeriesId::Mem),
        ramp: theme.mem,
        theme,
        scale: Scale::Fixed(1.0),
        axis: Axis::Percent,
        ink: GraphInk::Load(view.snapshot.thermal),
        time: view.time_for(view.history(SeriesId::Mem)),
    };
    let inner = frame_panel(frame, area, Panel::Mem, view, theme, Some(&graph));
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let (bar, plot) = split(inner);
    if let Some(bar) = bar {
        render_stacked_bar(frame, bar, &composition_segments(mem, theme), theme);
    }
    if let Some(plot) = plot {
        render_scaled_graph(frame, plot, graph);
    }
}

/// One geometry source: the composition row on top when the graph can
/// spare it, the graph below; a one-row panel is the bar alone.
fn split(inner: Rect) -> (Option<Rect>, Option<Rect>) {
    match inner.height {
        0 => (None, None),
        1 => (Some(inner), None),
        h if h < BAR_MIN_ROWS => (None, Some(inner)),
        _ => (
            Some(Rect { height: 1, ..inner }),
            Some(Rect {
                y: inner.y.saturating_add(1),
                height: inner.height - 1,
                ..inner
            }),
        ),
    }
}

/// app / wired / compressed in the memory ramp's three shades, brightest
/// first, then the cache in the faint tone.
pub(crate) fn composition_segments(mem: &MemorySnapshot, theme: &Theme) -> [(f32, Style, char); 4] {
    let c = mem.composition();
    let ink = |color| Style::default().fg(color);
    [
        (c.ratio(c.app), ink(theme.mem.crest), FULL_CELL),
        (c.ratio(c.wired), ink(theme.mem.body), FULL_CELL),
        (c.ratio(c.compressed), ink(theme.mem.foot), FULL_CELL),
        (c.ratio(c.cached), theme.dim(), CACHE_CELL),
    ]
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::widgets::tests_support::fixture;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::text::Line;
    use siltop_core::Pressure;
    use siltop_core::SeriesId;

    const G: u64 = 1024 * 1024 * 1024;

    fn title(mem: &MemorySnapshot, theme: &Theme) -> Line<'static> {
        let mut fx = fixture("");
        fx.snap.memory = *mem;
        crate::titles::mem(&fx.view(), theme).full(theme)
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn sample() -> MemorySnapshot {
        MemorySnapshot {
            used_bytes: 18 * G,
            total_bytes: 36 * G,
            wired_bytes: 4 * G,
            compressed_bytes: 2 * G,
            cache_bytes: 6 * G,
            ..MemorySnapshot::default()
        }
    }

    fn paint(mem: MemorySnapshot, w: u16, h: u16) -> Vec<String> {
        let mut fx = fixture("");
        fx.snap.memory = mem;
        fx.histories.get_mut(SeriesId::Mem).push(0.4);
        fx.histories.get_mut(SeriesId::Mem).push(0.5);
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &fx.view(), &Theme::default()))
            .unwrap();
        let buf = terminal.backend().buffer();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn title_used_over_total_and_pressure_word() {
        let mut m = sample();
        m.pressure = Pressure::Warn;
        let text = line_text(&title(&m, &Theme::default()));
        assert!(text.contains("mem"));
        assert!(text.contains("18.0G"));
        assert!(text.contains("36.0G"));
        assert!(text.contains("● warn"), "{text}");
        assert!(!text.contains("nominal"));
    }

    #[test]
    fn nominal_pressure_has_no_word_or_dot() {
        let text = line_text(&title(&sample(), &Theme::default()));
        assert!(!text.contains('●'), "{text}");
        assert!(!text.contains("nominal"), "{text}");
    }

    #[test]
    fn swap_token_absent_without_swap() {
        let text = line_text(&title(&sample(), &Theme::default()));
        assert!(!text.contains("swap"), "{text}");
    }

    #[test]
    fn swap_token_shows_size_and_direction() {
        let mut m = sample();
        m.swap_used_bytes = 820 * 1024 * 1024;
        m.swap_out_bps = Some(4096);
        let text = line_text(&title(&m, &Theme::default()));
        assert!(text.contains("swap"), "{text}");
        assert!(text.contains('↑'), "{text}");
        m.swap_out_bps = None;
        let idle = line_text(&title(&m, &Theme::default()));
        assert!(!idle.contains('↑') && !idle.contains('↓'), "{idle}");
    }

    #[test]
    fn composition_row_sits_above_the_graph() {
        let rows = paint(sample(), 40, 8);
        let bar = &rows[1];
        assert!(bar.contains(FULL_CELL), "{bar}");
        assert!(bar.contains(CACHE_CELL), "{bar}");
        assert!(
            rows[2..].iter().all(|r| !r.contains(CACHE_CELL)),
            "the bar is one row only"
        );
    }

    #[test]
    fn composition_row_never_overruns_the_panel_sweep() {
        for w in 3..60 {
            for h in 1..10 {
                let rows = paint(sample(), w, h);
                for r in &rows {
                    assert_eq!(r.chars().count(), usize::from(w), "{w}x{h}");
                }
            }
        }
    }

    #[test]
    fn bar_takes_a_row_only_when_the_graph_can_spare_it() {
        let inner = |h| Rect::new(0, 0, 20, h);
        assert_eq!(split(inner(0)), (None, None));
        assert_eq!(split(inner(1)).1, None, "one row: bar alone");
        assert!(split(inner(1)).0.is_some());
        assert_eq!(split(inner(2)).0, None, "two rows: graph keeps both");
        for h in BAR_MIN_ROWS..30 {
            let (bar, plot) = split(inner(h));
            let (bar, plot) = (bar.unwrap(), plot.unwrap());
            assert_eq!(bar.height + plot.height, h);
            assert_eq!(plot.y, bar.y + 1);
        }
    }

    #[test]
    fn segments_are_absent_for_unknown_total() {
        let segs = composition_segments(&MemorySnapshot::default(), &Theme::default());
        assert!(segs.iter().all(|(r, _, _)| *r == 0.0));
    }

    #[test]
    fn segment_ratios_sum_to_at_most_one() {
        let mut m = sample();
        m.cache_bytes = 64 * G;
        let segs = composition_segments(&m, &Theme::default());
        let sum: f32 = segs.iter().map(|s| s.0).sum();
        assert!(sum <= 1.0 + f32::EPSILON, "{sum}");
    }
}
