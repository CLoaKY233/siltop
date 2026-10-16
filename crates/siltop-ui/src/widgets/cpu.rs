use ratatui::Frame;
use ratatui::layout::Rect;
use siltop_core::{Scale, SeriesId};

use crate::chrome::{Axis, Graph, GraphInk, render_scaled_graph};
use crate::layout::Panel;
use crate::theme::Theme;
use crate::widgets::{AppView, frame_panel};

pub(super) fn render(frame: &mut Frame, area: Rect, view: &AppView<'_>, theme: &Theme) {
    let graph = Graph {
        history: view.history(SeriesId::Cpu),
        ramp: theme.cpu,
        theme,
        scale: Scale::LOAD,
        axis: Axis::Percent,
        ink: GraphInk::Signal(view.snapshot.thermal),
        time: view.time_for(view.history(SeriesId::Cpu)),
    };
    let inner = frame_panel(frame, area, Panel::Cpu, view, theme, Some(&graph));
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    render_scaled_graph(frame, inner, graph);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::titles::{TITLE_CHROME, budget, cluster_clocks};
    use ratatui::text::Line;
    use siltop_core::ready_pct;

    fn title(view: &AppView<'_>, theme: &Theme) -> Line<'static> {
        crate::titles::cpu(view, theme).full(theme)
    }

    fn title_for(view: &AppView<'_>, theme: &Theme, width: u16) -> Line<'static> {
        crate::titles::cpu(view, theme).fit(budget(width, 0), theme)
    }
    use crate::widgets::tests_support::fixture;
    use siltop_core::Thermal;

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn title_ellipsis_until_ready() {
        let mut fx = fixture("");
        fx.ready = false;
        fx.snap.cpu.active = 0.184;
        fx.snap.cpu.watts = Some(8.24);
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(text.contains("cpu"));
        assert!(text.contains('…'));
        assert!(!text.contains('%'));
        assert!(!text.contains('W'));
    }

    #[test]
    fn title_percent_and_watts() {
        let mut fx = fixture("");
        fx.ready = true;
        fx.snap.cpu.scaled = 0.184;
        fx.snap.cpu.active = 0.184;
        fx.snap.cpu.watts = Some(8.24);
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(text.contains("cpu"));
        assert!(text.contains("18%"));
        assert!(text.contains("8.2W"));
        assert!(!text.contains("busy"), "{text}");
    }

    fn clustered(s_mhz: u32, p_mhz: u32, e_mhz: u32) -> siltop_core::CpuSnapshot {
        use siltop_core::{Cluster, ClusterKind, CpuSnapshot};
        let cluster = |kind, freq_mhz| {
            (freq_mhz > 0).then_some(Cluster {
                kind,
                scaled: 0.5,
                active: 0.5,
                freq_mhz,
            })
        };
        CpuSnapshot {
            s_cluster: cluster(ClusterKind::Super, s_mhz),
            p_cluster: cluster(ClusterKind::Performance, p_mhz),
            e_cluster: cluster(ClusterKind::Efficiency, e_mhz),
            ..CpuSnapshot::default()
        }
    }

    #[test]
    fn narrow_titles_drop_clocks_before_watts_and_temp() {
        let mut fx = fixture("");
        fx.ready = true;
        fx.snap.cpu = clustered(4_380, 1_344, 0);
        fx.snap.cpu.scaled = 0.34;
        fx.snap.cpu.active = 0.47;
        fx.snap.cpu.watts = Some(11.6);
        fx.snap.cpu.temp_c = Some(58.0);
        let theme = Theme::default();
        let wide = line_text(&title_for(&fx.view(), &theme, 80));
        assert!(
            wide.contains("S 4.4G · P 1.3G") && wide.contains("12W"),
            "{wide}"
        );
        for width in [26_u16, 40, 48] {
            let line = title_for(&fx.view(), &theme, width);
            let text = line_text(&line);
            assert!(
                line.width() + TITLE_CHROME <= usize::from(width),
                "{width}: {text}"
            );
            assert!(!text.contains('G'), "clocks go first at {width}: {text}");
            assert!(
                text.contains("34%") && text.contains("58°"),
                "{width}: {text}"
            );
            assert!(text.ends_with("… "), "{width}: {text}");
        }
        let mid = line_text(&title_for(&fx.view(), &theme, 48));
        assert!(mid.contains("busy 47%") && mid.contains("12W"), "{mid}");
    }

    #[test]
    fn cluster_clock_table() {
        let cases = [
            ((4_300, 3_100, 0), Some("S 4.3G · P 3.1G")),
            ((0, 1_344, 0), Some("P 1.3G")),
            ((0, 0, 2_064), Some("E 2.1G")),
            ((4_608, 4_380, 972), Some("S 4.6G · P 4.4G · E 1.0G")),
            ((0, 0, 0), None),
            ((949, 0, 0), Some("S 0.9G")),
            ((10_000, 0, 0), Some("S 10.0G")),
        ];
        for ((s, p, e), want) in cases {
            assert_eq!(cluster_clocks(&clustered(s, p, e)).as_deref(), want);
        }
    }

    #[test]
    fn title_shows_cluster_clocks_once_ready() {
        let mut fx = fixture("");
        fx.ready = true;
        fx.snap.cpu = clustered(4_300, 3_100, 0);
        fx.snap.cpu.scaled = 0.42;
        fx.snap.cpu.active = 0.45;
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(text.contains("42%"), "{text}");
        assert!(text.contains("S 4.3G · P 3.1G"), "{text}");
        assert!(!text.contains("busy"), "3 points apart is noise: {text}");
        fx.ready = false;
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(!text.contains('G'), "{text}");
    }

    #[test]
    fn busy_token_threshold_is_ten_points() {
        let mut fx = fixture("");
        fx.ready = true;
        for (scaled, active, shown) in [
            (0.30, 0.40, false),
            (0.30, 0.41, true),
            (0.50, 0.39, true),
            (0.0, 0.0, false),
            (1.0, 1.0, false),
            (0.05, 0.95, true),
        ] {
            fx.snap.cpu.scaled = scaled;
            fx.snap.cpu.active = active;
            let text = line_text(&title(&fx.view(), &Theme::default()));
            assert_eq!(text.contains("busy"), shown, "{scaled} {active}: {text}");
        }
    }

    #[test]
    fn busy_rides_along_when_scaled_diverges() {
        let mut fx = fixture("");
        fx.ready = true;
        fx.snap.cpu.scaled = 0.18;
        fx.snap.cpu.active = 0.41;
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(text.contains("18%"), "{text}");
        assert!(text.contains("busy 41%"), "{text}");
    }

    #[test]
    fn title_marks_thermal_when_not_nominal() {
        let mut fx = fixture("");
        fx.ready = true;
        fx.snap.thermal = Thermal::Fair;
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(text.contains("fair"), "{text}");
        fx.snap.thermal = Thermal::Nominal;
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(!text.contains("nominal"), "{text}");
        assert!(!text.contains("fair"), "{text}");
    }

    #[test]
    fn tiny_temp_title_contains_degree() {
        let mut fx = fixture("");
        fx.ready = true;
        fx.snap.cpu.temp_c = Some(42.0);
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(text.contains("42°"), "{text}");
        fx.snap.cpu.temp_c = None;
        fx.snap.sensors.cpu_c = Some(38.0);
        let text = line_text(&title(&fx.view(), &Theme::default()));
        assert!(text.contains("38°"), "{text}");
    }

    #[test]
    fn ready_pct_matches_header() {
        assert_eq!(ready_pct(false, 0.5), "…");
        assert_eq!(ready_pct(true, 0.184), "18%");
    }
}
