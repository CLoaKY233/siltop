//! Expanded GPU: utilisation and temperature cells.

use siltop_core::{SeriesId, ready_pct, watts_display};

use super::cells::{graph_spec, live};
use super::{ID_GPU_TEMP, ID_GPU_UTIL};
use crate::widgets::AppView;
use crate::widgets::grid::Band;

pub(super) fn gpu_temp_live(view: &AppView<'_>) -> bool {
    live(
        view.snapshot
            .gpu
            .and_then(|g| g.temp_c)
            .or(view.snapshot.sensors.gpu_c),
        view.history(SeriesId::GpuTemp),
    )
}

pub(super) fn gpu_bands(view: &AppView<'_>) -> Vec<Band> {
    let Some(gpu) = view.snapshot.gpu else {
        return Vec::new();
    };
    let temp = gpu.temp_c.or(view.snapshot.sensors.gpu_c);
    vec![
        Band {
            take_leftover: true,
            ..Band::new(
                5,
                vec![graph_spec(
                    ID_GPU_UTIL,
                    "gpu",
                    Some(gpu_load_value(view, gpu)),
                    true,
                )],
            )
        },
        Band {
            take_leftover: true,
            ..Band::new(
                5,
                vec![graph_spec(
                    ID_GPU_TEMP,
                    "gpu temp",
                    temp.map(|c| format!("{c:.0}°")),
                    live(temp, view.history(SeriesId::GpuTemp)),
                )],
            )
        },
    ]
}

/// `12%  1.1W`: load and power share the gpu cell; the outer frame is context.
pub(super) fn gpu_load_value(view: &AppView<'_>, gpu: siltop_core::GpuSnapshot) -> String {
    let load = ready_pct(view.ready, gpu.scaled);
    match gpu.watts.filter(|w| view.ready && *w > 0.0) {
        Some(w) => format!("{load}  {}", watts_display(w)),
        None => load,
    }
}
