//! Hand-written command-line parser. No clap: the grammar is small and a
//! table of tests pins it down.

use std::ffi::OsString;
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use siltop_core::{Day, Surface};
use siltop_ui::Theme;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_SHA: Option<&str> = option_env!("SILTOP_GIT_SHA");

pub const INTERVAL_MIN: Duration = Duration::from_millis(100);
pub const INTERVAL_MAX: Duration = Duration::from_secs(60);
pub const PROCS_DEFAULT: usize = 10;
/// Headless modes without `-i` sample once a second: steadier averages.
pub const HEADLESS_INTERVAL: Duration = Duration::from_secs(1);
/// `serve` listens on loopback only unless told otherwise.
pub const SERVE_ADDR: &str = "127.0.0.1:9101";

/// A runtime error, explained in one line on stderr.
pub const EXIT_ERROR: u8 = 1;
/// A usage error, or the dashboard started without a terminal.
pub const EXIT_USAGE: u8 = 2;

/// Every exit status, for `--help`, the man page and the docs.
pub const EXIT_CODES: &[(u8, &str)] = &[
    (0, "success"),
    (EXIT_ERROR, "a runtime error, in one line on stderr"),
    (
        EXIT_USAGE,
        "a usage error, or no terminal for the dashboard",
    ),
    (10, "an update is available; siltop update --check"),
];

/// `YYYY-MM-DD` the binary was built, from `build.rs`.
pub const BUILD_DATE: Option<&str> = option_env!("SILTOP_BUILD_DATE");

/// Which `--help` section a flag is listed under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Dashboard,
    Scripting,
    Info,
    /// Options that belong to one subcommand; listed after the commands.
    Command,
}

impl Section {
    /// Every group, in `--help` order: [`Section::Command`] comes after the
    /// commands themselves.
    pub const ALL: [Self; 4] = [Self::Dashboard, Self::Scripting, Self::Info, Self::Command];

    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Dashboard => "Dashboard options",
            Self::Scripting => "Scripting options",
            Self::Info => "Information",
            Self::Command => "Command options",
        }
    }
}

/// What a flag's value completes to in a shell.
#[derive(Debug, Clone, Copy)]
pub enum Complete {
    /// Free text: a number or a time.
    Text,
    /// A file path.
    File,
    /// One of a fixed list.
    Words(fn() -> Vec<&'static str>),
}

/// Every flag the parser knows. [`parse`] dispatches on it, and `--help`,
/// the shell completions and the man page are generated from [`FLAGS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlagId {
    Interval,
    Theme,
    Surface,
    NoMouse,
    Config,
    Demo,
    PrintConfig,
    Json,
    JsonStream,
    Count,
    Procs,
    Diagnose,
    Help,
    Version,
    Verbose,
    Format,
    Style,
    Out,
    Duration,
    Addr,
    Csv,
    From,
    To,
    Light,
}

/// The one command a command option belongs to.
#[derive(Debug, Clone, Copy)]
pub struct Owner {
    pub command: &'static str,
    /// Further commands that take the same option (`--duration`).
    pub also: &'static [&'static str],
    /// `anything but <command>`, for the conflict message.
    pub others: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct FlagSpec {
    pub id: FlagId,
    pub short: Option<char>,
    pub long: &'static str,
    /// `Some((placeholder, completion))` for a flag that takes a value.
    pub value: Option<(&'static str, Complete)>,
    pub section: Section,
    /// The command this option belongs to; `None` for a global flag.
    pub owner: Option<Owner>,
    pub help: &'static str,
}

impl FlagSpec {
    /// `-i, --interval <TIME>` as `--help` shows it.
    #[must_use]
    pub fn synopsis(&self) -> String {
        let short = self
            .short
            .map_or_else(|| String::from("    "), |c| format!("-{c}, "));
        match self.value {
            Some((placeholder, _)) => format!("{short}{} <{placeholder}>", self.long),
            None => format!("{short}{}", self.long),
        }
    }

    /// Whether `word` (`-i` or `--interval`) names this flag.
    pub fn matches(&self, word: &str) -> bool {
        word == self.long
            || self.short.is_some_and(|c| {
                let mut chars = word.chars();
                chars.next() == Some('-') && chars.next() == Some(c) && chars.next().is_none()
            })
    }
}

fn theme_words() -> Vec<&'static str> {
    std::iter::once(Theme::AUTO)
        .chain(Theme::builtins())
        .chain(Theme::pack())
        .collect()
}

fn surface_words() -> Vec<&'static str> {
    vec!["work", "glance"]
}

fn shell_words() -> Vec<&'static str> {
    Shell::ALL.iter().map(|shell| shell.name()).collect()
}

fn style_words() -> Vec<&'static str> {
    vec!["plain", "ansi", "tmux"]
}

const fn flag(
    id: FlagId,
    short: Option<char>,
    long: &'static str,
    value: Option<(&'static str, Complete)>,
    section: Section,
    help: &'static str,
) -> FlagSpec {
    FlagSpec {
        id,
        short,
        long,
        value,
        section,
        owner: None,
        help,
    }
}

/// A command option: listed under [`Section::Command`] and accepted only
/// with its command, so `--style` without `line` is an error.
macro_rules! command_flag {
    ($id:ident, $long:literal, $value:expr, $command:literal, $help:literal) => {
        command_flag!($id, $long, $value, $command, [], $help)
    };
    ($id:ident, $long:literal, $value:expr, $command:literal, [$($also:literal),*], $help:literal) => {
        FlagSpec {
            id: FlagId::$id,
            short: None,
            long: $long,
            value: $value,
            section: Section::Command,
            owner: Some(Owner {
                command: $command,
                also: &[$($also),*],
                others: concat!("anything but ", $command $(, " or ", $also)*),
            }),
            help: $help,
        }
    };
}

/// The one source of truth for flags.
pub const FLAGS: &[FlagSpec] = &[
    flag(
        FlagId::Interval,
        Some('i'),
        "--interval",
        Some(("TIME", Complete::Text)),
        Section::Dashboard,
        "Sampling interval: 250, 250ms, 1s, 1.5s (100ms..60s)",
    ),
    flag(
        FlagId::Theme,
        None,
        "--theme",
        Some(("NAME", Complete::Words(theme_words))),
        Section::Dashboard,
        "Colour theme (auto, billabong, paper, terminal, ...)",
    ),
    flag(
        FlagId::Surface,
        None,
        "--surface",
        Some(("NAME", Complete::Words(surface_words))),
        Section::Dashboard,
        "Start on the work or glance surface",
    ),
    flag(
        FlagId::NoMouse,
        None,
        "--no-mouse",
        None,
        Section::Dashboard,
        "Do not capture the mouse",
    ),
    flag(
        FlagId::Config,
        None,
        "--config",
        Some(("PATH", Complete::File)),
        Section::Dashboard,
        "Read preferences from PATH",
    ),
    flag(
        FlagId::Demo,
        None,
        "--demo",
        None,
        Section::Dashboard,
        "Play a canned 60 s session instead of this Mac",
    ),
    flag(
        FlagId::Json,
        None,
        "--json",
        None,
        Section::Scripting,
        "Print one JSON sample and exit",
    ),
    flag(
        FlagId::JsonStream,
        None,
        "--json-stream",
        None,
        Section::Scripting,
        "Print one JSON sample per interval (NDJSON)",
    ),
    flag(
        FlagId::Count,
        None,
        "--count",
        Some(("N", Complete::Text)),
        Section::Scripting,
        "Stop --json-stream after N samples",
    ),
    flag(
        FlagId::Procs,
        None,
        "--procs",
        Some(("N", Complete::Text)),
        Section::Scripting,
        "Processes in JSON output (default 10)",
    ),
    flag(
        FlagId::Diagnose,
        None,
        "--diagnose",
        None,
        Section::Scripting,
        "Print a one-line verdict with its evidence",
    ),
    flag(
        FlagId::PrintConfig,
        None,
        "--print-config",
        None,
        Section::Scripting,
        "Print the effective preferences and exit",
    ),
    flag(
        FlagId::Help,
        Some('h'),
        "--help",
        None,
        Section::Info,
        "Print this help",
    ),
    flag(
        FlagId::Version,
        Some('V'),
        "--version",
        None,
        Section::Info,
        "Print the version; with --json, as JSON",
    ),
    flag(
        FlagId::Verbose,
        None,
        "--verbose",
        None,
        Section::Info,
        "Add build details and file paths to --version",
    ),
    command_flag!(
        Format,
        "--format",
        Some(("TEMPLATE", Complete::Text)),
        "line",
        "Template: {cpu} {gpu} {mem} {temp} etc.; {{ }} = { }"
    ),
    command_flag!(
        Style,
        "--style",
        Some(("STYLE", Complete::Words(style_words))),
        "line",
        "Paint line as plain text, ANSI colour or tmux markup"
    ),
    command_flag!(
        Out,
        "--out",
        Some(("FILE", Complete::File)),
        "record",
        "record's output (default siltop-<time>.ptrace)"
    ),
    command_flag!(
        Duration,
        "--duration",
        Some(("SPAN", Complete::Text)),
        "record",
        ["bench"],
        "Stop record or bench after 90s, 10m, 2h"
    ),
    command_flag!(
        Addr,
        "--addr",
        Some(("HOST:PORT", Complete::Text)),
        "serve",
        "Where serve listens (default 127.0.0.1:9101)"
    ),
    command_flag!(
        Csv,
        "--csv",
        None,
        "export",
        "Export as CSV (required: the one format so far)"
    ),
    command_flag!(
        From,
        "--from",
        Some(("WHEN", Complete::Text)),
        "export",
        "Export from a UTC date (2026-09-23) or 24h, 7d ago"
    ),
    command_flag!(
        To,
        "--to",
        Some(("WHEN", Complete::Text)),
        "export",
        "Export up to a UTC date or a span ago (default: now)"
    ),
    command_flag!(
        Light,
        "--light",
        None,
        "bench",
        "Load half the cores: a gentler bench"
    ),
];

/// A subcommand and the words that may follow it.
#[derive(Debug, Clone, Copy)]
pub struct CommandSpec {
    pub name: &'static str,
    /// `(word, argument placeholder, help)`; empty for a command without
    /// sub-words.
    pub subs: &'static [(&'static str, &'static str, &'static str)],
    /// What completes after the command when it has no sub-words.
    pub value: Option<Complete>,
    pub args: &'static str,
    pub help: &'static str,
}

/// The one source of truth for subcommands.
pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "doctor",
        subs: &[],
        value: None,
        args: "",
        help: "Print what this Mac exposes, for bug reports",
    },
    CommandSpec {
        name: "theme",
        subs: &[
            ("list", "", "List every theme"),
            (
                "import",
                "<FILE>",
                "Import a btop .theme file as a siltop theme",
            ),
        ],
        value: None,
        args: "",
        help: "List the themes or import a btop theme (default: list)",
    },
    CommandSpec {
        name: "report",
        subs: &[
            ("today", "", "Energy by app, peaks and hot time today"),
            ("week", "", "The same over the last 7 days"),
            ("month", "", "The same over the last 30 days"),
        ],
        value: None,
        args: "[--json]",
        help: "Energy by app, peak power and temperatures, time spent hot",
    },
    CommandSpec {
        name: "agent",
        subs: &[
            ("status", "", "Show whether the background agent is running"),
            ("install", "", "Run alerts in the background at login"),
            ("uninstall", "", "Stop and remove the background agent"),
            ("run", "", "Run in the foreground, as launchd does"),
        ],
        value: None,
        args: "",
        help: "Alerts in the background as a LaunchAgent (default: status)",
    },
    CommandSpec {
        name: "line",
        subs: &[],
        value: None,
        args: "[--format <TEMPLATE>] [--style <STYLE>]",
        help: "One status line for tmux, SketchyBar or Starship",
    },
    CommandSpec {
        name: "record",
        subs: &[],
        value: None,
        args: "[--out <FILE>] [--duration <SPAN>]",
        help: "Record this Mac to a .ptrace file",
    },
    CommandSpec {
        name: "bench",
        subs: &[],
        value: None,
        args: "[--duration <SPAN>] [--light] [--json]",
        help: "Sustained CPU test: clocks, heat, throttling",
    },
    CommandSpec {
        name: "replay",
        subs: &[],
        value: Some(Complete::File),
        args: "<FILE>",
        help: "Play a .ptrace in the dashboard; p pauses, +/- speed",
    },
    CommandSpec {
        name: "serve",
        subs: &[],
        value: None,
        args: "[--addr <HOST:PORT>]",
        help: "Serve Prometheus metrics over local HTTP",
    },
    CommandSpec {
        name: "fleet",
        subs: &[],
        value: None,
        args: "<HOST>...",
        help: "Several Macs in one terminal over your own ssh",
    },
    CommandSpec {
        name: "export",
        subs: &[],
        value: None,
        args: "--csv [--from <WHEN>] [--to <WHEN>]",
        help: "Print saved history as CSV",
    },
    CommandSpec {
        name: "onboarding",
        subs: &[],
        value: None,
        args: "",
        help: "Show the first-run screens again",
    },
    CommandSpec {
        name: "update",
        subs: &[],
        value: None,
        args: "--check | --yes | --rollback",
        help: "Check and say how to install; --yes installs",
    },
    CommandSpec {
        name: "feedback",
        subs: &[],
        value: None,
        args: "--bug | --copy",
        help: "Open a GitHub issue; --bug for a bug report",
    },
    CommandSpec {
        name: "reset",
        subs: &[],
        value: None,
        args: "--yes",
        help: "Delete config, state and history",
    },
    CommandSpec {
        name: "completions",
        subs: &[],
        value: Some(Complete::Words(shell_words)),
        args: "<SHELL>",
        help: "Print a zsh, bash or fish completion script",
    },
    CommandSpec {
        name: "man",
        subs: &[],
        value: None,
        args: "",
        help: "Print the manual page (roff)",
    },
];

const EXAMPLES: &[(&str, &str)] = &[
    ("siltop", "the dashboard"),
    ("siltop --surface glance -i 2s", "a quiet corner monitor"),
    (
        "siltop --json-stream -i 1s --count 60",
        "a minute of NDJSON for a script",
    ),
    ("siltop --diagnose", "why is this Mac slow, in one line"),
    (
        "siltop report week --json",
        "a week of energy by app and peak temperatures",
    ),
    (
        "siltop export --csv --from 24h > day.csv",
        "the last day of saved history",
    ),
    (
        "siltop fleet studio mini.local",
        "this Mac beside two others, over ssh",
    ),
    (
        "siltop completions zsh > ~/.zfunc/_siltop",
        "tab completion",
    ),
    ("siltop man | mandoc -a", "the manual"),
];

/// Width of the `--help` left column, gutter included.
const HELP_COL: usize = 26;

/// One `--help` row: the head padded to [`HELP_COL`], or, when it is too
/// long for that, the help on its own line so the two never run together.
fn help_row(out: &mut String, head: &str, help: &str) {
    use std::fmt::Write as _;
    let head = head.trim_end();
    if head.chars().count() + 2 <= HELP_COL {
        let _ = writeln!(out, "  {head:<HELP_COL$}{help}");
    } else {
        let _ = writeln!(out, "  {head}\n  {:HELP_COL$}{help}", "");
    }
}

/// `--help`, generated from [`FLAGS`] and [`COMMANDS`].
#[must_use]
pub fn usage() -> String {
    use std::fmt::Write as _;
    let mut out = String::from("Usage: siltop [OPTIONS]\n");
    for command in COMMANDS {
        let args = if command.subs.is_empty() {
            command.args.to_owned()
        } else {
            let words: Vec<String> = command
                .subs
                .iter()
                .map(|(word, arg, _)| format!("{word} {arg}").trim_end().to_owned())
                .collect();
            format!("[{}] {}", words.join(" | "), command.args)
        };
        let line = format!("siltop {} {args}", command.name);
        let _ = writeln!(out, "       {}", line.trim_end());
    }
    out.push_str("\nApple Silicon system monitor. No telemetry, no sudo.\n");
    let section = |out: &mut String, section: Section| {
        let _ = write!(out, "\n{}:\n", section.title());
        for spec in FLAGS.iter().filter(|spec| spec.section == section) {
            help_row(out, &spec.synopsis(), spec.help);
        }
    };
    for global in Section::ALL.into_iter().filter(|s| *s != Section::Command) {
        section(&mut out, global);
    }
    out.push_str("\nCommands:\n");
    for command in COMMANDS {
        if command.subs.is_empty() {
            help_row(
                &mut out,
                &format!("{} {}", command.name, command.args),
                command.help,
            );
        }
        for (word, arg, help) in command.subs {
            help_row(&mut out, &format!("{} {word} {arg}", command.name), help);
        }
    }
    section(&mut out, Section::Command);
    out.push_str("\nExamples:\n");
    for (line, what) in EXAMPLES {
        let _ = writeln!(out, "  {line}\n      {what}");
    }
    out.push_str("\nExit status:\n");
    for (code, what) in EXIT_CODES {
        help_row(&mut out, &code.to_string(), what);
    }
    out.push_str("\nIn the dashboard, m opens the menu, ? lists every key and q quits.\n");
    out
}

/// A shell `siltop completions` writes a script for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
}

impl Shell {
    pub const ALL: [Self; 3] = [Self::Zsh, Self::Bash, Self::Fish];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Zsh => "zsh",
            Self::Bash => "bash",
            Self::Fish => "fish",
        }
    }

    fn by_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|shell| shell.name().eq_ignore_ascii_case(name.trim()))
    }
}

/// What to do after parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Tui,
    Help,
    Version,
    PrintConfig,
    Json,
    JsonStream,
    Diagnose,
    Doctor,
    Completions(Shell),
    Man,
    /// `theme list`, or `theme import` with its file in [`Cli::file`].
    Theme,
    /// `siltop report <span>`; `--json` sets [`Cli::report_json`].
    Report(siltop_core::store::Span),
    Agent(AgentAction),
    /// `siltop line`, shaped by [`Cli::line_format`] and [`Cli::line_style`].
    Line,
    Record,
    /// `siltop replay <FILE>`, the file in [`Cli::file`].
    Replay,
    Serve,
    Export,
    /// `siltop bench`: [`Cli::duration`], [`Cli::bench_light`] and
    /// `--json` in [`Cli::report_json`].
    Bench,
    /// `siltop fleet <HOST>...`, the hosts in [`Cli::hosts`].
    Fleet,
    /// `siltop onboarding`: the first-run screens again.
    Onboarding,
    /// `siltop update`.
    Update(UpdateAction),
    /// `siltop feedback`.
    Feedback,
    /// `siltop reset`.
    Reset,
}

/// `siltop update` and its flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateAction {
    /// Bare `update`: check, and say how to install a newer one.
    Ask,
    /// Print the result and exit 0 or 10.
    Check,
    /// Install without asking.
    Yes,
    /// Restore `siltop.old`.
    Rollback,
}

impl Mode {
    /// The flag or command that selects this mode, for conflict messages.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Tui => "",
            Self::Help => "--help",
            Self::Version => "--version",
            Self::PrintConfig => "--print-config",
            Self::Json => "--json",
            Self::JsonStream => "--json-stream",
            Self::Diagnose => "--diagnose",
            Self::Doctor => "doctor",
            Self::Completions(_) => "completions",
            Self::Man => "man",
            Self::Theme => "theme",
            Self::Report(_) => "report",
            Self::Agent(_) => "agent",
            Self::Line => "line",
            Self::Record => "record",
            Self::Replay => "replay",
            Self::Serve => "serve",
            Self::Export => "export",
            Self::Bench => "bench",
            Self::Fleet => "fleet",
            Self::Onboarding => "onboarding",
            Self::Update(_) => "update",
            Self::Feedback => "feedback",
            Self::Reset => "reset",
        }
    }
}

/// `siltop agent <action>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentAction {
    Status,
    Install,
    Uninstall,
    Run,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cli {
    pub mode: Mode,
    pub interval: Option<Duration>,
    /// Canonical built-in name, or `None` for auto.
    pub theme: Option<String>,
    pub surface: Option<Surface>,
    pub no_mouse: bool,
    pub config: Option<PathBuf>,
    pub count: Option<u64>,
    pub procs: usize,
    /// The file a subcommand reads: `theme import`, `replay`.
    pub file: Option<PathBuf>,
    /// The ssh hosts `fleet` watches beside this Mac.
    pub hosts: Vec<String>,
    /// Where `record` writes.
    pub out: Option<PathBuf>,
    /// How long `record` or `bench` runs; `None` until interrupted (record)
    /// or [`crate::bench::DEFAULT_DURATION`] (bench).
    pub duration: Option<Duration>,
    /// Where `serve` listens.
    pub addr: Option<SocketAddr>,
    /// `export --csv`; the only export format so far, and required.
    pub csv: bool,
    pub from: Option<When>,
    pub to: Option<When>,
    /// Replay the canned demo session instead of sampling this Mac.
    pub demo: bool,
    /// `--version --verbose`: build details and paths too.
    pub verbose: bool,
    /// `--version --json`: the same facts as one JSON object.
    pub version_json: bool,
    /// `report --json` and `bench --json`: the report as one JSON object.
    pub report_json: bool,
    /// `bench --light`: half the cores.
    pub bench_light: bool,
    /// `line --format`: the template, or `None` for the default.
    pub line_format: Option<String>,
    pub line_style: crate::line::Style,
    /// `feedback --bug`.
    pub feedback_bug: bool,
    /// `feedback --copy`.
    pub feedback_copy: bool,
    /// `reset --yes`: skip the typed confirmation.
    pub reset_yes: bool,
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            mode: Mode::Tui,
            interval: None,
            theme: None,
            surface: None,
            no_mouse: false,
            config: None,
            count: None,
            procs: PROCS_DEFAULT,
            file: None,
            hosts: Vec::new(),
            out: None,
            duration: None,
            addr: None,
            csv: false,
            from: None,
            to: None,
            demo: false,
            verbose: false,
            version_json: false,
            report_json: false,
            bench_light: false,
            line_format: None,
            line_style: crate::line::Style::Plain,
            feedback_bug: false,
            feedback_copy: false,
            reset_yes: false,
        }
    }
}

impl Cli {
    /// The interval headless modes sample at.
    #[must_use]
    pub fn headless_interval(&self) -> Duration {
        self.interval.unwrap_or(HEADLESS_INTERVAL)
    }
}

/// An `export` bound: a UTC calendar day or a span before now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    Day(Day),
    Ago(Duration),
}

impl When {
    /// Unix seconds at `now`. A day as the upper bound means its end, so
    /// `--from 2026-09-23 --to 2026-09-23` is that whole day.
    #[must_use]
    pub fn resolve(self, now: u64, upper: bool) -> u64 {
        match self {
            Self::Ago(span) => now.saturating_sub(span.as_secs()),
            Self::Day(day) => {
                let start = u64::try_from(day.days_since_epoch()).unwrap_or(0) * 86_400;
                if upper { start + 86_400 } else { start }
            }
        }
    }
}

const SPAN_HINT: &str = "expected a span such as 90s, 10m, 2h or 7d";

/// `90s`, `10m`, `2h`, `7d`; a bare number is seconds. At least a second.
pub fn parse_span(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let (number, unit) = match text.char_indices().last() {
        Some((i, c)) if c.is_ascii_alphabetic() => (&text[..i], c),
        _ => (text, 's'),
    };
    let scale = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        'd' => 86_400,
        _ => return Err(String::from(SPAN_HINT)),
    };
    let value: u64 = number.trim().parse().map_err(|_| String::from(SPAN_HINT))?;
    let secs = value
        .checked_mul(scale)
        .ok_or_else(|| String::from(SPAN_HINT))?;
    if secs == 0 {
        return Err(String::from("must be at least 1s"));
    }
    Ok(Duration::from_secs(secs))
}

fn parse_when(text: &str) -> Result<When, String> {
    if let Some(day) = Day::parse(text.trim()) {
        return Ok(When::Day(day));
    }
    parse_span(text)
        .map(When::Ago)
        .map_err(|_| String::from("expected a date (2026-09-23) or a span (24h, 7d)"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    Unknown(String),
    MissingValue(&'static str),
    BadValue {
        flag: &'static str,
        value: String,
        why: String,
    },
    Conflict(&'static str, &'static str),
    /// `doctor --json`: a command with no JSON form.
    NoJson(&'static str),
    /// `export` without `--csv`: a command missing a flag it requires.
    NeedsFlag(&'static str, &'static str),
    /// `report --json week`: a sub-word after an option instead of right
    /// after its command.
    Misplaced {
        command: &'static str,
        word: String,
    },
    NotUtf8,
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(arg) => write!(f, "unknown argument {arg:?}"),
            Self::MissingValue(flag) => write!(f, "{flag} needs a value"),
            Self::BadValue { flag, value, why } => write!(f, "{flag} {value:?}: {why}"),
            Self::Conflict(a, b) => write!(f, "{a} cannot be combined with {b}"),
            Self::NoJson(command) => write!(
                f,
                "{command} has no JSON output; --json on its own prints one sample, and report takes it too"
            ),
            Self::NeedsFlag(command, flag) => write!(f, "{command} needs {flag}"),
            Self::Misplaced { command, word } => write!(
                f,
                "{word:?} goes right after {command}: siltop {command} {word} [OPTIONS]"
            ),
            Self::NotUtf8 => write!(f, "arguments must be UTF-8"),
        }
    }
}

impl std::error::Error for CliError {}

/// `250` and `250ms` are milliseconds; `1s` and `1.5s` are seconds.
pub fn parse_interval(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let (number, scale_ms) = if let Some(ms) = text.strip_suffix("ms") {
        (ms, 1.0)
    } else if let Some(s) = text.strip_suffix('s') {
        (s, 1000.0)
    } else {
        (text, 1.0)
    };
    let value: f64 = number
        .trim()
        .parse()
        .map_err(|_| String::from("expected a number such as 250, 250ms or 1s"))?;
    if !value.is_finite() || value < 0.0 {
        return Err(String::from("expected a positive time"));
    }
    let ms = value * scale_ms;
    if ms > INTERVAL_MAX.as_millis() as f64 {
        return Err(String::from("must be at most 60s"));
    }
    let interval = Duration::from_micros((ms * 1000.0).round() as u64);
    if interval < INTERVAL_MIN {
        return Err(String::from("must be at least 100ms"));
    }
    Ok(interval)
}

fn parse_theme(name: &str) -> Result<Option<String>, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
        return Ok(None);
    }
    Theme::by_name(trimmed)
        .map(|theme| Some(String::from(theme.name)))
        .ok_or_else(|| format!("unknown theme; try {}", Theme::names().join(", ")))
}

fn parse_surface(name: &str) -> Result<Surface, String> {
    match name.trim().to_ascii_lowercase().as_str() {
        "work" => Ok(Surface::Work),
        "glance" => Ok(Surface::Glance),
        _ => Err(String::from("expected work or glance")),
    }
}

fn parse_count<T>(text: &str, min: T) -> Result<T, String>
where
    T: std::str::FromStr + PartialOrd + fmt::Display + Copy,
{
    let value: T = text
        .trim()
        .parse()
        .map_err(|_| String::from("expected a whole number"))?;
    if value < min {
        return Err(format!("must be at least {min}"));
    }
    Ok(value)
}

fn set_mode(cli: &mut Cli, mode: Mode, flag: &'static str) -> Result<(), CliError> {
    if cli.mode == Mode::Tui || cli.mode == mode {
        cli.mode = mode;
        return Ok(());
    }
    // `doctor --json` and `--json doctor` alike: say what --json is for.
    let is_command = |name: &str| COMMANDS.iter().any(|c| c.name == name);
    match (cli.mode, mode) {
        (Mode::Json, other) | (other, Mode::Json) if is_command(other.name()) => {
            Err(CliError::NoJson(other.name()))
        }
        _ => Err(CliError::Conflict(cli.mode.name(), flag)),
    }
}

/// A word the parser has no use for: a sub-word of the command already
/// chosen, typed after an option, gets a hint instead of a bare "unknown".
fn unknown_word(cli: &Cli, word: String) -> CliError {
    let misplaced = COMMANDS
        .iter()
        .find(|command| command.name == cli.mode.name())
        .filter(|command| command.subs.iter().any(|(sub, ..)| *sub == word));
    match misplaced {
        Some(command) => CliError::Misplaced {
            command: command.name,
            word,
        },
        None => CliError::Unknown(word),
    }
}

/// Flags that parse on their own but make no sense together. `--demo`
/// cannot feed `doctor` or `agent`: both describe this Mac, and a canned
/// session has nothing to say there.
fn check_combinations(cli: &Cli, seen: &Seen) -> Result<(), CliError> {
    // Checked last: hosts may follow flags (`fleet --demo a b`).
    if cli.mode == Mode::Fleet && cli.hosts.is_empty() {
        return Err(CliError::MissingValue("fleet"));
    }
    if cli.count.is_some() && cli.mode != Mode::JsonStream {
        return Err(CliError::Conflict("--count", "anything but --json-stream"));
    }
    if cli.verbose {
        return Err(CliError::Conflict("--verbose", "anything but --version"));
    }
    // Each command option belongs to exactly one command.
    if let Some((spec, owner)) = seen.owned.iter().find_map(|spec| {
        spec.owner
            .filter(|owner| {
                owner.command != cli.mode.name() && !owner.also.contains(&cli.mode.name())
            })
            .map(|owner| (spec, owner))
    }) {
        return Err(CliError::Conflict(spec.long, owner.others));
    }
    match cli.mode {
        Mode::Doctor if cli.demo => Err(CliError::Conflict("--demo", "doctor")),
        Mode::Agent(_) if cli.demo => Err(CliError::Conflict("--demo", "agent")),
        Mode::Report(_) if cli.demo => Err(CliError::Conflict("--demo", "report")),
        Mode::Replay if cli.demo => Err(CliError::Conflict("--demo", "replay")),
        Mode::Export if cli.demo => Err(CliError::Conflict("--demo", "export")),
        Mode::Export if !cli.csv => Err(CliError::NeedsFlag("export", "--csv")),
        _ => Ok(()),
    }
}

fn bad(flag: &'static str, value: &str, why: String) -> CliError {
    CliError::BadValue {
        flag,
        value: value.to_owned(),
        why,
    }
}

/// What the parse saw beyond [`Cli`] itself: `-h` and `-V` are
/// remembered, not applied, so they win at the end; command options are
/// checked against the command once every word is in.
#[derive(Default)]
struct Seen {
    help: bool,
    version: bool,
    owned: Vec<&'static FlagSpec>,
}

/// `expected a, b or c`.
fn choices(words: &[&str]) -> String {
    match words.split_last() {
        Some((last, rest)) if !rest.is_empty() => {
            format!("expected {} or {last}", rest.join(", "))
        }
        Some((last, _)) => format!("expected {last}"),
        None => String::from("expected nothing more"),
    }
}

/// Applies one flag from [`FLAGS`] with its (possibly empty) value.
fn apply_flag(
    cli: &mut Cli,
    seen: &mut Seen,
    spec: &'static FlagSpec,
    value: &str,
) -> Result<(), CliError> {
    let flag = spec.long;
    if spec.owner.is_some() {
        seen.owned.push(spec);
    }
    match spec.id {
        FlagId::Help => seen.help = true,
        FlagId::Version => seen.version = true,
        FlagId::Verbose => cli.verbose = true,
        FlagId::Interval => {
            cli.interval = Some(parse_interval(value).map_err(|why| bad(flag, value, why))?);
        }
        FlagId::Theme => cli.theme = parse_theme(value).map_err(|why| bad(flag, value, why))?,
        FlagId::Surface => {
            cli.surface = Some(parse_surface(value).map_err(|why| bad(flag, value, why))?);
        }
        FlagId::Config => {
            if value.is_empty() {
                return Err(bad(flag, value, String::from("expected a path")));
            }
            cli.config = Some(PathBuf::from(value));
        }
        FlagId::Count => {
            cli.count = Some(parse_count::<u64>(value, 1).map_err(|why| bad(flag, value, why))?);
        }
        FlagId::Procs => {
            cli.procs = parse_count::<usize>(value, 0).map_err(|why| bad(flag, value, why))?;
        }
        FlagId::NoMouse => cli.no_mouse = true,
        FlagId::Demo => cli.demo = true,
        FlagId::PrintConfig => set_mode(cli, Mode::PrintConfig, flag)?,
        // After `report`, `--json` shapes the report instead.
        FlagId::Json if matches!(cli.mode, Mode::Report(_) | Mode::Bench) => {
            cli.report_json = true;
        }
        FlagId::Json => set_mode(cli, Mode::Json, flag)?,
        FlagId::JsonStream => set_mode(cli, Mode::JsonStream, flag)?,
        FlagId::Diagnose => set_mode(cli, Mode::Diagnose, flag)?,
        FlagId::Format => {
            if value.trim().is_empty() {
                return Err(bad(flag, value, String::from("expected a template")));
            }
            cli.line_format = Some(value.to_owned());
        }
        FlagId::Style => {
            cli.line_style = crate::line::Style::parse(value)
                .ok_or_else(|| bad(flag, value, choices(&style_words())))?;
        }
        FlagId::Out => {
            if value.is_empty() {
                return Err(bad(flag, value, String::from("expected a path")));
            }
            cli.out = Some(PathBuf::from(value));
        }
        FlagId::Duration => {
            cli.duration = Some(parse_span(value).map_err(|why| bad(flag, value, why))?);
        }
        FlagId::Addr => {
            let why = || String::from("expected HOST:PORT, e.g. 127.0.0.1:9101");
            cli.addr = Some(value.trim().parse().map_err(|_| bad(flag, value, why()))?);
        }
        FlagId::Csv => cli.csv = true,
        FlagId::From => cli.from = Some(parse_when(value).map_err(|why| bad(flag, value, why))?),
        FlagId::To => cli.to = Some(parse_when(value).map_err(|why| bad(flag, value, why))?),
        FlagId::Light => cli.bench_light = true,
    }
    Ok(())
}

/// The word after a command when there is one: `report --json` and a
/// bare `agent` both leave the default sub-word to the command.
fn sub_word(args: &mut std::vec::IntoIter<String>) -> Option<String> {
    let word = args
        .as_slice()
        .first()
        .filter(|word| !word.starts_with('-'))
        .cloned();
    if word.is_some() {
        args.next();
    }
    word
}

/// `expected list or import`: the command's sub-words from [`COMMANDS`].
fn expected(command: &CommandSpec) -> String {
    let words: Vec<&str> = command.subs.iter().map(|(word, ..)| *word).collect();
    choices(&words)
}

/// The file a sub-word takes, as in `theme import <FILE>`.
fn file_arg(
    args: &mut std::vec::IntoIter<String>,
    what: &'static str,
) -> Result<PathBuf, CliError> {
    args.next()
        .filter(|file| !file.is_empty())
        .map(PathBuf::from)
        .ok_or(CliError::MissingValue(what))
}

/// `onboarding`, `update`, `feedback` and `reset`, which share no other grammar.
fn consent_mode(
    cli: &mut Cli,
    name: &'static str,
    args: &mut std::vec::IntoIter<String>,
) -> Result<Mode, CliError> {
    Ok(match name {
        "onboarding" => Mode::Onboarding,
        "update" => {
            let flag = args.as_slice().first().cloned();
            match flag.as_deref() {
                Some("--check") => {
                    args.next();
                    Mode::Update(UpdateAction::Check)
                }
                Some("--yes") => {
                    args.next();
                    Mode::Update(UpdateAction::Yes)
                }
                Some("--rollback") => {
                    args.next();
                    Mode::Update(UpdateAction::Rollback)
                }
                Some(other) if other.starts_with('-') => {
                    return Err(bad(
                        name,
                        other,
                        String::from("expected --check, --yes or --rollback"),
                    ));
                }
                _ => Mode::Update(UpdateAction::Ask),
            }
        }
        "feedback" => {
            while let Some(flag) = args.as_slice().first().cloned() {
                match flag.as_str() {
                    "--bug" => cli.feedback_bug = true,
                    "--copy" => cli.feedback_copy = true,
                    other if other.starts_with('-') => {
                        return Err(bad(name, other, String::from("expected --bug or --copy")));
                    }
                    _ => break,
                }
                args.next();
            }
            Mode::Feedback
        }
        _ => {
            if args.as_slice().first().is_some_and(|flag| flag == "--yes") {
                args.next();
                cli.reset_yes = true;
            }
            Mode::Reset
        }
    })
}

/// Applies a subcommand from [`COMMANDS`], taking the words it needs.
fn apply_command(
    cli: &mut Cli,
    command: &CommandSpec,
    args: &mut std::vec::IntoIter<String>,
) -> Result<(), CliError> {
    let name = command.name;
    let unknown = |word: &str| bad(name, word, expected(command));
    let mode = match name {
        "doctor" => Mode::Doctor,
        "man" => Mode::Man,
        "completions" => {
            let word = args.next().ok_or(CliError::MissingValue(name))?;
            let shell = Shell::by_name(&word).ok_or_else(|| {
                let choices = shell_words().join(", ");
                bad(name, &word, format!("expected one of {choices}"))
            })?;
            Mode::Completions(shell)
        }
        "theme" => match sub_word(args).as_deref() {
            None | Some("list") => Mode::Theme,
            Some("import") => {
                cli.file = Some(file_arg(args, "theme import")?);
                Mode::Theme
            }
            Some(other) => return Err(unknown(other)),
        },
        "report" => {
            let span = match sub_word(args) {
                None => siltop_core::store::Span::Today,
                Some(word) => crate::report::span_by_name(&word).ok_or_else(|| unknown(&word))?,
            };
            Mode::Report(span)
        }
        "agent" => match sub_word(args).as_deref() {
            None | Some("status") => Mode::Agent(AgentAction::Status),
            Some("install") => Mode::Agent(AgentAction::Install),
            Some("uninstall") => Mode::Agent(AgentAction::Uninstall),
            Some("run") => Mode::Agent(AgentAction::Run),
            Some(other) => return Err(unknown(other)),
        },
        "line" => Mode::Line,
        "record" => Mode::Record,
        "serve" => Mode::Serve,
        "export" => Mode::Export,
        "bench" => Mode::Bench,
        "fleet" => {
            while let Some(host) = sub_word(args) {
                cli.hosts.push(host);
            }
            Mode::Fleet
        }
        "replay" => {
            cli.file = Some(file_arg(args, "replay")?);
            Mode::Replay
        }
        "onboarding" | "update" | "feedback" | "reset" => consent_mode(cli, name, args)?,
        other => return Err(CliError::Unknown(other.to_owned())),
    };
    // `--json report week` reads the same as `report week --json`.
    if matches!(mode, Mode::Report(_) | Mode::Bench) && cli.mode == Mode::Json {
        cli.mode = Mode::Tui;
        cli.report_json = true;
    }
    set_mode(cli, mode, name)
}

/// Parse arguments after the program name.
pub fn parse<I, A>(args: I) -> Result<Cli, CliError>
where
    I: IntoIterator<Item = A>,
    A: Into<OsString>,
{
    let mut args = args
        .into_iter()
        .map(|arg| arg.into().into_string().map_err(|_| CliError::NotUtf8))
        .collect::<Result<Vec<String>, CliError>>()?
        .into_iter();
    let mut cli = Cli::default();
    let mut seen = Seen::default();
    while let Some(arg) = args.next() {
        // `--flag=value` is the same as `--flag value`.
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => {
                (name.to_owned(), Some(value.to_owned()))
            }
            _ => (arg.clone(), None),
        };
        if let Some(spec) = FLAGS.iter().find(|spec| spec.matches(&name)) {
            let value = match (spec.value, inline) {
                (Some(_), Some(value)) => value,
                (Some(_), None) => args.next().ok_or(CliError::MissingValue(spec.long))?,
                (None, Some(_)) => return Err(CliError::Unknown(arg)),
                (None, None) => String::new(),
            };
            apply_flag(&mut cli, &mut seen, spec, &value)?;
        } else if let Some(command) = COMMANDS
            .iter()
            .find(|command| inline.is_none() && command.name == name)
        {
            apply_command(&mut cli, command, &mut args)?;
        } else if cli.mode == Mode::Fleet && inline.is_none() && !arg.starts_with('-') {
            // `fleet --demo a b`: hosts may follow flags too.
            cli.hosts.push(arg);
        } else {
            return Err(unknown_word(&cli, arg));
        }
    }
    // Help and version win over everything else, like every other CLI.
    if seen.help {
        cli.mode = Mode::Help;
    } else if seen.version {
        cli.version_json = cli.mode == Mode::Json;
        cli.mode = Mode::Version;
    } else {
        check_combinations(&cli, &seen)?;
    }
    Ok(cli)
}

#[must_use]
pub fn version_line() -> String {
    match GIT_SHA.filter(|sha| !sha.is_empty()) {
        Some(sha) => format!("siltop {VERSION} ({sha})"),
        None => format!("siltop {VERSION}"),
    }
}

/// Where this binary came from: commit and build date, when known. The
/// settings About page and `--version --verbose` both show these rows.
#[must_use]
pub fn build_facts() -> Vec<(&'static str, String)> {
    [("commit", GIT_SHA), ("built", BUILD_DATE)]
        .into_iter()
        .filter_map(|(key, value)| value.filter(|v| !v.is_empty()).map(|v| (key, v.to_owned())))
        .collect()
}

/// `--version --json`: `name`, `version`, then each row as a string field.
#[must_use]
pub fn json_version(rows: &[(&'static str, String)]) -> String {
    let mut object = serde_json::Map::new();
    object.insert("name".into(), "siltop".into());
    object.insert("version".into(), VERSION.into());
    for (key, value) in rows {
        object.insert((*key).into(), value.as_str().into());
    }
    format!("{}\n", serde_json::Value::Object(object))
}

/// `--version --verbose`: the version line, then aligned `key value` rows.
#[must_use]
pub fn verbose_version(rows: &[(&'static str, String)]) -> String {
    use std::fmt::Write as _;
    let mut out = format!("siltop {VERSION}\n");
    for (key, value) in rows {
        let _ = writeln!(out, "{key:<9}{value}");
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn ok(args: &[&str]) -> Cli {
        parse(args.iter().copied()).unwrap_or_else(|err| panic!("{args:?}: {err}"))
    }

    fn err(args: &[&str]) -> CliError {
        match parse(args.iter().copied()) {
            Ok(cli) => panic!("{args:?} parsed as {cli:?}"),
            Err(err) => err,
        }
    }

    #[test]
    fn theme_subcommand_table() {
        let table: &[(&[&str], Option<&str>)] = &[
            (&["theme"], None),
            (&["theme", "list"], None),
            (&["theme", "import", "dracula.theme"], Some("dracula.theme")),
        ];
        for (args, file) in table {
            let cli = ok(args);
            assert_eq!(cli.mode, Mode::Theme, "{args:?}");
            assert_eq!(cli.file.as_deref(), file.map(std::path::Path::new));
        }
    }

    #[test]
    fn theme_subcommand_errors() {
        assert_eq!(
            err(&["theme", "import"]),
            CliError::MissingValue("theme import")
        );
        assert!(matches!(
            err(&["theme", "paint"]),
            CliError::BadValue { flag: "theme", .. }
        ));
        assert_eq!(
            err(&["theme", "doctor"]),
            CliError::BadValue {
                flag: "theme",
                value: String::from("doctor"),
                why: String::from("expected list or import"),
            }
        );
        assert_eq!(
            err(&["doctor", "theme"]),
            CliError::Conflict("doctor", "theme")
        );
    }

    #[test]
    fn theme_flag_accepts_pack_names() {
        assert_eq!(
            ok(&["--theme", "rose-pine"]).theme.as_deref(),
            Some("rose-pine")
        );
    }

    #[test]
    fn agent_subcommand_table() {
        let table: &[(&[&str], AgentAction)] = &[
            (&["agent"], AgentAction::Status),
            (&["agent", "status"], AgentAction::Status),
            (&["agent", "install"], AgentAction::Install),
            (&["agent", "uninstall"], AgentAction::Uninstall),
            (&["agent", "run"], AgentAction::Run),
            (
                &["--config", "/tmp/c.toml", "agent", "run"],
                AgentAction::Run,
            ),
        ];
        for (args, action) in table {
            assert_eq!(ok(args).mode, Mode::Agent(*action), "{args:?}");
        }
        assert!(matches!(
            err(&["agent", "start"]),
            CliError::BadValue { flag: "agent", .. }
        ));
        assert_eq!(err(&["agent", "run", "--json"]), CliError::NoJson("agent"));
        assert_eq!(
            err(&["--demo", "agent", "run"]),
            CliError::Conflict("--demo", "agent")
        );
    }

    #[test]
    fn line_subcommand_and_its_flags() {
        let cli = ok(&["line"]);
        assert_eq!(cli.mode, Mode::Line);
        assert_eq!(cli.line_format, None);
        let cli = ok(&["line", "--format", "{cpu} {temp}", "--style=tmux"]);
        assert_eq!(cli.line_format.as_deref(), Some("{cpu} {temp}"));
        assert_eq!(cli.line_style, crate::line::Style::Tmux);
        assert!(
            ok(&["--demo", "line"]).demo,
            "demo feeds the fallback sample"
        );
        assert_eq!(
            err(&["--format", "{cpu}"]),
            CliError::Conflict("--format", "anything but line")
        );
        assert_eq!(
            err(&["--json", "--style", "ansi"]),
            CliError::Conflict("--style", "anything but line")
        );
        assert!(matches!(
            err(&["line", "--style", "html"]),
            CliError::BadValue {
                flag: "--style",
                ..
            }
        ));
        assert!(matches!(
            err(&["line", "--format", " "]),
            CliError::BadValue {
                flag: "--format",
                ..
            }
        ));
    }

    #[test]
    fn theme_import_needs_a_non_empty_file() {
        assert_eq!(
            err(&["theme", "import", ""]),
            CliError::MissingValue("theme import")
        );
        assert_eq!(err(&["theme", "list", "--json"]), CliError::NoJson("theme"));
    }

    #[test]
    fn report_subcommand_table() {
        use siltop_core::store::Span;
        for (args, span, json) in [
            (&["report"][..], Span::Today, false),
            (&["report", "today"], Span::Today, false),
            (&["report", "week"], Span::Week, false),
            (&["report", "month", "--json"], Span::Month, true),
            (&["report", "--json"], Span::Today, true),
            (&["--json", "report", "week"], Span::Week, true),
        ] {
            let cli = ok(args);
            assert_eq!(cli.mode, Mode::Report(span), "{args:?}");
            assert_eq!(cli.report_json, json, "{args:?}");
        }
    }

    #[test]
    fn fleet_takes_hosts_before_and_after_flags() {
        let cli = ok(&["fleet", "studio", "mini.local"]);
        assert_eq!(cli.mode, Mode::Fleet);
        assert_eq!(cli.hosts, ["studio", "mini.local"]);
        let cli = ok(&["fleet", "--demo", "a", "-i", "2s", "b"]);
        assert_eq!(cli.hosts, ["a", "b"]);
        assert!(cli.demo);
        assert_eq!(ok(&["--demo", "fleet", "a"]).hosts, ["a"]);
    }

    #[test]
    fn fleet_needs_a_host_and_never_takes_a_flag_as_one() {
        assert!(matches!(err(&["fleet"]), CliError::MissingValue("fleet")));
        assert!(matches!(err(&["fleet", "--bogus"]), CliError::Unknown(_)));
        assert!(
            matches!(err(&["studio"]), CliError::Unknown(_)),
            "hosts only after fleet"
        );
    }

    #[test]
    fn record_replay_serve_export_table() {
        let cli = ok(&["record", "--out", "a.ptrace", "--duration", "10m"]);
        assert_eq!(cli.mode, Mode::Record);
        assert_eq!(cli.out, Some(PathBuf::from("a.ptrace")));
        assert_eq!(cli.duration, Some(Duration::from_secs(600)));
        assert_eq!(ok(&["record", "--demo", "-i", "250"]).mode, Mode::Record);
        let cli = ok(&["replay", "b.ptrace"]);
        assert_eq!(cli.mode, Mode::Replay);
        assert_eq!(cli.file, Some(PathBuf::from("b.ptrace")));
        let cli = ok(&["serve", "--addr=0.0.0.0:9200"]);
        assert_eq!(cli.mode, Mode::Serve);
        assert_eq!(cli.addr.map(|a| a.port()), Some(9200));
        let cli = ok(&["export", "--csv", "--from", "2026-09-01", "--to", "24h"]);
        assert_eq!(cli.mode, Mode::Export);
        assert_eq!(
            cli.from,
            Some(When::Day(Day::from_ymd(2026, 9, 1).unwrap()))
        );
        assert_eq!(cli.to, Some(When::Ago(Duration::from_hours(24))));
    }

    #[test]
    fn bench_table() {
        let cli = ok(&["bench"]);
        assert_eq!(cli.mode, Mode::Bench);
        assert_eq!(
            (cli.duration, cli.bench_light, cli.report_json),
            (None, false, false)
        );
        let cli = ok(&["bench", "--duration", "90s", "--light", "--json"]);
        assert_eq!(cli.mode, Mode::Bench);
        assert_eq!(cli.duration, Some(Duration::from_secs(90)));
        assert!(cli.bench_light && cli.report_json);
        // `--json` before the command shapes the bench, as with report.
        let cli = ok(&["--json", "bench", "--duration=2m"]);
        assert_eq!(cli.mode, Mode::Bench);
        assert!(cli.report_json);
        assert_eq!(cli.duration, Some(Duration::from_secs(120)));
        assert_eq!(ok(&["bench", "--demo"]).mode, Mode::Bench);
    }

    #[test]
    fn bench_options_stay_with_bench() {
        let table: &[(&[&str], &str)] = &[
            (
                &["--light"],
                "--light cannot be combined with anything but bench",
            ),
            (
                &["record", "--light"],
                "--light cannot be combined with anything but bench",
            ),
            (&["bench", "record"], "bench cannot be combined with record"),
            (
                &["bench", "--json-stream"],
                "bench cannot be combined with --json-stream",
            ),
            (
                &["bench", "--duration", "0"],
                "--duration \"0\": must be at least 1s",
            ),
        ];
        for (args, message) in table {
            assert_eq!(err(args).to_string(), *message, "{args:?}");
        }
    }

    #[test]
    fn command_options_belong_to_their_command() {
        let table: &[(&[&str], &str)] = &[
            (&["replay"], "replay needs a value"),
            (&["export"], "export needs --csv"),
            (
                &["--out", "x"],
                "--out cannot be combined with anything but record",
            ),
            (
                &["serve", "--duration", "1m"],
                "--duration cannot be combined with anything but record or bench",
            ),
            (
                &["record", "--addr", "127.0.0.1:1"],
                "--addr cannot be combined with anything but serve",
            ),
            (
                &["--csv"],
                "--csv cannot be combined with anything but export",
            ),
            (
                &["record", "--from", "1d"],
                "--from cannot be combined with anything but export",
            ),
            (
                &["replay", "t.ptrace", "--demo"],
                "--demo cannot be combined with replay",
            ),
            (
                &["export", "--csv", "--demo"],
                "--demo cannot be combined with export",
            ),
            (&["record", "serve"], "record cannot be combined with serve"),
            (
                &["serve", "--addr", "localhost"],
                "--addr \"localhost\": expected HOST:PORT, e.g. 127.0.0.1:9101",
            ),
            (
                &["record", "--duration", "0s"],
                "--duration \"0s\": must be at least 1s",
            ),
            (
                &["export", "--csv", "--from", "yesterday"],
                "--from \"yesterday\": expected a date (2026-09-23) or a span (24h, 7d)",
            ),
        ];
        for (args, message) in table {
            assert_eq!(err(args).to_string(), *message, "{args:?}");
        }
    }

    #[test]
    fn report_subcommand_errors() {
        assert!(matches!(
            err(&["report", "year"]),
            CliError::BadValue { flag: "report", .. }
        ));
        assert_eq!(
            err(&["--demo", "report"]),
            CliError::Conflict("--demo", "report")
        );
        assert_eq!(
            err(&["doctor", "report"]),
            CliError::Conflict("doctor", "report")
        );
    }

    #[test]
    fn span_table() {
        let table: &[(&str, Option<u64>)] = &[
            ("90", Some(90)),
            ("90s", Some(90)),
            ("10m", Some(600)),
            ("2h", Some(7200)),
            ("7d", Some(604_800)),
            (" 5m ", Some(300)),
            ("0", None),
            ("1w", None),
            ("m", None),
            ("-1m", None),
            ("1.5h", None),
            ("99999999999999999999d", None),
            ("", None),
        ];
        for (text, want) in table {
            assert_eq!(
                parse_span(text).ok().map(|d| d.as_secs()),
                *want,
                "{text:?}"
            );
        }
    }

    #[test]
    fn when_resolves_days_to_whole_utc_days() {
        let day = When::Day(Day::from_ymd(1970, 1, 2).unwrap());
        assert_eq!(day.resolve(1_000_000, false), 86_400);
        assert_eq!(day.resolve(1_000_000, true), 172_800);
        assert_eq!(When::Ago(Duration::from_secs(60)).resolve(1000, false), 940);
        assert_eq!(When::Ago(Duration::from_mins(100)).resolve(1000, true), 0);
    }

    #[test]
    fn no_args_is_the_tui_with_defaults() {
        assert_eq!(ok(&[]), Cli::default());
    }

    #[test]
    fn mode_table() {
        let table: &[(&[&str], Mode)] = &[
            (&["-h"], Mode::Help),
            (&["--help"], Mode::Help),
            (&["-V"], Mode::Version),
            (&["--version"], Mode::Version),
            (&["--json"], Mode::Json),
            (&["--json-stream"], Mode::JsonStream),
            (&["--json-stream", "--count", "3"], Mode::JsonStream),
            (&["--diagnose"], Mode::Diagnose),
            (&["doctor"], Mode::Doctor),
            (&["--print-config"], Mode::PrintConfig),
            (&["--no-mouse"], Mode::Tui),
            (&["--json", "--json"], Mode::Json),
            // Help and version win even next to other modes or bad combos.
            (&["--json", "--help"], Mode::Help),
            (&["doctor", "-V"], Mode::Version),
            (&["-V", "-h"], Mode::Help),
        ];
        for (args, mode) in table {
            assert_eq!(ok(args).mode, *mode, "{args:?}");
        }
    }

    #[test]
    fn value_flags_table() {
        let cli = ok(&[
            "-i",
            "500",
            "--theme",
            "Tokyo Night",
            "--surface",
            "glance",
            "--no-mouse",
            "--config",
            "/tmp/p.toml",
        ]);
        assert_eq!(cli.interval, Some(Duration::from_millis(500)));
        assert_eq!(cli.theme.as_deref(), Some("tokyo-night"));
        assert_eq!(cli.surface, Some(Surface::Glance));
        assert!(cli.no_mouse);
        assert_eq!(cli.config, Some(PathBuf::from("/tmp/p.toml")));
    }

    #[test]
    fn equals_form_matches_space_form() {
        assert_eq!(ok(&["--interval=1s"]), ok(&["--interval", "1s"]));
        assert_eq!(ok(&["--theme=paper"]), ok(&["--theme", "paper"]));
        assert_eq!(
            ok(&["--json-stream", "--count=2"]),
            ok(&["--json-stream", "--count", "2"])
        );
        assert_eq!(ok(&["--json", "--procs=0"]).procs, 0);
    }

    #[test]
    fn interval_table() {
        let table: &[(&str, Option<u64>)] = &[
            ("250", Some(250)),
            ("250ms", Some(250)),
            ("1s", Some(1000)),
            ("1.5s", Some(1500)),
            ("0.25s", Some(250)),
            (" 500 ", Some(500)),
            ("100", Some(100)),
            ("60s", Some(60_000)),
            ("60000", Some(60_000)),
            ("99", None),
            ("0", None),
            ("-5", None),
            ("61s", None),
            ("1m", None),
            ("", None),
            ("ms", None),
            ("NaN", None),
            ("inf", None),
            ("1e400", None),
            ("½s", None),
            ("18446744073709551616", None),
        ];
        for (text, want) in table {
            let got = parse_interval(text).ok().map(|d| d.as_millis() as u64);
            assert_eq!(got, *want, "{text:?}");
        }
    }

    #[test]
    fn interval_sweep_accepts_exactly_the_range() {
        for ms in 0..=70_000_u64 {
            if ms % 7 != 0 && ms > 1000 {
                continue;
            }
            let got = parse_interval(&ms.to_string());
            let in_range = (100..=60_000).contains(&ms);
            assert_eq!(got.is_ok(), in_range, "{ms}");
            if let Ok(d) = got {
                assert_eq!(d.as_millis(), u128::from(ms));
            }
        }
    }

    #[test]
    fn error_table() {
        let table: &[(&[&str], &str)] = &[
            (&["--bogus"], "unknown argument \"--bogus\""),
            (&["-x"], "unknown argument \"-x\""),
            (&["top"], "unknown argument \"top\""),
            (&["--no-mouse=yes"], "unknown argument \"--no-mouse=yes\""),
            (&["--json=1"], "unknown argument \"--json=1\""),
            (&["-i"], "--interval needs a value"),
            (&["--theme"], "--theme needs a value"),
            (&["--config"], "--config needs a value"),
            (&["--count"], "--count needs a value"),
            (&["--procs"], "--procs needs a value"),
            (&["--surface"], "--surface needs a value"),
            (
                &["--surface", "wide"],
                "--surface \"wide\": expected work or glance",
            ),
            (
                &["--count", "0", "--json-stream"],
                "--count \"0\": must be at least 1",
            ),
            (&["--count", "x"], "--count \"x\": expected a whole number"),
            (
                &["--procs", "-1"],
                "--procs \"-1\": expected a whole number",
            ),
            (&["--config="], "--config \"\": expected a path"),
            (
                &["--json", "--json-stream"],
                "--json cannot be combined with --json-stream",
            ),
            (
                &["doctor", "--diagnose"],
                "doctor cannot be combined with --diagnose",
            ),
            (
                &["--json", "--count", "3"],
                "--count cannot be combined with anything but --json-stream",
            ),
            (&["--demo=1"], "unknown argument \"--demo=1\""),
            (
                &["--demo", "doctor"],
                "--demo cannot be combined with doctor",
            ),
            (
                &["agent", "status", "--demo"],
                "--demo cannot be combined with agent",
            ),
        ];
        for (args, message) in table {
            assert_eq!(err(args).to_string(), *message, "{args:?}");
        }
    }

    #[test]
    fn unknown_theme_lists_the_choices() {
        let message = err(&["--theme", "neon"]).to_string();
        assert!(message.starts_with("--theme \"neon\": unknown theme; try auto, "));
        for name in Theme::names() {
            assert!(message.contains(name), "{name} missing from {message}");
        }
    }

    #[test]
    fn demo_flag_table() {
        let table: &[(&[&str], Mode)] = &[
            (&["--demo"], Mode::Tui),
            (&["--demo", "--json"], Mode::Json),
            (
                &["--json-stream", "--demo", "--count", "2"],
                Mode::JsonStream,
            ),
            (&["--demo", "--diagnose"], Mode::Diagnose),
            (&["--demo", "--surface", "glance"], Mode::Tui),
            (&["--demo", "--print-config"], Mode::PrintConfig),
            (&["--demo", "--demo"], Mode::Tui),
            (&["--demo", "-V"], Mode::Version),
            (&["--demo", "doctor", "-h"], Mode::Help),
        ];
        for (args, mode) in table {
            let cli = ok(args);
            assert_eq!(cli.mode, *mode, "{args:?}");
            assert!(cli.demo, "{args:?}");
        }
        assert!(!ok(&[]).demo, "live by default");
        assert!(!ok(&["--json"]).demo);
    }

    #[test]
    fn demo_is_not_a_mode_so_it_keeps_other_flags() {
        let cli = ok(&["--demo", "-i", "250", "--theme", "paper", "--no-mouse"]);
        assert!(cli.demo && cli.no_mouse);
        assert_eq!(cli.interval, Some(Duration::from_millis(250)));
        assert_eq!(cli.theme.as_deref(), Some("paper"));
    }

    #[test]
    fn auto_theme_is_none() {
        assert_eq!(ok(&["--theme", "auto"]).theme, None);
        assert_eq!(ok(&["--theme", "AUTO"]).theme, None);
    }

    #[test]
    fn every_builtin_theme_round_trips() {
        for name in Theme::builtins() {
            assert_eq!(ok(&["--theme", name]).theme.as_deref(), Some(name));
        }
    }

    #[test]
    fn value_that_looks_like_a_flag_is_still_the_value() {
        // `--config --json` means a file literally named "--json".
        let cli = ok(&["--config", "--json"]);
        assert_eq!(cli.mode, Mode::Tui);
        assert_eq!(cli.config, Some(PathBuf::from("--json")));
    }

    #[test]
    fn unicode_and_huge_values_are_rejected_cleanly() {
        assert!(matches!(err(&["--procs", "五"]), CliError::BadValue { .. }));
        assert!(matches!(
            err(&["--count", "99999999999999999999999", "--json-stream"]),
            CliError::BadValue { .. }
        ));
        assert!(matches!(err(&["--ünknown"]), CliError::Unknown(_)));
        let path = ok(&["--config", "/tmp/π/配置.toml"]).config;
        assert_eq!(path, Some(PathBuf::from("/tmp/π/配置.toml")));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_argument_is_an_error() {
        use std::os::unix::ffi::OsStringExt;
        let bad = OsString::from_vec(vec![0x2d, 0x2d, 0xff]);
        assert_eq!(parse([bad]), Err(CliError::NotUtf8));
    }

    #[test]
    fn last_value_wins_for_repeated_flags() {
        let cli = ok(&["-i", "1s", "-i", "250"]);
        assert_eq!(cli.interval, Some(Duration::from_millis(250)));
    }

    #[test]
    fn headless_interval_defaults_to_one_second() {
        assert_eq!(ok(&["--json"]).headless_interval(), Duration::from_secs(1));
        assert_eq!(
            ok(&["--json", "-i", "300"]).headless_interval(),
            Duration::from_millis(300)
        );
    }

    #[test]
    fn usage_mentions_every_flag() {
        // The 1.0 flags, pinned by name so a table edit cannot drop one.
        let usage = usage();
        for flag in [
            "--interval",
            "--theme",
            "--surface",
            "--no-mouse",
            "--config",
            "--print-config",
            "--json",
            "--json-stream",
            "--count",
            "--procs",
            "--diagnose",
            "--demo",
            "--help",
            "--version",
            "doctor",
            "record",
            "replay",
            "serve",
            "export",
            "--out",
            "--duration",
            "--addr",
            "--csv",
            "--from",
            "--to",
            "bench",
            "--light",
        ] {
            assert!(usage.contains(flag), "{flag}");
        }
        for spec in FLAGS {
            assert!(usage.contains(&spec.synopsis()), "{}", spec.long);
            assert!(usage.contains(spec.help), "{}", spec.long);
        }
        for command in COMMANDS {
            assert!(usage.contains(&format!("siltop {}", command.name)));
        }
        assert!(!usage.contains('\x1b'));
    }

    /// Regression: `completions <zsh|bash|fish>` once ran straight into its
    /// help text because the head was wider than the column.
    #[test]
    fn help_rows_never_run_head_into_help() {
        let mut out = String::new();
        help_row(&mut out, &"x".repeat(HELP_COL), "help");
        assert_eq!(
            out,
            format!(
                "  {}\n  {}help\n",
                "x".repeat(HELP_COL),
                " ".repeat(HELP_COL)
            )
        );
        let usage = usage();
        for (word, arg, help) in COMMANDS.iter().flat_map(|c| c.subs.iter()) {
            let _ = (word, arg);
            assert!(usage.contains(&format!("  {help}")), "{help}");
        }
        for spec in FLAGS {
            assert!(usage.contains(&format!("  {}", spec.help)), "{}", spec.long);
        }
        for command in COMMANDS.iter().filter(|c| c.subs.is_empty()) {
            assert!(
                usage.contains(&format!("  {}", command.help)),
                "{}",
                command.name
            );
        }
    }

    /// macOS `man` has no `-l`, so `siltop man | man -l -` failed with
    /// "illegal option" on the only platform siltop supports.
    #[test]
    fn man_example_works_with_macos_man_tools() {
        let usage = usage();
        assert!(usage.contains("siltop man | mandoc -a"), "{usage}");
        assert!(!usage.contains("man -l"), "{usage}");
    }

    #[test]
    fn usage_is_grouped_with_examples_and_fits_80_columns() {
        let usage = usage();
        let mut at = 0;
        let titles = |sections: &[Section]| sections.iter().map(|s| s.title()).collect::<Vec<_>>();
        for heading in titles(&[Section::Dashboard, Section::Scripting, Section::Info])
            .into_iter()
            .chain(["Commands"])
            .chain(titles(&[Section::Command]))
            .chain(["Examples", "Exit status"])
        {
            let found = usage[at..]
                .find(&format!("\n{heading}:\n"))
                .unwrap_or_else(|| panic!("{heading} missing or out of order"));
            at += found + 1;
        }
        for line in usage.lines() {
            assert!(line.chars().count() <= 80, "{line:?}");
        }
        // Every example is itself a valid command line.
        for (line, _) in EXAMPLES {
            let args: Vec<&str> = line
                .split(" > ")
                .next()
                .unwrap()
                .split(" | ")
                .next()
                .unwrap()
                .split_whitespace()
                .skip(1)
                .collect();
            assert!(parse(args.iter().copied()).is_ok(), "{line}");
        }
    }

    #[test]
    fn flag_table_has_no_duplicate_names() {
        let mut seen = std::collections::HashSet::new();
        for spec in FLAGS {
            assert!(seen.insert(spec.long.to_owned()), "{}", spec.long);
            if let Some(c) = spec.short {
                assert!(seen.insert(format!("-{c}")), "-{c}");
            }
            assert!(spec.long.starts_with("--"));
        }
        for command in COMMANDS {
            assert!(seen.insert(command.name.to_owned()), "{}", command.name);
        }
    }

    #[test]
    fn version_json_is_one_object_either_way_round() {
        for args in [
            &["--version", "--json"][..],
            &["--json", "-V"],
            &["-V", "--json", "--verbose"],
        ] {
            let cli = ok(args);
            assert_eq!(cli.mode, Mode::Version, "{args:?}");
            assert!(cli.version_json, "{args:?}");
        }
        assert!(!ok(&["-V"]).version_json);
        let text = json_version(&[("commit", String::from("abc"))]);
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["name"], "siltop");
        assert_eq!(value["version"], VERSION);
        assert_eq!(value["commit"], "abc");
        assert!(text.ends_with("}\n"));
    }

    #[test]
    fn version_verbose_table() {
        assert_eq!(ok(&["--version", "--verbose"]).mode, Mode::Version);
        assert!(ok(&["--verbose", "-V"]).verbose);
        assert_eq!(
            err(&["--verbose"]),
            CliError::Conflict("--verbose", "anything but --version")
        );
        assert_eq!(
            err(&["--verbose=1", "-V"]),
            CliError::Unknown(String::from("--verbose=1"))
        );
    }

    #[test]
    fn man_and_completions_are_modes() {
        assert_eq!(ok(&["man"]).mode, Mode::Man);
        assert_eq!(
            ok(&["completions", "FISH"]).mode,
            Mode::Completions(Shell::Fish)
        );
        assert_eq!(err(&["man", "doctor"]), CliError::Conflict("man", "doctor"));
        assert_eq!(err(&["man=1"]), CliError::Unknown(String::from("man=1")));
    }

    #[test]
    fn verbose_version_lists_rows_aligned() {
        let text = verbose_version(&[
            ("commit", String::from("abc")),
            ("state", String::from("/s")),
        ]);
        assert_eq!(
            text,
            format!("siltop {VERSION}\ncommit   abc\nstate    /s\n")
        );
    }

    #[test]
    fn build_facts_leave_out_what_is_unknown() {
        for (key, value) in build_facts() {
            assert!(["commit", "built"].contains(&key));
            assert!(!value.is_empty());
        }
    }

    /// The storefront files ship with every release; keep them honest.
    fn repo_file(path: &str) -> String {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        std::fs::read_to_string(root.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    #[test]
    fn docs_document_every_usage_flag() {
        let docs = repo_file("README.md") + &repo_file("docs/guide.md");
        for flag in usage()
            .split_whitespace()
            .filter(|w| w.starts_with("--"))
            .map(|w| w.trim_end_matches(','))
        {
            assert!(docs.contains(flag), "README and guide are missing {flag}");
        }
    }

    #[test]
    fn guide_covers_every_command_and_topic_and_readme_links_it() {
        let guide = repo_file("docs/guide.md");
        for command in COMMANDS {
            let needle = format!("siltop {}", command.name);
            assert!(guide.contains(&needle), "{needle}");
        }
        for heading in [
            "## Getting started",
            "## Reading the dashboard",
            "## Keys",
            "## Settings",
            "## Presets",
            "## Themes",
            "## Scripts and JSON",
            "## Doctor",
            "## Troubleshooting",
            "## Privacy",
        ] {
            assert!(guide.contains(heading), "{heading}");
        }
        for preset in siltop_core::Preset::ALL {
            assert!(guide.contains(preset.label()), "{}", preset.label());
        }
        // The guide opens with the same three keys the first-run hint names.
        for key in ["**`m`**", "**`?`**", "**`T`**"] {
            assert!(guide.contains(key), "{key}");
        }
        assert!(repo_file("README.md").contains("(docs/guide.md)"));
    }

    #[test]
    fn changelog_names_every_command() {
        let log = repo_file("CHANGELOG.md");
        for command in COMMANDS {
            let spelled = format!("siltop {}", command.name);
            let quoted = format!("`{}`", command.name);
            assert!(
                log.contains(&spelled) || log.contains(&quoted),
                "CHANGELOG is missing {}",
                command.name
            );
        }
    }

    #[test]
    fn readme_images_exist_and_stay_small() {
        let readme = repo_file("README.md");
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let images: Vec<&str> = readme
            .split("docs/media/")
            .skip(1)
            .filter_map(|rest| rest.split(['"', ')', '`']).next())
            .filter(|name| {
                std::path::Path::new(name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
            })
            .collect();
        assert!(
            images.len() >= 3,
            "hero, glance and light shots: {images:?}"
        );
        for name in images {
            let meta = std::fs::metadata(root.join("docs/media").join(name))
                .unwrap_or_else(|e| panic!("docs/media/{name}: {e}"));
            assert!(meta.len() <= 400 * 1024, "{name} is {} bytes", meta.len());
        }
    }

    #[test]
    fn readme_keeps_the_privacy_promises() {
        let readme = repo_file("README.md");
        for promise in ["No network of its own", "No telemetry", "No sudo"] {
            assert!(readme.contains(promise), "{promise}");
        }
    }

    #[test]
    fn changelog_starts_with_unreleased() {
        let log = repo_file("CHANGELOG.md");
        let first = log.lines().find(|l| l.starts_with("## ")).unwrap_or("");
        assert_eq!(first, "## [Unreleased]");
    }

    #[test]
    fn homebrew_formula_is_a_prebuilt_arm64_macos_binary() {
        let formula = repo_file("packaging/homebrew/siltop.rb");
        for needle in [
            "class Siltop < Formula",
            "depends_on :macos",
            "depends_on arch: :arm64",
            "bin.install \"siltop\"",
            "sha256 \"@SHA256@\"",
            "siltop --version",
        ] {
            assert!(formula.contains(needle), "{needle}");
        }
        // A prebuilt binary: no Rust toolchain on the user's Mac.
        for stale in ["depends_on \"rust\"", "cargo"] {
            assert!(!formula.contains(stale), "{stale}");
        }
    }

    /// Every `@NAME@` in the formula template is filled by `package.sh formula`.
    #[test]
    fn homebrew_formula_placeholders_are_all_rendered() {
        let formula = repo_file("packaging/homebrew/siltop.rb");
        let script = repo_file("scripts/package.sh");
        let body = &formula[formula.find("class ").unwrap_or(0)..];
        let placeholders: Vec<&str> = body.split('@').skip(1).step_by(2).collect();
        assert!(!placeholders.is_empty());
        for name in placeholders {
            assert!(
                script.contains(&format!("s|@{name}@|")),
                "package.sh does not render @{name}@"
            );
        }
    }

    /// Formula url, packaging script and README agree on the asset names.
    #[test]
    fn release_asset_names_agree_everywhere() {
        let formula = repo_file("packaging/homebrew/siltop.rb");
        let script = repo_file("scripts/package.sh");
        let readme = repo_file("README.md");
        assert!(formula.contains("/releases/download/@TAG@/siltop-@TAG@-macos-arm64.tar.gz"));
        assert!(script.contains("name=\"siltop-${tag}-macos-arm64\""));
        assert!(readme.contains("siltop-<tag>-macos-arm64.zip"));
    }

    /// CI packages through the same script a local dry run uses.
    #[test]
    fn release_workflow_packages_with_package_sh() {
        let workflow = repo_file(".github/workflows/release.yml");
        assert!(workflow.contains("run: scripts/package.sh \"${GITHUB_REF_NAME}\" --no-build"));
        assert!(workflow.contains("tags: [\"v*\"]"));
    }

    /// Optional release steps are no-ops without their secrets.
    #[test]
    fn release_workflow_optional_steps_are_gated_on_secrets() {
        let workflow = repo_file(".github/workflows/release.yml");
        for gate in [
            "if: env.HAVE_CODESIGN == 'true'",
            "steps.kind.outputs.prerelease == 'false' && env.HAVE_TAP == 'true' }}",
        ] {
            assert!(workflow.contains(gate), "{gate}");
        }
    }

    #[test]
    fn json_after_a_command_without_json_says_what_json_is_for() {
        // Regression: `theme --json` said only "theme cannot be
        // combined with --json", leaving the user to guess why.
        for args in [
            &["theme", "--json"][..],
            &["--json", "theme"],
            &["theme", "list", "--json"],
            &["line", "--json"],
            &["doctor", "--json"],
            &["record", "--json"],
        ] {
            let error = err(args);
            assert!(matches!(error, CliError::NoJson(_)), "{args:?}: {error:?}");
            let text = error.to_string();
            assert!(text.contains("has no JSON output"), "{text}");
            assert!(text.contains("report takes it"), "{text}");
        }
        assert_eq!(err(&["--json", "doctor"]), CliError::NoJson("doctor"));
        // Two flag modes still read as a plain conflict.
        assert_eq!(
            err(&["--json", "--diagnose"]),
            CliError::Conflict("--json", "--diagnose")
        );
    }

    #[test]
    fn sub_word_after_an_option_points_back_at_its_command() {
        // Regression: `report --json week` said `unknown argument "week"`.
        let table: &[(&[&str], &str, &str)] = &[
            (&["report", "--json", "week"], "report", "week"),
            (
                &["agent", "--config", "c.toml", "install"],
                "agent",
                "install",
            ),
            (&["theme", "--theme", "auto", "list"], "theme", "list"),
        ];
        for (args, command, word) in table {
            let error = err(args);
            assert_eq!(
                error,
                CliError::Misplaced {
                    command,
                    word: (*word).to_owned(),
                },
                "{args:?}"
            );
            assert_eq!(
                error.to_string(),
                format!("{word:?} goes right after {command}: siltop {command} {word} [OPTIONS]")
            );
        }
        // A word that is no sub-word of the command stays plainly unknown.
        assert_eq!(
            err(&["report", "week", "hourly"]),
            CliError::Unknown(String::from("hourly"))
        );
        assert_eq!(
            err(&["doctor", "extra"]),
            CliError::Unknown(String::from("extra"))
        );
    }

    #[test]
    fn json_stream_and_diagnose_with_a_command_stay_plain_conflicts() {
        // Only --json has a command that takes it (report); the hint would
        // mislead for the other headless modes.
        assert_eq!(
            err(&["theme", "--json-stream"]),
            CliError::Conflict("theme", "--json-stream")
        );
        assert_eq!(
            err(&["--diagnose", "doctor"]),
            CliError::Conflict("--diagnose", "doctor")
        );
        assert_eq!(
            err(&["--json", "replay", "t.ptrace"]),
            CliError::NoJson("replay")
        );
    }

    #[test]
    fn export_without_csv_names_the_missing_flag_not_a_missing_value() {
        // Regression: `export` alone said "export --csv needs a value", but
        // --csv takes no value.
        assert_eq!(err(&["export"]), CliError::NeedsFlag("export", "--csv"));
        assert_eq!(err(&["export"]).to_string(), "export needs --csv");
        assert_eq!(ok(&["export", "--csv"]).mode, Mode::Export);
    }

    /// Every command line a row of `--help` stands for, with a file where
    /// the row takes one.
    fn help_rows() -> Vec<(Vec<&'static str>, &'static str)> {
        let mut rows = Vec::new();
        for command in COMMANDS {
            let fill = |arg: &str| match arg {
                "<FILE>" => Some("f.toml"),
                "<SHELL>" => Some("zsh"),
                "<HOST>..." => Some("studio"),
                _ => None,
            };
            if command.subs.is_empty() {
                let mut args = vec![command.name];
                args.extend(fill(command.args));
                if command.name == "export" {
                    args.push("--csv");
                }
                rows.push((args, command.help));
            }
            for (word, arg, help) in command.subs {
                let mut args = vec![command.name, *word];
                args.extend(fill(arg));
                rows.push((args, *help));
            }
        }
        rows
    }

    #[test]
    fn every_help_row_parses_as_the_command_it_names() {
        // --help, the man page and completions all read COMMANDS, so a row
        // that parses here parses everywhere.
        let rows = help_rows();
        assert!(rows.len() >= COMMANDS.len());
        for (args, help) in rows {
            assert_eq!(ok(&args).mode.name(), args[0], "{args:?}: {help}");
        }
    }

    #[test]
    fn usage_lists_every_exit_status() {
        let usage = usage();
        let section = usage.split("\nExit status:\n").nth(1).unwrap();
        for (code, what) in EXIT_CODES {
            assert!(
                section
                    .lines()
                    .any(|l| l.trim_start().starts_with(&code.to_string()) && l.contains(what)),
                "{code} {what}"
            );
        }
        let codes: Vec<u8> = EXIT_CODES.iter().map(|(code, _)| *code).collect();
        assert_eq!(codes, [0, EXIT_ERROR, EXIT_USAGE, 10]);
    }

    #[test]
    fn version_line_starts_with_name_and_version() {
        let line = version_line();
        assert!(line.starts_with(&format!("siltop {VERSION}")));
        assert!(!line.contains('\n'));
    }
}
