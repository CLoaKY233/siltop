use siltop_core::{History, ScaleRange, TimeAxis};

const LEVELS: usize = 4;
// Fill from the BOTTOM of the cell (btop "up"): dots 7-3-2-1 / 8-6-5-4.
const LEFT_DOTS: [u32; LEVELS] = [6, 2, 1, 0];
const RIGHT_DOTS: [u32; LEVELS] = [7, 5, 4, 3];
// Fill from the TOP of the cell (btop "down"): dots 1-2-3-7 / 4-5-6-8.
const LEFT_DOTS_DOWN: [u32; LEVELS] = [0, 1, 2, 6];
const RIGHT_DOTS_DOWN: [u32; LEVELS] = [3, 4, 5, 7];
const BLANK: char = '\u{2800}';

/// btop `braille_up`, generated from bits. Index `[left 0..=4][right 0..=4]`.
pub(crate) const BRAILLE_UP: [[char; 5]; 5] = braille_table(LEFT_DOTS, RIGHT_DOTS);
/// btop `braille_down`: the same levels hanging from the top of the cell.
pub(crate) const BRAILLE_DOWN: [[char; 5]; 5] = braille_table(LEFT_DOTS_DOWN, RIGHT_DOTS_DOWN);

/// Which way a graph grows from its baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Grow {
    #[default]
    Up,
    /// Hangs from the top edge: the lower half of a mirrored graph.
    Down,
}

/// One braille column: two samples stacked into `height` glyphs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrailleCell {
    pub glyph: char,
    pub intensity: f32,
}

const BLANK_CELL: BrailleCell = BrailleCell {
    glyph: BLANK,
    intensity: 0.0,
};

#[must_use]
pub const fn braille_cell(left: u8, right: u8) -> char {
    braille_bits(left, right, LEFT_DOTS, RIGHT_DOTS)
}

const fn braille_bits(
    left: u8,
    right: u8,
    left_dots: [u32; LEVELS],
    right_dots: [u32; LEVELS],
) -> char {
    let left = if left > LEVELS as u8 {
        LEVELS as u8
    } else {
        left
    };
    let right = if right > LEVELS as u8 {
        LEVELS as u8
    } else {
        right
    };
    let mut bits = 0_u32;
    let mut i = 0_u8;
    while i < left {
        bits |= 1 << left_dots[i as usize];
        i += 1;
    }
    i = 0;
    while i < right {
        bits |= 1 << right_dots[i as usize];
        i += 1;
    }
    braille_from_bits(bits)
}

#[cfg(test)]
#[must_use]
pub(crate) const fn braille_transition(prev: u8, curr: u8) -> char {
    let prev = if prev > 4 { 4 } else { prev } as usize;
    let curr = if curr > 4 { 4 } else { curr } as usize;
    BRAILLE_UP[prev][curr]
}

const fn braille_from_bits(bits: u32) -> char {
    match char::from_u32(0x2800 + (bits & 0xFF)) {
        Some(ch) => ch,
        None => BLANK,
    }
}

const fn braille_table(left_dots: [u32; LEVELS], right_dots: [u32; LEVELS]) -> [[char; 5]; 5] {
    let mut table = [[BLANK; 5]; 5];
    let mut left = 0_u8;
    while left < 5 {
        let mut right = 0_u8;
        while right < 5 {
            table[left as usize][right as usize] = braille_bits(left, right, left_dots, right_dots);
            right += 1;
        }
        left += 1;
    }
    table
}

/// Round-to-nearest dot for 1-row strips (a value under half a dot draws
/// nothing, so idle strips stay quiet); a small lift for taller graphs.
#[must_use]
pub(crate) fn bias_for_height(height: usize) -> f32 {
    if height <= 1 { 0.125 } else { 0.1 }
}

fn level_in_band(value: f32, lo: f32, hi: f32, bias: f32) -> u8 {
    if value <= 0.0 {
        return 0;
    }
    let span = (hi - lo).max(f32::EPSILON);
    let t = (value - lo) / span + bias;
    if t <= 0.0 {
        0
    } else if t >= 1.0 {
        LEVELS as u8
    } else {
        (t * LEVELS as f32).floor().min(LEVELS as f32) as u8
    }
}

/// Two normalised dots per column across `width` columns: the bucket means of
/// `history` over the axis window, mapped through `range`. `None` = no data.
#[must_use]
pub(crate) fn dots(
    history: &History,
    axis: TimeAxis,
    width: u16,
    range: ScaleRange,
) -> Vec<Option<f32>> {
    let n = usize::from(width) * 2;
    let span = range.span();
    history
        .buckets(axis.span(n), n, axis.now)
        .into_iter()
        .map(|b| b.map(|b| ((b.mean - range.min) / span).clamp(0.0, 1.0)))
        .collect()
}

#[must_use]
pub fn render_history(history: &History, width: u16, height: u16) -> Vec<String> {
    render_cells(history, width, height, 1.0)
        .into_iter()
        .map(|row| row.into_iter().map(|cell| cell.glyph).collect())
        .collect()
}

#[must_use]
pub fn render_cells(
    history: &History,
    width: u16,
    height: u16,
    scale: f32,
) -> Vec<Vec<BrailleCell>> {
    render_cells_range(history, width, height, 0.0, scale)
}

/// Live view of one history on its own clock, `min..=max` mapped to the height.
#[must_use]
pub(crate) fn render_cells_range(
    history: &History,
    width: u16,
    height: u16,
    min: f32,
    max: f32,
) -> Vec<Vec<BrailleCell>> {
    let range = ScaleRange { min, max };
    let dots = dots(history, TimeAxis::live(history), width, range);
    render_dots(&dots, width, height, Grow::Up)
}

/// Stack dot pairs into `height` rows of `width` cells growing `grow`.
#[must_use]
pub(crate) fn render_dots(
    dots: &[Option<f32>],
    width: u16,
    height: u16,
    grow: Grow,
) -> Vec<Vec<BrailleCell>> {
    let height = usize::from(height.max(1));
    let width = usize::from(width);
    let mut rows = vec![Vec::with_capacity(width); height];
    let bias = bias_for_height(height);
    let table = match grow {
        Grow::Up => &BRAILLE_UP,
        Grow::Down => &BRAILLE_DOWN,
    };
    let pairs = dots.as_chunks::<2>().0;
    let pad = width.saturating_sub(pairs.len());
    for row in &mut rows {
        row.resize(pad, BLANK_CELL);
    }
    for &pair in pairs.iter().skip(pairs.len().saturating_sub(width)) {
        let Some((left, right)) = pair_dots(pair) else {
            for row in &mut rows {
                row.push(BLANK_CELL);
            }
            continue;
        };
        let intensity = left.max(right);
        for (r, row) in rows.iter_mut().enumerate() {
            // band of this row measured from the baseline
            let from_base = match grow {
                Grow::Up => height - 1 - r,
                Grow::Down => r,
            };
            let lo = from_base as f32 / height as f32;
            let hi = (from_base + 1) as f32 / height as f32;
            let l = level_in_band(left, lo, hi, bias);
            let rr = level_in_band(right, lo, hi, bias);
            row.push(BrailleCell {
                glyph: table[usize::from(l)][usize::from(rr)],
                intensity,
            });
        }
    }
    rows
}

/// A missing partner copies the measured one: never pair a sample with a
/// synthetic 0, so the oldest dot does not blink as it scrolls.
fn pair_dots([left, right]: [Option<f32>; 2]) -> Option<(f32, f32)> {
    match (left, right) {
        (Some(l), Some(r)) => Some((l, r)),
        (Some(v), None) | (None, Some(v)) => Some((v, v)),
        (None, None) => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn bit_pattern_empty() {
        assert_eq!(braille_cell(0, 0), '\u{2800}');
    }

    #[test]
    fn bit_pattern_full() {
        assert_eq!(braille_cell(4, 4), '⣿');
        assert_eq!(braille_cell(4, 4), '\u{28ff}');
    }

    #[test]
    fn bit_pattern_left_one() {
        // left level 1 = bottom-left (dot 7)
        assert_eq!(braille_cell(1, 0), '⡀');
        assert_eq!(braille_cell(1, 0), '\u{2840}');
    }

    #[test]
    fn bit_pattern_right_full() {
        // right level 4 = dots 8,6,5,4
        assert_eq!(braille_cell(0, 4), '⢸');
        assert_eq!(braille_cell(0, 4), '\u{28b8}');
    }

    #[test]
    fn render_history_width_height() {
        let rows = render_history(&History::default(), 7, 3);
        assert_eq!(rows.len(), 3);
        for row in &rows {
            assert_eq!(row.chars().count(), 7);
        }
    }

    #[test]
    fn render_history_zero_width() {
        let rows = render_history(&History::default(), 0, 2);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(String::is_empty));
    }

    #[test]
    fn render_history_zero_height_becomes_one() {
        let rows = render_history(&History::default(), 3, 0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].chars().count(), 3);
    }

    #[test]
    fn empty_history_is_blank_braille() {
        let rows = render_history(&History::default(), 4, 2);
        let blank = "\u{2800}".repeat(4);
        assert_eq!(rows, vec![blank.clone(), blank]);
        assert!(rows.iter().all(|row| row.chars().all(|c| c == '\u{2800}')));
    }

    #[test]
    fn full_history_fills_cells() {
        let mut history = History::with_capacity(8);
        for _ in 0..8 {
            history.push(1.0);
        }
        let rows = render_history(&history, 4, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], "⣿⣿⣿⣿");
        assert_eq!(rows[0].chars().count(), 4);
    }

    #[test]
    fn full_history_stacks_both_rows() {
        let mut history = History::with_capacity(8);
        for _ in 0..8 {
            history.push(1.0);
        }
        let rows = render_history(&history, 4, 2);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], "⣿⣿⣿⣿");
        assert_eq!(rows[1], "⣿⣿⣿⣿");
    }

    #[test]
    fn short_history_pads_left() {
        let mut history = History::with_capacity(8);
        history.push(1.0);
        history.push(1.0);
        let rows = render_history(&history, 4, 1);
        assert_eq!(rows[0].chars().count(), 4);
        let chars: Vec<char> = rows[0].chars().collect();
        assert_eq!(chars[0], '\u{2800}');
        assert_eq!(chars[1], '\u{2800}');
        assert_eq!(chars[2], '\u{2800}');
        assert_eq!(chars[3], '⣿');
    }

    #[test]
    fn cells_carry_intensity() {
        let mut history = History::with_capacity(4);
        history.push(0.25);
        history.push(0.75);
        let rows = render_cells(&history, 1, 1, 1.0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), 1);
        assert!((rows[0][0].intensity - 0.75).abs() < f32::EPSILON);
        assert_ne!(rows[0][0].glyph, '\u{2800}');
    }

    #[test]
    fn tiny_value_lights_bottom_dot() {
        let mut history = History::with_capacity(4);
        history.push(0.04);
        history.push(0.04);
        let rows = render_cells(&history, 1, 4, 1.0);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0][0].glyph, '\u{2800}');
        assert_ne!(rows[3][0].glyph, '\u{2800}');
    }

    #[test]
    fn auto_scale_fills_height() {
        let mut history = History::with_capacity(4);
        history.push(80_000.0);
        history.push(80_000.0);
        let scale = history.scale(siltop_core::Scale::Auto { floor: 1.0 });
        let rows = render_cells(&history, 1, 2, scale);
        assert_ne!(rows[0][0].glyph, '\u{2800}');
    }

    #[test]
    fn lut_matches_braille_cell_for_all_25() {
        for left in 0_u8..=4 {
            for right in 0_u8..=4 {
                assert_eq!(
                    braille_transition(left, right),
                    braille_cell(left, right),
                    "lut[{left}][{right}]"
                );
            }
        }
    }

    #[test]
    fn transition_2_4_is_not_4_2() {
        assert_ne!(braille_transition(2, 4), braille_transition(4, 2));
        assert_eq!(braille_transition(2, 4), braille_cell(2, 4));
        assert_eq!(braille_transition(4, 2), braille_cell(4, 2));
    }

    #[test]
    fn lut_is_not_two_independent_bars_for_slopes() {
        assert_eq!(braille_transition(2, 4), '⣼');
        assert_eq!(braille_transition(4, 2), '⣧');
        assert_ne!(braille_transition(2, 4), '⣿');
        assert_ne!(braille_transition(2, 4), '\u{2800}');
    }

    #[test]
    fn single_sample_is_full_cell_not_right_zero() {
        let mut history = History::with_capacity(4);
        history.push(1.0);
        let rows = render_history(&history, 1, 1);
        assert_eq!(rows[0], "⣿");
        assert_ne!(rows[0].chars().next(), Some(braille_cell(4, 0)));
    }

    #[test]
    fn odd_n_cell_count_is_ceil() {
        for n in 1_usize..=5 {
            let mut history = History::with_capacity(8);
            for _ in 0..n {
                history.push(1.0);
            }
            let rows = render_cells(&history, 4, 1, 1.0);
            let lit = rows[0].iter().filter(|c| c.glyph != '\u{2800}').count();
            assert_eq!(lit, n.div_ceil(2), "n={n}");
        }
    }

    #[test]
    fn odd_sample_count_rightmost_is_not_half_empty() {
        for n in [1_usize, 3, 5] {
            let mut history = History::with_capacity(8);
            for _ in 0..n {
                history.push(1.0);
            }
            let rows = render_cells(&history, 4, 1, 1.0);
            let last = rows[0]
                .iter()
                .rev()
                .find(|c| c.glyph != '\u{2800}')
                .expect("data cell");
            assert_ne!(last.glyph, braille_cell(4, 0), "n={n}");
            assert_eq!(last.glyph, '⣿');
            assert!((last.intensity - 1.0).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn scroll_n_to_n_plus_1_no_blank_inserted_at_right() {
        let mut history = History::with_capacity(16);
        for _ in 0..8 {
            history.push(1.0);
        }
        let before = render_cells(&history, 4, 1, 1.0);
        history.push(1.0);
        let after = render_cells(&history, 4, 1, 1.0);
        assert_ne!(after[0][3].glyph, '\u{2800}');
        assert_eq!(after[0][3].glyph, '⣿');
        assert_eq!(before[0][3].glyph, '⣿');
    }

    #[test]
    fn forty_col_braille_recent_is_last_value_not_peak() {
        let mut history = History::with_capacity(3600);
        for _ in 0..3555 {
            history.push(0.0);
        }
        for _ in 0..5 {
            history.push(1.0);
        }
        for _ in 0..40 {
            history.push(0.2);
        }
        let rows = render_cells(&history, 40, 1, 1.0);
        assert_eq!(rows[0].len(), 40);
        let last = rows[0][39];
        assert!(
            last.intensity < 0.3,
            "rightmost cell must be the 0.2 tail, not the 1.0 peak just behind it: {}",
            last.intensity
        );
        assert!(
            rows[0].iter().any(|c| c.intensity > 0.9),
            "older 1.0 peak must still appear on the left"
        );
    }

    fn bits(c: char) -> u32 {
        u32::from(c) - 0x2800
    }

    #[test]
    fn every_glyph_is_in_the_braille_block() {
        for left in 0_u8..=u8::MAX {
            for right in [0_u8, 1, 2, 3, 4, 5, 9, 200, u8::MAX] {
                let c = braille_cell(left, right);
                assert!(
                    ('\u{2800}'..='\u{28FF}').contains(&c),
                    "{left},{right} -> {c:?}"
                );
                let t = braille_transition(left, right);
                assert!(('\u{2800}'..='\u{28FF}').contains(&t));
            }
        }
    }

    #[test]
    fn out_of_range_levels_clamp_to_full() {
        for over in [5_u8, 17, u8::MAX] {
            assert_eq!(braille_cell(over, 0), braille_cell(4, 0));
            assert_eq!(braille_cell(0, over), braille_cell(0, 4));
            assert_eq!(braille_transition(over, over), '⣿');
        }
    }

    #[test]
    fn dot_count_is_left_plus_right() {
        for left in 0_u8..=4 {
            for right in 0_u8..=4 {
                let n = bits(braille_cell(left, right)).count_ones();
                assert_eq!(n, u32::from(left + right), "{left},{right}");
            }
        }
    }

    #[test]
    fn fill_is_monotone_in_each_half() {
        for left in 0_u8..4 {
            for right in 0_u8..=4 {
                let lo = bits(braille_cell(left, right));
                let hi = bits(braille_cell(left + 1, right));
                assert_eq!(lo & hi, lo, "left {left}->{} lost a dot", left + 1);
                let lo = bits(braille_cell(right, left));
                let hi = bits(braille_cell(right, left + 1));
                assert_eq!(lo & hi, lo, "right {left}->{} lost a dot", left + 1);
            }
        }
    }

    #[test]
    fn halves_are_independent_and_disjoint() {
        const LEFT_COLUMN: u32 = 0b0100_0111; // dots 1 2 3 7
        const RIGHT_COLUMN: u32 = 0b1011_1000; // dots 4 5 6 8
        for left in 0_u8..=4 {
            for right in 0_u8..=4 {
                let both = bits(braille_cell(left, right));
                let l = bits(braille_cell(left, 0));
                let r = bits(braille_cell(0, right));
                assert_eq!(both, l | r, "{left},{right}");
                assert_eq!(l & r, 0);
                assert_eq!(l & !LEFT_COLUMN, 0, "left level {left} strayed right");
                assert_eq!(r & !RIGHT_COLUMN, 0, "right level {right} strayed left");
            }
        }
    }

    #[test]
    fn fill_grows_from_the_bottom() {
        // Level 1 is the bottom dot of each column (dots 7 and 8).
        assert_eq!(bits(braille_cell(1, 0)), 1 << 6);
        assert_eq!(bits(braille_cell(0, 1)), 1 << 7);
        // Level 4 adds the top dot last (dots 1 and 4).
        assert_eq!(bits(braille_cell(4, 0)) & !bits(braille_cell(3, 0)), 1 << 0);
        assert_eq!(bits(braille_cell(0, 4)) & !bits(braille_cell(0, 3)), 1 << 3);
    }

    #[test]
    fn all_25_glyphs_are_distinct() {
        let mut seen = std::collections::HashSet::new();
        for row in BRAILLE_UP {
            for c in row {
                assert!(seen.insert(c), "duplicate {c:?}");
            }
        }
        assert_eq!(seen.len(), 25);
    }

    #[test]
    fn render_shape_sweep() {
        for len in [0_usize, 1, 2, 3, 7, 40, 81, 450] {
            let mut h = History::with_capacity(450);
            for i in 0..len {
                h.push((i % 10) as f32 / 10.0);
            }
            for width in [0_u16, 1, 2, 3, 5, 40, 120] {
                for height in [0_u16, 1, 2, 3, 6] {
                    let rows = render_cells(&h, width, height, 1.0);
                    assert_eq!(rows.len(), usize::from(height.max(1)));
                    for row in &rows {
                        assert_eq!(row.len(), usize::from(width), "{len} {width}x{height}");
                        for cell in row {
                            assert!(('\u{2800}'..='\u{28FF}').contains(&cell.glyph));
                            assert!((0.0..=1.0).contains(&cell.intensity));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn n_samples_light_ceil_n_over_2_cells() {
        for n in 0_usize..=100 {
            let mut h = History::with_capacity(200);
            for _ in 0..n {
                h.push(1.0);
            }
            for width in [1_u16, 7, 30, 50, 60] {
                let rows = render_cells(&h, width, 1, 1.0);
                let lit = rows[0].iter().filter(|c| c.glyph != '\u{2800}').count();
                assert_eq!(
                    lit,
                    n.div_ceil(2).min(usize::from(width)),
                    "n={n} w={width}"
                );
                // data hugs the right edge: no blank after the first lit cell
                if let Some(first) = rows[0].iter().position(|c| c.glyph != '\u{2800}') {
                    assert!(rows[0][first..].iter().all(|c| c.glyph == '⣿'), "n={n}");
                }
            }
        }
    }

    #[test]
    fn higher_values_never_light_fewer_dots() {
        for height in [1_u16, 2, 3, 4, 8] {
            let mut last: Option<Vec<u32>> = None;
            for step in 0..=100 {
                let v = step as f32 / 100.0;
                let mut h = History::with_capacity(4);
                h.push(v);
                h.push(v);
                let rows = render_cells(&h, 1, height, 1.0);
                let dots: Vec<u32> = rows.iter().map(|r| bits(r[0].glyph)).collect();
                if let Some(prev) = &last {
                    for (row, (a, b)) in prev.iter().zip(&dots).enumerate() {
                        assert_eq!(a & b, *a, "h={height} v={v} row {row} lost dots");
                    }
                }
                last = Some(dots);
            }
        }
    }

    #[test]
    fn stacked_rows_fill_below_the_crest() {
        // Once a row shows any dot, every row beneath it is solid.
        for height in [2_u16, 3, 4, 6, 10] {
            for step in 0..=50 {
                let v = step as f32 / 50.0;
                let mut h = History::with_capacity(4);
                h.push(v);
                h.push(v);
                let rows = render_cells(&h, 1, height, 1.0);
                let top = rows.iter().position(|r| r[0].glyph != '\u{2800}');
                if let Some(top) = top {
                    for row in &rows[top + 1..] {
                        assert_eq!(row[0].glyph, '⣿', "h={height} v={v}");
                    }
                }
            }
        }
    }

    #[test]
    fn zero_is_blank_at_every_height() {
        for height in 1_u16..=8 {
            let mut h = History::with_capacity(8);
            for _ in 0..8 {
                h.push(0.0);
            }
            let rows = render_cells(&h, 4, height, 1.0);
            assert!(
                rows.iter().flatten().all(|c| c.glyph == '\u{2800}'),
                "h={height}"
            );
        }
    }

    #[test]
    fn bias_is_larger_for_one_row_graphs() {
        assert!(bias_for_height(0) > bias_for_height(2));
        assert!((bias_for_height(1) - 0.125).abs() < f32::EPSILON);
        assert!((bias_for_height(50) - 0.1).abs() < f32::EPSILON);
    }

    #[test]
    fn range_mapping_uses_min_as_the_floor() {
        let mut h = History::with_capacity(4);
        h.push(40.0);
        h.push(40.0);
        let at_floor = render_cells_range(&h, 1, 2, 40.0, 80.0);
        let above = render_cells_range(&h, 1, 2, 0.0, 80.0);
        assert!(at_floor.iter().all(|r| r[0].glyph == '\u{2800}'));
        assert!(above.iter().any(|r| r[0].glyph != '\u{2800}'));
        let inverted = render_cells_range(&h, 3, 2, 80.0, 40.0);
        assert_eq!(inverted.len(), 2);
        assert!(inverted.iter().all(|r| r.len() == 3));
    }
}
