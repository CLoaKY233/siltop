use ratatui::style::Color;
use unicode_width::UnicodeWidthStr;

use super::adaptive::Surface;
use super::depth::ColorDepth;
use crate::layout::Panel;

/// Every colour role a widget may ask for. Widgets never name a hex value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub name: &'static str,
    pub depth: ColorDepth,
    pub icons: Icons,
    /// Body text. `Reset` hands it to the terminal's own foreground.
    pub fg: Color,
    /// Hero numbers and keys; `Theme::title` always adds BOLD.
    pub title: Color,
    pub dim: Color,
    /// Meta and hints: quieter than `dim`, still 3:1.
    pub muted: Color,
    /// Corner marks and bar tracks.
    pub faint: Color,
    pub border: Color,
    /// Highlight ink: search hits, the selection mark, the process panel's focus ring.
    pub hi: Color,
    pub sel_bg: Color,
    /// Selection fill while the table is not focused.
    pub sel_idle: Color,
    pub kill_bg: Color,
    /// Select with REVERSED instead of a fill (ANSI and attribute-only themes).
    pub reverse_selection: bool,
    /// Opaque, or a translucent window that must never get a cell background.
    pub surface: Surface,
    pub ok: Color,
    pub warn: Color,
    pub crit: Color,
    pub cpu: Ramp,
    pub gpu: Ramp,
    pub mem: Ramp,
    /// Download; `net_up` is the deeper shade for upload.
    pub net: Ramp,
    pub net_up: Ramp,
    /// Read; `disk_write` is the deeper shade for write.
    pub disk: Ramp,
    pub disk_write: Ramp,
    pub fan: Ramp,
    pub temp: Ramp,
    pub heat: Heat,
}

/// Three stops of one hue: a deep foot, a readable body, a bright crest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ramp {
    pub foot: Color,
    pub body: Color,
    pub crest: Color,
}

impl Ramp {
    #[must_use]
    pub const fn new(foot: Color, body: Color, crest: Color) -> Self {
        Self { foot, body, crest }
    }

    #[must_use]
    pub const fn flat(ink: Color) -> Self {
        Self::new(ink, ink, ink)
    }

    /// `t` runs foot (0) → body (0.5) → crest (1), blended in RGB.
    #[must_use]
    pub fn at(&self, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        if t <= 0.5 {
            blend(self.foot, self.body, t * 2.0)
        } else {
            blend(self.body, self.crest, (t - 0.5) * 2.0)
        }
    }
}

/// A ramp stands in for its body wherever one colour is enough.
impl From<Ramp> for Color {
    fn from(ramp: Ramp) -> Self {
        ramp.body
    }
}

/// Temperature ink, anchored to degrees rather than to a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Heat {
    pub stops: [Color; 5],
}

impl Heat {
    /// Degrees at which each stop is reached; colder is stop 0, hotter the last.
    pub const AT: [f32; 5] = [45.0, 70.0, 80.0, 88.0, 95.0];

    #[must_use]
    pub const fn new(stops: [Color; 5]) -> Self {
        Self { stops }
    }

    #[must_use]
    pub fn at(&self, celsius: f32) -> Color {
        if celsius.is_nan() || celsius <= Self::AT[0] {
            return self.stops[0];
        }
        for (at, ink) in Self::AT.windows(2).zip(self.stops.windows(2)) {
            if celsius < at[1] {
                return blend(ink[0], ink[1], (celsius - at[0]) / (at[1] - at[0]));
            }
        }
        self.stops[4]
    }
}

/// Which glyph family decorates the chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Icons {
    /// Plain ASCII marks, no panel icons.
    None,
    #[default]
    Unicode,
    /// Nerd Font icons prefix each panel label.
    Nerd,
}

impl Icons {
    pub const NAMES: [&'static str; 3] = ["none", "unicode", "nerd"];

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "none" | "ascii" => Some(Self::None),
            "unicode" => Some(Self::Unicode),
            "nerd" | "nerdfont" | "nerd-font" => Some(Self::Nerd),
            _ => None,
        }
    }

    /// Columns the panel icon takes in front of a title (its gap
    /// included), which the title must leave free; 0 without one.
    #[must_use]
    pub fn panel_width(self, panel: Panel) -> usize {
        self.panel(panel).map_or(0, |glyph| 1 + glyph.width())
    }

    /// Panel label icon, only when it measures one or two cells.
    #[must_use]
    pub fn panel(self, panel: Panel) -> Option<&'static str> {
        if self != Self::Nerd {
            return None;
        }
        let glyph = match panel {
            Panel::Cpu => "\u{f4bc}",
            Panel::Gpu => "\u{f08ae}",
            Panel::Mem => "\u{f035b}",
            Panel::Net => "\u{f05a9}",
            Panel::Disk => "\u{f02ca}",
            Panel::Fans => "\u{f050f}",
            Panel::Power => "\u{f0241}",
            Panel::Processes => "\u{f018d}",
        };
        (1..=2).contains(&glyph.width()).then_some(glyph)
    }

    #[must_use]
    pub const fn select_mark(self) -> &'static str {
        match self {
            Self::None => ">",
            Self::Unicode | Self::Nerd => "▌",
        }
    }
}

/// RGB blend; a named colour cannot blend, so it steps at the midpoint.
#[must_use]
pub(crate) fn blend(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    match (a, b) {
        (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) => {
            Color::Rgb(mix(ar, br, t), mix(ag, bg, t), mix(ab, bb, t))
        }
        _ if t < 0.5 => a,
        _ => b,
    }
}

fn mix(a: u8, b: u8, t: f32) -> u8 {
    let a = f32::from(a);
    let b = f32::from(b);
    (a + (b - a) * t).round().clamp(0.0, 255.0) as u8
}

#[must_use]
pub(crate) const fn rgb(hex: u32) -> Color {
    super::color::Rgb::hex(hex).color()
}
