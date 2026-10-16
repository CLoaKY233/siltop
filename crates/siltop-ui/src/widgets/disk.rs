use ratatui::Frame;
use ratatui::layout::Rect;
use siltop_core::{Scale, SeriesId};

use crate::chrome::{Axis, render_fill_bar};
use crate::layout::Panel;
use crate::theme::Theme;
use crate::widgets::mirror::{Mirror, render_mirror};
use crate::widgets::{AppView, frame_panel};

/// Reads rise from the midline, writes hang below it, on one scale.
pub(super) fn render(frame: &mut Frame, area: Rect, view: &AppView<'_>, theme: &Theme) {
    let disk = &view.snapshot.disk;
    let mirror = Mirror {
        up: view.history(SeriesId::DiskRead),
        down: view.history(SeriesId::DiskWrite),
        up_ramp: theme.disk,
        down_ramp: theme.disk,
        theme,
        scale: Scale::Auto { floor: 1_024.0 },
        axis: Axis::Bytes,
        time: view.time_for(view.history(SeriesId::DiskRead)),
    };
    // A one-row frame shows the capacity bar, which has no rate scale.
    let shown = (!disk.volumes.is_empty() && area.height >= 4).then_some(&mirror);
    let inner = frame_panel(frame, area, Panel::Disk, view, theme, shown);
    if inner.width == 0 || inner.height == 0 || disk.volumes.is_empty() {
        return;
    }
    if inner.height >= 2 {
        render_mirror(frame, inner, mirror);
    } else {
        render_fill_bar(frame, inner, disk.used_ratio(), theme.disk, theme);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Line;
    use siltop_core::DiskSnapshot;

    fn title(disk: &DiskSnapshot, theme: &Theme) -> Line<'static> {
        let mut fx = crate::widgets::tests_support::fixture("");
        fx.snap.disk = disk.clone();
        crate::titles::disk(&fx.view(), theme).full(theme)
    }
    use siltop_core::DiskVolume;

    #[test]
    fn title_dash_when_empty() {
        let text: String = title(&DiskSnapshot::default(), &Theme::default())
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.contains("disk"));
        assert!(text.contains('—'));
    }

    #[test]
    fn title_primary_volume() {
        let disk = DiskSnapshot {
            volumes: vec![DiskVolume {
                name: String::from("Macintosh HD"),
                mount: String::from("/"),
                used_bytes: 400 * 1024 * 1024 * 1024,
                total_bytes: 926 * 1024 * 1024 * 1024,
            }],
            read_bps: 0,
            write_bps: 0,
            devices: Vec::new(),
        };
        let text: String = title(&disk, &Theme::default())
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        // Capacity lives in the expanded view; the compact title is I/O.
        assert!(!text.contains("Macintosh HD"), "{text}");
        assert!(text.contains("r ") && text.contains("w "), "{text}");
        assert!(!text.contains("B/s"), "idle rates print a dim 0: {text}");
    }
}
