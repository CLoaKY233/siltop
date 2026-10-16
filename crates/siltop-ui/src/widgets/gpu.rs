use ratatui::Frame;
use ratatui::layout::Rect;
use siltop_core::{Scale, SeriesId};

use crate::chrome::{Axis, Graph, GraphInk, render_scaled_graph};
use crate::layout::Panel;
use crate::theme::Theme;
use crate::widgets::{AppView, frame_panel};

pub(super) fn render(frame: &mut Frame, area: Rect, view: &AppView<'_>, theme: &Theme) {
    let graph = Graph {
        history: view.history(SeriesId::Gpu),
        ramp: theme.gpu,
        theme,
        scale: Scale::LOAD,
        axis: Axis::Percent,
        ink: GraphInk::Load(view.snapshot.thermal),
        time: view.time_for(view.history(SeriesId::Gpu)),
    };
    let inner = frame_panel(frame, area, Panel::Gpu, view, theme, Some(&graph));
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    render_scaled_graph(frame, inner, graph);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::titles::budget;
    use ratatui::text::Line;

    fn title(view: &AppView<'_>, theme: &Theme) -> Line<'static> {
        crate::titles::gpu(view, theme).full(theme)
    }

    #[allow(dead_code)]
    fn title_for(view: &AppView<'_>, theme: &Theme, width: u16) -> Line<'static> {
        crate::titles::gpu(view, theme).fit(budget(width, 0), theme)
    }
    use crate::widgets::tests_support::fixture;
    use siltop_core::{GpuSnapshot, TempReading};

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn title_dash_when_missing() {
        let fx = fixture("");
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(text.contains("gpu"));
        assert!(text.contains('—'));
    }

    #[test]
    fn title_percent_and_watts() {
        let mut fx = fixture("");
        fx.ready = true;
        fx.snap.gpu = Some(GpuSnapshot {
            scaled: 0.12,
            watts: Some(1.1),
            freq_mhz: Some(461),
            ane_watts: Some(0.4),
            ..GpuSnapshot::default()
        });
        let theme = Theme::default();
        let text = line_text(&title(&fx.view(), &theme));
        assert!(text.contains("12%"));
        assert!(text.contains("461MHz"), "{text}");
        assert!(text.contains("1.1W"));
    }

    #[test]
    fn d32_headline_is_frequency_weighted_with_renderer_busy_beside_it() {
        // The btop #1833 trap: 48% "Device Utilization" at 589 MHz is 17%.
        let mut fx = fixture("");
        fx.ready = true;
        fx.snap.gpu = Some(GpuSnapshot {
            scaled: 0.17,
            active: 0.48,
            renderer_busy: Some(0.48),
            freq_mhz: Some(589),
            max_mhz: Some(1_620),
            watts: Some(1.5),
            ..GpuSnapshot::default()
        });
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(text.contains("17%"), "{text}");
        assert!(text.contains("589MHz"), "{text}");
        assert!(text.contains("busy 48%"), "{text}");
        assert!(text.contains("1.5W"), "{text}");
        let pct = text.find("17%").unwrap();
        assert!(pct < text.find("busy").unwrap(), "headline first: {text}");
    }

    #[test]
    fn narrow_gpu_title_keeps_watts_and_temp() {
        let mut fx = fixture("");
        fx.ready = true;
        fx.snap.gpu = Some(GpuSnapshot {
            scaled: 0.17,
            renderer_busy: Some(0.48),
            freq_mhz: Some(589),
            watts: Some(1.5),
            temp_c: Some(47.0),
            ..GpuSnapshot::default()
        });
        let theme = Theme::default();
        let narrow = line_text(&title_for(&fx.view(), &theme, 30));
        assert!(
            narrow.contains("1.5W") && narrow.contains("47°"),
            "{narrow}"
        );
        assert!(
            !narrow.contains("MHz") && narrow.ends_with("… "),
            "{narrow}"
        );
        // Tighter still: temperature outranks watts.
        let tight = line_text(&title_for(&fx.view(), &theme, 26));
        assert!(tight.contains("47°") && !tight.contains('W'), "{tight}");
        let wide = line_text(&title_for(&fx.view(), &theme, 80));
        assert!(
            wide.contains("589MHz") && wide.contains("busy 48%"),
            "{wide}"
        );
    }

    #[test]
    fn gpu_title_never_shows_zero_mhz_or_close_busy() {
        let mut fx = fixture("");
        fx.ready = true;
        fx.snap.gpu = Some(GpuSnapshot {
            scaled: 0.40,
            renderer_busy: Some(0.45),
            freq_mhz: Some(0),
            ..GpuSnapshot::default()
        });
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(!text.contains("MHz"), "{text}");
        assert!(!text.contains("busy"), "{text}");
        assert!(!text.contains('W'), "no watts is absent, not 0 W: {text}");
        fx.ready = false;
        fx.snap.gpu = Some(GpuSnapshot {
            freq_mhz: Some(636),
            renderer_busy: Some(0.9),
            ..GpuSnapshot::default()
        });
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(!text.contains("MHz") && !text.contains("busy"), "{text}");
    }

    #[test]
    fn title_includes_gpu_temp() {
        let mut fx = fixture("");
        fx.ready = true;
        fx.snap.gpu = Some(GpuSnapshot {
            scaled: 0.12,
            ..GpuSnapshot::default()
        });
        fx.snap.sensors.gpu_c = Some(38.0);
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(text.contains("38°"), "{text}");
        fx.snap.sensors.gpu_c = None;
        fx.snap.gpu = Some(GpuSnapshot {
            scaled: 0.12,
            temp_c: Some(51.0),
            ..GpuSnapshot::default()
        });
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(text.contains("51°"), "{text}");
    }

    #[test]
    fn title_temp_without_gpu_snapshot() {
        let mut fx = fixture("");
        fx.snap.gpu = None;
        fx.snap.sensors.gpu_c = Some(44.0);
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(text.contains("44°"), "{text}");
        assert!(text.contains('—'), "{text}");
        fx.snap.sensors.gpu_c = None;
        fx.snap.sensors.readings = vec![TempReading {
            name: String::from("GPU die"),
            celsius: 39.0,
        }];
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(text.contains("39°"), "{text}");
    }

    #[test]
    fn title_ellipsis_until_ready() {
        let mut fx = fixture("");
        fx.ready = false;
        fx.snap.gpu = Some(GpuSnapshot {
            scaled: 0.4,
            watts: Some(2.0),
            ..GpuSnapshot::default()
        });
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(text.contains('…'));
        assert!(!text.contains('%'));
    }
}
