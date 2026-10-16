//! The meta column: identity lines, hop rows and their hit-testing.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use siltop_core::{Scale, SeriesId, bytes_short, watts_display};

use super::cells::{paint_dim_line, paint_series, paint_temp};
use super::cpu::{cpu_has_spark_hops, has_any_cluster, paint_cores};
use super::disk::paint_volumes;
use super::gpu::gpu_temp_live;
use super::heat::{any_cpu_zone, any_sens_graph, paint_fan_hop};
use super::{
    ID_HOP_CPU, ID_HOP_DISK, ID_HOP_FAN, ID_HOP_GPU, ID_HOP_NET, ID_HOP_POWER, ID_HOP_SENS,
};
use crate::chrome::{Axis, GraphInk};
use crate::layout::Panel;
use crate::theme::Theme;
use crate::widgets::grid::CellKind;
use crate::widgets::{AppView, identity};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HopStyle {
    Absent,
    Spark,
    LabelOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MetaBudget {
    pub(super) hops: HopStyle,
    pub(super) mosaic: bool,
    pub(super) identity: bool,
    pub(super) extras: bool,
    pub(super) volumes: bool,
}

impl MetaBudget {
    fn is_empty(self) -> bool {
        self.hops == HopStyle::Absent
            && !self.mosaic
            && !self.identity
            && !self.extras
            && !self.volumes
    }

    fn hops_h(self) -> u16 {
        match self.hops {
            HopStyle::Spark => 3,
            HopStyle::LabelOnly => 1,
            HopStyle::Absent => 0,
        }
    }
}

pub(super) const META_COL_MIN_WIDTH: u16 = 100;
pub(super) const META_COL_WIDTH: u16 = 26;

pub(super) const META_COL_GAP: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MetaSide {
    Left,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MetaPlan {
    pub(super) main: Rect,
    pub(super) meta: Option<Rect>,
    pub(super) side: MetaSide,
}

pub(super) fn split_meta(inner: Rect, budget: MetaBudget, panel_mins: u16) -> MetaPlan {
    if inner.width == 0 || inner.height == 0 || budget.is_empty() {
        return MetaPlan {
            main: inner,
            meta: None,
            side: MetaSide::Bottom,
        };
    }
    let hops_h = budget.hops_h();
    let col_w = META_COL_WIDTH.saturating_add(META_COL_GAP);
    if inner.width >= META_COL_MIN_WIDTH
        && inner.width >= col_w.saturating_add(16)
        && (budget.hops == HopStyle::Spark || budget.mosaic)
        && inner.height >= hops_h.max(1)
    {
        return MetaPlan {
            main: Rect {
                x: inner.x.saturating_add(col_w),
                width: inner.width.saturating_sub(col_w),
                ..inner
            },
            meta: Some(Rect {
                width: META_COL_WIDTH,
                height: inner.height,
                ..inner
            }),
            side: MetaSide::Left,
        };
    }

    let mut height = 0_u16;
    if budget.mosaic && inner.height >= panel_mins.saturating_add(hops_h).saturating_add(5) {
        height = height.saturating_add(5);
    }
    if budget.hops == HopStyle::Spark && inner.height >= panel_mins.saturating_add(3) {
        height = height.saturating_add(3);
    } else if budget.volumes
        && budget.hops == HopStyle::Absent
        && !budget.mosaic
        && inner.height >= panel_mins.saturating_add(4)
    {
        height = height.saturating_add(4);
    } else if budget.hops == HopStyle::LabelOnly
        && !budget.mosaic
        && !budget.volumes
        && inner.height >= panel_mins.saturating_add(1)
    {
        height = height.saturating_add(1);
    }
    let max_steal = inner.height.saturating_sub(panel_mins);
    height = height.min(max_steal);
    if height == 0 {
        return MetaPlan {
            main: inner,
            meta: None,
            side: MetaSide::Bottom,
        };
    }
    let main_h = inner.height.saturating_sub(height);
    MetaPlan {
        main: Rect {
            height: main_h,
            ..inner
        },
        meta: Some(Rect {
            x: inner.x,
            y: inner.y.saturating_add(main_h),
            width: inner.width,
            height,
        }),
        side: MetaSide::Bottom,
    }
}

pub(super) fn panel_mins(panel: Panel, view: &AppView<'_>) -> u16 {
    match panel {
        Panel::Cpu => {
            let mut n = 4_u16;
            if any_cpu_zone(view) {
                n = n.saturating_add(5);
            }
            n
        }
        Panel::Gpu => {
            let mut n = 5_u16;
            if gpu_temp_live(view) {
                n = n.saturating_add(5);
            }
            n
        }
        Panel::Fans => {
            let mut n = 0_u16;
            if any_sens_graph(view) {
                n = n.saturating_add(5);
            }
            if view.snapshot.fans.is_present() {
                n = n.saturating_add(5);
            }
            n
        }
        Panel::Mem | Panel::Net | Panel::Disk | Panel::Power => 5,
        Panel::Processes => 0,
    }
}

pub(super) fn meta_budget(panel: Panel, view: &AppView<'_>) -> MetaBudget {
    match panel {
        Panel::Cpu => MetaBudget {
            hops: if cpu_has_spark_hops(view) {
                HopStyle::Spark
            } else {
                HopStyle::Absent
            },
            mosaic: view.show_cores && has_any_cluster(view),
            identity: false,
            extras: false,
            volumes: false,
        },
        Panel::Gpu | Panel::Fans | Panel::Power => MetaBudget {
            hops: HopStyle::Spark,
            mosaic: false,
            identity: false,
            extras: false,
            volumes: false,
        },
        Panel::Mem => MetaBudget {
            hops: HopStyle::LabelOnly,
            mosaic: false,
            identity: false,
            extras: view.snapshot.memory.swap_used_bytes > 0
                || view.snapshot.memory.cache_bytes > 0,
            volumes: false,
        },
        Panel::Net => MetaBudget {
            hops: if view.show_disk && view.flags().has_disk {
                HopStyle::Spark
            } else {
                HopStyle::Absent
            },
            mosaic: false,
            identity: false,
            extras: false,
            volumes: false,
        },
        Panel::Disk => MetaBudget {
            hops: if view.show_net {
                HopStyle::Spark
            } else {
                HopStyle::Absent
            },
            mosaic: false,
            identity: false,
            extras: false,
            volumes: !view.snapshot.disk.volumes.is_empty(),
        },
        Panel::Processes => MetaBudget {
            hops: HopStyle::Absent,
            mosaic: false,
            identity: false,
            extras: false,
            volumes: false,
        },
    }
}

pub(super) fn hop_targets(panel: Panel, view: &AppView<'_>) -> Vec<(u8, Panel)> {
    match panel {
        Panel::Cpu => {
            let mut hops = Vec::new();
            if view.show_gpu && view.snapshot.gpu.is_some() {
                hops.push((ID_HOP_GPU, Panel::Gpu));
            }
            if view.show_fans && view.snapshot.fans.is_present() {
                hops.push((ID_HOP_FAN, Panel::Fans));
            }
            hops
        }
        Panel::Gpu => {
            let mut hops = vec![(ID_HOP_CPU, Panel::Cpu)];
            if view.show_fans && view.flags().has_fans {
                hops.push((ID_HOP_SENS, Panel::Fans));
            }
            hops
        }
        Panel::Fans => {
            let mut hops = vec![(ID_HOP_CPU, Panel::Cpu)];
            if view.show_gpu && view.snapshot.gpu.is_some() {
                hops.push((ID_HOP_GPU, Panel::Gpu));
            }
            if view.flags().visible(Panel::Power) {
                hops.push((ID_HOP_POWER, Panel::Power));
            }
            hops
        }
        Panel::Power => {
            let mut hops = Vec::new();
            if view.flags().visible(Panel::Fans) {
                hops.push((ID_HOP_SENS, Panel::Fans));
            }
            hops.push((ID_HOP_CPU, Panel::Cpu));
            hops
        }
        Panel::Net if view.show_disk && view.flags().has_disk => {
            vec![(ID_HOP_DISK, Panel::Disk)]
        }
        Panel::Disk if view.show_net => vec![(ID_HOP_NET, Panel::Net)],
        _ => Vec::new(),
    }
}

pub(super) fn paint_meta(
    frame: &mut Frame,
    area: Rect,
    side: MetaSide,
    panel: Panel,
    view: &AppView<'_>,
    theme: &Theme,
) {
    match side {
        MetaSide::Left => paint_meta_left(frame, area, panel, view, theme),
        MetaSide::Bottom => paint_meta_bottom(frame, area, panel, view, theme),
    }
}

pub(super) fn paint_meta_bottom(
    frame: &mut Frame,
    area: Rect,
    panel: Panel,
    view: &AppView<'_>,
    theme: &Theme,
) {
    if panel == Panel::Mem {
        paint_label_hop(frame, area, "proc", Panel::Processes, theme);
        return;
    }
    if panel == Panel::Disk && hop_targets(panel, view).is_empty() {
        paint_volumes(frame, area, view, theme);
        return;
    }
    let hops = hop_targets(panel, view);
    let hop_h = if hops.is_empty() {
        0
    } else {
        3.min(area.height)
    };
    if hop_h > 0 {
        paint_hop_row(
            frame,
            Rect {
                height: hop_h,
                ..area
            },
            &hops,
            view,
            theme,
        );
    }
    if view.show_cores && panel == Panel::Cpu && area.height > hop_h {
        paint_cores(
            frame,
            Rect {
                y: area.y.saturating_add(hop_h),
                height: area.height.saturating_sub(hop_h),
                ..area
            },
            view,
            theme,
        );
    }
}

pub(super) struct LeftSlots {
    identity: u16,
    hop_rects: Vec<(u8, Panel, Rect)>,
    cores: Option<Rect>,
}

pub(super) fn left_slots(area: Rect, panel: Panel, view: &AppView<'_>) -> LeftSlots {
    let hops = hop_targets(panel, view);
    let ident_n = u16::try_from(identity_lines(panel, view).len()).unwrap_or(0);
    let hop_n = u16::try_from(hops.len()).unwrap_or(0);
    let hop_min = 3u16.saturating_mul(hop_n);
    let cores_want = if view.show_cores && panel == Panel::Cpu {
        u16::try_from(view.snapshot.cpu.cores.len()).unwrap_or(0)
    } else {
        0
    };
    let cores_min = u16::from(cores_want > 0);
    let keep_after_ident = hop_min.saturating_add(cores_min);
    let ident = ident_n.min(area.height.saturating_sub(keep_after_ident));
    let rest = area.height.saturating_sub(ident);
    let cores_h = cores_want.min(rest.saturating_sub(hop_min));
    let hop_space = rest.saturating_sub(cores_h);
    let mut hop_rects = Vec::new();
    let mut y = area.y.saturating_add(ident);
    if hop_n > 0 && hop_space >= 3 {
        let base = hop_space / hop_n;
        let extra = hop_space % hop_n;
        for (i, (id, hop)) in hops.into_iter().enumerate() {
            let h = base.saturating_add(u16::from(i < extra as usize)).max(3);
            let h = h.min(area.y.saturating_add(area.height).saturating_sub(y));
            if h < 3 {
                break;
            }
            hop_rects.push((
                id,
                hop,
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: h,
                },
            ));
            y = y.saturating_add(h);
        }
    }
    let cores = if cores_h > 0 {
        Some(Rect {
            x: area.x,
            y,
            width: area.width,
            height: cores_h,
        })
    } else {
        None
    };
    LeftSlots {
        identity: ident,
        hop_rects,
        cores,
    }
}

pub(super) fn paint_meta_left(
    frame: &mut Frame,
    area: Rect,
    panel: Panel,
    view: &AppView<'_>,
    theme: &Theme,
) {
    let slots = left_slots(area, panel, view);
    for (i, line) in identity_lines(panel, view)
        .into_iter()
        .take(usize::from(slots.identity))
        .enumerate()
    {
        let y = area.y.saturating_add(u16::try_from(i).unwrap_or(0));
        paint_dim_line(frame, area.x, y, area.width, line, theme);
    }
    for (id, hop, rect) in slots.hop_rects {
        paint_one_hop(frame, rect, id, hop, view, theme);
    }
    if let Some(cores) = slots.cores {
        paint_cores(frame, cores, view, theme);
    }
}

/// The meta column's facts. The CPU view is the machine identity's one
/// home on screen (chip, core split, GPU cores, memory); the GPU view keeps
/// its clocks here and its core count in the title.
pub(super) fn identity_lines(panel: Panel, view: &AppView<'_>) -> Vec<String> {
    let mut lines = Vec::new();
    match panel {
        Panel::Cpu => {
            lines.extend(identity::facts(view.snapshot).into_iter().map(|(_, v)| v));
        }
        Panel::Gpu => {
            if let Some(gpu) = view.snapshot.gpu {
                if let Some(mhz) = gpu.freq_mhz.filter(|mhz| *mhz > 0) {
                    if mhz >= 1000 {
                        lines.push(format!("{:.1}GHz", f64::from(mhz) / 1000.0));
                    } else {
                        lines.push(format!("{mhz}MHz"));
                    }
                }
                if let Some(watts) = gpu.ane_watts.filter(|w| *w > 0.0) {
                    lines.push(format!("ane {}", watts_display(watts)));
                }
                lines.extend(gpu_memory_line(&gpu));
            }
        }
        _ => {}
    }
    lines
}

/// `mem 1.2G of 5.3G`: GPU memory in use against what is allocated to it.
pub(super) fn gpu_memory_line(gpu: &siltop_core::GpuSnapshot) -> Option<String> {
    let used = gpu.mem_in_use_bytes.filter(|b| *b > 0)?;
    Some(match gpu.mem_alloc_bytes.filter(|b| *b > 0) {
        Some(budget) => format!("mem {} of {}", bytes_short(used), bytes_short(budget)),
        None => format!("mem {}", bytes_short(used)),
    })
}

pub(super) fn hop_row_rects(area: Rect, n: usize) -> Vec<Rect> {
    if n == 0 || area.width == 0 {
        return Vec::new();
    }
    let constraints: Vec<Constraint> = (0..n).map(|_| Constraint::Fill(1)).collect();
    Layout::horizontal(constraints).split(area).to_vec()
}

pub(super) fn hop_row_hit(area: Rect, hops: &[(u8, Panel)], col: u16, row: u16) -> Option<Panel> {
    if row < area.y || row >= area.y.saturating_add(area.height) {
        return None;
    }
    hop_row_rects(area, hops.len())
        .into_iter()
        .zip(hops.iter())
        .find(|(rect, _)| crate::layout::contains(*rect, col, row))
        .map(|(_, (_, panel))| *panel)
}

pub(super) fn mem_label_hit(area: Rect, col: u16, row: u16) -> Option<Panel> {
    let line = Rect { height: 1, ..area };
    if crate::layout::contains(line, col, row) {
        Some(Panel::Processes)
    } else {
        None
    }
}

pub(super) fn paint_hop_row(
    frame: &mut Frame,
    area: Rect,
    hops: &[(u8, Panel)],
    view: &AppView<'_>,
    theme: &Theme,
) {
    for ((id, hop), col) in hops.iter().zip(hop_row_rects(area, hops.len())) {
        paint_one_hop(frame, col, *id, *hop, view, theme);
    }
}

pub(super) fn paint_one_hop(
    frame: &mut Frame,
    area: Rect,
    id: u8,
    hop: Panel,
    view: &AppView<'_>,
    theme: &Theme,
) {
    let cell = crate::widgets::grid::Placed {
        id,
        kind: CellKind::Spark,
        rect: area,
        hop: Some(hop),
    };
    match id {
        ID_HOP_FAN => paint_fan_hop(frame, &cell, view, theme),
        ID_HOP_CPU => paint_series(
            frame,
            &cell,
            view,
            view.history(SeriesId::Cpu),
            theme.cpu,
            GraphInk::Signal(view.snapshot.thermal),
            Scale::LOAD,
            Axis::Percent,
            theme,
        ),
        ID_HOP_GPU => paint_series(
            frame,
            &cell,
            view,
            view.history(SeriesId::Gpu),
            theme.gpu,
            GraphInk::Load(view.snapshot.thermal),
            Scale::LOAD,
            Axis::Percent,
            theme,
        ),
        ID_HOP_SENS => paint_temp(frame, &cell, view, view.history(SeriesId::CpuTemp), theme),
        ID_HOP_POWER => paint_series(
            frame,
            &cell,
            view,
            view.history(SeriesId::PowerSystem),
            theme.fan,
            GraphInk::Flat,
            crate::widgets::power::WATTS_SCALE,
            Axis::Watts,
            theme,
        ),
        ID_HOP_DISK => paint_series(
            frame,
            &cell,
            view,
            view.history(SeriesId::Disk),
            theme.disk,
            GraphInk::Flat,
            Scale::Auto { floor: 1_024.0 },
            Axis::Bytes,
            theme,
        ),
        ID_HOP_NET => paint_series(
            frame,
            &cell,
            view,
            view.history(SeriesId::NetRx),
            theme.net,
            GraphInk::Flat,
            Scale::Auto { floor: 8_000.0 },
            Axis::Net(view.net_unit),
            theme,
        ),
        _ => {}
    }
}

pub(super) fn paint_label_hop(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    _hop: Panel,
    theme: &Theme,
) {
    if area.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {label}  "), theme.dim()),
            Span::styled("→", theme.dim()),
        ])),
        Rect { height: 1, ..area },
    );
}

pub(super) fn meta_hop_hit(
    area: Rect,
    side: MetaSide,
    panel: Panel,
    view: &AppView<'_>,
    col: u16,
    row: u16,
) -> Option<Panel> {
    if !crate::layout::contains(area, col, row) {
        return None;
    }
    if panel == Panel::Mem {
        return mem_label_hit(area, col, row);
    }
    let hops = hop_targets(panel, view);
    if hops.is_empty() {
        return None;
    }
    match side {
        MetaSide::Bottom => {
            let hop_h = 3.min(area.height);
            if row >= area.y.saturating_add(hop_h) {
                return None;
            }
            hop_row_hit(
                Rect {
                    height: hop_h,
                    ..area
                },
                &hops,
                col,
                row,
            )
        }
        MetaSide::Left => left_slots(area, panel, view)
            .hop_rects
            .into_iter()
            .find_map(|(_, hop, rect)| crate::layout::contains(rect, col, row).then_some(hop)),
    }
}
