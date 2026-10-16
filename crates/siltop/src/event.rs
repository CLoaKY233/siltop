use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
    MouseEventKind,
};
use siltop_core::{Error, Result};

use crate::keymap::{self, Action, Mode, Profile};
use crate::msg::AppMsg;

/// Forwards terminal events to the main loop. Pointer motion, key releases
/// and late colour-query replies never reach the channel, so they cannot wake
/// a redraw.
pub fn spawn_input(tx: Sender<AppMsg>) -> Result<()> {
    thread::Builder::new()
        .name(String::from("siltop-input"))
        .spawn(move || {
            if let Err(why) = crate::tui::guard_thread(|| read_loop(&tx)) {
                let _ = tx.send(AppMsg::InputClosed(format!("input thread: {why}")));
            }
        })
        .map(|_| ())
        .map_err(|err| Error::system(format!("spawn input thread: {err}")))
}

fn read_loop(tx: &Sender<AppMsg>) {
    let mut swallow = OscSwallow::new(Instant::now());
    loop {
        match event::read() {
            Ok(ev) if !wakes(&ev) || swallow.eats(&ev, Instant::now()) => {}
            Ok(ev) => {
                if tx.send(AppMsg::Input(ev)).is_err() {
                    break;
                }
            }
            Err(err) => {
                let _ = tx.send(AppMsg::InputClosed(err.to_string()));
                break;
            }
        }
    }
}

/// How long after startup a colour-query reply may still straggle in.
const LATE_REPLY_WINDOW: Duration = Duration::from_secs(3);
/// No real reply is longer than this many characters.
const MAX_REPLY_KEYS: u8 = 64;

/// Swallows an OSC colour reply that arrived after the startup probe gave up.
/// crossterm reads `ESC ] 11;rgb:… ESC \\` as Alt+`]`, plain characters, then
/// Alt+`\\` (or Ctrl+G for a BEL terminator); none of that may reach the
/// keymap. Only armed shortly after startup, so a deliberate Alt+`]` later on
/// still works.
#[derive(Debug)]
pub struct OscSwallow {
    armed_until: Instant,
    /// Characters eaten so far inside a reply, `None` outside one.
    inside: Option<u8>,
}

impl OscSwallow {
    pub fn new(start: Instant) -> Self {
        Self {
            armed_until: start + LATE_REPLY_WINDOW,
            inside: None,
        }
    }

    /// True when `ev` is part of a late reply and must be dropped.
    pub fn eats(&mut self, ev: &TermEvent, now: Instant) -> bool {
        let TermEvent::Key(key) = ev else {
            self.inside = None;
            return false;
        };
        match self.inside {
            None if now < self.armed_until && is_alt(key, ']') => {
                self.inside = Some(0);
                true
            }
            None => false,
            Some(_) if is_alt(key, '\\') || is_bel(key) => {
                self.inside = None;
                true
            }
            Some(eaten) if matches!(key.code, KeyCode::Char(_)) && eaten < MAX_REPLY_KEYS => {
                self.inside = Some(eaten + 1);
                true
            }
            Some(_) => {
                self.inside = None;
                false
            }
        }
    }
}

fn is_alt(key: &KeyEvent, c: char) -> bool {
    key.code == KeyCode::Char(c) && key.modifiers.contains(KeyModifiers::ALT)
}

fn is_bel(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('g') && key.modifiers.contains(KeyModifiers::CONTROL)
}

pub fn wakes(ev: &TermEvent) -> bool {
    match ev {
        // Mouse motion wakes the loop too, so hover can light a button;
        // the app draws only when the target under the pointer changes.
        TermEvent::Key(key) => key.kind != KeyEventKind::Release,
        _ => true,
    }
}

/// crossterm → [`Action`]: keys go through the keymap for `mode`, the
/// pointer and resizes map one to one.
pub fn map_event(ev: &TermEvent, mode: Mode, profile: Profile) -> Option<Action> {
    match ev {
        TermEvent::Resize(cols, rows) => Some(Action::Resize {
            cols: *cols,
            rows: *rows,
        }),
        TermEvent::Key(key) if key.kind != KeyEventKind::Release => {
            keymap::resolve(*key, mode, profile)
        }
        TermEvent::Mouse(mouse) => map_mouse(mouse.kind, mouse.column, mouse.row),
        _ => None,
    }
}

fn map_mouse(kind: MouseEventKind, col: u16, row: u16) -> Option<Action> {
    match kind {
        MouseEventKind::Down(MouseButton::Right) => Some(Action::ContextMenu { col, row }),
        MouseEventKind::Down(_) => Some(Action::Click { col, row }),
        MouseEventKind::Drag(_) => Some(Action::Drag { col, row }),
        MouseEventKind::Up(_) => Some(Action::MouseUp),
        MouseEventKind::Moved => Some(Action::Hover { col, row }),
        MouseEventKind::ScrollDown => Some(Action::Scroll { col, row, delta: 1 }),
        MouseEventKind::ScrollUp => Some(Action::Scroll {
            col,
            row,
            delta: -1,
        }),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// How crossterm tokenises `bytes`: ESC x → Alt+x, 0x07 → Ctrl+G.
    fn keys_of(bytes: &str) -> Vec<TermEvent> {
        let mut out = Vec::new();
        let mut chars = bytes.chars();
        while let Some(c) = chars.next() {
            let key = match c {
                '\u{1b}' => KeyEvent::new(KeyCode::Char(chars.next().unwrap()), KeyModifiers::ALT),
                '\u{7}' => KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
                c => KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            };
            out.push(TermEvent::Key(key));
        }
        out
    }

    fn passed(swallow: &mut OscSwallow, bytes: &str, now: Instant) -> String {
        keys_of(bytes)
            .iter()
            .filter(|ev| !swallow.eats(ev, now))
            .filter_map(|ev| match ev {
                TermEvent::Key(key) => match key.code {
                    KeyCode::Char(c) => Some(c),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    #[test]
    fn late_osc_reply_is_swallowed_st_and_bel() {
        let t0 = Instant::now();
        let mut swallow = OscSwallow::new(t0);
        let st = "\u{1b}]11;rgb:1414/1313/1313\u{1b}\\q";
        assert_eq!(
            passed(&mut swallow, st, t0),
            "q",
            "only the real key survives"
        );
        let bel = "\u{1b}]4;2;rgb:8a8a/9a9a/7b7b\u{7}j";
        assert_eq!(passed(&mut swallow, bel, t0), "j");
    }

    #[test]
    fn alt_bracket_after_the_window_is_a_real_key() {
        let t0 = Instant::now();
        let mut swallow = OscSwallow::new(t0);
        let later = t0 + LATE_REPLY_WINDOW + Duration::from_millis(1);
        assert_eq!(passed(&mut swallow, "\u{1b}]ab", later), "]ab");
    }

    #[test]
    fn runaway_reply_gives_keys_back() {
        let t0 = Instant::now();
        let mut swallow = OscSwallow::new(t0);
        let long = format!("\u{1b}]{}", "x".repeat(usize::from(MAX_REPLY_KEYS) + 3));
        assert_eq!(passed(&mut swallow, &long, t0), "xxx");
    }
    use ratatui::crossterm::event::{
        KeyCode, KeyEvent, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
    };

    const P: Profile = Profile::Siltop;

    fn mouse(kind: MouseEventKind) -> TermEvent {
        TermEvent::Mouse(MouseEvent {
            kind,
            column: 5,
            row: 7,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[test]
    fn keys_go_through_the_keymap() {
        let q = TermEvent::Key(KeyEvent::from(KeyCode::Char('q')));
        assert_eq!(map_event(&q, Mode::Normal, P), Some(Action::Quit));
        assert_eq!(
            map_event(&q, Mode::Search, P),
            Some(Action::FilterChar('q'))
        );
        assert_eq!(map_event(&q, Mode::Help, P), Some(Action::Close));
    }

    #[test]
    fn pointer_motion_maps_to_hover() {
        let moved = mouse(MouseEventKind::Moved);
        assert!(wakes(&moved));
        assert!(matches!(
            map_event(&moved, Mode::Normal, P),
            Some(Action::Hover { .. })
        ));
        assert!(wakes(&mouse(MouseEventKind::Down(MouseButton::Left))));
        assert!(wakes(&mouse(MouseEventKind::Drag(MouseButton::Left))));
        let release = TermEvent::Key(KeyEvent {
            code: KeyCode::Char('j'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
        });
        assert!(!wakes(&release));
        assert_eq!(map_event(&release, Mode::Normal, P), None);
    }

    #[test]
    fn mouse_maps_the_same_in_every_mode() {
        for mode in Mode::ALL {
            assert_eq!(
                map_event(&mouse(MouseEventKind::Down(MouseButton::Left)), mode, P),
                Some(Action::Click { col: 5, row: 7 })
            );
            assert_eq!(
                map_event(&mouse(MouseEventKind::Up(MouseButton::Left)), mode, P),
                Some(Action::MouseUp)
            );
        }
    }

    #[test]
    fn wheel_carries_the_pointer_position() {
        assert_eq!(
            map_event(&mouse(MouseEventKind::ScrollDown), Mode::Normal, P),
            Some(Action::Scroll {
                col: 5,
                row: 7,
                delta: 1
            })
        );
        assert_eq!(
            map_event(&mouse(MouseEventKind::ScrollUp), Mode::Normal, P),
            Some(Action::Scroll {
                col: 5,
                row: 7,
                delta: -1
            })
        );
    }

    #[test]
    fn resize_reports_the_new_size() {
        assert_eq!(
            map_event(&TermEvent::Resize(120, 40), Mode::Normal, P),
            Some(Action::Resize {
                cols: 120,
                rows: 40
            })
        );
    }
}
