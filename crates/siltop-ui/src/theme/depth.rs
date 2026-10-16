use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

use super::Theme;

/// How many colours the terminal can show. Themes are authored in RGB; everything
/// else is derived from the finished frame in one pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorDepth {
    #[default]
    TrueColor,
    /// xterm-256: the 6×6×6 cube plus the grey ramp.
    Ansi256,
    /// The 16 named colours the user's scheme defines.
    Ansi16,
    /// `NO_COLOR`: attributes only.
    NoColor,
}

impl ColorDepth {
    #[must_use]
    pub fn map(self, color: Color) -> Color {
        match (self, color) {
            (Self::NoColor, _) => Color::Reset,
            (Self::Ansi256, Color::Rgb(r, g, b)) => Color::Indexed(xterm256(r, g, b)),
            (Self::Ansi16, Color::Rgb(r, g, b)) => ansi16(r, g, b),
            (Self::Ansi16, Color::Indexed(i)) if i >= 16 => {
                let (r, g, b) = indexed_rgb(i);
                ansi16(r, g, b)
            }
            _ => color,
        }
    }
}

/// Bring a rendered frame down to the theme's colour depth.
pub(super) fn finish(theme: &Theme, buf: &mut Buffer) {
    let depth = theme.depth;
    if depth == ColorDepth::TrueColor {
        return;
    }
    let lowered = matches!(depth, ColorDepth::Ansi16 | ColorDepth::NoColor);
    let fill = |bg: Color, token: Color| bg != Color::Reset && bg == token;
    for cell in &mut buf.content {
        if lowered {
            if fill(cell.bg, theme.sel_bg) || fill(cell.bg, theme.kill_bg) {
                cell.modifier |= Modifier::REVERSED;
                cell.fg = Color::Reset;
                cell.bg = Color::Reset;
            } else if fill(cell.bg, theme.sel_idle) {
                cell.bg = Color::Reset;
            }
        }
        if depth == ColorDepth::NoColor {
            cell.modifier |= attribute_for(theme, cell.fg);
        }
        cell.fg = depth.map(cell.fg);
        cell.bg = depth.map(cell.bg);
    }
}

fn attribute_for(theme: &Theme, fg: Color) -> Modifier {
    if fg == Color::Reset {
        Modifier::empty()
    } else if fg == theme.dim || fg == theme.muted || fg == theme.faint || fg == theme.border {
        Modifier::DIM
    } else if fg == theme.warn || fg == theme.crit || fg == theme.hi {
        Modifier::BOLD
    } else {
        Modifier::empty()
    }
}

const CUBE: [u8; 6] = [0x00, 0x5f, 0x87, 0xaf, 0xd7, 0xff];

fn cube_step(v: u8) -> u8 {
    if v < 48 {
        0
    } else if v < 115 {
        1
    } else {
        (v - 35) / 40
    }
}

fn distance(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
    let d = |x: u8, y: u8| u32::from(x.abs_diff(y)).pow(2);
    d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
}

/// Nearest xterm-256 index, choosing between the colour cube and the grey ramp.
fn xterm256(r: u8, g: u8, b: u8) -> u8 {
    let (qr, qg, qb) = (cube_step(r), cube_step(g), cube_step(b));
    let cube = (
        CUBE[usize::from(qr)],
        CUBE[usize::from(qg)],
        CUBE[usize::from(qb)],
    );
    let cube_index = 16 + 36 * qr + 6 * qg + qb;
    if cube == (r, g, b) {
        return cube_index;
    }
    let mean = (u16::from(r) + u16::from(g) + u16::from(b)) / 3;
    let grey_step = if mean > 238 {
        23
    } else {
        (mean.saturating_sub(3) / 10).min(23) as u8
    };
    let grey = 8 + 10 * grey_step;
    if distance((grey, grey, grey), (r, g, b)) < distance(cube, (r, g, b)) {
        232 + grey_step
    } else {
        cube_index
    }
}

fn indexed_rgb(i: u8) -> (u8, u8, u8) {
    if i >= 232 {
        let v = 8 + 10 * (i - 232);
        return (v, v, v);
    }
    let i = i.saturating_sub(16);
    (
        CUBE[usize::from(i / 36)],
        CUBE[usize::from((i / 6) % 6)],
        CUBE[usize::from(i % 6)],
    )
}

/// Pick the named colour by hue, not by RGB distance: the user's scheme decides
/// what "green" looks like, so only the family and the brightness matter.
fn ansi16(r: u8, g: u8, b: u8) -> Color {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let chroma = f32::from(max - min);
    if chroma < 48.0 {
        return match max {
            0..=0x30 => Color::Black,
            0x31..=0xb0 => Color::DarkGray,
            0xb1..=0xe0 => Color::Gray,
            _ => Color::White,
        };
    }
    let (rf, gf, bf) = (f32::from(r), f32::from(g), f32::from(b));
    let sector = if r == max {
        ((gf - bf) / chroma).rem_euclid(6.0)
    } else if g == max {
        (bf - rf) / chroma + 2.0
    } else {
        (rf - gf) / chroma + 4.0
    };
    let hue = sector * 60.0;
    let bright = max >= 0xc0;
    let pick = |dark: Color, light: Color| if bright { light } else { dark };
    match hue {
        h if !(20.0..330.0).contains(&h) => pick(Color::Red, Color::LightRed),
        h if h < 75.0 => pick(Color::Yellow, Color::LightYellow),
        h if h < 160.0 => pick(Color::Green, Color::LightGreen),
        h if h < 200.0 => pick(Color::Cyan, Color::LightCyan),
        h if h < 250.0 => pick(Color::Blue, Color::LightBlue),
        _ => pick(Color::Magenta, Color::LightMagenta),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn cube_corners_are_exact() {
        assert_eq!(xterm256(0, 0, 0), 16);
        assert_eq!(xterm256(0xff, 0xff, 0xff), 231);
        assert_eq!(xterm256(0x5f, 0x87, 0xaf), 16 + 36 + 12 + 3);
    }

    #[test]
    fn greys_land_on_the_grey_ramp() {
        assert_eq!(xterm256(0x16, 0x16, 0x16), 232 + 1);
        assert_eq!(xterm256(0x80, 0x80, 0x80), 244);
    }

    #[test]
    fn indexed_round_trips_through_rgb() {
        for i in 16..=255u8 {
            let (r, g, b) = indexed_rgb(i);
            assert_eq!(indexed_rgb(xterm256(r, g, b)), (r, g, b), "index {i}");
        }
    }

    #[test]
    fn ansi16_keeps_each_hue_family() {
        assert_eq!(ansi16(0x5f, 0xd4, 0x97), Color::LightGreen);
        assert_eq!(ansi16(0x23, 0x79, 0x4f), Color::Green);
        assert_eq!(ansi16(0xae, 0x8c, 0xf5), Color::LightMagenta);
        assert_eq!(ansi16(0x5c, 0xbf, 0xe8), Color::LightCyan);
        assert_eq!(ansi16(0x78, 0xa2, 0xf7), Color::LightBlue);
        assert_eq!(ansi16(0xff, 0xb3, 0x47), Color::LightYellow);
        assert_eq!(ansi16(0xff, 0x5f, 0x6d), Color::LightRed);
        assert_eq!(ansi16(0x95, 0x9c, 0xa5), Color::DarkGray);
        assert_eq!(ansi16(0x10, 0x10, 0x10), Color::Black);
    }

    #[test]
    fn no_color_drops_every_colour() {
        assert_eq!(ColorDepth::NoColor.map(Color::Green), Color::Reset);
        assert_eq!(ColorDepth::NoColor.map(Color::Rgb(1, 2, 3)), Color::Reset);
        assert_eq!(
            ColorDepth::TrueColor.map(Color::Rgb(1, 2, 3)),
            Color::Rgb(1, 2, 3)
        );
    }

    fn rgb_grid() -> impl Iterator<Item = (u8, u8, u8)> {
        (0..=255_u16).step_by(17).flat_map(|r| {
            (0..=255_u16).step_by(17).flat_map(move |g| {
                (0..=255_u16)
                    .step_by(17)
                    .map(move |b| (r as u8, g as u8, b as u8))
            })
        })
    }

    #[test]
    fn xterm256_never_lands_in_the_16_named_slots() {
        for (r, g, b) in rgb_grid() {
            assert!(xterm256(r, g, b) >= 16, "{r},{g},{b}");
        }
    }

    #[test]
    fn xterm256_picks_the_closer_of_cube_and_grey() {
        for (r, g, b) in rgb_grid() {
            let got = distance(indexed_rgb(xterm256(r, g, b)), (r, g, b));
            let cube = (
                CUBE[usize::from(cube_step(r))],
                CUBE[usize::from(cube_step(g))],
                CUBE[usize::from(cube_step(b))],
            );
            assert!(got <= distance(cube, (r, g, b)), "{r},{g},{b}");
        }
    }

    #[test]
    fn cube_step_is_monotone_and_hits_every_level() {
        let mut last = 0;
        let mut seen = [false; 6];
        for v in 0..=255_u8 {
            let s = cube_step(v);
            assert!(s < 6 && s >= last, "{v} -> {s}");
            seen[usize::from(s)] = true;
            last = s;
        }
        assert!(seen.iter().all(|s| *s));
        for (i, level) in CUBE.iter().enumerate() {
            assert_eq!(usize::from(cube_step(*level)), i, "cube level {level:#x}");
        }
    }

    #[test]
    fn every_depth_is_idempotent() {
        let samples = [
            Color::Rgb(0x5f, 0xd4, 0x97),
            Color::Rgb(0, 0, 0),
            Color::Rgb(255, 255, 255),
            Color::Rgb(0x80, 0x80, 0x80),
            Color::Indexed(200),
            Color::Indexed(3),
            Color::Green,
            Color::Reset,
        ];
        for depth in [
            ColorDepth::TrueColor,
            ColorDepth::Ansi256,
            ColorDepth::Ansi16,
            ColorDepth::NoColor,
        ] {
            for c in samples {
                let once = depth.map(c);
                assert_eq!(depth.map(once), once, "{depth:?} {c:?}");
            }
        }
    }

    #[test]
    fn lowered_depths_never_emit_rgb() {
        for (r, g, b) in rgb_grid() {
            let c = Color::Rgb(r, g, b);
            assert!(!matches!(ColorDepth::Ansi256.map(c), Color::Rgb(..)));
            let sixteen = ColorDepth::Ansi16.map(c);
            assert!(
                !matches!(sixteen, Color::Rgb(..) | Color::Indexed(_)),
                "{c:?} -> {sixteen:?}"
            );
            assert_eq!(ColorDepth::NoColor.map(c), Color::Reset);
        }
        for i in 16..=255_u8 {
            assert!(!matches!(
                ColorDepth::Ansi16.map(Color::Indexed(i)),
                Color::Indexed(_)
            ));
        }
    }

    #[test]
    fn ansi16_greys_follow_brightness() {
        let rank = |c: Color| match c {
            Color::Black => 0,
            Color::DarkGray => 1,
            Color::Gray => 2,
            Color::White => 3,
            other => panic!("grey mapped to hue {other:?}"),
        };
        let mut last = 0;
        for v in 0..=255_u8 {
            let r = rank(ansi16(v, v, v));
            assert!(r >= last, "{v}");
            last = r;
        }
    }

    #[test]
    fn ansi16_primaries_keep_their_family() {
        let cases = [
            ((0xff, 0, 0), Color::LightRed),
            ((0x80, 0, 0), Color::Red),
            ((0, 0xff, 0), Color::LightGreen),
            ((0, 0x80, 0), Color::Green),
            ((0, 0, 0xff), Color::LightBlue),
            ((0, 0xff, 0xff), Color::LightCyan),
            ((0xff, 0, 0xff), Color::LightMagenta),
            ((0xff, 0xff, 0), Color::LightYellow),
        ];
        for ((r, g, b), want) in cases {
            assert_eq!(ansi16(r, g, b), want, "{r:#x},{g:#x},{b:#x}");
        }
    }

    #[test]
    fn indexed_rgb_grey_ramp_is_strictly_increasing() {
        let mut last = None;
        for i in 232..=255_u8 {
            let (r, g, b) = indexed_rgb(i);
            assert!(r == g && g == b);
            if let Some(prev) = last {
                assert!(r > prev);
            }
            last = Some(r);
        }
    }
}
