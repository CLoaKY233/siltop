use ratatui::Frame;
use ratatui::layout::Rect;
use siltop_core::{Scale, SeriesId};

use crate::chrome::Axis;
use crate::layout::Panel;
use crate::theme::Theme;
use crate::widgets::mirror::{Mirror, render_mirror};
use crate::widgets::{AppView, frame_panel};

/// Download rises from the midline, upload hangs below it, on one scale.
pub(super) fn render(frame: &mut Frame, area: Rect, view: &AppView<'_>, theme: &Theme) {
    let mirror = Mirror {
        up: view.history(SeriesId::NetRx),
        down: view.history(SeriesId::NetTx),
        up_ramp: theme.net,
        down_ramp: theme.net,
        theme,
        scale: Scale::Auto { floor: 8_000.0 },
        axis: Axis::Net(view.net_unit),
        time: view.time_for(view.history(SeriesId::NetRx)),
    };
    let inner = frame_panel(frame, area, Panel::Net, view, theme, Some(&mirror));
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    render_mirror(frame, inner, mirror);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Line;

    fn title(view: &AppView<'_>, theme: &Theme) -> Line<'static> {
        crate::titles::net(view, theme).full(theme)
    }
    use crate::widgets::tests_support::fixture;

    #[test]
    fn title_shows_iface_and_rates() {
        let mut fx = fixture("");
        fx.snap.network.iface = String::from("en0");
        fx.snap.network.rx_bps = 12_400_000;
        fx.snap.network.tx_bps = 800;
        let text: String = title(&fx.view(), &Theme::default())
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.contains("net"));
        assert!(text.contains("en0"));
        assert!(text.contains("1.6MB/s"), "bytes by default: {text}");
        assert!(text.contains("100B/s"), "{text}");
        fx.net_unit = siltop_core::NetUnit::Bits;
        let text: String = title(&fx.view(), &Theme::default())
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            text.contains("12.4Mb/s") && text.contains("800b/s"),
            "{text}"
        );
    }
}
