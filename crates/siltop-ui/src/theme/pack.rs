//! The theme pack (master plan P7): extra built-ins derived from their
//! published terminal palettes with the adaptive OKLCH role method, user theme
//! files, and the btop `.theme` importer that writes them.
//!
//! The UI crate does no I/O: the binary reads `themes/*.toml`, hands the text
//! to [`Theme::from_toml`] and installs the results once at startup with
//! [`install_user_themes`]; after that every lookup (`by_name`, the settings
//! picker) sees them.

// Colours read as the six-digit hex every palette document uses.
#![allow(clippy::unreadable_literal)]

use std::fmt::Write as _;
use std::sync::{LazyLock, Mutex, OnceLock};

use ratatui::style::Color;

use super::adaptive::{Family, Surface, TermPalette, keep_clear, wears_crit};
use super::color::{Rgb, mix};
use super::palettes;
use super::tokens::{Heat, Ramp, Theme};

/// One pack entry: its name and the terminal palette it is derived from.
struct Source {
    name: &'static str,
    fg: u32,
    bg: u32,
    ansi: [u32; 16],
}

/// Published palettes, normal colours then bright, in ANSI order.
const SOURCES: [Source; 7] = [
    Source {
        name: "dracula",
        fg: 0xf8f8f2,
        bg: 0x282a36,
        ansi: [
            0x21222c, 0xff5555, 0x50fa7b, 0xf1fa8c, 0xbd93f9, 0xff79c6, 0x8be9fd, 0xf8f8f2,
            0x6272a4, 0xff6e6e, 0x69ff94, 0xffffa5, 0xd6acff, 0xff92df, 0xa4ffff, 0xffffff,
        ],
    },
    Source {
        name: "rose-pine",
        fg: 0xe0def4,
        bg: 0x191724,
        ansi: [
            0x26233a, 0xeb6f92, 0x31748f, 0xf6c177, 0x9ccfd8, 0xc4a7e7, 0xebbcba, 0xe0def4,
            0x6e6a86, 0xeb6f92, 0x31748f, 0xf6c177, 0x9ccfd8, 0xc4a7e7, 0xebbcba, 0xe0def4,
        ],
    },
    Source {
        name: "solarized-dark",
        fg: 0x839496,
        bg: 0x002b36,
        ansi: SOLARIZED,
    },
    Source {
        name: "solarized-light",
        fg: 0x657b83,
        bg: 0xfdf6e3,
        ansi: SOLARIZED,
    },
    Source {
        name: "everforest",
        fg: 0xd3c6aa,
        bg: 0x2d353b,
        ansi: [
            0x475258, 0xe67e80, 0xa7c080, 0xdbbc7f, 0x7fbbb3, 0xd699b6, 0x83c092, 0xd3c6aa,
            0x475258, 0xe67e80, 0xa7c080, 0xdbbc7f, 0x7fbbb3, 0xd699b6, 0x83c092, 0xd3c6aa,
        ],
    },
    Source {
        name: "one-dark",
        fg: 0xabb2bf,
        bg: 0x282c34,
        ansi: [
            0x282c34, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xabb2bf,
            0x5c6370, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xffffff,
        ],
    },
    Source {
        name: "catppuccin-latte",
        fg: 0x4c4f69,
        bg: 0xeff1f5,
        ansi: [
            0x5c5f77, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xacb0be,
            0x6c6f85, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xbcc0cc,
        ],
    },
];

const SOLARIZED: [u32; 16] = [
    0x073642, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5, 0x002b36,
    0xcb4b16, 0x586e75, 0x657b83, 0x839496, 0x6c71c4, 0x93a1a1, 0xfdf6e3,
];

impl Source {
    fn palette(&self) -> TermPalette {
        TermPalette {
            fg: Some(Rgb::hex(self.fg)),
            bg: Some(Rgb::hex(self.bg)),
            ansi: self.ansi.map(|hex| Some(Rgb::hex(hex))),
        }
    }
}

/// The pack, derived once. A palette the role method cannot read is
/// skipped rather than faked (every shipped one can; a test holds that).
static PACK: LazyLock<Vec<Theme>> = LazyLock::new(|| {
    SOURCES
        .iter()
        .filter_map(|source| {
            let theme = Theme::from_terminal(&source.palette(), Surface::Opaque)?;
            Some(Theme {
                name: source.name,
                ..theme
            })
        })
        .collect()
});

static USER: OnceLock<Vec<Theme>> = OnceLock::new();

/// Install the user's theme files. Only the first call counts (startup);
/// returns whether this one did. A user theme never shadows a built-in.
pub fn install_user_themes(themes: Vec<Theme>) -> bool {
    let fresh: Vec<Theme> = themes
        .into_iter()
        .filter(|theme| lookup_builtin(theme.name).is_none())
        .collect();
    USER.set(fresh).is_ok()
}

fn user() -> &'static [Theme] {
    USER.get().map_or(&[], Vec::as_slice)
}

pub(super) fn lookup_builtin(name: &str) -> Option<Theme> {
    palettes::ALL
        .iter()
        .copied()
        .chain(PACK.iter())
        .find(|theme| theme.name == name)
        .cloned()
}

/// Built-ins (core and pack), then the user's own themes.
pub(super) fn lookup(name: &str) -> Option<Theme> {
    lookup_builtin(name).or_else(|| {
        user()
            .iter()
            .find(|theme| theme.name.eq_ignore_ascii_case(name))
            .cloned()
    })
}

/// Names from a static table or a user file live for the whole run; the
/// table interns each distinct user name once, so reloads never grow it.
fn intern(name: &str) -> &'static str {
    static NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let Ok(mut names) = NAMES.lock() else {
        return "custom";
    };
    if let Some(known) = names.iter().find(|known| **known == name) {
        return known;
    }
    let leaked: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.push(leaked);
    leaked
}

impl Theme {
    /// The extra built-ins the theme pack adds, in menu order.
    #[must_use]
    pub fn pack() -> Vec<&'static str> {
        PACK.iter().map(|theme| theme.name).collect()
    }

    /// Names of the installed user themes.
    #[must_use]
    pub fn user_themes() -> Vec<&'static str> {
        user().iter().map(|theme| theme.name).collect()
    }

    /// Every choice the picker offers: [`Theme::names`] (auto and the core
    /// built-ins), then the pack, then the user's themes.
    #[must_use]
    pub fn catalog() -> Vec<&'static str> {
        Self::names()
            .into_iter()
            .chain(Self::pack())
            .chain(Self::user_themes())
            .collect()
    }

    /// Parse a user theme file. Every token is optional: missing ones come
    /// from Billabong (or Paper when `background` is light), and the result
    /// is hardened to the contrast floors against that background, so a
    /// hand-written file can never produce unreadable text.
    ///
    /// # Errors
    /// A line that is not `key = value`, an unknown key, or a bad colour.
    pub fn from_toml(text: &str, fallback_name: &str) -> Result<Self, String> {
        let mut name = fallback_name.to_owned();
        let mut background = None;
        let mut pairs = Vec::new();
        for (n, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| format!("line {}: expected key = value", n + 1))?;
            let (key, value) = (key.trim(), value.trim());
            match key {
                "name" => unquote(value).clone_into(&mut name),
                "background" => background = Some(parse_rgb(unquote(value), n)?),
                _ => pairs.push((n, key, value)),
            }
        }
        let bg = background.unwrap_or(Rgb::hex(0x161616));
        let mut theme = if bg.is_light() {
            palettes::PAPER
        } else {
            palettes::BILLABONG
        };
        for (n, key, value) in pairs {
            theme.set_token(key, value, n)?;
        }
        let name = name.trim();
        if name.is_empty() {
            return Err("theme has no name".into());
        }
        // A theme file is text from disk: its name is shown and printed.
        let name = siltop_core::printable(name);
        theme.name = intern(&name.to_ascii_lowercase().replace([' ', '_'], "-"));
        let surface = Surface::Opaque;
        Ok(theme.harden(surface, &surface.backgrounds(bg), surface.floors(false)))
    }

    /// This theme as a user theme file [`Theme::from_toml`] reads back.
    /// Named ANSI colours have no hex spelling and are left out.
    #[must_use]
    pub fn to_toml(&self, background: Option<Rgb>) -> String {
        let mut out = format!("# siltop theme\nname = \"{}\"\n", self.name);
        if let Some(bg) = background {
            let _ = writeln!(out, "background = \"{}\"", hex(bg));
        }
        let mut copy = self.clone();
        for (key, ink) in copy.inks() {
            if let Some(rgb) = spell(*ink) {
                let _ = writeln!(out, "{key} = \"{rgb}\"");
            }
        }
        for (key, ramp) in copy.ramps() {
            let stops = [ramp.foot, ramp.body, ramp.crest].map(spell);
            if let [Some(a), Some(b), Some(c)] = stops {
                let _ = writeln!(out, "{key} = [\"{a}\", \"{b}\", \"{c}\"]");
            }
        }
        let stops: Option<Vec<String>> = self.heat.stops.iter().map(|c| spell(*c)).collect();
        if let Some(stops) = stops {
            let quoted: Vec<String> = stops.iter().map(|s| format!("\"{s}\"")).collect();
            let _ = writeln!(out, "heat = [{}]", quoted.join(", "));
        }
        out
    }

    /// The single-colour tokens a theme file may set, by file key.
    fn inks(&mut self) -> [(&'static str, &mut Color); 13] {
        [
            ("fg", &mut self.fg),
            ("title", &mut self.title),
            ("dim", &mut self.dim),
            ("muted", &mut self.muted),
            ("faint", &mut self.faint),
            ("border", &mut self.border),
            ("hi", &mut self.hi),
            ("sel_bg", &mut self.sel_bg),
            ("sel_idle", &mut self.sel_idle),
            ("kill_bg", &mut self.kill_bg),
            ("ok", &mut self.ok),
            ("warn", &mut self.warn),
            ("crit", &mut self.crit),
        ]
    }

    /// The three-stop ramps a theme file may set, by file key.
    fn ramps(&mut self) -> [(&'static str, &mut Ramp); 9] {
        [
            ("cpu", &mut self.cpu),
            ("gpu", &mut self.gpu),
            ("mem", &mut self.mem),
            ("net", &mut self.net),
            ("net_up", &mut self.net_up),
            ("disk", &mut self.disk),
            ("disk_write", &mut self.disk_write),
            ("fan", &mut self.fan),
            ("temp", &mut self.temp),
        ]
    }

    fn set_token(&mut self, key: &str, value: &str, line: usize) -> Result<(), String> {
        if key == "heat" {
            let stops = parse_list(value, line)?;
            let stops: [Color; 5] = stops
                .try_into()
                .map_err(|_| format!("line {}: heat takes 5 colours", line + 1))?;
            self.heat = Heat::new(stops);
            return Ok(());
        }
        if let Some((_, ink)) = self.inks().into_iter().find(|(k, _)| *k == key) {
            *ink = parse_color(unquote(value), line)?;
            return Ok(());
        }
        if let Some((_, ramp)) = self.ramps().into_iter().find(|(k, _)| *k == key) {
            let stops = parse_list(value, line)?;
            *ramp = match stops.as_slice() {
                [ink] => Ramp::flat(*ink),
                [foot, body, crest] => Ramp::new(*foot, *body, *crest),
                _ => return Err(format!("line {}: {key} takes 1 or 3 colours", line + 1)),
            };
            return Ok(());
        }
        Err(format!("line {}: unknown key {key:?}", line + 1))
    }
}

fn unquote(value: &str) -> &str {
    value.trim().trim_matches('"').trim()
}

fn hex(rgb: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb.0, rgb.1, rgb.2)
}

fn spell(color: Color) -> Option<String> {
    match color {
        Color::Reset => Some("default".into()),
        other => Rgb::of(other).map(hex),
    }
}

fn parse_rgb(text: &str, line: usize) -> Result<Rgb, String> {
    let digits = text.trim().trim_start_matches('#');
    match (digits.len(), u32::from_str_radix(digits, 16)) {
        (6, Ok(value)) => Ok(Rgb::hex(value)),
        _ => Err(format!("line {}: {text:?} is not #rrggbb", line + 1)),
    }
}

fn parse_color(text: &str, line: usize) -> Result<Color, String> {
    if text.eq_ignore_ascii_case("default") {
        return Ok(Color::Reset);
    }
    parse_rgb(text, line).map(Rgb::color)
}

fn parse_list(value: &str, line: usize) -> Result<Vec<Color>, String> {
    let value = value.trim();
    if !value.starts_with('[') {
        return parse_color(unquote(value), line).map(|ink| vec![ink]);
    }
    let inner = value
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .ok_or_else(|| format!("line {}: expected a [list] of colours", line + 1))?;
    inner
        .split(',')
        .map(unquote)
        .filter(|item| !item.is_empty())
        .map(|item| parse_color(item, line))
        .collect()
}

/// One colour in a btop theme: `#rrggbb`, `#gg` greyscale, or `r g b`
/// decimal. Empty means "btop's default", which reads as absent.
fn btop_color(value: &str) -> Option<Rgb> {
    let value = value.trim();
    if let Some(digits) = value.strip_prefix('#') {
        return match (digits.len(), u32::from_str_radix(digits, 16)) {
            (6, Ok(v)) => Some(Rgb::hex(v)),
            (2, Ok(v)) => u8::try_from(v).ok().map(|g| Rgb(g, g, g)),
            _ => None,
        };
    }
    let parts: Vec<u8> = value
        .split_whitespace()
        .filter_map(|p| p.parse().ok())
        .collect();
    match parts.as_slice() {
        [r, g, b] => Some(Rgb(*r, *g, *b)),
        _ => None,
    }
}

/// A btop `.theme` file read into its `theme[key]="value"` pairs.
#[derive(Debug, Default)]
pub struct BtopTheme {
    keys: Vec<(String, Rgb)>,
}

impl BtopTheme {
    /// Parse btop's format; lines that are not `theme[...]=` are skipped.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let keys = text
            .lines()
            .filter_map(|line| {
                let rest = line.trim().strip_prefix("theme[")?;
                let (key, value) = rest.split_once("]=")?;
                let color = btop_color(unquote(value))?;
                Some((key.trim().to_owned(), color))
            })
            .collect();
        Self { keys }
    }

    /// The first `theme[key]="value"` line whose value is not a colour,
    /// as `(line number from 1, key, value)`. An empty value is btop's
    /// "use the default" and is fine.
    #[must_use]
    pub fn bad_colour(text: &str) -> Option<(usize, String, String)> {
        text.lines().enumerate().find_map(|(i, line)| {
            let rest = line.trim().strip_prefix("theme[")?;
            let (key, value) = rest.split_once("]=")?;
            let value = unquote(value);
            (!value.is_empty() && btop_color(value).is_none())
                .then(|| (i + 1, key.trim().to_owned(), value.to_owned()))
        })
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<Rgb> {
        self.keys.iter().find(|(k, _)| k == key).map(|(_, c)| *c)
    }

    /// How many colours the file set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// btop's own background, when the file names one.
    #[must_use]
    pub fn background(&self) -> Option<Rgb> {
        self.get("main_bg")
    }

    /// Map btop's keys onto siltop tokens. btop's gradients run
    /// low → high value, so only their start (a calm hue) becomes a
    /// resource accent; the mid and end become warn and crit, which keeps
    /// "warm = alarm". The result is hardened like any user theme.
    ///
    /// # Errors
    /// A file with no `main_fg` and no resource colours is not a theme.
    pub fn to_theme(&self, name: &str) -> Result<Theme, String> {
        let bg = self.background().unwrap_or(Rgb::hex(0x161616));
        let fg = self
            .get("main_fg")
            .ok_or("not a btop theme: no theme[main_fg]")?;
        let or = |key: &str, fallback: Rgb| self.get(key).unwrap_or(fallback);
        let accent = |keys: &[&str], fallback: Rgb| {
            keys.iter()
                .find_map(|key| self.get(key))
                .unwrap_or(fallback)
        };
        let ramp = |ink: Rgb| {
            Ramp::new(
                mix(ink, bg, 0.45).color(),
                ink.color(),
                mix(ink, fg, 0.4).color(),
            )
        };
        let crit = accent(&["cpu_end", "used_end", "temp_end"], Rgb::hex(0xff5f6d));
        let warn = accent(&["cpu_mid", "used_mid", "temp_mid"], Rgb::hex(0xffb347));
        let cpu = accent(&["cpu_start", "cpu_box"], Rgb::hex(0x5fd497));
        let mem = accent(&["used_start", "mem_box"], Rgb::hex(0x5cbfe8));
        let net = accent(&["download_start", "net_box"], Rgb::hex(0x78a2f7));
        let up = accent(&["upload_start", "net_box"], net);
        let disk = accent(&["available_start", "cached_start"], Rgb::hex(0xbba9ea));
        let gpu = accent(
            &["gpu_box", "proc_misc", "process_start"],
            Rgb::hex(0xae8cf5),
        );
        let temp = accent(&["temp_start"], Rgb::hex(0x7fb2ff));
        // btop colours its resources freely; here each keeps its own hue
        // unless it would read as a state or as another panel.
        let mut accents = [cpu, gpu, mem, net, disk];
        let families = [
            Family::CPU,
            Family::GPU,
            Family::MEM,
            Family::NET,
            Family::DISK,
        ];
        let surface = Surface::Opaque;
        let bgs = surface.backgrounds(bg);
        keep_clear(&mut accents, &families, &[warn, crit], &bgs);
        let [cpu, gpu, mem, net, disk] = accents;
        // Red is the alarm: a red focus mark or selection would make every
        // selected row look kill-armed, so those fall back to the cpu accent.
        let hi = self
            .get("hi_fg")
            .filter(|ink| !wears_crit(*ink))
            .unwrap_or(cpu);
        let sel = self
            .get("selected_bg")
            .filter(|ink| !wears_crit(*ink))
            .unwrap_or_else(|| mix(bg, hi, 0.2));
        let theme = Theme {
            name: intern(&siltop_core::printable(name)),
            fg: fg.color(),
            title: or("title", fg).color(),
            dim: or("graph_text", mix(fg, bg, 0.3)).color(),
            muted: or("inactive_fg", mix(fg, bg, 0.5)).color(),
            faint: or("meter_bg", mix(fg, bg, 0.78)).color(),
            border: or("div_line", mix(fg, bg, 0.72)).color(),
            hi: hi.color(),
            sel_bg: sel.color(),
            sel_idle: mix(bg, sel, 0.5).color(),
            kill_bg: mix(bg, crit, 0.38).color(),
            ok: cpu.color(),
            warn: warn.color(),
            crit: crit.color(),
            cpu: ramp(cpu),
            gpu: ramp(gpu),
            mem: ramp(mem),
            net: ramp(net),
            net_up: ramp(up),
            disk: ramp(disk),
            disk_write: ramp(mix(disk, bg, 0.2)),
            fan: ramp(mix(fg, bg, 0.3)),
            temp: ramp(temp),
            heat: Heat::new([
                temp.color(),
                or("temp_mid", mix(temp, warn, 0.5)).color(),
                warn.color(),
                mix(warn, crit, 0.5).color(),
                or("temp_end", crit).color(),
            ]),
            ..if bg.is_light() {
                palettes::PAPER
            } else {
                palettes::BILLABONG
            }
        };
        Ok(theme.harden(surface, &bgs, surface.floors(false)))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::super::adaptive::{ACCENT_SEPARATION, GRAPH_FLOOR, TEXT_FLOOR, hue_clash};
    use super::super::color::{contrast, delta_e};
    use super::*;

    const BTOP_DEFAULT: &str = include_str!("fixtures/btop-default.theme");
    const BTOP_NORD: &str = include_str!("fixtures/btop-nord.theme");
    const BTOP_PAPER: &str = include_str!("fixtures/btop-paper.theme");

    fn bg_of(name: &str) -> Rgb {
        let source = SOURCES.iter().find(|s| s.name == name).unwrap();
        Rgb::hex(source.bg)
    }

    fn text_inks(theme: &Theme) -> Vec<(&'static str, Color)> {
        let mut inks = vec![
            ("fg", theme.fg),
            ("title", theme.title),
            ("dim", theme.dim),
            ("hi", theme.hi),
            ("ok", theme.ok),
            ("warn", theme.warn),
            ("crit", theme.crit),
        ];
        for (key, ramp) in theme.clone().ramps() {
            if !matches!(key, "net_up" | "disk_write") {
                inks.push((key, ramp.body));
            }
        }
        inks.extend(theme.heat.stops.iter().map(|stop| ("heat", *stop)));
        inks
    }

    fn assert_floors(theme: &Theme, bg: Rgb) {
        for (key, ink) in text_inks(theme) {
            let ratio = contrast(Rgb::of(ink).unwrap(), bg);
            assert!(
                ratio >= TEXT_FLOOR - 0.01,
                "{} {key} is {ratio:.2}:1",
                theme.name
            );
        }
        for ink in [theme.muted, theme.net_up.body, theme.disk_write.body] {
            let ratio = contrast(Rgb::of(ink).unwrap(), bg);
            assert!(ratio >= GRAPH_FLOOR - 0.01, "{} {ratio:.2}:1", theme.name);
        }
    }

    #[test]
    fn theme_names_from_files_are_printable() {
        let theme = Theme::from_toml("name = \"evil\u{1b}]2;x\u{7}\"\n", "fallback").unwrap();
        assert_eq!(theme.name, "evil\u{FFFD}]2;x\u{FFFD}");
        let btop = BtopTheme::parse("theme[main_fg]=\"#ffffff\"\n");
        let theme = btop.to_theme("nord\u{9b}").unwrap();
        assert_eq!(theme.name, "nord\u{FFFD}");
    }

    #[test]
    fn pack_derives_every_source() {
        assert_eq!(
            Theme::pack(),
            [
                "dracula",
                "rose-pine",
                "solarized-dark",
                "solarized-light",
                "everforest",
                "one-dark",
                "catppuccin-latte"
            ]
        );
    }

    #[test]
    fn every_pack_theme_meets_the_contrast_floors_on_its_own_background() {
        for name in Theme::pack() {
            let theme = Theme::by_name(name).unwrap();
            assert_eq!(theme.name, name);
            assert_floors(&theme, bg_of(name));
        }
    }

    #[test]
    fn pack_light_themes_are_light() {
        for name in ["solarized-light", "catppuccin-latte"] {
            let theme = Theme::by_name(name).unwrap();
            // Dark ink on a light page.
            assert!(!Rgb::of(theme.fg).unwrap().is_light(), "{name}");
        }
    }

    #[test]
    fn pack_names_do_not_collide_with_the_free_ten() {
        for name in Theme::pack() {
            assert!(!Theme::builtins().contains(&name), "{name}");
        }
    }

    #[test]
    fn catalog_is_free_names_then_pack() {
        let catalog = Theme::catalog();
        assert_eq!(&catalog[..11], &Theme::names()[..]);
        assert_eq!(&catalog[11..18], &Theme::pack()[..]);
    }

    #[test]
    fn btop_bad_colour_names_the_first_bad_line() {
        let text = "# comment\ntheme[main_fg]=\"#ffffff\"\ntheme[main_bg]=\"\"\ntheme[hi_fg]=\"#zzzzzz\"\n";
        assert_eq!(
            BtopTheme::bad_colour(text),
            Some((4, String::from("hi_fg"), String::from("#zzzzzz")))
        );
        assert_eq!(BtopTheme::bad_colour("theme[main_fg]=\"10 20 30\""), None);
    }

    #[test]
    fn btop_parses_hex_grey_and_decimal_colours() {
        let theme = BtopTheme::parse(
            "theme[main_bg]=\"#00\"\ntheme[main_fg]=\"204 204 204\"\ntheme[title]=\"#ee00ff\"\n\
             # comment\ntheme[hi_fg]=\"\"\nnot a key\n",
        );
        assert_eq!(theme.len(), 3);
        assert_eq!(theme.get("main_bg"), Some(Rgb(0, 0, 0)));
        assert_eq!(theme.get("main_fg"), Some(Rgb(204, 204, 204)));
        assert_eq!(theme.get("title"), Some(Rgb::hex(0xee00ff)));
        assert_eq!(theme.get("hi_fg"), None, "empty means btop's default");
    }

    #[test]
    fn btop_fixtures_import_and_meet_the_floors() {
        for (name, text) in [
            ("btop-default", BTOP_DEFAULT),
            ("nord-btop", BTOP_NORD),
            ("paper-btop", BTOP_PAPER),
        ] {
            let btop = BtopTheme::parse(text);
            assert!(btop.len() >= 20, "{name}: {}", btop.len());
            let theme = btop.to_theme(name).unwrap();
            assert_eq!(theme.name, name);
            assert_floors(&theme, btop.background().unwrap());
        }
    }

    #[test]
    fn btop_gradient_ends_become_states_not_accents() {
        let btop = BtopTheme::parse(BTOP_DEFAULT);
        let raw = btop.to_theme("x").unwrap();
        // btop-default's cpu gradient: green start, red end.
        let cpu = Oklch::from(Rgb::of(raw.cpu.body).unwrap());
        let crit = Oklch::from(Rgb::of(raw.crit).unwrap());
        assert!((100.0..200.0).contains(&cpu.h), "cpu hue {}", cpu.h);
        assert!(crit.h < 45.0 || crit.h > 330.0, "crit hue {}", crit.h);
    }

    /// Regression: btop Default imported with a crit-red mem, a red focus
    /// ring and selection, and cpu and gpu both green.
    #[test]
    fn btop_default_keeps_alarm_red_for_alarms() {
        let theme = BtopTheme::parse(BTOP_DEFAULT).to_theme("x").unwrap();
        let rgb = |color| Rgb::of(color).unwrap();
        let (warn, crit) = (rgb(theme.warn), rgb(theme.crit));
        for (role, ink) in [("hi", theme.hi), ("sel_bg", theme.sel_bg)] {
            assert!(!wears_crit(rgb(ink)), "{role} {ink:?} reads as crit");
        }
        let bodies = [theme.cpu, theme.gpu, theme.mem, theme.net].map(|r| rgb(r.body));
        for (i, body) in bodies.iter().enumerate() {
            for state in [warn, crit] {
                assert!(!hue_clash(*body, state), "accent {i} {body:?} ~ {state:?}");
            }
            for other in &bodies[..i] {
                let gap = delta_e(*body, *other);
                assert!(gap >= ACCENT_SEPARATION, "accent {i} ΔE {gap:.3}");
            }
        }
    }

    #[test]
    fn btop_without_main_fg_is_refused() {
        let err = BtopTheme::parse("theme[cpu_start]=\"#50f095\"")
            .to_theme("x")
            .unwrap_err();
        assert!(err.contains("main_fg"), "{err}");
        assert!(BtopTheme::parse("hello").is_empty());
    }

    #[test]
    fn btop_import_round_trips_through_the_theme_file() {
        let btop = BtopTheme::parse(BTOP_NORD);
        let theme = btop.to_theme("nord-btop").unwrap();
        let file = theme.to_toml(btop.background());
        let back = Theme::from_toml(&file, "ignored").unwrap();
        assert_eq!(back, theme);
    }

    #[test]
    fn user_file_fills_missing_tokens_and_hardens() {
        let theme = Theme::from_toml(
            "name = \"My Theme\"\nbackground = \"#101010\"\ncpu = \"#202020\"\n",
            "file",
        )
        .unwrap();
        assert_eq!(theme.name, "my-theme");
        assert_eq!(theme.gpu, palettes::BILLABONG.gpu);
        // A near-black cpu ink on a near-black page is lifted to the floor.
        let body = Rgb::of(theme.cpu.body).unwrap();
        assert!(contrast(body, Rgb::hex(0x101010)) >= TEXT_FLOOR - 0.01);
    }

    #[test]
    fn user_file_on_light_background_starts_from_paper() {
        let theme = Theme::from_toml("background = \"#fafafa\"\n", "light-one").unwrap();
        assert_eq!(theme.name, "light-one");
        assert_eq!(theme.title, palettes::PAPER.title);
    }

    #[test]
    fn user_file_errors_name_the_line() {
        let bad = Theme::from_toml("fg = \"#12\"\n", "x").unwrap_err();
        assert!(bad.starts_with("line 1"), "{bad}");
        let unknown = Theme::from_toml("\n\nsparkle = \"#123456\"\n", "x").unwrap_err();
        assert!(unknown.contains("line 3") && unknown.contains("sparkle"));
        let ramp = Theme::from_toml("cpu = [\"#111111\", \"#222222\"]\n", "x").unwrap_err();
        assert!(ramp.contains("1 or 3"), "{ramp}");
        assert!(Theme::from_toml("no equals sign", "x").is_err());
    }

    #[test]
    fn intern_reuses_one_name() {
        let a = intern("same-name-twice");
        let b = intern("same-name-twice");
        assert!(std::ptr::eq(a, b));
    }

    use super::super::color::Oklch;
}
