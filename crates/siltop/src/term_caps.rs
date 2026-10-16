//! Terminal capabilities, probed once at startup: colour depth, the terminal's
//! own palette (OSC 10/11/4 in one batched query, then `COLORFGBG`), whether the
//! window is translucent (the terminal's config file) and `NO_COLOR`. The probe
//! runs before raw input starts; a reply that straggles past the budget is
//! swallowed by the input thread (`event::OscSwallow`).

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use siltop_core::{Config, Glass};
use siltop_ui::{ColorDepth, Icons, Opacity, Rgb, Surface, TermPalette, Theme};

/// How long an unanswered colour query may delay startup.
const QUERY_BUDGET: Duration = Duration::from_millis(120);

/// Assumed opacity when glass is forced on but the terminal's config says nothing.
const DEFAULT_GLASS_ALPHA: f32 = 0.75;

/// Apple shipped 24-bit colour in Terminal.app with macOS 26; macOS 15's build is 455.
const APPLE_TERMINAL_TRUECOLOR_BUILD: u32 = 460;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermCaps {
    pub depth: ColorDepth,
    /// `Some(true)` on a light background, `None` when nobody would say.
    pub light: Option<bool>,
    pub no_color: bool,
    /// The terminal's default colours and ANSI palette, as far as it answered.
    pub palette: TermPalette,
    pub surface: Surface,
}

static CAPS: OnceLock<TermCaps> = OnceLock::new();

/// Probe the terminal. Call before raw mode and before anything reads input.
pub fn probe() {
    CAPS.get_or_init(|| {
        let env = |key: &str| std::env::var(key).ok();
        let requested = env("SILTOP_THEME");
        if let Some(name) = requested.as_deref()
            && !is_auto(name)
            && Theme::by_name(name).is_none()
        {
            // ignore a failed write: stderr may already be closed
            use std::io::Write as _;
            let _ = writeln!(
                std::io::stderr(),
                "siltop: unknown theme {name:?}; using auto (themes: {})",
                Theme::catalog().join(", ")
            );
        }
        let wants_auto = requested
            .as_deref()
            .is_none_or(|name| is_auto(name) || Theme::by_name(name).is_none());
        detect(&env, wants_auto && may_query(&env))
    });
}

/// The probed capabilities; environment-only if `probe` never ran.
pub fn caps() -> TermCaps {
    *CAPS.get_or_init(|| detect(&|key| std::env::var(key).ok(), false))
}

/// A variable that says something; blank counts as unset.
fn env_set(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// The theme to start with: `SILTOP_THEME` / `SILTOP_ICONS` win over the
/// config file, then a named theme beats `NO_COLOR`, then auto.
pub fn startup_theme(config: &Config) -> Theme {
    let theme = theme_name_in(&|key| std::env::var(key).ok(), config);
    let icons = env_set("SILTOP_ICONS");
    resolve_look(
        caps_for(config.glass),
        Some(&theme),
        Some(pick(icons.as_deref(), &config.icons)),
        config.extra_contrast,
    )
}

fn theme_name_in(env: &dyn Fn(&str) -> Option<String>, config: &Config) -> String {
    env("SILTOP_THEME")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| config.theme.clone())
}

/// The theme a settings change asks for, on this terminal's depth.
pub fn theme_for(config: &Config) -> Theme {
    resolve_look(
        caps_for(config.glass),
        Some(&config.theme),
        Some(&config.icons),
        config.extra_contrast,
    )
}

/// The probed capabilities with the surface the `glass` setting asks for;
/// `SILTOP_GLASS` still wins over the setting.
fn caps_for(glass: Glass) -> TermCaps {
    let caps = caps();
    if glass == Glass::Auto {
        return caps;
    }
    let surface = glass_surface(
        &|key| std::env::var(key).ok(),
        &|path| std::fs::read_to_string(path).ok(),
        glass,
    );
    TermCaps { surface, ..caps }
}

/// [`resolve`], then the "extra contrast" floors on top when asked.
fn resolve_look(caps: TermCaps, theme: Option<&str>, icons: Option<&str>, extra: bool) -> Theme {
    let theme = resolve(caps, theme, icons);
    if extra {
        theme.on_surface_with(caps.surface, caps.palette.bg, true)
    } else {
        theme
    }
}

/// The environment wins over the config file when it names anything.
fn pick<'a>(env: Option<&'a str>, config: &'a str) -> &'a str {
    env.unwrap_or(config)
}

fn is_auto(name: &str) -> bool {
    name.trim().is_empty() || name.trim().eq_ignore_ascii_case("auto")
}

/// A named theme adapted to this terminal and the look settings: its colour
/// depth, surface and contrast floors.
pub fn fit(theme: Theme, config: &Config) -> Theme {
    let caps = caps_for(config.glass);
    theme.with_depth(caps.depth).on_surface_with(
        caps.surface,
        caps.palette.bg,
        config.extra_contrast,
    )
}

fn resolve(caps: TermCaps, theme: Option<&str>, icons: Option<&str>) -> Theme {
    let named = theme.filter(|name| !is_auto(name)).and_then(Theme::by_name);
    let adaptive = || {
        matches!(caps.depth, ColorDepth::TrueColor | ColorDepth::Ansi256)
            .then(|| Theme::from_terminal(&caps.palette, caps.surface))
            .flatten()
    };
    let theme = match named {
        Some(theme) => theme.with_depth(caps.depth),
        None if caps.no_color => Theme::auto(caps.light, ColorDepth::NoColor),
        None => adaptive().map_or_else(
            || Theme::auto(caps.light, caps.depth),
            |theme| theme.with_depth(caps.depth),
        ),
    }
    .on_surface(caps.surface, caps.palette.bg);
    let icons = icons.and_then(Icons::parse).unwrap_or_default();
    theme.with_icons(icons)
}

fn detect(env: &dyn Fn(&str) -> Option<String>, query: bool) -> TermCaps {
    let depth = color_depth(env);
    let no_color = env("NO_COLOR").is_some_and(|value| !value.is_empty());
    let chooses_by_background = matches!(depth, ColorDepth::TrueColor | ColorDepth::Ansi256);
    let palette = (query && chooses_by_background)
        .then(query_palette)
        .flatten()
        .unwrap_or_default();
    let light = palette
        .bg
        .map(Rgb::is_light)
        .or_else(|| env("COLORFGBG").as_deref().and_then(colorfgbg_light));
    let surface = glass_surface(env, &|path| std::fs::read_to_string(path).ok(), Glass::Auto);
    TermCaps {
        depth,
        light,
        no_color,
        palette,
        surface,
    }
}

/// `SILTOP_GLASS` = `off` | `on` | an opacity like `0.8` | `auto`, else
/// the `glass` setting (`mode`): auto reads the running terminal's own
/// opacity setting, on assumes 0.75 when the terminal's config says nothing.
fn glass_surface(
    env: &dyn Fn(&str) -> Option<String>,
    read: &dyn Fn(&Path) -> Option<String>,
    mode: Glass,
) -> Surface {
    let detected = || terminal_opacity(env, read);
    let alpha = match env("SILTOP_GLASS")
        .map(|v| v.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("off" | "false" | "no" | "0" | "opaque") => None,
        Some("on" | "true" | "yes" | "1") => Some(detected().unwrap_or(DEFAULT_GLASS_ALPHA)),
        Some(value) if value != "auto" => value.parse::<f32>().ok().or_else(detected),
        _ => match mode {
            Glass::Off => None,
            Glass::On => Some(detected().unwrap_or(DEFAULT_GLASS_ALPHA)),
            Glass::Auto => detected(),
        },
    };
    alpha
        .and_then(Opacity::new)
        .map_or(Surface::Opaque, Surface::Glass)
}

/// Background opacity from the running terminal's config file, if it sets one.
fn terminal_opacity(
    env: &dyn Fn(&str) -> Option<String>,
    read: &dyn Fn(&Path) -> Option<String>,
) -> Option<f32> {
    let home = env("HOME").map(PathBuf::from)?;
    let config = env("XDG_CONFIG_HOME").map_or_else(|| home.join(".config"), PathBuf::from);
    let term = env("TERM").unwrap_or_default();
    let program = env("TERM_PROGRAM").unwrap_or_default();
    let last = |paths: &[PathBuf], parse: fn(&str) -> Option<f32>| {
        // Later files override earlier ones, so the last one that sets it wins.
        paths
            .iter()
            .filter_map(|path| read(path))
            .filter_map(|text| parse(&text))
            .last()
    };
    if program.eq_ignore_ascii_case("ghostty") || term == "xterm-ghostty" {
        let support = home.join("Library/Application Support/com.mitchellh.ghostty");
        return last(
            &[
                config.join("ghostty/config"),
                support.join("config"),
                support.join("config.ghostty"),
            ],
            ghostty_opacity,
        );
    }
    if term == "xterm-kitty" || env("KITTY_WINDOW_ID").is_some() {
        return last(&[config.join("kitty/kitty.conf")], kitty_opacity);
    }
    if term == "alacritty" || env("ALACRITTY_WINDOW_ID").is_some() {
        return last(
            &[
                home.join(".alacritty.toml"),
                config.join("alacritty/alacritty.toml"),
            ],
            alacritty_opacity,
        );
    }
    None
}

/// Setting lines with `#` comments and blanks stripped.
fn settings(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .map(|line| line.split('#').next().unwrap_or_default().trim())
        .filter(|line| !line.is_empty())
}

/// Ghostty: `background-opacity = 0.75`; the last setting wins.
fn ghostty_opacity(text: &str) -> Option<f32> {
    settings(text)
        .filter_map(|line| line.split_once('='))
        .filter(|(key, _)| key.trim() == "background-opacity")
        .filter_map(|(_, value)| value.trim().trim_matches('"').parse().ok())
        .last()
}

/// Kitty: `background_opacity 0.8`; the last setting wins.
fn kitty_opacity(text: &str) -> Option<f32> {
    settings(text)
        .filter_map(|line| line.split_once(char::is_whitespace))
        .filter(|(key, _)| *key == "background_opacity")
        .filter_map(|(_, value)| value.trim().parse().ok())
        .last()
}

/// Alacritty TOML: `opacity = 0.9` under `[window]`, or `window.opacity = 0.9`.
fn alacritty_opacity(text: &str) -> Option<f32> {
    let mut section = "";
    let mut found = None;
    for line in settings(text) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let is_opacity = (section == "window" && key == "opacity")
            || (section.is_empty() && key == "window.opacity");
        if is_opacity && let Ok(alpha) = value.trim().parse() {
            found = Some(alpha);
        }
    }
    found
}

/// Query only a local interactive terminal: over ssh a reply may land after the
/// budget and would be read as keystrokes.
fn may_query(env: &dyn Fn(&str) -> Option<String>) -> bool {
    std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && env("TERM").is_none_or(|term| term != "dumb")
        && env("SSH_TTY").is_none()
        && env("SSH_CONNECTION").is_none()
        && env("NO_COLOR").is_none_or(|value| value.is_empty())
}

fn color_depth(env: &dyn Fn(&str) -> Option<String>) -> ColorDepth {
    if env("TERM").as_deref() == Some("dumb") {
        return ColorDepth::NoColor;
    }
    if let Some(forced) = env("SILTOP_COLORS").as_deref().and_then(parse_depth) {
        return forced;
    }
    let colorterm = env("COLORTERM").unwrap_or_default().to_ascii_lowercase();
    if colorterm == "truecolor" || colorterm == "24bit" {
        return ColorDepth::TrueColor;
    }
    match env("TERM_PROGRAM").as_deref() {
        Some(
            "iTerm.app" | "WezTerm" | "ghostty" | "vscode" | "Hyper" | "WarpTerminal" | "Tabby"
            | "rio" | "zed",
        ) => {
            return ColorDepth::TrueColor;
        }
        Some("Apple_Terminal") => {
            let build = env("TERM_PROGRAM_VERSION")
                .as_deref()
                .and_then(leading_number)
                .unwrap_or(0);
            return if build >= APPLE_TERMINAL_TRUECOLOR_BUILD {
                ColorDepth::TrueColor
            } else {
                ColorDepth::Ansi256
            };
        }
        _ => {}
    }
    let term = env("TERM").unwrap_or_default().to_ascii_lowercase();
    if term.contains("truecolor")
        || term.contains("24bit")
        || term.ends_with("-direct")
        || matches!(
            term.as_str(),
            "xterm-kitty" | "xterm-ghostty" | "alacritty" | "wezterm" | "foot" | "contour"
        )
    {
        ColorDepth::TrueColor
    } else if term.contains("256") {
        ColorDepth::Ansi256
    } else if matches!(
        term.as_str(),
        "linux"
            | "xterm"
            | "xterm-color"
            | "xterm-16color"
            | "screen"
            | "tmux"
            | "vt100"
            | "vt220"
            | "ansi"
            | "rxvt"
            | "cygwin"
    ) {
        ColorDepth::Ansi16
    } else {
        ColorDepth::Ansi256
    }
}

fn parse_depth(value: &str) -> Option<ColorDepth> {
    match value.trim().to_ascii_lowercase().as_str() {
        "truecolor" | "24bit" | "rgb" => Some(ColorDepth::TrueColor),
        "256" | "ansi256" => Some(ColorDepth::Ansi256),
        "16" | "ansi16" | "ansi" => Some(ColorDepth::Ansi16),
        "none" | "mono" | "0" => Some(ColorDepth::NoColor),
        _ => None,
    }
}

fn leading_number(text: &str) -> Option<u32> {
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// `COLORFGBG` is `fg;bg` or `fg;default;bg`, as ANSI indices.
fn colorfgbg_light(value: &str) -> Option<bool> {
    match value.rsplit(';').next()?.trim().parse::<u8>().ok()? {
        0..=6 | 8 => Some(false),
        7 | 9..=15 => Some(true),
        _ => None,
    }
}

/// The batched query: foreground, background, the 16 ANSI slots, then primary
/// device attributes, which every terminal answers last.
fn palette_query() -> Vec<u8> {
    let mut query = b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\".to_vec();
    for slot in 0..16 {
        query.extend_from_slice(format!("\x1b]4;{slot};?\x1b\\").as_bytes());
    }
    query.extend_from_slice(b"\x1b[c");
    query
}

/// Every complete `ESC ] … (BEL | ESC \\)` reply in `reply`: OSC 10 / 11 fill
/// the defaults, `4;N` the ANSI slot N. Partial and garbled replies are skipped.
fn parse_replies(reply: &[u8]) -> TermPalette {
    let text = String::from_utf8_lossy(reply);
    let mut palette = TermPalette::default();
    for chunk in text.split("\x1b]").skip(1) {
        let Some(end) = chunk.find(['\u{7}', '\u{1b}']) else {
            continue;
        };
        let mut fields = chunk[..end].splitn(3, ';');
        match (fields.next(), fields.next(), fields.next()) {
            (Some("10"), Some(spec), None) => palette.fg = parse_spec(spec),
            (Some("11"), Some(spec), None) => palette.bg = parse_spec(spec),
            (Some("4"), Some(slot), Some(spec)) => {
                if let Some(entry) = slot
                    .parse::<usize>()
                    .ok()
                    .and_then(|slot| palette.ansi.get_mut(slot))
                {
                    *entry = parse_spec(spec);
                }
            }
            _ => {}
        }
    }
    palette
}

/// `rgb:RRRR/GGGG/BBBB` (1–4 hex digits a channel), `rgba:` or `#RRGGBB`.
fn parse_spec(spec: &str) -> Option<Rgb> {
    if let Some(hex) = spec.strip_prefix('#') {
        return u32::from_str_radix(hex.get(..6)?, 16).ok().map(Rgb::hex);
    }
    let spec = spec
        .strip_prefix("rgba:")
        .or_else(|| spec.strip_prefix("rgb:"))?;
    let mut parts = spec.split('/').map(channel);
    Some(Rgb(parts.next()??, parts.next()??, parts.next()??))
}

/// One `rgb:` component of 1–4 hex digits, scaled to 8 bits.
fn channel(hex: &str) -> Option<u8> {
    let digits = u32::try_from(hex.len())
        .ok()
        .filter(|n| (1..=4).contains(n))?;
    let value = u32::from_str_radix(hex, 16).ok()?;
    let max = (1u32 << (4 * digits)) - 1;
    u8::try_from((value * 255 + max / 2) / max).ok()
}

/// Primary device attributes (`ESC [ ? … c`) come back after the colour replies, so
/// seeing them means the terminal has said everything it will.
fn has_device_attributes(reply: &[u8]) -> bool {
    reply
        .windows(3)
        .position(|w| w == b"\x1b[?")
        .is_some_and(|at| reply[at..].contains(&b'c'))
}

#[cfg(unix)]
fn query_palette() -> Option<TermPalette> {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::time::Instant;

    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok()?;
    let guard = RawTty::enter(tty.as_raw_fd())?;
    tty.write_all(&palette_query()).ok()?;
    tty.flush().ok()?;
    let deadline = Instant::now() + QUERY_BUDGET;
    let mut reply = Vec::with_capacity(1024);
    let mut chunk = [0u8; 256];
    while Instant::now() < deadline && !has_device_attributes(&reply) && reply.len() < 4096 {
        match tty.read(&mut chunk) {
            Ok(n) => reply.extend_from_slice(chunk.get(..n)?),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    drop(guard);
    Some(parse_replies(&reply))
}

#[cfg(not(unix))]
fn query_palette() -> Option<TermPalette> {
    None
}

/// Raw, non-canonical tty whose reads give up after 0.1 s; restored on drop,
/// flushing unread input so a straggling reply never reaches the key handler.
#[cfg(unix)]
struct RawTty {
    fd: std::os::fd::RawFd,
    saved: libc::termios,
}

#[cfg(unix)]
impl RawTty {
    fn enter(fd: std::os::fd::RawFd) -> Option<Self> {
        let mut saved = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: `fd` is an open tty and `saved` is valid for one termios write.
        if unsafe { libc::tcgetattr(fd, saved.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: tcgetattr returned 0, so it initialised `saved`.
        let saved = unsafe { saved.assume_init() };
        let mut raw = saved;
        // SAFETY: `raw` is an initialised termios owned by this frame.
        unsafe { libc::cfmakeraw(&raw mut raw) };
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 1;
        // SAFETY: `fd` is an open tty and `raw` is an initialised termios.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw const raw) } != 0 {
            return None;
        }
        Some(Self { fd, saved })
    }
}

#[cfg(unix)]
impl Drop for RawTty {
    fn drop(&mut self) {
        // SAFETY: restores the attributes read from this same open fd in
        // `enter`, discarding any reply bytes still unread.
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSAFLUSH, &raw const self.saved);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn siltop_theme_wins_over_the_config_unless_blank() {
        let config = Config {
            theme: String::from("nord"),
            ..Config::default()
        };
        let env = env_of(&[("SILTOP_THEME", "dracula")]);
        assert_eq!(theme_name_in(&env, &config), "dracula");
        let blank = env_of(&[("SILTOP_THEME", "  ")]);
        assert_eq!(theme_name_in(&blank, &config), "nord");
    }

    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    fn depth(pairs: &[(&str, &str)]) -> ColorDepth {
        color_depth(&env_of(pairs))
    }

    #[test]
    fn colorterm_and_known_terminals_mean_truecolor() {
        assert_eq!(depth(&[("COLORTERM", "truecolor")]), ColorDepth::TrueColor);
        assert_eq!(depth(&[("COLORTERM", "24bit")]), ColorDepth::TrueColor);
        assert_eq!(depth(&[("TERM_PROGRAM", "ghostty")]), ColorDepth::TrueColor);
        assert_eq!(
            depth(&[("TERM_PROGRAM", "iTerm.app")]),
            ColorDepth::TrueColor
        );
        assert_eq!(depth(&[("TERM", "xterm-kitty")]), ColorDepth::TrueColor);
        assert_eq!(depth(&[("TERM", "xterm-direct")]), ColorDepth::TrueColor);
    }

    #[test]
    fn apple_terminal_gained_truecolor_with_macos_26() {
        let old = [
            ("TERM_PROGRAM", "Apple_Terminal"),
            ("TERM_PROGRAM_VERSION", "455"),
        ];
        let new = [
            ("TERM_PROGRAM", "Apple_Terminal"),
            ("TERM_PROGRAM_VERSION", "488"),
        ];
        assert_eq!(depth(&old), ColorDepth::Ansi256);
        assert_eq!(depth(&new), ColorDepth::TrueColor);
    }

    #[test]
    fn plain_terms_fall_back_to_256_or_16() {
        assert_eq!(depth(&[("TERM", "xterm-256color")]), ColorDepth::Ansi256);
        assert_eq!(
            depth(&[("TERM", "tmux-256color"), ("TERM_PROGRAM", "tmux")]),
            ColorDepth::Ansi256
        );
        assert_eq!(depth(&[("TERM", "xterm")]), ColorDepth::Ansi16);
        assert_eq!(depth(&[("TERM", "dumb")]), ColorDepth::NoColor);
        assert_eq!(
            depth(&[("TERM", "dumb"), ("COLORTERM", "truecolor")]),
            ColorDepth::NoColor
        );
        assert_eq!(depth(&[("TERM", "linux")]), ColorDepth::Ansi16);
        assert_eq!(depth(&[]), ColorDepth::Ansi256);
    }

    #[test]
    fn siltop_colors_overrides_detection() {
        let env = [("COLORTERM", "truecolor"), ("SILTOP_COLORS", "256")];
        assert_eq!(depth(&env), ColorDepth::Ansi256);
        assert_eq!(depth(&[("SILTOP_COLORS", "none")]), ColorDepth::NoColor);
    }

    fn bg_of(reply: &[u8]) -> Option<Rgb> {
        parse_replies(reply).bg
    }

    #[test]
    fn osc11_replies_parse_in_every_shape() {
        let st = b"\x1b]11;rgb:f5f5/f5f5/f5f5\x1b\\\x1b[?62;22c";
        assert_eq!(bg_of(st), Some(Rgb(0xf5, 0xf5, 0xf5)));
        let bel = b"\x1b]11;rgb:16/16/16\x07";
        assert_eq!(bg_of(bel), Some(Rgb(0x16, 0x16, 0x16)));
        let short = b"\x1b]11;rgb:f/0/8\x07";
        assert_eq!(bg_of(short), Some(Rgb(0xff, 0x00, 0x88)));
        let rgba = b"\x1b]11;rgba:1e1e/1e1e/2e2e/ffff\x1b\\";
        assert_eq!(bg_of(rgba), Some(Rgb(0x1e, 0x1e, 0x2e)));
        assert_eq!(bg_of(b"\x1b]11;#141313\x07"), Some(Rgb(0x14, 0x13, 0x13)));
        assert_eq!(bg_of(b"\x1b[?62;22c"), None);
        assert_eq!(bg_of(b"\x1b]11;rgb:zz/00/00\x07"), None);
    }

    /// What Ghostty sends back for the whole batched query, Kanagawa Dragon.
    fn batched_reply() -> Vec<u8> {
        let hexes = [
            "0d0c0c", "c4746e", "8a9a7b", "c4b28a", "8ba4b0", "a292a3", "8ea4a2", "c8c093",
            "a6a69c", "e46876", "87a987", "e6c384", "7fb4ca", "938aa9", "7aa89f", "c5c9c5",
        ];
        let mut reply = b"\x1b]10;rgb:c5c5/c9c9/c5c5\x1b\\\x1b]11;rgb:1414/1313/1313\x07".to_vec();
        for (slot, hex) in hexes.iter().enumerate() {
            let (r, g, b) = (&hex[0..2], &hex[2..4], &hex[4..6]);
            reply.extend_from_slice(
                format!("\x1b]4;{slot};rgb:{r}{r}/{g}{g}/{b}{b}\x1b\\").as_bytes(),
            );
        }
        reply.extend_from_slice(b"\x1b[?62;22c");
        reply
    }

    #[test]
    fn batched_reply_fills_the_whole_palette() {
        let palette = parse_replies(&batched_reply());
        assert_eq!(palette.fg, Some(Rgb(0xc5, 0xc9, 0xc5)));
        assert_eq!(palette.bg, Some(Rgb(0x14, 0x13, 0x13)));
        assert!(palette.ansi.iter().all(Option::is_some));
        assert_eq!(palette.ansi[2], Some(Rgb::hex(0x8a_9a7b)));
        assert_eq!(palette.ansi[15], Some(Rgb::hex(0xc5_c9c5)));
        assert!(palette.is_usable());
    }

    #[test]
    fn partial_reply_keeps_what_completed() {
        let full = batched_reply();
        // Cut in the middle of the fifth ANSI reply.
        let cut = full.windows(5).position(|w| w == b"]4;4;").unwrap() + 9;
        let palette = parse_replies(&full[..cut]);
        assert!(palette.fg.is_some() && palette.bg.is_some());
        assert!(palette.ansi[3].is_some());
        assert!(palette.ansi[4].is_none(), "unterminated reply is dropped");
        assert!(!palette.is_usable());
    }

    #[test]
    fn garbage_replies_parse_to_nothing() {
        for junk in [
            &b""[..],
            b"hello",
            b"\x1b]",
            b"\x1b]4;99;rgb:ff/ff/ff\x07",
            b"\x1b]4;x;rgb:ff/ff/ff\x07",
            b"\x1b]12;rgb:ff/ff/ff\x07",
            b"\x1b]10;rgb:ff/ff\x07",
            b"\x1b]10;rgb:fffff/0/0\x07",
        ] {
            assert_eq!(parse_replies(junk), TermPalette::default(), "{junk:?}");
        }
    }

    #[test]
    fn query_asks_for_every_slot_and_ends_with_device_attributes() {
        let query = String::from_utf8(palette_query()).unwrap();
        assert!(query.starts_with("\x1b]10;?"));
        assert!(query.contains("\x1b]11;?"));
        assert_eq!(query.matches("\x1b]4;").count(), 16);
        assert!(query.ends_with("\x1b[c"));
    }

    #[test]
    fn ghostty_opacity_parses_last_setting() {
        assert_eq!(ghostty_opacity("background-opacity = 0.75"), Some(0.75));
        let text = "# background-opacity = 0.1\ntheme = Kanagawa Dragon\nbackground-opacity=0.9\nbackground-opacity = 0.8 # glass\n";
        assert_eq!(ghostty_opacity(text), Some(0.8));
        assert_eq!(ghostty_opacity("background-blur = 30"), None);
        assert_eq!(ghostty_opacity("background-opacity = lots"), None);
    }

    #[test]
    fn kitty_opacity_parses_space_separated() {
        assert_eq!(kitty_opacity("background_opacity 0.85"), Some(0.85));
        assert_eq!(
            kitty_opacity("background_opacity\t0.6\n#background_opacity 0.1"),
            Some(0.6)
        );
        assert_eq!(kitty_opacity("dynamic_background_opacity yes"), None);
    }

    #[test]
    fn alacritty_opacity_reads_the_window_table() {
        assert_eq!(alacritty_opacity("[window]\nopacity = 0.9\n"), Some(0.9));
        assert_eq!(alacritty_opacity("window.opacity = 0.7"), Some(0.7));
        assert_eq!(alacritty_opacity("[colors]\nopacity = 0.5\n"), None);
        assert_eq!(
            alacritty_opacity("[window]\ndecorations = \"none\"\n[font]\nsize = 12\n"),
            None
        );
    }

    #[test]
    fn glass_follows_env_then_terminal_config() {
        let files = |path: &Path| {
            path.ends_with("ghostty/config")
                .then(|| String::from("background-opacity = 0.75"))
        };
        let surface = |pairs: &[(&str, &str)]| glass_surface(&env_of(pairs), &files, Glass::Auto);
        let alpha = |s: Surface| match s {
            Surface::Glass(o) => Some(o.alpha()),
            Surface::Opaque => None,
        };
        let ghostty = [("HOME", "/h"), ("TERM_PROGRAM", "ghostty")];
        assert_eq!(alpha(surface(&ghostty)), Some(0.75));
        assert_eq!(alpha(surface(&[("HOME", "/h")])), None, "unknown terminal");
        assert_eq!(
            alpha(surface(&[
                ("HOME", "/h"),
                ("TERM_PROGRAM", "ghostty"),
                ("SILTOP_GLASS", "off")
            ])),
            None
        );
        assert_eq!(
            alpha(surface(&[("HOME", "/h"), ("SILTOP_GLASS", "on")])),
            Some(0.75)
        );
        assert_eq!(alpha(surface(&[("SILTOP_GLASS", "0.6")])), Some(0.6));
        assert_eq!(
            alpha(surface(&[("SILTOP_GLASS", "1.0")])),
            None,
            "opaque is not glass"
        );
    }

    #[test]
    fn glass_setting_overrides_detection_but_not_the_env() {
        let files = |path: &Path| {
            path.ends_with("ghostty/config")
                .then(|| String::from("background-opacity = 0.8"))
        };
        let surface = |pairs: &[(&str, &str)], mode: Glass| {
            glass_surface(&env_of(pairs), &files, mode).is_glass()
        };
        let ghostty = [("HOME", "/h"), ("TERM_PROGRAM", "ghostty")];
        let plain = [("HOME", "/h")];
        assert!(surface(&ghostty, Glass::Auto), "auto reads the terminal");
        assert!(
            !surface(&ghostty, Glass::Off),
            "off wins over a translucent config"
        );
        assert!(
            surface(&plain, Glass::On),
            "on assumes glass with no config"
        );
        assert!(!surface(&plain, Glass::Auto));
        let forced = [("HOME", "/h"), ("SILTOP_GLASS", "on")];
        assert!(surface(&forced, Glass::Off), "the env still wins");
        let env_auto = [("HOME", "/h"), ("SILTOP_GLASS", "auto")];
        assert!(
            surface(&env_auto, Glass::On),
            "env auto defers to the setting"
        );
        let on = glass_surface(&env_of(&plain), &files, Glass::On);
        assert_eq!(
            on,
            Surface::Glass(Opacity::new(DEFAULT_GLASS_ALPHA).unwrap())
        );
    }

    #[test]
    fn extra_contrast_resolves_brighter_frames() {
        let dark = caps(ColorDepth::TrueColor, Some(false), false);
        let plain = resolve_look(dark, Some("nord"), None, false);
        let raised = resolve_look(dark, Some("nord"), None, true);
        assert_eq!(plain.name, raised.name);
        assert_ne!(plain.border, raised.border);
        assert_eq!(plain.cpu.body, raised.cpu.body, "accents already pass");
    }

    #[test]
    fn device_attributes_end_the_wait() {
        assert!(has_device_attributes(b"\x1b]11;rgb:0/0/0\x07\x1b[?1;2c"));
        assert!(!has_device_attributes(b"\x1b]11;rgb:0/0/0\x07\x1b[?1;2"));
        assert!(!has_device_attributes(b""));
    }

    #[test]
    fn lightness_splits_at_mid_grey() {
        assert!(Rgb(0xff, 0xff, 0xff).is_light());
        assert!(Rgb(0xf5, 0xf5, 0xf5).is_light());
        assert!(!Rgb(0x16, 0x16, 0x16).is_light());
        assert!(!Rgb(0x1e, 0x1e, 0x2e).is_light());
        assert!(!Rgb(0x70, 0x70, 0x70).is_light());
        assert!(Rgb(0x80, 0x80, 0x80).is_light());
    }

    #[test]
    fn colorfgbg_names_the_background() {
        assert_eq!(colorfgbg_light("15;0"), Some(false));
        assert_eq!(colorfgbg_light("0;15"), Some(true));
        assert_eq!(colorfgbg_light("0;default;7"), Some(true));
        assert_eq!(colorfgbg_light("7;default"), None);
    }

    fn caps(depth: ColorDepth, light: Option<bool>, no_color: bool) -> TermCaps {
        TermCaps {
            depth,
            light,
            no_color,
            palette: TermPalette::default(),
            surface: Surface::Opaque,
        }
    }

    #[test]
    fn auto_builds_from_the_terminal_palette_when_it_answered() {
        let mut answered = caps(ColorDepth::TrueColor, Some(false), false);
        answered.palette = parse_replies(&batched_reply());
        assert_eq!(resolve(answered, None, None).name, "auto");
        assert_eq!(resolve(answered, Some("nord"), None).name, "nord");
        answered.depth = ColorDepth::Ansi16;
        assert_eq!(resolve(answered, None, None).name, "terminal");
    }

    #[test]
    fn glass_reaches_named_themes_too() {
        let mut glass = caps(ColorDepth::TrueColor, Some(false), false);
        glass.surface = Surface::Glass(Opacity::new(0.75).unwrap());
        assert!(resolve(glass, Some("nord"), None).surface.is_glass());
        assert!(resolve(glass, None, None).surface.is_glass());
    }

    #[test]
    fn env_theme_overrides_config_theme() {
        let dark = caps(ColorDepth::TrueColor, Some(false), false);
        let chosen = |env: Option<&str>, config: &str| resolve(dark, Some(pick(env, config)), None);
        assert_eq!(chosen(None, "nord").name, "nord", "config applies");
        assert_eq!(chosen(Some("paper"), "nord").name, "paper", "env wins");
        assert_eq!(
            chosen(Some("auto"), "nord").name,
            "billabong",
            "env auto wins"
        );
        assert_eq!(chosen(None, "auto").name, "billabong");
        assert_eq!(
            chosen(None, "no-such").name,
            "billabong",
            "unknown falls back"
        );
        assert_eq!(chosen(None, "Tokyo Night").name, "tokyo-night");
    }

    #[test]
    fn resolve_honours_name_then_no_color_then_background() {
        let dark = caps(ColorDepth::TrueColor, Some(false), false);
        let light = caps(ColorDepth::TrueColor, Some(true), false);
        assert_eq!(resolve(dark, None, None).name, "billabong");
        assert_eq!(resolve(light, None, None).name, "paper");
        assert_eq!(resolve(light, Some("auto"), None).name, "paper");
        assert_eq!(resolve(dark, Some("nord"), None).name, "nord");
        assert_eq!(resolve(dark, Some("no-such-theme"), None).name, "billabong");

        let mono = caps(ColorDepth::TrueColor, None, true);
        let theme = resolve(mono, None, None);
        assert_eq!(
            (theme.name, theme.depth),
            ("monotreme", ColorDepth::NoColor)
        );
        let named = resolve(mono, Some("paper"), None);
        assert_eq!((named.name, named.depth), ("paper", ColorDepth::TrueColor));

        let ansi = caps(ColorDepth::Ansi16, Some(true), false);
        assert_eq!(resolve(ansi, None, None).name, "terminal");
        let quantised = resolve(caps(ColorDepth::Ansi256, None, false), Some("okabe"), None);
        assert_eq!(quantised.depth, ColorDepth::Ansi256);
    }

    #[test]
    fn icons_come_from_the_environment() {
        let dark = caps(ColorDepth::TrueColor, None, false);
        assert_eq!(resolve(dark, None, None).icons, Icons::Unicode);
        assert_eq!(resolve(dark, None, Some("nerd")).icons, Icons::Nerd);
        assert_eq!(resolve(dark, None, Some("bogus")).icons, Icons::Unicode);
    }

    #[test]
    fn detect_reads_colorfgbg_without_a_query() {
        let env = env_of(&[("COLORFGBG", "0;15"), ("COLORTERM", "truecolor")]);
        let found = detect(&env, false);
        assert_eq!(found.light, Some(true));
        assert_eq!(found.depth, ColorDepth::TrueColor);
        assert!(!found.no_color);
        let env = env_of(&[("NO_COLOR", "1")]);
        assert!(detect(&env, false).no_color);
        let env = env_of(&[("NO_COLOR", "")]);
        assert!(!detect(&env, false).no_color);
    }
}
