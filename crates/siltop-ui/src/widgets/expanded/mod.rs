use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use siltop_core::{Scale, SeriesId};

use crate::chrome::{Axis, GraphInk, panel_block};
use crate::layout::Panel;
use crate::theme::Theme;
use crate::widgets::AppView;
use crate::widgets::grid::{Band, Pack, pack};

mod cells;
mod cpu;
mod disk;
mod gpu;
mod heat;
mod mem;
mod meta;
mod net;
mod power;

use cells::{paint_lines, paint_readings, paint_series, paint_temp};
use cpu::cpu_bands;
use disk::{disk_bands, paint_volumes};
use gpu::gpu_bands;
use heat::{extra_readings, paint_fan, sens_bands};
use mem::{mem_bands, paint_mem_parts, paint_swap};
use meta::{meta_budget, meta_hop_hit, paint_meta, panel_mins, split_meta};
use net::net_bands;
use power::power_bands;

const ID_CPU: u8 = 0;
const ID_SUPER_LOAD: u8 = 1;

const ID_PERF_LOAD: u8 = 2;
const ID_EFF_LOAD: u8 = 3;

const ID_GPU_UTIL: u8 = 4;
const ID_SUPER_ZONE: u8 = 10;

const ID_PERF_ZONE: u8 = 11;
const ID_EFF_ZONE: u8 = 12;

const ID_PACKAGE: u8 = 13;
const ID_GPU_TEMP: u8 = 14;

const ID_HOP_CPU: u8 = 20;
const ID_HOP_GPU: u8 = 21;

const ID_HOP_SENS: u8 = 22;
const ID_HOP_FAN: u8 = 23;

const ID_HOP_DISK: u8 = 25;

const ID_HOP_NET: u8 = 26;
const ID_FAN0: u8 = 40;

const ID_READINGS: u8 = 50;
const ID_MEM: u8 = 60;

const ID_MEM_PARTS: u8 = 61;
const ID_SWAP: u8 = 62;

const ID_NET_DOWN: u8 = 70;
const ID_NET_UP: u8 = 71;

const ID_DISK_READ: u8 = 80;
const ID_DISK_WRITE: u8 = 81;

const ID_VOLUMES: u8 = 82;
const ID_POWER_SYS: u8 = 90;

const ID_BAT_PCT: u8 = 91;
const ID_BAT_WATTS: u8 = 92;

const ID_POWER_FACTS: u8 = 93;
const ID_HOP_POWER: u8 = 27;

pub(super) fn render(
    frame: &mut Frame,
    area: Rect,
    view: &AppView<'_>,
    theme: &Theme,
    panel: Panel,
) {
    if panel == Panel::Processes {
        super::processes::render(frame, area, view, theme);
        return;
    }
    let block = panel_block(
        panel,
        crate::titles::context(panel, view, theme).fit(
            crate::titles::panel_budget(panel, area.width, 0, theme),
            theme,
        ),
        view.is_focused(panel),
        true,
        theme,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if panel == Panel::Gpu && view.snapshot.gpu.is_none() {
        frame.render_widget(
            Paragraph::new(Span::styled("no readings on this machine", theme.dim())),
            inner,
        );
        return;
    }
    let budget = meta_budget(panel, view);
    let mins = panel_mins(panel, view);
    let plan = split_meta(inner, budget, mins);
    let placed = pack(plan.main, &bands_for(panel, view));
    paint_pack(frame, &placed, view, theme);
    if let Some(meta) = plan.meta {
        paint_meta(frame, meta, plan.side, panel, view, theme);
    }
}

#[must_use]
pub fn hop_hit(area: Rect, view: &AppView<'_>, col: u16, row: u16) -> Option<Panel> {
    let panel = view.expanded?;
    if panel == Panel::Processes {
        return None;
    }
    let inner = inset(area);
    let budget = meta_budget(panel, view);
    let plan = split_meta(inner, budget, panel_mins(panel, view));
    if let Some(meta) = plan.meta
        && let Some(hit) = meta_hop_hit(meta, plan.side, panel, view, col, row)
    {
        return Some(hit);
    }
    pack(plan.main, &bands_for(panel, view)).hop_at(col, row)
}

fn inset(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
}

fn bands_for(panel: Panel, view: &AppView<'_>) -> Vec<Band> {
    match panel {
        Panel::Cpu => cpu_bands(view),
        Panel::Gpu => gpu_bands(view),
        Panel::Fans => sens_bands(view),
        Panel::Mem => mem_bands(view),
        Panel::Net => net_bands(view),
        Panel::Disk => disk_bands(view),
        Panel::Power => power_bands(view),
        Panel::Processes => Vec::new(),
    }
}

#[allow(clippy::too_many_lines)]
fn paint_pack(frame: &mut Frame, placed: &Pack, view: &AppView<'_>, theme: &Theme) {
    for cell in &placed.cells {
        match cell.id {
            ID_GPU_UTIL => paint_series(
                frame,
                cell,
                view,
                view.history(SeriesId::Gpu),
                theme.gpu,
                GraphInk::Load(view.snapshot.thermal),
                Scale::LOAD,
                Axis::Percent,
                theme,
            ),
            ID_CPU => paint_series(
                frame,
                cell,
                view,
                view.history(SeriesId::Cpu),
                theme.cpu,
                GraphInk::Signal(view.snapshot.thermal),
                Scale::LOAD,
                Axis::Percent,
                theme,
            ),
            ID_SUPER_LOAD => paint_series(
                frame,
                cell,
                view,
                view.history(SeriesId::SLoad),
                theme.cpu,
                GraphInk::Load(view.snapshot.thermal),
                Scale::LOAD,
                Axis::Percent,
                theme,
            ),
            ID_PERF_LOAD => paint_series(
                frame,
                cell,
                view,
                view.history(SeriesId::PLoad),
                theme.cpu,
                GraphInk::Load(view.snapshot.thermal),
                Scale::LOAD,
                Axis::Percent,
                theme,
            ),
            ID_EFF_LOAD => paint_series(
                frame,
                cell,
                view,
                view.history(SeriesId::ELoad),
                theme.cpu,
                GraphInk::Load(view.snapshot.thermal),
                Scale::LOAD,
                Axis::Percent,
                theme,
            ),
            ID_SUPER_ZONE => paint_temp(frame, cell, view, view.history(SeriesId::STemp), theme),
            ID_PERF_ZONE => paint_temp(frame, cell, view, view.history(SeriesId::PTemp), theme),
            ID_EFF_ZONE => paint_temp(frame, cell, view, view.history(SeriesId::ETemp), theme),
            ID_PACKAGE => paint_temp(frame, cell, view, view.history(SeriesId::CpuTemp), theme),
            ID_GPU_TEMP => paint_temp(frame, cell, view, view.history(SeriesId::GpuTemp), theme),
            ID_MEM => paint_series(
                frame,
                cell,
                view,
                view.history(SeriesId::Mem),
                theme.mem,
                GraphInk::Load(view.snapshot.thermal),
                Scale::Fixed(1.0),
                Axis::Percent,
                theme,
            ),
            ID_NET_DOWN => paint_series(
                frame,
                cell,
                view,
                view.history(SeriesId::NetRx),
                theme.net,
                GraphInk::Flat,
                Scale::Auto { floor: 8_000.0 },
                Axis::Net(view.net_unit),
                theme,
            ),
            ID_NET_UP => paint_series(
                frame,
                cell,
                view,
                view.history(SeriesId::NetTx),
                theme.net_up,
                GraphInk::Flat,
                Scale::Auto { floor: 8_000.0 },
                Axis::Net(view.net_unit),
                theme,
            ),
            ID_DISK_READ => paint_series(
                frame,
                cell,
                view,
                view.history(SeriesId::DiskRead),
                theme.disk,
                GraphInk::Flat,
                Scale::Auto { floor: 1_024.0 },
                Axis::Bytes,
                theme,
            ),
            ID_DISK_WRITE => paint_series(
                frame,
                cell,
                view,
                view.history(SeriesId::DiskWrite),
                theme.disk_write,
                GraphInk::Flat,
                Scale::Auto { floor: 1_024.0 },
                Axis::Bytes,
                theme,
            ),
            ID_VOLUMES => paint_volumes(frame, cell.rect, view, theme),
            ID_POWER_SYS => paint_series(
                frame,
                cell,
                view,
                view.history(SeriesId::PowerSystem),
                theme.fan,
                GraphInk::Flat,
                crate::widgets::power::WATTS_SCALE,
                Axis::Watts,
                theme,
            ),
            ID_BAT_PCT => paint_series(
                frame,
                cell,
                view,
                view.history(SeriesId::BatteryPct),
                theme.fan,
                GraphInk::Flat,
                crate::widgets::power::CHARGE_SCALE,
                Axis::Percent,
                theme,
            ),
            ID_BAT_WATTS => paint_series(
                frame,
                cell,
                view,
                view.history(SeriesId::BatteryWatts),
                theme.fan,
                GraphInk::Flat,
                crate::widgets::power::WATTS_SCALE,
                Axis::Watts,
                theme,
            ),
            ID_POWER_FACTS => paint_lines(
                frame,
                cell.rect,
                "details",
                &crate::widgets::power::facts(view.snapshot),
                theme,
            ),
            ID_READINGS => paint_readings(frame, cell.rect, &extra_readings(view), theme),
            id if (ID_FAN0..ID_FAN0 + 4).contains(&id) => {
                let i = usize::from(id - ID_FAN0);
                if let Some(fan) = view.snapshot.fans.fans.get(i) {
                    paint_fan(frame, cell, view, fan, view.fan_histories().get(i), theme);
                }
            }
            ID_MEM_PARTS => paint_mem_parts(frame, cell.rect, view, theme),
            ID_SWAP => paint_swap(frame, cell, view, theme),
            _ => {}
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::ID_SWAP;
    use super::cells::pack_items;
    use super::cpu::cluster_value;
    use super::heat::peak_fan_index;
    use super::mem::{mem_bands, mem_value};
    use crate::layout::Panel;
    use crate::widgets::tests_support::fixture;
    use ratatui::layout::Rect;
    use ratatui::text::{Line, Span};
    use siltop_core::ClusterKind;
    use siltop_core::SeriesId;
    use siltop_core::{FanMetric, FanSnapshot};

    fn fan(rpm: u16, max_rpm: u16) -> FanMetric {
        FanMetric {
            name: String::from("Fan"),
            rpm,
            max_rpm,
        }
    }

    #[test]
    fn peak_fan_index_picks_highest_rpm_then_lowest_index() {
        let mut fx = fixture("");
        fx.snap.fans = FanSnapshot {
            fans: vec![fan(1200, 6000), fan(2140, 6000), fan(2140, 6000)],
        };
        let view = fx.view();
        assert_eq!(peak_fan_index(&view), Some(1));
    }

    #[test]
    fn peak_fan_idle_zero_is_present() {
        let mut fx = fixture("");
        fx.snap.fans = FanSnapshot {
            fans: vec![fan(0, 6000)],
        };
        let view = fx.view();
        assert_eq!(peak_fan_index(&view), Some(0));
    }

    #[test]
    fn peak_fan_absent_when_no_hardware() {
        let fx = fixture("");
        assert!(peak_fan_index(&fx.view()).is_none());
    }

    #[test]
    fn sensors_without_fans_do_not_open_a_fan_hop() {
        let mut fx = fixture("");
        fx.expanded = Some(Panel::Cpu);
        fx.snap.sensors.cpu_c = Some(42.0);
        fx.snap.fans = FanSnapshot { fans: Vec::new() };
        let hops = super::meta::hop_targets(Panel::Cpu, &fx.view());
        assert!(!hops.iter().any(|(id, _)| *id == super::ID_HOP_FAN));
    }

    #[test]
    fn split_meta_78x13_hops_only() {
        let budget = super::meta::MetaBudget {
            hops: super::meta::HopStyle::Spark,
            mosaic: true,
            identity: false,
            extras: false,
            volumes: false,
        };
        let plan = super::meta::split_meta(Rect::new(0, 0, 78, 13), budget, 9);
        assert_eq!(plan.main.height, 10);
        assert_eq!(plan.meta.map(|r| r.height), Some(3));
    }

    #[test]
    fn split_meta_78x16_still_no_mosaic() {
        let budget = super::meta::MetaBudget {
            hops: super::meta::HopStyle::Spark,
            mosaic: true,
            identity: false,
            extras: false,
            volumes: false,
        };
        let plan = super::meta::split_meta(Rect::new(0, 0, 78, 16), budget, 9);
        assert_eq!(plan.meta.map(|r| r.height), Some(3));
        assert_eq!(plan.main.height, 13);
    }

    #[test]
    fn split_meta_78x17_mosaic_exact() {
        let budget = super::meta::MetaBudget {
            hops: super::meta::HopStyle::Spark,
            mosaic: true,
            identity: false,
            extras: false,
            volumes: false,
        };
        let plan = super::meta::split_meta(Rect::new(0, 0, 78, 17), budget, 9);
        assert_eq!(plan.meta.map(|r| r.height), Some(8));
        assert_eq!(plan.main.height, 9);
    }

    #[test]
    fn pack_cpu_main_78x13_reference() {
        use crate::widgets::grid::pack;
        let mut fx = fixture("");
        fx.snap.cpu.cores = vec![
            siltop_core::CoreSample {
                kind: siltop_core::ClusterKind::Super,
                index: 0,
                scaled: 0.8,
                active: 0.8,
                freq_mhz: None,
            },
            siltop_core::CoreSample {
                kind: siltop_core::ClusterKind::Performance,
                index: 0,
                scaled: 0.4,
                active: 0.4,
                freq_mhz: None,
            },
        ];
        fx.snap.sensors.s_c = Some(71.0);
        fx.snap.sensors.p_c = Some(62.0);
        fx.histories.get_mut(SeriesId::STemp).push(71.0);
        fx.histories.get_mut(SeriesId::PTemp).push(62.0);
        let bands = super::cpu::cpu_bands(&fx.view());
        let packed = pack(Rect::new(0, 0, 78, 13), &bands);
        let usage = packed.get(super::ID_SUPER_LOAD).expect("super");
        let zone = packed.get(super::ID_SUPER_ZONE).expect("super zone");
        assert!(
            usage.rect.height > 4,
            "usage must grow with leftover, got {}",
            usage.rect.height
        );
        assert_eq!(usage.rect.height.saturating_add(zone.rect.height), 13);
        assert_eq!(usage.rect.y, 0);
        assert_eq!(zone.rect.y, usage.rect.height);
        assert!(packed.get(super::ID_CPU).is_none());
        assert!(packed.get(super::ID_PACKAGE).is_none());
    }

    #[test]
    fn split_meta_wide_opens_a_left_column() {
        let budget = super::meta::MetaBudget {
            hops: super::meta::HopStyle::Spark,
            mosaic: true,
            identity: false,
            extras: false,
            volumes: false,
        };
        let plan = super::meta::split_meta(Rect::new(0, 0, 160, 40), budget, 9);
        assert_eq!(plan.side, super::meta::MetaSide::Left);
        let meta = plan.meta.expect("left column");
        assert_eq!(meta.x, 0);
        assert_eq!(meta.width, 26);
        assert_eq!(meta.height, 40);
        assert_eq!(plan.main.x, 27);
        assert_eq!(plan.main.width, 133);
        assert_eq!(plan.main.height, 40, "graphs keep the full height");
    }

    #[test]
    fn split_meta_narrow_stays_a_bottom_strip() {
        let budget = super::meta::MetaBudget {
            hops: super::meta::HopStyle::Spark,
            mosaic: true,
            identity: false,
            extras: false,
            volumes: false,
        };
        let plan = super::meta::split_meta(Rect::new(0, 0, 78, 21), budget, 9);
        assert_eq!(plan.side, super::meta::MetaSide::Bottom);
        assert_eq!(plan.main.width, 78);
        assert_eq!(plan.meta.map(|r| r.height), Some(8));
    }

    #[test]
    fn pack_cpu_shares_leftover_on_a_tall_pane() {
        use crate::widgets::grid::pack;
        let mut fx = fixture("");
        fx.snap.cpu.cores = vec![
            siltop_core::CoreSample {
                kind: siltop_core::ClusterKind::Super,
                index: 0,
                scaled: 0.8,
                active: 0.8,
                freq_mhz: None,
            },
            siltop_core::CoreSample {
                kind: siltop_core::ClusterKind::Performance,
                index: 0,
                scaled: 0.4,
                active: 0.4,
                freq_mhz: None,
            },
        ];
        fx.snap.sensors.s_c = Some(71.0);
        fx.snap.sensors.p_c = Some(62.0);
        fx.histories.get_mut(SeriesId::STemp).push(71.0);
        fx.histories.get_mut(SeriesId::PTemp).push(62.0);
        let packed = pack(Rect::new(0, 0, 78, 30), &super::cpu::cpu_bands(&fx.view()));
        let usage = packed.get(super::ID_SUPER_LOAD).expect("super");
        let zone = packed.get(super::ID_SUPER_ZONE).expect("super zone");
        assert_eq!(usage.rect.height.saturating_add(zone.rect.height), 30);
        assert!(usage.rect.height >= 10, "got {}", usage.rect.height);
        assert!(zone.rect.height >= 10, "got {}", zone.rect.height);
    }

    #[test]
    fn pack_cpu_without_zones_fills_a_tall_pane() {
        use crate::widgets::grid::pack;
        let mut fx = fixture("");
        fx.snap.cpu.cores = vec![siltop_core::CoreSample {
            kind: siltop_core::ClusterKind::Performance,
            index: 0,
            scaled: 0.4,
            active: 0.4,
            freq_mhz: None,
        }];
        fx.snap.sensors.e_c = None;
        fx.snap.sensors.p_c = None;
        fx.snap.sensors.s_c = None;
        fx.snap.sensors.cpu_c = None;
        fx.snap.cpu.temp_c = None;
        let packed = pack(Rect::new(0, 0, 130, 40), &super::cpu::cpu_bands(&fx.view()));
        let usage = packed.get(super::ID_PERF_LOAD).expect("performance");
        assert_eq!(usage.rect.height, 40, "usage must fill when heat is absent");
        assert!(packed.get(super::ID_SUPER_ZONE).is_none());
        assert!(packed.get(super::ID_PACKAGE).is_none());
    }

    #[test]
    fn pack_gpu_without_temp_fills_a_tall_pane() {
        use crate::widgets::grid::pack;
        let mut fx = fixture("");
        fx.snap.gpu = Some(siltop_core::GpuSnapshot {
            scaled: 0.16,
            temp_c: None,
            ..siltop_core::GpuSnapshot::default()
        });
        fx.snap.sensors.gpu_c = None;
        let packed = pack(Rect::new(0, 0, 130, 40), &super::gpu::gpu_bands(&fx.view()));
        let util = packed.get(super::ID_GPU_UTIL).expect("util");
        assert_eq!(util.rect.height, 40);
        assert!(packed.get(super::ID_GPU_TEMP).is_none());
    }

    #[test]
    fn pack_gpu_shares_height_between_util_and_temp() {
        use crate::widgets::grid::pack;
        let mut fx = fixture("");
        fx.snap.gpu = Some(siltop_core::GpuSnapshot {
            scaled: 0.16,
            temp_c: Some(51.0),
            ..siltop_core::GpuSnapshot::default()
        });
        fx.snap.sensors.gpu_c = Some(51.0);
        fx.histories.get_mut(SeriesId::Gpu).push(0.16);
        fx.histories.get_mut(SeriesId::GpuTemp).push(51.0);
        let packed = pack(Rect::new(0, 0, 130, 30), &super::gpu::gpu_bands(&fx.view()));
        let util = packed.get(super::ID_GPU_UTIL).expect("util");
        let temp = packed.get(super::ID_GPU_TEMP).expect("temp");
        assert_eq!(util.rect.height.saturating_add(temp.rect.height), 30);
        let gap = util.rect.height.abs_diff(temp.rect.height);
        assert!(
            gap <= 1,
            "util {} vs temp {}",
            util.rect.height,
            temp.rect.height
        );
    }

    #[test]
    fn cluster_cells_show_their_clock_once_known() {
        let mut fx = fixture("");
        fx.ready = true;
        fx.snap.cpu.s_cluster = Some(siltop_core::Cluster {
            kind: ClusterKind::Super,
            scaled: 0.75,
            active: 0.9,
            freq_mhz: 3_512,
        });
        fx.snap.cpu.p_cluster = Some(siltop_core::Cluster {
            kind: ClusterKind::Performance,
            scaled: 0.88,
            active: 0.9,
            freq_mhz: 0,
        });
        let view = fx.view();
        assert_eq!(cluster_value(&view, ClusterKind::Super), "75% 3.5G");
        assert_eq!(
            cluster_value(&view, ClusterKind::Performance),
            "88%",
            "0 MHz is unknown"
        );
        fx.ready = false;
        let view = fx.view();
        assert_eq!(cluster_value(&view, ClusterKind::Super), "75%");
    }

    #[test]
    fn overflow_rows_table() {
        use super::cells::overflow_rows;
        // (cores, rows) -> (shown, hidden); shown + hidden is always cores.
        for (cores, rows, want) in [
            (0, 0, (0, 0)),
            (0, 5, (0, 0)),
            (18, 18, (18, 0)),
            (18, 40, (18, 0)),
            (18, 5, (4, 14)),
            (32, 1, (0, 32)),
            (32, 0, (0, 32)),
            (8, 7, (6, 2)),
        ] {
            assert_eq!(
                overflow_rows(cores, rows, 1),
                want,
                "{cores} cores in {rows} rows"
            );
        }
        // Volumes take two rows each: the marker needs a row of its own.
        for (vols, rows, want) in [
            (3, 6, (3, 0)),
            (3, 5, (2, 1)),
            (3, 4, (1, 2)),
            (4, 2, (0, 4)),
            (1, 1, (0, 1)),
        ] {
            assert_eq!(overflow_rows(vols, rows, 2), want, "{vols} in {rows} rows");
        }
    }

    fn item(text: &str) -> Vec<Span<'static>> {
        vec![Span::raw(text.to_owned())]
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn legend_items_never_split_across_lines() {
        let items = || {
            vec![
                item("█ app 10.7G"),
                item("█ wired 3.1G"),
                item("█ compressed 1.4G"),
                item("▒ cached 5.3G"),
            ]
        };
        // from the widest item up; a narrower cell clips, it never splits
        for w in 17..90 {
            let lines = pack_items(items(), w);
            let joined: Vec<String> = lines.iter().map(line_text).collect();
            for want in [
                "█ app 10.7G",
                "█ wired 3.1G",
                "█ compressed 1.4G",
                "▒ cached 5.3G",
            ] {
                assert!(joined.iter().any(|l| l.contains(want)), "{w}: {joined:?}");
            }
            for l in &lines {
                assert!(l.width() <= usize::from(w), "{w}: {}", line_text(l));
            }
        }
    }

    #[test]
    fn legend_packs_onto_one_line_when_it_fits() {
        let lines = pack_items(vec![item("ab"), item("cd")], 6);
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "ab  cd");
    }

    #[test]
    fn expanded_mem_hides_swap_cell_without_swap() {
        let mut fx = crate::widgets::tests_support::fixture("");
        fx.snap.memory.total_bytes = 16 << 30;
        fx.snap.memory.used_bytes = 8 << 30;
        let bands = mem_bands(&fx.view());
        let swap = bands
            .iter()
            .flat_map(|b| b.cells.iter())
            .find(|c| c.id == ID_SWAP)
            .unwrap();
        assert!(!swap.present);
        fx.snap.memory.swap_used_bytes = 1 << 20;
        let bands = mem_bands(&fx.view());
        assert!(
            bands
                .iter()
                .flat_map(|b| b.cells.iter())
                .any(|c| c.id == ID_SWAP && c.present)
        );
    }

    #[test]
    fn mem_value_calls_the_kernel_level_available_not_free() {
        let mut fx = crate::widgets::tests_support::fixture("");
        fx.snap.memory.total_bytes = 24 << 30;
        fx.snap.memory.used_bytes = 15 << 30;
        assert!(!mem_value(&fx.view()).contains('%'));
        fx.snap.memory.free_percent = Some(62);
        let value = mem_value(&fx.view());
        assert!(value.ends_with("62% available"), "{value}");
        assert!(!value.contains("free"), "{value}");
    }
}
