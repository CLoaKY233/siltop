//! First-run consent. Three screens, once, remembered in `config.toml`.
//! Headless modes, a missing TTY, `CI` and `SILTOP_NO_ONBOARDING` never
//! see it and never wait on it. They keep the safe default: no update check.

use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::time::Duration;

use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::widgets::{Paragraph, Wrap};
use siltop_core::Config;
use siltop_ui::{OnboardScreen, OnboardView, onboard_hit, render_onboarding};

use crate::cli::{self, Mode};
use crate::config_io::{self, ConfigStore};
use crate::first_run;
use crate::keymap::Profile;

/// Whether this launch is allowed to draw the screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Launch {
    pub tty: bool,
    pub ci: bool,
    pub disabled: bool,
    pub mode: Mode,
    pub demo: bool,
}

impl Launch {
    /// The real process: stdin and stdout are terminals, and the env is read.
    #[must_use]
    pub fn current(mode: Mode, demo: bool) -> Self {
        Self {
            tty: std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
            ci: std::env::var_os("CI").is_some(),
            disabled: std::env::var_os("SILTOP_NO_ONBOARDING").is_some(),
            mode,
            demo,
        }
    }

    /// Headless, scripted and non-interactive launches never ask, and
    /// `SILTOP_NO_ONBOARDING` skips the screens.
    #[must_use]
    pub fn suppressed(self) -> bool {
        self.disabled || self.non_interactive()
    }

    /// No one is watching: no terminal, CI, the demo, or a scripted mode.
    /// The opt-in update check honours this but not `SILTOP_NO_ONBOARDING`.
    #[must_use]
    pub fn non_interactive(self) -> bool {
        if !self.tty || self.ci || self.demo {
            return true;
        }
        matches!(
            self.mode,
            Mode::Json
                | Mode::JsonStream
                | Mode::Line
                | Mode::Diagnose
                | Mode::Doctor
                | Mode::Replay
                | Mode::Help
                | Mode::Version
                | Mode::PrintConfig
                | Mode::Man
                | Mode::Completions(_)
        )
    }
}

/// Whether this launch shows the screens. `force` is
/// `siltop onboarding`; otherwise only a config that never finished
/// the flow (`onboarding.completed` empty) asks.
#[must_use]
pub fn present(config: &Config, launch: Launch, force: bool) -> bool {
    if launch.suppressed() {
        return false;
    }
    if force {
        return true;
    }
    // Only the dashboard asks by itself. Other commands leave the choice.
    matches!(launch.mode, Mode::Tui | Mode::Onboarding) && config.onboarding_completed.is_empty()
}

/// Remember a finished or skipped flow: the version that ran it and the
/// update choice. Esc passes `false`.
pub fn remember(config: &mut Config, check: bool) {
    cli::VERSION.clone_into(&mut config.onboarding_completed);
    config.updates_check = check;
}

/// How the flow ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// An answer on the update question: the flow is done.
    Finished,
    /// Esc: the rest is skipped with update checks off.
    Skipped,
    /// `q` or Ctrl-C: nothing is saved and the app does not start.
    Quit,
}

/// Smallest terminal the screens fit in without spilling past their border.
pub const MIN_COLS: u16 = 40;
pub const MIN_ROWS: u16 = 12;

/// The first focus on `screen`: "No" on the update question, so an Enter
/// pressed without reading keeps siltop offline.
fn default_focus(screen: OnboardScreen) -> usize {
    match screen {
        OnboardScreen::Updates => screen.choices().len().saturating_sub(1),
        _ => 0,
    }
}

/// Where the flow stands, apart from the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Flow {
    screen: OnboardScreen,
    focus: usize,
    check: bool,
}

impl Flow {
    fn new(screen: OnboardScreen) -> Self {
        Self {
            screen,
            focus: default_focus(screen),
            check: false,
        }
    }

    /// Activates the focused choice.
    fn choose(&mut self) -> Option<End> {
        let next = match self.screen {
            OnboardScreen::Welcome => OnboardScreen::Privacy,
            OnboardScreen::Privacy => OnboardScreen::Updates,
            OnboardScreen::Updates => {
                self.check = self.focus == 0;
                return Some(End::Finished);
            }
        };
        *self = Self {
            check: self.check,
            ..Self::new(next)
        };
        None
    }

    fn key(&mut self, key: KeyEvent) -> Option<End> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        let last = self.screen.choices().len().saturating_sub(1);
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Some(End::Quit);
            }
            KeyCode::Char('q' | 'Q') => return Some(End::Quit),
            KeyCode::Esc => return Some(End::Skipped),
            KeyCode::Enter | KeyCode::Char(' ') => return self.choose(),
            KeyCode::Left | KeyCode::BackTab | KeyCode::Char('h') => {
                self.focus = self.focus.saturating_sub(1);
            }
            KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => {
                self.focus = (self.focus + 1).min(last);
            }
            KeyCode::Char('y' | 'Y') if self.screen == OnboardScreen::Updates => {
                self.focus = 0;
                return self.choose();
            }
            KeyCode::Char('n' | 'N') if self.screen == OnboardScreen::Updates => {
                self.focus = last;
                return self.choose();
            }
            _ => {}
        }
        None
    }

    /// A left click: a choice is focused and taken, as Enter would.
    fn click(&mut self, area: Rect, keys: &str, col: u16, row: u16) -> Option<End> {
        if !fits(area) {
            return None;
        }
        let view = OnboardView {
            screen: self.screen,
            focus: self.focus,
            keys,
        };
        let at = onboard_hit(area, &view, col, row)?;
        self.focus = at;
        self.choose()
    }
}

fn fits(area: Rect) -> bool {
    area.width >= MIN_COLS && area.height >= MIN_ROWS
}

/// The screens, or on a terminal too small for them, a line asking for a
/// bigger window (the keys still work).
fn draw(frame: &mut ratatui::Frame, flow: &Flow, keys: &str) {
    let area = frame.area();
    let theme = siltop_ui::Theme::default();
    if fits(area) {
        let view = OnboardView {
            screen: flow.screen,
            focus: flow.focus,
            keys,
        };
        render_onboarding(frame, area, &view, &theme);
        return;
    }
    let text = format!(
        "siltop setup needs a {MIN_COLS}x{MIN_ROWS} window. Make it bigger, or press Esc to skip."
    );
    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(
        Paragraph::new(text)
            .style(theme.fg())
            .wrap(Wrap { trim: true }),
        area,
    );
}

/// Draw the flow and save the result. A suppressed launch returns at once
/// with [`End::Skipped`], saving nothing.
pub fn run(config_path: Option<&Path>, force: bool, demo: bool) -> io::Result<End> {
    let path = config_path
        .map(Path::to_path_buf)
        .or_else(config_io::default_path);
    let mut config = path.as_deref().map(Config::load).unwrap_or_default();
    let launch = Launch::current(if force { Mode::Onboarding } else { Mode::Tui }, demo);
    if !present(&config, launch, force) {
        return Ok(End::Skipped);
    }
    let mut flow = Flow::new(OnboardScreen::Welcome);
    let keys = first_run::hint(Profile::default());
    let mut term = crate::tui::install().map_err(io::Error::other)?;
    let outcome = (|| {
        loop {
            term.draw(|frame| draw(frame, &flow, &keys))?;
            if !event::poll(Duration::from_secs(3600))? {
                continue;
            }
            let (cols, rows) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
            let area = Rect::new(0, 0, cols, rows);
            let end = match event::read()? {
                Event::Key(key) => flow.key(key),
                Event::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left) => {
                    flow.click(area, &keys, mouse.column, mouse.row)
                }
                _ => None,
            };
            if let Some(end) = end {
                return Ok::<End, io::Error>(end);
            }
        }
    })();
    drop(term);
    crate::tui::restore_best_effort();
    let end = outcome?;
    if end == End::Quit {
        return Ok(end);
    }
    remember(&mut config, end == End::Finished && flow.check);
    if let Some(path) = path.as_deref() {
        save(path, &config)?;
    }
    Ok(end)
}

fn save(path: &Path, config: &Config) -> io::Result<()> {
    let (mut store, _, _) = ConfigStore::open(path);
    store.mark(std::time::Instant::now());
    // Flush compares against what the file already held. A brand-new file
    // has the defaults, so the onboarding keys are the change.
    if let Some(message) = store.flush(config) {
        return Err(io::Error::other(message));
    }
    // `flush` skips a file that matches what it loaded. Force the keys
    // when the in-memory config was built from defaults and nothing else
    // differed, which is a fresh directory.
    if !path.exists() {
        let text = config.to_toml().map_err(io::Error::other)?;
        config_io::atomic_write(path, &text)?;
    }
    Ok(())
}

/// `siltop onboarding`, including the "needs a terminal" line.
pub fn command(config: Option<&Path>, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let launch = Launch::current(Mode::Onboarding, false);
    if launch.suppressed() && !launch.tty {
        let _ = writeln!(err, "siltop: onboarding needs a terminal");
        return cli::EXIT_USAGE;
    }
    if launch.ci || launch.disabled {
        return 0;
    }
    match run(config, true, false) {
        Ok(End::Quit) => {
            let _ = writeln!(out, "siltop: onboarding closed, nothing saved");
            0
        }
        Ok(End::Skipped) => {
            let _ = writeln!(out, "siltop: onboarding skipped; update checks stay off");
            0
        }
        Ok(End::Finished) => {
            let _ = writeln!(out, "siltop: onboarding saved");
            0
        }
        Err(error) => {
            let _ = writeln!(err, "siltop: {error}");
            cli::EXIT_ERROR
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn dash(tty: bool) -> Launch {
        Launch {
            tty,
            ci: false,
            disabled: false,
            mode: Mode::Tui,
            demo: false,
        }
    }

    #[test]
    fn a_fresh_config_is_due_once_and_a_finished_one_is_not() {
        let fresh = Config::default();
        assert!(present(&fresh, dash(true), false));
        let mut done = fresh.clone();
        remember(&mut done, false);
        assert_eq!(done.onboarding_completed, cli::VERSION);
        assert!(!present(&done, dash(true), false));
        assert!(present(&done, dash(true), true), "the command asks again");
    }

    #[test]
    fn esc_records_checks_off() {
        let mut config = Config {
            updates_check: true,
            ..Config::default()
        };
        remember(&mut config, false);
        assert_eq!(config.onboarding_completed, cli::VERSION);
        assert!(!config.updates_check);
    }

    #[test]
    fn yes_turns_the_check_on_and_is_remembered() {
        let mut config = Config::default();
        remember(&mut config, true);
        assert!(config.updates_check);
        let text = config.to_toml().unwrap();
        assert!(text.contains("check = true"), "{text}");
    }

    #[test]
    fn headless_modes_never_ask_even_with_a_fresh_config() {
        let fresh = Config::default();
        for mode in [
            Mode::Json,
            Mode::JsonStream,
            Mode::Line,
            Mode::Diagnose,
            Mode::Doctor,
            Mode::Replay,
        ] {
            let launch = Launch { mode, ..dash(true) };
            assert!(!present(&fresh, launch, false), "{mode:?}");
        }
        assert!(!present(&fresh, dash(false), false), "no tty");
        let mut ci = dash(true);
        ci.ci = true;
        assert!(!present(&fresh, ci, true), "CI wins over force");
        let mut off = dash(true);
        off.disabled = true;
        assert!(!present(&fresh, off, false));
        let mut demo = dash(true);
        demo.demo = true;
        assert!(!present(&fresh, demo, false));
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn choosing_no_finishes_with_checks_off() {
        let mut flow = Flow::new(OnboardScreen::Updates);
        flow.check = true;
        flow.focus = 1;
        assert_eq!(flow.choose(), Some(End::Finished));
        assert!(!flow.check);
    }

    #[test]
    fn the_update_question_starts_on_no() {
        let mut flow = Flow::new(OnboardScreen::Welcome);
        assert_eq!(flow.key(press(KeyCode::Enter)), None);
        assert_eq!(flow.key(press(KeyCode::Enter)), None);
        assert_eq!(flow.screen, OnboardScreen::Updates);
        assert_eq!(OnboardScreen::Updates.choices()[flow.focus], "No");
        assert_eq!(flow.key(press(KeyCode::Enter)), Some(End::Finished));
        assert!(!flow.check, "Enter without choosing keeps checks off");
        let mut yes = Flow::new(OnboardScreen::Updates);
        assert_eq!(yes.key(press(KeyCode::Char('y'))), Some(End::Finished));
        assert!(yes.check);
    }

    #[test]
    fn q_and_ctrl_c_quit_and_esc_skips_on_every_screen() {
        for screen in [
            OnboardScreen::Welcome,
            OnboardScreen::Privacy,
            OnboardScreen::Updates,
        ] {
            let mut flow = Flow::new(screen);
            assert_eq!(flow.key(press(KeyCode::Char('q'))), Some(End::Quit));
            let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
            assert_eq!(flow.key(ctrl_c), Some(End::Quit));
            assert_eq!(flow.key(press(KeyCode::Esc)), Some(End::Skipped));
            assert_eq!(flow.key(press(KeyCode::Char('c'))), None);
        }
    }

    #[test]
    fn every_choice_is_clickable_and_a_tiny_window_ignores_clicks() {
        let area = Rect::new(0, 0, 80, 24);
        for screen in [
            OnboardScreen::Welcome,
            OnboardScreen::Privacy,
            OnboardScreen::Updates,
        ] {
            for at in 0..screen.choices().len() {
                let (col, row) = (0..area.height)
                    .flat_map(|row| (0..area.width).map(move |col| (col, row)))
                    .find(|&(col, row)| {
                        let view = OnboardView {
                            screen,
                            focus: 0,
                            keys: "m menu",
                        };
                        onboard_hit(area, &view, col, row) == Some(at)
                    })
                    .unwrap_or_else(|| panic!("{screen:?} choice {at} is not clickable"));
                let mut flow = Flow::new(screen);
                let end = flow.click(area, "m menu", col, row);
                if screen == OnboardScreen::Updates {
                    assert_eq!(end, Some(End::Finished));
                    assert_eq!(flow.check, at == 0);
                } else {
                    assert_eq!(flow.screen, next(screen), "{screen:?}");
                }
                let mut tiny = Flow::new(screen);
                assert_eq!(tiny.click(Rect::new(0, 0, 20, 5), "", 3, 2), None);
                assert_eq!(tiny.screen, screen);
            }
        }
    }

    fn next(screen: OnboardScreen) -> OnboardScreen {
        let mut flow = Flow::new(screen);
        flow.choose();
        flow.screen
    }

    #[test]
    fn a_tiny_window_asks_for_room_inside_its_bounds() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut term = Terminal::new(TestBackend::new(20, 5)).unwrap();
        let flow = Flow::new(OnboardScreen::Welcome);
        term.draw(|frame| draw(frame, &flow, "")).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains("bigger"), "{text}");
        assert!(!text.contains('╭'), "no card drawn: {text}");
    }
}
