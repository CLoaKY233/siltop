//! Theme engine: tokens, built-in palettes and colour-depth lowering.
//!
//! "Warm = alarm": each resource owns a cool hue, and amber or coral only ever
//! mean a state. Nothing paints a background except the selection and kill rows,
//! and on a glass (translucent) surface nothing paints one at all.

mod adaptive;
mod color;
mod depth;
mod pack;
mod palettes;
mod tokens;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use siltop_core::{Pressure, Thermal};

pub use adaptive::{Opacity, Surface, TermPalette};
pub use color::Rgb;
pub use depth::ColorDepth;
pub use pack::{BtopTheme, install_user_themes};
pub use tokens::{Heat, Icons, Ramp, Theme};

use crate::layout::Panel;
use tokens::blend;

impl Default for Theme {
    fn default() -> Self {
        palettes::BILLABONG
    }
}

impl Theme {
    /// The name that follows the terminal instead of naming a palette.
    pub const AUTO: &'static str = "auto";

    /// The core theme choices in menu order: [`Theme::AUTO`] first, then
    /// each core built-in, so cycling with `T` comes back round to auto.
    /// Not every name [`Theme::by_name`] accepts: an error or a list of
    /// choices wants [`Theme::catalog`], which adds the pack and the
    /// user's own themes.
    #[must_use]
    pub fn names() -> [&'static str; 11] {
        let mut names = [Self::AUTO; 11];
        for (slot, name) in names.iter_mut().skip(1).zip(Self::builtins()) {
            *slot = name;
        }
        names
    }

    /// Built-in palette names, lowercase, in menu order (no `auto`).
    #[must_use]
    pub fn builtins() -> [&'static str; 10] {
        palettes::ALL.map(|theme| theme.name)
    }

    /// The choice after `current` in [`Theme::names`], wrapping; an unknown
    /// name (or a resolved auto theme's own name) steps to the first built-in.
    #[must_use]
    pub fn next_name(current: &str) -> &'static str {
        Self::next_in(&Self::names(), current)
    }

    /// The choice after `current` in `names`, wrapping; unknown steps to
    /// the second entry (the first after auto).
    #[must_use]
    pub fn next_in(names: &[&'static str], current: &str) -> &'static str {
        if names.is_empty() {
            return Self::AUTO;
        }
        let at = names
            .iter()
            .position(|name| name.eq_ignore_ascii_case(current.trim()))
            .unwrap_or(0);
        names[(at + 1) % names.len()]
    }

    /// Look up a built-in by name; case, spaces and underscores are forgiven.
    #[must_use]
    pub fn by_name(name: &str) -> Option<Self> {
        let wanted: String = name
            .trim()
            .chars()
            .map(|c| match c {
                ' ' | '_' => '-',
                c => c.to_ascii_lowercase(),
            })
            .collect();
        let wanted = match wanted.as_str() {
            "catppuccin" | "mocha" => "catppuccin-mocha",
            "tokyonight" | "tokyo" => "tokyo-night",
            "mono" => "monotreme",
            "ansi" => "terminal",
            "kanagawa-dragon" | "dragon" => "kanagawa",
            "gruvbox" => "gruvbox-dark",
            other => other,
        };
        pack::lookup(wanted)
    }

    /// `theme = auto`: Paper on a light background, Billabong otherwise;
    /// the Terminal theme on 16-colour terminals and Monotreme without colour.
    #[must_use]
    pub fn auto(light_background: Option<bool>, depth: ColorDepth) -> Self {
        let base = match (depth, light_background) {
            (ColorDepth::NoColor, _) => palettes::MONOTREME,
            (ColorDepth::Ansi16, _) => palettes::TERMINAL,
            (_, Some(true)) => palettes::PAPER,
            _ => palettes::BILLABONG,
        };
        base.with_depth(depth)
    }

    #[must_use]
    pub fn with_depth(mut self, depth: ColorDepth) -> Self {
        self.depth = depth;
        self
    }

    #[must_use]
    pub fn with_icons(mut self, icons: Icons) -> Self {
        self.icons = icons;
        self
    }

    /// How far a veiled cell's ink sinks toward [`Theme::faint`].
    pub const VEIL: f32 = 0.6;

    /// The ink a cell under a modal takes: its own colour sunk toward the
    /// faint token (the background's direction on every palette). The
    /// terminal default and named ANSI colours cannot blend, so they take
    /// the faint ink outright. Only ever a foreground: never a fill.
    #[must_use]
    pub fn veiled(&self, ink: Color) -> Color {
        match ink {
            Color::Rgb(..) => blend(ink, self.faint, Self::VEIL),
            // Default ink reads as the palette's dim before it sinks.
            Color::Reset if matches!(self.dim, Color::Rgb(..)) => {
                blend(self.dim, self.faint, Self::VEIL)
            }
            _ => self.faint,
        }
    }

    /// Lower a finished frame to this theme's colour depth. Call once, last.
    pub fn finish(&self, buf: &mut Buffer) {
        depth::finish(self, buf);
    }

    #[must_use]
    pub fn title(&self) -> Style {
        Style::default().fg(self.title).add_modifier(Modifier::BOLD)
    }

    #[must_use]
    pub fn fg(&self) -> Style {
        Style::default().fg(self.fg)
    }

    #[must_use]
    pub fn dim(&self) -> Style {
        Style::default().fg(self.dim)
    }

    #[must_use]
    pub fn muted(&self) -> Style {
        Style::default().fg(self.muted)
    }

    #[must_use]
    pub fn faint(&self) -> Style {
        Style::default().fg(self.faint)
    }

    #[must_use]
    pub fn cpu(&self) -> Style {
        Style::default().fg(self.cpu.body)
    }

    #[must_use]
    pub fn gpu(&self) -> Style {
        Style::default().fg(self.gpu.body)
    }

    #[must_use]
    pub fn mem(&self) -> Style {
        Style::default().fg(self.mem.body)
    }

    #[must_use]
    pub fn net(&self) -> Style {
        Style::default().fg(self.net.body)
    }

    #[must_use]
    pub fn disk(&self) -> Style {
        Style::default().fg(self.disk.body)
    }

    #[must_use]
    pub fn fan(&self) -> Style {
        Style::default().fg(self.fan.body)
    }

    /// Every °C on screen goes through here, so one reading has one colour.
    #[must_use]
    pub fn temp(&self, celsius: f32) -> Style {
        Style::default().fg(self.heat.at(celsius))
    }

    #[must_use]
    pub fn warn_style(&self) -> Style {
        Style::default().fg(self.warn)
    }

    /// Search matches: highlight ink plus underline, never a warm fill.
    #[must_use]
    pub fn search_hit(&self) -> Style {
        Style::default()
            .fg(self.hi)
            .add_modifier(Modifier::UNDERLINED | Modifier::BOLD)
    }

    /// Row highlight. Only the fill changes, so every cell keeps its own ink;
    /// a row armed for kill turns into one solid alarm block.
    #[must_use]
    pub fn selection(&self, focused: bool, kill: bool) -> Style {
        if self.surface.is_glass() {
            return self.glass_selection(focused, kill);
        }
        if kill {
            return Style::default()
                .fg(self.title)
                .bg(self.kill_bg)
                .add_modifier(Modifier::BOLD);
        }
        match (self.reverse_selection, focused) {
            (true, true) => Style::default()
                .fg(Color::Reset)
                .add_modifier(Modifier::REVERSED | Modifier::BOLD),
            (true, false) => Style::default().add_modifier(Modifier::BOLD),
            (false, true) => Style::default()
                .bg(self.sel_bg)
                .add_modifier(Modifier::BOLD),
            (false, false) => Style::default().bg(self.sel_idle),
        }
    }

    /// Paint the selection over one table row already drawn into `area`.
    /// Opaque themes lay the [`Theme::selection`] fill over the row. Glass
    /// has no fill to carry it, so the row goes bold and only its neutral
    /// text takes the highlight ink: numeric columns keep their value tint.
    pub fn paint_selection(&self, buf: &mut Buffer, area: Rect, focused: bool, kill: bool) {
        if !self.surface.is_glass() || kill {
            buf.set_style(area, self.selection(focused, kill));
            return;
        }
        let neutral = [Color::Reset, self.fg, self.title, self.dim, self.muted];
        for pos in area.intersection(buf.area).positions() {
            let cell = &mut buf[pos];
            let mut style = cell.style().add_modifier(Modifier::BOLD);
            if focused && style.fg.is_none_or(|ink| neutral.contains(&ink)) {
                style = style.fg(self.hi);
            }
            cell.set_style(style);
        }
    }

    /// The accent bar in front of the selected row, dimmed when unfocused.
    #[must_use]
    pub fn selection_mark(&self, focused: bool) -> Span<'static> {
        let ink = if focused { self.hi } else { self.muted };
        Span::styled(self.icons.select_mark(), Style::default().fg(ink))
    }

    /// Quiet frame, or the highlight ink for overlays that own the keyboard.
    #[must_use]
    pub fn border(&self, focused: bool) -> Style {
        Style::default().fg(if focused { self.hi } else { self.border })
    }

    /// Focus ring and focused-label colour for a panel.
    #[must_use]
    pub fn panel_accent(&self, panel: Panel) -> Color {
        match panel {
            Panel::Cpu => self.cpu.body,
            Panel::Gpu => self.gpu.body,
            Panel::Mem => self.mem.body,
            Panel::Net => self.net.body,
            Panel::Disk => self.disk.body,
            Panel::Fans => self.temp.body,
            Panel::Power => self.fan.body,
            Panel::Processes => self.hi,
        }
    }

    #[must_use]
    pub fn pressure(&self, pressure: Pressure) -> Style {
        match pressure {
            Pressure::Nominal => self.dim(),
            Pressure::Warn => Style::default().fg(self.warn),
            Pressure::Critical => Style::default().fg(self.crit),
        }
    }

    #[must_use]
    pub fn thermal(&self, thermal: Thermal) -> Style {
        match thermal {
            Thermal::Nominal => self.dim(),
            Thermal::Fair => Style::default().fg(self.warn),
            Thermal::Serious | Thermal::Critical => Style::default().fg(self.crit),
        }
    }

    /// Row ink for a braille mountain: body on the top row, deep foot at the bottom.
    #[must_use]
    pub fn series_row(&self, ramp: &Ramp, row: usize, rows: usize) -> Color {
        if rows <= 1 {
            return ramp.body;
        }
        let t = row as f32 / (rows.saturating_sub(1) as f32);
        ramp.at(0.5 * (1.0 - t.clamp(0.0, 1.0)))
    }

    /// Ink for one row of a braille graph (C08). Fair is a mild state, so
    /// only the graph that `signals` it (cpu) says so, and only on its crest
    /// row; every other graph keeps its own accent. Serious and worse are an
    /// alarm and take over every load graph.
    #[must_use]
    pub fn graph_ink(
        &self,
        ramp: &Ramp,
        row: usize,
        rows: usize,
        thermal: Thermal,
        signals: bool,
    ) -> Color {
        match thermal {
            Thermal::Fair if signals && row == 0 => self.warn,
            Thermal::Nominal | Thermal::Fair => self.series_row(ramp, row, rows),
            Thermal::Serious | Thermal::Critical => self.crit,
        }
    }

    /// The resting frame of a panel that reports thermal state (cpu and the
    /// heat panel): the quiet border tinted toward warn or crit, `None` while
    /// nominal or for any other panel, which keeps its plain frame.
    #[must_use]
    pub fn thermal_border(&self, panel: Panel, thermal: Thermal) -> Option<Style> {
        if !matches!(panel, Panel::Cpu | Panel::Fans) {
            return None;
        }
        let state = match thermal {
            Thermal::Nominal => return None,
            Thermal::Fair => self.warn,
            Thermal::Serious | Thermal::Critical => self.crit,
        };
        Some(Style::default().fg(blend(self.border, state, 0.6)))
    }

    #[must_use]
    pub fn temp_color(&self, celsius: f32) -> Color {
        self.heat.at(celsius)
    }

    #[must_use]
    pub fn stain(&self, accent: impl Into<Color>, intensity: f32, thermal: Thermal) -> Style {
        let hot = match thermal {
            Thermal::Nominal => accent.into(),
            Thermal::Fair => self.warn,
            Thermal::Serious | Thermal::Critical => self.crit,
        };
        Style::default().fg(blend(self.dim, hot, intensity))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::palettes::{REFERENCES, TERMINAL};
    use super::*;

    fn channels(color: Color) -> Option<(u8, u8, u8)> {
        match color {
            Color::Rgb(r, g, b) => Some((r, g, b)),
            _ => None,
        }
    }

    fn hex(value: u32) -> (u8, u8, u8) {
        ((value >> 16) as u8, (value >> 8) as u8, value as u8)
    }

    fn linear(channel: u8) -> f64 {
        let c = f64::from(channel) / 255.0;
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    /// WCAG relative luminance.
    fn luminance((r, g, b): (u8, u8, u8)) -> f64 {
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
    }

    fn contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    /// CIE L*a*b* under D65, for ΔE76 hue-collision checks.
    fn lab(rgb: (u8, u8, u8)) -> (f64, f64, f64) {
        let (red, green, blue) = (linear(rgb.0), linear(rgb.1), linear(rgb.2));
        let x_n = (0.4124 * red + 0.3576 * green + 0.1805 * blue) / 0.950_47;
        let y_n = luminance(rgb);
        let z_n = (0.0193 * red + 0.1192 * green + 0.9505 * blue) / 1.088_83;
        let curve = |t: f64| {
            if t > 0.008_856 {
                t.cbrt()
            } else {
                7.787 * t + 16.0 / 116.0
            }
        };
        (
            116.0 * curve(y_n) - 16.0,
            500.0 * (curve(x_n) - curve(y_n)),
            200.0 * (curve(y_n) - curve(z_n)),
        )
    }

    fn delta_e(a: Color, b: Color) -> f64 {
        let (a, b) = (lab(channels(a).unwrap()), lab(channels(b).unwrap()));
        ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) + (a.2 - b.2).powi(2)).sqrt()
    }

    /// `Reset` resolves to the terminal default the palette was tuned for.
    fn resolve(color: Color, default_fg: u32) -> (u8, u8, u8) {
        channels(color).unwrap_or_else(|| hex(default_fg))
    }

    fn text_tokens(theme: &Theme) -> Vec<(&'static str, Color)> {
        let mut tokens = vec![
            ("fg", theme.fg),
            ("title", theme.title),
            ("dim", theme.dim),
            ("hi", theme.hi),
            ("ok", theme.ok),
            ("warn", theme.warn),
            ("crit", theme.crit),
            ("cpu", theme.cpu.body),
            ("gpu", theme.gpu.body),
            ("mem", theme.mem.body),
            ("net", theme.net.body),
            ("disk", theme.disk.body),
            ("fan", theme.fan.body),
            ("temp", theme.temp.body),
        ];
        tokens.extend(theme.heat.stops.iter().map(|stop| ("heat", *stop)));
        tokens
    }

    fn graph_bodies(theme: &Theme) -> [(&'static str, Color); 9] {
        [
            ("cpu", theme.cpu.body),
            ("gpu", theme.gpu.body),
            ("mem", theme.mem.body),
            ("net", theme.net.body),
            ("net_up", theme.net_up.body),
            ("disk", theme.disk.body),
            ("disk_write", theme.disk_write.body),
            ("fan", theme.fan.body),
            ("temp", theme.temp.body),
        ]
    }

    #[test]
    fn every_theme_is_listed_and_found_by_name() {
        let names = Theme::builtins();
        assert_eq!(names.len(), 10);
        for name in names {
            assert_eq!(Theme::by_name(name).unwrap().name, name);
        }
        assert_eq!(Theme::by_name("Kanagawa Dragon").unwrap().name, "kanagawa");
        assert_eq!(Theme::by_name("gruvbox").unwrap().name, "gruvbox-dark");
        assert!(
            Theme::by_name(Theme::AUTO).is_none(),
            "auto is resolved by the caller"
        );
        assert_eq!(Theme::by_name("Tokyo Night").unwrap().name, "tokyo-night");
        assert_eq!(
            Theme::by_name("catppuccin").unwrap().name,
            "catppuccin-mocha"
        );
        assert!(Theme::by_name("solarized").is_none());
        assert_eq!(Theme::default().name, "billabong");
    }

    #[test]
    fn names_start_with_auto_then_every_builtin() {
        let names = Theme::names();
        assert_eq!(names[0], Theme::AUTO);
        assert_eq!(&names[1..], &Theme::builtins()[..]);
        assert!(names.contains(&"kanagawa") && names.contains(&"gruvbox-dark"));
    }

    #[test]
    fn t_cycle_regression_wraps_back_to_auto() {
        let last = *Theme::builtins().last().unwrap();
        assert_eq!(Theme::next_name(last), Theme::AUTO);
        assert_eq!(Theme::next_name(Theme::AUTO), Theme::builtins()[0]);
        assert_eq!(Theme::next_name("AUTO"), Theme::builtins()[0]);
        assert_eq!(Theme::next_name("no-such"), Theme::builtins()[0]);
        let mut seen = vec![Theme::AUTO];
        let mut at = Theme::AUTO;
        for _ in 1..Theme::names().len() {
            at = Theme::next_name(at);
            assert!(!seen.contains(&at), "{at} twice");
            seen.push(at);
        }
        assert_eq!(Theme::next_name(at), Theme::AUTO);
    }

    fn graph_rows(theme: &Theme, ramp: &Ramp, thermal: Thermal, signals: bool) -> Vec<Color> {
        (0..4)
            .map(|row| theme.graph_ink(ramp, row, 4, thermal, signals))
            .collect()
    }

    #[test]
    fn fair_thermal_leaves_mem_gpu_net_inks_unchanged() {
        for name in Theme::builtins() {
            let theme = Theme::by_name(name).unwrap();
            for ramp in [theme.mem, theme.gpu, theme.net, theme.disk] {
                assert_eq!(
                    graph_rows(&theme, &ramp, Thermal::Fair, false),
                    graph_rows(&theme, &ramp, Thermal::Nominal, false),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn fair_thermal_marks_only_the_cpu_crest() {
        let theme = Theme::default();
        let rows = graph_rows(&theme, &theme.cpu, Thermal::Fair, true);
        let calm = graph_rows(&theme, &theme.cpu, Thermal::Nominal, true);
        assert_eq!(rows[0], theme.warn);
        assert_eq!(rows[1..], calm[1..]);
    }

    #[test]
    fn serious_thermal_is_still_an_alarm_everywhere() {
        let theme = Theme::default();
        for signals in [true, false] {
            for thermal in [Thermal::Serious, Thermal::Critical] {
                assert!(
                    graph_rows(&theme, &theme.mem, thermal, signals)
                        .iter()
                        .all(|ink| *ink == theme.crit)
                );
            }
        }
    }

    #[test]
    fn thermal_border_tints_only_cpu_and_heat() {
        let theme = Theme::default();
        for panel in [Panel::Cpu, Panel::Fans] {
            assert_eq!(theme.thermal_border(panel, Thermal::Nominal), None);
            let fair = theme.thermal_border(panel, Thermal::Fair).unwrap().fg;
            assert_ne!(fair, Some(theme.border));
            assert_ne!(fair, Some(theme.warn), "a tint, not the full alarm ink");
            assert_ne!(
                theme.thermal_border(panel, Thermal::Serious).unwrap().fg,
                fair
            );
        }
        for panel in [
            Panel::Gpu,
            Panel::Mem,
            Panel::Net,
            Panel::Disk,
            Panel::Processes,
        ] {
            assert_eq!(
                theme.thermal_border(panel, Thermal::Critical),
                None,
                "{panel:?}"
            );
        }
    }

    fn glass_theme() -> Theme {
        Theme::default().on_surface(Surface::Glass(Opacity::new(0.75).unwrap()), None)
    }

    /// A row as the process table paints it: name in body ink, then a cpu
    /// value in its accent and a dim column.
    fn table_row(theme: &Theme) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 1));
        buf[(0, 0)].set_symbol("a").set_fg(theme.fg);
        buf[(1, 0)].set_symbol("b");
        buf[(2, 0)].set_symbol("9").set_fg(theme.cpu.body);
        buf[(3, 0)].set_symbol("7").set_fg(theme.heat.stops[3]);
        buf[(4, 0)].set_symbol("m").set_fg(theme.dim);
        buf
    }

    #[test]
    fn glass_selection_regression_keeps_numeric_column_tint() {
        let theme = glass_theme();
        let mut buf = table_row(&theme);
        theme.paint_selection(&mut buf, Rect::new(0, 0, 6, 1), true, false);
        assert_eq!(buf[(2, 0)].fg, theme.cpu.body, "value tint survives");
        assert_eq!(buf[(3, 0)].fg, theme.heat.stops[3]);
        assert_eq!(buf[(0, 0)].fg, theme.hi, "neutral text takes the highlight");
        assert_eq!(buf[(1, 0)].fg, theme.hi, "unstyled text too");
        assert_eq!(buf[(4, 0)].fg, theme.hi);
        for x in 0..6 {
            assert_eq!(buf[(x, 0)].bg, Color::Reset, "glass paints no fill");
            assert!(buf[(x, 0)].modifier.contains(Modifier::BOLD));
        }
    }

    #[test]
    fn glass_selection_unfocused_only_bolds() {
        let theme = glass_theme();
        let mut buf = table_row(&theme);
        theme.paint_selection(&mut buf, Rect::new(0, 0, 6, 1), false, false);
        assert_eq!(buf[(0, 0)].fg, theme.fg);
        assert_eq!(buf[(2, 0)].fg, theme.cpu.body);
        assert!(buf[(0, 0)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn glass_kill_row_is_one_alarm_block() {
        let theme = glass_theme();
        let mut buf = table_row(&theme);
        theme.paint_selection(&mut buf, Rect::new(0, 0, 6, 1), true, true);
        for x in 0..6 {
            assert_eq!(buf[(x, 0)].fg, theme.crit);
            assert_eq!(buf[(x, 0)].bg, Color::Reset);
        }
    }

    #[test]
    fn opaque_selection_paint_is_the_fill() {
        let theme = Theme::default();
        let mut buf = table_row(&theme);
        theme.paint_selection(&mut buf, Rect::new(0, 0, 6, 1), true, false);
        for x in 0..6 {
            assert_eq!(buf[(x, 0)].bg, theme.sel_bg);
        }
        assert_eq!(buf[(2, 0)].fg, theme.cpu.body);
    }

    #[test]
    fn paint_selection_clips_to_the_buffer() {
        let theme = glass_theme();
        let mut buf = table_row(&theme);
        theme.paint_selection(&mut buf, Rect::new(4, 0, 40, 3), true, false);
        assert_eq!(buf[(0, 0)].fg, theme.fg, "outside the area is untouched");
    }

    #[test]
    fn text_tokens_reach_4_5_to_1() {
        for reference in &REFERENCES {
            let theme = reference.theme;
            for bg in reference.backgrounds {
                for (token, color) in text_tokens(theme) {
                    let ratio = contrast(resolve(color, reference.default_fg), hex(*bg));
                    assert!(
                        ratio >= 4.5,
                        "{} {token} on #{bg:06x} is {ratio:.2}:1",
                        theme.name
                    );
                }
            }
        }
    }

    #[test]
    fn graph_bodies_and_muted_reach_3_to_1() {
        for reference in &REFERENCES {
            let theme = reference.theme;
            for bg in reference.backgrounds {
                let mut inks = graph_bodies(theme).to_vec();
                inks.push(("muted", theme.muted));
                for (token, color) in inks {
                    let ratio = contrast(resolve(color, reference.default_fg), hex(*bg));
                    assert!(
                        ratio >= 3.0,
                        "{} {token} on #{bg:06x} is {ratio:.2}:1",
                        theme.name
                    );
                }
            }
        }
    }

    #[test]
    fn selected_rows_stay_readable() {
        for reference in &REFERENCES {
            let theme = reference.theme;
            let fill = channels(theme.sel_bg).unwrap();
            for (token, color) in [("fg", theme.fg), ("title", theme.title), ("dim", theme.dim)] {
                let ratio = contrast(resolve(color, reference.default_fg), fill);
                assert!(
                    ratio >= 4.5,
                    "{} {token} on selection is {ratio:.2}:1",
                    theme.name
                );
            }
            let mut inks = graph_bodies(theme).to_vec();
            inks.push(("hi", theme.hi));
            for (token, color) in inks {
                let ratio = contrast(resolve(color, reference.default_fg), fill);
                assert!(
                    ratio >= 3.0,
                    "{} {token} on selection is {ratio:.2}:1",
                    theme.name
                );
            }
        }
    }

    #[test]
    fn warm_means_alarm() {
        for reference in &REFERENCES {
            let theme = reference.theme;
            for (token, body) in graph_bodies(theme) {
                if token == "temp" {
                    continue;
                }
                for (state, ink) in [("warn", theme.warn), ("crit", theme.crit)] {
                    let de = delta_e(body, ink);
                    assert!(de >= 30.0, "{} {token} ~ {state}: ΔE {de:.1}", theme.name);
                }
            }
            assert!(delta_e(theme.warn, theme.crit) >= 20.0, "{}", theme.name);
        }
    }

    #[test]
    fn side_by_side_series_do_not_collide() {
        for reference in &REFERENCES {
            let theme = reference.theme;
            if theme.name == "monotreme" {
                continue;
            }
            let primary = [theme.cpu, theme.gpu, theme.mem, theme.net];
            for (i, a) in primary.iter().enumerate() {
                for b in primary.iter().skip(i + 1) {
                    let de = delta_e(a.body, b.body);
                    assert!(de >= 20.0, "{}: ΔE {de:.1}", theme.name);
                }
            }
            let all = [
                theme.cpu, theme.gpu, theme.mem, theme.net, theme.disk, theme.fan,
            ];
            for (i, a) in all.iter().enumerate() {
                for b in all.iter().skip(i + 1) {
                    assert!(delta_e(a.body, b.body) >= 10.0, "{}", theme.name);
                }
            }
        }
    }

    fn all_colors(theme: &Theme) -> Vec<Color> {
        let mut colors = vec![
            theme.fg,
            theme.title,
            theme.dim,
            theme.muted,
            theme.faint,
            theme.border,
            theme.hi,
            theme.sel_bg,
            theme.sel_idle,
            theme.kill_bg,
            theme.ok,
            theme.warn,
            theme.crit,
        ];
        for ramp in [
            theme.cpu,
            theme.gpu,
            theme.mem,
            theme.net,
            theme.net_up,
            theme.disk,
            theme.disk_write,
            theme.fan,
            theme.temp,
        ] {
            colors.extend([ramp.foot, ramp.body, ramp.crest]);
        }
        colors.extend(theme.heat.stops);
        colors
    }

    #[test]
    fn terminal_theme_has_no_rgb() {
        for color in all_colors(&TERMINAL) {
            assert!(!matches!(color, Color::Rgb(..)), "{color:?}");
        }
        let sel = TERMINAL.selection(true, false);
        assert!(sel.add_modifier.contains(Modifier::REVERSED));
        assert_eq!(sel.bg, None);
        assert_eq!(
            sel.fg,
            Some(Color::Reset),
            "the whole row reverses as one bar"
        );
    }

    #[test]
    fn auto_follows_the_background() {
        assert_eq!(Theme::auto(Some(true), ColorDepth::TrueColor).name, "paper");
        assert_eq!(
            Theme::auto(Some(false), ColorDepth::TrueColor).name,
            "billabong"
        );
        assert_eq!(Theme::auto(None, ColorDepth::Ansi256).name, "billabong");
        assert_eq!(Theme::auto(Some(true), ColorDepth::Ansi16).name, "terminal");
        assert_eq!(Theme::auto(None, ColorDepth::NoColor).name, "monotreme");
        assert_eq!(
            Theme::auto(None, ColorDepth::Ansi256).depth,
            ColorDepth::Ansi256
        );
    }

    #[test]
    fn billabong_text_follows_the_terminal() {
        let theme = Theme::default();
        assert_eq!(theme.fg, Color::Reset);
        assert_eq!(theme.title, Color::Reset);
        assert!(theme.title().add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn series_row_runs_body_to_foot() {
        let theme = Theme::default();
        assert_eq!(theme.series_row(&theme.cpu, 0, 4), theme.cpu.body);
        assert_eq!(theme.series_row(&theme.cpu, 3, 4), theme.cpu.foot);
        assert_ne!(theme.series_row(&theme.cpu, 1, 4), theme.cpu.body);
        assert_eq!(theme.series_row(&theme.cpu, 0, 1), theme.cpu.body);
    }

    #[test]
    fn ramp_blends_three_stops() {
        let ramp = Theme::default().gpu;
        assert_eq!(ramp.at(0.0), ramp.foot);
        assert_eq!(ramp.at(0.5), ramp.body);
        assert_eq!(ramp.at(1.0), ramp.crest);
        let flat = Ramp::flat(Color::Green);
        assert_eq!(flat.at(0.3), Color::Green);
    }

    #[test]
    fn heat_is_anchored_to_degrees() {
        let theme = Theme::default();
        assert_eq!(theme.temp_color(30.0), theme.heat.stops[0]);
        assert_eq!(theme.temp_color(80.0), theme.heat.stops[2]);
        assert_eq!(theme.temp_color(99.0), theme.crit);
        assert_eq!(theme.temp_color(f32::NAN), theme.heat.stops[0]);
        assert_ne!(theme.temp_color(75.0), theme.heat.stops[1]);
        assert_eq!(TERMINAL.temp_color(84.5), Color::LightRed);
    }

    #[test]
    fn pressure_nominal_is_dim() {
        let theme = Theme::default();
        assert_eq!(theme.pressure(Pressure::Nominal), theme.dim());
        assert_eq!(theme.pressure(Pressure::Warn).fg, Some(theme.warn));
    }

    #[test]
    fn stain_ramps_from_dim_to_accent() {
        let theme = Theme::default();
        assert_eq!(
            theme.stain(theme.cpu, 1.0, Thermal::Nominal).fg,
            Some(theme.cpu.body)
        );
        assert_eq!(
            theme.stain(theme.cpu, 0.0, Thermal::Nominal).fg,
            Some(theme.dim)
        );
        assert_eq!(
            theme.stain(theme.cpu, 1.0, Thermal::Fair).fg,
            Some(theme.warn)
        );
    }

    #[test]
    fn selection_is_neutral_and_dims_when_unfocused() {
        let theme = Theme::default();
        let focused = theme.selection(true, false);
        assert_eq!(focused.bg, Some(theme.sel_bg));
        assert_eq!(focused.fg, None, "cells keep their own ink");
        assert_eq!(theme.selection(false, false).bg, Some(theme.sel_idle));
        assert_eq!(theme.selection(true, true).bg, Some(theme.kill_bg));
        assert_eq!(theme.selection_mark(true).style.fg, Some(theme.hi));
        assert_eq!(theme.selection_mark(false).style.fg, Some(theme.muted));
        assert_ne!(theme.sel_bg, theme.kill_bg);
    }

    mod frames {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::buffer::Buffer;
        use ratatui::style::{Color, Modifier};
        use siltop_core::SeriesId;
        use siltop_core::{
            ClusterKind, CoreSample, DiskSnapshot, DiskVolume, FanMetric, FanSnapshot, GpuSnapshot,
            Pressure, Surface, TempReading, Thermal,
        };

        use super::super::palettes::REFERENCES;
        use super::super::{ColorDepth, Theme};
        use super::{contrast, hex, resolve};
        use crate::layout::Panel;
        use crate::widgets::tests_support::{Fixture, fixture, process};
        use crate::widgets::{Focus, render_app};

        fn busy(theme: &Theme) -> Fixture {
            let mut fx = fixture("");
            fx.theme = theme.clone();
            fx.ready = true;
            let snap = &mut fx.snap;
            snap.cpu.scaled = 0.42;
            snap.cpu.active = 0.61;
            snap.cpu.watts = Some(8.2);
            snap.cpu.temp_c = Some(84.0);
            snap.cpu.cores = vec![
                CoreSample {
                    kind: ClusterKind::Performance,
                    index: 0,
                    scaled: 0.9,
                    active: 0.9,
                    freq_mhz: None,
                },
                CoreSample {
                    kind: ClusterKind::Efficiency,
                    index: 0,
                    scaled: 0.1,
                    active: 0.1,
                    freq_mhz: None,
                },
            ];
            snap.thermal = Thermal::Fair;
            snap.memory.used_bytes = 17 << 30;
            snap.memory.total_bytes = 24 << 30;
            snap.memory.wired_bytes = 3 << 30;
            snap.memory.pressure = Pressure::Warn;
            snap.gpu = Some(GpuSnapshot {
                scaled: 0.3,
                watts: Some(1.5),
                temp_c: Some(71.0),
                ..GpuSnapshot::default()
            });
            snap.network.iface = String::from("en0");
            snap.network.rx_bps = 1_200_000;
            snap.network.tx_bps = 300_000;
            snap.disk = DiskSnapshot {
                volumes: vec![DiskVolume {
                    name: String::from("Macintosh HD"),
                    mount: String::from("/"),
                    used_bytes: 400 << 30,
                    total_bytes: 926 << 30,
                }],
                read_bps: 4_000_000,
                write_bps: 900_000,
                devices: Vec::new(),
            };
            snap.fans = FanSnapshot {
                fans: vec![FanMetric {
                    name: String::from("Fan 1"),
                    rpm: 2400,
                    max_rpm: 6000,
                }],
            };
            snap.sensors.cpu_c = Some(84.0);
            snap.sensors.gpu_c = Some(71.0);
            snap.sensors.p_c = Some(92.0);
            snap.sensors.e_c = Some(48.0);
            snap.sensors.readings = vec![TempReading {
                name: String::from("battery"),
                celsius: 31.0,
            }];
            snap.processes = vec![
                process(904, "Xcode", 48.1),
                process(77, "WindowServer", 12.5),
                process(1, "launchd", 0.0),
            ];
            for i in 0..120 {
                let v = (i % 17) as f32 / 16.0;
                fx.histories.get_mut(SeriesId::Cpu).push(v);
                fx.histories.get_mut(SeriesId::Gpu).push(v * 0.5);
                fx.histories.get_mut(SeriesId::Mem).push(0.7);
                fx.histories.get_mut(SeriesId::NetRx).push(v * 2e6);
                fx.histories.get_mut(SeriesId::NetTx).push(v * 4e5);
                fx.histories.get_mut(SeriesId::Disk).push(v * 5e6);
                fx.histories.get_mut(SeriesId::DiskRead).push(v * 5e6);
                fx.histories.get_mut(SeriesId::DiskWrite).push(v * 1e6);
                fx.histories.get_mut(SeriesId::PLoad).push(v);
                fx.histories.get_mut(SeriesId::ELoad).push(v * 0.3);
                fx.histories
                    .get_mut(SeriesId::CpuTemp)
                    .push(60.0 + v * 30.0);
                fx.histories
                    .get_mut(SeriesId::GpuTemp)
                    .push(50.0 + v * 20.0);
                fx.histories.get_mut(SeriesId::PTemp).push(60.0 + v * 30.0);
                fx.histories.get_mut(SeriesId::ETemp).push(40.0 + v * 10.0);
            }
            fx
        }

        fn scene(
            name: String,
            fx: &Fixture,
            width: u16,
            height: u16,
        ) -> (String, Buffer, Option<ratatui::layout::Rect>) {
            let buf = paint(fx, width, height);
            let modal = crate::widgets::modal_rect(&fx.view(), buf.area);
            (name, buf, modal)
        }

        fn paint(fx: &Fixture, width: u16, height: u16) -> Buffer {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| render_app(frame, &fx.view()))
                .unwrap();
            terminal.backend().buffer().clone()
        }

        fn scenes(theme: &Theme) -> Vec<(String, Buffer, Option<ratatui::layout::Rect>)> {
            let mut out = Vec::new();
            let mut fx = busy(theme);
            out.push(scene(String::from("work"), &fx, 160, 45));
            fx.focus = Focus::Cpu;
            out.push(scene(String::from("work cpu focus"), &fx, 120, 36));
            fx.focus = Focus::Search;
            fx.proc.filter = String::from("dow");
            fx.confirm_kill = true;
            out.push(scene(String::from("search + confirm"), &fx, 120, 36));
            let mut fx = busy(theme);
            fx.surface = Surface::Glance;
            out.push(scene(String::from("glance"), &fx, 100, 30));
            for panel in Panel::ALL {
                let mut fx = busy(theme);
                fx.expanded = Some(panel);
                fx.focus = Focus::from_panel(panel);
                out.push(scene(format!("expanded {panel:?}"), &fx, 160, 45));
            }
            let mut fx = busy(theme);
            fx.help = true;
            out.push(scene(String::from("help"), &fx, 120, 36));
            fx.help = false;
            fx.settings = true;
            out.push(scene(String::from("settings"), &fx, 120, 36));
            fx.settings = false;
            fx.detail_pid = Some(904);
            out.push(scene(String::from("detail"), &fx, 120, 36));
            out
        }

        fn is_text(symbol: &str) -> bool {
            symbol
                .chars()
                .any(|c| c.is_alphanumeric() || c.is_ascii_punctuation() || "°…—↓↑→·".contains(c))
        }

        #[test]
        fn every_text_cell_meets_4_5_to_1_on_reference_backgrounds() {
            for reference in &REFERENCES {
                let theme = reference.theme;
                for (scene, buf, modal) in scenes(theme) {
                    // Cells behind a modal are veiled on purpose; only the
                    // modal itself (or the whole frame without one) must read.
                    let lit = modal.unwrap_or(buf.area);
                    for pos in lit.positions() {
                        let cell = &buf[pos];
                        if !is_text(cell.symbol()) {
                            continue;
                        }
                        for bg in reference.backgrounds {
                            let back = match cell.bg {
                                Color::Reset => hex(*bg),
                                other => resolve(other, *bg),
                            };
                            let ratio = contrast(resolve(cell.fg, reference.default_fg), back);
                            assert!(
                                ratio >= 4.5,
                                "{} {scene}: {:?} in {:?} on #{bg:06x} is {ratio:.2}:1",
                                theme.name,
                                cell.symbol(),
                                cell.fg,
                            );
                        }
                    }
                }
            }
        }

        fn assert_no_rgb(theme: &Theme) {
            for (scene, buf, _) in scenes(theme) {
                for cell in &buf.content {
                    for color in [cell.fg, cell.bg] {
                        assert!(
                            !matches!(color, Color::Rgb(..)),
                            "{} {:?} {scene}: {color:?} at {:?}",
                            theme.name,
                            theme.depth,
                            cell.symbol()
                        );
                    }
                }
            }
        }

        /// Ghostty draws explicit cell backgrounds opaque, so on glass any
        /// background at all would show as a slab over the wallpaper.
        #[test]
        fn glass_frames_paint_no_cell_background() {
            let surface = crate::theme::Surface::Glass(crate::theme::Opacity::new(0.75).unwrap());
            for name in Theme::builtins() {
                let theme = Theme::by_name(name).unwrap().on_surface(surface, None);
                for (scene, buf, _) in scenes(&theme) {
                    for cell in &buf.content {
                        assert_eq!(
                            cell.bg,
                            Color::Reset,
                            "{name} {scene}: {:?} has a background",
                            cell.symbol()
                        );
                    }
                }
            }
        }

        #[test]
        fn lowered_depths_and_the_terminal_theme_emit_no_rgb() {
            for name in Theme::builtins() {
                let theme = Theme::by_name(name).unwrap();
                assert_no_rgb(&theme.clone().with_depth(ColorDepth::Ansi256));
                assert_no_rgb(&theme.with_depth(ColorDepth::Ansi16));
            }
            assert_no_rgb(&Theme::by_name("terminal").unwrap());
        }

        #[test]
        fn no_color_frames_carry_attributes_only() {
            let theme = Theme::auto(None, ColorDepth::NoColor);
            let mut dimmed = 0;
            for (scene, buf, _) in scenes(&theme) {
                for cell in &buf.content {
                    assert_eq!((cell.fg, cell.bg), (Color::Reset, Color::Reset), "{scene}");
                    if cell.modifier.contains(Modifier::DIM) {
                        dimmed += 1;
                    }
                }
            }
            assert!(dimmed > 0, "labels should fall back to DIM");
            let fx = busy(&theme);
            let buf = paint(&fx, 120, 36);
            assert!(
                buf.content
                    .iter()
                    .any(|cell| cell.modifier.contains(Modifier::REVERSED)),
                "the selected row should fall back to REVERSED"
            );
        }

        #[test]
        fn selected_row_is_neutral_with_an_accent_mark() {
            let theme = Theme::default();
            let fx = busy(&theme);
            let buf = paint(&fx, 120, 36);
            let mark = buf
                .content
                .iter()
                .find(|cell| cell.symbol() == "▌")
                .expect("selection mark");
            assert_eq!(mark.fg, theme.hi);
            assert_eq!(mark.bg, theme.sel_bg);
            assert!(
                buf.content
                    .iter()
                    .all(|cell| cell.bg != Color::Rgb(0x5c, 0x2e, 0x2e)),
                "no danger-red selection"
            );
        }
    }

    #[test]
    fn icons_only_label_panels_in_nerd_mode() {
        assert_eq!(Icons::Unicode.panel(Panel::Cpu), None);
        assert!(Icons::Nerd.panel(Panel::Cpu).is_some());
        assert_eq!(Icons::parse("NERD"), Some(Icons::Nerd));
        assert_eq!(Icons::parse("emoji"), None);
        assert_eq!(Icons::None.select_mark(), ">");
    }

    #[test]
    fn ramp_endpoints_are_exact_and_t_clamps() {
        for theme in super::palettes::ALL {
            for ramp in [
                theme.cpu, theme.gpu, theme.mem, theme.net, theme.disk, theme.fan, theme.temp,
            ] {
                assert_eq!(ramp.at(0.0), ramp.foot);
                assert_eq!(ramp.at(0.5), ramp.body);
                assert_eq!(ramp.at(1.0), ramp.crest);
                assert_eq!(ramp.at(-3.0), ramp.foot);
                assert_eq!(ramp.at(7.0), ramp.crest);
                assert_eq!(Color::from(ramp), ramp.body);
            }
        }
    }

    #[test]
    fn ramp_channels_move_monotonically_per_segment() {
        let ramp = Ramp::new(
            Color::Rgb(0, 200, 10),
            Color::Rgb(100, 100, 100),
            Color::Rgb(255, 0, 90),
        );
        let mut prev = channels(ramp.at(0.0)).unwrap();
        for step in 1..=100 {
            let t = step as f32 / 100.0;
            let now = channels(ramp.at(t)).unwrap();
            let (lo, hi) = if t <= 0.5 {
                (channels(ramp.foot).unwrap(), channels(ramp.body).unwrap())
            } else {
                (channels(ramp.body).unwrap(), channels(ramp.crest).unwrap())
            };
            let between = |c: u8, a: u8, b: u8| c >= a.min(b) && c <= a.max(b);
            assert!(
                between(now.0, lo.0, hi.0)
                    && between(now.1, lo.1, hi.1)
                    && between(now.2, lo.2, hi.2),
                "t={t} {now:?}"
            );
            if t <= 0.5 || step > 51 {
                let dir = |a: u8, b: u8, x: u8, y: u8| {
                    (i16::from(b) - i16::from(a)).signum() * (i16::from(y) - i16::from(x)).signum()
                        >= 0
                };
                assert!(dir(lo.0, hi.0, prev.0, now.0), "red reversed at t={t}");
                assert!(dir(lo.1, hi.1, prev.1, now.1), "green reversed at t={t}");
            }
            prev = now;
        }
    }

    #[test]
    fn named_colours_step_at_the_midpoint() {
        let ramp = Ramp::new(Color::Red, Color::Green, Color::Blue);
        assert_eq!(ramp.at(0.1), Color::Red);
        assert_eq!(ramp.at(0.3), Color::Green);
        assert_eq!(ramp.at(0.9), Color::Blue);
        assert_eq!(Ramp::flat(Color::Cyan).at(0.77), Color::Cyan);
    }

    #[test]
    fn heat_hits_each_stop_at_its_degree() {
        for theme in super::palettes::ALL {
            let heat = theme.heat;
            for (at, stop) in Heat::AT.iter().zip(heat.stops) {
                assert_eq!(heat.at(*at), stop, "{} at {at}", theme.name);
            }
            assert_eq!(heat.at(-40.0), heat.stops[0]);
            assert_eq!(heat.at(f32::NAN), heat.stops[0]);
            assert_eq!(heat.at(f32::NEG_INFINITY), heat.stops[0]);
            assert_eq!(heat.at(f32::INFINITY), heat.stops[4]);
            assert_eq!(heat.at(150.0), heat.stops[4]);
        }
    }

    #[test]
    fn heat_blends_stay_between_neighbouring_stops() {
        let theme = Theme::default();
        for tenth in 450..950 {
            let celsius = tenth as f32 / 10.0;
            let i = Heat::AT
                .windows(2)
                .position(|w| celsius >= w[0] && celsius < w[1])
                .unwrap();
            let (Some(a), Some(b), Some(x)) = (
                channels(theme.heat.stops[i]),
                channels(theme.heat.stops[i + 1]),
                channels(theme.heat.at(celsius)),
            ) else {
                continue;
            };
            let between = |v: u8, p: u8, q: u8| v >= p.min(q) && v <= p.max(q);
            assert!(
                between(x.0, a.0, b.0) && between(x.1, a.1, b.1) && between(x.2, a.2, b.2),
                "{celsius}° -> {x:?}"
            );
        }
    }

    #[test]
    fn series_row_never_leaves_the_foot_to_body_half() {
        let theme = Theme::default();
        for rows in 0..=12 {
            for row in 0..rows.max(1) + 2 {
                let c = theme.series_row(&theme.cpu, row, rows);
                let (Some(x), Some(foot), Some(body)) = (
                    channels(c),
                    channels(theme.cpu.foot),
                    channels(theme.cpu.body),
                ) else {
                    continue;
                };
                let between = |v: u8, p: u8, q: u8| v >= p.min(q) && v <= p.max(q);
                assert!(
                    between(x.0, foot.0, body.0)
                        && between(x.1, foot.1, body.1)
                        && between(x.2, foot.2, body.2),
                    "row {row}/{rows}"
                );
            }
        }
    }

    #[test]
    fn stain_endpoints_are_dim_and_hot() {
        for theme in super::palettes::ALL {
            let accent = theme.cpu.body;
            assert_eq!(
                theme.stain(accent, 0.0, Thermal::Nominal).fg,
                Some(theme.dim)
            );
            assert_eq!(theme.stain(accent, 1.0, Thermal::Nominal).fg, Some(accent));
            assert_eq!(theme.stain(accent, 1.0, Thermal::Fair).fg, Some(theme.warn));
            assert_eq!(
                theme.stain(accent, 1.0, Thermal::Critical).fg,
                Some(theme.crit)
            );
            assert_eq!(
                theme.stain(accent, 5.0, Thermal::Nominal).fg,
                Some(accent),
                "clamps"
            );
        }
    }

    #[test]
    fn by_name_forgives_case_space_and_underscore() {
        for name in Theme::builtins() {
            let shouty = name.to_ascii_uppercase().replace('-', "_");
            assert_eq!(
                Theme::by_name(&shouty).map(|t| t.name),
                Some(name),
                "{shouty}"
            );
            let spaced = format!("  {}  ", name.replace('-', " "));
            assert_eq!(
                Theme::by_name(&spaced).map(|t| t.name),
                Some(name),
                "{spaced}"
            );
        }
        for alias in ["mocha", "tokyo", "tokyonight", "mono", "ansi"] {
            assert!(Theme::by_name(alias).is_some(), "{alias}");
        }
        for junk in ["", " ", "billabong!", "🦆", "auto"] {
            assert!(Theme::by_name(junk).is_none(), "{junk:?}");
        }
    }

    #[test]
    fn theme_names_are_unique_lowercase_kebab() {
        let names = Theme::names();
        for (i, n) in names.iter().enumerate() {
            assert!(n.chars().all(|c| c.is_ascii_lowercase() || c == '-'), "{n}");
            assert!(!names[i + 1..].contains(n), "duplicate {n}");
        }
    }

    #[test]
    fn auto_picks_by_depth_before_background() {
        assert_eq!(
            Theme::auto(Some(true), ColorDepth::NoColor).name,
            "monotreme"
        );
        assert_eq!(Theme::auto(Some(true), ColorDepth::Ansi16).name, "terminal");
        assert_eq!(Theme::auto(None, ColorDepth::TrueColor).name, "billabong");
        assert_eq!(
            Theme::auto(Some(false), ColorDepth::Ansi256).name,
            "billabong"
        );
        for depth in [
            ColorDepth::TrueColor,
            ColorDepth::Ansi256,
            ColorDepth::Ansi16,
            ColorDepth::NoColor,
        ] {
            for light in [None, Some(true), Some(false)] {
                assert_eq!(Theme::auto(light, depth).depth, depth);
            }
        }
    }

    #[test]
    fn icons_parse_table() {
        let cases = [
            ("none", Some(Icons::None)),
            ("ASCII", Some(Icons::None)),
            (" unicode ", Some(Icons::Unicode)),
            ("Nerd", Some(Icons::Nerd)),
            ("nerd-font", Some(Icons::Nerd)),
            ("nerdfont", Some(Icons::Nerd)),
            ("emoji", None),
            ("", None),
        ];
        for (name, want) in cases {
            assert_eq!(Icons::parse(name), want, "{name:?}");
        }
        for name in Icons::NAMES {
            assert!(Icons::parse(name).is_some(), "{name}");
        }
        assert_eq!(Icons::None.select_mark(), ">");
    }
}
