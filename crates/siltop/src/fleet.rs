//! `siltop fleet <HOST>...`: this Mac and several others in one terminal.
//!
//! siltop does not open a connection of its own. Each remote is a child process,
//! `ssh -o BatchMode=yes -- <host> siltop --json-stream --procs 30`, run with
//! the user's own ssh config and keys; its stdout is schema-1 NDJSON (the
//! same [`Report`] `--json-stream` writes), read back line by line. A dropped
//! link is retried with exponential backoff, and every ssh child is killed and
//! reaped when the view closes.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, Read};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use serde::Deserialize;
use siltop_core::{Error, Pressure, Result};
use siltop_ui::{CardLink, FleetCard, FleetProc, Theme};

use crate::cli::Cli;
use crate::headless::{Context, Report, SCHEMA_VERSION};
use crate::msg::AppMsg;

/// Processes each remote reports, for the Enter view: enough to fill its
/// full-screen pane on a tall terminal (about 200 bytes each per line).
pub const PROCS: usize = 30;
/// Samples of CPU history a card keeps (more than any card is wide).
const HISTORY_LEN: usize = 160;
/// A host is stale after this many of its own intervals without a line...
const STALE_INTERVALS: u32 = 3;
/// ...and never sooner than this, so a fast interval does not flicker.
const STALE_FLOOR: Duration = Duration::from_secs(3);
/// A pending partial line longer than this is garbage, not a report.
const MAX_LINE: usize = 1 << 20;
/// First and largest reconnect delays.
const BACKOFF_BASE: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(30);
/// How long the loop waits for a key before redrawing.
const FRAME: Duration = Duration::from_millis(250);

/// Why one NDJSON line was not a usable report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineError {
    /// Not JSON, or JSON that is not a schema-1 report.
    Garbage(String),
    /// A report from a siltop speaking another schema.
    Schema(u32),
}

/// One line of `--json-stream`, schema checked before the full parse so a
/// future schema is named rather than reported as garbage. Every string in
/// the result is printable: another Mac's text never acts on this terminal.
pub fn parse_line(line: &str) -> std::result::Result<Report, LineError> {
    #[derive(Deserialize)]
    struct Peek {
        schema_version: u32,
    }
    let peek: Peek = serde_json::from_str(line).map_err(|e| LineError::Garbage(e.to_string()))?;
    if peek.schema_version != SCHEMA_VERSION {
        return Err(LineError::Schema(peek.schema_version));
    }
    serde_json::from_str::<Report>(line)
        .map(Report::into_printable)
        .map_err(|e| LineError::Garbage(siltop_core::into_printable(e.to_string())))
}

/// Splits a byte stream into whole lines; a partial line waits for the rest.
#[derive(Debug, Default)]
pub struct LineBuffer {
    pending: Vec<u8>,
}

impl LineBuffer {
    /// The complete, non-blank lines `bytes` finished. A runaway line with no
    /// newline is dropped once it passes [`MAX_LINE`].
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(bytes);
        let mut lines = Vec::new();
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let text = String::from_utf8_lossy(&line);
            let text = text.trim();
            if !text.is_empty() {
                lines.push(text.to_owned());
            }
        }
        if self.pending.len() > MAX_LINE {
            self.pending.clear();
        }
        lines
    }
}

/// Exponential reconnect delay: 1s, 2s, 4s ... capped at 30s; a report resets it.
#[derive(Debug, Default, Clone, Copy)]
pub struct Backoff {
    attempt: u32,
}

impl Backoff {
    pub fn next_delay(&mut self) -> Duration {
        let delay = BACKOFF_BASE
            .saturating_mul(1u32 << self.attempt.min(16))
            .min(BACKOFF_CAP);
        self.attempt = self.attempt.saturating_add(1);
        delay
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

/// Builds the command that streams one remote host. Injectable so tests run
/// a fake `ssh` script instead of the real one.
#[derive(Debug, Clone)]
pub struct Launcher {
    pub program: OsString,
}

impl Default for Launcher {
    fn default() -> Self {
        Self {
            program: OsString::from("ssh"),
        }
    }
}

impl Launcher {
    /// `--` ends ssh's options, so a host can never be read as a flag.
    #[must_use]
    pub fn args(host: &str) -> Vec<String> {
        let procs = PROCS.to_string();
        [
            "-o",
            "BatchMode=yes",
            "-o",
            "ServerAliveInterval=5",
            "--",
            host,
            "siltop",
            "--json-stream",
            "--procs",
            &procs,
        ]
        .map(str::to_owned)
        .to_vec()
    }

    fn spawn(&self, host: &str) -> io::Result<Child> {
        Command::new(&self.program)
            .args(Self::args(host))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
    }
}

/// What reader threads tell the loop, tagged with host index and generation
/// so a line from a replaced child is ignored.
#[derive(Debug)]
pub enum FleetEvent {
    Report(usize, u64, Box<Report>),
    Ended(usize, u64),
}

/// A running ssh child; dropping it kills and reaps the process. The reader
/// thread is not joined: it ends at EOF, and a grandchild still holding the
/// pipe must never stall the quit.
#[derive(Debug)]
pub struct Remote {
    child: Child,
}

impl Remote {
    fn start(
        launcher: &Launcher,
        host: &str,
        index: usize,
        generation: u64,
        tx: Sender<FleetEvent>,
    ) -> io::Result<Self> {
        let mut child = launcher.spawn(host)?;
        let Some(mut stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("ssh child has no stdout"));
        };
        thread::Builder::new()
            .name(format!("siltop-fleet-{index}"))
            .spawn(move || {
                let mut lines = LineBuffer::default();
                let mut chunk = [0u8; 8192];
                loop {
                    match stdout.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            for line in lines.push(&chunk[..n]) {
                                if let Ok(report) = parse_line(&line) {
                                    let event =
                                        FleetEvent::Report(index, generation, Box::new(report));
                                    if tx.send(event).is_err() {
                                        return;
                                    }
                                }
                            }
                        }
                    }
                }
                let _ = tx.send(FleetEvent::Ended(index, generation));
            })?;
        Ok(Self { child })
    }

    #[cfg(test)]
    fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Where a host's readings come from.
enum Source {
    /// A remote over ssh; `None` while waiting to reconnect.
    Ssh(Option<Remote>),
    /// This Mac's sampler (or the demo), on the shared worker thread.
    Sampler(Receiver<AppMsg>, #[allow(dead_code)] crate::worker::Handle),
    /// Nothing streams it (tests).
    #[cfg(test)]
    Idle,
}

/// One card's state.
pub struct Host {
    /// What ssh is given, exactly as typed.
    pub name: String,
    /// `name` made printable, for the card.
    label: String,
    source: Source,
    last: Option<Box<Report>>,
    last_seen: Option<Instant>,
    cpu: VecDeque<f32>,
    backoff: Backoff,
    /// When a dropped ssh link is retried.
    retry_at: Option<Instant>,
    generation: u64,
}

impl Host {
    fn new(name: String, source: Source) -> Self {
        Self {
            label: siltop_core::printable(&name).into_owned(),
            name,
            source,
            last: None,
            last_seen: None,
            cpu: VecDeque::with_capacity(HISTORY_LEN),
            backoff: Backoff::default(),
            retry_at: None,
            generation: 0,
        }
    }

    fn on_report(&mut self, report: Box<Report>, now: Instant) {
        if self.cpu.len() == HISTORY_LEN {
            self.cpu.pop_front();
        }
        self.cpu.push_back(report.cpu.usage);
        // `history` hands out the first slice: keep the ring in one piece.
        self.cpu.make_contiguous();
        self.last = Some(report);
        self.last_seen = Some(now);
        self.backoff.reset();
    }

    /// The link dropped: schedule the next attempt.
    fn on_ended(&mut self, now: Instant) {
        if let Source::Ssh(remote) = &mut self.source {
            *remote = None;
        }
        self.retry_at = Some(now + self.backoff.next_delay());
    }

    /// No line for [`STALE_INTERVALS`] of the host's own interval.
    #[must_use]
    pub fn is_stale(&self, now: Instant) -> bool {
        let (Some(seen), Some(last)) = (self.last_seen, &self.last) else {
            return false;
        };
        let interval = Duration::from_millis(last.interval_ms);
        let limit = interval.saturating_mul(STALE_INTERVALS).max(STALE_FLOOR);
        now.saturating_duration_since(seen) > limit
    }

    #[must_use]
    pub fn link(&self, now: Instant) -> CardLink {
        match (&self.last, self.retry_at) {
            (None, None) => CardLink::Connecting,
            (Some(_), None) if !self.is_stale(now) => CardLink::Live,
            _ => CardLink::Reconnecting,
        }
    }

    fn history(&self) -> &[f32] {
        self.cpu.as_slices().0
    }

    fn card(&self, now: Instant) -> FleetCard<'_> {
        let link = self.link(now);
        let Some(r) = self.last.as_deref() else {
            return FleetCard {
                host: &self.label,
                link,
                chip: None,
                cpu: None,
                clocks: Vec::new(),
                gpu: None,
                mem: None,
                pressure: None,
                power_w: None,
                peak_c: None,
                battery: None,
                verdict: None,
                cpu_history: &[],
            };
        };
        let clocks = ["super", "performance", "efficiency"]
            .into_iter()
            .filter_map(|kind| {
                let cluster = r.cpu.clusters.iter().find(|c| c.kind == kind)?;
                let tag = &kind[..1];
                Some((tag, cluster.freq_mhz?))
            })
            .map(|(tag, mhz)| {
                (
                    if tag == "s" {
                        "S"
                    } else if tag == "p" {
                        "P"
                    } else {
                        "E"
                    },
                    mhz,
                )
            })
            .collect();
        let temps = &r.temp;
        let peak_c = [
            temps.cpu_temp_avg,
            temps.gpu_temp_avg,
            temps.hotspot_c,
            temps.e_cluster_c,
            temps.p_cluster_c,
            temps.s_cluster_c,
        ]
        .into_iter()
        .flatten()
        .reduce(f32::max);
        FleetCard {
            host: &self.label,
            link,
            chip: Some(r.soc.name.as_str()).filter(|n| !n.is_empty()),
            cpu: Some(r.cpu.usage),
            clocks,
            gpu: r.gpu.as_ref().map(|g| g.usage),
            mem: (r.memory.ram_total > 0).then_some((r.memory.ram_usage, r.memory.ram_total)),
            pressure: pressure_by_word(&r.memory.pressure),
            power_w: r.sys_power.or(r.all_power),
            peak_c,
            battery: r
                .battery
                .as_ref()
                .and_then(|b| Some((b.percent?, b.charging.unwrap_or(false)))),
            verdict: Some(r.verdict.headline.as_str()),
            cpu_history: self.history(),
        }
    }

    fn procs(&self) -> Vec<FleetProc<'_>> {
        self.last.as_deref().map_or_else(Vec::new, |r| {
            r.processes
                .iter()
                .map(|p| FleetProc {
                    pid: p.pid,
                    name: &p.name,
                    cpu_pct: p.cpu_pct,
                    mem_bytes: p.mem_bytes,
                })
                .collect()
        })
    }
}

fn pressure_by_word(word: &str) -> Option<Pressure> {
    match word {
        "nominal" => Some(Pressure::Nominal),
        "warn" => Some(Pressure::Warn),
        "critical" => Some(Pressure::Critical),
        _ => None,
    }
}

/// Every host plus the channel its ssh readers report on.
pub struct Fleet {
    pub hosts: Vec<Host>,
    launcher: Launcher,
    tx: Sender<FleetEvent>,
    rx: Receiver<FleetEvent>,
    /// For turning a local snapshot into the same report a remote sends.
    ctx: Context,
}

impl Fleet {
    fn new(launcher: Launcher, interval: Duration) -> Self {
        let (tx, rx) = channel();
        Self {
            hosts: Vec::new(),
            launcher,
            tx,
            rx,
            ctx: Context {
                unix_ms: 0,
                interval,
                procs: PROCS,
            },
        }
    }

    /// Adds a remote and starts its ssh child. A failed spawn (no `ssh` on
    /// PATH) is retried like a dropped link.
    fn add_remote(&mut self, name: &str, now: Instant) {
        let index = self.hosts.len();
        self.hosts
            .push(Host::new(name.to_owned(), Source::Ssh(None)));
        self.connect(index, now);
    }

    fn add_sampler(&mut self, name: String, rx: Receiver<AppMsg>, handle: crate::worker::Handle) {
        self.hosts
            .push(Host::new(name, Source::Sampler(rx, handle)));
    }

    fn connect(&mut self, index: usize, now: Instant) {
        let Some(host) = self.hosts.get_mut(index) else {
            return;
        };
        host.generation += 1;
        host.retry_at = None;
        let started = Remote::start(
            &self.launcher,
            &host.name,
            index,
            host.generation,
            self.tx.clone(),
        );
        match started {
            Ok(remote) => host.source = Source::Ssh(Some(remote)),
            Err(_) => host.on_ended(now),
        }
    }

    /// Applies everything the readers and samplers sent, then retries links
    /// whose backoff has passed.
    pub fn pump(&mut self, now: Instant) {
        loop {
            match self.rx.try_recv() {
                Ok(FleetEvent::Report(i, generation, report)) => {
                    if let Some(host) = self.hosts.get_mut(i).filter(|h| h.generation == generation)
                    {
                        host.on_report(report, now);
                    }
                }
                Ok(FleetEvent::Ended(i, generation)) => {
                    if let Some(host) = self.hosts.get_mut(i).filter(|h| h.generation == generation)
                    {
                        host.on_ended(now);
                    }
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        let ctx = Context {
            unix_ms: crate::headless::unix_ms_now(),
            ..self.ctx
        };
        for host in &mut self.hosts {
            let Source::Sampler(rx, _) = &host.source else {
                continue;
            };
            let mut latest = None;
            while let Ok(msg) = rx.try_recv() {
                if let AppMsg::Snapshot(result) = msg
                    && let Ok(snap) = *result
                {
                    latest = Some(snap);
                }
            }
            if let Some(snap) = latest {
                let verdict = siltop_core::assess(&snap, &siltop_core::Signals::default());
                host.on_report(Box::new(Report::from_snapshot(&snap, &verdict, ctx)), now);
            }
        }
        let due: Vec<usize> = self
            .hosts
            .iter()
            .enumerate()
            .filter(|(_, h)| h.retry_at.is_some_and(|at| now >= at))
            .map(|(i, _)| i)
            .collect();
        for index in due {
            self.connect(index, now);
        }
    }

    /// Kills and reaps every ssh child and stops the samplers.
    pub fn shutdown(&mut self) {
        self.hosts.clear();
    }
}

impl Drop for Fleet {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Selection and view state for the loop.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct View {
    pub selected: usize,
    pub procs_open: bool,
}

/// What a key asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Stay,
    Quit,
}

impl View {
    /// Arrows / hjkl move between cards (`cols` per row; Up/Down stay in
    /// their column and do nothing past the grid's edge), Enter opens a
    /// host's processes, Esc closes them (and is a no-op on the grid), q
    /// quits.
    pub fn key(&mut self, code: KeyCode, ctrl: bool, hosts: usize, cols: usize) -> Step {
        let last = hosts.saturating_sub(1);
        let cols = cols.max(1);
        match code {
            KeyCode::Char('q') => return Step::Quit,
            KeyCode::Char('c') if ctrl => return Step::Quit,
            KeyCode::Esc | KeyCode::Backspace => self.procs_open = false,
            KeyCode::Enter => self.procs_open = !self.procs_open,
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                self.selected = (self.selected + 1).min(last);
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') if self.selected + cols <= last => {
                self.selected += cols;
            }
            KeyCode::Up | KeyCode::Char('k') if self.selected >= cols => self.selected -= cols,
            _ => {}
        }
        Step::Stay
    }
}

/// The local card's name under `--demo`: the data is canned, so the real
/// hostname never appears beside it.
const DEMO_LOCAL_NAME: &str = "this-mac";

/// The local Mac's card name: its short hostname, else "this Mac"; a fixed
/// stand-in under `--demo`.
fn local_name(demo: bool) -> String {
    if demo {
        return String::from(DEMO_LOCAL_NAME);
    }
    Command::new("hostname")
        .arg("-s")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| String::from("this Mac"))
}

fn sampler_host(fleet: &mut Fleet, name: String, demo: bool, interval: Duration) -> Result<()> {
    let (tx, rx) = channel();
    let handle = crate::worker::spawn_with(interval, tx, move || crate::sampler(demo, interval))?;
    fleet.add_sampler(name, rx, handle);
    Ok(())
}

/// A `--demo` remote: a canned Mac of its own, never a copy of this one.
fn demo_host(
    fleet: &mut Fleet,
    name: String,
    profile: crate::demo::Profile,
    interval: Duration,
) -> Result<()> {
    let (tx, rx) = channel();
    let handle = crate::worker::spawn_with(interval, tx, move || {
        Ok(crate::demo::ProfiledDemo::new(interval, profile))
    })?;
    fleet.add_sampler(name, rx, handle);
    Ok(())
}

/// Columns the grid uses for `n` hosts at `width`, for Up/Down.
fn grid_cols(area: ratatui::layout::Rect, n: usize) -> usize {
    let rects = siltop_ui::fleet_grid(area, n);
    rects.iter().take_while(|r| r.y == area.y).count().max(1)
}

/// `siltop fleet`: the full-screen loop.
pub fn run(cli: &Cli) -> Result<()> {
    let config = crate::cli_config(cli);
    let theme = crate::term_caps::startup_theme(&config);
    let interval = config.interval;
    let now = Instant::now();
    let mut fleet = Fleet::new(Launcher::default(), interval);
    sampler_host(&mut fleet, local_name(cli.demo), cli.demo, interval)?;
    for (host, profile) in cli.hosts.iter().zip(crate::demo::FLEET.iter().cycle()) {
        if cli.demo {
            // A canned Mac per name: the grid without any ssh.
            demo_host(&mut fleet, host.clone(), *profile, interval)?;
        } else {
            fleet.add_remote(host, now);
        }
    }
    let (sig_tx, _sig_rx) = channel();
    let shutdown = crate::signals::install(sig_tx)?;
    let mut terminal = crate::tui::install()?;
    let result = event_loop(&mut terminal, &mut fleet, &theme, &|| shutdown.requested());
    fleet.shutdown();
    crate::tui::restore()?;
    result
}

fn event_loop(
    terminal: &mut crate::tui::AppTerminal,
    fleet: &mut Fleet,
    theme: &Theme,
    stop: &dyn Fn() -> bool,
) -> Result<()> {
    let mut view = View::default();
    let mut area = ratatui::layout::Rect::default();
    while !stop() {
        let now = Instant::now();
        fleet.pump(now);
        terminal
            .draw(|frame| {
                area = frame.area();
                let cards: Vec<FleetCard<'_>> = fleet.hosts.iter().map(|h| h.card(now)).collect();
                let selected = view.selected.min(cards.len().saturating_sub(1));
                match (
                    view.procs_open,
                    fleet.hosts.get(selected),
                    cards.get(selected),
                ) {
                    (true, Some(host), Some(card)) => {
                        siltop_ui::render_fleet_procs(frame, area, card, &host.procs(), theme);
                    }
                    _ => siltop_ui::render_fleet(frame, area, &cards, selected, theme),
                }
            })
            .map_err(|e| Error::terminal(e.to_string()))?;
        if !event::poll(FRAME).map_err(|e| Error::terminal(e.to_string()))? {
            continue;
        }
        match event::read().map_err(|e| Error::terminal(e.to_string()))? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                let cols = grid_cols(area, fleet.hosts.len());
                if view.key(key.code, ctrl, fleet.hosts.len(), cols) == Step::Quit {
                    break;
                }
            }
            Event::Mouse(mouse) if matches!(mouse.kind, event::MouseEventKind::Down(_)) => {
                if let Some(i) =
                    siltop_ui::fleet_hit(area, fleet.hosts.len(), mouse.column, mouse.row)
                {
                    view.procs_open = view.selected == i && !view.procs_open;
                    view.selected = i;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    /// A real schema-1 line, from the headless golden.
    fn golden_line() -> String {
        crate::headless::tests::GOLDEN.to_owned()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("siltop-fleet-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A fake `ssh` that prints `body` (shell) and records its pid and args.
    fn fake_ssh(name: &str, body: &str) -> (Launcher, PathBuf) {
        let dir = scratch(name);
        let script = dir.join("ssh");
        let text = format!(
            "#!/bin/sh\necho $$ > '{pid}'\necho \"$@\" > '{args}'\n{body}\n",
            pid = dir.join("pid").display(),
            args = dir.join("args").display(),
        );
        std::fs::write(&script, text).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (
            Launcher {
                program: script.into_os_string(),
            },
            dir,
        )
    }

    fn wait_for(mut done: impl FnMut() -> bool) {
        for _ in 0..500 {
            if done() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("condition never held");
    }

    fn alive(pid: u32) -> bool {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|s| s.success())
    }

    fn idle_host(name: &str) -> Host {
        Host::new(name.to_owned(), Source::Idle)
    }

    #[test]
    fn parses_a_schema_one_line() {
        let report = parse_line(&golden_line()).unwrap();
        assert_eq!(report.soc.name, "Apple M5 Pro");
        assert_eq!(report.processes.len(), 2);
        assert_eq!(report.verdict.headline, "All normal");
    }

    #[test]
    fn round_trips_what_json_stream_writes() {
        let report = parse_line(&golden_line()).unwrap();
        let again = serde_json::to_string(&report).unwrap();
        assert_eq!(again, golden_line());
    }

    #[test]
    fn rejects_another_schema_by_number() {
        let line = golden_line().replacen("\"schema_version\":1", "\"schema_version\":2", 1);
        assert_eq!(parse_line(&line).unwrap_err(), LineError::Schema(2));
    }

    #[test]
    fn garbage_lines_are_errors_not_panics() {
        for line in [
            "",
            "hello",
            "{",
            "[1,2]",
            "{\"schema_version\":1}",
            "Last login: Mon",
            "\u{0}\u{1}",
        ] {
            assert!(
                matches!(parse_line(line), Err(LineError::Garbage(_))),
                "{line:?}"
            );
        }
    }

    #[test]
    fn line_buffer_holds_partial_lines_until_newline() {
        let mut buf = LineBuffer::default();
        let line = golden_line();
        let (a, b) = line.split_at(100);
        assert!(buf.push(a.as_bytes()).is_empty());
        let lines = buf.push(format!("{b}\n").as_bytes());
        assert_eq!(lines, vec![line.clone()]);
        assert!(parse_line(&lines[0]).is_ok());
    }

    #[test]
    fn line_buffer_splits_many_lines_in_one_chunk_and_skips_blanks() {
        let mut buf = LineBuffer::default();
        let lines = buf.push(b"one\r\n\n  \ntwo\nthr");
        assert_eq!(lines, vec!["one", "two"]);
        assert_eq!(buf.push(b"ee\n"), vec!["three"]);
    }

    #[test]
    fn line_buffer_splits_multibyte_utf8_across_chunks() {
        let mut buf = LineBuffer::default();
        let text = "61°C\n".as_bytes();
        assert!(buf.push(&text[..3]).is_empty());
        assert_eq!(buf.push(&text[3..]), vec!["61°C"]);
    }

    #[test]
    fn line_buffer_drops_runaway_line_without_newline() {
        let mut buf = LineBuffer::default();
        assert!(buf.push(&vec![b'x'; MAX_LINE + 1]).is_empty());
        assert_eq!(buf.push(b"ok\n"), vec!["ok"]);
    }

    #[test]
    fn backoff_doubles_to_a_cap_and_resets() {
        let mut backoff = Backoff::default();
        let delays: Vec<u64> = (0..8).map(|_| backoff.next_delay().as_secs()).collect();
        assert_eq!(delays, vec![1, 2, 4, 8, 16, 30, 30, 30]);
        backoff.reset();
        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
    }

    #[test]
    fn backoff_never_overflows_after_many_failures() {
        let mut backoff = Backoff {
            attempt: u32::MAX - 1,
        };
        assert_eq!(backoff.next_delay(), BACKOFF_CAP);
        assert_eq!(backoff.next_delay(), BACKOFF_CAP);
    }

    #[test]
    fn stale_after_three_intervals_with_a_floor() {
        let t0 = Instant::now();
        let mut host = idle_host("a");
        assert!(
            !host.is_stale(t0 + Duration::from_secs(99)),
            "no data is connecting, not stale"
        );
        host.on_report(Box::new(parse_line(&golden_line()).unwrap()), t0);
        // interval_ms 1000: floor of 3s wins over 3 x 1s.
        assert!(!host.is_stale(t0 + Duration::from_secs(3)));
        assert!(host.is_stale(t0 + Duration::from_millis(3001)));
        assert_eq!(host.link(t0 + Duration::from_secs(1)), CardLink::Live);
        assert_eq!(
            host.link(t0 + Duration::from_secs(4)),
            CardLink::Reconnecting
        );
    }

    #[test]
    fn stale_scales_with_a_slow_remote_interval() {
        let t0 = Instant::now();
        let mut host = idle_host("a");
        let mut report = parse_line(&golden_line()).unwrap();
        report.interval_ms = 5000;
        host.on_report(Box::new(report), t0);
        assert!(!host.is_stale(t0 + Duration::from_secs(14)));
        assert!(host.is_stale(t0 + Duration::from_secs(16)));
    }

    #[test]
    fn link_states_connecting_then_reconnecting_after_drop() {
        let t0 = Instant::now();
        let mut host = idle_host("a");
        assert_eq!(host.link(t0), CardLink::Connecting);
        host.on_ended(t0);
        assert_eq!(host.link(t0), CardLink::Reconnecting);
        assert_eq!(host.retry_at, Some(t0 + BACKOFF_BASE));
        host.on_ended(t0);
        assert_eq!(host.retry_at, Some(t0 + BACKOFF_BASE * 2));
    }

    #[test]
    fn report_resets_backoff_so_next_drop_retries_fast() {
        let t0 = Instant::now();
        let mut host = idle_host("a");
        host.on_ended(t0);
        host.on_ended(t0);
        host.on_report(Box::new(parse_line(&golden_line()).unwrap()), t0);
        host.on_ended(t0);
        assert_eq!(host.retry_at, Some(t0 + BACKOFF_BASE));
    }

    #[test]
    fn remote_text_is_made_printable_where_it_enters() {
        let line = golden_line()
            .replacen("Apple M5 Pro", "Apple\\u001b]2;pwn\\u0007 M5", 1)
            .replacen("WindowServer", "Window\\u202eServer", 1)
            .replacen("All normal", "All\\u009b31m normal", 1)
            .replacen("\"cpu\":\"ok\"", "\"cpu\":\"o\\u0008k\"", 1);
        let report = parse_line(&line).unwrap();
        assert_eq!(report.soc.name, "Apple\u{FFFD}]2;pwn\u{FFFD} M5");
        assert_eq!(report.processes[1].name, "Window\u{FFFD}Server");
        assert_eq!(report.verdict.headline, "All\u{FFFD}31m normal");
        assert_eq!(
            report.health.get("cpu").map(std::borrow::Cow::as_ref),
            Some("o\u{FFFD}k")
        );
        let host = Host::new(String::from("evil\u{1b}[2J"), Source::Idle);
        assert_eq!(host.card(Instant::now()).host, "evil\u{FFFD}[2J");
        assert_eq!(host.name, "evil\u{1b}[2J", "ssh still gets what was typed");
    }

    #[test]
    fn history_is_bounded() {
        let t0 = Instant::now();
        let mut host = idle_host("a");
        let report = parse_line(&golden_line()).unwrap();
        for _ in 0..HISTORY_LEN + 20 {
            let copy = parse_line(&serde_json::to_string(&report).unwrap()).unwrap();
            host.on_report(Box::new(copy), t0);
        }
        assert_eq!(host.cpu.len(), HISTORY_LEN);
    }

    #[test]
    fn card_maps_report_without_inventing_absent_hardware() {
        let t0 = Instant::now();
        let mut host = idle_host("studio");
        host.on_report(Box::new(parse_line(&golden_line()).unwrap()), t0);
        let card = host.card(t0);
        assert_eq!(card.chip, Some("Apple M5 Pro"));
        assert_eq!(card.clocks, vec![("P", 2748)], "super has no clock yet");
        assert_eq!(card.battery, None, "golden is a desktop");
        assert_eq!(card.power_w, None, "no rails measured");
        assert_eq!(card.peak_c, Some(46.0));
        assert_eq!(card.pressure, Some(Pressure::Nominal));
        assert_eq!(host.procs().len(), 2);
    }

    #[test]
    fn launcher_puts_host_after_double_dash() {
        let args = Launcher::args("-oProxyCommand=evil");
        let dash = args.iter().position(|a| a == "--").unwrap();
        let host = args
            .iter()
            .position(|a| a == "-oProxyCommand=evil")
            .unwrap();
        assert!(dash < host);
        assert!(args.contains(&String::from("BatchMode=yes")));
        assert_eq!(
            &args[host + 1..],
            ["siltop", "--json-stream", "--procs", "30"]
        );
    }

    #[test]
    fn view_keys_move_open_and_quit() {
        let mut view = View::default();
        assert_eq!(view.key(KeyCode::Right, false, 4, 2), Step::Stay);
        assert_eq!(view.selected, 1);
        view.key(KeyCode::Down, false, 4, 2);
        assert_eq!(view.selected, 3);
        view.key(KeyCode::Down, false, 4, 2);
        assert_eq!(view.selected, 3, "no row below: stays put");
        view.key(KeyCode::Enter, false, 4, 2);
        assert!(view.procs_open);
        assert_eq!(
            view.key(KeyCode::Esc, false, 4, 2),
            Step::Stay,
            "esc closes procs first"
        );
        assert!(!view.procs_open);
        assert_eq!(
            view.key(KeyCode::Esc, false, 4, 2),
            Step::Stay,
            "esc on the grid does not quit"
        );
        assert_eq!(view.key(KeyCode::Char('q'), false, 4, 2), Step::Quit);
        assert_eq!(view.key(KeyCode::Char('c'), true, 4, 2), Step::Quit);
    }

    /// Regression: in a one-row grid Up/Down jumped to the first or last
    /// card; they stay in their column instead.
    #[test]
    fn up_down_in_a_single_row_stay_put() {
        let mut view = View::default();
        view.key(KeyCode::Right, false, 4, 4);
        assert_eq!(view.selected, 1);
        for code in [
            KeyCode::Down,
            KeyCode::Char('j'),
            KeyCode::Up,
            KeyCode::Char('k'),
        ] {
            view.key(code, false, 4, 4);
            assert_eq!(view.selected, 1, "{code:?}");
        }
        let mut view = View::default();
        view.key(KeyCode::Down, false, 3, 2);
        assert_eq!(view.selected, 2, "down a column");
        view.key(KeyCode::Up, false, 3, 2);
        assert_eq!(view.selected, 0, "and back up");
    }

    /// Privacy: the demo's local card never carries this Mac's hostname.
    #[test]
    fn demo_local_card_is_not_named_after_this_mac() {
        assert_eq!(local_name(true), DEMO_LOCAL_NAME);
    }

    #[test]
    fn fake_ssh_streams_reports_through_partial_writes() {
        let line = golden_line();
        let (head, tail) = line.split_at(200);
        let body = format!(
            "echo 'Welcome banner'\nprintf '%s' '{head}'\nsleep 0.05\nprintf '%s\\n' '{tail}'\nexec sleep 30"
        );
        let (launcher, dir) = fake_ssh("stream", &body);
        let mut fleet = Fleet::new(launcher, Duration::from_secs(1));
        let t0 = Instant::now();
        fleet.add_remote("studio", t0);
        wait_for(|| {
            fleet.pump(t0);
            fleet.hosts[0].last.is_some()
        });
        assert_eq!(fleet.hosts[0].link(t0), CardLink::Live);
        let args = std::fs::read_to_string(dir.join("args")).unwrap();
        assert!(
            args.contains("-- studio siltop --json-stream --procs 30"),
            "{args}"
        );
    }

    #[test]
    fn shutdown_kills_and_reaps_ssh_children() {
        let (launcher, dir) = fake_ssh("kill", "exec sleep 30");
        let mut fleet = Fleet::new(launcher, Duration::from_secs(1));
        let t0 = Instant::now();
        fleet.add_remote("a", t0);
        fleet.add_remote("b", t0);
        let pids: Vec<u32> = fleet
            .hosts
            .iter()
            .map(|h| match &h.source {
                Source::Ssh(Some(remote)) => remote.pid(),
                _ => panic!("not started"),
            })
            .collect();
        wait_for(|| dir.join("pid").exists());
        assert!(pids.iter().all(|pid| alive(*pid)));
        fleet.shutdown();
        assert!(fleet.hosts.is_empty());
        assert!(
            pids.iter().all(|pid| !alive(*pid)),
            "children still running"
        );
    }

    /// Regression: `Drop` joined the reader thread, and a grandchild that
    /// still held the pipe (`sh` running a non-exec `sleep`) stalled quit
    /// until it exited.
    #[test]
    fn shutdown_does_not_wait_for_grandchild_holding_pipe() {
        let (launcher, dir) = fake_ssh("grandchild", "sleep 20");
        let mut fleet = Fleet::new(launcher, Duration::from_secs(1));
        fleet.add_remote("a", Instant::now());
        wait_for(|| dir.join("pid").exists());
        let started = Instant::now();
        fleet.shutdown();
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn dropped_link_reconnects_after_backoff_with_fake_clock() {
        let (launcher, dir) = fake_ssh("reconnect", "exit 0");
        let mut fleet = Fleet::new(launcher, Duration::from_secs(1));
        let t0 = Instant::now();
        fleet.add_remote("a", t0);
        assert_eq!(fleet.hosts[0].generation, 1);
        wait_for(|| {
            fleet.pump(t0);
            fleet.hosts[0].retry_at.is_some()
        });
        assert_eq!(fleet.hosts[0].link(t0), CardLink::Reconnecting);
        // Not yet due: no new child.
        fleet.pump(t0 + Duration::from_millis(500));
        assert_eq!(fleet.hosts[0].generation, 1);
        fleet.pump(t0 + BACKOFF_BASE);
        assert_eq!(
            fleet.hosts[0].generation, 2,
            "retried once the delay passed"
        );
        let _ = dir;
    }

    #[test]
    fn missing_ssh_binary_backs_off_instead_of_failing() {
        let launcher = Launcher {
            program: OsString::from("/nonexistent/siltop-test-ssh"),
        };
        let mut fleet = Fleet::new(launcher, Duration::from_secs(1));
        let t0 = Instant::now();
        fleet.add_remote("a", t0);
        assert_eq!(fleet.hosts[0].retry_at, Some(t0 + BACKOFF_BASE));
        assert_eq!(fleet.hosts[0].link(t0), CardLink::Reconnecting);
    }

    #[test]
    fn stale_reader_generation_is_ignored() {
        let (launcher, _dir) = fake_ssh("gen", "exec sleep 30");
        let mut fleet = Fleet::new(launcher, Duration::from_secs(1));
        let t0 = Instant::now();
        fleet.add_remote("a", t0);
        let report = parse_line(&golden_line()).unwrap();
        fleet
            .tx
            .send(FleetEvent::Report(0, 0, Box::new(report)))
            .unwrap();
        fleet.tx.send(FleetEvent::Ended(0, 0)).unwrap();
        fleet.pump(t0);
        assert!(fleet.hosts[0].last.is_none());
        assert!(fleet.hosts[0].retry_at.is_none());
    }
}
