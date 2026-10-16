use ratatui::layout::{Constraint, Layout, Rect};

use crate::layout::Panel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CellKind {
    Graph,
    Spark,
    Cluster,
    List,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CellTitle {
    pub label: &'static str,
    pub value: Option<String>,
    pub hop: Option<Panel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CellSpec {
    pub id: u8,
    pub kind: CellKind,
    pub title: CellTitle,
    pub min: (u16, u16),
    pub weight: u16,
    pub present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Band {
    pub min_height: u16,
    pub max_height: Option<u16>,
    pub grow_to: Option<u16>,
    pub take_leftover: bool,
    pub cells: Vec<CellSpec>,
}

impl Band {
    #[must_use]
    pub(super) fn new(min_height: u16, cells: Vec<CellSpec>) -> Self {
        Self {
            min_height,
            max_height: None,
            grow_to: None,
            take_leftover: false,
            cells,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Placed {
    pub id: u8,
    pub kind: CellKind,
    pub rect: Rect,
    pub hop: Option<Panel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Pack {
    pub cells: Vec<Placed>,
}

impl Pack {
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn get(&self, id: u8) -> Option<Placed> {
        self.cells.iter().copied().find(|c| c.id == id)
    }

    #[must_use]
    pub(super) fn hop_at(&self, col: u16, row: u16) -> Option<Panel> {
        self.cells.iter().find_map(|c| {
            if crate::layout::contains(c.rect, col, row) {
                c.hop
            } else {
                None
            }
        })
    }
}

#[must_use]
pub(super) fn pack(area: Rect, bands: &[Band]) -> Pack {
    if area.width == 0 || area.height == 0 {
        return Pack { cells: Vec::new() };
    }
    let mut live: Vec<Band> = bands
        .iter()
        .cloned()
        .map(|mut band| {
            band.cells.retain(|c| c.present);
            band
        })
        .filter(|band| !band.cells.is_empty())
        .collect();

    while live.len() > 1 {
        let sum: u16 = live.iter().map(|b| b.min_height).sum();
        if sum <= area.height {
            break;
        }
        live.pop();
    }

    let mut heights: Vec<u16> = live.iter().map(|b| b.min_height.min(area.height)).collect();
    let used: u16 = heights.iter().copied().sum();
    let mut leftover = area.height.saturating_sub(used);

    for (i, band) in live.iter().enumerate() {
        if leftover == 0 {
            break;
        }
        if let Some(cap) = band.grow_to {
            let room = cap.saturating_sub(heights[i]);
            let give = room.min(leftover);
            heights[i] = heights[i].saturating_add(give);
            leftover = leftover.saturating_sub(give);
        }
    }
    let takers: Vec<usize> = live
        .iter()
        .enumerate()
        .filter(|(_, band)| band.take_leftover)
        .map(|(i, _)| i)
        .collect();
    let mut progressed = !takers.is_empty();
    while leftover > 0 && progressed {
        progressed = false;
        for &i in &takers {
            if leftover == 0 {
                break;
            }
            let cap = live[i].max_height.unwrap_or(u16::MAX);
            if heights[i] < cap {
                heights[i] = heights[i].saturating_add(1);
                leftover = leftover.saturating_sub(1);
                progressed = true;
            }
        }
    }
    if leftover > 0 {
        let target = takers.last().copied().unwrap_or(0);
        if let Some(h) = heights.get_mut(target) {
            *h = h.saturating_add(leftover);
        }
    }

    let mut y = area.y;
    let mut cells = Vec::new();
    for (band, height) in live.iter().zip(heights) {
        let band_rect = Rect {
            x: area.x,
            y,
            width: area.width,
            height,
        };
        y = y.saturating_add(height);
        place_band(&mut cells, band, band_rect);
    }
    Pack { cells }
}

fn place_band(out: &mut Vec<Placed>, band: &Band, rect: Rect) {
    let mut cells: Vec<&CellSpec> = band.cells.iter().collect();
    if cells.is_empty() || rect.height == 0 || rect.width == 0 {
        return;
    }
    loop {
        let splits = split_fill(rect, &cells);
        let wide_enough = cells.len() == 1
            || cells
                .iter()
                .zip(splits.iter())
                .all(|(c, r)| r.width >= c.min.0);
        if wide_enough {
            for (spec, slot) in cells.iter().zip(splits) {
                let Some(kind) = resolve_kind(spec.kind, slot.height) else {
                    continue;
                };
                out.push(Placed {
                    id: spec.id,
                    kind,
                    rect: slot,
                    hop: spec.title.hop,
                });
            }
            return;
        }
        if cells.len() <= 1 {
            return;
        }
        cells.pop();
    }
}

fn split_fill(area: Rect, cells: &[&CellSpec]) -> Vec<Rect> {
    if cells.is_empty() {
        return Vec::new();
    }
    if cells.len() == 1 {
        return vec![area];
    }
    let constraints: Vec<Constraint> = cells
        .iter()
        .map(|c| Constraint::Fill(c.weight.max(1)))
        .collect();
    Layout::horizontal(constraints).split(area).to_vec()
}

fn resolve_kind(kind: CellKind, height: u16) -> Option<CellKind> {
    match kind {
        CellKind::Graph if height >= 4 => Some(CellKind::Graph),
        CellKind::Graph | CellKind::Spark if height >= 3 => Some(CellKind::Spark),
        CellKind::Cluster if height >= 4 => Some(CellKind::Cluster),
        CellKind::List if height >= 3 => Some(CellKind::List),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn title(label: &'static str) -> CellTitle {
        CellTitle {
            label,
            value: None,
            hop: None,
        }
    }

    fn graph(id: u8, label: &'static str) -> CellSpec {
        CellSpec {
            id,
            kind: CellKind::Graph,
            title: title(label),
            min: (16, 5),
            weight: 1,
            present: true,
        }
    }

    fn spark(id: u8, label: &'static str, hop: Panel) -> CellSpec {
        CellSpec {
            id,
            kind: CellKind::Spark,
            title: CellTitle {
                label,
                value: None,
                hop: Some(hop),
            },
            min: (12, 3),
            weight: 1,
            present: true,
        }
    }

    fn cluster(id: u8, label: &'static str) -> CellSpec {
        CellSpec {
            id,
            kind: CellKind::Cluster,
            title: title(label),
            min: (14, 5),
            weight: 1,
            present: true,
        }
    }

    fn cpu_bands_hops(hops: bool) -> Vec<Band> {
        vec![
            Band {
                min_height: 4,
                max_height: Some(4),
                take_leftover: false,
                ..Band::new(
                    4,
                    vec![graph(0, "cpu"), graph(1, "super"), graph(2, "perf")],
                )
            },
            Band {
                take_leftover: true,
                ..Band::new(
                    5,
                    vec![
                        graph(10, "super zone"),
                        graph(11, "perf zone"),
                        graph(13, "package"),
                    ],
                )
            },
            Band::new(
                3,
                vec![
                    CellSpec {
                        present: hops,
                        ..spark(20, "gpu", Panel::Gpu)
                    },
                    CellSpec {
                        present: hops,
                        ..spark(21, "fan", Panel::Fans)
                    },
                ],
            ),
            Band {
                grow_to: Some(8),
                ..Band::new(5, vec![cluster(30, "super"), cluster(31, "perf")])
            },
        ]
    }

    #[test]
    fn pack_cpu_80x23_reference() {
        let area = Rect::new(0, 0, 78, 21);
        let pack = pack(area, &cpu_bands_hops(true));
        let cpu = pack.get(0).expect("cpu");
        let super_g = pack.get(1).expect("super");
        let perf_g = pack.get(2).expect("perf");
        assert_eq!(cpu.kind, CellKind::Graph);
        assert_eq!(cpu.rect, Rect::new(0, 0, 26, 4));
        assert_eq!(super_g.rect, Rect::new(26, 0, 26, 4));
        assert_eq!(perf_g.rect, Rect::new(52, 0, 26, 4));

        let zone = pack.get(10).expect("super zone");
        assert_eq!(zone.rect, Rect::new(0, 4, 26, 6));
        assert_eq!(
            pack.get(11).expect("perf zone").rect,
            Rect::new(26, 4, 26, 6)
        );
        assert_eq!(pack.get(13).expect("package").rect, Rect::new(52, 4, 26, 6));

        let gpu = pack.get(20).expect("gpu hop");
        let fan = pack.get(21).expect("fan hop");
        assert_eq!(gpu.kind, CellKind::Spark);
        assert_eq!(gpu.rect, Rect::new(0, 10, 39, 3));
        assert_eq!(fan.rect, Rect::new(39, 10, 39, 3));
        assert_eq!(gpu.hop, Some(Panel::Gpu));

        let strip = pack.get(30).expect("super strip");
        assert_eq!(strip.kind, CellKind::Cluster);
        assert_eq!(strip.rect, Rect::new(0, 13, 39, 8));
        assert_eq!(
            pack.get(31).expect("perf strip").rect,
            Rect::new(39, 13, 39, 8)
        );
        assert_eq!(pack.cells.len(), 10);
    }

    #[test]
    fn pack_leftover_does_not_fatten_capped_band() {
        let area = Rect::new(0, 0, 78, 21);
        let pack = pack(area, &cpu_bands_hops(false));
        assert!(pack.get(20).is_none());
        let cpu = pack.get(0).expect("cpu");
        assert_eq!(cpu.rect.height, 4);
        assert_eq!(pack.get(10).expect("zone").rect.height, 9);
        assert_eq!(pack.get(30).expect("strip").rect.height, 8);
    }

    #[test]
    fn pack_shares_leftover_across_takers() {
        let bands = [
            Band {
                take_leftover: true,
                ..Band::new(4, vec![graph(0, "super"), graph(1, "perf")])
            },
            Band {
                take_leftover: true,
                ..Band::new(5, vec![graph(10, "super zone"), graph(11, "perf zone")])
            },
        ];
        let pack = pack(Rect::new(0, 0, 78, 21), &bands);
        let usage = pack.get(0).expect("super");
        let zone = pack.get(10).expect("zone");
        assert_eq!(usage.rect.height, 10);
        assert_eq!(zone.rect.height, 11);
        assert_eq!(
            usage.rect.height.saturating_add(zone.rect.height),
            21,
            "sibling bands must fill the pane"
        );
    }

    #[test]
    fn pack_fills_the_pane_when_heat_band_is_absent() {
        let bands = [Band {
            max_height: Some(4),
            take_leftover: false,
            ..Band::new(4, vec![graph(0, "cpu"), graph(1, "super")])
        }];
        let pack = pack(Rect::new(0, 0, 78, 21), &bands);
        let cpu = pack.get(0).expect("cpu");
        assert_eq!(cpu.rect.height, 21, "unused leftover must fill the pane");
        assert_eq!(pack.get(1).expect("super").rect.height, 21);
    }

    #[test]
    fn pack_drops_tail_band_when_short() {
        let area = Rect::new(0, 0, 58, 13);
        let pack = pack(area, &cpu_bands_hops(true));
        assert!(pack.get(30).is_none());
        assert_eq!(pack.get(0).expect("cpu").kind, CellKind::Graph);
        assert_eq!(pack.get(20).expect("hop").kind, CellKind::Spark);
    }

    #[test]
    fn pack_omits_absent_cells() {
        let bands = [Band::new(
            5,
            vec![
                graph(0, "cpu"),
                CellSpec {
                    present: false,
                    ..graph(3, "eff")
                },
            ],
        )];
        let pack = pack(Rect::new(0, 0, 40, 5), &bands);
        assert!(pack.get(0).is_some());
        assert!(pack.get(3).is_none());
        assert_eq!(pack.get(0).expect("cpu").rect.width, 40);
    }

    #[test]
    fn hop_at_hits_spark_rect() {
        let pack = pack(Rect::new(0, 0, 78, 21), &cpu_bands_hops(true));
        assert_eq!(pack.hop_at(1, 11), Some(Panel::Gpu));
        assert_eq!(pack.hop_at(50, 11), Some(Panel::Fans));
        assert_eq!(pack.hop_at(1, 1), None);
    }

    fn lcg(seed: &mut u64) -> u16 {
        *seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (*seed >> 33) as u16
    }

    fn random_bands(seed: u64) -> Vec<Band> {
        let mut s = seed;
        let kinds = [
            CellKind::Graph,
            CellKind::Spark,
            CellKind::Cluster,
            CellKind::List,
        ];
        let hops = [None, Some(Panel::Cpu), Some(Panel::Net)];
        let mut id = 0_u8;
        (0..=lcg(&mut s) % 5)
            .map(|_| {
                let cells = (0..=lcg(&mut s) % 4)
                    .map(|_| {
                        id += 1;
                        CellSpec {
                            id,
                            kind: kinds[usize::from(lcg(&mut s) % 4)],
                            title: CellTitle {
                                label: "c",
                                value: None,
                                hop: hops[usize::from(lcg(&mut s) % 3)],
                            },
                            min: (lcg(&mut s) % 40, 3),
                            weight: lcg(&mut s) % 4,
                            present: !lcg(&mut s).is_multiple_of(5),
                        }
                    })
                    .collect();
                let mut band = Band::new(1 + lcg(&mut s) % 14, cells);
                band.take_leftover = lcg(&mut s).is_multiple_of(2);
                band.grow_to = lcg(&mut s)
                    .is_multiple_of(3)
                    .then(|| band.min_height + lcg(&mut s) % 10);
                band.max_height = lcg(&mut s)
                    .is_multiple_of(3)
                    .then(|| band.min_height + lcg(&mut s) % 6);
                band
            })
            .collect()
    }

    #[test]
    fn pack_laws_hold_for_random_bands() {
        for seed in 0..200_u64 {
            let bands = random_bands(seed);
            for (w, h) in [
                (1_u16, 1_u16),
                (20, 6),
                (80, 23),
                (120, 39),
                (7, 40),
                (300, 90),
            ] {
                let area = Rect::new(3, 2, w, h);
                let packed = pack(area, &bands);
                let ctx = format!("seed {seed} {w}x{h}");
                for (i, a) in packed.cells.iter().enumerate() {
                    assert!(
                        a.rect.x >= area.x && a.rect.right() <= area.right(),
                        "{ctx}"
                    );
                    assert!(
                        a.rect.y >= area.y && a.rect.bottom() <= area.bottom(),
                        "{ctx}"
                    );
                    let spec = bands
                        .iter()
                        .flat_map(|b| &b.cells)
                        .find(|c| c.id == a.id)
                        .unwrap();
                    assert!(spec.present, "absent cell {} placed {ctx}", a.id);
                    assert_eq!(a.hop, spec.title.hop);
                    let min_h = match a.kind {
                        CellKind::Graph | CellKind::Cluster => 4,
                        CellKind::Spark | CellKind::List => 3,
                    };
                    assert!(a.rect.height >= min_h, "{:?} too short {ctx}", a.kind);
                    for b in &packed.cells[i + 1..] {
                        assert_ne!(a.id, b.id, "{ctx}");
                        assert!(!a.rect.intersects(b.rect), "{a:?} vs {b:?} {ctx}");
                    }
                }
            }
        }
    }

    #[test]
    fn pack_zero_area_is_empty() {
        let bands = cpu_bands_hops(true);
        assert!(pack(Rect::new(0, 0, 0, 30), &bands).cells.is_empty());
        assert!(pack(Rect::new(0, 0, 80, 0), &bands).cells.is_empty());
        assert!(pack(Rect::new(0, 0, 80, 23), &[]).cells.is_empty());
    }

    #[test]
    fn pack_is_deterministic() {
        for seed in 0..50_u64 {
            let bands = random_bands(seed);
            let area = Rect::new(0, 0, 100, 30);
            assert_eq!(pack(area, &bands), pack(area, &bands));
        }
    }

    #[test]
    fn hop_at_agrees_with_placed_rects() {
        for seed in 0..100_u64 {
            let bands = random_bands(seed);
            let area = Rect::new(0, 0, 90, 30);
            let packed = pack(area, &bands);
            for y in 0..30 {
                for x in (0..90).step_by(3) {
                    let owner = packed
                        .cells
                        .iter()
                        .find(|c| crate::layout::contains(c.rect, x, y));
                    assert_eq!(packed.hop_at(x, y), owner.and_then(|c| c.hop), "{x},{y}");
                }
            }
            assert_eq!(packed.hop_at(500, 500), None);
        }
    }

    #[test]
    fn resolve_kind_ladder() {
        let cases = [
            (CellKind::Graph, 4, Some(CellKind::Graph)),
            (CellKind::Graph, 3, Some(CellKind::Spark)),
            (CellKind::Graph, 2, None),
            (CellKind::Spark, 9, Some(CellKind::Spark)),
            (CellKind::Spark, 2, None),
            (CellKind::Cluster, 4, Some(CellKind::Cluster)),
            (CellKind::Cluster, 3, None),
            (CellKind::List, 3, Some(CellKind::List)),
            (CellKind::List, 0, None),
        ];
        for (kind, h, want) in cases {
            assert_eq!(resolve_kind(kind, h), want, "{kind:?} at {h}");
        }
    }

    #[test]
    fn get_finds_by_id() {
        let packed = pack(Rect::new(0, 0, 80, 23), &cpu_bands_hops(true));
        for placed in &packed.cells {
            assert_eq!(packed.get(placed.id), Some(*placed));
        }
        assert_eq!(packed.get(250), None);
    }
}
