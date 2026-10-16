//! Colour math for theme building: sRGB ↔ linear light ↔ `OKLab` / `OKLCH`,
//! perceptual mixing, WCAG contrast and a lightness clamp that walks an ink
//! until it reads against every background it may sit on.

// The `OKLab` matrices are published with single-letter channel names (l, m, s).
#![allow(clippy::many_single_char_names)]

use ratatui::style::Color;

/// An 8-bit sRGB triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    #[must_use]
    pub const fn hex(value: u32) -> Self {
        Self((value >> 16) as u8, (value >> 8) as u8, value as u8)
    }

    #[must_use]
    pub const fn color(self) -> Color {
        Color::Rgb(self.0, self.1, self.2)
    }

    #[must_use]
    pub const fn of(color: Color) -> Option<Self> {
        match color {
            Color::Rgb(r, g, b) => Some(Self(r, g, b)),
            _ => None,
        }
    }

    fn linear(self) -> [f32; 3] {
        [self.0, self.1, self.2].map(to_linear)
    }

    fn from_linear([r, g, b]: [f32; 3]) -> Self {
        Self(from_linear(r), from_linear(g), from_linear(b))
    }

    /// WCAG relative luminance, 0 (black) to 1 (white).
    #[must_use]
    pub fn luminance(self) -> f32 {
        let [r, g, b] = self.linear();
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    #[must_use]
    pub fn is_light(self) -> bool {
        // CIE L* = 50 sits at relative luminance 0.1842.
        self.luminance() > 0.1842
    }

    /// Composite `self` at `alpha` over `backdrop` in linear light, the way a
    /// translucent terminal window blends into what is behind it.
    #[must_use]
    pub fn over(self, backdrop: Self, alpha: f32) -> Self {
        let alpha = alpha.clamp(0.0, 1.0);
        let (top, under) = (self.linear(), backdrop.linear());
        Self::from_linear([0, 1, 2].map(|i| top[i] * alpha + under[i] * (1.0 - alpha)))
    }
}

fn to_linear(channel: u8) -> f32 {
    let c = f32::from(channel) / 255.0;
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn from_linear(value: f32) -> u8 {
    let v = value.clamp(0.0, 1.0);
    let c = if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (c * 255.0).round().clamp(0.0, 255.0) as u8
}

/// WCAG contrast ratio, 1 to 21.
#[must_use]
pub(super) fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (a.luminance(), b.luminance());
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Björn Ottosson's `OKLab`: perceptual lightness `l` in 0..1 and two opponent axes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Oklab {
    pub l: f32,
    pub a: f32,
    pub b: f32,
}

/// `OKLab` in polar form: lightness, chroma and hue in degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Oklch {
    pub l: f32,
    pub c: f32,
    pub h: f32,
}

impl From<Rgb> for Oklab {
    fn from(rgb: Rgb) -> Self {
        let [r, g, b] = rgb.linear();
        let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
        let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
        let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
        Self {
            l: 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
            a: 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
            b: 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
        }
    }
}

impl Oklab {
    fn linear(self) -> [f32; 3] {
        let l = (self.l + 0.396_337_78 * self.a + 0.215_803_76 * self.b).powi(3);
        let m = (self.l - 0.105_561_346 * self.a - 0.063_854_17 * self.b).powi(3);
        let s = (self.l - 0.089_484_18 * self.a - 1.291_485_5 * self.b).powi(3);
        [
            4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
            -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
            -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
        ]
    }

    fn in_gamut(self) -> bool {
        self.linear()
            .iter()
            .all(|c| (-0.000_5..=1.000_5).contains(c))
    }

    /// Euclidean ΔE in `OKLab`; 0.02 is a just-noticeable step.
    #[must_use]
    pub fn distance(self, other: Self) -> f32 {
        ((self.l - other.l).powi(2) + (self.a - other.a).powi(2) + (self.b - other.b).powi(2))
            .sqrt()
    }
}

impl From<Oklab> for Oklch {
    fn from(lab: Oklab) -> Self {
        Self {
            l: lab.l,
            c: lab.a.hypot(lab.b),
            h: lab.b.atan2(lab.a).to_degrees().rem_euclid(360.0),
        }
    }
}

impl From<Oklch> for Oklab {
    fn from(lch: Oklch) -> Self {
        let (sin, cos) = lch.h.to_radians().sin_cos();
        Self {
            l: lch.l,
            a: lch.c * cos,
            b: lch.c * sin,
        }
    }
}

impl From<Rgb> for Oklch {
    fn from(rgb: Rgb) -> Self {
        Oklab::from(rgb).into()
    }
}

impl From<Oklch> for Rgb {
    /// Out-of-gamut colours keep hue and lightness and give up chroma.
    fn from(lch: Oklch) -> Self {
        let mut lch = Oklch {
            l: lch.l.clamp(0.0, 1.0),
            ..lch
        };
        for _ in 0..24 {
            if Oklab::from(lch).in_gamut() {
                break;
            }
            lch.c *= 0.9;
        }
        Rgb::from_linear(Oklab::from(lch).linear())
    }
}

impl From<Oklab> for Rgb {
    fn from(lab: Oklab) -> Self {
        Oklch::from(lab).into()
    }
}

/// The signed turn (degrees, -180..180) that takes hue `from` to hue `to` the
/// short way round.
#[must_use]
pub(super) fn hue_turn(from: f32, to: f32) -> f32 {
    (to - from + 540.0).rem_euclid(360.0) - 180.0
}

/// ΔE OK between two sRGB colours.
#[must_use]
pub(super) fn delta_e(a: Rgb, b: Rgb) -> f32 {
    Oklab::from(a).distance(Oklab::from(b))
}

/// Perceptual mix: `t` = 0 gives `a`, 1 gives `b`.
#[must_use]
pub(super) fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let (a, b) = (Oklab::from(a), Oklab::from(b));
    Oklab {
        l: a.l + (b.l - a.l) * t,
        a: a.a + (b.a - a.a) * t,
        b: a.b + (b.b - a.b) * t,
    }
    .into()
}

/// Rotate hue, scale chroma and shift lightness in `OKLCH`.
#[must_use]
pub(super) fn shade(ink: Rgb, dl: f32, chroma: f32, dh: f32) -> Rgb {
    let lch = Oklch::from(ink);
    Oklch {
        l: lch.l + dl,
        c: lch.c * chroma,
        h: lch.h + dh,
    }
    .into()
}

/// The lowest contrast `ink` reaches against any of `backgrounds`.
#[must_use]
pub(super) fn worst_contrast(ink: Rgb, backgrounds: &[Rgb]) -> f32 {
    backgrounds
        .iter()
        .map(|bg| contrast(ink, *bg))
        .fold(f32::INFINITY, f32::min)
}

/// Walk `ink`'s `OKLCH` lightness away from the first background until it reads
/// at `min` against every background; hue and (where the gamut allows) chroma
/// stay put, so an accent never fades to grey on the way.
#[must_use]
pub(super) fn ensure_contrast(ink: Rgb, backgrounds: &[Rgb], min: f32) -> Rgb {
    let Some(reference) = backgrounds.first() else {
        return ink;
    };
    let step = if reference.is_light() { -0.01 } else { 0.01 };
    let mut lch = Oklch::from(ink);
    let mut best = ink;
    for _ in 0..100 {
        if worst_contrast(best, backgrounds) >= min {
            return best;
        }
        lch.l = (lch.l + step).clamp(0.0, 1.0);
        best = lch.into();
        if lch.l <= 0.0 || lch.l >= 1.0 {
            break;
        }
    }
    best
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreadable_literal
)]
mod tests {
    use super::*;

    const BLACK: Rgb = Rgb(0, 0, 0);
    const WHITE: Rgb = Rgb(255, 255, 255);

    #[test]
    fn contrast_known_values() {
        assert!((contrast(BLACK, WHITE) - 21.0).abs() < 0.01);
        assert!((contrast(WHITE, WHITE) - 1.0).abs() < 1e-6);
        // #767676 is the classic 4.54:1 grey on white.
        let ratio = contrast(Rgb::hex(0x767676), WHITE);
        assert!((ratio - 4.54).abs() < 0.02, "{ratio}");
    }

    #[test]
    fn oklab_known_values() {
        let white = Oklab::from(WHITE);
        assert!((white.l - 1.0).abs() < 1e-3 && white.a.abs() < 1e-3 && white.b.abs() < 1e-3);
        let red = Oklch::from(Rgb(255, 0, 0));
        assert!((red.l - 0.628).abs() < 0.002, "{red:?}");
        assert!((red.c - 0.2577).abs() < 0.002, "{red:?}");
        assert!((red.h - 29.23).abs() < 0.2, "{red:?}");
    }

    #[test]
    fn rgb_round_trips_through_oklab_and_oklch() {
        for r in (0..=255u8).step_by(17) {
            for g in (0..=255u8).step_by(51) {
                for b in (0..=255u8).step_by(85) {
                    let rgb = Rgb(r, g, b);
                    let back = Rgb::from(Oklab::from(rgb));
                    let back_lch = Rgb::from(Oklch::from(rgb));
                    for (x, y) in [(rgb, back), (rgb, back_lch)] {
                        assert!(
                            x.0.abs_diff(y.0) <= 1
                                && x.1.abs_diff(y.1) <= 1
                                && x.2.abs_diff(y.2) <= 1,
                            "{x:?} → {y:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn mix_endpoints_are_exact_and_midpoint_is_between() {
        let (a, b) = (Rgb::hex(0x5fd497), Rgb::hex(0x141313));
        assert_eq!(mix(a, b, 0.0), a);
        assert_eq!(mix(a, b, 1.0), b);
        let mid = Oklab::from(mix(a, b, 0.5)).l;
        assert!(mid < Oklab::from(a).l && mid > Oklab::from(b).l);
    }

    #[test]
    fn over_is_linear_light_compositing() {
        assert_eq!(WHITE.over(BLACK, 1.0), WHITE);
        assert_eq!(WHITE.over(BLACK, 0.0), BLACK);
        // Half white in linear light is 0.5 linear → sRGB 188.
        assert_eq!(WHITE.over(BLACK, 0.5), Rgb(188, 188, 188));
    }

    #[test]
    fn ensure_contrast_reaches_the_floor_both_ways() {
        let dark = [Rgb::hex(0x141313), Rgb::hex(0x3a3a3a)];
        let ink = ensure_contrast(Rgb::hex(0x40454d), &dark, 4.5);
        assert!(worst_contrast(ink, &dark) >= 4.5);
        let light = [WHITE];
        let ink = ensure_contrast(Rgb::hex(0xc0c0c0), &light, 4.5);
        assert!(contrast(ink, WHITE) >= 4.5);
    }

    #[test]
    fn ensure_contrast_keeps_hue() {
        let ink = Rgb::hex(0x2f4a8a);
        let lifted = ensure_contrast(ink, &[Rgb::hex(0x141313)], 4.5);
        let (before, after) = (Oklch::from(ink), Oklch::from(lifted));
        assert!((before.h - after.h).abs() < 6.0, "{before:?} {after:?}");
        assert!(after.c > 0.05, "chroma survived: {after:?}");
    }

    #[test]
    fn ensure_contrast_leaves_good_ink_alone() {
        let ink = Rgb::hex(0xe6e8eb);
        assert_eq!(ensure_contrast(ink, &[BLACK], 4.5), ink);
        assert_eq!(ensure_contrast(ink, &[], 4.5), ink);
    }

    #[test]
    fn delta_e_is_symmetric_and_zero_on_self() {
        let (a, b) = (Rgb::hex(0x5fd497), Rgb::hex(0xae8cf5));
        assert!(delta_e(a, a) < 1e-6);
        assert!((delta_e(a, b) - delta_e(b, a)).abs() < 1e-6);
        assert!(delta_e(a, b) > 0.1);
    }

    #[test]
    fn shade_rotates_hue_and_scales_chroma() {
        let base = Oklch::from(Rgb::hex(0x78a2f7));
        let turned = Oklch::from(shade(Rgb::hex(0x78a2f7), 0.0, 0.5, 30.0));
        assert!(((turned.h - base.h).rem_euclid(360.0) - 30.0).abs() < 3.0);
        assert!(turned.c < base.c * 0.6);
    }
}
