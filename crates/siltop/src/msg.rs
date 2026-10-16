use std::time::{Duration, Instant};

use ratatui::crossterm::event::Event as TermEvent;
use siltop_core::{Error, Result, Snapshot};

/// Everything that can wake the main loop arrives on one channel.
#[derive(Debug)]
pub enum AppMsg {
    Input(TermEvent),
    InputClosed(String),
    Snapshot(Box<Result<Snapshot>>),
    /// The sampler thread is gone for good (it failed to start or
    /// panicked); no snapshot follows.
    Stopped(Error),
    /// The sampler entered (`true`) or left low-power mode, so the tick
    /// runs at `tick_interval(requested, low)` from now on.
    LowPower(bool),
    /// The loop's own deadline passed (toast expiry, frame pacing).
    Timer,
    /// SIGTSTP from outside (`kill -TSTP`): restore the terminal and stop.
    Suspend,
    /// SIGCONT: back from a stop, so the screen needs a full redraw.
    Continued,
    /// A newer release. Shown as a toast and a footer badge; it does not interrupt.
    Update {
        version: String,
        url: String,
        sha256: String,
        notes: String,
        brew: bool,
    },
    /// A quiet or confirmed install finished.
    UpdateInstalled {
        version: String,
    },
    /// An install stopped. The old binary is still the one on disk.
    UpdateFailed {
        message: String,
    },
}

/// Hold the first frame until data arrives, so the layout does not jump
/// from "no gpu" to "gpu" a few milliseconds after launch.
pub const FIRST_FRAME_HOLD: Duration = Duration::from_millis(500);
/// Caps drag and key-repeat redraws at 60 per second.
pub const MIN_FRAME_GAP: Duration = Duration::from_millis(16);

/// Decides when the loop draws: only when something changed, never before
/// the first data (or the hold) and never faster than `MIN_FRAME_GAP`.
#[derive(Debug, Clone, Copy)]
pub struct Pacer {
    dirty: bool,
    hold_until: Option<Instant>,
    last_draw: Option<Instant>,
}

impl Pacer {
    pub fn new(start: Instant) -> Self {
        Self {
            dirty: true,
            hold_until: Some(start + FIRST_FRAME_HOLD),
            last_draw: None,
        }
    }

    pub fn mark(&mut self) {
        self.dirty = true;
    }

    pub fn release(&mut self) {
        self.hold_until = None;
    }

    pub fn due(&self, now: Instant) -> bool {
        self.dirty && self.ready_at().is_none_or(|at| now >= at)
    }

    pub fn deadline(&self) -> Option<Instant> {
        if self.dirty { self.ready_at() } else { None }
    }

    pub fn drawn(&mut self, now: Instant) {
        self.dirty = false;
        self.hold_until = None;
        self.last_draw = Some(now);
    }

    fn ready_at(&self) -> Option<Instant> {
        let paced = self.last_draw.map(|at| at + MIN_FRAME_GAP);
        match (self.hold_until, paced) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }
}
