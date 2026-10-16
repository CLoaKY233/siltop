use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use siltop_core::{
    Config, Error, Histories, PROC_RATIO_MAX, PROC_RATIO_MIN, ProcSort, Result, SampleClock,
    Snapshot, Stamp, Surface, VerdictEngine, Zoom, auto_surface,
};
use siltop_metrics::{
    ActionError, KillTarget, Needs, Signal, can_signal, is_critical, nice, send_signal_all,
    set_background, set_nice,
};
use siltop_ui::{
    Anchor, AppView, DossierTab, Focus, HelpGroup, Hit, LayoutFlags, Nesting, Panel, ProcView,
    SettingStep, SettingsCategory, SettingsHit, SettingsState, SignalMenuView, SignalRow, Theme,
    ToastKind, detail_actions, detail_close, detail_rect, detail_tabs, footer_hit, help_max_scroll,
    help_rect, hit_at, hop_hit, hop_ring, interval_label, plan, proc_rows, render_app,
    reveal_swatch, scroll_swatches, settings_hit, signal_menu_row_at, theme_options, work_fits,
};

use crate::config_io::{self, ConfigStore};
use crate::event;
use crate::keymap::{self, Action, Mode, Profile};
use crate::msg::{AppMsg, Pacer};
use crate::signals::{self, Shutdown};
use crate::source::SnapshotSource;
use crate::tui::{self, AppTerminal};
use crate::worker;

mod menus;
mod timeline;

/// What a full-screen page (settings, the timeline) needs sampled: what
/// the history keeps recording underneath it.
const PAGE_NEEDS: Needs = Needs::GPU_CLOCKS.union(Needs::POWER);

const STATUS_TTL: Duration = Duration::from_secs(4);
/// Interval changes flash briefly: the footer segment already shows the rate.
const INTERVAL_TOAST_TTL: Duration = Duration::from_millis(1500);
/// The toast a replay shows instead of signalling a recorded pid.
#[cfg(test)]
const REPLAY_READ_ONLY: &str = "replay: process actions are off";
/// The replay scrub bar stays up; every frame and control refreshes it.
const REPLAY_TOAST_TTL: Duration = Duration::from_secs(3600);
/// Rows a help page scrolls.
const HELP_PAGE: i32 = 10;
/// Far enough to reach either end of any process list.
const LIST_END: i32 = 1_000_000;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const WHEEL_ROWS: i32 = 3;
/// Why a process key does nothing on a view without the table.
const NO_LIST: &str = "no process list on this view";

/// The open signal picker: whom it targets and which row is under the
/// cursor. Rows are dimmed up front when the kernel would refuse.
#[derive(Debug, Clone)]
struct SignalMenu {
    target: KillTarget,
    /// Everyone the picker signals: the tagged set or an app group's
    /// members, `target` first. Empty for a single process.
    set: Vec<KillTarget>,
    label: String,
    rows: Vec<SignalRow>,
    selected: usize,
    note: Option<String>,
}

/// The session shows this Mac, now: neither the demo's canned load nor a
/// replayed trace. Only then may snapshots reach Notification Center, a
/// hook, or the saved history.
fn this_mac_now(demo: bool, source: &SnapshotSource) -> bool {
    !demo && source.player().is_none()
}

fn watch_for_updates(app: &App, tx: &std::sync::mpsc::Sender<crate::msg::AppMsg>, live: bool) {
    if !live {
        return;
    }
    let tx_update = tx.clone();
    let opted_in = app.config.updates_check;
    let auto_install = app.config.updates_auto_install;
    std::thread::spawn(move || {
        let launch = crate::onboarding::Launch::current(crate::cli::Mode::Tui, false);
        let Some(manifest) = crate::update::automatic(opted_in, launch) else {
            return;
        };
        let exe = std::env::current_exe().ok();
        let kind = exe.as_deref().map_or(
            crate::update::InstallKind::Other,
            crate::update::install_kind,
        );
        let newer = crate::update::is_newer(crate::cli::VERSION, &manifest.version);
        if crate::update::plan_check(auto_install, kind, newer) == crate::update::CheckPlan::Install
            && let Some(exe) = exe
        {
            match crate::update::install(&exe, &manifest) {
                Ok(()) => {
                    let _ = tx_update.send(crate::msg::AppMsg::UpdateInstalled {
                        version: manifest.version,
                    });
                }
                Err(error) => {
                    let _ = tx_update.send(crate::msg::AppMsg::UpdateFailed {
                        message: error.to_string(),
                    });
                }
            }
            return;
        }
        let _ = tx_update.send(crate::msg::AppMsg::Update {
            version: manifest.version,
            url: manifest.url,
            sha256: manifest.sha256,
            notes: manifest.notes,
            brew: kind == crate::update::InstallKind::Brew,
        });
    });
}

fn show_startup_note(app: &mut App, warning: Option<String>, live: bool) {
    if let Some(warning) = warning {
        app.set_toast(warning, ToastKind::Warn, STATUS_TTL);
    } else if app.first_run.detected() {
        app.set_toast(
            String::from(crate::first_run::WELCOME),
            ToastKind::Info,
            crate::first_run::WELCOME_TTL,
        );
    }
    if live && crate::crash::take().is_some() {
        app.set_toast(
            String::from(crate::crash::TOAST),
            ToastKind::Warn,
            STATUS_TTL,
        );
    }
}

pub fn run(cli: &crate::cli::Cli) -> Result<()> {
    tui::install_panic_hook();
    crate::term_caps::probe();
    let path = cli.config.clone().or_else(config_io::default_path);
    let shown_path = path.as_ref().map(|p| p.display().to_string());
    let first_run = crate::first_run::FirstRun::detect(path.as_deref(), cli.demo, Instant::now());
    let sealed_path = path.clone().filter(|_| first_run.detected());
    let (mut store, config, load_warning) = if let Some(path) = path {
        ConfigStore::open(&path)
    } else {
        let (store, config) = ConfigStore::disabled();
        (store, config, None)
    };
    let replay = match (cli.mode, cli.file.as_deref()) {
        (crate::cli::Mode::Replay, Some(path)) => Some(crate::replay::load(path, Instant::now())?),
        _ => None,
    };
    let mut session = crate::with_cli_overrides(config.clone(), cli);
    Profile::apply_session(&mut session, std::env::var("SILTOP_KEYS").ok().as_deref());
    if let Some((trace, _)) = &replay {
        session.interval = crate::replay::interval(trace);
    }
    let overrides = config_io::SessionOverrides::between(&config, &session);
    let config = session;
    tui::set_mouse(!cli.no_mouse);
    let (tx, rx) = channel();
    let shutdown = signals::install(tx.clone())?;
    signals::install_job_control(tx.clone())?;
    let (source, replay_warning) = if let Some((trace, warning)) = replay {
        let player = crate::replay::Player::new(trace, Instant::now());
        (SnapshotSource::Replay(Box::new(player)), warning)
    } else if cli.demo {
        let step = config.interval;
        let worker =
            worker::spawn_power(config.interval, config.low_power, tx.clone(), move || {
                Ok(crate::demo::Demo::new(step))
            })?;
        (SnapshotSource::Live(worker), None)
    } else {
        let worker = worker::spawn(config.interval, config.low_power, tx.clone())?;
        (SnapshotSource::Live(worker), None)
    };
    let mut app = App::with_source(config, source);
    // Neither the demo nor a replayed trace lands in this Mac's own history.
    let live = this_mac_now(cli.demo, &app.source);
    let (recorder, history_warning) = if live {
        crate::store_disk::Recorder::new(crate::store_disk::default_root().as_deref())
    } else {
        (crate::store_disk::Recorder::session(), replay_warning)
    };
    app.recorder = recorder;
    app.blank_first_cpu = live;
    app.demo = cli.demo;
    app.watch = live.then(|| crate::sinks::Watch::new(&app.config.alerts, true));
    app.overrides = overrides;
    app.config_path = shown_path;
    app.about = crate::cli::build_facts();
    app.about.push(crate::state_row());
    app.first_run = first_run;
    app.first_run.use_profile(app.profile);
    app.update_tx = Some(tx.clone());
    watch_for_updates(&app, &tx, live);
    show_startup_note(&mut app, load_warning.or(history_warning), live);
    if let Some(theme) = cli.theme.as_deref().and_then(siltop_ui::Theme::by_name) {
        let icons = app.theme.icons;
        app.theme = crate::term_caps::fit(theme, &app.config).with_icons(icons);
    }
    if let Some(profile) = std::env::var("SILTOP_KEYS")
        .ok()
        .as_deref()
        .and_then(Profile::by_name)
    {
        app.set_profile(profile);
    }
    tui::set_ansi16(app.theme.depth == siltop_ui::ColorDepth::Ansi16);
    let mut terminal = tui::install()?;
    let mut prefs = Prefs {
        store: &mut store,
        shutdown: &shutdown,
    };
    let result =
        event::spawn_input(tx).and_then(|()| run_loop(&mut terminal, &mut app, &rx, &mut prefs));
    let restored = tui::restore();
    let warning = store.flush(&app.prefs());
    // The first run leaves a config file so the welcome never repeats; a
    // read-only home just means it shows again, which is harmless.
    if let Some(path) = sealed_path {
        let _ = crate::first_run::seal(&path);
    }
    result.and(restored)?;
    if let Some(warning) = warning {
        // The terminal may be gone (window closed): a failed write is not worth a panic.
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), "{warning}");
    }
    Ok(())
}

/// The config file and the shutdown flag, as the loop sees them.
struct Prefs<'a> {
    store: &'a mut ConfigStore,
    shutdown: &'a Shutdown,
}

/// Blocks until a message or the next deadline, applies everything queued,
/// then draws at most once. Nothing changed means nothing is drawn.
fn run_loop(
    terminal: &mut AppTerminal,
    app: &mut App,
    rx: &Receiver<AppMsg>,
    prefs: &mut Prefs,
) -> Result<()> {
    loop {
        let deadline = match (app.next_deadline(), prefs.store.deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let Some(first) = next_msg(rx, deadline) else {
            return Ok(());
        };
        let now = Instant::now();
        drain(app, first, rx, now);
        if let Some(err) = app.fatal.take() {
            return Err(err);
        }
        if app.quit || prefs.shutdown.requested() {
            return Ok(());
        }
        if let Some(warning) = app.sync_prefs(prefs.store, now) {
            app.set_toast(warning, ToastKind::Warn, STATUS_TTL);
        }
        if std::mem::take(&mut app.suspend) {
            tui::suspend()?;
            app.repaint = true;
            app.pacer.mark();
        }
        if std::mem::take(&mut app.repaint) {
            tui::reenter()?;
            terminal
                .clear()
                .map_err(|err| Error::terminal(err.to_string()))?;
        }
        if app.pacer.due(now) {
            terminal
                .draw(|frame| app.draw(frame))
                .map_err(|err| Error::terminal(err.to_string()))?;
            if let Some(seq) = app.clipboard.take() {
                // A terminal without OSC 52 ignores it; nothing to report.
                let _ = crate::osc::emit(&mut std::io::stdout(), &seq);
            }
            if !app.alert_out.is_empty() {
                let _ = crate::osc::emit(&mut std::io::stdout(), &app.alert_out);
                app.alert_out.clear();
            }
            app.pacer.drawn(now);
        }
    }
}

fn next_msg(rx: &Receiver<AppMsg>, deadline: Option<Instant>) -> Option<AppMsg> {
    let Some(at) = deadline else {
        return rx.recv().ok();
    };
    match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
        Ok(msg) => Some(msg),
        Err(RecvTimeoutError::Timeout) => Some(AppMsg::Timer),
        Err(RecvTimeoutError::Disconnected) => None,
    }
}

fn drain(app: &mut App, first: AppMsg, rx: &Receiver<AppMsg>, now: Instant) {
    app.update(first, now);
    while let Ok(msg) = rx.try_recv() {
        app.update(msg, now);
    }
}

/// The layers that sit over the dashboard, topmost first in
/// [`App::top_overlay`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Overlay {
    Update,
    Confirm,
    SignalMenu,
    Help,
    Menu,
    Timeline,
    Settings,
}

#[derive(Clone)]
struct PendingUpdate {
    version: String,
    url: String,
    sha256: String,
    notes: String,
    brew: bool,
}

struct App {
    theme: siltop_ui::Theme,
    /// The button under the pointer, lit; `None` off any button.
    hover: Option<Rect>,
    config: Config,
    source: SnapshotSource,
    pacer: Pacer,
    now: Instant,
    quit: bool,
    fatal: Option<Error>,
    snapshot: Snapshot,
    histories: Histories,
    surface: Surface,
    focus: Focus,
    expanded: Option<Panel>,
    proc: ProcView,
    last_click: Option<(Instant, u32)>,
    help: bool,
    /// The open popup menu: the main menu or a process row's.
    menu: Option<menus::MenuState>,
    /// The Timeline page, while open.
    timeline: Option<siltop_ui::TimelineView>,
    /// The unix second a parked Timeline cursor points at, kept exactly so
    /// re-binning never rounds it (see `refresh_timeline`).
    timeline_parked: Option<u64>,
    /// Judges each snapshot for the footer's verdict slot (as `--diagnose`).
    verdict: VerdictEngine,
    /// The full-screen settings page, when open.
    settings: Option<SettingsState>,
    /// Shown on the settings page's about section.
    config_path: Option<String>,
    /// Build and state-path rows for the about section.
    about: Vec<(&'static str, String)>,
    /// The first launch's key hint, shown in the verdict slot.
    first_run: crate::first_run::FirstRun,
    confirm_kill: bool,
    /// The armed kill's pid and start time, so a reused pid is refused.
    confirm: Option<KillTarget>,
    /// Every target of the armed confirm when it covers more than one
    /// process (tags, an app group); `confirm` is its first.
    confirm_set: Vec<KillTarget>,
    detail_pid: Option<u32>,
    searching: bool,
    frozen: bool,
    ready: bool,
    status: Option<(String, Instant)>,
    status_kind: ToastKind,
    status_ttl: Duration,
    /// The toast slot holds the replay scrub bar, which a new frame may
    /// refresh; any other toast keeps the slot until it expires.
    status_replay: bool,
    profile: Profile,
    help_groups: Vec<HelpGroup>,
    help_scroll: u16,
    paused_at: Option<String>,
    dirty_prefs: bool,
    pending_signal: Signal,
    signal_menu: Option<SignalMenu>,
    /// Processes we moved to the background band this session.
    backgrounded: std::collections::HashSet<KillTarget>,
    /// An OSC 52 sequence waiting to be written after the next draw.
    clipboard: Option<String>,
    last_area: ratatui::layout::Rect,
    proc_ratio: u16,
    dragging_split: bool,
    /// `g` / `w` pick a surface for this session only.
    surface_lock: Option<Surface>,
    /// CLI flags that must never reach the config file.
    overrides: config_io::SessionOverrides,
    /// `z` graph zoom, `[compact, expanded]`; `None` = that view's default
    /// (the shared live window compact, 5m expanded).
    zoom: [Option<Zoom>; 2],
    /// Stamps every snapshot on one clock so all graphs share a time axis.
    sample_clock: SampleClock,
    epoch: Instant,
    clock: Option<Stamp>,
    /// Long history: segment files on disk, else session memory.
    recorder: crate::store_disk::Recorder,
    /// Alert rules while the dashboard is open; `None` for the demo and tests.
    watch: Option<crate::sinks::Watch>,
    /// Terminal notifications (OSC 9 / 777, bell) waiting for the next draw.
    alert_out: String,
    /// The last [`Needs`] sent to the worker.
    needs: Needs,
    /// The worker runs its low-power cadences (battery saver), so the tick
    /// is at least [`siltop_metrics::LOW_POWER_TICK`].
    low_power: bool,
    /// The sampler thread is gone: the numbers on screen are its last.
    stopped: bool,
    /// `--demo`: the rows are canned, so their pids name nothing here.
    demo: bool,
    /// Footer badge once a newer version is known.
    update_badge: Option<String>,
    update_offer: Option<PendingUpdate>,
    update_dialog: bool,
    update_focus: usize,
    update_tx: Option<std::sync::mpsc::Sender<crate::msg::AppMsg>>,
    /// Ctrl-Z or SIGTSTP: the loop gives the terminal back and stops.
    suspend: bool,
    /// Back from a stop: the loop repaints the whole screen.
    repaint: bool,
    /// Process sweeps applied so far; the first one's CPU is not a reading.
    proc_sweeps: u32,
    /// Blank the first sweep's CPU: set for a live sampler, whose first
    /// sweep spans only the priming instant.
    blank_first_cpu: bool,
}

impl App {
    fn with_source(config: Config, source: SnapshotSource) -> Self {
        let now = Instant::now();
        let proc_ratio = config.proc_ratio;
        let interval = config.interval;
        let profile = Profile::by_name(&config.keymap).unwrap_or_default();
        Self {
            theme: crate::term_caps::startup_theme(&config),
            hover: None,
            config,
            source,
            pacer: Pacer::new(now),
            now,
            quit: false,
            fatal: None,
            snapshot: Snapshot::empty(),
            histories: Histories::new(interval),
            surface: Surface::Work,
            focus: Focus::Processes,
            expanded: None,
            proc: ProcView::default(),
            last_click: None,
            help: false,
            menu: None,
            timeline: None,
            timeline_parked: None,
            verdict: VerdictEngine::default(),
            settings: None,
            config_path: None,
            about: Vec::new(),
            first_run: crate::first_run::FirstRun::default(),
            confirm_kill: false,
            confirm: None,
            confirm_set: Vec::new(),
            detail_pid: None,
            searching: false,
            frozen: false,
            ready: false,
            status: None,
            status_kind: ToastKind::Info,
            status_ttl: STATUS_TTL,
            status_replay: false,
            profile,
            help_groups: keymap::help_groups(profile),
            help_scroll: 0,
            zoom: [None; 2],
            sample_clock: SampleClock::default(),
            epoch: now,
            clock: None,
            recorder: crate::store_disk::Recorder::session(),
            watch: None,
            alert_out: String::new(),
            needs: Needs::ALL,
            low_power: false,
            stopped: false,
            demo: false,
            paused_at: None,
            dirty_prefs: false,
            pending_signal: Signal::Term,
            signal_menu: None,
            backgrounded: std::collections::HashSet::new(),
            clipboard: None,
            last_area: ratatui::layout::Rect::new(0, 0, 80, 24),
            proc_ratio,
            dragging_split: false,
            surface_lock: None,
            overrides: config_io::SessionOverrides::default(),
            update_badge: None,
            update_offer: None,
            update_dialog: false,
            update_focus: 0,
            update_tx: None,
            suspend: false,
            repaint: false,
            proc_sweeps: 0,
            blank_first_cpu: false,
        }
    }

    fn update(&mut self, msg: AppMsg, now: Instant) {
        self.now = now;
        match msg {
            AppMsg::Input(ev) => {
                if matches!(ev, ratatui::crossterm::event::Event::Key(_))
                    && self.first_run.dismiss()
                {
                    self.pacer.mark();
                }
                let action = self
                    .replay_key(&ev)
                    .or_else(|| event::map_event(&ev, self.mode(), self.profile));
                match action {
                    // Motion draws only when the lit button changes.
                    Some(Action::Hover { col, row }) => {
                        if self.hover_to(col, row) {
                            self.pacer.mark();
                        }
                    }
                    Some(action) => {
                        // Whatever the action changes, the lit rect may be stale.
                        self.hover = None;
                        self.reduce(action);
                        self.pacer.mark();
                    }
                    None => {}
                }
            }
            AppMsg::InputClosed(err) => self.fatal = Some(Error::terminal(err)),
            AppMsg::Snapshot(result) => match *result {
                // Pausing the display never pauses history.
                Ok(ref snapshot) if self.frozen => self.record(snapshot),
                Ok(snapshot) => {
                    self.apply_snapshot(snapshot);
                    self.pacer.release();
                    self.pacer.mark();
                }
                Err(err) => {
                    self.set_toast(err.to_string(), ToastKind::Error, STATUS_TTL);
                    self.pacer.release();
                }
            },
            AppMsg::Stopped(err) => {
                self.stopped = true;
                self.set_toast(err.to_string(), ToastKind::Error, STATUS_TTL);
                self.pacer.release();
                self.pacer.mark();
            }
            AppMsg::LowPower(low) => {
                self.low_power = low;
                self.histories.set_interval(self.cadence());
                self.pacer.mark();
            }
            AppMsg::Timer => {}
            AppMsg::Suspend => self.suspend = true,
            AppMsg::Continued => {
                self.repaint = true;
                self.pacer.mark();
            }
            AppMsg::Update {
                version,
                url,
                sha256,
                notes,
                brew,
            } => {
                self.update_badge = Some(format!("update {version} available"));
                self.update_offer = Some(PendingUpdate {
                    version,
                    url,
                    sha256,
                    notes,
                    brew,
                });
                self.set_toast(
                    String::from("update available"),
                    ToastKind::Info,
                    STATUS_TTL,
                );
                self.pacer.mark();
            }
            AppMsg::UpdateInstalled { version } => {
                self.update_badge = None;
                self.update_offer = None;
                self.update_dialog = false;
                self.set_toast(
                    format!("Updated to {version}, restart to use it"),
                    ToastKind::Info,
                    STATUS_TTL,
                );
                self.pacer.mark();
            }
            AppMsg::UpdateFailed { message } => {
                self.set_toast(message, ToastKind::Error, STATUS_TTL);
                self.pacer.mark();
            }
        }
        self.play_due(now);
        self.settle_procs();
        self.expire(now);
        self.sync_needs();
    }

    /// What the visible views read from the costly samplers: a full-screen
    /// page (settings, the timeline) shows no live clocks, so CPU residency
    /// parks until the dashboard is back. What the history records stays
    /// on: GPU residency *is* its GPU figure (a held one would draw a flat
    /// line, at about 1 ms a window), and parked rails would age out and
    /// leave CPU and GPU watt gaps in the very history the timeline shows
    /// (watts alone sample PMGR only every 8 s).
    fn wanted_needs(&self) -> Needs {
        if self.settings.is_some() || self.timeline.is_some() {
            PAGE_NEEDS
        } else {
            Needs::ALL
        }
    }

    fn sync_needs(&mut self) {
        let needs = self.wanted_needs();
        if needs != self.needs {
            self.needs = needs;
            self.source.set_needs(needs);
        }
    }

    /// The topmost open overlay, the one place their stacking order is
    /// written: keys, clicks, the wheel, Esc and `q` all route through it.
    fn top_overlay(&self) -> Option<Overlay> {
        if self.update_dialog {
            Some(Overlay::Update)
        } else if self.confirm_kill {
            Some(Overlay::Confirm)
        } else if self.signal_menu.is_some() {
            Some(Overlay::SignalMenu)
        } else if self.help {
            Some(Overlay::Help)
        } else if self.menu.is_some() {
            Some(Overlay::Menu)
        } else if self.settings.is_some() {
            // settings before the timeline: a page opened from the timeline
            // sits on top of it
            Some(Overlay::Settings)
        } else if self.timeline.is_some() {
            Some(Overlay::Timeline)
        } else {
            None
        }
    }

    /// The mode keys are read in; the topmost overlay wins.
    fn mode(&self) -> Mode {
        match self.top_overlay() {
            Some(Overlay::Update) => Mode::Update,
            Some(Overlay::Confirm) => Mode::Confirm,
            Some(Overlay::SignalMenu) => Mode::SignalMenu,
            Some(Overlay::Help) => Mode::Help,
            Some(Overlay::Menu) => Mode::Menu,
            Some(Overlay::Timeline) => Mode::Timeline,
            Some(Overlay::Settings) if self.settings.as_ref().is_some_and(|s| s.filtering) => {
                Mode::SettingsFilter
            }
            Some(Overlay::Settings) => Mode::Settings,
            None if self.searching => Mode::Search,
            None if self.detail_pid.is_some() => Mode::Detail,
            None => Mode::Normal,
        }
    }

    /// The session profile's key for a Normal-mode `action`, for toasts
    /// that teach one (`?` when the profile leaves it unbound).
    fn key_label(&self, action: Action) -> String {
        keymap::key_for(Mode::Normal, self.profile, action).unwrap_or_else(|| String::from("?"))
    }

    fn set_profile(&mut self, profile: Profile) {
        self.profile = profile;
        self.help_groups = keymap::help_groups(profile);
        self.first_run.use_profile(profile);
    }

    fn next_deadline(&self) -> Option<Instant> {
        let toast = self.status.as_ref().map(|(_, at)| *at + self.status_ttl);
        [
            self.pacer.deadline(),
            toast,
            self.first_run.deadline(),
            self.source.deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Replay keys win over Normal ones only while no overlay is open.
    fn replay_key(&self, ev: &ratatui::crossterm::event::Event) -> Option<Action> {
        match ev {
            ratatui::crossterm::event::Event::Key(key)
                if self.source.player().is_some() && self.mode() == Mode::Normal =>
            {
                keymap::resolve_replay(*key)
            }
            _ => None,
        }
    }

    /// Applies every replay frame due by `now` and refreshes the scrub bar.
    fn play_due(&mut self, now: Instant) {
        let due = self.source.due(now);
        let moved = !due.is_empty();
        for snapshot in due {
            self.apply_snapshot(snapshot);
        }
        if moved {
            self.pacer.release();
            if self.status.is_none() || self.status_replay {
                self.show_replay(now);
            }
        }
    }

    fn show_replay(&mut self, now: Instant) {
        if let Some(line) = self.source.player().map(|p| p.status(now)) {
            self.set_toast(line, ToastKind::Info, REPLAY_TOAST_TTL);
            self.status_replay = true;
        }
    }

    fn replay_control(&mut self, action: Action) {
        let now = self.now;
        let Some(player) = self.source.player_mut() else {
            return;
        };
        match action {
            Action::ReplaySpeed(faster) => player.step_speed(faster, now),
            Action::ReplayScrub(parts) => {
                if let Some(snapshot) = player.scrub(parts, now) {
                    self.apply_snapshot(snapshot);
                }
            }
            _ => {}
        }
        self.show_replay(now);
    }

    fn expire(&mut self, now: Instant) {
        if self.first_run.expire(now) {
            self.pacer.mark();
        }
        if self
            .status
            .as_ref()
            .is_some_and(|(_, at)| now >= *at + self.status_ttl)
        {
            self.status = None;
            self.pacer.mark();
            // a paused or finished replay gets no frames to bring it back
            self.show_replay(now);
        }
    }

    fn set_interval(&mut self, interval: std::time::Duration) {
        self.config.interval = interval;
        let cadence = self.cadence();
        self.histories.set_interval(cadence);
        self.source.set_interval(interval);
        self.touch();
        let ms = u64::try_from(cadence.as_millis()).unwrap_or(u64::MAX);
        let saver = if cadence > interval {
            " · battery saver"
        } else {
            ""
        };
        self.set_toast(
            format!("sampling every {}{saver}", interval_label(ms)),
            ToastKind::Info,
            INTERVAL_TOAST_TTL,
        );
    }

    /// The interval snapshots really arrive at: the chosen one, stretched
    /// while the worker runs its battery saver cadences.
    fn cadence(&self) -> std::time::Duration {
        siltop_metrics::tick_interval(self.config.interval, self.low_power)
    }

    fn touch(&mut self) {
        self.dirty_prefs = true;
    }

    /// The config this session runs on, with the live split ratio.
    fn live_config(&self) -> Config {
        Config {
            proc_ratio: self.proc_ratio,
            ..self.config.clone()
        }
    }

    /// What gets persisted: the live config minus anything only a CLI flag
    /// set. The session-only surface override is deliberately not part of it.
    fn prefs(&self) -> Config {
        self.overrides.persistable(self.live_config())
    }

    /// Restarts the save debounce after a change and saves once it runs out.
    fn sync_prefs(&mut self, store: &mut ConfigStore, now: Instant) -> Option<String> {
        if std::mem::take(&mut self.dirty_prefs) {
            store.mark(now);
        }
        store.poll(now, &self.prefs())
    }

    /// Folds a snapshot into the open history bucket; cheap every tick, one
    /// store append per 6 s.
    fn record(&mut self, snapshot: &Snapshot) {
        let now = crate::store_disk::unix_now();
        if let Some(warning) = self.recorder.observe(snapshot, now) {
            self.set_toast(warning, ToastKind::Warn, STATUS_TTL);
        }
        self.tick_timeline(now);
    }

    fn apply_snapshot(&mut self, mut snapshot: Snapshot) {
        self.record(&snapshot);
        if snapshot.cpu.temp_c.is_none() {
            snapshot.cpu.temp_c = snapshot.sensors.cpu_c;
        }
        if let Some(gpu) = snapshot.gpu.as_mut()
            && gpu.temp_c.is_none()
        {
            gpu.temp_c = snapshot.sensors.gpu_c;
        }
        if snapshot.sampled.procs {
            self.proc_sweeps = self.proc_sweeps.saturating_add(1);
            // The first sweep's CPU spans only the priming instant, when
            // siltop itself is busy starting up (it read ~140%): show
            // `—` until a second sweep measures a real interval.
            if self.proc_sweeps == 1 && self.blank_first_cpu {
                for process in &mut snapshot.processes {
                    process.cpu = None;
                }
            }
        }
        let exited = self.watched_exit(&snapshot);
        let lost = self.confirm_lost(&snapshot);
        if snapshot.sampled.procs {
            self.proc.generation += 1;
            if self.detail_pid.is_some() && self.proc.tab.inspects() {
                self.refresh_inspection();
            }
            self.proc.prune(&snapshot.processes);
            let alive = |t: &KillTarget| snapshot.processes.iter().any(|p| KillTarget::of(p) == *t);
            self.confirm_set.retain(alive);
        }
        self.snapshot = snapshot;
        let drain = siltop_core::battery_drain_w(&self.snapshot.battery);
        self.verdict.update(&self.snapshot, self.now, drain);
        if let Some(watch) = self.watch.as_mut() {
            for delivery in watch.observe(&self.config.alerts, &self.snapshot, self.now) {
                delivery.send();
                self.alert_out.extend(delivery.terminal);
            }
        }
        let at = self.sample_clock.stamp(
            Instant::now().saturating_duration_since(self.epoch),
            self.cadence(),
        );
        self.clock = Some(at);
        self.histories.push_snapshot(&self.snapshot, at);
        self.ready = true;
        if let Some((pid, notice)) = exited {
            if self.detail_pid == Some(pid) {
                self.detail_pid = None;
            }
            if self.proc.anchor == Anchor::Pinned(pid) {
                self.proc.reset_top();
            }
            self.set_toast(notice, ToastKind::Warn, STATUS_TTL);
        }
        if let Some(notice) = lost {
            self.cancel_kill();
            self.set_toast(notice, ToastKind::Warn, STATUS_TTL);
        }
        self.settle_procs();
    }

    /// The armed kill's target, if the new sample no longer lists it (or
    /// lists its pid with another start time: the pid was reused).
    fn confirm_lost(&self, next: &Snapshot) -> Option<String> {
        let target = self.confirm.filter(|_| next.sampled.procs)?;
        let alive = next.processes.iter().any(|p| KillTarget::of(p) == target);
        (!alive).then(|| {
            let pid = target.pid;
            format!("{} ({pid}) exited; kill cancelled", self.proc_name(pid))
        })
    }

    /// The last sample's name for `pid`, for notices.
    fn proc_name(&self, pid: u32) -> &str {
        self.snapshot
            .processes
            .iter()
            .find(|p| p.pid == pid)
            .map_or("process", |p| p.name.as_str())
    }

    /// The dossier's or the pinned row's process, if the new sample lost it.
    fn watched_exit(&self, next: &Snapshot) -> Option<(u32, String)> {
        if !next.sampled.procs {
            return None;
        }
        let pinned = match self.proc.anchor {
            Anchor::Pinned(pid) => Some(pid),
            Anchor::Top => None,
        };
        let pid = self.detail_pid.or(pinned)?;
        if next.processes.iter().any(|p| p.pid == pid) {
            return None;
        }
        Some((pid, format!("{} ({pid}) exited", self.proc_name(pid))))
    }

    fn sync_model(&mut self) {
        self.proc.sync(
            &self.snapshot.processes,
            self.config.proc_sort,
            self.sort_reversed(),
            self.config.show_tree,
        );
    }

    /// Model current, pinned pid still listed (else back to the top), and
    /// the viewport clamped to the table the last frame drew.
    fn settle_procs(&mut self) {
        self.sync_model();
        if let Anchor::Pinned(pid) = self.proc.anchor
            && self.proc.model.position(pid).is_none()
        {
            self.proc.reset_top();
        }
        self.proc.settle(self.table_rows());
    }

    fn table_rows(&self) -> usize {
        proc_rows(
            self.last_area,
            self.effective_surface(),
            self.layout_flags(),
        )
    }

    /// Every input ends here: keys via the keymap, clicks via `apply_hit`.
    fn reduce(&mut self, action: Action) {
        match action {
            Action::Quit => self.quit = true,
            Action::Suspend => self.suspend = true,
            Action::Resize { cols, rows } => {
                self.last_area = Rect::new(0, 0, cols, rows);
                self.drop_hidden_detail();
                self.refresh_timeline(crate::store_disk::unix_now());
            }
            Action::Click { col, row } => {
                // a press always starts fresh: a release lost to an overlay
                // or outside the window must not leave a drag armed
                self.dragging_split = false;
                self.on_click(col, row);
            }
            Action::Scroll { col, row, delta } => self.on_scroll(col, row, delta),
            Action::ContextMenu { col, row } => {
                self.dragging_split = false;
                self.open_context_menu(col, row);
            }
            Action::Drag { col, row } if self.top_overlay() == Some(Overlay::Timeline) => {
                self.drag_timeline(col, row);
            }
            Action::Drag { col, .. } if !self.overlay_open() => self.drag_split(col),
            Action::Drag { .. } => {}
            Action::MouseUp => self.dragging_split = false,
            Action::Help => self.toggle_help(),
            Action::CheckUpdate => self.open_update_dialog(),
            Action::Cancel => self.cancel_overlay(),
            Action::Close => self.close_overlay(),
            Action::ReplayScrub(_) | Action::ReplaySpeed(_) => self.replay_control(action),
            _ => match self.top_overlay() {
                Some(Overlay::Update) => self.handle_update_dialog(action),
                Some(Overlay::Confirm) => self.handle_confirm(action),
                Some(Overlay::SignalMenu) => self.handle_signal_menu(action),
                Some(Overlay::Help) => self.reduce_help(action),
                Some(Overlay::Menu) => self.handle_menu(action),
                Some(Overlay::Settings) => self.handle_settings(action),
                Some(Overlay::Timeline) => self.handle_timeline(action),
                None => self.reduce_page(action),
            },
        }
    }

    /// Keys with no overlay open: the dossier's own, then the dashboard's.
    fn reduce_page(&mut self, action: Action) {
        match action {
            Action::NextPanel | Action::PrevPanel | Action::DossierTab(_)
                if self.detail_pid.is_some() =>
            {
                self.switch_tab(action);
            }
            Action::Move(_) | Action::Page(_) | Action::Top | Action::Bottom
                if self.detail_pid.is_some() && self.proc.tab.inspects() =>
            {
                self.scroll_page(action);
            }
            _ => self.handle_normal(action),
        }
    }

    /// Tab, shift-tab or a digit in the dossier: pick the page, and read
    /// the pid's threads, files and sockets when that page shows them.
    fn switch_tab(&mut self, action: Action) {
        let tab = self.proc.tab;
        self.proc.tab = match action {
            Action::NextPanel => tab.step(1),
            Action::PrevPanel => tab.step(-1),
            Action::DossierTab(n) => DossierTab::nth(n).unwrap_or(tab),
            _ => tab,
        };
        self.proc.page_offset = 0;
        self.refresh_inspection();
    }

    /// Arrows, page keys and home/end on a threads, files or ports page
    /// scroll its list; the overview keeps walking the process list.
    fn scroll_page(&mut self, action: Action) {
        let Some(pane) = self.proc_pane() else {
            return;
        };
        let step = i64::try_from(self.proc.page_step(pane)).unwrap_or(1);
        let delta = match action {
            Action::Move(d) => i64::from(d),
            Action::Page(p) => i64::from(p).saturating_mul(step),
            Action::Top => i64::MIN,
            Action::Bottom => i64::MAX,
            _ => 0,
        };
        self.proc.scroll_page(delta, pane);
    }

    /// The process pane of the last drawn layout, if it shows one.
    fn proc_pane(&self) -> Option<Rect> {
        plan(
            self.last_area,
            self.effective_surface(),
            self.layout_flags(),
        )
        .processes
    }

    /// On demand, for the open dossier's pid only; nothing is read while
    /// the overview (or no dossier) is showing.
    fn refresh_inspection(&mut self) {
        let Some(pid) = self.detail_pid.filter(|_| self.proc.tab.inspects()) else {
            self.proc.inspection = None;
            self.proc.page_offset = 0;
            return;
        };
        if self.proc.inspection.as_ref().map(|i| i.pid) != Some(pid) {
            self.proc.page_offset = 0;
        }
        // A recorded pid names some other process on this Mac, or none.
        if self.recorded().is_some() {
            self.proc.inspection = Some(siltop_core::Inspection {
                pid,
                recorded: true,
                ..siltop_core::Inspection::default()
            });
            return;
        }
        self.proc.inspection = match siltop_metrics::inspect(pid) {
            Ok(found) => Some(found),
            Err(err) if err.is_not_permitted() => Some(siltop_core::Inspection {
                pid,
                denied: true,
                ..siltop_core::Inspection::default()
            }),
            Err(_) => None,
        };
        // a refresh can shorten the list: keep the window on real rows
        if let Some(pane) = self.proc_pane() {
            self.proc.scroll_page(0, pane);
        }
    }

    fn toggle_help(&mut self) {
        self.menu = None;
        self.help = !self.help;
        self.help_scroll = 0;
    }

    /// A busy verdict's click: select the process it names.
    fn select_verdict(&mut self) {
        let Some(pid) = self.verdict.shown().and_then(|v| v.pid) else {
            return;
        };
        if !self.work_shows() {
            return;
        }
        self.sync_model();
        if self.proc.model.position(pid).is_none() && !self.proc.filter.is_empty() {
            self.proc.filter.clear();
            self.sync_model();
        }
        let Some(index) = self.proc.model.position(pid) else {
            return;
        };
        self.searching = false;
        self.detail_pid = None;
        self.expanded = None;
        self.reveal_processes();
        self.focus = Focus::Processes;
        self.proc.select(index, self.table_rows());
    }

    fn open_update_dialog(&mut self) {
        if self.update_offer.is_none() {
            self.set_toast(
                String::from("no update waiting"),
                ToastKind::Info,
                STATUS_TTL,
            );
            return;
        }
        self.menu = None;
        self.update_focus = 0;
        self.update_dialog = true;
    }

    fn handle_update_dialog(&mut self, action: Action) {
        match action {
            Action::Move(delta) => {
                let next = self.update_focus as i32 + delta;
                self.update_focus = next.rem_euclid(4) as usize;
            }
            Action::Open => self.choose_update(),
            Action::Cancel | Action::Close => self.update_dialog = false,
            _ => {}
        }
    }

    /// The four dialog buttons. "Always" is what turns quiet installs on.
    fn choose_update(&mut self) {
        let Some(offer) = self.update_offer.clone() else {
            self.update_dialog = false;
            return;
        };
        let choice = crate::update::DialogChoice::ALL[self.update_focus];
        let path = crate::update::state_path();
        let mut state = path
            .as_deref()
            .map(crate::update::State::load)
            .unwrap_or_default();
        let effect =
            crate::update::apply_dialog(choice, &offer.version, &mut self.config, &mut state);
        if let Some(path) = path.as_deref() {
            let _ = state.save(path);
        }
        if self.config.updates_auto_install {
            self.touch();
        }
        self.update_dialog = false;
        if matches!(choice, crate::update::DialogChoice::Skip) {
            self.update_badge = None;
            self.update_offer = None;
        }
        if effect.install_now {
            self.start_install(offer);
        }
    }

    fn start_install(&mut self, offer: PendingUpdate) {
        if offer.brew {
            self.set_toast(
                String::from("run brew upgrade siltop"),
                ToastKind::Info,
                STATUS_TTL,
            );
            return;
        }
        let Some(tx) = self.update_tx.clone() else {
            return;
        };
        std::thread::spawn(move || {
            let manifest = crate::update::Manifest {
                version: offer.version.clone(),
                released: String::new(),
                url: offer.url,
                sha256: offer.sha256,
                notes: offer.notes,
            };
            let Ok(exe) = std::env::current_exe() else {
                return;
            };
            match crate::update::install(&exe, &manifest) {
                Ok(()) => {
                    let _ = tx.send(crate::msg::AppMsg::UpdateInstalled {
                        version: manifest.version,
                    });
                }
                Err(error) => {
                    let _ = tx.send(crate::msg::AppMsg::UpdateFailed {
                        message: error.to_string(),
                    });
                }
            }
        });
    }

    /// `q` inside an overlay closes that overlay and nothing else.
    fn close_overlay(&mut self) {
        match self.top_overlay() {
            Some(Overlay::Update) => self.update_dialog = false,
            Some(Overlay::Confirm) => self.cancel_kill(),
            Some(Overlay::SignalMenu) => self.signal_menu = None,
            Some(Overlay::Help) => self.help = false,
            Some(Overlay::Menu) => self.menu = None,
            Some(Overlay::Timeline) => self.timeline = None,
            Some(Overlay::Settings) => self.settings = None,
            None => self.detail_pid = None,
        }
    }

    /// Esc peels one layer: overlay, then the dossier, the filter text, the
    /// search line, the tags and the expanded box.
    fn cancel_overlay(&mut self) {
        match self.top_overlay() {
            Some(Overlay::Settings) => {
                if self.settings.as_mut().is_some_and(SettingsState::cancel) {
                    self.settings = None;
                }
            }
            Some(_) => self.close_overlay(),
            None if self.detail_pid.is_some() => self.close_overlay(),
            None if self.searching => {
                if self.proc.filter.is_empty() {
                    self.searching = false;
                    self.focus = Focus::Processes;
                } else {
                    self.proc.filter.clear();
                    self.proc.reset_top();
                }
            }
            None if !self.proc.filter.is_empty() => {
                self.proc.filter.clear();
                self.focus = Focus::Processes;
                self.proc.reset_top();
            }
            None if !self.proc.tags.is_empty() => self.clear_tags(),
            None if self.expanded.is_some() => self.expanded = None,
            // Nothing left to peel on the plain home screen: Esc is the menu.
            None => self.toggle_menu(),
        }
    }

    fn reduce_help(&mut self, action: Action) {
        let max = i32::from(help_max_scroll(self.last_area, &self.help_groups));
        let current = i32::from(self.help_scroll);
        let next = match action {
            Action::Move(delta) => current.saturating_add(delta),
            Action::Page(delta) => current.saturating_add(delta.saturating_mul(HELP_PAGE)),
            Action::Top => 0,
            Action::Bottom => max,
            _ => return,
        };
        self.help_scroll = u16::try_from(next.clamp(0, max)).unwrap_or(0);
    }

    #[allow(clippy::too_many_lines)]
    fn handle_normal(&mut self, action: Action) {
        match action {
            Action::Settings => self.open_settings(SettingsCategory::General),
            Action::Menu => self.toggle_menu(),
            Action::Timeline => self.toggle_timeline(),
            Action::Themes => self.open_settings(SettingsCategory::Appearance),
            Action::About => self.open_settings(SettingsCategory::About),
            Action::CycleSurface => {
                // step from what is on screen, not from a lock that fell back
                let shown = plan(
                    self.last_area,
                    self.effective_surface(),
                    self.layout_flags(),
                );
                match shown.surface {
                    Surface::Work => {
                        self.expanded = None;
                        self.lock_surface(Surface::Glance);
                    }
                    Surface::Glance if self.work_shows() => self.lock_surface(Surface::Work),
                    Surface::Glance => {}
                }
            }
            Action::Open => {
                if self.searching {
                    self.searching = false;
                    self.focus = Focus::Processes;
                    self.open_detail();
                } else if self.focus.panel() == Panel::Processes {
                    self.sync_model();
                    let group = self.proc.selected().is_some_and(|row| {
                        row.agg.is_some()
                            && self.proc.nesting(self.config.show_tree) == Nesting::Apps
                    });
                    if group {
                        self.proc.fold_selected(None, &self.snapshot.processes);
                    } else {
                        self.open_detail();
                    }
                } else {
                    self.expand_focus();
                }
            }
            Action::Expand => {
                let panel = self.focus.panel();
                if self.expanded == Some(panel) {
                    self.expanded = None;
                } else {
                    self.expand_panel(panel);
                }
            }
            Action::NextPanel => {
                if self.expanded.is_some() {
                    self.hop_related(1);
                } else {
                    self.cycle_focus(1);
                }
            }
            Action::PrevPanel => {
                if self.expanded.is_some() {
                    self.hop_related(-1);
                } else {
                    self.cycle_focus(-1);
                }
            }
            Action::Search => {
                self.reveal_processes();
                self.searching = true;
                self.focus = Focus::Search;
            }
            Action::FilterChar(c) => {
                self.reveal_processes();
                self.searching = true;
                self.focus = Focus::Search;
                self.proc.filter.push(c);
                self.proc.reset_top();
            }
            Action::FilterBackspace => {
                self.proc.filter.pop();
                self.proc.reset_top();
            }
            Action::FilterClear => self.clear_filter(),
            Action::FilterDone => {
                self.searching = false;
                self.focus = Focus::Processes;
                if !self.proc.filter.is_empty() {
                    let n = self.proc.model.len();
                    let noun = if n == 1 { "match" } else { "matches" };
                    let esc = self.key_label(Action::Cancel);
                    self.set_toast(
                        format!("filter kept · {n} {noun} · {esc} clears"),
                        ToastKind::Info,
                        STATUS_TTL,
                    );
                }
            }
            Action::Move(delta) => self.handle_move(delta),
            Action::Page(pages) => {
                let rows = i32::try_from(self.table_rows().max(1)).unwrap_or(1);
                self.move_list(pages.saturating_mul(rows));
            }
            Action::Top => self.move_list(-LIST_END),
            Action::Bottom => self.move_list(LIST_END),
            Action::Kill => self.arm(Signal::Term),
            Action::ForceKill => self.arm(Signal::Kill),
            Action::Interrupt => self.arm(Signal::Int),
            Action::SignalMenu => self.open_signal_menu(),
            Action::Renice(step) => self.renice(step),
            Action::ToggleBackground => self.toggle_background(),
            Action::CopyPid => self.copy(false),
            Action::CopyCommand => self.copy(true),
            Action::IntervalSlower => self.set_interval(self.config.step_interval(false)),
            Action::IntervalFaster => self.set_interval(self.config.step_interval(true)),
            Action::Pause => self.toggle_freeze(),
            Action::ToggleTree => self.toggle_tree(),
            Action::CycleSort | Action::SortNext => {
                self.set_sort(self.config.proc_sort.next(), false);
            }
            Action::SortPrev => self.set_sort(self.config.proc_sort.prev(), false),
            Action::SortBy(sort) => self.set_sort(sort, false),
            Action::ReverseSort => {
                self.config.sort_desc = !self.config.sort_desc;
                self.touch();
                self.proc.reset_top();
            }
            Action::CycleTheme => self.cycle_theme(),
            Action::Zoom => self.cycle_zoom(),
            Action::Preset(preset) => self.apply_preset(preset),
            Action::ToggleZen => self.toggle_zen(),
            Action::ToggleGroups => self.toggle_groups(),
            Action::Tag if self.proc_pane().is_none() => {
                self.set_toast(String::from(NO_LIST), ToastKind::Warn, STATUS_TTL);
            }
            Action::Tag => self.tag_selected(),
            Action::ClearTags => self.clear_tags(),
            Action::Unfold | Action::Fold => {
                let open = action == Action::Unfold;
                if self.nested_focus() {
                    self.sync_model();
                    self.proc
                        .fold_selected(Some(open), &self.snapshot.processes);
                } else {
                    self.handle_normal(if open {
                        Action::NextPanel
                    } else {
                        Action::PrevPanel
                    });
                }
            }
            Action::FoldAll(open) if self.proc.nesting(self.config.show_tree) != Nesting::Flat => {
                self.proc.fold_all(open);
                self.proc.reset_top();
            }
            _ => {}
        }
    }

    /// The process table has focus and nests rows (tree or app groups), so
    /// ←/→ fold nodes instead of hopping boxes.
    fn nested_focus(&self) -> bool {
        self.focus.panel() == Panel::Processes
            && !self.searching
            && self.proc.nesting(self.config.show_tree) != Nesting::Flat
    }

    fn clear_filter(&mut self) {
        if !self.proc.filter.is_empty() {
            self.proc.filter.clear();
            self.proc.reset_top();
        }
    }

    fn toggle_groups(&mut self) {
        self.proc.grouped = !self.proc.grouped;
        self.proc.reset_top();
        let note = match (self.proc.grouped, self.config.show_tree) {
            (true, true) => format!(
                "grouped by app once tree view is off ({})",
                self.key_label(Action::ToggleTree)
            ),
            (true, false) => String::from("grouped by app · enter opens a group"),
            (false, _) => String::from("ungrouped"),
        };
        self.set_toast(note, ToastKind::Info, STATUS_TTL);
    }

    /// Space: tags the selected row (all of an app group) and steps down,
    /// so a run of spaces tags a run of rows.
    fn tag_selected(&mut self) {
        self.sync_model();
        let Some(pid) = self.proc.selected().map(|row| row.pid) else {
            return;
        };
        let members = self.proc.model.members_of(pid);
        self.proc.toggle_tags(&members, &self.snapshot.processes);
        let n = self.proc.tags.len();
        if n > 0 {
            self.set_toast(
                format!(
                    "{n} tagged · {}/{} act on them · {} clears",
                    self.key_label(Action::Kill),
                    self.key_label(Action::SignalMenu),
                    self.key_label(Action::ClearTags),
                ),
                ToastKind::Info,
                STATUS_TTL,
            );
        }
        self.move_list(1);
    }

    fn clear_tags(&mut self) {
        if !self.proc.tags.is_empty() {
            self.proc.tags.clear();
            self.set_toast(String::from("tags cleared"), ToastKind::Info, STATUS_TTL);
        }
    }

    /// Whom a signal goes to: the open dossier's process; else every tagged
    /// process; else the selected row, all members of an app group. In
    /// display order of the snapshot, the selected/first one first.
    fn action_targets(&mut self) -> Vec<&siltop_core::Process> {
        self.sync_model();
        let pids: Vec<u32> = if let Some(pid) = self.detail_pid {
            vec![pid]
        } else if !self.proc.tags.is_empty() {
            self.proc.tags.pids().collect()
        } else if let Some(row) = self.proc.selected() {
            self.proc.model.members_of(row.pid)
        } else {
            Vec::new()
        };
        pids.iter()
            .filter_map(|pid| self.snapshot.processes.iter().find(|p| p.pid == *pid))
            .collect()
    }

    /// "Safari (412)", or "3 processes" for a set.
    fn set_label(targets: &[&siltop_core::Process]) -> String {
        match targets {
            [one] => Self::who(one),
            many => format!("{} processes", many.len()),
        }
    }

    fn open_settings(&mut self, category: SettingsCategory) {
        self.help = false;
        self.menu = None;
        self.timeline = None;
        self.settings = Some(SettingsState::open(category));
    }

    /// Steps the focused settings row and applies what changed.
    fn step_setting(&mut self, step: SettingStep) {
        let Some(def) = self.settings.as_ref().and_then(SettingsState::selected) else {
            return;
        };
        let mut next = self.live_config();
        def.step(&mut next, step);
        self.apply_config(next);
        self.reveal_theme();
    }

    /// Keeps the theme in use in view in the appearance preview.
    fn reveal_theme(&mut self) {
        let Some(index) = theme_options()
            .iter()
            .position(|n| n.eq_ignore_ascii_case(&self.config.theme))
        else {
            return;
        };
        let area = self.last_area;
        if let Some(settings) = self.settings.as_mut() {
            reveal_swatch(area, settings, index);
        }
    }

    /// Makes `next` the live config and runs each change's side effect, so
    /// the settings page, `T` and the keymap all reach the same state.
    fn apply_config(&mut self, next: Config) {
        // The live config, not `prefs()`: a CLI flag must keep holding while
        // the user changes some other setting.
        let before = self.live_config();
        if before == next {
            return;
        }
        self.config = next;
        self.proc_ratio = self.config.proc_ratio;
        if before.interval != self.config.interval {
            self.set_interval(self.config.interval);
        }
        if before.low_power != self.config.low_power {
            self.source.set_low_power(self.config.low_power);
        }
        if before.theme != self.config.theme
            || before.icons != self.config.icons
            || before.glass != self.config.glass
            || before.extra_contrast != self.config.extra_contrast
        {
            self.theme = crate::term_caps::theme_for(&self.config);
        }
        if before.surface != self.config.surface {
            // A chosen layout (settings or a preset) outranks the session's
            // `v` lock; glance has no expanded box to keep.
            self.surface_lock = None;
            if self.config.surface == Some(Surface::Glance) {
                self.expanded = None;
            }
        }
        if before.keymap != self.config.keymap {
            self.set_profile(Profile::by_name(&self.config.keymap).unwrap_or_default());
        }
        if before.proc_sort != self.config.proc_sort && before.sort_desc == self.config.sort_desc {
            // a new column (settings, a preset) starts its natural way
            self.config.sort_desc = self.config.proc_sort.natural_descending();
        }
        if before.show_tree != self.config.show_tree
            || before.proc_sort != self.config.proc_sort
            || before.sort_desc != self.config.sort_desc
        {
            self.proc.reset_top();
        }
        self.touch();
    }

    /// Applies a named look; its surface replaces any `v` lock.
    fn apply_preset(&mut self, preset: siltop_core::Preset) {
        let mut next = self.live_config();
        preset.apply(&mut next);
        self.apply_config(next);
        self.set_toast(
            format!("preset {}", preset.label()),
            ToastKind::Info,
            INTERVAL_TOAST_TTL,
        );
    }

    fn toggle_zen(&mut self) {
        let mut next = self.live_config();
        next.zen = !next.zen;
        self.apply_config(next);
    }

    /// Which `zoom` pick applies: compact views share one, expanded another.
    fn zoom_slot(&self) -> usize {
        usize::from(self.expanded.is_some())
    }

    fn cycle_zoom(&mut self) {
        let next = self.view().zoom().next();
        self.zoom[self.zoom_slot()] = Some(next);
        self.set_toast(
            format!("graphs {}", next.label()),
            ToastKind::Info,
            INTERVAL_TOAST_TTL,
        );
    }

    /// Move the pointer to `(col, row)`; true when the lit button changed,
    /// which is the only time motion costs a frame.
    fn hover_to(&mut self, col: u16, row: u16) -> bool {
        let next = siltop_ui::hover_rect(&self.view(), self.last_area, col, row);
        if next == self.hover {
            return false;
        }
        self.hover = next;
        true
    }

    /// `T`: auto → every built-in, the pack and the user's themes → back
    /// to auto, stepping from the saved choice.
    fn cycle_theme(&mut self) {
        let next = Theme::next_in(&Theme::catalog(), &self.config.theme);
        if let Some(theme) = Theme::by_name(next) {
            self.set_theme(theme);
            self.reveal_theme();
            return;
        }
        Theme::AUTO.clone_into(&mut self.config.theme);
        self.theme = crate::term_caps::theme_for(&self.config);
        self.touch();
        self.set_toast(
            format!("theme {}", Theme::AUTO),
            ToastKind::Info,
            INTERVAL_TOAST_TTL,
        );
    }

    /// Swaps the palette, keeping the terminal's colour depth and icons.
    fn set_theme(&mut self, mut theme: Theme) {
        theme.depth = self.theme.depth;
        theme.icons = self.theme.icons;
        let theme = theme.on_surface_with(
            self.theme.surface,
            crate::term_caps::caps().palette.bg,
            self.config.extra_contrast,
        );
        let name = theme.name;
        name.clone_into(&mut self.config.theme);
        self.theme = theme;
        self.touch();
        self.set_toast(format!("theme {name}"), ToastKind::Info, INTERVAL_TOAST_TTL);
    }

    fn point_in(rect: Rect, col: u16, row: u16) -> bool {
        col >= rect.x
            && col < rect.x.saturating_add(rect.width)
            && row >= rect.y
            && row < rect.y.saturating_add(rect.height)
    }

    fn drag_split(&mut self, col: u16) {
        if !self.dragging_split || self.last_area.width < 40 {
            return;
        }
        let proc_w = self.last_area.width.saturating_sub(col.saturating_add(1));
        let ratio =
            u16::try_from(u32::from(proc_w) * 100 / u32::from(self.last_area.width)).unwrap_or(34);
        self.proc_ratio = ratio.clamp(PROC_RATIO_MIN, PROC_RATIO_MAX);
        self.config.proc_ratio = self.proc_ratio;
        self.touch();
    }

    fn handle_move(&mut self, delta: i32) {
        if self.expanded.is_some() && self.expanded != Some(Panel::Processes) {
            return;
        }
        if self.searching
            || self.focus.panel() == Panel::Processes
            || self.expanded == Some(Panel::Processes)
        {
            self.move_list(delta);
        } else {
            self.cycle_focus(delta.signum());
        }
    }

    /// Moves the process selection; the search line keeps focus (fzf).
    fn move_list(&mut self, delta: i32) {
        if self.expanded.is_some() && self.expanded != Some(Panel::Processes) {
            return;
        }
        if self.searching
            || self.focus.panel() == Panel::Processes
            || self.expanded == Some(Panel::Processes)
        {
            if !self.searching {
                self.focus = Focus::Processes;
            }
            self.sync_model();
            self.proc.move_by(delta, self.table_rows());
            if self.detail_pid.is_some() {
                self.detail_pid = self.proc.selected_pid();
                self.refresh_inspection();
            }
        }
    }

    /// The wheel reaches only the topmost layer: a picker or menu moves its
    /// highlight, help scrolls, a page steps; on the dashboard it scrolls
    /// the table under the pointer and never moves the selection or focus.
    fn on_scroll(&mut self, col: u16, row: u16, delta: i32) {
        let step = Action::Move(delta.signum());
        match self.mode() {
            Mode::Confirm => {}
            Mode::SignalMenu => self.handle_signal_menu(step),
            Mode::Help => self.reduce_help(Action::Move(delta.saturating_mul(WHEEL_ROWS))),
            Mode::Menu => self.handle_menu(step),
            Mode::Update => self.handle_update_dialog(step),
            Mode::Timeline => {
                if let Some(view) = self.timeline.as_mut() {
                    view.scrub(delta.signum());
                }
            }
            Mode::Settings | Mode::SettingsFilter => {
                let area = self.last_area;
                if let Some(settings) = self.settings.as_mut()
                    && !scroll_swatches(area, settings, col, row, delta.signum())
                {
                    settings.move_row(delta.signum());
                }
            }
            Mode::Search | Mode::Detail | Mode::Normal => self.scroll_dashboard(col, row, delta),
        }
    }

    fn scroll_dashboard(&mut self, col: u16, row: u16, delta: i32) {
        if self.detail_pid.is_some() {
            if self.proc.tab.inspects()
                && let Some(pane) = self.proc_pane()
                && Self::point_in(detail_rect(pane), col, row)
            {
                let rows = i64::from(delta.saturating_mul(WHEEL_ROWS));
                self.proc.scroll_page(rows, pane);
            }
            return;
        }
        let planned = plan(
            self.last_area,
            self.effective_surface(),
            self.layout_flags(),
        );
        if let Some(pane) = planned.processes
            && Self::point_in(pane, col, row)
        {
            self.sync_model();
            self.proc
                .scroll_by(delta.saturating_mul(WHEEL_ROWS), self.table_rows());
        }
    }

    fn handle_settings(&mut self, action: Action) {
        let step = match action {
            Action::SettingNext => Some(SettingStep::Next),
            Action::SettingPrev => Some(SettingStep::Prev),
            _ => None,
        };
        if let Some(step) = step {
            self.step_setting(step);
            return;
        }
        if action == Action::CycleTheme {
            self.cycle_theme();
            return;
        }
        let Some(settings) = self.settings.as_mut() else {
            return;
        };
        match action {
            Action::Move(delta) => settings.move_row(delta),
            Action::Top => settings.move_row(i32::MIN),
            Action::Bottom => settings.move_row(i32::MAX),
            Action::NextPanel => settings.move_category(1),
            Action::PrevPanel => settings.move_category(-1),
            Action::Search => settings.filtering = true,
            Action::FilterChar(c) => settings.push_filter(c),
            Action::FilterBackspace => settings.pop_filter(),
            Action::FilterClear => settings.select_category(settings.category),
            Action::Open => settings.filtering = false,
            _ => {}
        }
    }

    /// A click on the settings page, read from the geometry it painted.
    fn click_settings(&mut self, col: u16, row: u16) {
        let Some(settings) = self.settings.as_mut() else {
            return;
        };
        match settings_hit(self.last_area, settings, col, row) {
            Some(SettingsHit::Close) => self.settings = None,
            Some(SettingsHit::Category(category)) => settings.select_category(category),
            Some(SettingsHit::Row(index)) => settings.row = index,
            Some(SettingsHit::Value(index)) => {
                settings.row = index;
                self.step_setting(SettingStep::Next);
            }
            Some(SettingsHit::Theme(index)) => {
                if let Some(name) = theme_options().get(index) {
                    let mut next = self.live_config();
                    (*name).clone_into(&mut next.theme);
                    self.apply_config(next);
                }
            }
            None => {}
        }
    }

    fn toggle_tree(&mut self) {
        self.config.show_tree = !self.config.show_tree;
        self.proc.reset_top();
        self.touch();
    }

    /// Whether the list runs against its column's natural direction; the
    /// saved `sort_desc` ("biggest first") is the one source of truth.
    fn sort_reversed(&self) -> bool {
        self.config.sort_desc != self.config.proc_sort.natural_descending()
    }

    /// Sorts by `sort`, its natural way unless `reversed`; both are saved.
    fn set_sort(&mut self, sort: ProcSort, reversed: bool) {
        let desc = sort.natural_descending() != reversed;
        if self.config.proc_sort != sort || self.config.sort_desc != desc {
            self.config.proc_sort = sort;
            self.config.sort_desc = desc;
            self.touch();
        }
        self.proc.reset_top();
    }

    /// Header click: a new column sorts its natural way; the active column
    /// flips direction.
    fn sort_by(&mut self, sort: ProcSort) {
        let reversed = self.config.proc_sort == sort && !self.sort_reversed();
        self.set_sort(sort, reversed);
    }

    fn toggle_freeze(&mut self) {
        self.frozen = !self.frozen;
        self.paused_at = self.frozen.then(clock_hms);
        self.source.set_paused(self.frozen, self.now);
        self.show_replay(self.now);
    }

    /// A replay's pids are history: here they may name nothing, or another
    /// process that reused the number, so none is signalled or inspected.
    fn read_only(&self) -> bool {
        self.source.player().is_some()
    }

    /// Arms `signal` for the dossier's pid or the selected row.
    fn arm(&mut self, signal: Signal) {
        if self.may_act() {
            self.pending_signal = signal;
            self.arm_kill();
        }
    }

    fn open_detail(&mut self) {
        self.sync_model();
        if let Some(pid) = self.proc.selected_pid() {
            self.detail_pid = Some(pid);
            self.proc.anchor = Anchor::Pinned(pid);
            self.refresh_inspection();
        } else {
            self.set_toast(
                String::from("no process selected"),
                ToastKind::Warn,
                STATUS_TTL,
            );
        }
    }

    /// Clicks go to the topmost overlay first; only the kill prompt, which
    /// lives in the footer, lets footer clicks through. With nothing open
    /// the footer, the dossier, the hop row and the dashboard answer.
    fn on_click(&mut self, col: u16, row: u16) {
        let flags = self.layout_flags();
        let planned = plan(self.last_area, self.effective_surface(), flags);
        let on_footer = row == planned.footer.y;
        match self.top_overlay() {
            Some(Overlay::Update) => {
                if siltop_ui::update_dialog_close(self.last_area)
                    .is_some_and(|r| Self::point_in(r, col, row))
                {
                    self.update_dialog = false;
                } else if let Some(index) = siltop_ui::update_dialog_hit(self.last_area, col, row) {
                    self.update_focus = index;
                    self.choose_update();
                }
                return;
            }
            Some(Overlay::Confirm) if on_footer => {}
            Some(Overlay::Confirm) => return,
            Some(Overlay::SignalMenu) => {
                self.click_signal_menu(col, row);
                return;
            }
            Some(Overlay::Help) => {
                let card = help_rect(self.last_area, &self.help_groups);
                let close = siltop_ui::card_close(card);
                if !Self::point_in(card, col, row)
                    || close.is_some_and(|r| Self::point_in(r, col, row))
                {
                    self.help = false;
                }
                return;
            }
            Some(Overlay::Menu) => {
                self.click_menu(col, row);
                return;
            }
            Some(Overlay::Timeline) => {
                self.click_timeline(col, row);
                return;
            }
            Some(Overlay::Settings) => {
                self.click_settings(col, row);
                return;
            }
            None => {}
        }
        if on_footer {
            // the footer was painted for the surface the layout fell back to
            let hit = footer_hit(
                &AppView {
                    surface: planned.surface,
                    ..self.view()
                },
                planned.footer,
                col,
                row,
            );
            if let Some(hit) = hit {
                self.apply_hit(hit);
            }
            return;
        }
        if self.detail_pid.is_some()
            && let Some(pane) = planned.processes
        {
            self.click_detail(pane, col, row);
            return;
        }
        if let Some(panel) = self.expanded
            && let Some(area) = planned.panel(panel)
            && let Some(target) = hop_hit(area, &self.view(), col, row)
        {
            self.expanded = Some(target);
            self.focus = Focus::from_panel(target);
            return;
        }
        self.sync_model();
        if let Some(hit) = hit_at(&self.view(), self.last_area, col, row) {
            self.apply_hit(hit);
        }
    }

    /// A click with the signal picker open: a row sends its signal; the
    /// `[×]` or anywhere else closes the picker. Same geometry as painted.
    fn click_signal_menu(&mut self, col: u16, row: u16) {
        let view = self.view();
        let area = siltop_ui::signal_menu_area(&view, self.last_area);
        let picked = view.signal_menu.and_then(|menu| {
            let close = siltop_ui::card_close(siltop_ui::signal_menu_rect(area, &menu));
            if close.is_some_and(|r| Self::point_in(r, col, row)) {
                None
            } else {
                signal_menu_row_at(area, &menu, col, row)
            }
        });
        match picked {
            Some(index) => {
                if let Some(menu) = self.signal_menu.as_mut() {
                    menu.selected = index;
                }
                self.pick_signal();
            }
            None => self.signal_menu = None,
        }
    }

    /// A click with the dossier open in `pane`: an action chip, a page
    /// tab, the `[×]`; outside the card closes it.
    fn click_detail(&mut self, pane: Rect, col: u16, row: u16) {
        let action = detail_actions(pane, self.read_only())
            .into_iter()
            .find(|(rect, _)| Self::point_in(*rect, col, row));
        let tab = detail_tabs(pane, &self.view())
            .into_iter()
            .find(|(rect, _)| Self::point_in(*rect, col, row));
        if let Some((_, hit)) = action {
            self.apply_hit(hit);
        } else if detail_close(pane).is_some_and(|r| Self::point_in(r, col, row)) {
            self.detail_pid = None;
        } else if let Some((_, tab)) = tab {
            let n = DossierTab::ALL.iter().position(|t| *t == tab).unwrap_or(0);
            self.switch_tab(Action::DossierTab(u8::try_from(n + 1).unwrap_or(1)));
        } else if !Self::point_in(detail_rect(pane), col, row) {
            self.detail_pid = None;
        }
    }

    fn apply_hit(&mut self, hit: Hit) {
        match hit {
            Hit::Help => self.toggle_help(),
            Hit::Settings => self.open_settings(SettingsCategory::General),
            Hit::Themes => self.open_settings(SettingsCategory::Appearance),
            Hit::About => self.open_settings(SettingsCategory::About),
            Hit::CheckUpdate => self.open_update_dialog(),
            Hit::Timeline => self.toggle_timeline(),
            Hit::Menu => self.toggle_menu(),
            Hit::Verdict => self.select_verdict(),
            Hit::Quit => self.quit = true,
            Hit::Search => {
                self.reveal_processes();
                self.searching = true;
                self.focus = Focus::Search;
            }
            Hit::FilterClear => {
                self.clear_filter();
                self.set_toast(String::from("filter cleared"), ToastKind::Info, STATUS_TTL);
            }
            Hit::Work if self.work_shows() => self.lock_surface(Surface::Work),
            Hit::Work => {}
            Hit::Pause => self.toggle_freeze(),
            Hit::HopNext => self.hop_related(1),
            Hit::ExpandClose => self.expanded = None,
            Hit::DetailClose => self.detail_pid = None,
            Hit::Kill | Hit::DetailTerm => self.arm(Signal::Term),
            Hit::DetailKill => self.arm(Signal::Kill),
            Hit::DetailInterrupt => self.arm(Signal::Int),
            Hit::ConfirmYes => self.handle_confirm(Action::ConfirmYes),
            Hit::ConfirmNo => self.cancel_kill(),
            Hit::SortBy(sort) => self.sort_by(sort),
            Hit::ProcRow(index) => self.click_row(index),
            Hit::Panel(panel) => self.focus = Focus::from_panel(panel),
            Hit::Expand(panel) => {
                self.focus = Focus::from_panel(panel);
                self.expand_panel(panel);
            }
            Hit::Split => self.dragging_split = true,
        }
    }

    /// A click selects; a second click on the same process within 400 ms
    /// opens its dossier.
    fn click_row(&mut self, index: usize) {
        self.searching = false;
        self.focus = Focus::Processes;
        let Some(pid) = self.proc.model.rows().get(index).map(|row| row.pid) else {
            return;
        };
        let double = self.last_click.is_some_and(|(at, last)| {
            last == pid && self.now.saturating_duration_since(at) <= DOUBLE_CLICK
        });
        self.proc.select(index, self.table_rows());
        if double {
            self.last_click = None;
            self.open_detail();
        } else {
            self.last_click = Some((self.now, pid));
        }
    }

    fn layout_flags(&self) -> LayoutFlags {
        LayoutFlags {
            show_gpu: self.config.show_gpu,
            show_net: self.config.show_net,
            show_disk: self.config.show_disk,
            show_fans: self.config.show_fans,
            show_power: self.config.show_power,
            has_gpu: self.snapshot.gpu.is_some(),
            has_fans: self.snapshot.fans.is_present() || self.snapshot.sensors.is_present(),
            has_disk: !self.snapshot.disk.volumes.is_empty(),
            has_power: siltop_ui::has_power(&self.snapshot),
            expanded: self.expanded,
            proc_ratio: self.proc_ratio,
            no_footer: self.view().zen_quiet(),
        }
    }

    fn expand_focus(&mut self) {
        if self.expanded.is_some() {
            return;
        }
        self.expand_panel(self.focus.panel());
    }

    fn expand_panel(&mut self, panel: Panel) {
        self.expanded = Some(panel);
        if panel != Panel::Processes {
            self.searching = false;
        }
    }

    fn hop_related(&mut self, dir: i32) {
        let Some(current) = self.expanded else {
            return;
        };
        let ring = hop_ring(self.layout_flags(), current);
        if ring.len() < 2 {
            return;
        }
        let Some(i) = ring.iter().position(|p| *p == current) else {
            return;
        };
        let next = if dir < 0 {
            i.checked_sub(1).unwrap_or(ring.len() - 1)
        } else {
            (i + 1) % ring.len()
        };
        if let Some(&panel) = ring.get(next) {
            self.expanded = Some(panel);
            self.focus = Focus::from_panel(panel);
        }
    }

    fn cycle_focus(&mut self, dir: i32) {
        let panels = self.layout_flags().visible_panels();
        if panels.is_empty() {
            return;
        }
        let current = self.focus.panel();
        let next = match panels.iter().position(|&p| p == current) {
            Some(i) if dir < 0 => i.checked_sub(1).unwrap_or(panels.len() - 1),
            Some(i) => (i + 1) % panels.len(),
            None if dir < 0 => panels.len() - 1,
            None => 0,
        };
        if let Some(&panel) = panels.get(next) {
            self.focus = Focus::from_panel(panel);
        }
    }

    /// Where the rows came from when they are not this Mac's processes now.
    fn recorded(&self) -> Option<&'static str> {
        if self.source.player().is_some() {
            Some("replay")
        } else if self.demo {
            Some("demo")
        } else {
            None
        }
    }

    /// Whether a signal, renice or background may reach a process: never
    /// from a recording (its pids name whatever runs here now), and never
    /// at a list this view does not show. A refusal says why.
    fn may_act(&mut self) -> bool {
        let why = if let Some(source) = self.recorded() {
            format!("{source}: process actions are off")
        } else if self.proc_pane().is_none() {
            String::from(NO_LIST)
        } else {
            return true;
        };
        self.set_toast(why, ToastKind::Warn, STATUS_TTL);
        false
    }

    /// Brings the process list into view: collapses another expanded box
    /// and leaves Glance, which has no table.
    fn reveal_processes(&mut self) {
        if self.expanded.is_some_and(|panel| panel != Panel::Processes) {
            self.expanded = None;
        }
        if self.effective_surface() == Surface::Glance {
            self.lock_surface(Surface::Work);
        }
    }

    /// Targets only the open dossier's pid or a pid the list shows.
    fn arm_kill(&mut self) {
        let targets = self.action_targets();
        let first_denied = targets.iter().find_map(|p| Self::denied_note(p));
        let allowed: Vec<KillTarget> = targets
            .iter()
            .filter(|p| Self::denied_note(p).is_none())
            .map(|p| KillTarget::of(p))
            .collect();
        let empty = targets.is_empty();
        if allowed.is_empty() && !empty {
            self.cancel_kill();
            if let Some(note) = first_denied {
                self.set_toast(note, ToastKind::Warn, STATUS_TTL);
            }
            return;
        }
        self.arm_set(allowed);
        if !self.confirm_kill {
            self.set_toast(
                String::from("no process selected"),
                ToastKind::Warn,
                STATUS_TTL,
            );
        }
    }

    fn set_toast(&mut self, message: String, kind: ToastKind, ttl: Duration) {
        self.status = Some((message, self.now));
        self.status_kind = kind;
        self.status_ttl = ttl;
        self.status_replay = false;
        self.pacer.mark();
    }

    /// A modal layer (help, settings, the kill prompt, a menu) owns the pointer.
    fn overlay_open(&self) -> bool {
        self.top_overlay().is_some()
    }

    /// The process an action applies to: the dossier's pid, else the
    /// selected row, as it was listed.
    fn action_process(&mut self) -> Option<&siltop_core::Process> {
        self.sync_model();
        let pid = self.detail_pid.or_else(|| self.proc.selected_pid())?;
        self.snapshot.processes.iter().find(|p| p.pid == pid)
    }

    /// "Safari (412)".
    fn who(proc: &siltop_core::Process) -> String {
        format!("{} ({})", proc.name, proc.pid)
    }

    /// Why `proc` cannot be signalled, before anything is armed: EPERM is
    /// found with `kill(pid, 0)`, which delivers nothing.
    fn denied_note(proc: &siltop_core::Process) -> Option<String> {
        match can_signal(KillTarget::of(proc)) {
            Err(ActionError::InvalidPid { .. }) => {
                Some(String::from("the kernel itself cannot be signalled"))
            }
            Err(err) if err.is_not_permitted() => {
                let owner = if proc.user.is_empty() {
                    format!("uid {}", proc.uid)
                } else {
                    proc.user.clone()
                };
                Some(format!("owned by {owner}: needs admin"))
            }
            _ => None,
        }
    }

    fn open_signal_menu(&mut self) {
        if !self.may_act() {
            return;
        }
        let targets = self.action_targets();
        let Some(&proc) = targets.first() else {
            self.set_toast(
                String::from("no process selected"),
                ToastKind::Warn,
                STATUS_TTL,
            );
            return;
        };
        let target = KillTarget::of(proc);
        let label = Self::set_label(&targets);
        let allowed: Vec<KillTarget> = targets
            .iter()
            .filter(|p| Self::denied_note(p).is_none())
            .map(|p| KillTarget::of(p))
            .collect();
        let denied = allowed
            .is_empty()
            .then(|| Self::denied_note(proc))
            .flatten();
        let critical = targets
            .iter()
            .find(|p| is_critical(p.pid, &p.name))
            .map(|p| format!("{} is critical: ending it can log you out", p.name));
        let enabled = denied.is_none();
        let set = if targets.len() > 1 {
            allowed
        } else {
            Vec::new()
        };
        let target = set.first().copied().unwrap_or(target);
        let rows = Signal::ALL
            .iter()
            .map(|s| SignalRow {
                label: s.label(),
                describe: s.describe(),
                enabled,
            })
            .collect();
        self.proc.armed = set.iter().map(|t| t.pid).collect();
        self.signal_menu = Some(SignalMenu {
            target,
            set,
            label,
            rows,
            selected: 0,
            note: denied.or(critical),
        });
    }

    fn handle_signal_menu(&mut self, action: Action) {
        let Some(menu) = self.signal_menu.as_mut() else {
            return;
        };
        match action {
            Action::Move(delta) => {
                let last = menu.rows.len().saturating_sub(1);
                let next = i64::try_from(menu.selected).unwrap_or(0) + i64::from(delta);
                menu.selected = usize::try_from(next.max(0)).unwrap_or(0).min(last);
            }
            Action::ConfirmYes => self.pick_signal(),
            Action::ConfirmNo | Action::SignalMenu => self.signal_menu = None,
            _ => {}
        }
    }

    /// Sends the picked signal, arming a confirm first for the ones that
    /// end or freeze the process. A refused process never arms.
    fn pick_signal(&mut self) {
        let Some(menu) = self.signal_menu.take() else {
            return;
        };
        let Some(&signal) = Signal::ALL.get(menu.selected) else {
            return;
        };
        if !menu.rows.get(menu.selected).is_some_and(|r| r.enabled) {
            let note = menu.note.unwrap_or_else(|| String::from("not permitted"));
            self.set_toast(note, ToastKind::Warn, STATUS_TTL);
            return;
        }
        let set = if menu.set.is_empty() {
            vec![menu.target]
        } else {
            menu.set
        };
        if signal.needs_confirm() {
            self.pending_signal = signal;
            self.arm_set(set);
        } else {
            self.proc.armed.clear();
            self.deliver_all(&set, signal);
        }
    }

    /// Renice by `step`; the kernel refuses raising priority without root
    /// and the toast says so.
    fn renice(&mut self, step: i32) {
        if !self.may_act() {
            return;
        }
        let Some(proc) = self.action_process() else {
            return;
        };
        let (target, who) = (KillTarget::of(proc), Self::who(proc));
        let result = nice(target).and_then(|now| {
            let next = (now + step).clamp(-20, 20);
            set_nice(target, next).map(|()| (now, next))
        });
        match result {
            Ok((from, to)) => self.set_toast(
                renice_note(from, to, &who, running_as_root()),
                ToastKind::Info,
                STATUS_TTL,
            ),
            Err(err) => self.set_toast(err.to_string(), ToastKind::Error, STATUS_TTL),
        }
    }

    fn toggle_background(&mut self) {
        if !self.may_act() {
            return;
        }
        let Some(proc) = self.action_process() else {
            return;
        };
        let (target, who) = (KillTarget::of(proc), Self::who(proc));
        let on = !self.backgrounded.contains(&target);
        match set_background(target, on) {
            Ok(()) => {
                if on {
                    self.backgrounded.insert(target);
                } else {
                    self.backgrounded.remove(&target);
                }
                let state = if on {
                    "background (E-cores, throttled I/O)"
                } else {
                    "normal priority"
                };
                self.set_toast(format!("{who}: {state}"), ToastKind::Info, STATUS_TTL);
            }
            Err(err) => self.set_toast(err.to_string(), ToastKind::Error, STATUS_TTL),
        }
    }

    /// Queues an OSC 52 copy of the dossier's (or the selected row's) pid
    /// or command line.
    fn copy(&mut self, command: bool) {
        let Some(proc) = self.action_process() else {
            self.set_toast(
                String::from("no process selected"),
                ToastKind::Warn,
                STATUS_TTL,
            );
            return;
        };
        let (text, what) = if command {
            let cmd = if proc.args.is_empty() {
                proc.command.clone().unwrap_or_else(|| proc.name.clone())
            } else {
                proc.args.join(" ")
            };
            (cmd, "command")
        } else {
            (proc.pid.to_string(), "pid")
        };
        match crate::osc::sequence(&text) {
            Some(seq) => {
                self.clipboard = Some(seq);
                self.set_toast(format!("copied {what}"), ToastKind::Info, STATUS_TTL);
            }
            None => self.set_toast(
                format!("{what} too long to copy"),
                ToastKind::Warn,
                STATUS_TTL,
            ),
        }
    }

    fn cancel_kill(&mut self) {
        self.confirm = None;
        self.confirm_set.clear();
        self.proc.armed.clear();
        self.confirm_kill = false;
    }

    /// How many armed targets are critical, as listed now.
    fn armed_critical(&self) -> usize {
        if !self.confirm_kill {
            return 0;
        }
        let armed = if self.confirm_set.is_empty() {
            self.confirm.as_slice()
        } else {
            self.confirm_set.as_slice()
        };
        self.snapshot
            .processes
            .iter()
            .filter(|p| armed.contains(&KillTarget::of(p)) && is_critical(p.pid, &p.name))
            .count()
    }

    /// Arms the confirm for `set` (first is the one the prompt names).
    fn arm_set(&mut self, set: Vec<KillTarget>) {
        self.confirm = set.first().copied();
        self.proc.armed = if set.len() > 1 {
            set.iter().map(|t| t.pid).collect()
        } else {
            Vec::new()
        };
        self.confirm_set = set;
        self.confirm_kill = self.confirm.is_some();
    }

    /// Sends `signal` to every target; one toast sums it up. Returns the
    /// pids it reached or whose process had already exited (gone, or its
    /// pid reused by another): the ones it is over for if `signal` ends a
    /// process.
    fn deliver_all(&mut self, targets: &[KillTarget], signal: Signal) -> Vec<u32> {
        let (sent, failed) = send_signal_all(targets, signal);
        let reached = targets
            .iter()
            .map(|t| t.pid)
            .filter(|&pid| {
                failed.iter().all(|err| {
                    err.pid() != pid
                        || matches!(err, ActionError::Exited { .. } | ActionError::Reused { .. })
                })
            })
            .collect();
        let first_err = failed.first().map(ToString::to_string);
        let label = signal.label();
        match (targets, first_err) {
            ([one], None) => {
                let pid = one.pid;
                let msg = format!("sent {label} to {} ({pid})", self.proc_name(pid));
                self.set_toast(msg, ToastKind::Info, STATUS_TTL);
            }
            (_, None) => {
                self.set_toast(
                    format!("sent {label} to {sent} processes"),
                    ToastKind::Info,
                    STATUS_TTL,
                );
            }
            ([_], Some(err)) => self.set_toast(err, ToastKind::Error, STATUS_TTL),
            (all, Some(err)) => self.set_toast(
                format!("sent {label} to {sent} of {}: {err}", all.len()),
                ToastKind::Error,
                STATUS_TTL,
            ),
        }
        reached
    }

    fn handle_confirm(&mut self, action: Action) {
        match action {
            Action::ConfirmYes => {
                if let Some(target) = self.confirm.take() {
                    let mut set = std::mem::take(&mut self.confirm_set);
                    if set.is_empty() {
                        set.push(target);
                    }
                    let signal = self.pending_signal;
                    let reached = self.deliver_all(&set, signal);
                    // A frozen, hung-up or refused process is still there to
                    // watch and to CONT: only one that is ending lets go.
                    if signal.ends_process() {
                        if self.detail_pid.is_some_and(|pid| reached.contains(&pid)) {
                            self.detail_pid = None;
                        }
                        self.proc.tags.retain(|pid| !reached.contains(&pid));
                    }
                }
                self.cancel_kill();
                self.pending_signal = Signal::Term;
            }
            Action::ConfirmNo | Action::Kill => self.cancel_kill(),
            _ => {}
        }
    }

    /// Whether Work can show in this window; if not, says so and returns
    /// false, so no key or click flips a lock the user cannot see.
    fn work_shows(&mut self) -> bool {
        let fits = work_fits(self.last_area);
        if !fits {
            self.set_toast(
                String::from(siltop_ui::WORK_TOO_SMALL),
                ToastKind::Warn,
                STATUS_TTL,
            );
        }
        fits
    }

    /// The dossier lives inside the process pane: once a resize hides the
    /// pane, close it rather than leave its keys armed behind a card the
    /// user cannot see.
    fn drop_hidden_detail(&mut self) {
        if self.detail_pid.is_some()
            && plan(
                self.last_area,
                self.effective_surface(),
                self.layout_flags(),
            )
            .processes
            .is_none()
        {
            self.detail_pid = None;
            self.signal_menu = None;
            if self.confirm_kill {
                self.cancel_kill();
            }
        }
    }

    /// Session only: a keypress never rewrites `surface` in the config.
    fn lock_surface(&mut self, surface: Surface) {
        self.surface_lock = Some(surface);
        self.surface = surface;
    }

    fn view(&self) -> AppView<'_> {
        AppView {
            theme: &self.theme,
            snapshot: &self.snapshot,
            histories: &self.histories,
            surface: self.effective_surface(),
            focus: self.focus,
            proc: &self.proc,
            help: self.help,
            settings: self.settings.as_ref(),
            config: &self.config,
            config_path: self.config_path.as_deref(),
            confirm_kill: self.confirm_kill,
            confirm_pid: self.confirm.map(|target| target.pid),
            confirm_signal: self.pending_signal.label(),
            confirm_critical: self.armed_critical(),
            signal_menu: self.signal_menu.as_ref().map(|m| SignalMenuView {
                target: &m.label,
                rows: &m.rows,
                selected: m.selected,
                note: m.note.as_deref(),
            }),
            searching: self.searching,
            ready: self.ready,
            frozen: self.frozen,
            read_only: self.read_only(),
            show_gpu: self.config.show_gpu,
            show_net: self.config.show_net,
            show_disk: self.config.show_disk,
            show_fans: self.config.show_fans,
            show_power: self.config.show_power,
            show_cores: self.config.show_cores,
            show_threads: self.config.show_threads,
            show_tree: self.config.show_tree,
            sort: self.config.proc_sort,
            sort_reversed: self.sort_reversed(),
            detail_pid: self.detail_pid,
            expanded: self.expanded,
            proc_ratio: self.proc_ratio,
            interval_ms: u64::try_from(self.cadence().as_millis()).unwrap_or(1000),
            battery_saver: self.low_power && self.cadence() > self.config.interval,
            stopped: self.stopped,
            status: self.status.as_ref().map(|(message, _)| message.as_str()),
            status_kind: self.status_kind,
            paused_at: self.paused_at.as_deref(),
            help_groups: &self.help_groups,
            help_scroll: self.help_scroll,
            net_unit: siltop_core::NetUnit::from_bits_flag(self.config.net_bits),
            zoom: self.zoom[self.zoom_slot()],
            clock: self.clock,
            menu: self.menu.as_ref().map(menus::MenuState::view),
            verdict: self
                .first_run
                .shown(self.now)
                .or_else(|| self.verdict.shown()),
            about: &self.about,
            hover: self.hover,
            timeline: self.timeline.as_ref(),
            update_note: self.update_badge.as_deref(),
            keys: keymap::hints(self.profile),
        }
    }

    fn effective_surface(&self) -> Surface {
        self.surface_lock
            .or(self.config.surface)
            .unwrap_or(self.surface)
    }

    fn draw(&mut self, frame: &mut Frame) {
        self.last_area = frame.area();
        if self.surface_lock.or(self.config.surface).is_none() {
            self.surface = auto_surface(self.last_area.width, self.last_area.height);
        }
        render_app(frame, &self.view());
        if self.update_dialog
            && let Some(offer) = &self.update_offer
        {
            siltop_ui::render_update_dialog(
                frame,
                frame.area(),
                &siltop_ui::UpdateDialogView {
                    version: &offer.version,
                    notes: &offer.notes,
                    focus: self.update_focus,
                },
                &self.theme,
            );
        }
    }
}

/// Local wall-clock `hh:mm:ss`, for the paused marker.
fn clock_hms() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let t = libc::time_t::try_from(secs).unwrap_or(0);
    // SAFETY: `libc::tm` is plain old data; all-zero is a valid value
    // (the `tm_zone` pointer is null, which libc never dereferences here).
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are to live locals on this frame; localtime_r
    // is the reentrant variant and writes only into `tm`.
    let ok = unsafe { !libc::localtime_r(&raw const t, &raw mut tm).is_null() };
    if ok {
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    } else {
        String::from("--:--:--")
    }
}

/// The renice toast. Without root a raised nice value is one-way (the
/// kernel refuses to lower it again), so the toast says so up front.
fn renice_note(from: i32, to: i32, who: &str, root: bool) -> String {
    let note = format!("nice {from} → {to} for {who}");
    if to > from && !root {
        format!("{note}; undo needs admin")
    } else {
        note
    }
}

pub(crate) fn running_as_root() -> bool {
    // SAFETY: geteuid(2) has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    mod rc2;

    use std::sync::mpsc::channel;

    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{
        Event as TermEvent, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    };
    use siltop_core::{
        ClusterKind, CoreSample, DiskVolume, FanMetric, GpuSnapshot, Process, SeriesId, SocInfo,
    };
    use siltop_ui::{LayoutPlan, hit_test, plan};

    const MS: Duration = Duration::from_millis(1);

    fn proc(pid: u32, name: &str, cpu: f32) -> Process {
        Process {
            pid,
            ppid: 1,
            name: name.to_owned(),
            cpu: Some(cpu),
            mem_bytes: Some(u64::from(pid) * 1024),
            threads: Some(1),
            user: String::from("cloaky"),
            status: "sleeping",
            ..Process::default()
        }
    }

    /// Shaped like the M5 Pro test machine; no clock, no hardware.
    fn snapshot() -> Snapshot {
        let mut snap = Snapshot::empty();
        snap.soc = SocInfo {
            name: String::from("Apple M5 Pro"),
            p_cores: 12,
            s_cores: 6,
            gpu_cores: 20,
            memory_bytes: 24 << 30,
            ..SocInfo::default()
        };
        snap.cpu.scaled = 0.21;
        snap.cpu.active = 0.21;
        snap.cpu.cores = (0..18)
            .map(|i| CoreSample {
                kind: if i < 6 {
                    ClusterKind::Super
                } else {
                    ClusterKind::Performance
                },
                index: if i < 6 { i } else { i - 6 },
                scaled: 0.2,
                active: 0.2,
                freq_mhz: None,
            })
            .collect();
        snap.gpu = Some(GpuSnapshot {
            scaled: 0.12,
            ..GpuSnapshot::default()
        });
        snap.memory.used_bytes = 14 << 30;
        snap.memory.total_bytes = 24 << 30;
        snap.network.iface = String::from("en0");
        snap.network.rx_bps = 12_000;
        snap.network.tx_bps = 3_000;
        snap.disk.volumes = vec![DiskVolume {
            name: String::from("Macintosh HD"),
            mount: String::from("/"),
            used_bytes: 400 << 30,
            total_bytes: 926 << 30,
        }];
        snap.fans.fans = vec![FanMetric {
            name: String::from("Fan 1"),
            rpm: 1850,
            max_rpm: 6000,
        }];
        snap.sensors.cpu_c = Some(48.0);
        snap.sensors.gpu_c = Some(44.0);
        snap.processes = (1..=40)
            .map(|pid| proc(pid, &format!("proc{pid}"), 40.0 - pid as f32))
            .collect();
        snap
    }

    fn scripted(snaps: Vec<Snapshot>) -> App {
        App::with_source(Config::default(), SnapshotSource::Scripted(snaps.into()))
    }

    pub(super) fn app() -> App {
        let mut app = scripted(vec![snapshot()]);
        assert!(app.pump());
        app
    }

    #[test]
    fn preset_key_applies_the_spec_and_beats_the_surface_lock() {
        let mut app = app();
        app.lock_surface(Surface::Work);
        app.reduce(Action::Preset(siltop_core::Preset::Ricer));
        assert_eq!(
            siltop_core::Preset::matching(&app.config),
            Some(siltop_core::Preset::Ricer)
        );
        assert_eq!(app.effective_surface(), Surface::Glance);
        assert!(app.config.zen);
        assert!(app.dirty_prefs, "a preset is a saved choice");
    }

    #[test]
    fn preset_to_glance_collapses_an_expanded_box() {
        let mut app = app();
        app.expanded = Some(Panel::Mem);
        app.reduce(Action::Preset(siltop_core::Preset::Ricer));
        assert_eq!(app.expanded, None);
    }

    #[test]
    fn zen_key_toggles_and_toggles_back() {
        let mut app = app();
        assert!(!app.config.zen);
        app.reduce(Action::ToggleZen);
        assert!(app.config.zen);
        assert!(app.view().zen_quiet() || app.view().status.is_some());
        app.reduce(Action::ToggleZen);
        assert!(!app.config.zen);
    }

    impl App {
        fn pump(&mut self) -> bool {
            let Some(snapshot) = self.source.next_scripted() else {
                return false;
            };
            let now = self.now;
            self.update(AppMsg::Snapshot(Box::new(Ok(snapshot))), now);
            true
        }
    }

    fn snap_msg() -> AppMsg {
        AppMsg::Snapshot(Box::new(Ok(snapshot())))
    }

    fn key_msg(c: char) -> AppMsg {
        AppMsg::Input(TermEvent::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::NONE,
        )))
    }

    fn mouse_msg(kind: MouseEventKind, column: u16, row: u16) -> AppMsg {
        AppMsg::Input(TermEvent::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }))
    }

    /// The loop's draw step on a test backend, counting frames.
    struct Screen {
        terminal: Terminal<TestBackend>,
        draws: usize,
    }

    impl Screen {
        fn new() -> Self {
            Self {
                terminal: Terminal::new(TestBackend::new(160, 45)).unwrap(),
                draws: 0,
            }
        }

        fn deliver(&mut self, app: &mut App, msgs: Vec<AppMsg>, now: Instant) {
            let (tx, rx) = channel();
            for msg in msgs {
                tx.send(msg).unwrap();
            }
            if let Ok(first) = rx.try_recv() {
                drain(app, first, &rx, now);
            }
            self.frame(app, now);
        }

        fn frame(&mut self, app: &mut App, now: Instant) {
            if app.pacer.due(now) {
                self.terminal.draw(|frame| app.draw(frame)).unwrap();
                app.pacer.drawn(now);
                self.draws += 1;
            }
        }
    }

    #[test]
    fn new_app_loads() {
        let app = app();
        assert!(app.ready, "first scripted sample is applied");
        assert!(app.expanded.is_none());
        assert!(!app.histories.get(SeriesId::Cpu).is_empty());
        assert!(!app.histories.get(SeriesId::NetRx).is_empty());
    }

    #[test]
    fn draws_equal_snapshots_when_idle() {
        let mut app = scripted(Vec::new());
        let mut screen = Screen::new();
        let t0 = app.now;
        for i in 0..20_u32 {
            let now = t0 + i * 500 * MS;
            screen.deliver(&mut app, vec![snap_msg()], now);
            assert_eq!(app.next_deadline(), None, "idle loop blocks until data");
        }
        assert_eq!(screen.draws, 20);
    }

    #[test]
    fn queued_messages_coalesce_into_one_draw() {
        let mut app = scripted(Vec::new());
        let mut screen = Screen::new();
        let now = app.now;
        screen.deliver(
            &mut app,
            vec![snap_msg(), snap_msg(), key_msg('j'), snap_msg()],
            now,
        );
        assert_eq!(screen.draws, 1);
        assert_eq!(
            app.histories.get(SeriesId::Cpu).len(),
            3,
            "every queued sample is kept"
        );
    }

    #[test]
    fn every_snapshot_feeds_the_history_recorder() {
        let app = app();
        assert_eq!(app.recorder.open_samples(), 1);
        assert!(!app.recorder.persistent(), "tests never touch the disk");
    }

    /// Regression: a sample already in flight when `p` froze the display
    /// was dropped from history as well as from the screen.
    #[test]
    fn a_frozen_display_still_records_the_in_flight_sample() {
        let mut app = app();
        let t0 = app.now;
        app.update(key_msg('p'), t0);
        assert!(app.frozen);
        let before = app.recorder.open_samples();
        app.update(snap_msg(), t0);
        let after = app.recorder.open_samples();
        // Either the sample joined the open bucket or it opened a new one.
        assert!(after == before + 1 || after == 1, "{before} -> {after}");
    }

    #[test]
    fn frozen_app_draws_nothing_without_input() {
        let mut app = app();
        let mut screen = Screen::new();
        let t0 = app.now;
        screen.frame(&mut app, t0);
        screen.deliver(&mut app, vec![key_msg('p')], t0 + 20 * MS);
        assert!(app.frozen);
        let before = screen.draws;
        for i in 1..=20_u32 {
            let now = t0 + i * 500 * MS;
            screen.deliver(&mut app, vec![snap_msg()], now);
        }
        assert_eq!(screen.draws, before, "a sample in flight is dropped");
        assert_eq!(app.next_deadline(), None);
    }

    #[test]
    fn pointer_motion_causes_no_draws() {
        let mut app = app();
        let mut screen = Screen::new();
        let t0 = app.now;
        screen.frame(&mut app, t0);
        for i in 0..1200_u32 {
            let now = t0 + i * 8 * MS;
            screen.deliver(
                &mut app,
                vec![mouse_msg(MouseEventKind::Moved, 10 + (i % 50) as u16, 10)],
                now,
            );
        }
        assert_eq!(screen.draws, 1);
    }

    /// A lit button on the dashboard: a cell hover lights, plus its rect.
    fn some_button(app: &App) -> (u16, u16, Rect) {
        let area = app.last_area;
        area.positions()
            .find_map(|p| siltop_ui::hover_rect(&app.view(), area, p.x, p.y).map(|r| (p.x, p.y, r)))
            .expect("the dashboard has a button")
    }

    #[test]
    fn hover_redraws_only_when_the_target_changes() {
        let mut app = app();
        let mut screen = Screen::new();
        let t0 = app.now;
        screen.frame(&mut app, t0);
        let (x, y, rect) = some_button(&app);
        let at = |app: &mut App, screen: &mut Screen, i: u32, col: u16, row: u16| {
            screen.deliver(
                app,
                vec![mouse_msg(MouseEventKind::Moved, col, row)],
                t0 + i * 40 * MS,
            );
        };
        at(&mut app, &mut screen, 1, x, y);
        assert_eq!(screen.draws, 2, "entering a button draws once");
        assert_eq!(app.hover, Some(rect));
        for (i, col) in (rect.x..rect.x + rect.width).enumerate() {
            at(&mut app, &mut screen, 2 + i as u32, col, y);
        }
        assert_eq!(screen.draws, 2, "moving within one button never draws");
        let away = if y > 0 { y - 1 } else { y + 1 };
        let off = (0..app.last_area.width)
            .find(|c| siltop_ui::hover_rect(&app.view(), app.last_area, *c, away).is_none())
            .unwrap();
        at(&mut app, &mut screen, 20, off, away);
        assert_eq!(screen.draws, 3, "leaving draws once");
        assert_eq!(app.hover, None);
        at(&mut app, &mut screen, 21, off, away);
        assert_eq!(screen.draws, 3);
    }

    #[test]
    fn a_click_clears_a_stale_hover() {
        let mut app = app();
        let mut screen = Screen::new();
        let t0 = app.now;
        screen.frame(&mut app, t0);
        let (x, y, _) = some_button(&app);
        app.update(mouse_msg(MouseEventKind::Moved, x, y), app.now);
        assert!(app.hover.is_some());
        app.update(key_msg('?'), app.now);
        assert_eq!(app.hover, None);
    }

    #[test]
    fn toast_expiry_redraws_exactly_once() {
        let mut app = app();
        let mut screen = Screen::new();
        let t0 = app.now;
        app.set_toast(String::from("sent TERM"), ToastKind::Info, STATUS_TTL);
        screen.frame(&mut app, t0);
        assert_eq!(screen.draws, 1);
        let expiry = app.next_deadline().expect("toast deadline");
        assert_eq!(expiry, t0 + STATUS_TTL);
        screen.deliver(
            &mut app,
            vec![AppMsg::Timer],
            expiry.checked_sub(MS).unwrap(),
        );
        assert_eq!(screen.draws, 1, "nothing to draw before expiry");
        screen.deliver(&mut app, vec![AppMsg::Timer], expiry);
        assert_eq!(screen.draws, 2);
        assert!(app.status.is_none());
        assert_eq!(app.next_deadline(), None);
        screen.deliver(&mut app, vec![AppMsg::Timer], expiry + MS);
        assert_eq!(screen.draws, 2);
    }

    #[test]
    fn drag_redraws_are_capped_at_60_per_second() {
        let mut app = app();
        let mut screen = Screen::new();
        let t0 = app.now;
        screen.frame(&mut app, t0);
        let drag = MouseEventKind::Drag(MouseButton::Left);
        for i in 1..=100_u32 {
            let now = t0 + i * MS;
            screen.deliver(&mut app, vec![mouse_msg(drag, 50, 10)], now);
        }
        assert!(screen.draws <= 8, "{} draws in 100 ms", screen.draws);
        let pending = app.next_deadline().expect("last drag still owed a frame");
        screen.deliver(&mut app, vec![AppMsg::Timer], pending);
        assert_eq!(app.next_deadline(), None);
    }

    #[test]
    fn first_frame_waits_for_data_or_the_hold() {
        let mut app = scripted(Vec::new());
        let mut screen = Screen::new();
        let t0 = app.now;
        screen.frame(&mut app, t0);
        assert_eq!(screen.draws, 0, "no empty frame before the first sample");
        assert_eq!(app.next_deadline(), Some(t0 + crate::msg::FIRST_FRAME_HOLD));
        screen.deliver(
            &mut app,
            vec![AppMsg::Timer],
            t0 + crate::msg::FIRST_FRAME_HOLD,
        );
        assert_eq!(screen.draws, 1, "the hold gives up and draws");
    }

    #[test]
    fn quit_key_and_closed_input_stop_the_loop() {
        let mut app = app();
        let now = app.now;
        app.update(key_msg('q'), now);
        assert!(app.quit);
        let mut app = scripted(Vec::new());
        app.update(AppMsg::InputClosed(String::from("tty gone")), now);
        assert!(app.fatal.is_some());
    }

    #[test]
    fn brackets_change_the_interval_both_ways() {
        let mut app = app();
        let now = app.now;
        assert_eq!(app.config.interval, 500 * MS);
        app.update(key_msg('['), now);
        assert_eq!(app.config.interval, 1000 * MS);
        app.update(key_msg(']'), now);
        app.update(key_msg(']'), now);
        assert_eq!(app.config.interval, 250 * MS);
    }

    #[test]
    fn z_cycles_graph_zoom_from_the_view_default() {
        use siltop_core::Zoom;
        let mut app = app();
        let now = app.now;
        assert_eq!(app.view().zoom(), Zoom::Live);
        for want in [Zoom::FiveMin, Zoom::FifteenMin, Zoom::OneMin, Zoom::Live] {
            app.update(key_msg('z'), now);
            assert_eq!(app.view().zoom(), want);
        }
        app.expanded = Some(Panel::Cpu);
        assert_eq!(app.view().zoom(), Zoom::FiveMin);
        app.update(key_msg('z'), now);
        assert_eq!(app.view().zoom(), Zoom::FifteenMin);
    }

    #[test]
    fn expanded_zoom_does_not_move_the_shared_compact_window() {
        use siltop_core::Zoom;
        let mut app = app();
        let now = app.now;
        app.update(key_msg('z'), now);
        assert_eq!(app.view().zoom(), Zoom::FiveMin);
        app.expanded = Some(Panel::Cpu);
        assert_eq!(
            app.view().zoom(),
            Zoom::FiveMin,
            "expanded keeps its default"
        );
        app.update(key_msg('z'), now);
        app.update(key_msg('z'), now);
        assert_eq!(app.view().zoom(), Zoom::OneMin);
        app.expanded = None;
        assert_eq!(app.view().zoom(), Zoom::FiveMin, "compact pick survives");
    }

    #[test]
    fn interval_change_keeps_history_and_stamps_move_forward() {
        // Regression D21: 250 -> 500 ms used to drop half the history.
        let mut app = scripted((0..8).map(|_| snapshot()).collect());
        let now = app.now;
        app.update(key_msg(']'), now);
        for _ in 0..4 {
            assert!(app.pump());
        }
        let before = app.histories.get(SeriesId::Cpu).len();
        let stamp = app.clock;
        app.update(key_msg('['), now);
        assert_eq!(app.histories.get(SeriesId::Cpu).len(), before);
        assert!(app.pump());
        assert!(app.clock > stamp, "{:?} !> {stamp:?}", app.clock);
        assert_eq!(app.histories.get(SeriesId::Cpu).newest(), app.clock);
        assert_eq!(app.view().clock, app.clock);
    }

    #[test]
    fn has_fans_when_sensors_present() {
        let mut app = app();
        app.snapshot.fans = siltop_core::FanSnapshot::default();
        app.snapshot.sensors.cpu_c = Some(42.0);
        assert!(app.layout_flags().has_fans);
        assert!(app.view().flags().has_fans);
    }

    #[test]
    fn hop_related_sets_expanded_and_focus() {
        let mut app = app();
        app.snapshot.gpu = Some(siltop_core::GpuSnapshot {
            scaled: 0.2,
            ..siltop_core::GpuSnapshot::default()
        });
        app.snapshot.sensors.cpu_c = Some(40.0);
        app.expanded = Some(Panel::Cpu);
        app.focus = Focus::Cpu;
        app.hop_related(1);
        assert_eq!(app.expanded, Some(Panel::Gpu));
        assert_eq!(app.focus, Focus::Gpu);
    }

    #[test]
    fn hop_related_one_member_is_noop() {
        let mut app = app();
        app.config.show_disk = false;
        app.expanded = Some(Panel::Net);
        app.focus = Focus::Net;
        app.hop_related(1);
        assert_eq!(app.expanded, Some(Panel::Net));
        assert_eq!(app.focus, Focus::Net);
    }

    #[test]
    fn tick_stores_raw_history_values() {
        let mut app = app();
        let mut snap = app.snapshot.clone();
        snap.cpu.active = 0.42;
        snap.cpu.scaled = 0.42;
        snap.network.rx_bps = 1_250;
        snap.network.tx_bps = 800;
        snap.memory.used_bytes = 8;
        snap.memory.total_bytes = 16;
        app.apply_snapshot(snap);
        assert_eq!(app.histories.get(SeriesId::Cpu).last(), Some(0.42));
        assert_eq!(app.histories.get(SeriesId::NetRx).last(), Some(1_250.0));
        assert_eq!(app.histories.get(SeriesId::NetTx).last(), Some(800.0));
        let mem = if app.snapshot.memory.total_bytes == 0 {
            0.0
        } else {
            app.snapshot.memory.used_bytes as f32 / app.snapshot.memory.total_bytes as f32
        };
        assert_eq!(app.histories.get(SeriesId::Mem).last(), Some(mem));
    }

    #[test]
    fn desktop_without_power_readings_hides_the_power_box() {
        let mut app = app();
        app.snapshot.battery = siltop_core::BatterySnapshot::default();
        app.snapshot.power = siltop_core::PowerSnapshot::default();
        assert!(!app.layout_flags().visible(Panel::Power));
        assert!(!app.view().flags().visible(Panel::Power));
        app.snapshot.power.system = Some(siltop_core::Reading::measured(20.0));
        assert!(app.layout_flags().visible(Panel::Power));
        app.config.show_power = false;
        assert!(!app.view().flags().visible(Panel::Power));
    }

    #[test]
    fn laptop_shows_the_power_box_by_default() {
        let mut app = app();
        app.snapshot.battery.present = true;
        assert!(app.layout_flags().visible(Panel::Power));
        assert_eq!(app.layout_flags(), app.view().flags());
    }

    #[test]
    fn power_histories_never_record_missing_or_held_values() {
        let mut app = app();
        let mut snap = app.snapshot.clone();
        snap.power = siltop_core::PowerSnapshot::default();
        snap.battery = siltop_core::BatterySnapshot::default();
        app.apply_snapshot(snap);
        assert!(app.histories.get(SeriesId::PowerSystem).is_empty());
        assert!(app.histories.get(SeriesId::BatteryPct).is_empty());
        assert!(app.histories.get(SeriesId::BatteryWatts).is_empty());

        let mut snap = app.snapshot.clone();
        snap.power.system = Some(siltop_core::Reading {
            watts: 9.0,
            source: siltop_core::ReadingSource::Held {
                age: std::time::Duration::from_secs(3),
            },
        });
        app.apply_snapshot(snap);
        assert!(
            app.histories.get(SeriesId::PowerSystem).is_empty(),
            "held is not new"
        );

        let mut snap = app.snapshot.clone();
        snap.power.system = Some(siltop_core::Reading::measured(21.5));
        snap.battery.present = true;
        snap.battery.percent = Some(74);
        snap.battery.watts = Some(-11.0);
        app.apply_snapshot(snap);
        assert_eq!(app.histories.get(SeriesId::PowerSystem).last(), Some(21.5));
        assert_eq!(app.histories.get(SeriesId::BatteryPct).last(), Some(0.74));
        // History holds magnitudes; the cell title carries ▲/▼.
        assert_eq!(app.histories.get(SeriesId::BatteryWatts).last(), Some(11.0));
    }

    #[test]
    fn power_joins_the_cpu_gpu_heat_hop_ring() {
        let flags = LayoutFlags::default();
        for panel in [Panel::Cpu, Panel::Gpu, Panel::Fans, Panel::Power] {
            assert!(hop_ring(flags, panel).contains(&Panel::Power), "{panel:?}");
            assert!(hop_ring(flags, panel).contains(&Panel::Fans), "{panel:?}");
        }
    }

    #[test]
    fn missing_option_metrics_do_not_zero_fill_history() {
        let mut app = app();
        let cpu_len = app.histories.get(SeriesId::CpuTemp).len();
        let gpu_len = app.histories.get(SeriesId::GpuTemp).len();
        let gpu_util_len = app.histories.get(SeriesId::Gpu).len();
        let mut snap = app.snapshot.clone();
        snap.cpu.temp_c = None;
        snap.sensors = siltop_core::SensorsSnapshot::default();
        snap.gpu = None;
        snap.disk.read_bps = 1_024;
        snap.disk.write_bps = 512;
        app.apply_snapshot(snap);
        assert_eq!(app.histories.get(SeriesId::CpuTemp).len(), cpu_len);
        assert_eq!(app.histories.get(SeriesId::GpuTemp).len(), gpu_len);
        assert_eq!(app.histories.get(SeriesId::Gpu).len(), gpu_util_len);
        assert_eq!(app.histories.get(SeriesId::Disk).last(), Some(1_536.0));
        assert_eq!(app.histories.get(SeriesId::DiskRead).last(), Some(1_024.0));
        assert_eq!(app.histories.get(SeriesId::DiskWrite).last(), Some(512.0));

        let mut snap = app.snapshot.clone();
        snap.cpu.temp_c = Some(42.0);
        snap.gpu = Some(siltop_core::GpuSnapshot {
            scaled: 0.25,
            temp_c: Some(51.0),
            ..siltop_core::GpuSnapshot::default()
        });
        app.apply_snapshot(snap);
        assert_eq!(app.histories.get(SeriesId::CpuTemp).last(), Some(42.0));
        assert_eq!(app.histories.get(SeriesId::GpuTemp).last(), Some(51.0));
        assert_eq!(app.histories.get(SeriesId::Gpu).last(), Some(0.25));
    }

    #[test]
    fn stale_sensor_tick_does_not_extend_temp_history() {
        let mut app = app();
        let before = app.histories.get(SeriesId::CpuTemp).len();
        let mut snap = app.snapshot.clone();
        snap.sampled.sensors = false;
        snap.cpu.temp_c = Some(88.0);
        snap.sensors.cpu_c = Some(88.0);
        snap.sensors.e_c = Some(40.0);
        app.apply_snapshot(snap);
        assert_eq!(app.histories.get(SeriesId::CpuTemp).len(), before);
        assert_ne!(app.histories.get(SeriesId::CpuTemp).last(), Some(88.0));
    }

    #[test]
    fn cluster_load_history_skips_missing_family() {
        let mut app = app();
        let mut snap = app.snapshot.clone();
        snap.cpu.s_cluster = Some(siltop_core::Cluster {
            kind: siltop_core::ClusterKind::Super,
            scaled: 0.81,
            active: 0.81,
            freq_mhz: 0,
        });
        snap.cpu.e_cluster = None;
        let e_len = app.histories.get(SeriesId::ELoad).len();
        app.apply_snapshot(snap);
        assert_eq!(app.histories.get(SeriesId::SLoad).last(), Some(0.81));
        assert_eq!(app.histories.get(SeriesId::ELoad).len(), e_len);
    }

    /// A pid nothing on this machine is using. macOS stays under 100000, but
    /// Linux `pid_max` is often in the millions, so a fixed high number can
    /// be a live process and the kill tests then refuse to let go of it.
    fn safe(i: u32) -> u32 {
        let mut pid = 900_000 + i;
        for _ in 0..64 {
            if !pid_in_use(pid) {
                return pid;
            }
            pid = pid.saturating_add(10_000);
        }
        pid
    }

    fn pid_in_use(pid: u32) -> bool {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // SAFETY: `kill` with signal 0 only checks that the pid exists.
        let rc = unsafe { libc::kill(pid, 0) };
        if rc == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }

    fn procs(n: u32) -> Vec<Process> {
        (1..=n)
            .map(|i| proc(safe(i), &format!("proc{i}"), 50.0 - i as f32 * 0.01))
            .collect()
    }

    impl App {
        fn send(&mut self, action: Action) {
            self.reduce(action);
            self.settle_procs();
        }

        fn at(&mut self, cols: u16, rows: u16) {
            self.config.surface = Some(Surface::Work);
            self.surface = Surface::Work;
            self.last_area = Rect::new(0, 0, cols, rows);
            self.settle_procs();
        }

        fn set_processes(&mut self, processes: Vec<Process>) {
            self.snapshot.processes = processes;
            self.proc.generation += 1;
            self.settle_procs();
        }

        fn pane(&self) -> Rect {
            plan(
                self.last_area,
                self.effective_surface(),
                self.layout_flags(),
            )
            .processes
            .expect("work surface has a process pane")
        }

        fn screen(&mut self) -> Vec<String> {
            let area = self.last_area;
            let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
            terminal.draw(|frame| self.draw(frame)).unwrap();
            let buf = terminal.backend().buffer();
            (0..area.height)
                .map(|y| (0..area.width).map(|x| buf[(x, y)].symbol()).collect())
                .collect()
        }

        fn click(&mut self, col: u16, row: u16) {
            self.send(Action::Click { col, row });
        }
    }

    fn column_of(line: &str, needle: &str) -> u16 {
        let at = line
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} not in {line:?}"));
        u16::try_from(line[..at].chars().count()).unwrap()
    }

    #[test]
    fn pinned_selection_follows_its_pid_after_resort() {
        let mut app = app();
        app.at(120, 40);
        app.set_processes(vec![proc(safe(1), "low", 1.0), proc(safe(2), "high", 90.0)]);
        app.send(Action::Move(1));
        assert_eq!(app.proc.selected_pid(), Some(safe(1)));
        let mut next = app.snapshot.processes.clone();
        next[0].cpu = Some(99.0);
        next[1].cpu = Some(1.0);
        app.set_processes(next);
        assert_eq!(app.proc.selected_pid(), Some(safe(1)));
        assert_eq!(app.proc.selected_row(&app.proc.model), Some(0));
    }

    #[test]
    fn startup_selection_stays_on_the_top_row() {
        let mut app = scripted(Vec::new());
        app.at(120, 40);
        let mut first = snapshot();
        for p in &mut first.processes {
            p.cpu = Some(0.0);
        }
        app.update(AppMsg::Snapshot(Box::new(Ok(first))), app.now);
        assert_eq!(app.proc.anchor, Anchor::Top);
        let mut second = snapshot();
        second.processes[39].cpu = Some(99.0);
        let heaviest = second.processes[39].pid;
        app.update(AppMsg::Snapshot(Box::new(Ok(second))), app.now);
        assert_eq!(app.proc.selected_pid(), Some(heaviest), "top follows row 0");
        assert_eq!(app.proc.offset, 0);
    }

    #[test]
    fn sort_filter_and_tree_changes_return_to_the_top() {
        let mut app = app();
        app.at(120, 40);
        app.set_processes(procs(200));
        for reset in [
            Action::CycleSort,
            Action::FilterChar('p'),
            Action::FilterBackspace,
            Action::ToggleTree,
        ] {
            app.searching = false;
            app.focus = Focus::Processes;
            for _ in 0..60 {
                app.send(Action::Move(1));
            }
            assert!(app.proc.offset > 0);
            app.send(reset);
            assert_eq!(app.proc.anchor, Anchor::Top, "{reset:?}");
            assert_eq!(app.proc.offset, 0, "{reset:?}");
        }
    }

    #[test]
    fn a_filter_that_matches_nothing_cannot_arm_a_kill() {
        let mut app = app();
        app.at(120, 40);
        app.set_processes(procs(40));
        app.send(Action::Move(3));
        app.send(Action::Search);
        for c in "proc1".chars() {
            app.send(Action::FilterChar(c));
        }
        assert_eq!(app.proc.model.len(), 11, "proc1 and proc10..proc19");
        for c in "zzz".chars() {
            app.send(Action::FilterChar(c));
        }
        assert!(app.proc.model.is_empty());
        assert_eq!(app.proc.selected_pid(), None);
        app.send(Action::Search);
        app.send(Action::Kill);
        assert!(!app.confirm_kill, "nothing visible, nothing to kill");
        assert!(app.confirm.is_none());
        assert_eq!(
            app.status.as_ref().map(|(m, _)| m.as_str()),
            Some("no process selected")
        );
        app.send(Action::Open);
        assert!(app.detail_pid.is_none());
    }

    #[test]
    fn scrolled_clicks_hit_the_row_under_the_pointer() {
        let mut app = app();
        app.at(120, 40);
        app.set_processes(procs(200));
        let pane = app.pane();
        for _ in 0..7 {
            app.send(Action::Scroll {
                col: pane.x + 5,
                row: pane.y + 10,
                delta: 1,
            });
        }
        assert_eq!(app.proc.offset, 21);
        for y in [pane.y + 3, pane.y + 12, pane.y + pane.height - 2] {
            let lines = app.screen();
            app.click(pane.x + 20, y);
            let pid = app.proc.selected_pid().expect("a click selects");
            assert!(
                lines[usize::from(y)].contains(&format!(" {pid} ")),
                "clicked {:?}, selected {pid}",
                lines[usize::from(y)]
            );
            assert!(app.detail_pid.is_none(), "one click only selects");
        }
        let before = app.proc.anchor;
        app.last_click = None;
        app.click(pane.x + 20, pane.y + pane.height - 1);
        assert_eq!(app.proc.anchor, before, "the bottom border is not a row");
        assert!(app.detail_pid.is_none());
    }

    #[test]
    fn k_inside_the_viewport_keeps_it_still() {
        let mut app = app();
        app.at(120, 40);
        app.set_processes(procs(200));
        for _ in 0..40 {
            app.send(Action::Move(1));
        }
        let offset = app.proc.offset;
        assert!(offset > 0);
        for _ in 0..3 {
            app.send(Action::Move(-1));
            assert_eq!(app.proc.offset, offset);
        }
        let title = &app.screen()[usize::from(app.pane().y)];
        assert!(title.contains(&format!("↑ {offset}")), "{title}");
    }

    #[test]
    fn header_clicks_sort_by_that_column_and_flip_it() {
        let mut app = app();
        app.at(120, 40);
        app.set_processes(procs(50));
        let header_y = app.pane().y + 2;
        let header = app.screen()[usize::from(header_y)].clone();
        app.click(column_of(&header, "mem"), header_y);
        assert_eq!(app.config.proc_sort, ProcSort::Mem);
        assert!(!app.sort_reversed());
        app.click(column_of(&header, "mem"), header_y);
        assert!(app.sort_reversed(), "the active column flips");
        let header = app.screen()[usize::from(header_y)].clone();
        assert!(header.contains("mem↑"), "{header}");
        app.click(column_of(&header, "pid"), header_y);
        assert_eq!(app.config.proc_sort, ProcSort::Pid);
        assert!(!app.sort_reversed());
        assert!(app.screen()[usize::from(header_y)].contains("pid↑"));
        app.click(column_of(&header, "name"), header_y);
        assert_eq!(app.config.proc_sort, ProcSort::Name, "name sorts too");
        assert!(app.screen()[usize::from(header_y)].contains("name↑"));
    }

    #[test]
    fn angle_brackets_cycle_every_sort_key_and_r_reverses() {
        let mut app = app();
        app.at(120, 40);
        app.set_processes(procs(20));
        assert_eq!(app.config.proc_sort, ProcSort::Cpu);
        for want in ProcSort::ALL.into_iter().skip(1) {
            app.send(Action::SortNext);
            assert_eq!(app.config.proc_sort, want);
        }
        app.send(Action::SortNext);
        assert_eq!(app.config.proc_sort, ProcSort::Cpu, "wraps around");
        app.send(Action::SortPrev);
        assert_eq!(app.config.proc_sort, ProcSort::Started, "and back");
        app.send(Action::SortPrev);
        assert_eq!(app.config.proc_sort, ProcSort::Wakeups);
        let before = app.sort_reversed();
        app.send(Action::ReverseSort);
        assert_eq!(app.sort_reversed(), !before);
        app.send(Action::SortNext);
        assert!(!app.sort_reversed(), "a new column starts its natural way");
    }

    #[test]
    fn footer_chips_work_in_every_mode() {
        let mut app = app();
        app.at(120, 40);
        app.set_processes(procs(10));
        let footer_y = 39;
        app.send(Action::Kill);
        assert!(app.confirm_kill);
        let footer = app.screen()[usize::from(footer_y)].clone();
        app.click(column_of(&footer, "n cancel") + 1, footer_y);
        assert!(!app.confirm_kill, "clicks reach the confirm prompt");
        app.send(Action::Kill);
        let footer = app.screen()[usize::from(footer_y)].clone();
        app.click(column_of(&footer, "y yes") + 1, footer_y);
        assert!(!app.confirm_kill);
        assert!(app.status.is_some(), "the no-such-pid error is reported");
        let footer = app.screen()[usize::from(footer_y)].clone();
        app.click(column_of(&footer, "q quit") + 1, footer_y);
        assert!(app.quit, "q quit is a real button");
    }

    #[test]
    fn footer_hits_match_a_work_lock_that_fell_back_to_glance() {
        let mut app = app();
        app.at(50, 20);
        let footer = app.screen()[19].clone();
        assert!(!footer.contains("f filter"), "{footer}");
        // where a Work footer would put `f filter`: the drawn chips answer
        for col in 0..50 {
            app.click(col, 19);
            assert!(
                !app.searching,
                "{col}: the drawn chip, not a Work-surface chip"
            );
            app.quit = false;
            app.menu = None;
            app.help = false;
            app.settings = None;
        }
        app.click(column_of(&footer, "q quit") + 2, 19);
        assert!(app.quit, "{footer}");
    }

    #[test]
    fn small_window_regression_v_says_work_does_not_fit() {
        let mut app = app();
        app.at(55, 20);
        let lock = app.surface_lock;
        app.send(Action::CycleSurface);
        app.send(Action::CycleSurface);
        assert_eq!(app.surface_lock, lock, "no invisible lock flips");
        assert_eq!(
            app.status.as_ref().map(|(m, _)| m.as_str()),
            Some(siltop_ui::WORK_TOO_SMALL)
        );
        app.status = None;
        app.apply_hit(Hit::Work);
        assert_eq!(app.surface_lock, lock);
        assert!(app.status.is_some(), "a click on work says why too");
        app.at(120, 40);
        app.send(Action::CycleSurface);
        assert_eq!(
            app.surface_lock,
            Some(Surface::Glance),
            "steps from what shows"
        );
        app.send(Action::CycleSurface);
        assert_eq!(app.surface_lock, Some(Surface::Work));
    }

    #[test]
    fn dossier_regression_closes_when_a_resize_hides_the_pane() {
        let mut app = app();
        app.at(120, 40);
        app.focus = Focus::Processes;
        app.send(Action::Open);
        assert!(app.detail_pid.is_some(), "dossier open");
        app.send(Action::Resize { cols: 55, rows: 20 });
        assert!(
            app.detail_pid.is_none(),
            "no card on screen, no dossier keys"
        );
        let footer = app.screen()[19].clone();
        assert!(!footer.contains("x quit"), "{footer}");
    }

    #[test]
    fn wheel_scrolls_only_the_table_under_the_pointer() {
        let mut app = app();
        app.at(120, 40);
        app.set_processes(procs(200));
        app.focus = Focus::Cpu;
        let pane = app.pane();
        app.send(Action::Scroll {
            col: pane.x + 5,
            row: pane.y + 10,
            delta: 1,
        });
        assert_eq!(app.proc.offset, WHEEL_ROWS as usize);
        assert_eq!(app.proc.anchor, Anchor::Top, "selection untouched");
        assert_eq!(app.focus, Focus::Cpu, "focus untouched");
        let cpu = plan(app.last_area, Surface::Work, app.layout_flags())
            .cpu
            .unwrap();
        app.send(Action::Scroll {
            col: cpu.x + 3,
            row: cpu.y + 3,
            delta: 1,
        });
        assert_eq!(app.proc.offset, WHEEL_ROWS as usize);
        assert_eq!(app.focus, Focus::Cpu);
    }

    #[test]
    fn click_selects_and_double_click_opens() {
        let mut app = app();
        app.at(120, 40);
        app.set_processes(procs(20));
        let y = app.pane().y + 5;
        let x = app.pane().x + 20;
        let t0 = app.now;
        app.click(x, y);
        let pid = app.proc.selected_pid();
        assert!(pid.is_some());
        assert!(app.detail_pid.is_none());
        app.now = t0 + 500 * MS;
        app.click(x, y);
        assert!(app.detail_pid.is_none(), "too slow for a double click");
        app.now = t0 + 800 * MS;
        app.click(x, y);
        assert_eq!(app.detail_pid, pid, "second click within 400 ms opens");
    }

    #[test]
    fn e_expands_and_collapses_the_process_table() {
        let mut app = app();
        app.at(120, 40);
        app.focus = Focus::Processes;
        app.send(Action::Expand);
        assert_eq!(app.expanded, Some(Panel::Processes));
        app.send(Action::Expand);
        assert!(app.expanded.is_none());
        app.send(Action::Open);
        assert!(app.detail_pid.is_some(), "enter opens the dossier");
    }

    #[test]
    fn dossier_k_moves_up_and_capital_k_arms_kill() {
        let mut app = app();
        app.at(120, 40);
        app.set_processes(procs(20));
        app.send(Action::Move(5));
        app.send(Action::Open);
        let opened = app.detail_pid.expect("dossier open");
        let key = |c: char| {
            keymap::resolve(
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                app.mode(),
                app.profile,
            )
        };
        let up = key('k').unwrap();
        app.send(up);
        assert_ne!(app.detail_pid, Some(opened), "k moved up");
        assert!(!app.confirm_kill, "k never arms a kill");
        let kill = keymap::resolve(
            KeyEvent::new(KeyCode::Char('K'), KeyModifiers::NONE),
            app.mode(),
            app.profile,
        )
        .unwrap();
        app.send(kill);
        assert!(app.confirm_kill);
        assert_eq!(app.pending_signal, Signal::Kill);
        assert_eq!(app.confirm.map(|t| t.pid), app.detail_pid);
    }

    #[test]
    fn a_pinned_process_that_exits_returns_to_the_top_with_a_notice() {
        let mut app = scripted(Vec::new());
        app.at(120, 40);
        app.update(snap_msg(), app.now);
        app.send(Action::Move(4));
        let Anchor::Pinned(gone) = app.proc.anchor else {
            panic!("move pins");
        };
        let mut next = snapshot();
        next.processes.retain(|p| p.pid != gone);
        app.update(AppMsg::Snapshot(Box::new(Ok(next))), app.now);
        assert_eq!(app.proc.anchor, Anchor::Top);
        let status = app
            .status
            .as_ref()
            .map(|(m, _)| m.clone())
            .unwrap_or_default();
        assert!(status.ends_with(&format!("({gone}) exited")), "{status}");
    }

    #[test]
    fn model_rebuilds_only_for_new_processes_filter_or_sort() {
        let mut app = scripted(Vec::new());
        app.at(120, 40);
        app.update(snap_msg(), app.now);
        let base = app.proc.model.rebuilds();
        for _ in 0..10 {
            app.update(AppMsg::Timer, app.now);
            app.update(key_msg('j'), app.now);
            app.screen();
        }
        assert_eq!(
            app.proc.model.rebuilds(),
            base,
            "moves and frames reuse the model"
        );
        app.update(snap_msg(), app.now);
        assert_eq!(app.proc.model.rebuilds(), base + 1);
        let mut stale = snapshot();
        stale.sampled.procs = false;
        app.update(AppMsg::Snapshot(Box::new(Ok(stale))), app.now);
        assert_eq!(app.proc.model.rebuilds(), base + 1, "cached process list");
        app.send(Action::CycleSort);
        assert_eq!(app.proc.model.rebuilds(), base + 2);
    }

    #[test]
    fn click_box_focuses_without_expand() {
        let mut app = app();
        app.at(120, 30);
        let planned = plan(app.last_area, app.effective_surface(), app.layout_flags());
        let cpu = planned.cpu.expect("work surface always has cpu");
        let inner = (cpu.x + 2, cpu.y + 1);
        assert_eq!(
            hit_test(
                app.last_area,
                app.effective_surface(),
                app.layout_flags(),
                inner.0,
                inner.1
            ),
            Some(Hit::Panel(Panel::Cpu))
        );
        app.click(inner.0, inner.1);
        assert_eq!(app.focus, Focus::Cpu);
        assert!(app.expanded.is_none());
        let corner = LayoutPlan::corner_hit(cpu).expect("cpu pane is wide enough");
        app.click(corner.x, corner.y);
        assert_eq!(app.expanded, Some(Panel::Cpu));
    }

    #[test]
    fn search_question_is_not_a_letter() {
        let mut app = app();
        app.reduce(Action::Search);
        assert!(app.searching);
        app.reduce(Action::Help);
        assert!(app.help);
    }

    #[test]
    fn expand_collapse_and_next_panel() {
        let mut app = app();
        app.focus = Focus::Cpu;
        assert!(app.expanded.is_none());

        app.reduce(Action::Open);
        assert_eq!(app.expanded, Some(Panel::Cpu));

        app.reduce(Action::Cancel);
        assert!(app.expanded.is_none());

        app.reduce(Action::Expand);
        assert_eq!(app.expanded, Some(Panel::Cpu));
        app.reduce(Action::Cancel);
        assert!(app.expanded.is_none());

        let panels = app.view().flags().visible_panels();
        let idx = panels.iter().position(|&p| p == app.focus.panel()).unwrap();
        let expected = Focus::from_panel(panels[(idx + 1) % panels.len()]);
        app.reduce(Action::NextPanel);
        assert_eq!(app.focus, expected);
        assert!(app.expanded.is_none());

        let focused = app.focus.panel();
        app.reduce(Action::Expand);
        assert_eq!(app.expanded, Some(focused));
        app.focus = Focus::Processes;
        app.reduce(Action::Move(1));
        app.reduce(Action::Move(-1));
        assert_eq!(
            app.expanded,
            Some(focused),
            "moves must not switch the expanded panel"
        );
    }

    fn open_menu_on(app: &mut App, pid: u32) {
        app.detail_pid = Some(pid);
        app.reduce(Action::SignalMenu);
        assert!(app.signal_menu.is_some(), "X opens the picker");
        assert_eq!(app.mode(), Mode::SignalMenu);
    }

    #[test]
    fn signal_menu_lists_every_signal_for_the_dossier_pid() {
        let mut app = app();
        app.set_processes(vec![proc(safe(11), "target", 1.0)]);
        open_menu_on(&mut app, safe(11));
        let menu = app.signal_menu.as_ref().expect("menu");
        assert_eq!(menu.rows.len(), Signal::ALL.len());
        assert_eq!(menu.target.pid, safe(11));
        assert!(menu.label.contains("target"), "{}", menu.label);
        let view = app.view();
        let shown = view.signal_menu.expect("view carries the menu");
        assert_eq!(shown.rows.len(), Signal::ALL.len());
    }

    #[test]
    fn signal_menu_pick_stop_arms_a_confirm_for_that_pid() {
        let mut app = app();
        app.set_processes(vec![proc(safe(11), "target", 1.0)]);
        open_menu_on(&mut app, safe(11));
        let stop = Signal::ALL
            .iter()
            .position(|s| *s == Signal::Stop)
            .expect("stop listed");
        app.reduce(Action::Move(i32::try_from(stop).expect("small")));
        app.reduce(Action::ConfirmYes);
        assert!(app.signal_menu.is_none(), "picking closes the menu");
        assert!(app.confirm_kill, "pause asks first");
        assert_eq!(app.pending_signal, Signal::Stop);
        assert_eq!(app.confirm.map(|t| t.pid), Some(safe(11)));
        app.reduce(Action::ConfirmNo);
        assert!(!app.confirm_kill);
    }

    #[test]
    fn signal_menu_cont_sends_without_a_confirm() {
        let mut app = app();
        app.set_processes(vec![proc(safe(11), "target", 1.0)]);
        open_menu_on(&mut app, safe(11));
        let cont = Signal::ALL
            .iter()
            .position(|s| *s == Signal::Cont)
            .expect("cont listed");
        app.reduce(Action::Move(i32::try_from(cont).expect("small")));
        app.reduce(Action::ConfirmYes);
        assert!(!app.confirm_kill, "resume never asks");
        // The fake pid does not exist: the typed error is reported.
        let toast = app
            .status
            .as_ref()
            .map(|(m, _)| m.clone())
            .unwrap_or_default();
        assert!(toast.contains(&safe(11).to_string()), "{toast}");
    }

    #[test]
    fn signal_menu_cursor_clamps_at_both_ends() {
        let mut app = app();
        app.set_processes(vec![proc(safe(11), "target", 1.0)]);
        open_menu_on(&mut app, safe(11));
        app.reduce(Action::Move(-5));
        assert_eq!(app.signal_menu.as_ref().map(|m| m.selected), Some(0));
        app.reduce(Action::Move(500));
        assert_eq!(
            app.signal_menu.as_ref().map(|m| m.selected),
            Some(Signal::ALL.len() - 1)
        );
    }

    #[test]
    fn signal_menu_esc_and_x_close_it() {
        for close in [Action::ConfirmNo, Action::Cancel, Action::Close] {
            let mut app = app();
            app.set_processes(vec![proc(safe(11), "target", 1.0)]);
            open_menu_on(&mut app, safe(11));
            app.reduce(close);
            assert!(app.signal_menu.is_none(), "{close:?} closes the picker");
            assert!(!app.confirm_kill);
        }
    }

    #[test]
    fn signal_menu_warns_on_critical_processes() {
        let mut app = app();
        app.set_processes(vec![proc(safe(12), "WindowServer", 1.0)]);
        open_menu_on(&mut app, safe(12));
        let note = app.signal_menu.as_ref().and_then(|m| m.note.clone());
        assert!(note.is_some_and(|n| n.contains("critical")));
    }

    #[test]
    fn signal_menu_with_nothing_selected_toasts() {
        let mut app = app();
        app.set_processes(Vec::new());
        app.reduce(Action::SignalMenu);
        assert!(app.signal_menu.is_none());
        assert!(app.status.is_some());
    }

    #[test]
    fn confirm_outranks_the_signal_menu_in_mode_precedence() {
        let mut app = app();
        app.set_processes(vec![proc(safe(11), "target", 1.0)]);
        open_menu_on(&mut app, safe(11));
        app.confirm_kill = true;
        assert_eq!(app.mode(), Mode::Confirm);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn rv_eperm_never_arms_a_confirm() {
        // SAFETY: geteuid(2) has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let init = siltop_metrics::live_target(1).expect("launchd");
        let mut root = proc(1, "launchd", 0.1);
        root.start_unix = init.start_unix;
        root.user = String::from("root");
        let mut app = app();
        app.set_processes(vec![root]);
        app.detail_pid = Some(1);
        app.reduce(Action::ForceKill);
        assert!(!app.confirm_kill, "EPERM must not arm");
        let toast = app
            .status
            .as_ref()
            .map(|(m, _)| m.clone())
            .unwrap_or_default();
        assert!(toast.contains("owned by root: needs admin"), "{toast}");
        app.reduce(Action::SignalMenu);
        let menu = app.signal_menu.as_ref().expect("menu still opens");
        assert!(menu.rows.iter().all(|r| !r.enabled), "every verb dimmed");
        app.reduce(Action::ConfirmYes);
        assert!(!app.confirm_kill, "a dimmed verb never arms");
    }

    #[test]
    fn rv_kernel_task_pid_zero_never_arms() {
        let mut app = app();
        app.set_processes(vec![proc(0, "kernel_task", 3.0)]);
        app.detail_pid = Some(0);
        app.reduce(Action::Kill);
        assert!(!app.confirm_kill, "pid 0 is never armed");
        app.reduce(Action::SignalMenu);
        let menu = app.signal_menu.as_ref().expect("menu");
        assert!(menu.rows.iter().all(|r| !r.enabled));
        assert!(menu.note.as_deref().is_some_and(|n| n.contains("kernel")));
    }

    #[test]
    fn clicking_a_signal_row_picks_it_and_outside_closes() {
        let mut app = app();
        app.set_processes(vec![proc(safe(11), "target", 1.0)]);
        open_menu_on(&mut app, safe(11));
        let menu = app.view().signal_menu.expect("menu");
        let rect = siltop_ui::signal_menu_rect(app.last_area, &menu);
        let kill = Signal::ALL.len() - 1;
        let y = rect.y + 1 + u16::try_from(kill).expect("small");
        app.click(rect.x + 3, y);
        assert!(app.signal_menu.is_none());
        assert!(app.confirm_kill, "KILL arms its confirm");
        assert_eq!(app.pending_signal, Signal::Kill);
        app.reduce(Action::ConfirmNo);
        open_menu_on(&mut app, safe(11));
        app.click(0, 1);
        assert!(app.signal_menu.is_none(), "a click outside closes");
        assert!(!app.confirm_kill);
    }

    #[test]
    fn copy_pid_and_command_queue_osc52() {
        let mut app = app();
        let mut p = proc(safe(11), "target", 1.0);
        p.args = vec![String::from("/bin/target"), String::from("--flag")];
        app.set_processes(vec![p]);
        app.detail_pid = Some(safe(11));
        app.reduce(Action::CopyPid);
        let pid_seq = crate::osc::sequence(&safe(11).to_string());
        assert_eq!(app.clipboard, pid_seq);
        app.reduce(Action::CopyCommand);
        assert_eq!(app.clipboard, crate::osc::sequence("/bin/target --flag"));
    }

    #[test]
    fn copy_outside_the_dossier_copies_the_selected_row() {
        let mut app = app();
        app.set_processes(vec![proc(safe(11), "target", 1.0)]);
        app.reduce(Action::CopyPid);
        let pid_seq = crate::osc::sequence(&safe(11).to_string());
        assert_eq!(app.clipboard, pid_seq, "the row menu's copy");
        let mut app = self::app();
        app.set_processes(Vec::new());
        app.reduce(Action::CopyPid);
        assert_eq!(app.clipboard, None);
        assert!(app.status.is_some(), "says nothing is selected");
    }

    #[test]
    fn renice_lowering_priority_warns_it_is_one_way_without_admin() {
        assert_eq!(
            renice_note(0, 1, "cargo (4242)", false),
            "nice 0 → 1 for cargo (4242); undo needs admin"
        );
        assert_eq!(
            renice_note(0, 1, "cargo (4242)", true),
            "nice 0 → 1 for cargo (4242)"
        );
        assert_eq!(
            renice_note(1, 0, "cargo (4242)", false),
            "nice 1 → 0 for cargo (4242)"
        );
    }

    #[test]
    fn renice_a_vanished_process_reports_the_typed_error() {
        let mut app = app();
        app.set_processes(vec![proc(safe(11), "ghost", 1.0)]);
        app.detail_pid = Some(safe(11));
        app.reduce(Action::Renice(1));
        let toast = app
            .status
            .as_ref()
            .map(|(m, _)| m.clone())
            .unwrap_or_default();
        assert!(
            toast.starts_with(&format!("process {}", safe(11))),
            "{toast}"
        );
    }

    #[test]
    fn kill_from_detail_targets_the_detailed_pid() {
        let mut app = app();
        app.set_processes(vec![
            proc(safe(11), "target", 1.0),
            proc(safe(22), "other", 9.0),
        ]);
        app.detail_pid = Some(safe(11));
        app.reduce(Action::ForceKill);
        assert!(app.confirm_kill);
        assert_eq!(
            app.confirm.map(|t| t.pid),
            Some(safe(11)),
            "kill must target the detailed pid"
        );
        app.reduce(Action::ConfirmNo);
        assert!(!app.confirm_kill);
    }

    /// Signalling is macOS-only. On Linux `send_signal` returns Unsupported,
    /// so a confirmed kill does not let go of the dossier.
    #[cfg(target_os = "macos")]
    #[test]
    fn confirm_yes_from_detail_closes_the_card() {
        let mut app = app();
        app.set_processes(vec![
            proc(safe(11), "target", 1.0),
            proc(safe(22), "other", 9.0),
        ]);
        app.detail_pid = Some(safe(11));
        app.reduce(Action::ForceKill);
        assert!(app.confirm_kill);
        app.reduce(Action::ConfirmYes);
        assert!(!app.confirm_kill);
        assert!(app.detail_pid.is_none());
    }

    #[test]
    fn open_detail_pins_selection() {
        let mut app = app();
        app.at(120, 40);
        app.set_processes(vec![
            proc(safe(11), "low", 1.0),
            proc(safe(22), "high", 9.0),
        ]);
        app.proc.anchor = Anchor::Pinned(safe(11));
        app.send(Action::Open);
        assert_eq!(app.detail_pid, Some(safe(11)));
        assert_eq!(app.proc.selected_pid(), Some(safe(11)));
        app.send(Action::Move(-1));
        assert_eq!(app.detail_pid, app.proc.selected_pid());
        assert_eq!(app.detail_pid, Some(safe(22)));
    }

    /// `cargo test --release -p siltop -- --ignored --nocapture work_frame_cost`
    #[test]
    #[ignore = "timing probe: run in release"]
    fn work_frame_cost() {
        let mut app = scripted(Vec::new());
        app.at(160, 45);
        let mut snap = snapshot();
        snap.processes = (1..=870)
            .map(|i| proc(safe(i), &format!("process-{i}"), (i % 97) as f32 * 0.3))
            .collect();
        app.update(AppMsg::Snapshot(Box::new(Ok(snap))), app.now);
        let mut terminal = Terminal::new(TestBackend::new(160, 45)).unwrap();
        let frames = 2000_u32;
        let start = Instant::now();
        for _ in 0..frames {
            terminal.draw(|frame| app.draw(frame)).unwrap();
        }
        let per = start.elapsed() / frames;
        println!("Work 160x45, 870 processes: {per:?} per frame");
    }

    fn code_msg(code: KeyCode) -> AppMsg {
        AppMsg::Input(TermEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    /// Ten frames a second apart; frame `i` runs the CPU at `i / 10`.
    fn replaying(now: Instant) -> App {
        use siltop_core::trace::{Frame, Trace, TraceHeader};
        let frames = (0..10_u32).map(|i| {
            let mut snapshot = snapshot();
            snapshot.cpu.scaled = i as f32 / 10.0;
            Frame {
                offset: Duration::from_secs(u64::from(i)),
                snapshot,
            }
        });
        let header = TraceHeader {
            interval: Duration::from_secs(1),
            started_unix_ms: 0,
            note: String::new(),
        };
        let trace = Trace::from_frames(&header, frames, now).unwrap();
        let player = crate::replay::Player::new(trace, now);
        App::with_source(Config::default(), SnapshotSource::Replay(Box::new(player)))
    }

    /// One gate for alerts and the saved history: the demo once wrote its
    /// canned load into the user's real history.
    #[test]
    fn demo_and_replay_never_alert_or_save_history() {
        let live = SnapshotSource::Scripted(Vec::new().into());
        assert!(this_mac_now(false, &live));
        assert!(!this_mac_now(true, &live), "the demo is not this Mac");
        let replay = replaying(Instant::now());
        assert!(!this_mac_now(false, &replay.source), "nor is a trace");
    }

    fn shown_cpu(app: &App) -> u32 {
        (app.snapshot.cpu.scaled * 10.0).round() as u32
    }

    #[test]
    fn replay_frames_arrive_on_the_loop_deadline_with_a_scrub_bar() {
        let t0 = Instant::now();
        let mut app = replaying(t0);
        app.update(AppMsg::Timer, t0);
        assert_eq!(shown_cpu(&app), 0);
        assert!(app.ready);
        assert!(
            app.next_deadline()
                .is_some_and(|at| at <= t0 + Duration::from_secs(1))
        );
        app.update(AppMsg::Timer, t0 + Duration::from_secs(3));
        assert_eq!(shown_cpu(&app), 3);
        let status = app.status.as_ref().map(|(m, _)| m.clone()).unwrap();
        assert!(status.starts_with("replay ▶ 1× ━"), "{status}");
    }

    #[test]
    fn replay_keys_pause_speed_and_scrub_without_touching_prefs() {
        let t0 = Instant::now();
        let mut app = replaying(t0);
        app.update(AppMsg::Timer, t0 + Duration::from_secs(5));
        assert_eq!(shown_cpu(&app), 5);
        app.update(code_msg(KeyCode::Left), t0 + Duration::from_secs(5));
        assert_eq!(shown_cpu(&app), 4, "scrubbed back one step");
        app.update(key_msg('+'), t0 + Duration::from_secs(5));
        assert!(!app.dirty_prefs, "speed is not the saved interval");
        assert_eq!(app.config.interval, Config::default().interval);
        app.update(key_msg('p'), t0 + Duration::from_secs(5));
        assert!(app.frozen);
        assert_eq!(app.source.deadline(), None, "a paused replay sleeps");
        app.update(code_msg(KeyCode::Right), t0 + Duration::from_secs(6));
        assert_eq!(
            shown_cpu(&app),
            5,
            "scrubbing a paused replay still moves the view"
        );
        let status = app.status.as_ref().map(|(m, _)| m.clone()).unwrap();
        assert!(status.starts_with("replay ❚❚ 2× "), "{status}");
    }

    fn replaying_table(t0: Instant) -> App {
        let mut app = replaying(t0);
        app.update(AppMsg::Timer, t0);
        app.at(120, 40);
        app.set_processes(procs(30));
        app
    }

    #[test]
    fn replay_never_signals_or_renices_a_recorded_pid() {
        let t0 = Instant::now();
        for action in [
            Action::Kill,
            Action::ForceKill,
            Action::Interrupt,
            Action::SignalMenu,
            Action::Renice(1),
            Action::ToggleBackground,
        ] {
            let mut app = replaying_table(t0);
            app.send(action);
            assert!(!app.confirm_kill, "{action:?} armed a kill");
            assert!(app.signal_menu.is_none(), "{action:?} opened the picker");
            let status = app.status.as_ref().map(|(m, _)| m.as_str());
            assert_eq!(status, Some(REPLAY_READ_ONLY), "{action:?}");
        }
    }

    #[test]
    fn replay_dossier_never_inspects_the_live_pid() {
        let t0 = Instant::now();
        let mut app = replaying_table(t0);
        app.send(Action::Open);
        assert!(app.detail_pid.is_some());
        app.send(Action::DossierTab(2));
        assert!(app.proc.tab.inspects());
        assert!(
            app.proc
                .inspection
                .as_ref()
                .is_none_or(|i| i.recorded && i.threads.is_empty() && i.files.is_empty()),
            "a recorded pid is not read live"
        );
        let screen = app.screen().join("\n");
        assert!(screen.contains("not in a replay"), "{screen}");
        assert!(!screen.contains("x quit"), "{screen}");
    }

    #[test]
    fn replay_bar_leaves_a_live_toast_alone_and_returns_after_it() {
        let t0 = Instant::now();
        let mut app = replaying(t0);
        app.update(AppMsg::Timer, t0);
        app.set_toast(String::from("theme dracula"), ToastKind::Info, STATUS_TTL);
        app.update(AppMsg::Timer, t0 + Duration::from_secs(2));
        assert_eq!(shown_cpu(&app), 2);
        let status = app.status.as_ref().map(|(m, _)| m.clone()).unwrap();
        assert_eq!(status, "theme dracula", "a new frame kept the toast");
        // paused: no frame will come to redraw the bar once the toast ends
        app.update(key_msg('p'), t0 + Duration::from_secs(2));
        app.set_toast(String::from("theme nord"), ToastKind::Info, STATUS_TTL);
        app.update(AppMsg::Timer, t0 + Duration::from_secs(3) + STATUS_TTL);
        let status = app.status.as_ref().map(|(m, _)| m.clone()).unwrap();
        assert!(status.starts_with("replay ❚❚"), "{status}");
    }

    #[test]
    fn replay_arrows_keep_their_overlay_meaning() {
        let t0 = Instant::now();
        let mut app = replaying(t0);
        app.update(AppMsg::Timer, t0 + Duration::from_secs(5));
        app.update(key_msg('?'), t0 + Duration::from_secs(5));
        assert!(app.help);
        app.update(code_msg(KeyCode::Left), t0 + Duration::from_secs(5));
        assert_eq!(shown_cpu(&app), 5, "help open: no scrub");
    }

    #[test]
    fn m_opens_the_menu_and_arrows_enter_run_an_item() {
        let mut app = work_app();
        press(&mut app, 'm');
        assert_eq!(app.menu.as_ref().map(|m| m.selected), Some(1));
        assert_eq!(app.mode(), Mode::Menu);
        for _ in 0..3 {
            press_code(&mut app, KeyCode::Down);
        }
        press_code(&mut app, KeyCode::Enter);
        assert!(app.help, "fourth item is keys & help");
        assert!(app.menu.is_none());
    }

    #[test]
    fn menu_letters_jump_to_the_right_settings_page() {
        for (key, want) in [
            ('s', SettingsCategory::General),
            ('t', SettingsCategory::Appearance),
            ('a', SettingsCategory::About),
        ] {
            let mut app = work_app();
            press(&mut app, 'm');
            press(&mut app, key);
            assert!(app.menu.is_none(), "{key}");
            assert_eq!(
                app.settings.as_ref().map(|s| s.category),
                Some(want),
                "{key}"
            );
        }
    }

    #[test]
    fn menu_x_does_not_arm_a_kill_behind_it() {
        let mut app = work_app();
        press(&mut app, 'm');
        press(&mut app, 'x');
        assert!(!app.confirm_kill);
        assert_eq!(app.menu.as_ref().map(|m| m.selected), Some(1));
    }

    #[test]
    fn quick_kill_of_loginwindow_warns_it_is_critical() {
        let mut app = work_app();
        let pid = app.proc.selected_pid().unwrap();
        let mut list = procs(30);
        list.iter_mut().find(|p| p.pid == pid).unwrap().name = String::from("loginwindow");
        app.set_processes(list);
        press(&mut app, 'x');
        assert!(app.confirm_kill);
        assert_eq!(app.armed_critical(), 1);
        let footer = app.screen()[39].clone();
        assert!(footer.contains("Quit loginwindow"), "{footer}");
        assert!(footer.contains("critical"), "{footer}");
        press(&mut app, 'n');
        assert_eq!(app.armed_critical(), 0, "nothing armed, nothing to warn");
    }

    #[test]
    fn help_swallows_a_footer_kill_click() {
        let mut app = work_app();
        let footer = app.screen()[39].clone();
        let kill = column_of(&footer, "x kill");
        press(&mut app, '?');
        assert!(app.help);
        app.click(kill, 39);
        assert!(!app.confirm_kill, "a kill armed under the help box");
        assert_ne!(app.mode(), Mode::Confirm);
    }

    #[test]
    fn menu_swallows_a_footer_kill_click() {
        let mut app = work_app();
        let footer = app.screen()[39].clone();
        let kill = column_of(&footer, "x kill");
        press(&mut app, 'm');
        app.click(kill, 39);
        assert!(!app.confirm_kill, "a kill armed under the menu");
        assert!(app.menu.is_none(), "a click outside the menu closes it");
    }

    #[test]
    fn esc_closes_the_signal_picker_before_the_filter() {
        let mut app = work_app();
        app.proc.filter.push('p');
        app.send(Action::SignalMenu);
        assert!(app.signal_menu.is_some());
        press_code(&mut app, KeyCode::Esc);
        assert!(app.signal_menu.is_none());
        assert!(!app.proc.filter.is_empty(), "esc peeled the filter too");
    }

    #[test]
    fn biggest_first_off_sorts_smallest_first_and_r_is_saved() {
        let mut app = work_app();
        let mut next = app.live_config();
        next.sort_desc = false;
        app.apply_config(next);
        assert!(app.sort_reversed(), "cpu, smallest first");
        assert!(app.view().sort_reversed);
        app.dirty_prefs = false;
        press(&mut app, 'r');
        assert!(app.config.sort_desc, "r flips the saved direction");
        assert!(app.dirty_prefs, "and saves it");
        press(&mut app, 'r');
        app.apply_preset(siltop_core::Preset::Ml);
        assert_eq!(app.config.proc_sort, ProcSort::Mem);
        assert!(
            !app.sort_reversed(),
            "a preset's column starts its natural way"
        );
        let mut next = app.live_config();
        next.proc_sort = ProcSort::Name;
        app.apply_config(next);
        assert!(!app.config.sort_desc, "names start A to Z");
        assert!(!app.sort_reversed());
    }

    #[test]
    fn esc_on_the_plain_home_screen_opens_the_menu_and_esc_closes_it() {
        let mut app = work_app();
        press_code(&mut app, KeyCode::Esc);
        assert_eq!(app.menu.as_ref().map(|m| m.selected), Some(1));
        press_code(&mut app, KeyCode::Esc);
        assert!(app.menu.is_none());
        app.expanded = Some(Panel::Cpu);
        press_code(&mut app, KeyCode::Esc);
        assert_eq!(app.expanded, None, "esc peels the expanded box first");
        assert!(app.menu.is_none());
    }

    #[test]
    fn clicking_the_wordmark_opens_the_menu_and_items_run_on_click() {
        let mut app = work_app();
        let footer = app.screen()[39].clone();
        app.click(column_of(&footer, "siltop") + 2, 39);
        assert_eq!(app.menu.as_ref().map(|m| m.selected), Some(1), "{footer}");
        let screen = app.screen();
        let (row, line) = screen
            .iter()
            .enumerate()
            .find(|(_, l)| l.contains("Themes"))
            .expect("menu painted");
        app.click(column_of(line, "Themes") + 1, u16::try_from(row).unwrap());
        assert!(app.menu.is_none());
        assert_eq!(
            app.settings.as_ref().map(|s| s.category),
            Some(SettingsCategory::Appearance)
        );
    }

    #[test]
    fn clicking_outside_the_menu_closes_it_and_does_nothing_else() {
        let mut app = work_app();
        press(&mut app, 'm');
        let focus = app.focus;
        app.click(2, 2);
        assert!(app.menu.is_none());
        assert_eq!(app.focus, focus, "the click was swallowed by the menu");
    }

    #[test]
    fn busy_verdict_shows_in_the_footer_and_click_selects_its_process() {
        let mut app = work_app();
        app.verdict = VerdictEngine::new(Duration::ZERO);
        let hog = safe(99);
        let mut snap = snapshot();
        snap.primed = true;
        snap.processes = procs(30);
        snap.processes.push(proc(hog, "Hog", 310.0));
        snap.sampled.procs = true;
        app.apply_snapshot(snap);
        assert_eq!(app.verdict.shown().and_then(|v| v.pid), Some(hog));
        for _ in 0..3 {
            press(&mut app, 'j');
        }
        assert_ne!(app.proc.selected_pid(), Some(hog));
        let footer = app.screen()[39].clone();
        assert!(footer.contains("Busy: Hog using 3.1 cores"), "{footer}");
        app.click(column_of(&footer, "Busy:") + 1, 39);
        assert_eq!(app.proc.selected_pid(), Some(hog));
        assert_eq!(app.focus, Focus::Processes);
    }

    #[test]
    fn first_run_hint_sits_in_the_verdict_slot_until_a_key() {
        use crate::first_run::{FirstRun, HINT};
        let mut app = work_app();
        let missing = std::path::Path::new("/nonexistent/siltop/config.toml");
        app.first_run = FirstRun::detect(Some(missing), false, app.now);
        assert!(
            app.next_deadline().is_some(),
            "the loop wakes to take it down"
        );
        let footer = app.screen()[39].clone();
        assert!(footer.contains(HINT), "{footer}");
        // A click is not a key: the hint stays for the reader.
        app.click(5, 5);
        assert!(app.screen()[39].contains(HINT));
        press(&mut app, 'j');
        let footer = app.screen()[39].clone();
        assert!(!footer.contains(HINT), "{footer}");
        assert!(app.first_run.detected(), "still sealed on exit");
    }

    /// Property: at every width the hint is whole or absent, never cut, and
    /// it shows from 110 columns up.
    #[test]
    fn first_run_hint_is_whole_or_absent_at_every_width() {
        use crate::first_run::{FirstRun, HINT};
        let missing = std::path::Path::new("/nonexistent/siltop/config.toml");
        let prefix: String = HINT.chars().take(8).collect();
        for width in 60..=200 {
            let mut app = work_app();
            app.at(width, 40);
            app.first_run = FirstRun::detect(Some(missing), false, app.now);
            let footer = app.screen()[39].clone();
            let whole = footer.contains(HINT);
            assert!(whole || !footer.contains(&prefix), "{width}: {footer}");
            if width >= 110 {
                assert!(whole, "{width}: {footer}");
            }
        }
    }

    /// A laptop on battery has the footer's longest status; the hint must
    /// still show at a laptop's 120 columns.
    #[test]
    fn first_run_hint_shows_at_120_columns_on_a_laptop_battery() {
        use crate::first_run::{FirstRun, HINT};
        let mut app = work_app();
        let mut snap = app.snapshot.clone();
        snap.battery.present = true;
        snap.battery.percent = Some(79);
        snap.battery.watts = Some(-7.1);
        snap.battery.source = siltop_core::PowerSource::Battery;
        snap.battery.time_to_empty = Some(std::time::Duration::from_mins(485));
        app.apply_snapshot(snap);
        let missing = std::path::Path::new("/nonexistent/siltop/config.toml");
        app.first_run = FirstRun::detect(Some(missing), false, app.now);
        let footer = app.screen()[39].clone();
        assert!(footer.contains(HINT), "{footer}");
    }

    #[test]
    fn later_launches_show_no_hint_and_add_no_wakeups() {
        let mut app = work_app();
        let footer = app.screen()[39].clone();
        assert!(!footer.contains(crate::first_run::HINT));
        assert!(app.first_run.deadline().is_none());
    }

    #[test]
    fn corner_marks_are_bracketed_and_click_where_drawn() {
        let mut app = work_app();
        let screen = app.screen();
        let top = &screen[0];
        assert!(top.contains("[↗]"), "{top}");
        let col = column_of(top, "[↗]") + 1;
        app.click(col, 0);
        assert!(app.expanded.is_some(), "the first [↗] expands its box");
        let top = app.screen()[0].clone();
        assert!(top.contains("[×]"), "{top}");
        app.click(column_of(&top, "[×]") + 1, 0);
        assert_eq!(app.expanded, None);
    }

    fn press(app: &mut App, c: char) {
        let now = app.now;
        app.update(key_msg(c), now);
    }

    /// A sampler that only records the needs the worker hands it.
    struct NeedsLog(std::sync::Arc<std::sync::Mutex<Vec<Needs>>>);

    impl worker::Tick for NeedsLog {
        fn tick(&mut self) -> siltop_core::Result<Snapshot> {
            Ok(Snapshot::empty())
        }

        fn set_needs(&self, needs: Needs) {
            self.0.lock().unwrap().push(needs);
        }
    }

    /// The app, the needs log and the snapshot receiver. The receiver must
    /// outlive the app: the worker stops at its first tick once nobody
    /// listens, and a stopped worker applies no hints (a race under load).
    fn live_needs_app() -> (
        App,
        std::sync::Arc<std::sync::Mutex<Vec<Needs>>>,
        std::sync::mpsc::Receiver<AppMsg>,
    ) {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = std::sync::Arc::clone(&log);
        let (tx, rx) = channel();
        let handle =
            worker::spawn_with(Duration::from_secs(60), tx, move || Ok(NeedsLog(seen))).unwrap();
        (
            App::with_source(Config::default(), SnapshotSource::Live(handle)),
            log,
            rx,
        )
    }

    /// A sampler that records each low-power flip the worker hands it.
    struct LowLog(std::sync::Arc<std::sync::Mutex<Vec<bool>>>);

    impl worker::Tick for LowLog {
        fn tick(&mut self) -> siltop_core::Result<Snapshot> {
            Ok(Snapshot::empty())
        }

        fn set_low_power(&self, low: bool) {
            self.0.lock().unwrap().push(low);
        }
    }

    #[test]
    fn rv_battery_saver_setting_reaches_the_worker() {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = std::sync::Arc::clone(&log);
        let (tx, _snapshots) = channel();
        let handle = worker::spawn_power(
            Duration::from_secs(60),
            siltop_core::LowPower::Auto,
            tx,
            move || Ok(LowLog(seen)),
        )
        .unwrap();
        let mut app = App::with_source(Config::default(), SnapshotSource::Live(handle));
        for mode in [siltop_core::LowPower::On, siltop_core::LowPower::Off] {
            let mut next = app.live_config();
            next.low_power = mode;
            app.apply_config(next);
        }
        // Dropping the app joins the worker: every command has landed.
        drop(app);
        assert_eq!(*log.lock().unwrap(), vec![true, false]);
    }

    #[test]
    fn rv_low_power_shows_the_cadence_the_sampler_really_runs() {
        let mut app = app();
        let now = app.now;
        app.update(AppMsg::LowPower(true), now);
        let view = app.view();
        assert_eq!(view.interval_ms, 2000, "the footer shows the real tick");
        assert!(view.battery_saver);
        app.reduce(Action::IntervalFaster);
        assert_eq!(app.config.interval, Duration::from_millis(250), "kept");
        let (toast, _) = app.status.clone().expect("toast");
        assert_eq!(toast, "sampling every 2 s · battery saver");
        app.update(AppMsg::LowPower(false), now);
        let view = app.view();
        assert_eq!(view.interval_ms, 250);
        assert!(!view.battery_saver);
    }

    #[test]
    fn rv_a_stopped_sampler_stays_marked_after_its_toast() {
        let mut app = app();
        let now = app.now;
        app.update(AppMsg::Stopped(Error::system("sampler stopped: boom")), now);
        app.update(AppMsg::Timer, now + STATUS_TTL + Duration::from_secs(1));
        assert!(app.status.is_none(), "the toast has expired");
        assert!(app.view().stopped, "the footer still says stopped");
    }

    #[test]
    fn the_dashboard_needs_every_costly_source() {
        let app = app();
        assert_eq!(app.wanted_needs(), Needs::ALL);
        assert_eq!(app.needs, Needs::ALL, "nothing sent at start");
    }

    #[test]
    fn settings_parks_ioreport_and_closing_resumes_it() {
        let (mut app, log, _snapshots) = live_needs_app();
        press(&mut app, 's');
        assert!(app.settings.is_some());
        assert_eq!(app.needs, PAGE_NEEDS);
        press(&mut app, 'j');
        press(&mut app, 'q');
        assert!(app.settings.is_none());
        assert_eq!(app.needs, Needs::ALL);
        drop(app); // joins the worker: every command has been applied
        assert_eq!(
            *log.lock().unwrap(),
            vec![PAGE_NEEDS, Needs::ALL],
            "one hint per change, none repeated"
        );
    }

    #[test]
    fn the_timeline_page_parks_ioreport() {
        let mut app = app();
        app.toggle_timeline();
        assert!(app.timeline.is_some());
        assert_eq!(app.wanted_needs(), PAGE_NEEDS);
        app.toggle_timeline();
        assert_eq!(app.wanted_needs(), Needs::ALL);
    }

    #[test]
    fn help_over_the_dashboard_keeps_every_source() {
        let mut app = app();
        press(&mut app, '?');
        assert!(app.help);
        assert_eq!(app.needs, Needs::ALL, "the dashboard is still visible");
    }

    #[test]
    fn a_scripted_source_ignores_needs() {
        let mut app = app();
        press(&mut app, 's');
        assert_eq!(app.needs, PAGE_NEEDS, "tracked even with no worker");
    }

    #[test]
    fn pages_park_cpu_clocks_but_keep_what_history_records() {
        assert!(!PAGE_NEEDS.contains(Needs::CPU_CLOCKS));
        assert!(
            PAGE_NEEDS.contains(Needs::GPU_CLOCKS),
            "no flat GPU history"
        );
    }

    /// Opening the timeline once parked the rails, so the history it shows
    /// recorded no CPU or GPU watts for as long as it stayed open.
    #[test]
    fn history_power_has_no_gaps_while_a_page_is_open() {
        assert!(PAGE_NEEDS.contains(Needs::POWER));
        let mut app = app();
        app.toggle_timeline();
        assert!(app.wanted_needs().contains(Needs::POWER));
    }

    fn press_code(app: &mut App, code: KeyCode) {
        let now = app.now;
        app.update(code_msg(code), now);
    }

    fn work_app() -> App {
        let mut app = app();
        app.at(120, 40);
        app.set_processes(procs(30));
        app
    }

    /// A fresh process sample listing `processes`, delivered like the worker.
    fn deliver(app: &mut App, processes: Vec<Process>) {
        let mut snap = snapshot();
        snap.processes = processes;
        snap.sampled.procs = true;
        let now = app.now;
        app.update(AppMsg::Snapshot(Box::new(Ok(snap))), now);
    }

    fn without(list: &[Process], pid: u32) -> Vec<Process> {
        list.iter().filter(|p| p.pid != pid).cloned().collect()
    }

    #[test]
    fn rv_confirm_cancelled_on_exit() {
        let mut app = work_app();
        let listed = app.snapshot.processes.clone();
        press(&mut app, 'x');
        let pid = app.confirm.map(|t| t.pid).expect("armed");
        deliver(&mut app, without(&listed, pid));
        assert!(!app.confirm_kill && app.confirm.is_none());
        assert_eq!(app.mode(), Mode::Normal, "keys leave the prompt");
        let (toast, _) = app.status.clone().expect("toast");
        assert!(toast.contains("kill cancelled"), "{toast}");
        assert!(toast.contains(&pid.to_string()), "{toast}");
        assert_eq!(app.status_kind, ToastKind::Warn);
    }

    #[test]
    fn rv_confirm_cancelled_when_dossier_target_exits() {
        let mut app = work_app();
        let listed = app.snapshot.processes.clone();
        press_code(&mut app, KeyCode::Enter);
        let pid = app.detail_pid.expect("dossier");
        app.reduce(Action::ForceKill);
        assert_eq!(app.pending_signal, Signal::Kill);
        deliver(&mut app, without(&listed, pid));
        assert!(app.detail_pid.is_none(), "the dossier closes");
        assert!(!app.confirm_kill, "and the SIGKILL prompt with it");
        press(&mut app, 'y');
        assert!(
            !app.status
                .as_ref()
                .is_some_and(|(t, _)| t.starts_with("sent")),
            "y after the exit sends nothing"
        );
    }

    #[test]
    fn rv_confirm_cancelled_when_pid_is_reused() {
        let mut app = work_app();
        let mut listed = app.snapshot.processes.clone();
        press(&mut app, 'x');
        let pid = app.confirm.map(|t| t.pid).expect("armed");
        for p in listed.iter_mut().filter(|p| p.pid == pid) {
            p.start_unix += 60;
        }
        deliver(&mut app, listed);
        assert!(!app.confirm_kill, "same pid, new process: not our target");
    }

    #[test]
    fn rv_confirm_survives_a_sample_that_still_lists_it() {
        let mut app = work_app();
        let listed = app.snapshot.processes.clone();
        press(&mut app, 'x');
        let armed = app.confirm;
        deliver(&mut app, listed);
        assert!(app.confirm_kill);
        assert_eq!(app.confirm, armed);
    }

    #[test]
    fn rv_confirm_ignores_samples_without_a_process_sweep() {
        let mut app = work_app();
        press(&mut app, 'x');
        let mut snap = snapshot();
        snap.processes.clear();
        snap.sampled.procs = false;
        let now = app.now;
        app.update(AppMsg::Snapshot(Box::new(Ok(snap))), now);
        assert!(app.confirm_kill, "no sweep, no verdict");
    }

    #[test]
    fn rv_kill_target_records_the_listed_start_time() {
        let mut app = work_app();
        let mut listed = procs(30);
        for (i, p) in listed.iter_mut().enumerate() {
            p.start_unix = 1_700_000_000 + i64::try_from(i).unwrap();
        }
        app.set_processes(listed.clone());
        press(&mut app, 'x');
        let target = app.confirm.expect("armed");
        let shown = listed.iter().find(|p| p.pid == target.pid).unwrap();
        assert_eq!(target, KillTarget::of(shown));
    }

    #[test]
    fn rv_frozen_kill_goes_through_the_identity_check() {
        let mut app = work_app();
        app.reduce(Action::Pause);
        assert!(app.frozen);
        let listed = app.snapshot.processes.clone();
        press(&mut app, 'x');
        let pid = app.confirm.map(|t| t.pid).expect("armed");
        // frozen: the sample that would cancel is dropped
        deliver(&mut app, without(&listed, pid));
        assert!(app.confirm_kill);
        // fixture pids (900_000+) do not exist: the check refuses to signal
        press(&mut app, 'y');
        assert!(!app.confirm_kill);
        assert_eq!(app.status_kind, ToastKind::Error);
    }

    #[test]
    fn rv_click_clears_a_stuck_drag() {
        let mut app = work_app();
        app.dragging_split = true;
        press(&mut app, '?');
        app.click(1, 1);
        assert!(!app.dragging_split, "any press drops a lost drag");
        let ratio = app.proc_ratio;
        app.help = false;
        app.send(Action::Drag { col: 30, row: 5 });
        assert_eq!(app.proc_ratio, ratio, "a later drag does not resize");
    }

    #[test]
    fn rv_drag_under_an_overlay_never_resizes() {
        let mut app = work_app();
        let ratio = app.proc_ratio;
        for open in [
            |a: &mut App| a.help = true,
            |a: &mut App| a.settings = Some(SettingsState::default()),
            |a: &mut App| a.confirm_kill = true,
        ] {
            open(&mut app);
            app.dragging_split = true;
            app.send(Action::Drag { col: 30, row: 5 });
            assert_eq!(app.proc_ratio, ratio);
            app.send(Action::MouseUp);
            assert!(!app.dragging_split);
            app.help = false;
            app.settings = None;
            app.confirm_kill = false;
        }
    }

    #[test]
    fn rv_esc_in_expanded_search_leaves_search_before_the_expand() {
        let mut app = work_app();
        app.expand_panel(Panel::Processes);
        press(&mut app, '/');
        for c in "pr".chars() {
            press(&mut app, c);
        }
        press_code(&mut app, KeyCode::Esc);
        assert!(app.searching && app.expanded.is_some(), "clears the text");
        press_code(&mut app, KeyCode::Esc);
        assert!(!app.searching, "then leaves search");
        assert_eq!(app.expanded, Some(Panel::Processes), "expand stays");
        press_code(&mut app, KeyCode::Esc);
        assert!(app.expanded.is_none(), "and only then collapses");
    }

    #[test]
    fn rv_ctrl_and_alt_letters_never_act_or_type() {
        use ratatui::crossterm::event::{KeyEvent, KeyModifiers};
        for mods in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            for searching in [false, true] {
                let mut app = work_app();
                if searching {
                    press(&mut app, '/');
                }
                let before = (app.config.clone(), app.proc_ratio, app.surface);
                for c in ('a'..='z').filter(|c| *c != 'c') {
                    let key = KeyEvent::new(KeyCode::Char(c), mods);
                    let now = app.now;
                    app.update(AppMsg::Input(TermEvent::Key(key)), now);
                }
                assert!(app.proc.filter.is_empty(), "{mods:?}: typed into search");
                assert!(!app.confirm_kill && !app.frozen && !app.quit, "{mods:?}");
                assert!(!app.help && app.settings.is_none(), "{mods:?}");
                assert_eq!(before, (app.config.clone(), app.proc_ratio, app.surface));
            }
        }
    }

    #[test]
    fn q_closes_every_overlay_and_never_quits_from_one() {
        let mut app = work_app();
        press(&mut app, '?');
        assert!(app.help);
        press(&mut app, 'q');
        assert!(!app.help && !app.quit);
        press(&mut app, 's');
        assert!(app.settings.is_some());
        press(&mut app, 'q');
        assert!(app.settings.is_none() && !app.quit);
        press_code(&mut app, KeyCode::Enter);
        assert!(app.detail_pid.is_some());
        press(&mut app, 'x');
        assert!(app.confirm_kill);
        press(&mut app, 'q');
        assert!(!app.confirm_kill && !app.quit, "q cancels the confirm");
        assert!(app.detail_pid.is_some());
        press(&mut app, 'q');
        assert!(
            app.detail_pid.is_none() && !app.quit,
            "q closes the dossier"
        );
        press(&mut app, 'q');
        assert!(app.quit, "q quits only from the plain screen");
    }

    #[test]
    fn question_mark_toggles_help_and_esc_closes_it() {
        let mut app = work_app();
        press(&mut app, '?');
        assert!(app.help);
        press(&mut app, '?');
        assert!(!app.help);
        press(&mut app, '?');
        press_code(&mut app, KeyCode::Esc);
        assert!(!app.help);
    }

    #[test]
    fn esc_in_search_clears_the_text_then_leaves() {
        let mut app = work_app();
        press(&mut app, '/');
        for c in "proc1".chars() {
            press(&mut app, c);
        }
        assert_eq!(app.proc.filter, "proc1");
        press_code(&mut app, KeyCode::Esc);
        assert!(app.searching, "first esc only clears");
        assert!(app.proc.filter.is_empty());
        press_code(&mut app, KeyCode::Esc);
        assert!(!app.searching, "second esc leaves");
        assert_eq!(app.focus, Focus::Processes);
    }

    #[test]
    fn letters_typed_into_search_never_run_commands() {
        let mut app = work_app();
        press(&mut app, 'f');
        assert!(app.searching);
        let before = app.config.clone();
        for c in "qxKpTtveso[]-+XgG".chars() {
            press(&mut app, c);
        }
        assert_eq!(app.proc.filter, "qxKpTtveso[]-+XgG");
        assert!(!app.quit && !app.confirm_kill && !app.frozen && app.settings.is_none());
        assert_eq!(app.config.interval, before.interval);
        assert_eq!(app.config.show_tree, before.show_tree);
        assert!(app.expanded.is_none());
        assert!(app.status.is_none(), "no pending-action toast leaked");
    }

    #[test]
    fn search_arrows_move_the_selection_and_keep_typing_focus() {
        let mut app = work_app();
        press(&mut app, '/');
        press(&mut app, 'p');
        let first = app.proc.selected_pid();
        press_code(&mut app, KeyCode::Down);
        assert!(app.searching, "the input keeps focus");
        assert_eq!(app.focus, Focus::Search);
        assert_ne!(app.proc.selected_pid(), first);
        press(&mut app, 'r');
        assert_eq!(app.proc.filter, "pr", "typing continues after moving");
    }

    #[test]
    fn enter_in_search_keeps_the_filter_and_a_second_enter_opens() {
        let mut app = work_app();
        press(&mut app, '/');
        press(&mut app, 'p');
        press_code(&mut app, KeyCode::Down);
        let chosen = app.proc.selected_pid();
        press_code(&mut app, KeyCode::Enter);
        assert!(!app.searching, "back on the table");
        assert_eq!(app.detail_pid, None);
        assert_eq!(app.proc.filter, "p", "the filter is kept");
        press_code(&mut app, KeyCode::Enter);
        assert_eq!(app.detail_pid, chosen);
        assert_eq!(app.proc.filter, "p", "the filter survives opening");
    }

    #[test]
    fn delete_clears_the_filter_from_anywhere() {
        let mut app = work_app();
        press(&mut app, '/');
        press(&mut app, 'z');
        press_code(&mut app, KeyCode::Delete);
        assert!(app.proc.filter.is_empty());
        assert!(app.searching);
        press(&mut app, 'z');
        press_code(&mut app, KeyCode::Esc);
        press_code(&mut app, KeyCode::Esc);
        app.proc.filter.push('q');
        press_code(&mut app, KeyCode::Delete);
        assert!(app.proc.filter.is_empty(), "normal-mode delete clears too");
    }

    #[test]
    fn home_end_and_pages_move_through_the_list() {
        let mut app = work_app();
        press(&mut app, 'G');
        let last = app.proc.model.rows().last().map(|r| r.pid);
        assert_eq!(app.proc.selected_pid(), last);
        press(&mut app, 'g');
        let first = app.proc.model.rows().first().map(|r| r.pid);
        assert_eq!(app.proc.selected_pid(), first);
        press_code(&mut app, KeyCode::End);
        assert_eq!(app.proc.selected_pid(), last);
        press_code(&mut app, KeyCode::Home);
        assert_eq!(app.proc.selected_pid(), first);
        press_code(&mut app, KeyCode::PageDown);
        let paged = app
            .proc
            .model
            .position(app.proc.selected_pid().unwrap())
            .unwrap();
        assert!(paged > 1, "a page is more than one row: {paged}");
        press_code(&mut app, KeyCode::PageUp);
        assert_eq!(app.proc.selected_pid(), first);
    }

    #[test]
    fn v_cycles_the_surface_both_ways() {
        let mut app = work_app();
        assert_eq!(app.effective_surface(), Surface::Work);
        app.expanded = Some(Panel::Cpu);
        app.expanded = None;
        press(&mut app, 'v');
        assert_eq!(app.effective_surface(), Surface::Glance);
        press(&mut app, 'v');
        assert_eq!(app.effective_surface(), Surface::Work);
        press(&mut app, 'w');
        assert_eq!(app.effective_surface(), Surface::Work, "w is unbound");
    }

    #[test]
    fn p_pauses_and_stamps_the_wall_clock() {
        let mut app = work_app();
        press(&mut app, 'p');
        assert!(app.frozen);
        let at = app.paused_at.clone().expect("paused time");
        assert_eq!(at.len(), 8, "{at}");
        assert_eq!(at.as_bytes()[2], b':');
        assert_eq!(at.as_bytes()[5], b':');
        let footer = app.screen()[39].clone();
        assert!(footer.contains(&format!("❚❚ paused {at}")), "{footer}");
        press(&mut app, 'p');
        assert!(!app.frozen);
        assert!(app.paused_at.is_none());
        let footer = app.screen()[39].clone();
        assert!(footer.contains("● 500 ms"), "{footer}");
    }

    #[test]
    fn interval_change_flashes_a_short_toast() {
        let mut app = work_app();
        let now = app.now;
        press(&mut app, ']');
        assert_eq!(app.config.interval, 250 * MS);
        assert_eq!(app.status_ttl, INTERVAL_TOAST_TTL);
        assert_eq!(app.status_kind, ToastKind::Info);
        assert!(
            app.status.as_ref().unwrap().0.contains("250 ms"),
            "{:?}",
            app.status
        );
        assert_eq!(app.next_deadline(), Some(now + INTERVAL_TOAST_TTL));
        press(&mut app, '-');
        press(&mut app, '+');
        press(&mut app, '=');
        assert_eq!(app.config.interval, 250 * MS);
        app.update(AppMsg::Timer, now + INTERVAL_TOAST_TTL);
        assert!(app.status.is_none());
    }

    #[test]
    fn capital_t_cycles_every_theme_and_wraps() {
        let mut app = work_app();
        let start = app.config.theme.clone();
        let mut seen = vec![start.clone()];
        for _ in 1..Theme::catalog().len() {
            press(&mut app, 'T');
            assert!(!seen.contains(&app.config.theme), "{:?}", app.config.theme);
            seen.push(app.config.theme.clone());
        }
        press(&mut app, 'T');
        assert_eq!(app.config.theme, start);
        assert!(app.dirty_prefs, "the choice is saved");
    }

    #[test]
    fn capital_t_comes_back_round_to_auto() {
        let mut app = work_app();
        let last = *Theme::catalog().last().unwrap();
        last.clone_into(&mut app.config.theme);
        press(&mut app, 'T');
        assert_eq!(
            app.config.theme,
            Theme::AUTO,
            "the last theme wraps to auto"
        );
        press(&mut app, 'T');
        assert_eq!(app.config.theme, Theme::builtins()[0]);
        assert_eq!(app.theme.name, Theme::builtins()[0]);
    }

    fn settings_on(app: &App) -> &SettingsState {
        app.settings.as_ref().expect("settings open")
    }

    fn row_of(screen: &[String], needle: &str) -> (u16, u16) {
        let (y, line) = screen
            .iter()
            .enumerate()
            .find(|(_, l)| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle} not on screen:\n{}", screen.join("\n")));
        (column_of(line, needle), u16::try_from(y).unwrap())
    }

    #[test]
    fn settings_keys_walk_sections_and_change_values() {
        let mut app = work_app();
        press(&mut app, 's');
        assert_eq!(settings_on(&app).category, SettingsCategory::General);
        press_code(&mut app, KeyCode::Tab);
        press_code(&mut app, KeyCode::Tab);
        assert_eq!(settings_on(&app).category, SettingsCategory::Panels);
        let before = app.config.show_disk;
        press(&mut app, 'j');
        press(&mut app, 'j');
        assert_eq!(settings_on(&app).selected().unwrap().id, "panels.disk");
        press_code(&mut app, KeyCode::Enter);
        assert_eq!(app.config.show_disk, !before);
        press(&mut app, 'h');
        assert_eq!(app.config.show_disk, before, "h flips a toggle back");
        assert!(app.dirty_prefs);
        press_code(&mut app, KeyCode::BackTab);
        assert_eq!(settings_on(&app).category, SettingsCategory::Appearance);
    }

    #[test]
    fn settings_interval_row_resamples_like_the_bracket_keys() {
        let mut app = work_app();
        press(&mut app, 's');
        let before = app.config.interval;
        press_code(&mut app, KeyCode::Right);
        assert_eq!(
            app.config.interval,
            app.config
                .clone()
                .with_interval(before)
                .step_interval(false)
        );
        assert!(
            app.status
                .as_ref()
                .is_some_and(|(m, _)| m.starts_with("sampling every"))
        );
    }

    #[test]
    fn settings_filter_finds_a_row_in_another_section() {
        let mut app = work_app();
        press(&mut app, 's');
        press(&mut app, '/');
        assert_eq!(app.mode(), Mode::SettingsFilter);
        for c in "tree".chars() {
            press(&mut app, c);
        }
        assert_eq!(settings_on(&app).selected().unwrap().id, "proc.tree");
        press_code(&mut app, KeyCode::Enter);
        assert_eq!(
            app.mode(),
            Mode::Settings,
            "enter keeps the filter, stops typing"
        );
        let before = app.config.show_tree;
        press(&mut app, ' ');
        assert_eq!(app.config.show_tree, !before);
        press_code(&mut app, KeyCode::Esc);
        assert!(settings_on(&app).filter.is_empty(), "first esc clears");
        press_code(&mut app, KeyCode::Esc);
        assert!(app.settings.is_none(), "second esc closes");
    }

    #[test]
    fn settings_replaces_the_dashboard() {
        let mut app = work_app();
        let dashboard = app.screen().join("\n");
        assert!(dashboard.contains("proc1"), "{dashboard}");
        press(&mut app, 's');
        let screen = app.screen().join("\n");
        assert!(screen.contains("settings"), "{screen}");
        assert!(
            !screen.contains("proc1"),
            "no process rows behind settings:\n{screen}"
        );
        assert!(
            !screen.contains('╭'),
            "no panel boxes behind settings:\n{screen}"
        );
    }

    #[test]
    fn clicking_a_settings_value_changes_it_and_a_category_switches() {
        let mut app = work_app();
        press(&mut app, 's');
        let (x, y) = row_of(&app.screen(), "panels");
        app.click(x, y);
        assert_eq!(settings_on(&app).category, SettingsCategory::Panels);
        let screen = app.screen();
        let (_, y) = row_of(&screen, "disk box");
        let before = app.config.show_disk;
        app.click(app.last_area.width - 4, y);
        assert_eq!(app.config.show_disk, !before, "value column cycles");
        assert!(app.settings.is_some(), "clicks never close by accident");
        let (x, y) = row_of(&app.screen(), "esc close");
        app.click(x, y);
        assert!(app.settings.is_none());
    }

    #[test]
    fn clicking_a_theme_swatch_applies_and_persists_it() {
        let mut app = work_app();
        app.open_settings(SettingsCategory::Appearance);
        let name = theme_options()[2];
        let (x, y) = row_of(&app.screen(), &format!(" {name} "));
        app.click(x, y);
        assert_eq!(app.config.theme, name);
        assert_eq!(app.theme.name, name);
        assert!(app.dirty_prefs);
    }

    fn pick_theme(app: &mut App, name: &str) {
        let mut next = app.config.clone();
        name.clone_into(&mut next.theme);
        app.apply_config(next);
    }

    #[test]
    fn picking_a_pack_theme_applies_it() {
        let mut app = work_app();
        pick_theme(&mut app, "rose-pine");
        assert_eq!(app.config.theme, "rose-pine");
        assert_eq!(app.theme.name, "rose-pine");
    }

    #[test]
    fn picking_a_core_theme_applies_it() {
        let mut app = work_app();
        pick_theme(&mut app, "nord");
        assert_eq!(app.theme.name, "nord");
        assert!(app.status.is_none());
    }

    #[test]
    fn capital_t_walks_into_the_pack() {
        let mut app = work_app();
        "gruvbox-dark".clone_into(&mut app.config.theme);
        press(&mut app, 'T');
        assert_eq!(app.config.theme, "dracula");
    }

    #[test]
    fn wheel_in_settings_moves_the_row() {
        let mut app = work_app();
        press(&mut app, 's');
        app.send(Action::Scroll {
            col: 30,
            row: 5,
            delta: 1,
        });
        assert_eq!(settings_on(&app).row, 1);
    }

    #[test]
    fn key_profile_row_switches_the_keymap() {
        let mut app = work_app();
        app.open_settings(SettingsCategory::Keys);
        press_code(&mut app, KeyCode::Right);
        assert_eq!(app.config.keymap, "htop");
        assert_eq!(app.profile, Profile::Htop);
        assert_eq!(app.help_groups, keymap::help_groups(Profile::Htop));
    }

    #[test]
    fn keymap_from_config_applies_at_startup() {
        let app = App::with_source(
            Config {
                keymap: String::from("btop"),
                ..Config::default()
            },
            SnapshotSource::Scripted(Vec::new().into()),
        );
        assert_eq!(app.profile, Profile::Btop);
    }

    #[test]
    fn cycling_theme_writes_config_theme() {
        // regression: `T` swapped the palette but saved the old name
        let mut app = work_app();
        press(&mut app, 'T');
        assert_eq!(app.config.theme, app.theme.name);
    }

    /// Regression (owner feedback): the cluttered top settings bar is gone;
    /// settings and themes live in the footer's `siltop ≡` menu.
    #[test]
    fn no_top_bar_and_the_footer_menu_reaches_settings() {
        let mut app = work_app();
        let screen = app.screen();
        assert!(screen[0].starts_with('╭'), "{}", screen[0]);
        assert!(!screen[0].contains("themes"), "{}", screen[0]);
        let (x, y) = row_of(&screen, "siltop ≡");
        assert_eq!(y, 39, "the menu button sits in the footer");
        app.click(x, y);
        let (x, y) = row_of(&app.screen(), "Settings");
        app.click(x + 1, y);
        assert_eq!(settings_on(&app).category, SettingsCategory::General);
    }

    #[test]
    fn dragging_the_split_keeps_the_settings_row_in_step() {
        let mut app = work_app();
        app.dragging_split = true;
        app.drag_split(40);
        assert_eq!(app.config.proc_ratio, app.proc_ratio);
    }

    #[test]
    fn clicks_under_help_never_reach_the_hidden_footer() {
        for page in [None, Some('s'), Some('H')] {
            let mut app = app();
            app.at(80, 24);
            app.set_processes(procs(10));
            let footer = app.screen()[23].clone();
            let quit = column_of(&footer, "q quit") + 1;
            let kill = column_of(&footer, "x kill") + 1;
            if let Some(key) = page {
                press(&mut app, key);
            }
            press(&mut app, '?');
            assert!(app.help);
            for col in [kill, quit] {
                app.click(col, 23);
                assert!(!app.quit, "{page:?}: a click under help quit the app");
                assert!(
                    !app.confirm_kill,
                    "{page:?}: a click under help armed a kill"
                );
                if !app.help {
                    press(&mut app, '?');
                }
            }
            assert_eq!(app.settings.is_some(), page == Some('s'), "{page:?}");
        }
    }

    #[test]
    fn clicking_outside_help_closes_it_and_inside_keeps_it() {
        let mut app = work_app();
        press(&mut app, '?');
        let rect = help_rect(app.last_area, &app.help_groups);
        app.click(rect.x + 2, rect.y + 2);
        assert!(app.help);
        app.click(0, 1);
        assert!(!app.help);
    }

    #[test]
    fn help_scroll_is_clamped_at_60x16() {
        let mut app = work_app();
        app.last_area = Rect::new(0, 0, 60, 16);
        press(&mut app, '?');
        let max = help_max_scroll(app.last_area, &app.help_groups);
        assert!(max > 0, "the generated help cannot fit 16 rows");
        for _ in 0..500 {
            press(&mut app, 'j');
        }
        assert_eq!(app.help_scroll, max);
        press_code(&mut app, KeyCode::Home);
        assert_eq!(app.help_scroll, 0);
        press_code(&mut app, KeyCode::PageDown);
        assert_eq!(app.help_scroll, 10.min(max));
        press_code(&mut app, KeyCode::End);
        assert_eq!(app.help_scroll, max);
        for _ in 0..500 {
            press(&mut app, 'k');
        }
        assert_eq!(app.help_scroll, 0);
        assert!(app.proc.selected_pid().is_some());
        assert!(app.detail_pid.is_none(), "help keys never reach the list");
    }

    #[test]
    fn generated_help_reaches_every_group_by_scrolling() {
        let mut app = work_app();
        press(&mut app, '?');
        let mut text = String::new();
        for _ in 0..10 {
            text.push_str(&app.screen().join("\n"));
            press_code(&mut app, KeyCode::PageDown);
        }
        for group in &app.help_groups {
            assert!(text.contains(group.title), "{}", group.title);
        }
        assert!(text.contains("cycle work / glance"), "{text}");
    }

    #[test]
    fn htop_t_sorts_instead_of_saying_not_available_yet() {
        let mut app = work_app();
        app.set_profile(Profile::Htop);
        press(&mut app, 'T');
        assert_eq!(app.config.proc_sort, ProcSort::Started);
        let toast = app.status.as_ref().map_or("", |(m, _)| m.as_str());
        assert!(!toast.contains("not available"), "{toast}");
        assert!(!app.confirm_kill);
    }

    #[test]
    fn capital_k_force_quits_and_x_quits_from_the_list() {
        let mut app = work_app();
        press(&mut app, 'K');
        assert!(app.confirm_kill);
        assert_eq!(app.pending_signal, Signal::Kill);
        let footer = app.screen()[39].clone();
        assert!(footer.contains("Force quit"), "{footer}");
        press_code(&mut app, KeyCode::Esc);
        assert!(!app.confirm_kill);
        press(&mut app, 'x');
        assert_eq!(app.pending_signal, Signal::Term);
        let footer = app.screen()[39].clone();
        assert!(footer.contains(" Quit proc"), "{footer}");
        press(&mut app, 'n');
        assert!(!app.confirm_kill);
    }

    #[test]
    fn dossier_interrupt_arms_int_for_the_open_pid() {
        let mut app = work_app();
        press_code(&mut app, KeyCode::Enter);
        let pid = app.detail_pid;
        press(&mut app, 'i');
        assert_eq!(app.pending_signal, Signal::Int);
        assert_eq!(app.confirm.map(|t| t.pid), pid);
    }

    #[test]
    fn t_toggles_the_tree_and_o_cycles_sort() {
        let mut app = work_app();
        let tree = app.config.show_tree;
        press(&mut app, 't');
        assert_eq!(app.config.show_tree, !tree);
        let sort = app.config.proc_sort;
        press(&mut app, 'o');
        assert_eq!(app.config.proc_sort, sort.next());
    }

    #[test]
    fn htop_profile_drives_the_app() {
        let mut app = work_app();
        app.set_profile(Profile::Htop);
        press(&mut app, 'M');
        assert_eq!(app.config.proc_sort, ProcSort::Mem);
        press(&mut app, 'P');
        assert_eq!(app.config.proc_sort, ProcSort::Cpu);
        press_code(&mut app, KeyCode::F(9));
        assert!(app.signal_menu.is_some(), "F9 picks a signal, as in htop");
        press_code(&mut app, KeyCode::Esc);
        assert!(app.signal_menu.is_none());
        press(&mut app, 'x');
        assert!(app.confirm_kill);
        press_code(&mut app, KeyCode::Esc);
        press_code(&mut app, KeyCode::F(7));
        assert!(app.status.is_some(), "F7 renices (and reports the outcome)");
        press_code(&mut app, KeyCode::F(10));
        assert!(app.quit);
        assert!(
            app.help_groups
                .iter()
                .any(|g| g.rows.iter().any(|(k, _)| k.contains("F9")))
        );
    }

    #[test]
    fn btop_profile_arrows_walk_sort_columns() {
        let mut app = work_app();
        app.set_profile(Profile::Btop);
        let sort = app.config.proc_sort;
        press_code(&mut app, KeyCode::Right);
        assert_eq!(app.config.proc_sort, sort.next());
        press_code(&mut app, KeyCode::Left);
        assert_eq!(app.config.proc_sort, sort);
        let reversed = app.sort_reversed();
        press(&mut app, 'r');
        assert_eq!(app.sort_reversed(), !reversed);
        let tree = app.config.show_tree;
        press(&mut app, 'e');
        assert_eq!(app.config.show_tree, !tree, "btop's e is the tree");
    }

    #[test]
    fn dossier_exit_closes_the_card_with_a_warning() {
        let mut app = scripted(Vec::new());
        app.at(120, 40);
        app.update(snap_msg(), app.now);
        app.send(Action::Move(2));
        app.send(Action::Open);
        let gone = app.detail_pid.expect("open");
        let mut next = snapshot();
        next.processes.retain(|p| p.pid != gone);
        app.update(AppMsg::Snapshot(Box::new(Ok(next))), app.now);
        assert!(app.detail_pid.is_none());
        assert_eq!(app.status_kind, ToastKind::Warn);
        assert!(app.status.as_ref().unwrap().0.contains("exited"));
    }

    #[test]
    fn mode_precedence_puts_the_topmost_overlay_first() {
        let mut app = work_app();
        assert_eq!(app.mode(), Mode::Normal);
        app.detail_pid = Some(safe(1));
        assert_eq!(app.mode(), Mode::Detail);
        app.searching = true;
        assert_eq!(app.mode(), Mode::Search);
        app.settings = Some(SettingsState::default());
        assert_eq!(app.mode(), Mode::Settings);
        app.settings = Some(SettingsState {
            filtering: true,
            ..SettingsState::default()
        });
        assert_eq!(app.mode(), Mode::SettingsFilter);
        app.help = true;
        assert_eq!(app.mode(), Mode::Help);
        app.confirm_kill = true;
        assert_eq!(app.mode(), Mode::Confirm);
    }

    #[test]
    fn random_key_storm_never_panics_and_keeps_invariants() {
        let keys: Vec<AppMsg> = (' '..='~')
            .map(key_msg)
            .chain(
                [
                    KeyCode::Up,
                    KeyCode::Down,
                    KeyCode::Enter,
                    KeyCode::Esc,
                    KeyCode::Tab,
                    KeyCode::Home,
                    KeyCode::End,
                    KeyCode::PageUp,
                    KeyCode::PageDown,
                    KeyCode::Delete,
                    KeyCode::Backspace,
                ]
                .into_iter()
                .map(code_msg),
            )
            .collect();
        for profile in Profile::ALL {
            let mut app = work_app();
            app.set_profile(profile);
            let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
            for _ in 0..3000 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let i = usize::try_from(seed % keys.len() as u64).unwrap();
                let msg = match &keys[i] {
                    AppMsg::Input(ev) => AppMsg::Input(ev.clone()),
                    _ => unreachable!(),
                };
                // never send a real signal: answer every confirm with no
                if app.confirm_kill {
                    app.reduce(Action::ConfirmNo);
                }
                let now = app.now;
                if matches!(&msg, AppMsg::Input(TermEvent::Key(k)) if k.code == KeyCode::Char('y'))
                {
                    continue;
                }
                app.update(msg, now);
                if app.quit {
                    app.quit = false;
                }
                assert!(
                    !(app.help && app.settings.is_some() && app.mode() != Mode::Help),
                    "{profile:?}: help sits on top of settings"
                );
                assert!(
                    app.help_scroll <= help_max_scroll(app.last_area, &app.help_groups),
                    "{profile:?}"
                );
                if let Some(pid) = app.confirm.map(|t| t.pid) {
                    assert!(pid >= 900_000, "only fixture pids can be targeted");
                }
            }
        }
    }

    // ── tags, app groups and folds ──────────────────────────────────────

    fn listed(app: &App) -> Vec<u32> {
        app.proc.model.rows().iter().map(|r| r.pid).collect()
    }

    fn table_app(processes: Vec<Process>) -> App {
        let mut app = app();
        app.at(120, 40);
        app.focus = Focus::Processes;
        app.set_processes(processes);
        app.send(Action::SortBy(ProcSort::Pid));
        app
    }

    fn bundled(pid: u32, name: &str, cmd: &str, cpu: f32) -> Process {
        Process {
            command: Some(cmd.to_owned()),
            ..proc(pid, name, cpu)
        }
    }

    /// An app with two helpers, plus three plain processes.
    fn app_procs() -> Vec<Process> {
        let mut out = procs(3);
        out.push(bundled(
            safe(20),
            "Arc",
            "/Applications/Arc.app/Contents/MacOS/Arc",
            1.0,
        ));
        out.push(bundled(
            safe(21),
            "Arc Helper",
            "/Applications/Arc.app/Contents/Frameworks/Arc Helper.app/Contents/MacOS/Arc Helper",
            60.0,
        ));
        out.push(bundled(
            safe(22),
            "Arc Helper (GPU)",
            "/Applications/Arc.app/Contents/Frameworks/Arc Helper (GPU).app/Contents/MacOS/Arc Helper (GPU)",
            5.0,
        ));
        out
    }

    #[test]
    fn dossier_tabs_cycle_and_read_only_the_open_pid() {
        let mut app = table_app(procs(3));
        app.send(Action::Open);
        assert_eq!(app.proc.tab, DossierTab::Overview);
        assert!(app.proc.inspection.is_none(), "overview reads nothing");
        app.send(Action::NextPanel);
        assert_eq!(app.proc.tab, DossierTab::Threads);
        assert!(app.detail_pid.is_some(), "tab stays in the dossier");
        app.send(Action::PrevPanel);
        app.send(Action::PrevPanel);
        assert_eq!(app.proc.tab, DossierTab::Ports, "wraps backwards");
        app.send(Action::DossierTab(1));
        assert_eq!(app.proc.tab, DossierTab::Overview);
        assert!(app.proc.inspection.is_none());
        app.send(Action::DossierTab(9));
        assert_eq!(app.proc.tab, DossierTab::Overview, "no tab 9");
        app.send(Action::Close);
        app.send(Action::NextPanel);
        assert_eq!(
            app.proc.tab,
            DossierTab::Overview,
            "tab outside a dossier hops boxes"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn dossier_threads_page_inspects_this_process() {
        let me = std::process::id();
        let mut app = table_app(vec![proc(me, "siltop-test", 1.0)]);
        app.send(Action::Open);
        app.send(Action::DossierTab(2));
        let ins = app
            .proc
            .inspection
            .as_ref()
            .expect("own process is readable");
        assert_eq!(ins.pid, me);
        assert!(!ins.threads.is_empty(), "a test runner has threads");
    }

    #[test]
    fn space_tags_and_steps_down_so_a_run_of_spaces_tags_a_run() {
        let mut app = table_app(procs(5));
        app.send(Action::Tag);
        app.send(Action::Tag);
        assert_eq!(app.proc.tags.pids().collect::<Vec<_>>(), [safe(1), safe(2)]);
        assert_eq!(app.proc.selected_pid(), Some(safe(3)));
    }

    #[test]
    fn tags_survive_a_resort() {
        let mut app = table_app(procs(5));
        app.send(Action::Tag);
        for sort in [ProcSort::Mem, ProcSort::Name, ProcSort::Pid] {
            app.send(Action::SortBy(sort));
            app.send(Action::ReverseSort);
            assert_eq!(app.proc.tags.pids().collect::<Vec<_>>(), [safe(1)]);
        }
    }

    #[test]
    fn tags_drop_when_the_pid_exits_on_a_new_sample() {
        let mut app = table_app(procs(4));
        let shown = app.snapshot.processes.clone();
        app.proc.toggle_tags(&[safe(2), safe(3)], &shown);
        let mut snap = app.snapshot.clone();
        snap.processes.retain(|p| p.pid != safe(3));
        snap.sampled.procs = true;
        app.apply_snapshot(snap);
        assert_eq!(app.proc.tags.pids().collect::<Vec<_>>(), [safe(2)]);
    }

    #[test]
    fn tags_drop_when_a_new_process_reuses_the_pid_after_a_freeze() {
        // Pids wrap during a long freeze: the tag must not pass to the
        // stranger now holding pid 3, or x would signal it unseen.
        let mut app = table_app(procs(4));
        let shown = app.snapshot.processes.clone();
        app.proc.toggle_tags(&[safe(2), safe(3)], &shown);
        let mut snap = app.snapshot.clone();
        for p in snap.processes.iter_mut().filter(|p| p.pid == safe(3)) {
            p.start_unix += 86_400;
        }
        snap.sampled.procs = true;
        app.apply_snapshot(snap);
        assert_eq!(app.proc.tags.pids().collect::<Vec<_>>(), [safe(2)]);
    }

    #[test]
    fn esc_and_u_clear_tags() {
        let mut app = table_app(procs(4));
        let shown = app.snapshot.processes.clone();
        app.proc.toggle_tags(&[safe(1)], &shown);
        app.send(Action::Cancel);
        assert!(app.proc.tags.is_empty(), "esc clears tags");
        assert!(
            app.menu.is_none(),
            "and does not open the menu while doing so"
        );
        let shown = app.snapshot.processes.clone();
        app.proc.toggle_tags(&[safe(1)], &shown);
        app.send(Action::ClearTags);
        assert!(app.proc.tags.is_empty());
    }

    #[test]
    fn signal_picker_applies_to_the_tagged_set() {
        let mut app = table_app(procs(5));
        let shown = app.snapshot.processes.clone();
        app.proc.toggle_tags(&[safe(2), safe(4)], &shown);
        app.send(Action::SignalMenu);
        let menu = app.signal_menu.as_ref().expect("picker");
        let pids: Vec<u32> = menu.set.iter().map(|t| t.pid).collect();
        assert_eq!(
            pids,
            [safe(2), safe(4)],
            "the selection (proc1) is not in it"
        );
        assert_eq!(menu.label, "2 processes");
        assert_eq!(app.proc.armed, [safe(2), safe(4)]);
    }

    #[test]
    fn x_on_tags_arms_a_confirm_naming_every_target() {
        let mut app = table_app(procs(5));
        let shown = app.snapshot.processes.clone();
        app.proc.toggle_tags(&[safe(3), safe(5)], &shown);
        app.send(Action::Kill);
        assert!(app.confirm_kill);
        let set: Vec<u32> = app.confirm_set.iter().map(|t| t.pid).collect();
        assert_eq!(set, [safe(3), safe(5)]);
        let screen = app.screen().join("\n");
        assert!(
            screen.contains("Quit 2 processes: proc3, proc5?"),
            "prompt lists the set:\n{screen}"
        );
        app.send(Action::ConfirmNo);
        assert!(app.confirm_set.is_empty() && app.proc.armed.is_empty());
        assert_eq!(app.proc.tags.len(), 2, "cancel keeps the tags");
    }

    #[test]
    fn a_group_row_signals_every_member() {
        let mut app = table_app(app_procs());
        app.send(Action::SortBy(ProcSort::Cpu));
        app.send(Action::ToggleGroups);
        assert_eq!(listed(&app)[0], safe(20), "Arc's 66% heads the list");
        app.send(Action::Kill);
        let set: Vec<u32> = app.confirm_set.iter().map(|t| t.pid).collect();
        assert_eq!(set, [safe(20), safe(21), safe(22)]);
        app.send(Action::ConfirmNo);
        app.send(Action::Tag);
        assert_eq!(app.proc.tags.len(), 3, "space tags the whole group");
    }

    #[test]
    fn enter_opens_a_group_instead_of_the_dossier() {
        let mut app = table_app(app_procs());
        app.send(Action::SortBy(ProcSort::Cpu));
        app.send(Action::ToggleGroups);
        assert_eq!(listed(&app).len(), 4);
        app.send(Action::Open);
        assert!(app.detail_pid.is_none(), "a group has no single dossier");
        assert_eq!(listed(&app).len(), 6);
        app.send(Action::Move(1));
        app.send(Action::Open);
        assert_eq!(
            app.detail_pid,
            Some(safe(21)),
            "a helper row opens its dossier"
        );
    }

    /// The dossier open on the Files page of `procs(5)`'s first row, with
    /// `n` open files injected as if read (fake pids are not inspectable).
    fn files_page(n: i32) -> App {
        let mut app = table_app(procs(5));
        app.send(Action::Open);
        app.send(Action::DossierTab(3));
        let pid = app.detail_pid.expect("dossier open");
        app.proc.inspection = Some(siltop_core::Inspection {
            pid,
            files: (0..n)
                .map(|fd| siltop_core::OpenFile {
                    fd,
                    path: format!("/tmp/f{fd}"),
                })
                .collect(),
            ..siltop_core::Inspection::default()
        });
        app
    }

    #[test]
    fn arrows_scroll_an_open_page_and_keep_the_dossier_on_its_pid() {
        let mut app = files_page(100);
        let pid = app.detail_pid;
        app.send(Action::Move(1));
        app.send(Action::Move(1));
        assert_eq!(app.proc.page_offset, 2);
        assert_eq!(app.detail_pid, pid, "arrows scroll the page, not the list");
        app.send(Action::Move(-5));
        assert_eq!(app.proc.page_offset, 0, "never above the first row");
        app.send(Action::Bottom);
        let bottom = app.proc.page_offset;
        assert!(bottom > 50 && bottom < 100, "{bottom}");
        app.send(Action::Move(1));
        assert_eq!(app.proc.page_offset, bottom, "never past the last row");
        app.send(Action::Top);
        assert_eq!(app.proc.page_offset, 0);
        app.send(Action::Page(1));
        let pane = app.proc_pane().expect("pane");
        assert_eq!(app.proc.page_offset, app.proc.page_step(pane));
        let screen = app.screen().join("\n");
        assert!(screen.contains("↑"), "the scrolled page says so:\n{screen}");
    }

    #[test]
    fn switching_tabs_starts_the_new_page_at_its_top() {
        let mut app = files_page(100);
        app.send(Action::Move(4));
        assert_eq!(app.proc.page_offset, 4);
        app.send(Action::NextPanel);
        assert_eq!(app.proc.tab, DossierTab::Ports);
        assert_eq!(app.proc.page_offset, 0);
    }

    #[test]
    fn arrows_on_the_overview_still_walk_the_process_list() {
        let mut app = files_page(100);
        app.send(Action::DossierTab(1));
        let pid = app.detail_pid;
        app.send(Action::Move(1));
        assert_ne!(app.detail_pid, pid, "overview: ↓ opens the next process");
        assert_eq!(app.proc.page_offset, 0);
    }

    #[test]
    fn wheel_over_the_dossier_scrolls_its_page_only() {
        let mut app = files_page(100);
        let pane = app.proc_pane().expect("pane");
        let card = detail_rect(pane);
        let offset = app.proc.offset;
        app.send(Action::Scroll {
            col: card.x + 4,
            row: card.y + 5,
            delta: 1,
        });
        assert_eq!(app.proc.page_offset, usize::try_from(WHEEL_ROWS).unwrap());
        assert_eq!(app.proc.offset, offset, "the table under it stays put");
        app.send(Action::Scroll {
            col: 0,
            row: 0,
            delta: 1,
        });
        assert_eq!(
            app.proc.page_offset,
            usize::try_from(WHEEL_ROWS).unwrap(),
            "outside the card the wheel does nothing"
        );
    }

    #[test]
    fn a_shorter_reread_pulls_the_page_back_onto_real_rows() {
        let mut app = files_page(100);
        app.send(Action::Bottom);
        assert!(app.proc.page_offset > 0);
        if let Some(ins) = app.proc.inspection.as_mut() {
            ins.files.truncate(3);
        }
        let pane = app.proc_pane().expect("pane");
        app.proc.scroll_page(0, pane);
        assert_eq!(app.proc.page_offset, 0);
    }

    #[test]
    fn group_signal_picker_targets_exactly_the_members_and_names_them() {
        let mut app = table_app(app_procs());
        app.send(Action::SortBy(ProcSort::Cpu));
        app.send(Action::ToggleGroups);
        app.send(Action::SignalMenu);
        let menu = app.signal_menu.as_ref().expect("picker");
        let pids: Vec<u32> = menu.set.iter().map(|t| t.pid).collect();
        assert_eq!(pids, [safe(20), safe(21), safe(22)]);
        assert_eq!(menu.label, "3 processes");
        app.send(Action::ConfirmNo);
        app.send(Action::Kill);
        let screen = app.screen().join("\n");
        assert!(
            screen.contains("Quit 3 processes: Arc, Arc Helper, Arc Helper (GPU)?"),
            "the prompt counts and names every member:\n{screen}"
        );
    }

    #[test]
    fn a_collapsed_tree_node_stays_collapsed_across_refreshes() {
        let mut list = procs(5);
        list[1].ppid = safe(1);
        list[2].ppid = safe(1);
        let mut app = table_app(list);
        app.send(Action::ToggleTree);
        app.send(Action::Fold);
        assert_eq!(listed(&app), [safe(1), safe(4), safe(5)]);
        for round in 0..4_u32 {
            let mut snap = app.snapshot.clone();
            snap.sampled.procs = true;
            // cpu shifts every sample and a new child joins the subtree
            for p in &mut snap.processes {
                p.cpu = Some(((p.pid + round) % 7) as f32);
            }
            let mut child = proc(safe(10 + round), "late-child", 1.0);
            child.ppid = safe(1);
            snap.processes.push(child);
            app.apply_snapshot(snap);
            assert_eq!(
                listed(&app),
                [safe(1), safe(4), safe(5)],
                "round {round}: still collapsed"
            );
        }
        let row = app.proc.model.rows()[0];
        assert_eq!(
            row.agg.map(|a| a.hidden()),
            Some(6),
            "+N counts the new children"
        );
    }

    #[test]
    fn rv_a_reused_pid_does_not_inherit_a_dead_nodes_collapse() {
        let mut list = procs(3);
        list[1].ppid = safe(1);
        let mut app = table_app(list);
        app.send(Action::ToggleTree);
        app.send(Action::Fold);
        assert_eq!(listed(&app), [safe(1), safe(3)]);
        let mut snap = app.snapshot.clone();
        snap.sampled.procs = true;
        snap.processes.retain(|p| p.pid != safe(1));
        app.apply_snapshot(snap);
        let mut snap = app.snapshot.clone();
        snap.sampled.procs = true;
        snap.processes.push(proc(safe(1), "reborn", 1.0));
        snap.processes[0].ppid = safe(1);
        app.apply_snapshot(snap);
        assert_eq!(listed(&app).len(), 3, "the new pid opens unfolded");
    }

    #[test]
    fn arrows_fold_nested_rows_and_hop_boxes_when_flat() {
        let mut list = procs(4);
        list[1].ppid = safe(1);
        list[2].ppid = safe(1);
        let mut app = table_app(list);
        app.send(Action::Fold);
        assert_ne!(app.focus, Focus::Processes, "flat list: ← is previous box");
        app.focus = Focus::Processes;
        app.send(Action::ToggleTree);
        assert_eq!(listed(&app).len(), 4);
        app.send(Action::Fold);
        assert_eq!(app.focus, Focus::Processes, "tree: ← folds, focus stays");
        assert_eq!(listed(&app), [safe(1), safe(4)]);
        app.send(Action::Unfold);
        assert_eq!(listed(&app).len(), 4);
        app.send(Action::FoldAll(false));
        assert_eq!(listed(&app).len(), 2);
        app.send(Action::FoldAll(true));
        assert_eq!(listed(&app).len(), 4);
    }

    #[cfg(target_os = "macos")]
    mod live_signals {
        use super::*;
        use std::process::{Child, Command};

        /// A real child, listed with the start second the kernel reports:
        /// the one `kill(pid, 0)` accepts among the last few seconds.
        fn sleeper(name: &str) -> (Child, Process) {
            let child = Command::new("/bin/sleep").arg("30").spawn().expect("spawn");
            let pid = child.id();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_secs();
            let now = i64::try_from(now).expect("fits");
            let start = (now - 3..=now + 1)
                .find(|&s| can_signal(KillTarget { pid, start_unix: s }).is_ok())
                .expect("start second found");
            let proc = Process {
                start_unix: start,
                ..proc(pid, name, 1.0)
            };
            (child, proc)
        }

        fn stat(pid: u32) -> String {
            let out = Command::new("/bin/ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .expect("ps");
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        }

        fn settled(pid: u32, want: char) -> bool {
            (0..50).any(|_| {
                let s = stat(pid);
                std::thread::sleep(std::time::Duration::from_millis(20));
                s.starts_with(want)
            })
        }

        #[test]
        fn signal_to_the_tagged_set_reaches_exactly_those_pids() {
            let (mut a, pa) = sleeper("sleep-a");
            let (mut b, pb) = sleeper("sleep-b");
            let (mut c, pc) = sleeper("sleep-c");
            let (ia, ib, ic) = (pa.pid, pb.pid, pc.pid);
            let mut app = table_app(vec![pa, pb, pc]);
            let shown = app.snapshot.processes.clone();
            app.proc.toggle_tags(&[ia, ic], &shown);
            app.send(Action::SignalMenu);
            let stop = Signal::ALL
                .iter()
                .position(|s| *s == Signal::Stop)
                .expect("stop listed");
            app.reduce(Action::Move(i32::try_from(stop).expect("small")));
            app.reduce(Action::ConfirmYes);
            assert!(app.confirm_kill, "STOP asks first");
            app.reduce(Action::ConfirmYes);
            assert!(settled(ia, 'T'), "a stopped: {}", stat(ia));
            assert!(settled(ic, 'T'), "c stopped: {}", stat(ic));
            assert!(!stat(ib).starts_with('T'), "b untouched: {}", stat(ib));
            let tags: Vec<u32> = app.proc.tags.pids().collect();
            let mut want = vec![ia, ic];
            want.sort_unstable();
            assert_eq!(tags, want, "a frozen set stays tagged for CONT");
            let cont = Signal::ALL
                .iter()
                .position(|s| *s == Signal::Cont)
                .expect("cont listed");
            app.send(Action::SignalMenu);
            app.reduce(Action::Move(i32::try_from(cont).expect("small")));
            app.reduce(Action::ConfirmYes);
            assert!(settled(ia, 'S'), "a resumed: {}", stat(ia));
            assert!(settled(ic, 'S'), "c resumed: {}", stat(ic));
            for child in [&mut a, &mut b, &mut c] {
                let _ = child.kill();
                let _ = child.wait();
            }
        }

        #[test]
        fn quitting_a_tag_whose_pid_was_reused_lets_it_go() {
            // listed with a start time the live process never had: the
            // tagged process is gone and the pid names another one now
            let (mut child, listed) = sleeper("reused");
            let pid = listed.pid;
            let stale = Process {
                start_unix: listed.start_unix - 3600,
                ..listed
            };
            let mut app = table_app(vec![stale]);
            app.proc.toggle_tags(&[pid], &app.snapshot.processes);
            app.reduce(Action::Kill);
            app.reduce(Action::ConfirmYes);
            let alive = child.try_wait().expect("wait").is_none();
            let _ = child.kill();
            let _ = child.wait();
            assert!(alive, "the process now holding the pid is not signalled");
            assert!(app.proc.tags.is_empty(), "the gone process is untagged");
        }

        #[test]
        fn replay_never_signals_the_live_process_a_trace_names() {
            let (mut child, listed) = sleeper("recorded-sleep");
            let pid = listed.pid;
            let mut app = replaying(Instant::now());
            app.at(120, 40);
            app.set_processes(vec![listed]);
            for action in [Action::Kill, Action::ForceKill, Action::SignalMenu] {
                app.reduce(action);
                app.reduce(Action::ConfirmYes);
            }
            let alive = child.try_wait().expect("wait").is_none();
            let state = stat(pid);
            let _ = child.kill();
            let _ = child.wait();
            assert!(alive, "the live process a replay names is never signalled");
            assert!(!state.starts_with('T'), "nor frozen: {state}");
        }

        #[test]
        fn signal_to_an_app_group_reaches_exactly_its_members() {
            let bundle = "/Applications/Fake.app/Contents";
            let (mut a, mut pa) = sleeper("Fake");
            let (mut b, mut pb) = sleeper("Fake Helper");
            let (mut c, mut pc) = sleeper("Fake Helper (GPU)");
            let (mut d, pd) = sleeper("outsider");
            pa.command = Some(format!("{bundle}/MacOS/Fake"));
            pb.command = Some(format!(
                "{bundle}/Frameworks/Fake Helper.app/Contents/MacOS/Fake Helper"
            ));
            pc.command = Some(format!(
                "{bundle}/Frameworks/Fake Helper (GPU).app/Contents/MacOS/Fake Helper (GPU)"
            ));
            let (ia, ib, ic, id) = (pa.pid, pb.pid, pc.pid, pd.pid);
            let mut app = table_app(vec![pa, pb, pc, pd]);
            app.send(Action::ToggleGroups);
            assert_eq!(listed(&app), [ia, id], "one group row and the outsider");
            app.send(Action::SignalMenu);
            let stop = Signal::ALL
                .iter()
                .position(|s| *s == Signal::Stop)
                .expect("stop listed");
            app.reduce(Action::Move(i32::try_from(stop).expect("small")));
            app.reduce(Action::ConfirmYes);
            assert!(app.confirm_kill, "STOP asks first");
            let mut armed: Vec<u32> = app.confirm_set.iter().map(|t| t.pid).collect();
            armed.sort_unstable();
            assert_eq!(armed, [ia, ib, ic], "exactly the members");
            app.reduce(Action::ConfirmYes);
            for pid in [ia, ib, ic] {
                assert!(settled(pid, 'T'), "{pid} stopped: {}", stat(pid));
            }
            assert!(
                !stat(id).starts_with('T'),
                "outsider untouched: {}",
                stat(id)
            );
            for child in [&mut a, &mut b, &mut c, &mut d] {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    fn status_of(app: &App) -> &str {
        app.status.as_ref().map_or("", |(s, _)| s.as_str())
    }

    /// Every key that reaches a process, from the list and from the dossier.
    const PROCESS_ACTIONS: [Action; 6] = [
        Action::Kill,
        Action::ForceKill,
        Action::Interrupt,
        Action::SignalMenu,
        Action::Renice(1),
        Action::ToggleBackground,
    ];

    fn assert_actions_refused(app: &mut App, why: &str) {
        for dossier in [None, Some(safe(11))] {
            app.detail_pid = dossier;
            for action in PROCESS_ACTIONS {
                app.status = None;
                app.reduce(action);
                assert!(
                    !app.confirm_kill && app.signal_menu.is_none(),
                    "{action:?} armed nothing"
                );
                assert_eq!(status_of(app), why, "{action:?} says why");
            }
        }
        app.detail_pid = None;
    }

    #[test]
    fn replay_process_actions_never_reach_a_live_process() {
        let mut app = replaying(Instant::now());
        app.at(120, 40);
        app.set_processes(vec![proc(safe(11), "recorded", 1.0)]);
        assert_actions_refused(&mut app, "replay: process actions are off");
    }

    #[test]
    fn demo_process_actions_never_reach_a_live_process() {
        let mut app = work_app();
        app.demo = true;
        app.set_processes(vec![proc(safe(11), "Finder", 1.0)]);
        assert_actions_refused(&mut app, "demo: process actions are off");
    }

    #[test]
    fn recorded_dossier_never_inspects_the_live_pid_of_that_number() {
        // our own pid: live, readable, and certainly not the recorded row
        let live = std::process::id();
        for demo in [true, false] {
            let mut app = if demo {
                let mut app = work_app();
                app.demo = true;
                app
            } else {
                replaying(Instant::now())
            };
            app.at(120, 40);
            app.set_processes(vec![proc(live, "Finder", 1.0)]);
            app.detail_pid = Some(live);
            for tab in [DossierTab::Threads, DossierTab::Files, DossierTab::Ports] {
                app.proc.tab = tab;
                app.refresh_inspection();
                let ins = app
                    .proc
                    .inspection
                    .as_ref()
                    .expect("an answer, not reading…");
                assert!(ins.recorded, "demo={demo} {tab:?} is marked recorded");
                assert!(
                    ins.threads.is_empty() && ins.files.is_empty() && ins.sockets.is_empty(),
                    "demo={demo} {tab:?} read nothing live"
                );
            }
        }
    }

    fn first_row(app: &App) -> Option<u32> {
        app.proc.model.rows().first().map(|row| row.pid)
    }

    #[test]
    fn biggest_first_off_is_applied_at_startup_in_settings_and_by_r() {
        let mut app = App::with_source(
            Config {
                sort_desc: false,
                ..Config::default()
            },
            SnapshotSource::Scripted(Vec::new().into()),
        );
        app.at(120, 40);
        app.set_processes(
            (1..=5)
                .map(|i| proc(safe(i), "p", i as f32 * 10.0))
                .collect(),
        );
        app.sync_model();
        assert!(
            app.sort_reversed(),
            "the saved direction applies at startup"
        );
        assert_eq!(first_row(&app), Some(safe(1)), "least cpu first");
        let mut next = app.live_config();
        next.sort_desc = true;
        app.apply_config(next);
        app.sync_model();
        assert!(!app.sort_reversed(), "the settings toggle applies at once");
        assert_eq!(first_row(&app), Some(safe(5)), "most cpu first");
        app.dirty_prefs = false;
        app.reduce(Action::ReverseSort);
        assert!(!app.prefs().sort_desc && app.dirty_prefs, "r is saved");
        app.reduce(Action::SortBy(ProcSort::Pid));
        assert!(!app.sort_reversed(), "a new column starts its natural way");
    }

    #[test]
    fn process_keys_on_glance_or_another_expanded_box_never_act_unseen() {
        for hide in [Action::CycleSurface, Action::Expand] {
            let mut app = work_app();
            app.focus = Focus::Cpu;
            app.reduce(hide);
            assert!(app.proc_pane().is_none(), "{hide:?} hides the table");
            for action in [
                Action::Kill,
                Action::ForceKill,
                Action::SignalMenu,
                Action::Tag,
            ] {
                app.status = None;
                app.reduce(action);
                assert!(!app.confirm_kill && app.signal_menu.is_none());
                assert!(app.proc.tags.is_empty(), "{action:?} tagged nothing");
                assert_eq!(status_of(&app), NO_LIST, "{action:?} says why");
            }
            app.reduce(Action::Search);
            assert!(app.proc_pane().is_some(), "/ brings the list into view");
            app.reduce(Action::FilterChar('q'));
            assert!(!app.quit && app.proc.filter == "q", "typed where it shows");
        }
    }

    /// Same as [`confirm_yes_from_detail_closes_the_card`]: the let-go path
    /// runs only when the signal was actually delivered or the pid was gone.
    #[cfg(target_os = "macos")]
    #[test]
    fn confirming_stop_keeps_the_tags_and_the_dossier() {
        let rows = vec![proc(safe(1), "a", 1.0), proc(safe(2), "b", 2.0)];
        let mut app = table_app(rows);
        app.proc
            .toggle_tags(&[safe(1), safe(2)], &app.snapshot.processes);
        let stop = Signal::ALL
            .iter()
            .position(|s| *s == Signal::Stop)
            .expect("listed");
        app.reduce(Action::SignalMenu);
        app.reduce(Action::Move(i32::try_from(stop).expect("small")));
        app.reduce(Action::ConfirmYes);
        assert!(app.confirm_kill);
        app.reduce(Action::ConfirmYes);
        assert_eq!(app.proc.tags.len(), 2, "a frozen set stays tagged for CONT");
        app.proc.tags.clear();
        open_menu_on(&mut app, safe(1));
        app.reduce(Action::Move(i32::try_from(stop).expect("small")));
        app.reduce(Action::ConfirmYes);
        app.reduce(Action::ConfirmYes);
        assert_eq!(app.detail_pid, Some(safe(1)), "the dossier stays open");
        // an ending signal still lets go of a process that is gone
        app.proc
            .toggle_tags(&[safe(1), safe(2)], &app.snapshot.processes);
        app.detail_pid = None;
        app.reduce(Action::Kill);
        app.reduce(Action::ConfirmYes);
        assert!(app.proc.tags.is_empty(), "quit sets are released");
    }

    #[test]
    fn wheel_reaches_only_the_topmost_overlay() {
        let mut app = app();
        app.at(60, 16);
        app.set_processes(procs(200));
        let pane = app.pane();
        let scroll = Action::Scroll {
            col: pane.x + 5,
            row: pane.y + 5,
            delta: 1,
        };
        app.reduce(Action::Help);
        app.reduce(scroll);
        assert!(app.help_scroll > 0, "the wheel scrolls help");
        assert_eq!(app.proc.offset, 0, "not the table under it");
        app.reduce(Action::Help);
        app.reduce(Action::Menu);
        app.reduce(scroll);
        assert_eq!(
            app.menu.as_ref().map(|m| m.selected),
            Some(2),
            "the wheel walks the menu"
        );
        assert_eq!(app.proc.offset, 0);
        app.reduce(Action::Cancel);
        open_menu_on(&mut app, safe(1));
        app.reduce(scroll);
        assert_eq!(app.signal_menu.as_ref().map(|m| m.selected), Some(1));
        assert_eq!(app.proc.offset, 0);
    }

    #[test]
    fn a_saved_pack_theme_survives_a_look_change() {
        let mut app = App::with_source(
            Config {
                theme: String::from("dracula"),
                ..Config::default()
            },
            SnapshotSource::Scripted(Vec::new().into()),
        );
        let mut next = app.live_config();
        next.extra_contrast = !next.extra_contrast;
        app.apply_config(next);
        assert_eq!(app.theme.name, "dracula");
    }
}

/// Persistence as the loop drives it: session-only surface, the dragged
/// split, and the debounced save against a real file.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod config_tests {
    use super::*;
    use crate::config_io::SAVE_DEBOUNCE;
    use crate::config_io::tests::TempDir;

    fn blank(config: Config) -> App {
        App::with_source(config, SnapshotSource::Scripted(Vec::new().into()))
    }

    fn on_disk(tmp: &TempDir) -> Config {
        ConfigStore::open(&tmp.file()).1
    }

    #[test]
    fn d16_surface_keys_are_session_only() {
        let mut app = blank(Config::default());
        for surface in [Surface::Work, Surface::Glance, Surface::Work] {
            app.lock_surface(surface);
            assert_eq!(app.effective_surface(), surface);
            assert_eq!(app.prefs().surface, None, "config stays auto");
            assert!(!app.dirty_prefs, "a surface key never schedules a save");
        }
    }

    #[test]
    fn d16_configured_surface_is_kept_under_a_session_override() {
        let mut app = blank(Config {
            surface: Some(Surface::Glance),
            ..Config::default()
        });
        assert_eq!(app.effective_surface(), Surface::Glance);
        app.lock_surface(Surface::Work);
        assert_eq!(app.effective_surface(), Surface::Work);
        assert_eq!(app.prefs().surface, Some(Surface::Glance));
    }

    #[test]
    fn d16_auto_surface_follows_the_terminal_until_locked() {
        let mut app = blank(Config::default());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(50, 20)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!(
            app.effective_surface(),
            Surface::Glance,
            "50 cols is glance"
        );
        app.lock_surface(Surface::Work);
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!(app.effective_surface(), Surface::Work, "lock beats auto");
    }

    #[test]
    fn d15_dragged_split_is_persisted() {
        let tmp = TempDir::new("drag");
        let (mut store, config, _) = ConfigStore::open(&tmp.file());
        let mut app = blank(config);
        app.dragging_split = true;
        app.last_area = Rect::new(0, 0, 120, 40);
        app.drag_split(40);
        let ratio = app.proc_ratio;
        assert_ne!(ratio, PROC_RATIO_DEFAULT_FOR_TEST);
        assert_eq!(app.prefs().proc_ratio, ratio);
        let t0 = Instant::now();
        assert!(app.sync_prefs(&mut store, t0).is_none());
        assert_eq!(store.deadline(), Some(t0 + SAVE_DEBOUNCE));
        app.sync_prefs(&mut store, t0 + SAVE_DEBOUNCE);
        assert_eq!(on_disk(&tmp).proc_ratio, ratio);
    }

    const PROC_RATIO_DEFAULT_FOR_TEST: u16 = siltop_core::PROC_RATIO_DEFAULT;

    #[test]
    fn d15_drag_ratio_sweep_stays_in_bounds_and_round_trips() {
        let tmp = TempDir::new("drag-sweep");
        let (mut store, config, _) = ConfigStore::open(&tmp.file());
        let mut app = blank(config);
        app.dragging_split = true;
        for width in [40_u16, 60, 80, 120, 200, 400, u16::MAX] {
            app.last_area = Rect::new(0, 0, width, 40);
            for col in [
                0,
                1,
                width / 3,
                width / 2,
                width.saturating_sub(1),
                u16::MAX,
            ] {
                app.drag_split(col);
                assert!((PROC_RATIO_MIN..=PROC_RATIO_MAX).contains(&app.proc_ratio));
            }
        }
        store.flush(&app.prefs());
        assert_eq!(on_disk(&tmp).proc_ratio, app.proc_ratio);
    }

    #[test]
    fn d15_change_then_signal_flush_lands_on_disk() {
        let tmp = TempDir::new("hup");
        let (mut store, config, _) = ConfigStore::open(&tmp.file());
        let mut app = blank(config);
        app.config.show_disk = true;
        app.touch();
        let t0 = Instant::now();
        app.sync_prefs(&mut store, t0);
        assert!(!tmp.file().exists(), "still inside the debounce");
        // What run() does after the loop ends on HUP/TERM/INT or quit.
        assert!(store.flush(&app.prefs()).is_none());
        assert!(on_disk(&tmp).show_disk);
    }

    #[test]
    fn settings_changes_debounce_into_one_save() {
        let tmp = TempDir::new("burst");
        let (mut store, config, _) = ConfigStore::open(&tmp.file());
        let mut app = blank(config);
        let t0 = Instant::now();
        for step in 0..10_u32 {
            let now = t0 + step * Duration::from_millis(200);
            app.config.show_net = !app.config.show_net;
            app.set_interval(app.config.cycle_interval());
            app.sync_prefs(&mut store, now);
            assert!(!tmp.file().exists(), "step {step}");
        }
        let last = t0 + 9 * Duration::from_millis(200);
        app.sync_prefs(&mut store, last + Duration::from_millis(999));
        assert!(!tmp.file().exists());
        app.sync_prefs(&mut store, last + SAVE_DEBOUNCE);
        let saved = on_disk(&tmp);
        assert_eq!(saved.show_net, app.config.show_net);
        assert_eq!(saved.interval, app.config.interval);
    }

    #[test]
    fn idle_loop_never_schedules_a_save() {
        let tmp = TempDir::new("idle");
        let (mut store, config, _) = ConfigStore::open(&tmp.file());
        let mut app = blank(config);
        let t0 = Instant::now();
        for i in 0..50_u32 {
            assert!(app.sync_prefs(&mut store, t0 + i * SAVE_DEBOUNCE).is_none());
        }
        assert_eq!(store.deadline(), None);
        assert!(!tmp.file().exists());
    }

    #[test]
    fn surface_lock_is_not_written_even_with_other_changes() {
        let tmp = TempDir::new("surface-write");
        let (mut store, config, _) = ConfigStore::open(&tmp.file());
        let mut app = blank(config);
        app.lock_surface(Surface::Glance);
        app.config.show_fans = false;
        app.touch();
        store.flush(&app.prefs());
        let text = std::fs::read_to_string(tmp.file()).unwrap();
        assert!(!text.contains("surface"), "{text}");
        assert!(!on_disk(&tmp).show_fans);
    }

    #[test]
    fn cli_interval_survives_an_unrelated_settings_change() {
        // Regression (wave 3 merge): the settings page stepped from the
        // persisted prefs, so any change reverted a CLI `--interval` in the
        // live config while the worker kept sampling at the flag's rate.
        let file = Config::default();
        let cli = crate::cli::Cli {
            interval: Some(Duration::from_millis(250)),
            ..crate::cli::Cli::default()
        };
        let session = crate::with_cli_overrides(file.clone(), &cli);
        let mut app = blank(session.clone());
        app.overrides = config_io::SessionOverrides::between(&file, &session);
        let mut state = SettingsState::open(SettingsCategory::General);
        let row = state.rows().position(|def| def.id == "units.net_bits");
        state.row = row.expect("network units row");
        app.settings = Some(state);
        app.step_setting(SettingStep::Next);
        assert!(app.config.net_bits, "the setting changed");
        assert_eq!(
            app.config.interval,
            Duration::from_millis(250),
            "flag holds"
        );
        assert_eq!(app.prefs().interval, file.interval, "and is not persisted");
        assert!(app.prefs().net_bits, "the choice is");
    }

    #[test]
    fn rv_cli_interval_is_session_only_in_the_loop() {
        let tmp = TempDir::new("cli-interval");
        let (mut store, file, _) = ConfigStore::open(&tmp.file());
        let cli = crate::cli::Cli {
            interval: Some(Duration::from_millis(250)),
            surface: Some(Surface::Glance),
            ..crate::cli::Cli::default()
        };
        let session = crate::with_cli_overrides(file.clone(), &cli);
        let mut app = blank(session.clone());
        app.overrides = config_io::SessionOverrides::between(&file, &session);
        assert_eq!(app.config.interval, Duration::from_millis(250), "applies");
        app.config.show_gpu = !app.config.show_gpu;
        app.touch();
        store.flush(&app.prefs());
        let saved = on_disk(&tmp);
        assert_eq!(saved.interval, file.interval);
        assert_eq!(saved.surface, file.surface);
        assert_eq!(saved.show_gpu, !file.show_gpu);
        // a later in-session change is the user's choice and does persist
        app.set_interval(app.config.step_interval(false));
        let chosen = app.config.interval;
        store.flush(&app.prefs());
        assert_eq!(on_disk(&tmp).interval, chosen);
    }
}
