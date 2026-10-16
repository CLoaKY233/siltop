use std::cell::Cell;
use std::io::{self, BufWriter, Stdout, Write, stdout};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Once, OnceLock};
use std::thread::{self, ThreadId};

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::{MoveTo, Show};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::{Frame, Terminal};
use siltop_core::{Error, Result};

/// Button presses, drags, any-motion (`?1003`, for hover) and SGR
/// coordinates. Terminals report motion once per cell crossed, and the app
/// redraws only when the button under the pointer changes.
pub const MOUSE_ON: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h";
pub const MOUSE_OFF: &str = "\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l";
const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
const SYNC_END: &[u8] = b"\x1b[?2026l";
const OUT_CAPACITY: usize = 64 * 1024;

static RAW_MODE: AtomicBool = AtomicBool::new(false);
static ALT_SCREEN: AtomicBool = AtomicBool::new(false);
/// Cleared by `--no-mouse`: then mouse reporting is never switched on.
static MOUSE: AtomicBool = AtomicBool::new(true);

pub fn set_mouse(enabled: bool) {
    MOUSE.store(enabled, Ordering::SeqCst);
}

fn mouse_on() -> &'static str {
    if MOUSE.load(Ordering::SeqCst) {
        MOUSE_ON
    } else {
        ""
    }
}

pub type AppTerminal = Screen<Stdout>;

/// Set for a 16-colour terminal (`TERM=xterm`, `linux`): frames go out
/// with the classic `30–37`/`90–97` colours, not `38;5;N`.
static ANSI16: AtomicBool = AtomicBool::new(false);

pub fn set_ansi16(enabled: bool) {
    ANSI16.store(enabled, Ordering::SeqCst);
}

/// Buffers output and ignores flushes while its gate is held, so the
/// flushes ratatui issues mid-draw (cursor hide, clear) coalesce.
pub struct FrameWriter<W: Write> {
    out: BufWriter<W>,
    hold: Rc<Cell<bool>>,
    /// A 16-colour terminal's frame, kept whole until it is rewritten.
    frame: Vec<u8>,
    ansi16: bool,
}

impl<W: Write> Write for FrameWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.ansi16 {
            self.frame.extend_from_slice(buf);
            Ok(buf.len())
        } else {
            self.out.write(buf)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.hold.get() {
            return Ok(());
        }
        if !self.frame.is_empty() {
            let frame = std::mem::take(&mut self.frame);
            self.out.write_all(&sgr16(&frame))?;
        }
        self.out.flush()
    }
}

/// Rewrites every `38;5;N` / `48;5;N` with `N < 16` inside an SGR sequence
/// into the named form (`31`, `91`, `41`, `101`), which every 16-colour
/// terminal reads; crossterm spells named colours as indexed ones.
fn sgr16(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'[') {
            let start = i + 2;
            let end = bytes[start..]
                .iter()
                .position(|b| !(b.is_ascii_digit() || *b == b';'))
                .map(|n| start + n);
            if let Some(end) = end.filter(|&e| bytes[e] == b'm')
                && let Ok(params) = std::str::from_utf8(&bytes[start..end])
            {
                out.extend_from_slice(b"\x1b[");
                out.extend_from_slice(named_params(params).as_bytes());
                out.push(b'm');
                i = end + 1;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

fn named_params(params: &str) -> String {
    let parts: Vec<&str> = params.split(';').collect();
    let mut out: Vec<String> = Vec::with_capacity(parts.len());
    let mut k = 0;
    while k < parts.len() {
        let index = parts.get(k + 2).and_then(|n| n.parse::<u8>().ok());
        if let (Some(&("38" | "48")), Some(&"5"), Some(n)) = (parts.get(k), parts.get(k + 1), index)
            && n < 16
        {
            let base = if parts[k] == "38" { 30 } else { 40 };
            let code = if n < 8 { base + n } else { base + 60 + n - 8 };
            out.push(code.to_string());
            k += 3;
        } else {
            out.push(parts[k].to_owned());
            k += 1;
        }
    }
    out.join(";")
}

/// The terminal plus the gate of its writer: every draw reaches the
/// terminal as one write, wrapped in DEC 2026 synchronized-update markers.
pub struct Screen<W: Write> {
    terminal: Terminal<CrosstermBackend<FrameWriter<W>>>,
    hold: Rc<Cell<bool>>,
}

impl<W: Write> Screen<W> {
    fn with_viewport(inner: W, viewport: Option<ratatui::Viewport>) -> io::Result<Self> {
        Self::with_depth(inner, viewport, ANSI16.load(Ordering::SeqCst))
    }

    fn with_depth(inner: W, viewport: Option<ratatui::Viewport>, ansi16: bool) -> io::Result<Self> {
        let hold = Rc::new(Cell::new(false));
        let writer = FrameWriter {
            out: BufWriter::with_capacity(OUT_CAPACITY, inner),
            hold: Rc::clone(&hold),
            frame: Vec::new(),
            ansi16,
        };
        let backend = CrosstermBackend::new(writer);
        let terminal = match viewport {
            None => Terminal::new(backend)?,
            Some(viewport) => {
                Terminal::with_options(backend, ratatui::TerminalOptions { viewport })?
            }
        };
        Ok(Self { terminal, hold })
    }

    /// Wipes the screen and forgets what it showed, so the next draw
    /// repaints it all. (`Terminal::clear` would ask the terminal for the
    /// cursor, a reply the input thread may swallow.)
    pub fn clear(&mut self) -> io::Result<()> {
        execute!(self.terminal.backend_mut(), Clear(ClearType::All))?;
        self.terminal.swap_buffers();
        Ok(())
    }

    pub fn draw(&mut self, render: impl FnOnce(&mut Frame)) -> io::Result<()> {
        self.hold.set(true);
        let begun = self.terminal.backend_mut().write_all(SYNC_BEGIN);
        let drawn = begun.and_then(|()| self.terminal.draw(render).map(|_| ()));
        self.hold.set(false);
        let backend = self.terminal.backend_mut();
        let ended = backend
            .write_all(SYNC_END)
            .and_then(|()| Write::flush(backend));
        drawn.and(ended)
    }
}

/// The thread that owns the terminal: the one that installed the hook.
static OWNER: OnceLock<ThreadId> = OnceLock::new();

/// Only a panic on the terminal's owner tears the screen down. A background
/// thread's panic is caught at its boundary (`guard_thread`) and reported
/// to the loop, which then exits or carries on with the screen intact.
fn restores_for(current: ThreadId, owner: Option<ThreadId>) -> bool {
    owner.is_none_or(|owner| owner == current)
}

pub fn install_panic_hook() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let _ = OWNER.set(thread::current().id());
        let original = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            crate::crash::write(info);
            if restores_for(thread::current().id(), OWNER.get().copied()) {
                restore_best_effort();
                original(info);
            }
        }));
    });
}

/// Runs a background thread's body; a panic comes back as its message
/// instead of unwinding past the thread (and printing over the screen).
pub fn guard_thread(body: impl FnOnce()) -> std::result::Result<(), String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).map_err(|payload| {
        payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| String::from("unknown panic"))
    })
}

pub fn install() -> Result<AppTerminal> {
    install_panic_hook();
    enable_raw_mode().map_err(|err| Error::terminal(err.to_string()))?;
    RAW_MODE.store(true, Ordering::SeqCst);
    let mut out = stdout();
    let entered = execute!(out, EnterAlternateScreen)
        .and_then(|()| out.write_all(mouse_on().as_bytes()))
        .and_then(|()| out.flush());
    ALT_SCREEN.store(true, Ordering::SeqCst);
    if let Err(err) = entered {
        restore_best_effort();
        return Err(Error::terminal(err.to_string()));
    }
    Screen::with_viewport(stdout(), None).map_err(|err| {
        restore_best_effort();
        Error::terminal(err.to_string())
    })
}

/// Leaves the alternate screen blank, homed and with the cursor shown.
/// Clearing first matters where the alternate screen is missing (tmux with
/// `alternate-screen off`, a bare pty): whatever prints next (the bench
/// report, an error) starts at the top of an empty screen instead of on
/// top of the last frame.
fn leave_screen(out: &mut impl Write) -> io::Result<()> {
    out.write_all(MOUSE_OFF.as_bytes())?;
    execute!(
        out,
        Clear(ClearType::All),
        MoveTo(0, 0),
        Show,
        LeaveAlternateScreen
    )
}

/// Takes the screen back after a stop: raw mode, the alternate screen and
/// the mouse, each only if it is not already on.
pub fn reenter() -> Result<()> {
    if !RAW_MODE.swap(true, Ordering::SeqCst) {
        enable_raw_mode().map_err(|err| Error::terminal(err.to_string()))?;
    }
    if !ALT_SCREEN.swap(true, Ordering::SeqCst) {
        let mut out = stdout();
        execute!(out, EnterAlternateScreen)
            .and_then(|()| out.write_all(mouse_on().as_bytes()))
            .and_then(|()| out.flush())
            .map_err(|err| Error::terminal(err.to_string()))?;
    }
    Ok(())
}

/// Ctrl-Z: hand the terminal back as the shell left it, stop, and on
/// `fg` take it again. The caller redraws the whole screen after.
pub fn suspend() -> Result<()> {
    restore()?;
    crate::signals::stop_self();
    reenter()
}

/// Undoes only what `install` did, once; later calls are no-ops.
pub fn restore() -> Result<()> {
    let mut result = Ok(());
    if ALT_SCREEN.swap(false, Ordering::SeqCst) {
        result = leave_screen(&mut stdout()).map_err(|err| Error::terminal(err.to_string()));
    }
    if RAW_MODE.swap(false, Ordering::SeqCst) {
        let raw = disable_raw_mode().map_err(|err| Error::terminal(err.to_string()));
        result = result.and(raw);
    }
    result
}

pub fn restore_best_effort() {
    let _ = restore();
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use ratatui::Viewport;
    use ratatui::layout::Rect;
    use ratatui::widgets::Paragraph;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Stats {
        writes: usize,
        flushes: usize,
        bytes: Vec<u8>,
    }

    struct Counting(Rc<RefCell<Stats>>);

    impl Write for Counting {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut stats = self.0.borrow_mut();
            stats.writes += 1;
            stats.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.borrow_mut().flushes += 1;
            Ok(())
        }
    }

    #[test]
    fn sgr16_names_the_sixteen_colours_and_leaves_the_rest() {
        let out = |s: &str| String::from_utf8(sgr16(s.as_bytes())).unwrap();
        assert_eq!(out("\x1b[38;5;1mX"), "\x1b[31mX");
        assert_eq!(out("\x1b[38;5;9mX"), "\x1b[91mX");
        assert_eq!(out("\x1b[48;5;4m"), "\x1b[44m");
        assert_eq!(out("\x1b[48;5;15m"), "\x1b[107m");
        assert_eq!(out("\x1b[1;38;5;2;48;5;0m"), "\x1b[1;32;40m");
        assert_eq!(out("\x1b[38;5;196m"), "\x1b[38;5;196m", "256 stays");
        assert_eq!(out("\x1b[38;2;1;2;3m"), "\x1b[38;2;1;2;3m", "rgb stays");
        assert_eq!(out("\x1b[2J\x1b[5;1H"), "\x1b[2J\x1b[5;1H", "not SGR");
        assert_eq!(out("é ✕"), "é ✕");
    }

    #[test]
    fn a_16_colour_frame_leaves_as_named_sgr() {
        use ratatui::style::{Color, Style};
        let stats = Rc::new(RefCell::new(Stats::default()));
        let mut screen = Screen::with_depth(
            Counting(Rc::clone(&stats)),
            Some(Viewport::Fixed(Rect::new(0, 0, 4, 1))),
            true,
        )
        .unwrap();
        screen
            .draw(|f| {
                f.render_widget(
                    Paragraph::new("hi").style(Style::default().fg(Color::Red).bg(Color::Blue)),
                    f.area(),
                );
            })
            .unwrap();
        let out = stats.borrow();
        let text = String::from_utf8_lossy(&out.bytes);
        assert!(!text.contains("38;5;"), "{text:?}");
        assert!(
            text.contains("[31m") || text.contains(";31m") || text.contains("[31;"),
            "{text:?}"
        );
    }

    #[test]
    fn one_write_and_one_flush_per_draw() {
        let stats = Rc::new(RefCell::new(Stats::default()));
        let viewport = Viewport::Fixed(Rect::new(0, 0, 40, 10));
        let mut screen =
            Screen::with_viewport(Counting(Rc::clone(&stats)), Some(viewport)).unwrap();
        for frame in 0..3_usize {
            screen
                .draw(|f| {
                    f.render_widget(Paragraph::new(format!("frame {frame}")), f.area());
                })
                .unwrap();
        }
        let out = stats.borrow();
        assert_eq!(out.flushes, 3, "one flush per draw");
        assert_eq!(out.writes, 3, "one write per draw");
        let text = String::from_utf8_lossy(&out.bytes);
        assert_eq!(text.matches("\x1b[?2026h").count(), 3);
        assert_eq!(text.matches("\x1b[?2026l").count(), 3);
        let first_end = text.find("\x1b[?2026l").unwrap();
        assert!(
            text[..first_end].contains("frame"),
            "frame sits inside markers"
        );
    }

    #[test]
    fn mouse_modes_track_motion_for_hover_and_undo_it() {
        for mode in ["1000", "1002", "1003", "1006"] {
            assert!(MOUSE_ON.contains(&format!("?{mode}h")));
            assert!(MOUSE_OFF.contains(&format!("?{mode}l")));
        }
    }

    /// Regression (bench report overdraw): the last frame is wiped and the
    /// cursor homed before the alternate screen is left, so text printed
    /// after `restore` never lands on the frame where no alternate screen
    /// exists.
    #[test]
    fn bench_report_overdraw_leave_screen_clears_before_leaving() {
        let mut out = Vec::new();
        leave_screen(&mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        let off = text.find(MOUSE_OFF).unwrap();
        let clear = text.find("\x1b[2J").unwrap();
        let home = text.find("\x1b[1;1H").unwrap();
        let show = text.find("\x1b[?25h").unwrap();
        let leave = text.find("\x1b[?1049l").unwrap();
        assert!(off < clear && clear < home && home < leave, "{text:?}");
        assert!(show < leave, "{text:?}");
        assert!(
            text.ends_with("\x1b[?1049l"),
            "nothing after leaving: {text:?}"
        );
    }

    #[test]
    fn restore_without_install_writes_nothing() {
        assert!(!ALT_SCREEN.load(Ordering::SeqCst));
        assert!(restore().is_ok());
        assert!(restore().is_ok());
        assert!(!RAW_MODE.load(Ordering::SeqCst));
    }

    #[test]
    fn rv_panic_hook_restores_only_for_the_owner() {
        let main = thread::current().id();
        let other = thread::spawn(|| thread::current().id()).join().unwrap();
        assert!(restores_for(main, Some(main)));
        assert!(
            !restores_for(other, Some(main)),
            "a worker never tears down"
        );
        assert!(restores_for(other, None), "no owner yet: restore anyway");
    }

    #[test]
    fn rv_guard_thread_returns_the_panic_message() {
        assert_eq!(guard_thread(|| {}), Ok(()));
        let err = guard_thread(|| std::panic::panic_any("sensor blew up")).unwrap_err();
        assert_eq!(err, "sensor blew up");
        let owned = guard_thread(|| std::panic::panic_any(format!("tick {}", 7))).unwrap_err();
        assert_eq!(owned, "tick 7");
        let odd = guard_thread(|| std::panic::panic_any(42_u8)).unwrap_err();
        assert_eq!(odd, "unknown panic");
    }
}
