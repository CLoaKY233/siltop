//! The compact `heat` panel: temperatures and fans live in its title (one
//! fact, one home); the body is the hottest die's temperature history.

use ratatui::Frame;
use ratatui::layout::Rect;
use siltop_core::SeriesId;
use siltop_core::{History, Scale};

use crate::chrome::{Axis, Graph, GraphInk, render_scaled_graph};
use crate::layout::Panel;
use crate::theme::Theme;
use crate::widgets::{AppView, frame_panel};

pub(super) fn render(frame: &mut Frame, area: Rect, view: &AppView<'_>, theme: &Theme) {
    let graph = compact_temp_history(view).map(|history| Graph {
        history,
        ramp: theme.temp,
        theme,
        scale: Scale::TEMP,
        axis: Axis::Celsius,
        ink: GraphInk::Flat,
        time: view.time_for(history),
    });
    let inner = frame_panel(frame, area, Panel::Fans, view, theme, graph.as_ref());
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if let Some(graph) = graph {
        render_scaled_graph(frame, inner, graph);
    }
}

fn compact_temp_history<'a>(view: &'a AppView<'_>) -> Option<&'a History> {
    [
        view.history(SeriesId::CpuTemp),
        view.history(SeriesId::GpuTemp),
    ]
    .into_iter()
    .find(|h| !h.is_empty())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::widgets::tests_support::fixture;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::text::Line;
    use siltop_core::SeriesId;
    use siltop_core::{FanMetric, FanSnapshot, GpuSnapshot, TempReading};

    fn title(view: &AppView<'_>, theme: &Theme) -> Line<'static> {
        crate::titles::heat(view, theme).full(theme)
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn paint(view: &AppView<'_>, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), view, &Theme::default()))
            .unwrap();
        let buf = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn fans(rpms: &[u16]) -> FanSnapshot {
        FanSnapshot {
            fans: rpms
                .iter()
                .enumerate()
                .map(|(i, rpm)| FanMetric {
                    name: format!("Fan {}", i + 1),
                    rpm: *rpm,
                    max_rpm: 6000,
                })
                .collect(),
        }
    }

    fn thermal_fixture() -> crate::widgets::tests_support::Fixture {
        let mut fx = fixture("");
        fx.snap.cpu.scaled = 0.18;
        fx.snap.sensors.cpu_c = Some(42.0);
        fx.snap.sensors.gpu_c = Some(51.0);
        fx.snap.sensors.hotspot_c = Some(60.0);
        fx.snap.sensors.readings = vec![
            TempReading {
                name: String::from("cpu"),
                celsius: 42.0,
            },
            TempReading {
                name: String::from("nand"),
                celsius: 38.0,
            },
        ];
        fx.snap.gpu = Some(GpuSnapshot {
            scaled: 0.12,
            temp_c: Some(51.0),
            ..GpuSnapshot::default()
        });
        fx.snap.fans = fans(&[1200, 1850]);
        for _ in 0..8 {
            fx.histories.get_mut(SeriesId::CpuTemp).push(42.0);
        }
        fx
    }

    #[test]
    fn title_is_heat_not_sens() {
        let fx = thermal_fixture();
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(text.starts_with(" heat"), "{text}");
        assert!(!text.contains("sens") && !text.contains("hot "), "{text}");
    }

    #[test]
    fn title_lists_both_fans_on_one_token() {
        let mut fx = fixture("");
        fx.snap.fans = fans(&[1200, 1850]);
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(text.contains("1200·1850rpm"), "{text}");
    }

    #[test]
    fn title_dash_when_absent() {
        let fx = fixture("");
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(text.contains("heat"), "{text}");
        assert!(text.contains('—'), "{text}");
    }

    #[test]
    fn fans_idle_when_every_fan_is_stopped() {
        let mut fx = fixture("");
        fx.snap.sensors.hotspot_c = Some(40.0);
        fx.snap.fans = fans(&[0, 0]);
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(text.contains("fans idle"), "{text}");
        assert!(!text.contains("rpm"), "{text}");
    }

    #[test]
    fn fanless_mac_has_no_fan_token() {
        let mut fx = fixture("");
        fx.snap.sensors.hotspot_c = Some(55.0);
        fx.snap.fans = FanSnapshot::default();
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(!text.contains("fan") && !text.contains("rpm"), "{text}");
        assert!(text.contains("peak 55°"), "{text}");
    }

    #[test]
    fn heat_never_carries_cpu_temperature() {
        // Regression (D53): the same CPU °C printed in cpu and sens titles.
        let mut fx = fixture("");
        fx.snap.sensors.cpu_c = Some(91.0);
        fx.snap.sensors.hotspot_c = Some(64.0);
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(!text.contains("91"), "{text}");
        assert!(text.contains("peak 64°"), "{text}");
    }

    #[test]
    fn peak_is_the_hottest_reading() {
        let fx = thermal_fixture();
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(text.contains("peak 60°"), "{text}");
        assert!(text.contains("gpu 51°"), "{text}");
    }

    #[test]
    fn compact_body_does_not_repeat_the_title() {
        let fx = thermal_fixture();
        let text = paint(&fx.view(), 60, 8);
        assert_eq!(text.matches("1200·1850rpm").count(), 1, "{text}");
        assert_eq!(text.matches("51°").count(), 1, "{text}");
        assert!(!text.contains("nand"), "{text}");
    }
}
