//! Mirrored I/O graph: one direction grows up from the midline, the other
//! hangs below it, on one shared scale so the halves compare honestly. The
//! shared ceiling sits in the top border and time in the bottom one, like
//! every other graph.

use ratatui::Frame;
use ratatui::layout::Rect;
use siltop_core::{History, Scale, ScaleRange, TimeAxis};

use crate::braille::{self, Grow};
use crate::chrome::{
    Axis, BorderScale, MIN_SAMPLES, dots_for, paint_rows, render_sampling, scale_label,
    stamp_time_labels, visible_range,
};
use crate::theme::{Ramp, Theme};

#[derive(Debug, Clone, Copy)]
pub(super) struct Mirror<'a> {
    /// Drawn above the midline (download, read).
    pub up: &'a History,
    /// Drawn below the midline (upload, write).
    pub down: &'a History,
    pub up_ramp: Ramp,
    pub down_ramp: Ramp,
    pub theme: &'a Theme,
    pub scale: Scale,
    pub axis: Axis,
    pub time: TimeAxis,
}

impl Mirror<'_> {
    /// Whether either direction has enough samples to draw.
    #[must_use]
    pub(super) fn ready(&self) -> bool {
        self.up.len().max(self.down.len()) >= MIN_SAMPLES
    }

    /// One scale for both halves: the larger visible ceiling of the two.
    #[must_use]
    pub(super) fn range(&self, width: u16) -> ScaleRange {
        let ceiling = |h: &History| visible_range(h, self.scale, self.axis, self.time, width).max;
        ScaleRange {
            min: 0.0,
            max: ceiling(self.up).max(ceiling(self.down)),
        }
    }
}

impl BorderScale for Mirror<'_> {
    fn border_label(&self, width: u16) -> Option<String> {
        if !self.ready() {
            return None;
        }
        scale_label(self.scale, self.axis, self.range(width))
    }
}

/// Rows above the midline for a plot `height` tall; the upper half gets the
/// odd row so a 1-row plot still shows the primary direction.
#[must_use]
pub(super) fn split(height: u16) -> (u16, u16) {
    let up = height.div_ceil(2);
    (up, height - up)
}

pub(super) fn render_mirror(frame: &mut Frame, area: Rect, m: Mirror<'_>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if !m.ready() {
        render_sampling(frame, area, m.theme);
        return;
    }
    let plot = area;
    let span = m.time.span(dots_for(plot.width));
    let range = m.range(plot.width);
    let (up_h, down_h) = split(plot.height);
    let halves = [
        (
            m.up,
            m.up_ramp,
            Grow::Up,
            Rect {
                height: up_h,
                ..plot
            },
        ),
        (
            m.down,
            m.down_ramp,
            Grow::Down,
            Rect {
                y: plot.y.saturating_add(up_h),
                height: down_h,
                ..plot
            },
        ),
    ];
    for (history, ramp, grow, rect) in halves {
        if rect.height == 0 {
            continue;
        }
        let dots = braille::dots(history, m.time, rect.width, range);
        let rows = braille::render_dots(&dots, rect.width, rect.height, grow);
        let n = rows.len();
        // the shade deepens away from the midline in both halves
        paint_rows(frame.buffer_mut(), rect, &rows, |i| match grow {
            Grow::Up => m.theme.series_row(&ramp, i, n),
            Grow::Down => m.theme.series_row(&ramp, n - 1 - i, n),
        });
    }
    stamp_time_labels(frame.buffer_mut(), plot, span, m.theme);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn filled(value: f32, n: usize) -> History {
        let mut h = History::for_period(std::time::Duration::from_secs(1));
        for i in 0..n {
            h.push_at(value, i as u64 * 1_000);
        }
        h
    }

    fn draw(up: &History, down: &History, w: u16, h: u16) -> Vec<String> {
        let theme = Theme::default();
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| {
            render_mirror(
                f,
                f.area(),
                Mirror {
                    up,
                    down,
                    up_ramp: theme.net,
                    down_ramp: theme.net,
                    theme: &theme,
                    scale: Scale::Auto { floor: 1.0 },
                    axis: Axis::None,
                    time: TimeAxis::live(up),
                },
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_owned()).collect())
            .collect()
    }

    fn lit(row: &str) -> usize {
        row.chars()
            .filter(|c| ('\u{2801}'..='\u{28FF}').contains(c))
            .count()
    }

    #[test]
    fn upload_dots_land_below_the_midline() {
        let idle = filled(0.0, 40);
        let upload = filled(50.0, 40);
        let rows = draw(&idle, &upload, 20, 6);
        assert!(rows[..3].iter().all(|r| lit(r) == 0), "{rows:#?}");
        assert!(rows[3..].iter().any(|r| lit(r) > 0), "{rows:#?}");
        // hangs from the midline: the row under it fills before the bottom row
        assert!(lit(&rows[3]) >= lit(&rows[5]), "{rows:#?}");
    }

    #[test]
    fn download_dots_land_above_the_midline() {
        let rows = draw(&filled(50.0, 40), &filled(0.0, 40), 20, 6);
        assert!(rows[..3].iter().any(|r| lit(r) > 0), "{rows:#?}");
        assert!(rows[3..].iter().all(|r| lit(r) == 0), "{rows:#?}");
        assert!(lit(&rows[2]) >= lit(&rows[0]), "{rows:#?}");
    }

    #[test]
    fn halves_share_one_scale() {
        // 10 up vs 100 down: the small half must stay small.
        let rows = draw(&filled(10.0, 40), &filled(100.0, 40), 20, 8);
        let up: usize = rows[..4].iter().map(|r| lit(r)).sum();
        let down: usize = rows[4..].iter().map(|r| lit(r)).sum();
        assert!(up < down, "up {up} down {down}");
        assert!(
            lit(&rows[0]) == 0,
            "10 of 100 must not reach the top: {rows:#?}"
        );
    }

    #[test]
    fn border_label_is_the_shared_ceiling_once_ready() {
        let theme = Theme::default();
        let mirror = |up, down| Mirror {
            up,
            down,
            up_ramp: theme.net,
            down_ramp: theme.net,
            theme: &theme,
            scale: Scale::Auto { floor: 1_024.0 },
            axis: Axis::Bytes,
            time: TimeAxis::live(up),
        };
        let (small, big) = (filled(10_000.0, 40), filled(1_500_000.0, 40));
        assert_eq!(
            mirror(&small, &big).border_label(20).as_deref(),
            Some("2MB/s")
        );
        assert_eq!(
            mirror(&big, &small).border_label(20).as_deref(),
            Some("2MB/s")
        );
        let empty = filled(0.0, 1);
        assert!(mirror(&empty, &empty).border_label(20).is_none());
        let rows = draw(&empty, &empty, 20, 4);
        assert!(rows.iter().any(|r| r.contains("sampling…")), "{rows:#?}");
    }

    #[test]
    fn split_gives_the_odd_row_to_the_top() {
        assert_eq!(split(1), (1, 0));
        assert_eq!(split(2), (1, 1));
        assert_eq!(split(5), (3, 2));
        for h in 0..40 {
            let (a, b) = split(h);
            assert_eq!(a + b, h);
            assert!(a >= b && a - b <= 1);
        }
    }

    #[test]
    fn braille_down_mirrors_braille_up() {
        for l in 0..5 {
            for r in 0..5 {
                let up = braille::BRAILLE_UP[l][r] as u32 - 0x2800;
                let down = braille::BRAILLE_DOWN[l][r] as u32 - 0x2800;
                assert_eq!(up.count_ones(), down.count_ones(), "{l}/{r}");
            }
        }
        assert_eq!(braille::BRAILLE_DOWN[1][0], '⠁');
        assert_eq!(braille::BRAILLE_DOWN[4][4], '⣿');
    }
}
