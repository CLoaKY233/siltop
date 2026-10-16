mod agent;
mod app;
mod bench;
mod cli;
mod completions;
mod config_io;
mod crash;
mod demo;
mod doctor;
mod event;
mod export;
mod feedback;
mod first_run;
mod fleet;
mod headless;
mod keymap;
mod line;
mod man;
mod msg;
mod onboarding;
mod osc;
mod record;
mod replay;
mod report;
mod reset;
mod serve;
mod signals;
mod sinks;
mod source;
mod store_disk;
mod term_caps;
mod themes_io;
mod tui;
mod update;
mod worker;

use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

use siltop_core::Config;

use crate::cli::{Cli, CliError, EXIT_ERROR, EXIT_USAGE, Mode};

const NO_TTY_HINT: &str =
    "siltop: the dashboard needs a terminal on stdin and stdout; try --json, --diagnose or --help";
const DUMB_TERM_HINT: &str = "siltop: TERM=dumb cannot draw the dashboard; try --json, line or --diagnose, or run it in a full terminal";
/// The line after a usage error. The error stays on screen instead of
/// scrolling away under the whole help text.
const USAGE_HINT: &str = "try 'siltop --help' for every command and option";

fn main() -> ExitCode {
    // Before parsing: `--theme` may name one of the user's own themes.
    themes_io::install();
    let parsed = cli::parse(std::env::args_os().skip(1));
    let tty = Tty {
        stdin: io::stdin().is_terminal(),
        stdout: io::stdout().is_terminal(),
    };
    let mut out = io::stdout();
    let mut err = io::stderr();
    ExitCode::from(dispatch(parsed, tty, &mut out, &mut err))
}

#[derive(Debug, Clone, Copy)]
struct Tty {
    stdin: bool,
    stdout: bool,
}

/// Preferences from `--config` (or the default path), with CLI overrides.
pub(crate) fn cli_config(cli: &Cli) -> Config {
    let path = cli.config.clone().or_else(config_io::default_path);
    let config = path.map_or_else(Config::default, |path| Config::load(&path));
    with_cli_overrides(config, cli)
}

/// Session-only overrides from the command line.
pub(crate) fn with_cli_overrides(mut config: Config, cli: &Cli) -> Config {
    if let Some(interval) = cli.interval {
        config.interval = interval;
    }
    if let Some(surface) = cli.surface {
        config.surface = Some(surface);
    }
    if let Some(theme) = &cli.theme {
        config.theme.clone_from(theme);
    }
    config
}

/// The effective preferences as a whole v1 file: every key, the same layout
/// the app saves, so it can be pasted into `config.toml` as it is. `--no-mouse`
/// is a session flag with no key, so it shows as a comment.
fn config_text(config: &Config, cli: &Cli) -> io::Result<String> {
    let text = config.to_toml().map_err(io::Error::other)?;
    Ok(if cli.no_mouse {
        format!("# mouse is off for this session (--no-mouse); it has no config key.\n{text}")
    } else {
        text
    })
}

/// The config file as a command reads it, before command-line flags.
#[derive(Debug, Default)]
struct FileConfig {
    config: Config,
    /// One line per value or key the loader ignored.
    warnings: Vec<String>,
    /// `interval_ms` when the file sets it: the headless modes keep their
    /// own one-second default otherwise.
    interval: Option<std::time::Duration>,
}

/// A file error in plain words, without the `(os error N)` tail.
pub(crate) fn io_reason(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::NotFound => String::from("no such file"),
        io::ErrorKind::PermissionDenied => String::from("permission denied"),
        io::ErrorKind::IsADirectory => String::from("is a directory, not a file"),
        io::ErrorKind::InvalidData => String::from("not a text file"),
        _ => {
            let text = error.to_string();
            match text.split_once(" (os error") {
                Some((reason, _)) => reason.to_owned(),
                None => text,
            }
        }
    }
}

/// Reads `--config`, or the default path. A missing default file is the
/// defaults; a file named with `--config` must exist. `Err` is the one line
/// to print, naming the file.
fn read_config(cli: &Cli) -> Result<FileConfig, String> {
    let Some(path) = cli.config.clone().or_else(config_io::default_path) else {
        return Ok(FileConfig::default());
    };
    let shown = siltop_core::into_printable(path.display().to_string());
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound && cli.config.is_none() => {
            return Ok(FileConfig::default());
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(format!(
                "{shown}: no such file (run without --config to use the default)"
            ));
        }
        Err(e) => return Err(format!("{shown}: {}", io_reason(&e))),
    };
    let loaded = Config::from_toml(&text).map_err(|e| {
        let message = siltop_core::into_printable(e.to_string());
        format!("{shown}: not valid TOML: {}", message.trim())
    })?;
    let interval = loaded
        .doc
        .get("interval_ms")
        .is_some()
        .then_some(loaded.config.interval)
        .filter(|_| {
            !loaded
                .warnings
                .iter()
                .any(|w| w.starts_with("ignored interval_ms"))
        });
    let warnings = loaded
        .warnings
        .iter()
        .map(|warning| format!("{shown}: {}", siltop_core::printable(warning)))
        .collect();
    Ok(FileConfig {
        config: loaded.config,
        warnings,
        interval,
    })
}

/// Commands that read the config file: a file named with `--config` that
/// cannot be read stops them with one line, the same way everywhere.
const fn reads_config(mode: Mode) -> bool {
    matches!(
        mode,
        Mode::Tui
            | Mode::Replay
            | Mode::Json
            | Mode::JsonStream
            | Mode::Diagnose
            | Mode::PrintConfig
            | Mode::Line
            | Mode::Fleet
            | Mode::Bench
    )
}

fn warn_all(err: &mut dyn Write, warnings: &[String]) {
    for warning in warnings {
        let _ = writeln!(err, "siltop: {warning}");
    }
}

/// `--print-config`: [`config_text`] of the file after flags. Ignored values
/// are named on stderr; a file that does not parse is an error, not a quiet
/// page of defaults (the app itself says it is using defaults then).
fn print_config(cli: &Cli, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let file = match read_config(cli) {
        Ok(file) => file,
        Err(message) => return report(err, &message),
    };
    warn_all(err, &file.warnings);
    match config_text(&with_cli_overrides(file.config, cli), cli)
        .and_then(|text| out.write_all(text.as_bytes()))
    {
        Ok(()) => 0,
        Err(e) => report(err, &e.to_string()),
    }
}

/// Build and paths: `--version --verbose` shows all of them; the settings
/// About page shows the ones it does not already have.
fn about_rows(cli: &Cli) -> Vec<(&'static str, String)> {
    let mut rows = cli::build_facts();
    let config = cli.config.clone().or_else(config_io::default_path);
    rows.push(("config", path_label(config.as_deref())));
    rows.push(state_row());
    rows
}

/// Where saved history lives, for the About page and `--version --verbose`.
pub(crate) fn state_row() -> (&'static str, String) {
    ("state", path_label(store_disk::default_root().as_deref()))
}

fn path_label(path: Option<&std::path::Path>) -> String {
    path.map_or_else(
        || String::from("none (no home directory)"),
        |p| p.display().to_string(),
    )
}

fn consent_command(cli: &Cli, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    match cli.mode {
        Mode::Onboarding => onboarding::command(cli.config.as_deref(), out, err),
        Mode::Update(action) => update::command(action, out, err),
        Mode::Feedback => feedback::command(cli.feedback_bug, cli.feedback_copy, out, err),
        Mode::Reset => reset::command(cli.reset_yes, out, err),
        _ => 0,
    }
}

/// Only the live dashboard asks: `--demo` is a showcase, so it neither
/// shows the screens nor writes their answers to the user's config.
const fn onboards(cli: &Cli) -> bool {
    matches!(cli.mode, Mode::Tui) && !cli.demo
}

fn maybe_onboard(cli: &Cli) -> io::Result<onboarding::End> {
    if !onboards(cli) {
        return Ok(onboarding::End::Skipped);
    }
    let path = cli.config.clone().or_else(config_io::default_path);
    onboarding::run(path.as_deref(), false, cli.demo)
}

fn report(err: &mut dyn Write, message: &str) -> u8 {
    let _ = writeln!(err, "siltop: {message}");
    EXIT_ERROR
}

/// Runs one parsed command line.
fn dispatch(
    parsed: Result<Cli, CliError>,
    tty: Tty,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let cli = match parsed {
        Ok(cli) => cli,
        Err(error) => {
            let _ = writeln!(err, "siltop: {error}\n{USAGE_HINT}");
            return EXIT_USAGE;
        }
    };
    if reads_config(cli.mode)
        && cli.mode != Mode::PrintConfig
        && cli.config.is_some()
        && let Err(message) = read_config(&cli)
    {
        return report(err, &message);
    }
    let written = match cli.mode {
        Mode::Help => out.write_all(cli::usage().as_bytes()),
        Mode::Version if cli.version_json => {
            let rows = if cli.verbose {
                about_rows(&cli)
            } else {
                cli::build_facts()
            };
            out.write_all(cli::json_version(&rows).as_bytes())
        }
        Mode::Version if cli.verbose => {
            let rows = about_rows(&cli);
            out.write_all(cli::verbose_version(&rows).as_bytes())
        }
        Mode::Completions(shell) => out.write_all(completions::script(shell).as_bytes()),
        Mode::Man => out.write_all(man::page().as_bytes()),
        Mode::Version => writeln!(out, "{}", cli::version_line()),
        Mode::PrintConfig => return print_config(&cli, out, err),
        Mode::Json | Mode::JsonStream | Mode::Diagnose => return headless(&cli, out, err),
        Mode::Theme => {
            let dir = themes_io::default_dir();
            let file = cli.file.as_deref();
            return themes_io::run(file, dir.as_deref(), out, err);
        }
        Mode::Report(span) => {
            let root = store_disk::default_root();
            let now = store_disk::unix_now();
            return match report::run(span, cli.report_json, root.as_deref(), now, out) {
                Ok(()) => 0,
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => 0,
                Err(error) => report(err, &error.to_string()),
            };
        }
        Mode::Agent(action) => return agent_command(&cli, action, out, err),
        Mode::Line => return line_command(&cli, out, err),
        Mode::Doctor => {
            return match doctor::run(out) {
                Ok(()) => 0,
                Err(error) => report(err, &error.to_string()),
            };
        }
        Mode::Record | Mode::Serve | Mode::Export => return capture_command(&cli, out, err),
        Mode::Onboarding | Mode::Update(_) | Mode::Feedback | Mode::Reset => {
            return consent_command(&cli, out, err);
        }
        Mode::Bench => return bench_command(&cli, tty.stdin && tty.stdout, out, err),
        Mode::Fleet => {
            if !(tty.stdin && tty.stdout) {
                let _ = writeln!(err, "{NO_TTY_HINT}");
                return EXIT_USAGE;
            }
            if let Err(error) = fleet::run(&cli) {
                tui::restore_best_effort();
                return report(err, &error.to_string());
            }
            return 0;
        }
        Mode::Tui | Mode::Replay => return dashboard(&cli, tty, dumb_term(), err),
    };
    match written.and_then(|()| out.flush()) {
        Ok(()) => 0,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => 0,
        Err(error) => report(err, &error.to_string()),
    }
}

/// `TERM=dumb`: a terminal that cannot move the cursor or draw colour, such
/// as an editor's shell buffer.
fn dumb_term() -> bool {
    std::env::var_os("TERM").is_some_and(|term| term == "dumb")
}

/// The dashboard and `replay`. Everything that can fail in one plain line
/// (the recording, the terminal) is checked before the terminal is probed
/// or switched to raw mode.
fn dashboard(cli: &Cli, tty: Tty, dumb: bool, err: &mut dyn Write) -> u8 {
    if cli.mode == Mode::Replay
        && let Some(path) = cli.file.as_deref()
        && let Err(error) = replay::check(path)
    {
        return report(err, &error.to_string());
    }
    if !(tty.stdin && tty.stdout) {
        let _ = writeln!(err, "{NO_TTY_HINT}");
        return EXIT_USAGE;
    }
    if dumb {
        let _ = writeln!(err, "{DUMB_TERM_HINT}");
        return EXIT_USAGE;
    }
    match maybe_onboard(cli) {
        Ok(onboarding::End::Quit) => return 0,
        Ok(_) => {}
        Err(error) => return report(err, &error.to_string()),
    }
    tui::install_panic_hook();
    if let Err(error) = app::run(cli) {
        tui::restore_best_effort();
        return report(err, &error.to_string());
    }
    0
}

/// The canned session with `--demo`, else this Mac's sampler, for every
/// mode that runs without a TUI.
fn sampler(
    demo: bool,
    interval: std::time::Duration,
) -> siltop_core::Result<Box<dyn worker::Tick>> {
    if demo {
        return Ok(Box::new(demo::Demo::new(interval)));
    }
    let sampler = siltop_metrics::Sampler::new()?;
    sampler.set_interval(interval);
    Ok(Box::new(sampler))
}

/// The theme `line --style ansi` colours with, else the default palette.
fn line_theme(name: &str) -> siltop_ui::Theme {
    siltop_ui::Theme::by_name(name).unwrap_or_default()
}

fn agent_command(
    cli: &Cli,
    action: cli::AgentAction,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    if action != cli::AgentAction::Run {
        return agent::manage(action, cli.config.as_deref(), out, err);
    }
    let mut sampler = match sampler(cli.demo, agent::AGENT_INTERVAL) {
        Ok(sampler) => sampler,
        Err(error) => return report(err, &error.to_string()),
    };
    let config = cli_config(cli);
    let state = line::state_path();
    // On battery the agent's SMC sweep and process table slow down too,
    // as the TUI worker's do.
    let mut tick = || {
        let snap = sampler.tick();
        if let Ok(snap) = &snap {
            sampler.set_low_power(siltop_metrics::on_battery(snap));
        }
        snap
    };
    let never = || false; // launchd's SIGTERM ends the process
    let mut pacer = worker::Pacer::new(&never);
    let mut wait = |period| pacer.wait(period);
    match agent::run_loop(
        &mut tick,
        &mut wait,
        &config,
        store_disk::default_root().as_deref(),
        state.as_deref(),
        None,
    ) {
        Ok(()) => 0,
        Err(error) => report(err, &error.to_string()),
    }
}

/// How long `line` waits between its priming tick and the one it prints:
/// long enough for a rate, short enough for a status bar.
const LINE_SAMPLE: std::time::Duration = std::time::Duration::from_millis(250);

fn line_command(cli: &Cli, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let config = cli_config(cli);
    let palette = line::Palette::from_theme(&line_theme(&config.theme));
    let format = cli.line_format.as_deref().unwrap_or(line::DEFAULT_FORMAT);
    // The demo never reads the real agent's file: its numbers are canned.
    let state = line::state_path().filter(|_| !cli.demo);
    let sample = || {
        let mut sampler = sampler(cli.demo, LINE_SAMPLE)?;
        let _prime = sampler.tick()?;
        std::thread::sleep(LINE_SAMPLE);
        let snap = sampler.tick()?;
        let verdict = siltop_core::assess(&snap, &siltop_core::Signals::default());
        Ok(line::Summary::from_snapshot(
            &snap,
            &verdict,
            headless::unix_ms_now(),
        ))
    };
    let result = line::run(
        out,
        format,
        cli.line_style,
        &palette,
        state.as_deref(),
        headless::unix_ms_now(),
        sample,
    );
    match result {
        Ok(()) => 0,
        Err(error) => report(err, &error.to_string()),
    }
}

/// `record`, `serve` and `export`.
fn capture_command(cli: &Cli, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let result = match cli.mode {
        Mode::Record => run_record(cli, out),
        Mode::Serve => run_serve(cli, err),
        _ => run_export(cli, out),
    };
    match result {
        Ok(()) => 0,
        Err(error) => report(err, &error.to_string()),
    }
}

/// `bench`: the live page only on a terminal.
fn bench_command(cli: &Cli, live: bool, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    match bench::command(cli, live, out, err) {
        Ok(()) => 0,
        Err(error) => {
            tui::restore_best_effort();
            report(err, &error.to_string())
        }
    }
}

fn run_record(cli: &Cli, out: &mut dyn Write) -> siltop_core::Result<()> {
    let now = store_disk::unix_now();
    let path = cli.out.clone().unwrap_or_else(|| record::default_out(now));
    let file = record::create(&path)?;
    let mut sampler = sampler(cli.demo, cli.headless_interval())?;
    let (tx, _rx) = std::sync::mpsc::channel();
    let shutdown = signals::install(tx)?;
    let plan = record::Plan {
        interval: cli.headless_interval(),
        duration: cli.duration,
        started_unix_ms: now * 1000,
    };
    let stop = || shutdown.requested();
    let mut pacer = worker::Pacer::new(&stop);
    let writer = record::record(
        &mut || sampler.tick(),
        &mut |period| pacer.wait(period),
        &stop,
        file,
        plan,
    )?;
    writeln!(out, "{}", record::summary(&writer, &path))?;
    Ok(())
}

fn run_serve(cli: &Cli, err: &mut dyn Write) -> siltop_core::Result<()> {
    let addr = cli
        .addr
        .map_or_else(|| cli::SERVE_ADDR.to_owned(), |a| a.to_string());
    let listener = std::net::TcpListener::bind(&addr)
        .map_err(|e| siltop_core::Error::system(format!("listen on {addr}: {e}")))?;
    let bound = listener
        .local_addr()
        .map_err(|e| siltop_core::Error::system(format!("listen on {addr}: {e}")))?;
    let (tx, rx) = std::sync::mpsc::channel();
    let shutdown = signals::install(tx.clone())?;
    let (demo, step) = (cli.demo, cli.headless_interval());
    let _worker = worker::spawn_with(step, tx, move || sampler(demo, step))?;
    let latest = serve::Latest::default();
    let shared = latest.clone();
    std::thread::Builder::new()
        .name(String::from("siltop-serve"))
        .spawn(move || serve::accept_loop(&listener, &shared))
        .map_err(|e| siltop_core::Error::system(format!("serve thread: {e}")))?;
    let _ = err.write_all(serve::announce(bound).as_bytes());
    for msg in rx {
        if shutdown.requested() {
            break;
        }
        match msg {
            msg::AppMsg::Snapshot(result) => latest.set(serve::exposition(&(*result)?)),
            msg::AppMsg::Stopped(err) => return Err(err),
            _ => {}
        }
    }
    Ok(())
}

fn run_export(cli: &Cli, out: &mut dyn Write) -> siltop_core::Result<()> {
    let root = store_disk::default_root()
        .ok_or_else(|| siltop_core::Error::system("no home directory for saved history"))?;
    let store = store_disk::DiskStore::open(&root, store_disk::SIZE_CAP)?;
    let now = store_disk::unix_now();
    let from = cli.from.map_or(0, |w| w.resolve(now, false));
    let to = cli.to.map_or(now + 1, |w| w.resolve(now, true));
    export::finish(export::write_csv(&store, from, to, now, out))
}

/// `--json`, `--json-stream` and `--diagnose`. They read the config file as
/// the dashboard does: its `interval_ms` applies unless `-i` is given, and
/// ignored values are named on stderr. A broken default file only warns,
/// since the dashboard would carry on with defaults too.
fn headless(cli: &Cli, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let mut cli = cli.clone();
    match read_config(&cli) {
        Ok(file) => {
            warn_all(err, &file.warnings);
            cli.interval = cli.interval.or(file.interval);
        }
        Err(message) if cli.config.is_none() => {
            let _ = writeln!(err, "siltop: {message}; using defaults");
        }
        Err(message) => return report(err, &message),
    }
    let cli = &cli;
    let mut sampler = match sampler(cli.demo, cli.headless_interval()) {
        Ok(sampler) => sampler,
        Err(error) => return report(err, &error.to_string()),
    };
    let mut tick = || sampler.tick();
    let never = || false; // Ctrl-C and a closed pipe end the process
    let mut pacer = worker::Pacer::new(&never);
    let mut wait = |period| pacer.wait(period);
    let result = match cli.mode {
        Mode::Diagnose => headless::run_diagnose(&mut tick, &mut wait, out, cli),
        _ => headless::run_json(&mut tick, &mut wait, out, cli, cli.mode == Mode::JsonStream),
    };
    match result {
        Ok(()) => 0,
        Err(error) => report(err, &error.to_string()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use siltop_core::Surface;

    const NO_TTY: Tty = Tty {
        stdin: false,
        stdout: false,
    };

    #[test]
    fn line_colours_with_any_named_theme() {
        for name in ["dracula", "nord"] {
            let theme = siltop_ui::Theme::by_name(name).unwrap();
            assert_eq!(line_theme(name).fg, theme.fg, "{name}");
        }
        assert_eq!(
            line_theme("no-such-theme").fg,
            siltop_ui::Theme::default().fg
        );
    }

    /// An empty config file: every default, and nothing to warn about.
    fn empty_config() -> std::path::PathBuf {
        let path = std::env::temp_dir().join("siltop-tests-empty-config.toml");
        std::fs::write(&path, "").unwrap();
        path
    }

    /// Every dispatch test runs, unless it names a config file, with none:
    /// a theme on the developer's Mac must never change a result.
    fn run(args: &[&str], tty: Tty) -> (u8, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let parsed = cli::parse(args.iter().copied()).map(|mut cli| {
            cli.config.get_or_insert_with(empty_config);
            cli
        });
        let code = dispatch(parsed, tty, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn d18_no_tty_prints_one_line_exit_2_and_zero_escape_bytes() {
        for tty in [
            NO_TTY,
            Tty {
                stdin: true,
                stdout: false,
            },
            Tty {
                stdin: false,
                stdout: true,
            },
        ] {
            let (code, out, err) = run(&[], tty);
            assert_eq!(code, 2);
            assert!(out.is_empty());
            assert_eq!(err.lines().count(), 1, "{err}");
            assert_eq!(err.trim_end(), NO_TTY_HINT);
            assert!(!out.contains('\x1b') && !err.contains('\x1b'));
        }
    }

    #[test]
    fn demo_json_prints_the_canned_mac_without_hardware() {
        let (code, out, err) = run(&["--demo", "--json", "--procs", "3"], NO_TTY);
        assert_eq!(code, 0, "{err}");
        assert!(err.is_empty(), "{err}");
        let value: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(
            value["soc"]["name"], "Apple M5 Pro",
            "same form as a real Mac"
        );
        assert!(value.is_object());
        assert!(!out.contains('\x1b'));
    }

    /// Regression: `--demo` showed the first-run screens and wrote their
    /// answers into the user's config.
    #[test]
    fn demo_never_onboards() {
        assert!(!onboards(&cli::parse(["--demo"]).unwrap()));
        assert!(onboards(&cli::parse(Vec::<&str>::new()).unwrap()));
        assert!(!onboards(&cli::parse(["replay", "x.trace"]).unwrap()));
    }

    #[test]
    fn a_dumb_terminal_gets_one_line_and_exit_2() {
        let tty = Tty {
            stdin: true,
            stdout: true,
        };
        let mut err = Vec::new();
        let cli = cli::parse(["--demo"]).unwrap();
        assert_eq!(dashboard(&cli, tty, true, &mut err), EXIT_USAGE);
        let err = String::from_utf8(err).unwrap();
        assert_eq!(err.trim_end(), DUMB_TERM_HINT);
        assert!(err.contains("--json") && err.contains("line") && err.contains("--diagnose"));
    }

    /// Regression: `replay /nope` without a terminal said "needs a
    /// terminal" first, and a bad file probed the terminal before failing.
    #[test]
    fn replay_checks_the_file_before_the_terminal() {
        let dir = config_io::tests::TempDir::new("main-replay-check");
        let text = dir.0.join("notes.txt");
        std::fs::write(&text, "not a trace\n").unwrap();
        let missing = dir.0.join("nope.trace");
        for (path, why) in [
            (&missing, "no such file"),
            (&text, ""),
            (&dir.0, "is a directory"),
        ] {
            let path_arg = path.to_string_lossy().into_owned();
            let (code, out, err) = run(&["replay", &path_arg], NO_TTY);
            assert_eq!(code, EXIT_ERROR, "{err}");
            assert!(out.is_empty());
            assert_eq!(err.lines().count(), 1, "{err}");
            assert!(err.starts_with(&format!("siltop: {path_arg}: ")), "{err}");
            assert!(err.contains(why), "{err}");
            assert!(
                !err.contains("system:") && !err.contains("os error"),
                "{err}"
            );
        }
    }

    #[test]
    fn demo_diagnose_prints_one_verdict() {
        let (code, out, err) = run(&["--demo", "--diagnose"], NO_TTY);
        assert_eq!(code, 0, "{err}");
        assert!(!out.trim().is_empty());
    }

    #[test]
    fn demo_line_prints_one_plain_line_without_escapes() {
        let (code, out, err) = run(&["--demo", "line", "--format", "{cpu} | {mem}"], NO_TTY);
        assert_eq!(code, 0, "{err}");
        assert_eq!(out.lines().count(), 1, "{out}");
        assert!(out.starts_with("cpu "), "{out}");
        assert!(out.contains(" | mem "), "{out}");
        assert!(!out.contains('\x1b'));
    }

    #[test]
    fn demo_line_ansi_keeps_calm_readings_in_the_terminal_colour() {
        // The default theme's foreground is the terminal's own; only warn and
        // crit readings get painted, and the demo Mac is calm.
        let (code, ansi, _) = run(&["--demo", "line", "--style", "ansi"], NO_TTY);
        let (_, plain, _) = run(&["--demo", "line"], NO_TTY);
        assert_eq!(code, 0);
        assert_eq!(ansi, plain);
    }

    #[test]
    fn simple_commands_print_to_stdout() {
        for (args, want) in [
            (&["theme", "list"][..], "billabong"),
            (&["man"], ".SH EXIT STATUS"),
            (&["--help"], "Exit status:"),
            (&["completions", "fish"], "siltop"),
        ] {
            let (code, out, err) = run(args, NO_TTY);
            assert_eq!(code, 0, "{args:?}: {err}");
            assert!(out.contains(want), "{args:?}");
        }
    }

    #[test]
    fn malformed_commands_exit_2_before_running() {
        for args in [
            &["export"][..],
            &["report", "--json", "week"],
            &["record", "--duration", "0s"],
        ] {
            let (code, out, err) = run(args, NO_TTY);
            assert_eq!(code, cli::EXIT_USAGE, "{args:?}");
            assert!(out.is_empty());
            assert!(err.ends_with(&format!("\n{USAGE_HINT}\n")), "{args:?}");
        }
    }

    #[test]
    fn demo_bench_prints_a_json_report_card() {
        let cli = cli::parse(["bench", "--demo", "--duration", "1s", "--json"]).unwrap();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = bench_command(&cli, false, &mut out, &mut err);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value["samples"], 2);
        assert_eq!(value["stop"]["reason"], "completed");
        assert!(
            value["chip"].as_str().unwrap().contains("M5 Pro"),
            "{value}"
        );
        // The demo never compares with, or saves over, this Mac's runs.
        assert_eq!(value["previous"], serde_json::Value::Null);
    }

    #[test]
    fn records_a_demo_trace_that_replays() {
        let name = format!("siltop-main-{}.ptrace", std::process::id());
        let path = std::env::temp_dir().join(name);
        let _ = std::fs::remove_file(&path);
        let path_arg = path.to_string_lossy().into_owned();
        let args = [
            "record",
            "--demo",
            "-i",
            "100",
            "--duration",
            "1s",
            "--out",
            &path_arg,
        ];
        let cli = cli::parse(args).unwrap();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = capture_command(&cli, &mut out, &mut err);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        assert!(String::from_utf8(out).unwrap().starts_with("recorded "));
        let (trace, warning) = replay::load(&path, std::time::Instant::now()).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(warning.is_none());
        assert!(trace.len() >= 2, "{}", trace.len());
        assert!(
            trace.header.note.contains("M5 Pro"),
            "{}",
            trace.header.note
        );
    }

    #[test]
    fn replay_needs_a_terminal() {
        let dir = config_io::tests::TempDir::new("main-replay-tty");
        let path = dir.0.join("session.ptrace");
        std::fs::write(&path, siltop_core::trace::TRACE_MAGIC).unwrap();
        let path_arg = path.to_string_lossy().into_owned();
        let (code, _, err) = run(&["replay", &path_arg], NO_TTY);
        assert_eq!(code, 2);
        assert_eq!(err.trim_end(), NO_TTY_HINT);
    }

    #[test]
    fn demo_tui_still_needs_a_terminal() {
        let (code, _, err) = run(&["--demo"], NO_TTY);
        assert_eq!(code, 2);
        assert_eq!(err.trim_end(), NO_TTY_HINT);
    }

    #[test]
    fn version_exits_zero_without_a_tty() {
        let (code, out, err) = run(&["--version"], NO_TTY);
        assert_eq!(code, 0);
        assert!(out.starts_with("siltop "));
        assert_eq!(out.lines().count(), 1);
        assert!(err.is_empty());
    }

    #[test]
    fn version_verbose_lists_build_and_paths() {
        let (code, out, err) = run(&["--version", "--verbose"], NO_TTY);
        assert_eq!(code, 0, "{err}");
        let mut lines = out.lines();
        assert_eq!(
            lines.next(),
            Some(format!("siltop {}", cli::VERSION).as_str())
        );
        let keys: Vec<&str> = lines.filter_map(|l| l.split_whitespace().next()).collect();
        for key in ["config", "state"] {
            assert!(keys.contains(&key), "{key} in {out}");
        }
        let config = run(
            &["-V", "--verbose", "--config", "/tmp/elsewhere.toml"],
            NO_TTY,
        )
        .1;
        assert!(
            config.contains("config   /tmp/elsewhere.toml\n"),
            "{config}"
        );
    }

    #[test]
    fn completions_and_man_print_to_stdout() {
        for (args, head) in [
            (&["completions", "zsh"][..], "#compdef siltop"),
            (&["completions", "bash"][..], "# bash completion"),
            (&["completions", "fish"][..], "# fish completion"),
            (&["man"][..], ".TH SILTOP 1"),
        ] {
            let (code, out, err) = run(args, NO_TTY);
            assert_eq!(code, 0, "{args:?}: {err}");
            assert!(out.starts_with(head), "{args:?}");
            assert!(err.is_empty());
        }
    }

    #[test]
    fn help_goes_to_stdout_and_exits_zero() {
        let (code, out, err) = run(&["-h"], NO_TTY);
        assert_eq!(code, 0);
        assert_eq!(out, cli::usage());
        assert!(err.is_empty());
    }

    #[test]
    fn unknown_flag_prints_usage_to_stderr_and_exits_2() {
        let (code, out, err) = run(&["--frobnicate"], NO_TTY);
        assert_eq!(code, 2);
        assert!(out.is_empty());
        assert!(err.starts_with("siltop: unknown argument \"--frobnicate\""));
        assert!(!err.contains('\x1b'));
    }

    #[test]
    fn usage_error_is_the_error_and_one_hint_not_the_whole_help() {
        // Regression: every usage error printed the ~94-line help after the
        // error, which scrolled the error itself off an 80x24 terminal.
        for args in [
            &["-i", "50ms"][..],
            &["report", "yesterday"],
            &["completions"],
        ] {
            let (code, out, err) = run(args, NO_TTY);
            assert_eq!(code, cli::EXIT_USAGE, "{args:?}");
            assert!(out.is_empty(), "{args:?}");
            let lines: Vec<&str> = err.lines().collect();
            assert_eq!(lines.len(), 2, "{args:?}: {err}");
            assert!(lines[0].starts_with("siltop: "), "{args:?}: {err}");
            assert_eq!(lines[1], USAGE_HINT);
            assert!(!err.contains("Usage: siltop"), "{args:?}");
        }
    }

    #[test]
    fn dispatch_tests_never_read_the_developers_config() {
        // Regression: a theme set in the real ~/.config/siltop/config.toml
        // painted `--demo line --style ansi` and failed the suite.
        let (code, out, err) = run(&["--print-config"], NO_TTY);
        assert_eq!(code, 0, "{err}");
        assert_eq!(
            out,
            config_text(&Config::default(), &cli::parse(["--print-config"]).unwrap()).unwrap()
        );
    }

    #[test]
    fn print_config_reflects_overrides() {
        let path = std::env::temp_dir().join("siltop-cli-print-config.toml");
        std::fs::write(
            &path,
            "interval_ms = 1000\nshow_disk = true\nsort = \"mem\"\n",
        )
        .unwrap();
        let path_arg = path.to_string_lossy().into_owned();
        let (code, out, _) = run(
            &[
                "--print-config",
                "--config",
                &path_arg,
                "--surface",
                "glance",
                "--theme",
                "paper",
                "--no-mouse",
            ],
            NO_TTY,
        );
        let _ = std::fs::remove_file(&path);
        assert_eq!(code, 0);
        assert!(out.contains("interval_ms = 1000\n"), "{out}");
        assert!(out.contains("surface = \"glance\"\n"));
        assert!(out.contains("theme = \"paper\"\n"));
        assert!(out.starts_with("# mouse is off for this session (--no-mouse)"));
        assert!(
            !out.contains("mouse = "),
            "mouse is not a config key: {out}"
        );
        assert!(out.contains("[panels]\n"));
        assert!(out.contains("disk = true\n"));
        assert!(out.contains("column = \"mem\"\n"));
    }

    /// `--print-config` used to print pre-v1 flat keys (`show_gpu`, `sort`,
    /// `proc_ratio`) plus a fake `mouse` key, and to leave out most settings.
    #[test]
    fn print_config_prints_every_v1_key_not_legacy_ones() {
        let (code, out, _) = run(&["--print-config"], NO_TTY);
        assert_eq!(code, 0);
        for key in [
            "version = ",
            "icons = ",
            "keymap = ",
            "low_power = ",
            "zen = ",
            "glass = ",
            "extra_contrast = ",
            "[panels]",
            "power = ",
            "[proc]",
            "cpu_unit = ",
            "[proc.sort]",
            "desc = ",
            "[units]",
            "net_bits = ",
        ] {
            assert!(out.contains(key), "missing {key:?} in {out}");
        }
        for legacy in ["show_gpu", "show_tree", "proc_ratio", "sort = ", "mouse"] {
            assert!(!out.contains(legacy), "legacy {legacy:?} in {out}");
        }
    }

    /// Regression: a file that does not parse printed the defaults with
    /// exit 0, while the app itself says it is using defaults.
    #[test]
    fn print_config_of_a_broken_file_is_an_error() {
        let path = std::env::temp_dir().join("siltop-cli-print-config-broken.toml");
        std::fs::write(&path, "interval_ms = [\n").unwrap();
        let path_arg = path.to_string_lossy().into_owned();
        let (code, out, err) = run(&["--print-config", "--config", &path_arg], NO_TTY);
        let _ = std::fs::remove_file(&path);
        assert_eq!(code, EXIT_ERROR);
        assert!(out.is_empty(), "{out}");
        assert!(err.starts_with("siltop: "), "{err}");
        assert!(err.contains(&path_arg), "{err}");
    }

    /// An ignored value is named on stderr; the rest still prints.
    #[test]
    fn print_config_names_ignored_values_on_stderr() {
        let path = std::env::temp_dir().join("siltop-cli-print-config-warn.toml");
        std::fs::write(&path, "interval_ms = \"soon\"\nzen = true\n").unwrap();
        let path_arg = path.to_string_lossy().into_owned();
        let (code, out, err) = run(&["--print-config", "--config", &path_arg], NO_TTY);
        let _ = std::fs::remove_file(&path);
        assert_eq!(code, 0);
        assert!(out.contains("zen = true\n"), "{out}");
        assert!(err.contains("ignored interval_ms"), "{err}");
    }

    #[test]
    fn print_config_interval_flag_beats_file() {
        let (code, out, _) = run(&["--print-config", "-i", "2s"], NO_TTY);
        assert_eq!(code, 0);
        assert!(out.contains("\ninterval_ms = 2000\n"), "{out}");
        assert!(out.contains("surface = \"auto\"\n"));
    }

    /// A file named with `--config` that cannot be used stops every command
    /// that reads it with one line and exit 1, instead of quiet defaults.
    #[test]
    fn a_named_config_that_cannot_be_read_is_one_line_and_exit_1() {
        let dir = config_io::tests::TempDir::new("main-bad-config");
        let missing = dir.0.join("nope.toml");
        let broken = dir.0.join("broken.toml");
        std::fs::write(&broken, "interval_ms = [\n").unwrap();
        let binary = dir.0.join("binary.toml");
        std::fs::write(&binary, [0xff, 0xfe, 0x00, 0x81]).unwrap();
        for (path, why) in [
            (&missing, "no such file"),
            (&dir.0, "is a directory"),
            (&broken, "not valid TOML"),
            (&binary, "not a text file"),
        ] {
            let path_arg = path.to_string_lossy().into_owned();
            for args in [
                &["--print-config"][..],
                &["--demo", "--json"],
                &["--demo", "--json-stream", "--count", "1"],
                &["--demo", "--diagnose"],
                &["--demo", "line"],
                &[],
            ] {
                let mut args = args.to_vec();
                args.extend(["--config", &path_arg]);
                let (code, out, err) = run(&args, NO_TTY);
                assert_eq!(code, EXIT_ERROR, "{args:?}: {err}");
                assert!(out.is_empty(), "{args:?}: {out}");
                assert_eq!(err.lines().count(), 1, "{args:?}: {err}");
                assert!(err.contains(why), "{args:?}: {err}");
                assert!(!err.contains("os error"), "{err}");
            }
        }
    }

    #[test]
    fn json_and_diagnose_use_the_config_interval_and_name_unknown_keys() {
        let dir = config_io::tests::TempDir::new("main-json-config");
        let path = dir.0.join("config.toml");
        std::fs::write(&path, "interval_ms = 2000\nzen = \"x\"\n[ui]\ntheme = 5\n").unwrap();
        let path_arg = path.to_string_lossy().into_owned();
        let (code, out, err) = run(&["--demo", "--json", "--config", &path_arg], NO_TTY);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("\"interval_ms\":2000"), "{out}");
        assert_eq!(err.lines().count(), 2, "{err}");
        assert!(err.contains("ignored unknown table [ui]"), "{err}");
        assert!(err.contains("ignored zen = \"x\""), "{err}");
        let (_, out, _) = run(
            &["--demo", "--json", "-i", "300", "--config", &path_arg],
            NO_TTY,
        );
        assert!(out.contains("\"interval_ms\":300"), "the flag wins: {out}");
        let (code, _, err) = run(&["--demo", "--diagnose", "--config", &path_arg], NO_TTY);
        assert_eq!(code, 0, "{err}");
        assert!(err.contains("[ui]"), "{err}");
    }

    /// The printed config must load back to the same preferences.
    #[test]
    fn print_config_round_trips_through_the_loader() {
        let cli = cli::parse(["--surface", "work", "-i", "500"]).unwrap();
        let mut config = Config::default().with_interval(std::time::Duration::from_millis(500));
        config.surface = Some(Surface::Work);
        config.show_tree = true;
        let path = std::env::temp_dir().join("siltop-cli-roundtrip.toml");
        std::fs::write(&path, config_text(&config, &cli).unwrap()).unwrap();
        let loaded = Config::load(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded, config);
    }
}
