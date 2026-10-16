//! `siltop line`: one short status line for tmux, `SketchyBar`, Starship
//! or `SwiftBar`. It reads the agent's state file when that is fresh and
//! otherwise samples this Mac once, quickly.
//!
//! The same [`Summary`] is what the agent writes, so the line and the
//! agent can never disagree about a reading. Absent hardware leaves its
//! placeholder out; it never prints a made-up zero.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ratatui::style::Color;
use serde::{Deserialize, Serialize};
use siltop_core::{Result, Snapshot, Thermal, Verdict, celsius, percent_display, watts_display};

/// The template used when `--format` is not given.
pub const DEFAULT_FORMAT: &str = "{cpu} {gpu} {power} {temp}";
/// An agent state file older than this is ignored and the Mac is sampled.
pub const FRESH_FOR: Duration = Duration::from_secs(10);

/// How the line is painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Style {
    #[default]
    Plain,
    /// 24-bit ANSI colour from the configured theme.
    Ansi,
    /// tmux `#[fg=...]` markup for `status-right`.
    Tmux,
}

impl Style {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "plain" => Some(Self::Plain),
            "ansi" => Some(Self::Ansi),
            "tmux" => Some(Self::Tmux),
            _ => None,
        }
    }
}

/// The latest readings in the few numbers a status line needs. The agent
/// writes it as JSON; `None` is absent or unknown, never zero.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Summary {
    pub unix_ms: u64,
    pub cpu_pct: Option<f32>,
    pub gpu_pct: Option<f32>,
    pub power_w: Option<f32>,
    pub temp_c: Option<f32>,
    pub mem_pct: Option<f32>,
    pub swap_bytes: Option<u64>,
    /// 0 nominal, 1 fair, 2 serious, 3 critical.
    pub thermal: u8,
    pub verdict: String,
    /// Ids of alert rules that fired and have not cleared.
    pub alerts: Vec<String>,
}

fn finite(value: f32) -> Option<f32> {
    value.is_finite().then_some(value)
}

impl Summary {
    #[must_use]
    pub fn from_snapshot(snap: &Snapshot, verdict: &Verdict, unix_ms: u64) -> Self {
        let power = &snap.power;
        let rail = |r: Option<siltop_core::Reading>| r.and_then(siltop_core::Reading::usable);
        let package = match (rail(power.cpu), rail(power.gpu)) {
            (Some(c), Some(g)) => Some(c + g),
            (c, g) => c.or(g),
        };
        let mem = &snap.memory;
        Self {
            unix_ms,
            cpu_pct: snap
                .primed
                .then_some(snap.cpu.scaled * 100.0)
                .and_then(finite),
            gpu_pct: snap
                .gpu
                .filter(|_| snap.primed)
                .and_then(|g| finite(g.scaled * 100.0)),
            power_w: rail(power.system).or(package),
            temp_c: snap.cpu_temp_c().and_then(finite),
            mem_pct: mem.used_ratio().map(|r| r * 100.0),
            swap_bytes: (mem.total_bytes > 0).then_some(mem.swap_used_bytes),
            thermal: snap.thermal.level(),
            verdict: verdict.headline.clone(),
            alerts: Vec::new(),
        }
    }

    /// Whether this summary is young enough to show instead of sampling.
    #[must_use]
    pub fn is_fresh(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.unix_ms) < FRESH_FOR.as_millis() as u64
            && self.unix_ms <= now_ms + 1_000
    }
}

/// `$XDG_STATE_HOME/siltop/agent.json`, else `~/.local/state/...`.
#[must_use]
pub fn state_path() -> Option<PathBuf> {
    crate::store_disk::default_root()
        .and_then(|history| history.parent().map(|dir| dir.join("agent.json")))
}

/// The state file's summary, if it exists, parses and is fresh.
#[must_use]
pub fn read_fresh(path: &Path, now_ms: u64) -> Option<Summary> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<Summary>(&text)
        .ok()
        .filter(|s| s.is_fresh(now_ms))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Normal,
    Warn,
    Crit,
}

/// A 0–100 figure the way the dashboard prints it (half rounds up).
fn pct(p: f32) -> String {
    percent_display(p / 100.0)
}

/// One placeholder's text and how worrying it is; `None` when absent.
/// Values go through the dashboard's formatters, so the two never differ.
fn token(summary: &Summary, name: &str) -> Option<(String, Level)> {
    let pct_level = |pct: f32| match pct {
        p if p >= 90.0 => Level::Crit,
        p if p >= siltop_core::BUSY_TOTAL * 100.0 => Level::Warn,
        _ => Level::Normal,
    };
    match name {
        "cpu" => summary
            .cpu_pct
            .map(|p| (format!("cpu {}", pct(p)), pct_level(p))),
        "gpu" => summary
            .gpu_pct
            .map(|p| (format!("gpu {}", pct(p)), pct_level(p))),
        "mem" => summary
            .mem_pct
            .map(|p| (format!("mem {}", pct(p)), pct_level(p))),
        "power" => summary.power_w.map(|w| (watts_display(w), Level::Normal)),
        "temp" => summary.temp_c.map(|t| {
            let level = match summary.thermal {
                0 => Level::Normal,
                1 => Level::Warn,
                _ => Level::Crit,
            };
            (format!("{}C", celsius(t)), level)
        }),
        "swap" => summary.swap_bytes.filter(|b| *b > 0).map(|b| {
            (
                format!("swap {}", siltop_core::bytes_short(b)),
                Level::Normal,
            )
        }),
        "thermal" => {
            let thermal = Thermal::from_level(summary.thermal);
            let level = if thermal >= Thermal::Serious {
                Level::Crit
            } else {
                Level::Warn
            };
            thermal.word().map(|w| (String::from(w), level))
        }
        "verdict" => (!summary.verdict.trim().is_empty()).then(|| {
            let level = if summary.verdict == Verdict::normal().headline {
                Level::Normal
            } else {
                Level::Warn
            };
            (summary.verdict.clone(), level)
        }),
        "alerts" => (!summary.alerts.is_empty())
            .then(|| (format!("! {}", summary.alerts.join(",")), Level::Crit)),
        _ => None,
    }
}

/// Every placeholder `--format` understands.
pub const PLACEHOLDERS: [&str; 9] = [
    "cpu", "gpu", "mem", "power", "temp", "swap", "thermal", "verdict", "alerts",
];

/// Theme colours for the three levels.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    normal: Color,
    warn: Color,
    crit: Color,
}

impl Palette {
    #[must_use]
    pub fn from_theme(theme: &siltop_ui::Theme) -> Self {
        Self {
            normal: theme.fg,
            warn: theme.warn,
            crit: theme.crit,
        }
    }

    const fn pick(&self, level: Level) -> Color {
        match level {
            Level::Normal => self.normal,
            Level::Warn => self.warn,
            Level::Crit => self.crit,
        }
    }
}

fn ansi(color: Color) -> Option<String> {
    match color {
        Color::Rgb(r, g, b) => Some(format!("\x1b[38;2;{r};{g};{b}m")),
        Color::Indexed(n) => Some(format!("\x1b[38;5;{n}m")),
        _ => None,
    }
}

fn tmux(color: Color) -> Option<String> {
    match color {
        Color::Rgb(r, g, b) => Some(format!("#[fg=#{r:02x}{g:02x}{b:02x}]")),
        Color::Indexed(n) => Some(format!("#[fg=colour{n}]")),
        _ => None,
    }
}

/// Fills `format`. Unknown placeholders stay as typed so a typo shows;
/// absent ones vanish and the spaces around them collapse. `{{` and `}}`
/// print a literal brace, so `{{cpu}}` is the text `{cpu}`.
#[must_use]
pub fn render(format: &str, summary: &Summary, style: Style, palette: &Palette) -> String {
    let mut out = String::new();
    let mut rest = format;
    while let Some(open) = rest.find(['{', '}']) {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        if rest[open..].starts_with("{{") || rest[open..].starts_with("}}") {
            out.push_str(&rest[open..=open]);
            rest = &after[1..];
            continue;
        }
        if rest[open..].starts_with('}') {
            out.push('}');
            rest = after;
            continue;
        }
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let name = &after[..close];
        if PLACEHOLDERS.contains(&name) {
            if let Some((text, level)) = token(summary, name) {
                // The verdict names a process and the state file is read back
                // from disk, so a token is untrusted text. In tmux a `#` in it
                // would start markup of its own (`#[`, `#(`), so it is doubled.
                let text = siltop_core::into_printable(text);
                let text = match style {
                    Style::Tmux => text.replace('#', "##"),
                    Style::Plain | Style::Ansi => text,
                };
                let paint = match style {
                    Style::Plain => None,
                    Style::Ansi => ansi(palette.pick(level)).map(|on| (on, "\x1b[39m")),
                    Style::Tmux => tmux(palette.pick(level)).map(|on| (on, "#[fg=default]")),
                };
                match paint {
                    Some((on, off)) => {
                        out.push_str(&on);
                        out.push_str(&text);
                        out.push_str(off);
                    }
                    None => out.push_str(&text),
                }
            }
        } else {
            out.push('{');
            out.push_str(name);
            out.push('}');
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    collapse_spaces(&out)
}

fn collapse_spaces(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch == ' ' && (out.is_empty() || out.ends_with(' ')) {
            continue;
        }
        out.push(ch);
    }
    out.trim_end().to_owned()
}

/// Prints one line: the agent's fresh summary, else one quick sample.
pub fn run<W, F>(
    out: &mut W,
    format: &str,
    style: Style,
    palette: &Palette,
    state: Option<&Path>,
    now_ms: u64,
    sample: F,
) -> Result<()>
where
    W: Write + ?Sized,
    F: FnOnce() -> Result<Summary>,
{
    let summary = match state.and_then(|p| read_fresh(p, now_ms)) {
        Some(summary) => summary,
        None => sample()?,
    };
    let line = render(format, &summary, style, palette);
    let written = writeln!(out, "{line}").and_then(|()| out.flush());
    match written {
        Err(err) if err.kind() != std::io::ErrorKind::BrokenPipe => {
            Err(siltop_core::Error::system(format!("write output: {err}")))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const PALETTE: Palette = Palette {
        normal: Color::Rgb(200, 200, 200),
        warn: Color::Rgb(250, 200, 0),
        crit: Color::Rgb(250, 0, 0),
    };

    fn busy_pro() -> Summary {
        Summary {
            unix_ms: 1_000_000,
            cpu_pct: Some(42.4),
            gpu_pct: Some(7.0),
            power_w: Some(12.34),
            temp_c: Some(61.2),
            mem_pct: Some(55.0),
            swap_bytes: Some(0),
            thermal: 0,
            verdict: String::from("All normal"),
            alerts: Vec::new(),
        }
    }

    #[test]
    fn doubled_braces_print_one_brace() {
        let line = render("{{cpu}} {cpu} }} {", &busy_pro(), Style::Plain, &PALETTE);
        assert_eq!(line, "{cpu} cpu 42% } {");
    }

    #[test]
    fn default_format_plain() {
        let line = render(DEFAULT_FORMAT, &busy_pro(), Style::Plain, &PALETTE);
        assert_eq!(line, "cpu 42% gpu 7% 12W 61°C");
    }

    #[test]
    fn desktop_without_gpu_or_temp_drops_tokens_not_zero() {
        let summary = Summary {
            gpu_pct: None,
            temp_c: None,
            ..busy_pro()
        };
        let line = render(DEFAULT_FORMAT, &summary, Style::Plain, &PALETTE);
        assert_eq!(line, "cpu 42% 12W");
        assert!(!line.contains("0%"));
    }

    #[test]
    fn unknown_placeholders_and_literals_survive() {
        let line = render("[{cpu}] {nope} {", &busy_pro(), Style::Plain, &PALETTE);
        assert_eq!(line, "[cpu 42%] {nope} {");
    }

    #[test]
    fn ansi_paints_by_level_and_resets() {
        let summary = Summary {
            cpu_pct: Some(95.0),
            ..busy_pro()
        };
        let line = render("{cpu} {mem}", &summary, Style::Ansi, &PALETTE);
        assert_eq!(
            line,
            "\x1b[38;2;250;0;0mcpu 95%\x1b[39m \x1b[38;2;200;200;200mmem 55%\x1b[39m"
        );
    }

    #[test]
    fn tmux_markup_uses_hex_colours() {
        let summary = Summary {
            thermal: 1,
            ..busy_pro()
        };
        let line = render("{temp} {thermal}", &summary, Style::Tmux, &PALETTE);
        assert_eq!(
            line,
            "#[fg=#fac800]61°C#[fg=default] #[fg=#fac800]fair#[fg=default]"
        );
    }

    #[test]
    fn alerts_and_verdict_tokens() {
        let summary = Summary {
            alerts: vec![String::from("hot"), String::from("memory")],
            verdict: String::from("Busy: Safari using 3.1 cores"),
            ..busy_pro()
        };
        assert_eq!(
            render("{alerts} | {verdict}", &summary, Style::Plain, &PALETTE),
            "! hot,memory | Busy: Safari using 3.1 cores"
        );
        assert_eq!(
            render("{alerts}{swap}", &busy_pro(), Style::Plain, &PALETTE),
            ""
        );
    }

    #[test]
    fn a_process_name_cannot_inject_escapes_or_tmux_markup() {
        // Regression: a process named `y\e]2;RVBC\az` or `#[fg=red]#(cmd)`
        // reached the terminal and tmux's status bar as live sequences.
        let summary = Summary {
            verdict: String::from("Busy: y\u{1b}]2;RVBC\u{7}z#[fg=red]#(id) using 3 cores"),
            alerts: vec![String::from("a\u{1b}[2Jb")],
            ..busy_pro()
        };
        for style in [Style::Plain, Style::Ansi, Style::Tmux] {
            let line = render("{alerts} {verdict}", &summary, style, &PALETTE);
            let body = line
                .replace("\x1b[38;2;250;200;0m", "")
                .replace("\x1b[38;2;250;0;0m", "")
                .replace("\x1b[39m", "");
            assert!(
                !body.chars().any(siltop_core::is_unsafe),
                "{style:?}: {line:?}"
            );
            assert!(
                body.contains("y\u{FFFD}]2;RVBC\u{FFFD}z"),
                "{style:?}: {line:?}"
            );
        }
        let tmux = render("{verdict}", &summary, Style::Tmux, &PALETTE);
        assert_eq!(
            tmux,
            "#[fg=#fac800]Busy: y\u{FFFD}]2;RVBC\u{FFFD}z##[fg=red]##(id) using 3 cores#[fg=default]"
        );
        let plain = render("{verdict}", &summary, Style::Plain, &PALETTE);
        assert!(
            plain.contains("z#[fg=red]#(id)"),
            "only tmux doubles #: {plain}"
        );
    }

    #[test]
    fn style_parse_table() {
        assert_eq!(Style::parse("ANSI"), Some(Style::Ansi));
        assert_eq!(Style::parse("tmux"), Some(Style::Tmux));
        assert_eq!(Style::parse("plain"), Some(Style::Plain));
        assert_eq!(Style::parse("html"), None);
    }

    #[test]
    fn freshness_window() {
        let s = busy_pro();
        assert!(s.is_fresh(s.unix_ms));
        assert!(s.is_fresh(s.unix_ms + 9_999));
        assert!(!s.is_fresh(s.unix_ms + 10_000));
        assert!(
            !s.is_fresh(s.unix_ms - 5_000),
            "a file from the future is suspect"
        );
    }

    #[test]
    fn run_prefers_a_fresh_state_file_and_falls_back_to_sampling() {
        let dir = std::env::temp_dir().join(format!("siltop-line-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent.json");
        std::fs::write(&path, serde_json::to_string(&busy_pro()).unwrap()).unwrap();
        let mut out = Vec::new();
        let now = busy_pro().unix_ms + 2_000;
        run(
            &mut out,
            "{cpu}",
            Style::Plain,
            &PALETTE,
            Some(&path),
            now,
            || panic!("fresh state must not sample"),
        )
        .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "cpu 42%\n");

        let mut out = Vec::new();
        run(
            &mut out,
            "{cpu}",
            Style::Plain,
            &PALETTE,
            Some(&path),
            now + 60_000,
            || {
                Ok(Summary {
                    cpu_pct: Some(3.0),
                    ..Summary::default()
                })
            },
        )
        .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "cpu 3%\n");
        std::fs::write(&path, "{not json").unwrap();
        assert!(read_fresh(&path, now).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summary_from_an_unprimed_snapshot_has_no_rates() {
        let mut snap = Snapshot::empty();
        snap.cpu.scaled = 0.5;
        let s = Summary::from_snapshot(&snap, &Verdict::normal(), 1);
        assert_eq!(s.cpu_pct, None);
        assert_eq!(s.mem_pct, None, "no memory total, no percent");
        assert_eq!(s.power_w, None);
        snap.primed = true;
        snap.memory.total_bytes = 100;
        snap.memory.used_bytes = 25;
        let s = Summary::from_snapshot(&snap, &Verdict::normal(), 1);
        assert_eq!(s.cpu_pct, Some(50.0));
        assert_eq!(s.mem_pct, Some(25.0));
    }

    /// Regression: the line printed `cpu.active` ("cpu 25%") while the
    /// dashboard headline and the history store use `cpu.scaled` ("cpu 3%"),
    /// and its own `{:.0}` rounding printed 12% where the dashboard said 13%.
    #[test]
    fn line_shows_the_dashboards_cpu_figure_and_rounding() {
        let mut snap = Snapshot::empty();
        snap.primed = true;
        snap.cpu.active = 0.28;
        snap.cpu.scaled = 0.125;
        let s = Summary::from_snapshot(&snap, &Verdict::normal(), 1);
        assert_eq!(
            token(&s, "cpu").map(|(text, _)| text),
            Some(format!("cpu {}", percent_display(snap.cpu.scaled)))
        );
        assert_eq!(token(&s, "cpu").unwrap().0, "cpu 13%");
        let s = Summary {
            power_w: Some(18.7),
            temp_c: Some(60.5),
            ..Summary::default()
        };
        assert_eq!(token(&s, "power").unwrap().0, watts_display(18.7));
        assert_eq!(token(&s, "temp").unwrap().0, "61°C");
    }
}
