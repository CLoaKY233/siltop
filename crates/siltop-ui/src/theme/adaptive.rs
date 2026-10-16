//! The adaptive theme and glass surfaces.
//!
//! `Theme::from_terminal` builds a theme out of the user's own terminal palette
//! (OSC 10/11/4 replies): each panel role has a hue [`Family`] and takes the
//! palette colour closest to it (or a synthesised one when the palette has
//! none chromatic enough), greys are their foreground mixed toward their background in `OKLab`, and every
//! token is then *hardened*: walked in `OKLCH` lightness until it reads against
//! each background it may sit on. On a translucent window those backgrounds
//! include the worst case the wallpaper can make of the panel.

use ratatui::style::{Color, Modifier, Style};

use super::color::{Oklch, Rgb, contrast, delta_e, ensure_contrast, hue_turn, mix, shade};
use super::palettes;
use super::tokens::{Heat, Ramp, Theme};

/// Contrast floors (WCAG ratios) every hardened token must meet.
pub(super) const TEXT_FLOOR: f32 = 4.5;
pub(super) const GRAPH_FLOOR: f32 = 3.0;
/// Panel accents (and the warn/crit states) sit at least this far apart in ΔE OK.
pub(super) const ACCENT_SEPARATION: f32 = 0.08;

/// A bright, blurred wallpaper averages out near this grey; on glass it is the
/// worst case a dark theme's text must still read against.
const BRIGHT_BACKDROP: Rgb = Rgb(0x8a, 0x8a, 0x8a);
const DARK_BACKDROP: Rgb = Rgb(0, 0, 0);

/// Whole-percent background opacity of a translucent terminal window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opacity(u8);

impl Opacity {
    /// `None` for an opaque (≥ 0.995) or nonsensical value.
    #[must_use]
    pub fn new(alpha: f32) -> Option<Self> {
        (alpha.is_finite() && (0.0..0.995).contains(&alpha))
            .then(|| Self((alpha * 100.0).round().clamp(0.0, 99.0) as u8))
    }

    #[must_use]
    pub fn alpha(self) -> f32 {
        f32::from(self.0) / 100.0
    }
}

/// What the panels are painted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Surface {
    #[default]
    Opaque,
    /// A translucent window: explicit cell backgrounds would show as opaque
    /// slabs (Ghostty draws them at full opacity), so glass paints none.
    Glass(Opacity),
}

impl Surface {
    #[must_use]
    pub const fn is_glass(self) -> bool {
        matches!(self, Self::Glass(_))
    }

    /// Every effective background a token may land on.
    #[must_use]
    pub fn backgrounds(self, bg: Rgb) -> Vec<Rgb> {
        match self {
            Self::Opaque => vec![bg],
            Self::Glass(opacity) => vec![
                bg,
                bg.over(BRIGHT_BACKDROP, opacity.alpha()),
                bg.over(DARK_BACKDROP, opacity.alpha()),
            ],
        }
    }

    /// The floors every token is hardened to on this surface. Glass needs
    /// frames that survive a busy wallpaper; `extra` (the "extra contrast"
    /// setting, for bright wallpapers) raises the quiet inks and the frames.
    #[must_use]
    pub const fn floors(self, extra: bool) -> Floors {
        let (border, faint) = match (self, extra) {
            (Self::Opaque, false) => (1.5, 1.3),
            (Self::Opaque, true) | (Self::Glass(_), false) => (3.0, 2.0),
            (Self::Glass(_), true) => (4.5, 3.0),
        };
        Floors {
            dim: if extra { EXTRA_TEXT_FLOOR } else { TEXT_FLOOR },
            muted: if extra { TEXT_FLOOR } else { GRAPH_FLOOR },
            border,
            faint,
        }
    }
}

/// AAA text contrast: what "extra contrast" lifts dim text to.
pub(super) const EXTRA_TEXT_FLOOR: f32 = 7.0;

/// Surface-dependent floors (WCAG ratios); text and graph inks always use
/// [`TEXT_FLOOR`] and [`GRAPH_FLOOR`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Floors {
    pub dim: f32,
    pub muted: f32,
    pub border: f32,
    pub faint: f32,
}

/// The user's terminal colours as the OSC queries reported them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TermPalette {
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub ansi: [Option<Rgb>; 16],
}

/// ANSI slots by hue.
const RED: usize = 1;
const BLUE: usize = 4;
const CYAN: usize = 6;

/// A panel role's hue family: the `OKLCH` hue it aims for and how far either
/// side a palette colour may sit and still read as that family.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Family {
    pub target: f32,
    pub reach: f32,
    /// Warm families sit next to warn and crit by nature, so only ΔE (not a
    /// shared hue) keeps them apart from the states.
    pub warm: bool,
}

impl Family {
    pub(super) const CPU: Self = Self::cool(145.0, 35.0);
    pub(super) const GPU: Self = Self::cool(315.0, 45.0);
    pub(super) const MEM: Self = Self::cool(190.0, 25.0);
    pub(super) const NET: Self = Self::cool(250.0, 30.0);
    pub(super) const DISK: Self = Self::warm(92.0, 22.0);
    /// The heat panel: orange, a warm ramp that never reads as teal or blue.
    pub(super) const HEAT: Self = Self::warm(55.0, 18.0);
    /// The warn and crit states, picked the same way so a palette whose
    /// yellow slot is grey (Solarized's bright yellow) still warns in yellow.
    pub(super) const WARN: Self = Self::warm(88.0, 25.0);
    pub(super) const CRIT: Self = Self::warm(25.0, 25.0);
    /// Every panel role in the order accents are placed: earlier ones keep
    /// their colour when two collide, so heat (squeezed into oranges) goes
    /// before disk (which has all of gold to yellow to move through).
    pub(super) const PANELS: [Self; 6] = [
        Self::CPU,
        Self::GPU,
        Self::MEM,
        Self::NET,
        Self::HEAT,
        Self::DISK,
    ];

    const fn cool(target: f32, reach: f32) -> Self {
        Self {
            target,
            reach,
            warm: false,
        }
    }

    const fn warm(target: f32, reach: f32) -> Self {
        Self {
            target,
            reach,
            warm: true,
        }
    }

    /// How far (degrees) `hue` sits from this family's target.
    #[must_use]
    pub(super) fn distance(self, hue: f32) -> f32 {
        hue_turn(self.target, hue).abs()
    }

    #[must_use]
    pub(super) fn contains(self, hue: f32) -> bool {
        self.distance(hue) <= self.reach
    }
}

/// Chroma a palette colour needs before it may stand for a role.
pub(super) const PICK_CHROMA: f32 = 0.06;
/// No hardened panel accent is greyer than this.
pub(super) const ACCENT_CHROMA: f32 = 0.05;
/// Chroma a picked accent is lifted to: muted palettes (Kanagawa Dragon's
/// sage and lavender) otherwise wash out to grey once glass lifts them.
pub(super) const VIVID_CHROMA: f32 = 0.1;
/// Hue penalty (degrees) for the variant that reads worse on this background.
const OFF_VARIANT: f32 = 8.0;

impl TermPalette {
    /// Enough to build from: both defaults and the six chromatic hues.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.fg.is_some()
            && self.bg.is_some()
            && (RED..=CYAN).all(|slot| self.hue(slot, false).is_some())
    }

    /// The normal or bright variant of an ANSI hue, whichever reads better on
    /// this background: bright on dark, normal on light.
    fn hue(&self, slot: usize, light: bool) -> Option<Rgb> {
        let (normal, bright) = (self.ansi[slot], self.ansi[slot + 8]);
        if light {
            normal.or(bright)
        } else {
            bright.or(normal)
        }
    }

    /// The palette colour (either variant) closest in hue to `family`'s
    /// target among those chromatic enough and inside its reach.
    fn pick(&self, family: Family, light: bool) -> Option<Rgb> {
        (RED..=CYAN)
            .flat_map(|slot| {
                let preferred = self.hue(slot, light);
                [self.ansi[slot], self.ansi[slot + 8]]
                    .into_iter()
                    .flatten()
                    .map(move |ink| (ink, Some(ink) != preferred))
            })
            .filter_map(|(ink, off)| {
                let lch = Oklch::from(ink);
                let penalty = if off { OFF_VARIANT } else { 0.0 };
                (lch.c >= PICK_CHROMA && family.contains(lch.h))
                    .then(|| (ink, family.distance(lch.h) + penalty))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(ink, _)| ink)
    }

    /// `family`'s target hue at the palette's own lightness and (floored)
    /// chroma, for a palette that has no colour of that family.
    fn synthesize(&self, family: Family, light: bool) -> Rgb {
        let inks: Vec<Oklch> = (RED..=CYAN)
            .filter_map(|slot| self.hue(slot, light))
            .map(Oklch::from)
            .collect();
        let n = inks.len().max(1) as f32;
        Oklch {
            l: inks.iter().map(|lch| lch.l).sum::<f32>() / n,
            c: (inks.iter().map(|lch| lch.c).sum::<f32>() / n).clamp(0.1, 0.15),
            h: family.target,
        }
        .into()
    }

    /// The colour a role starts from before hardening: the palette's own
    /// when it has one of the family, lifted to [`VIVID_CHROMA`] so a muted
    /// palette's graphs still read as their hue rather than as beige.
    fn accent(&self, family: Family, light: bool) -> Rgb {
        self.pick(family, light).map_or_else(
            || self.synthesize(family, light),
            |ink| {
                let lch = Oklch::from(ink);
                if lch.c >= VIVID_CHROMA {
                    ink
                } else {
                    Oklch {
                        c: VIVID_CHROMA,
                        ..lch
                    }
                    .into()
                }
            },
        )
    }
}

impl Theme {
    /// Build the adaptive theme from the terminal's own palette; `None` when
    /// the terminal did not say enough.
    #[must_use]
    pub fn from_terminal(palette: &TermPalette, surface: Surface) -> Option<Self> {
        if !palette.is_usable() {
            return None;
        }
        let (fg, bg) = (palette.fg?, palette.bg?);
        let light = bg.is_light();
        let bgs = surface.backgrounds(bg);
        let body = |ink: Rgb| ensure_contrast(ink, &bgs, TEXT_FLOOR);

        let red = palette.accent(Family::CRIT, light);
        let blue = palette.hue(BLUE, light)?;
        let crit = body(red);
        let warn = body(palette.accent(Family::WARN, light));
        let mut accents = Family::PANELS.map(|family| body(palette.accent(family, light)));
        separate(&mut accents, &Family::PANELS, &[warn, crit], &bgs);
        let [cpu, gpu, mem, net, heat, disk] = accents;

        let toward_bg = if light { 0.12 } else { -0.12 };
        // The accent itself is the graph's top row; the foot sinks toward the
        // background and the crest lifts away from it, both at full chroma.
        let ramp = |ink: Rgb| {
            Ramp::new(
                shade(ink, toward_bg * 1.6, 0.9, -6.0).color(),
                ink.color(),
                shade(ink, -toward_bg * 0.5, 1.0, 0.0).color(),
            )
        };
        let deeper = |ink: Rgb| ramp(shade(ink, toward_bg * 0.6, 1.0, -10.0));
        // Heat runs yellow at its foot, the accent's orange, red at its crest.
        let heat_ramp = Ramp::new(
            shade(heat, toward_bg * 1.6, 0.9, 18.0).color(),
            heat.color(),
            shade(heat, -toward_bg * 0.5, 1.0, -18.0).color(),
        );
        let neutral = mix(fg, blue, 0.15);
        let hi = cpu;
        let theme = Theme {
            name: "auto",
            fg: fg.color(),
            title: fg.color(),
            dim: mix(fg, bg, 0.3).color(),
            muted: mix(fg, bg, 0.5).color(),
            faint: mix(fg, bg, 0.78).color(),
            border: mix(fg, bg, if surface.is_glass() { 0.45 } else { 0.72 }).color(),
            hi: hi.color(),
            sel_bg: mix(bg, hi, 0.2).color(),
            sel_idle: mix(bg, fg, 0.08).color(),
            kill_bg: mix(bg, red, 0.38).color(),
            reverse_selection: false,
            ok: cpu.color(),
            warn: warn.color(),
            crit: crit.color(),
            cpu: ramp(cpu),
            gpu: ramp(gpu),
            mem: ramp(mem),
            net: ramp(net),
            net_up: deeper(net),
            disk: ramp(disk),
            disk_write: deeper(disk),
            fan: ramp(neutral),
            temp: heat_ramp,
            heat: Heat::new(heat_stops(heat, warn, crit, &bgs)),
            ..palettes::BILLABONG
        };
        Some(theme.harden(surface, &bgs, surface.floors(false)))
    }

    /// Put this theme on `surface`. `bg` is the terminal background when known;
    /// otherwise the theme's own reference background stands in.
    #[must_use]
    pub fn on_surface(self, surface: Surface, bg: Option<Rgb>) -> Self {
        self.on_surface_with(surface, bg, false)
    }

    /// [`Theme::on_surface`] with the "extra contrast" setting: `extra`
    /// raises the dim, muted and frame floors (see [`Surface::floors`]).
    #[must_use]
    pub fn on_surface_with(mut self, surface: Surface, bg: Option<Rgb>, extra: bool) -> Self {
        // A light theme on a dark terminal (Paper one `T` press from auto)
        // painted its near-black hero numbers on near-black; built for the
        // other polarity, it is hardened against the real background.
        let foreign = bg.is_some_and(|bg| bg.is_light() != self.reference_background().is_light());
        if !surface.is_glass() && !extra && !foreign {
            // Built-ins meet the opaque floors natively.
            self.surface = surface;
            return self;
        }
        let bg = bg.unwrap_or_else(|| self.reference_background());
        let bgs = surface.backgrounds(bg);
        self.harden(surface, &bgs, surface.floors(extra))
    }

    fn reference_background(&self) -> Rgb {
        let dark_theme = Rgb::of(self.dim).is_none_or(Rgb::is_light);
        if dark_theme {
            Rgb::hex(0x16_1616)
        } else {
            Rgb::hex(0xff_ffff)
        }
    }

    /// Walk every RGB token up to its floor against `bgs`. Named ANSI colours
    /// are left to the terminal.
    pub(super) fn harden(mut self, surface: Surface, bgs: &[Rgb], floors: Floors) -> Self {
        let lift = |color: Color, floor: f32| {
            Rgb::of(color).map_or(color, |ink| ensure_contrast(ink, bgs, floor).color())
        };
        let ramp = |r: Ramp, body_floor: f32| {
            Ramp::new(
                lift(r.foot, GRAPH_FLOOR),
                lift(r.body, body_floor),
                lift(r.crest, TEXT_FLOOR),
            )
        };
        self.surface = surface;
        // Text never reads quieter than the labels beside it: when "extra
        // contrast" lifts dim above the text floor, text and bodies follow.
        let text = TEXT_FLOOR.max(floors.dim);
        for ink in [
            &mut self.fg,
            &mut self.title,
            &mut self.hi,
            &mut self.ok,
            &mut self.warn,
            &mut self.crit,
        ] {
            *ink = lift(*ink, text);
        }
        self.dim = lift(self.dim, floors.dim);
        self.muted = lift(self.muted, floors.muted);
        self.border = lift(self.border, floors.border);
        self.faint = lift(self.faint, floors.faint);
        for r in [
            &mut self.cpu,
            &mut self.gpu,
            &mut self.mem,
            &mut self.net,
            &mut self.disk,
            &mut self.fan,
            &mut self.temp,
        ] {
            *r = ramp(*r, text);
        }
        self.net_up = ramp(self.net_up, GRAPH_FLOOR);
        self.disk_write = ramp(self.disk_write, GRAPH_FLOOR);
        self.heat = Heat::new(self.heat.stops.map(|stop| lift(stop, text)));
        if let Some(&bg) = bgs.first() {
            self.hold_rows_on_fills(surface, bgs, bg);
            self.sel_bg = self.readable_fill(self.sel_bg, bg);
            self.sel_idle = self.readable_fill(self.sel_idle, bg);
        }
        self
    }

    /// The selection fills are backgrounds too. A fill authored for the other
    /// polarity (Billabong's navy on a white terminal) becomes a tint of
    /// itself toward `bg`, and on an opaque surface the row inks are held
    /// against the fills as well as `bgs`. Inks that sat exactly on their
    /// floor over `bg` left no fill that could read, so easing collapsed the
    /// selected row onto the background.
    fn hold_rows_on_fills(&mut self, surface: Surface, bgs: &[Rgb], bg: Rgb) {
        for fill in [&mut self.sel_bg, &mut self.sel_idle] {
            if let Some(rgb) = Rgb::of(*fill)
                && rgb.is_light() != bg.is_light()
            {
                *fill = mix(bg, rgb, 0.2).color();
            }
        }
        // Glass paints no fill (see `glass_selection`).
        if surface.is_glass() {
            return;
        }
        let grounds: Vec<Rgb> = bgs
            .iter()
            .copied()
            .chain([self.sel_bg, self.sel_idle].into_iter().filter_map(Rgb::of))
            .collect();
        let hold = |color: Color, floor: f32| {
            Rgb::of(color).map_or(color, |ink| ensure_contrast(ink, &grounds, floor).color())
        };
        for ink in [&mut self.fg, &mut self.title, &mut self.dim] {
            *ink = hold(*ink, TEXT_FLOOR);
        }
        self.hi = hold(self.hi, GRAPH_FLOOR);
        for r in [
            &mut self.cpu,
            &mut self.gpu,
            &mut self.mem,
            &mut self.net,
            &mut self.disk,
        ] {
            r.body = hold(r.body, GRAPH_FLOOR);
        }
    }

    /// `fill` eased toward `bg` until the row inks read on it: text and
    /// labels at [`TEXT_FLOOR`], the accent and graph bodies at
    /// [`GRAPH_FLOOR`]. The selection fill is a background, so hardening the
    /// inks against `bg` alone left a focused row's pid and name at ~3.6:1.
    fn readable_fill(&self, fill: Color, bg: Rgb) -> Color {
        let Some(fill) = Rgb::of(fill) else {
            return fill;
        };
        let rgb = |color: Color| Rgb::of(color);
        let text: Vec<Rgb> = [self.fg, self.title, self.dim]
            .into_iter()
            .filter_map(rgb)
            .collect();
        let bodies: Vec<Rgb> = [
            self.hi,
            self.cpu.body,
            self.gpu.body,
            self.mem.body,
            self.net.body,
            self.disk.body,
        ]
        .into_iter()
        .filter_map(rgb)
        .collect();
        let reads = |fill: Rgb| {
            text.iter().all(|ink| contrast(*ink, fill) >= TEXT_FLOOR)
                && bodies.iter().all(|ink| contrast(*ink, fill) >= GRAPH_FLOOR)
        };
        (0..=10u8)
            .map(|step| mix(fill, bg, f32::from(step) / 10.0))
            .find(|candidate| reads(*candidate))
            .unwrap_or(bg)
            .color()
    }

    /// Selection on glass: no fill at all. The accent gutter plus bold,
    /// accent-tinted ink carries the row; a kill-armed row turns alarm ink.
    pub(super) fn glass_selection(&self, focused: bool, kill: bool) -> Style {
        match (kill, focused) {
            (true, _) => Style::default()
                .fg(self.crit)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            (false, true) => Style::default().fg(self.hi).add_modifier(Modifier::BOLD),
            (false, false) => Style::default().add_modifier(Modifier::BOLD),
        }
    }
}

/// Heat stops, coldest first: yellow-gold, amber, the heat accent's orange,
/// red-orange, then crit's red. Warm all the way; never teal or blue. The two
/// cool stops (45 and 70 °C) share warn's hue, so they are held clear of its
/// ink: an idle machine must never read as a warning.
fn heat_stops(heat: Rgb, warn: Rgb, crit: Rgb, bgs: &[Rgb]) -> [Color; 5] {
    let cool = |dh: f32| {
        clear_of(
            ensure_contrast(shade(heat, 0.0, 1.0, dh), bgs, TEXT_FLOOR),
            warn,
            bgs,
        )
    };
    [cool(38.0), cool(19.0), heat, mix(heat, crit, 0.5), crit].map(Rgb::color)
}

/// `ink` moved until it sits [`ACCENT_SEPARATION`] from `state`: lightness
/// steps away from the state's (or, level with it, away from the background)
/// while chroma drains toward grey, re-hardened against `bgs` at each step.
/// The step that got furthest wins when none clears.
fn clear_of(ink: Rgb, state: Rgb, bgs: &[Rgb]) -> Rgb {
    let (lch, from) = (Oklch::from(ink), Oklch::from(state));
    let away_from_bg = bgs
        .first()
        .map_or(1.0, |bg| if bg.is_light() { -1.0 } else { 1.0 });
    let away = if (lch.l - from.l).abs() < 0.02 {
        away_from_bg
    } else {
        (lch.l - from.l).signum()
    };
    let mut best = (ink, delta_e(ink, state));
    for step in 1..=20u8 {
        if best.1 >= ACCENT_SEPARATION {
            break;
        }
        let k = f32::from(step);
        let candidate = Oklch {
            l: (lch.l + away * 0.01 * k).clamp(0.0, 1.0),
            c: lch.c * (1.0 - 0.04 * k),
            h: lch.h,
        };
        let moved = ensure_contrast(candidate.into(), bgs, TEXT_FLOOR);
        let gap = delta_e(moved, state);
        if gap > best.1 {
            best = (moved, gap);
        }
    }
    best.0
}

/// Hue gap (`OKLCH` degrees) a cool panel accent keeps from warn and crit.
pub(super) const STATE_HUE_GAP: f32 = 35.0;

/// Two chromatic inks whose hues sit within [`STATE_HUE_GAP`].
pub(super) fn hue_clash(a: Rgb, b: Rgb) -> bool {
    let (a, b) = (Oklch::from(a), Oklch::from(b));
    a.c > 0.04 && b.c > 0.04 && hue_turn(a.h, b.h).abs() < STATE_HUE_GAP
}

/// How clearly `ink` stands for `family` next to `others` and the `states`:
/// its ΔE OK to the nearest of them, or 0 when it has left its family, faded
/// to grey, or (for a cool role) taken a state's hue: Gruvbox's yellow-green
/// cpu once read as a warning that way.
fn standing(ink: Rgb, family: Family, others: &[Rgb], states: &[Rgb]) -> f32 {
    let lch = Oklch::from(ink);
    let strays = !family.contains(lch.h) || lch.c < ACCENT_CHROMA;
    // Warm roles share the states' hues by nature (heat runs up to crit's red),
    // so only a cool role fails on a shared hue; every role still keeps its
    // ΔE from warn and crit, or a calm disk paints in the warning's own ink.
    let clashes = !family.warm && states.iter().any(|state| hue_clash(ink, *state));
    if strays || clashes {
        return 0.0;
    }
    others
        .iter()
        .chain(states)
        .map(|other| delta_e(ink, *other))
        .fold(f32::INFINITY, f32::min)
}

/// Nudge accents apart: each one that sits closer than [`ACCENT_SEPARATION`]
/// to an earlier accent or a state colour (or that fails its family) tries
/// hue turns inside its family first, then lightness steps away from the
/// background (glass leaves little chroma headroom up there), and gains
/// chroma so greyish palettes separate. The first candidate that clears
/// everything wins; failing that, the one that got furthest.
fn separate(accents: &mut [Rgb], families: &[Family], states: &[Rgb], bgs: &[Rgb]) {
    for (i, family) in families.iter().enumerate().take(accents.len()) {
        let (earlier, rest) = accents.split_at_mut(i);
        let Some(accent) = rest.first_mut() else {
            continue;
        };
        if standing(*accent, *family, earlier, states) < ACCENT_SEPARATION {
            *accent = nudge(*accent, *family, earlier, states, bgs);
        }
    }
}

/// [`separate`] for a theme someone else authored (a btop import): an accent
/// keeps its own hue, even outside its family, while it reads on its own. It
/// moves (into its family) only when it sits within [`ACCENT_SEPARATION`] of
/// an earlier accent or a state, or when a cool role wears a state's hue:
/// btop Default's red-brown mem once read as crit that way.
pub(super) fn keep_clear(accents: &mut [Rgb], families: &[Family], states: &[Rgb], bgs: &[Rgb]) {
    for (i, family) in families.iter().enumerate().take(accents.len()) {
        let (earlier, rest) = accents.split_at_mut(i);
        let Some(accent) = rest.first_mut() else {
            continue;
        };
        let clashes = !family.warm && states.iter().any(|state| hue_clash(*accent, *state));
        let crowded = earlier
            .iter()
            .chain(states)
            .any(|other| delta_e(*accent, *other) < ACCENT_SEPARATION);
        if clashes || crowded {
            *accent = nudge(*accent, *family, earlier, states, bgs);
        }
    }
}

/// `accent` moved until it stands clear of `earlier` and `states`: hue turns
/// inside its family first, then lightness steps away from the background,
/// with chroma gained on the way. The first candidate that clears everything
/// wins; failing that, the one that got furthest.
fn nudge(accent: Rgb, family: Family, earlier: &[Rgb], states: &[Rgb], bgs: &[Rgb]) -> Rgb {
    const TURNS: [f32; 13] = [
        0.0, 10.0, -10.0, 20.0, -20.0, 30.0, -30.0, 40.0, -40.0, 50.0, -50.0, 60.0, -60.0,
    ];
    let away = bgs
        .first()
        .map_or(1.0, |bg| if bg.is_light() { -1.0 } else { 1.0 });
    let base = Oklch::from(accent);
    // Turn from the accent's own hue when it is in the family, else from
    // the family's target.
    let from = if family.contains(base.h) {
        base.h
    } else {
        family.target
    };
    let candidates = [
        (0.0, 1.3),
        (0.05, 1.3),
        (-0.05, 1.3),
        (0.1, 1.3),
        (-0.1, 1.6),
        (0.15, 1.6),
    ]
    .into_iter()
    .flat_map(|(dl, lift)| {
        TURNS
            .into_iter()
            .map(move |turn| from + turn)
            .filter(|h| family.contains(*h))
            .map(move |h| Oklch {
                l: base.l + dl * away,
                c: base.c.max(0.1) * lift,
                h,
            })
    });
    let mut best = (accent, standing(accent, family, earlier, states));
    for candidate in candidates {
        let ink = ensure_contrast(candidate.into(), bgs, TEXT_FLOOR);
        let gap = standing(ink, family, earlier, states);
        if gap >= ACCENT_SEPARATION {
            return ink;
        }
        if gap > best.1 {
            best = (ink, gap);
        }
    }
    best.0
}

/// Whether `ink` reads as crit's red: chromatic and inside [`Family::CRIT`].
pub(super) fn wears_crit(ink: Rgb) -> bool {
    let lch = Oklch::from(ink);
    lch.c > 0.04 && Family::CRIT.contains(lch.h)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreadable_literal
)]
mod tests {
    use super::super::color::worst_contrast;
    use super::super::palettes::ALL;
    use super::*;

    /// What separation survives near white: a mid-grey panel on strong glass
    /// forces accents so light that sRGB cannot hold eight hues 0.08 apart.
    const GLASS_SEPARATION_FLOOR: f32 = 0.065;
    /// What a warm role (disk, heat) keeps from warn and crit when that
    /// mid-grey panel sits on strong glass: plainly not the same ink.
    const NEAR_WHITE_STATE_FLOOR: f32 = 0.045;

    /// `name|bg|fg|p0,…,p15`, extracted from the themes Ghostty ships.
    const GHOSTTY: &str = include_str!("fixtures/ghostty-palettes.txt");

    fn hex(text: &str) -> Rgb {
        Rgb::hex(u32::from_str_radix(text, 16).unwrap())
    }

    fn fixtures() -> Vec<(&'static str, TermPalette)> {
        GHOSTTY
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| {
                let mut parts = line.split('|');
                let name = parts.next().unwrap();
                let bg = hex(parts.next().unwrap());
                let fg = hex(parts.next().unwrap());
                let mut ansi = [None; 16];
                for (slot, text) in parts.next().unwrap().split(',').enumerate() {
                    ansi[slot] = Some(hex(text));
                }
                (
                    name,
                    TermPalette {
                        fg: Some(fg),
                        bg: Some(bg),
                        ansi,
                    },
                )
            })
            .collect()
    }

    fn fixture(wanted: &str) -> TermPalette {
        fixtures()
            .into_iter()
            .find(|(name, _)| *name == wanted)
            .unwrap()
            .1
    }

    fn glass(alpha: f32) -> Surface {
        Surface::Glass(Opacity::new(alpha).unwrap())
    }

    fn rgb(color: Color) -> Rgb {
        Rgb::of(color).unwrap()
    }

    fn panel_ramps(theme: &Theme) -> [(&'static str, Ramp); 7] {
        [
            ("cpu", theme.cpu),
            ("gpu", theme.gpu),
            ("mem", theme.mem),
            ("net", theme.net),
            ("disk", theme.disk),
            ("fan", theme.fan),
            ("temp", theme.temp),
        ]
    }

    fn text_inks(theme: &Theme) -> Vec<(&'static str, Rgb)> {
        let mut inks = vec![
            ("fg", theme.fg),
            ("title", theme.title),
            ("dim", theme.dim),
            ("hi", theme.hi),
            ("ok", theme.ok),
            ("warn", theme.warn),
            ("crit", theme.crit),
        ];
        for (name, ramp) in panel_ramps(theme) {
            inks.push((name, ramp.body));
            inks.push((name, ramp.crest));
        }
        inks.extend(theme.heat.stops.iter().map(|stop| ("heat", *stop)));
        inks.into_iter()
            .filter_map(|(name, color)| Rgb::of(color).map(|ink| (name, ink)))
            .collect()
    }

    fn graph_inks(theme: &Theme) -> Vec<(&'static str, Rgb)> {
        let mut inks = vec![("muted", theme.muted)];
        for (name, ramp) in panel_ramps(theme) {
            inks.push((name, ramp.foot));
        }
        inks.push(("net_up", theme.net_up.body));
        inks.push(("disk_write", theme.disk_write.body));
        inks.into_iter()
            .filter_map(|(name, color)| Rgb::of(color).map(|ink| (name, ink)))
            .collect()
    }

    fn panel_accents(theme: &Theme) -> [Rgb; 6] {
        [
            theme.cpu, theme.gpu, theme.mem, theme.net, theme.disk, theme.temp,
        ]
        .map(|ramp| rgb(ramp.body))
    }

    fn assert_floors(label: &str, theme: &Theme, bgs: &[Rgb]) {
        for (token, ink) in text_inks(theme) {
            let ratio = worst_contrast(ink, bgs);
            assert!(
                ratio >= TEXT_FLOOR - 0.01,
                "{label} {token} {ink:?} is {ratio:.2}:1"
            );
        }
        for (token, ink) in graph_inks(theme) {
            let ratio = worst_contrast(ink, bgs);
            assert!(
                ratio >= GRAPH_FLOOR - 0.01,
                "{label} {token} {ink:?} is {ratio:.2}:1"
            );
        }
    }

    #[test]
    fn fixtures_cover_twenty_plus_real_palettes_light_and_dark() {
        let all = fixtures();
        assert!(all.len() >= 20, "{}", all.len());
        assert!(all.iter().any(|(_, p)| p.bg.unwrap().is_light()));
        assert!(all.iter().any(|(_, p)| !p.bg.unwrap().is_light()));
    }

    #[test]
    fn adaptive_theme_meets_contrast_floors_on_every_real_palette() {
        for (name, palette) in fixtures() {
            for surface in [Surface::Opaque, glass(0.75), glass(0.7)] {
                let theme = Theme::from_terminal(&palette, surface).unwrap();
                let bgs = surface.backgrounds(palette.bg.unwrap());
                assert_floors(&format!("{name} {surface:?}"), &theme, &bgs);
            }
        }
    }

    /// The families of [`panel_accents`], in the same order.
    const ACCENT_FAMILIES: [(&str, Family); 6] = [
        ("cpu", Family::CPU),
        ("gpu", Family::GPU),
        ("mem", Family::MEM),
        ("net", Family::NET),
        ("disk", Family::DISK),
        ("heat", Family::HEAT),
    ];

    const SURFACES: [Surface; 3] = [
        Surface::Opaque,
        Surface::Glass(Opacity(75)),
        Surface::Glass(Opacity(70)),
    ];

    fn every_theme() -> impl Iterator<Item = (String, Theme)> {
        fixtures().into_iter().flat_map(|(name, palette)| {
            SURFACES.into_iter().map(move |surface| {
                (
                    format!("{name} {surface:?}"),
                    Theme::from_terminal(&palette, surface).unwrap(),
                )
            })
        })
    }

    #[test]
    fn every_role_accent_is_chromatic_and_in_its_family() {
        for (label, theme) in every_theme() {
            for ((role, family), accent) in ACCENT_FAMILIES.iter().zip(panel_accents(&theme)) {
                let lch = Oklch::from(accent);
                assert!(
                    lch.c >= ACCENT_CHROMA,
                    "{label}: {role} {accent:?} is grey (C {:.3})",
                    lch.c
                );
                assert!(
                    family.contains(lch.h),
                    "{label}: {role} {accent:?} hue {:.0} is outside {family:?}",
                    lch.h
                );
            }
        }
    }

    #[test]
    fn adaptive_panel_accents_stay_apart_and_clear_of_alarm() {
        for (name, palette) in fixtures() {
            for surface in SURFACES {
                let theme = Theme::from_terminal(&palette, surface).unwrap();
                let accents = panel_accents(&theme);
                let states = [rgb(theme.warn), rgb(theme.crit)];
                // Warm roles (disk, heat) share the states' hues by design,
                // but never their ink: they are held to the ΔE floor below.
                for (i, accent) in accents.iter().enumerate() {
                    if ACCENT_FAMILIES[i].1.warm {
                        continue;
                    }
                    for state in &states {
                        assert!(
                            !hue_clash(*accent, *state),
                            "{name} {surface:?}: accent {i} {accent:?} shares a hue with {state:?}"
                        );
                    }
                }
                // A mid-grey panel (Zenburn #3f3f3f) on 70 % glass pushes every
                // accent near white, where sRGB has little chroma left; those get
                // the documented glass floor instead.
                let grey_panel = palette.bg.unwrap().luminance() > 0.03;
                let floor = if surface.is_glass() && grey_panel {
                    GLASS_SEPARATION_FLOOR
                } else {
                    ACCENT_SEPARATION
                };
                // A warm role and a state share a hue, so near white on glass
                // only lightness and a little chroma tell them apart.
                let state_floor = match (surface.is_glass(), grey_panel) {
                    (false, _) => ACCENT_SEPARATION,
                    (true, false) => GLASS_SEPARATION_FLOOR,
                    (true, true) => NEAR_WHITE_STATE_FLOOR,
                };
                for (i, a) in accents.iter().enumerate() {
                    let pairs = accents[..i]
                        .iter()
                        .map(|b| (b, floor))
                        .chain(states.iter().map(|b| (b, floor.min(state_floor))));
                    for (b, floor) in pairs {
                        let gap = delta_e(*a, *b);
                        assert!(
                            gap >= floor - 0.005,
                            "{name} {surface:?}: accent {i} {a:?} vs {b:?} ΔE {gap:.3}"
                        );
                    }
                }
            }
        }
    }

    /// Regression: auto picked the palette's yellow slot for both disk and
    /// warn, so a calm disk panel painted in the warning's exact ink.
    #[test]
    fn disk_panel_never_paints_in_warn_ink() {
        for (name, palette) in fixtures() {
            let theme = Theme::from_terminal(&palette, Surface::Opaque).unwrap();
            for state in [theme.warn, theme.crit] {
                let gap = delta_e(rgb(theme.disk.body), rgb(state));
                assert!(
                    gap >= ACCENT_SEPARATION,
                    "{name}: disk vs {state:?} ΔE {gap:.3}"
                );
            }
        }
    }

    /// Regression: stop 0 sat at warn's hue, so 43 °C on an idle Mac read as
    /// a warning. Stops below 80 °C keep clear of warn.
    #[test]
    fn idle_heat_stops_never_read_as_warn() {
        for (label, theme) in every_theme() {
            let warn = rgb(theme.warn);
            for stop in &theme.heat.stops[..2] {
                let gap = delta_e(rgb(*stop), warn);
                assert!(gap >= ACCENT_SEPARATION, "{label}: {stop:?} ΔE {gap:.3}");
            }
        }
    }

    /// Regression: `T` from auto reaches Paper on a dark terminal, where its
    /// hero numbers read at 1.08:1 and the selected row at 1.34:1. The
    /// reverse (Billabong on a white terminal) failed the same way.
    #[test]
    fn a_theme_on_the_other_polarity_is_hardened_to_the_terminal() {
        let cases = [
            (&palettes::PAPER, Rgb::hex(0x18_1616)),
            (&palettes::BILLABONG, Rgb::hex(0xff_ffff)),
        ];
        for (theme, bg) in cases {
            let on = theme.clone().on_surface(Surface::Opaque, Some(bg));
            let fill = rgb(on.sel_bg);
            for (token, ink) in [("title", on.title), ("dim", on.dim), ("fg", on.fg)] {
                let Some(ink) = Rgb::of(ink) else {
                    continue;
                };
                for (what, ground) in [("bg", bg), ("selection", fill)] {
                    let ratio = contrast(ink, ground);
                    assert!(
                        ratio >= TEXT_FLOOR,
                        "{} {token} on {what}: {ratio:.2}",
                        theme.name
                    );
                }
            }
        }
        // The same theme on its own polarity is left exactly as authored.
        let native = palettes::PAPER
            .clone()
            .on_surface(Surface::Opaque, Some(Rgb::hex(0xff_ffff)));
        assert_eq!(native.title, palettes::PAPER.title);
        assert_eq!(native.sel_bg, palettes::PAPER.sel_bg);
    }

    /// Regression: easing the fill of a theme hardened on the other polarity
    /// found nothing that read, so every dark built-in on a white terminal
    /// (and Paper on a dark one) painted its selected row as bare background.
    #[test]
    fn a_theme_on_the_other_polarity_keeps_a_visible_selection() {
        for theme in ALL {
            let bg = if theme.reference_background().is_light() {
                Rgb::hex(0x18_1616)
            } else {
                Rgb::hex(0xff_ffff)
            };
            let on = theme.clone().on_surface(Surface::Opaque, Some(bg));
            for (token, fill) in [("sel_bg", on.sel_bg), ("sel_idle", on.sel_idle)] {
                let Some(fill) = Rgb::of(fill) else {
                    continue;
                };
                let gap = delta_e(fill, bg);
                assert!(gap >= 0.05, "{} {token} ΔE {gap:.3} from bg", theme.name);
                for ink in [on.fg, on.title, on.dim].into_iter().filter_map(Rgb::of) {
                    let ratio = contrast(ink, fill);
                    assert!(ratio >= TEXT_FLOOR, "{} {token}: {ratio:.2}", theme.name);
                }
            }
        }
    }

    /// Regression: the adaptive selection fill was never checked, so the
    /// focused row on Nord or Solarized Light read at ~3.6:1.
    #[test]
    fn adaptive_selected_rows_stay_readable() {
        for (label, theme) in every_theme() {
            for fill in [theme.sel_bg, theme.sel_idle].map(rgb) {
                for ink in [theme.fg, theme.title, theme.dim].map(rgb) {
                    let ratio = contrast(ink, fill);
                    assert!(
                        ratio >= TEXT_FLOOR,
                        "{label}: {ink:?} on {fill:?} {ratio:.2}"
                    );
                }
                let bodies = panel_accents(&theme).into_iter().chain([rgb(theme.hi)]);
                for ink in bodies {
                    let ratio = contrast(ink, fill);
                    assert!(
                        ratio >= GRAPH_FLOOR,
                        "{label}: {ink:?} on {fill:?} {ratio:.2}"
                    );
                }
            }
        }
    }

    /// Regression: "extra contrast" lifted dim labels to 7:1 but left text
    /// at 4.5:1, so labels outshone the numbers they label.
    #[test]
    fn extra_contrast_never_ranks_labels_above_text() {
        let adaptive = fixtures().into_iter().map(|(name, palette)| {
            let theme = Theme::from_terminal(&palette, Surface::Opaque).unwrap();
            (name.to_owned(), theme, palette.bg)
        });
        let builtins = ALL
            .iter()
            .map(|theme| (theme.name.to_owned(), (*theme).clone(), None));
        for (name, theme, bg) in adaptive.chain(builtins) {
            for surface in SURFACES {
                let lifted = theme.clone().on_surface_with(surface, bg, true);
                let bgs = surface.backgrounds(bg.unwrap_or_else(|| theme.reference_background()));
                let Some(dim) = Rgb::of(lifted.dim) else {
                    continue;
                };
                let quiet = worst_contrast(dim, &bgs);
                let inks = [lifted.fg, lifted.title];
                for ink in inks.into_iter().filter_map(Rgb::of) {
                    let loud = worst_contrast(ink, &bgs);
                    assert!(
                        quiet <= loud + 0.25,
                        "{name} {surface:?}: dim {quiet:.2} outshines {ink:?} {loud:.2}"
                    );
                }
            }
        }
    }

    #[test]
    fn heat_ramp_and_stops_are_warm_and_never_mem() {
        for (label, theme) in every_theme() {
            let ramp = theme.temp;
            for ink in [ramp.foot, ramp.body, ramp.crest]
                .into_iter()
                .chain(theme.heat.stops)
                .map(rgb)
            {
                let lch = Oklch::from(ink);
                // Red through orange to yellow; never green, teal or blue.
                assert!(
                    lch.c < 0.03 || !(120.0..330.0).contains(&lch.h),
                    "{label}: heat ink {ink:?} hue {:.0}",
                    lch.h
                );
            }
            let (heat, mem) = (rgb(ramp.body), rgb(theme.mem.body));
            assert!(delta_e(heat, mem) >= ACCENT_SEPARATION, "{label}");
            let hues = theme.heat.stops.map(|stop| Oklch::from(rgb(stop)).h);
            assert!(
                hue_turn(hues[4], hues[0]) > 0.0,
                "{label}: cold stop should sit on the yellow side of hot: {hues:?}"
            );
        }
    }

    #[test]
    fn kanagawa_dragon_heat_is_not_teal_like_mem() {
        let theme = Theme::from_terminal(&fixture("Kanagawa Dragon"), glass(0.75)).unwrap();
        let heat = Oklch::from(rgb(theme.temp.body));
        let mem = Oklch::from(rgb(theme.mem.body));
        assert!(Family::HEAT.contains(heat.h), "heat {heat:?}");
        assert!(Family::MEM.contains(mem.h), "mem {mem:?}");
        assert!(hue_turn(heat.h, mem.h).abs() > 90.0);
    }

    #[test]
    fn kanagawa_dragon_cpu_and_gpu_are_not_washed_out() {
        let theme = Theme::from_terminal(&fixture("Kanagawa Dragon"), glass(0.75)).unwrap();
        for (role, family, ramp) in [
            ("cpu", Family::CPU, theme.cpu),
            ("gpu", Family::GPU, theme.gpu),
        ] {
            for ink in [ramp.body, ramp.crest].map(rgb) {
                let lch = Oklch::from(ink);
                assert!(lch.c >= 0.08, "{role} {ink:?} washed out: {lch:?}");
                assert!(family.contains(lch.h), "{role} {ink:?} left its family");
            }
            let foot = Oklch::from(rgb(ramp.foot));
            assert!(foot.c >= 0.06, "{role} foot washed out: {foot:?}");
        }
    }

    #[test]
    fn a_palette_without_a_family_gets_one_synthesised() {
        // Rose Pine Dawn's "green" slot is a teal-blue: cpu must still be green.
        let theme = Theme::from_terminal(&fixture("Rose Pine Dawn"), Surface::Opaque).unwrap();
        let cpu = Oklch::from(rgb(theme.cpu.body));
        assert!(Family::CPU.contains(cpu.h), "cpu {cpu:?}");
        assert!(cpu.c >= PICK_CHROMA, "cpu {cpu:?}");
    }

    #[test]
    fn pick_prefers_the_closest_chromatic_hue_and_skips_greys() {
        let mut palette = fixture("Kanagawa Dragon");
        // Magenta #a292a3 / #938aa9 are too grey to stand for gpu.
        assert_eq!(palette.pick(Family::GPU, false), None);
        palette.ansi[13] = Some(Rgb::hex(0xbb9af7));
        assert_eq!(palette.pick(Family::GPU, false), Some(Rgb::hex(0xbb9af7)));
        assert_eq!(palette.pick(Family::CPU, false), Some(Rgb::hex(0x87a987)));
    }

    #[test]
    fn solarized_dark_warns_in_yellow_not_its_grey_bright_yellow() {
        // Solarized's bright yellow slot (#657b83) is a grey; warn used to be it.
        let theme = Theme::from_terminal(&fixture("iTerm2 Solarized Dark"), glass(0.7)).unwrap();
        for (state, family) in [(theme.warn, Family::WARN), (theme.crit, Family::CRIT)] {
            let lch = Oklch::from(rgb(state));
            assert!(lch.c >= ACCENT_CHROMA, "{state:?} is grey: {lch:?}");
            assert!(family.contains(lch.h), "{state:?} left {family:?}");
        }
    }

    #[test]
    fn adaptive_theme_keeps_the_users_hues_when_they_already_work() {
        let theme = Theme::from_terminal(&fixture("Catppuccin Mocha"), Surface::Opaque).unwrap();
        // Bright green #89d88b is vivid and already reads on #1e1e2e: kept as is.
        assert_eq!(rgb(theme.cpu.body), Rgb::hex(0x89d88b));
        assert_eq!(rgb(theme.fg), Rgb::hex(0xcdd6f4));
        assert_eq!(theme.name, "auto");
        // A muted pick keeps its hue while gaining chroma.
        let theme = Theme::from_terminal(&fixture("Kanagawa Dragon"), Surface::Opaque).unwrap();
        let cpu = Oklch::from(rgb(theme.cpu.body));
        let user = Oklch::from(Rgb::hex(0x87a987));
        assert!(hue_turn(cpu.h, user.h).abs() < 3.0, "{cpu:?} vs {user:?}");
        assert!(cpu.c > user.c, "{cpu:?} vs {user:?}");
    }

    #[test]
    fn an_incomplete_palette_builds_nothing() {
        let mut palette = fixtures()[0].1;
        palette.fg = None;
        assert!(Theme::from_terminal(&palette, Surface::Opaque).is_none());
        let mut palette = fixtures()[0].1;
        palette.ansi[1] = None;
        palette.ansi[9] = None;
        assert!(Theme::from_terminal(&palette, Surface::Opaque).is_none());
        assert!(!TermPalette::default().is_usable());
    }

    #[test]
    fn only_one_ansi_variant_is_enough() {
        let mut palette = fixtures()[0].1;
        for slot in &mut palette.ansi[8..] {
            *slot = None;
        }
        assert!(Theme::from_terminal(&palette, Surface::Opaque).is_some());
    }

    #[test]
    fn every_builtin_on_glass_meets_floors_at_seventy_percent() {
        for theme in ALL {
            let surface = glass(0.7);
            let bg = theme.reference_background();
            let glassy = theme.clone().on_surface(surface, None);
            assert!(glassy.surface.is_glass());
            assert_floors(glassy.name, &glassy, &surface.backgrounds(bg));
        }
    }

    #[test]
    fn glass_borders_are_brighter_than_opaque_ones() {
        let base = ALL[0].clone();
        let bg = base.reference_background();
        let glassy = base.clone().on_surface(glass(0.75), None);
        assert!(contrast(rgb(glassy.border), bg) > contrast(rgb(base.border), bg));
        assert!(worst_contrast(rgb(glassy.border), &glass(0.75).backgrounds(bg)) >= 3.0);
    }

    #[test]
    fn opaque_surface_leaves_builtins_untouched() {
        // On a terminal of the theme's own polarity; the other polarity is
        // hardened (a_theme_on_the_other_polarity_is_hardened_to_the_terminal).
        for theme in ALL {
            let bg = if theme.reference_background().is_light() {
                Rgb(0xff, 0xff, 0xff)
            } else {
                Rgb(0, 0, 0)
            };
            let same = theme.clone().on_surface(Surface::Opaque, Some(bg));
            assert_eq!(&same, theme);
        }
    }

    #[test]
    fn glass_selection_paints_no_background() {
        for theme in ALL {
            let glassy = theme.clone().on_surface(glass(0.75), None);
            for focused in [true, false] {
                for kill in [true, false] {
                    let style = glassy.selection(focused, kill);
                    assert_eq!(style.bg, None, "{} {focused} {kill}", glassy.name);
                    assert!(style.add_modifier.contains(Modifier::BOLD));
                    assert!(!style.add_modifier.contains(Modifier::REVERSED));
                }
            }
        }
    }

    #[test]
    fn gruvbox_cpu_is_not_warning_yellow() {
        let theme = Theme::from_terminal(&fixture("Gruvbox Dark"), Surface::Opaque).unwrap();
        assert!(!hue_clash(rgb(theme.cpu.body), rgb(theme.warn)));
        let hue = Oklch::from(rgb(theme.cpu.body)).h;
        assert!(hue > 120.0, "cpu hue {hue} should read green, not yellow");
    }

    #[test]
    fn hue_clash_ignores_greys_and_wraps_round() {
        assert!(hue_clash(Rgb::hex(0xfabd2f), Rgb::hex(0xd79921)));
        assert!(
            !hue_clash(Rgb::hex(0x808080), Rgb::hex(0xfabd2f)),
            "grey has no hue"
        );
        assert!(
            hue_clash(Rgb::hex(0xff0040), Rgb::hex(0xff2000)),
            "wraps past 0°"
        );
        assert!(!hue_clash(Rgb::hex(0x5fd497), Rgb::hex(0xffb347)));
    }

    #[test]
    fn opacity_parsing_table() {
        for (alpha, want) in [
            (0.75, Some(75)),
            (0.0, Some(0)),
            (0.994, Some(99)),
            (0.995, None),
            (1.0, None),
            (1.5, None),
            (-0.1, None),
            (f32::NAN, None),
        ] {
            assert_eq!(Opacity::new(alpha).map(|o| o.0), want, "{alpha}");
        }
    }

    #[test]
    fn glass_backgrounds_bracket_the_panel() {
        let bg = Rgb::hex(0x14_1313);
        let bgs = glass(0.75).backgrounds(bg);
        assert_eq!(bgs.len(), 3);
        assert!(
            bgs[1].luminance() > bg.luminance(),
            "bright wallpaper lifts it"
        );
        assert!(
            bgs[2].luminance() <= bg.luminance(),
            "black wallpaper darkens it"
        );
        assert_eq!(Surface::Opaque.backgrounds(bg), vec![bg]);
    }

    #[test]
    fn graph_feet_keep_their_hue_on_glass() {
        let theme = Theme::from_terminal(&fixture("Kanagawa Dragon"), glass(0.75)).unwrap();
        for (name, ramp) in panel_ramps(&theme) {
            if name == "fan" {
                continue;
            }
            let (foot, body) = (Oklch::from(rgb(ramp.foot)), Oklch::from(rgb(ramp.body)));
            assert!(foot.c >= 0.02, "{name} foot faded to grey: {foot:?}");
            let turn = (foot.h - body.h + 540.0).rem_euclid(360.0) - 180.0;
            assert!(turn.abs() < 25.0, "{name} foot changed hue by {turn}");
        }
    }

    fn reference_bgs(reference: &super::super::palettes::Reference) -> Vec<Rgb> {
        reference
            .backgrounds
            .iter()
            .map(|bg| Rgb::hex(*bg))
            .collect()
    }

    /// The hex tables were re-derived with the `OKLCH` walk, so hardening a
    /// built-in against the backgrounds it was tuned for changes nothing.
    #[test]
    fn every_builtin_meets_floors_without_hardening() {
        for reference in &super::super::palettes::REFERENCES {
            let theme = reference.theme;
            let bgs = reference_bgs(reference);
            let hardened =
                theme
                    .clone()
                    .harden(Surface::Opaque, &bgs, Surface::Opaque.floors(false));
            assert_eq!(&hardened, theme, "{} needs hardening", theme.name);
            assert_floors(theme.name, theme, &bgs);
        }
    }

    #[test]
    fn every_rgb_builtin_has_a_reference_background() {
        for theme in ALL {
            let listed = super::super::palettes::REFERENCES
                .iter()
                .any(|r| r.theme.name == theme.name);
            assert_eq!(listed, theme.name != "terminal", "{}", theme.name);
        }
    }

    #[test]
    fn builtin_frames_meet_opaque_floors_natively() {
        let floors = Surface::Opaque.floors(false);
        for reference in &super::super::palettes::REFERENCES {
            let theme = reference.theme;
            let bgs = reference_bgs(reference);
            for (token, ink, floor) in [
                ("border", theme.border, floors.border),
                ("faint", theme.faint, floors.faint),
                ("muted", theme.muted, floors.muted),
            ] {
                let ratio = worst_contrast(rgb(ink), &bgs);
                assert!(ratio >= floor - 0.01, "{} {token} {ratio:.2}", theme.name);
            }
        }
    }

    #[test]
    fn extra_contrast_raises_quiet_inks_and_frames() {
        for surface in [Surface::Opaque, glass(0.75)] {
            let (plain, extra) = (surface.floors(false), surface.floors(true));
            assert!(extra.dim > plain.dim && extra.muted > plain.muted);
            assert!(extra.border > plain.border && extra.faint > plain.faint);
        }
        for theme in ALL {
            let bg = theme.reference_background();
            for surface in [Surface::Opaque, glass(0.75)] {
                let bgs = surface.backgrounds(bg);
                let floors = surface.floors(true);
                let raised = theme.clone().on_surface_with(surface, None, true);
                for (token, ink, floor) in [
                    ("dim", raised.dim, floors.dim),
                    ("muted", raised.muted, floors.muted),
                    ("border", raised.border, floors.border),
                    ("faint", raised.faint, floors.faint),
                ] {
                    let Some(ink) = Rgb::of(ink) else { continue };
                    let ratio = worst_contrast(ink, &bgs);
                    // Near white glass a few inks top out just short; they
                    // still move as far as sRGB allows.
                    assert!(
                        ratio >= floor.min(4.4) - 0.05,
                        "{} {surface:?} {token} {ratio:.2}",
                        theme.name
                    );
                }
            }
        }
    }

    #[test]
    fn extra_contrast_on_opaque_hardens_builtins() {
        let base = ALL[0].clone();
        let raised = base.clone().on_surface_with(Surface::Opaque, None, true);
        assert_ne!(raised.border, base.border, "frames get brighter");
        assert_eq!(raised.surface, Surface::Opaque);
        let same = base.clone().on_surface_with(Surface::Opaque, None, false);
        assert_eq!(same, base, "off leaves the built-in untouched");
    }

    #[test]
    fn new_builtins_hold_their_family_hues() {
        for name in ["kanagawa", "gruvbox-dark"] {
            let theme = Theme::by_name(name).unwrap();
            for (role, family, ink) in [
                ("cpu", Family::CPU, theme.cpu.body),
                ("gpu", Family::GPU, theme.gpu.body),
                ("net", Family::NET, theme.net.body),
            ] {
                let hue = Oklch::from(rgb(ink)).h;
                assert!(
                    family.distance(hue) <= family.reach + 20.0,
                    "{name} {role} hue {hue:.0}"
                );
            }
            assert_eq!(theme.surface, Surface::Opaque);
        }
    }
}
