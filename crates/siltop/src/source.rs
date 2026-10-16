use std::collections::VecDeque;
use std::time::{Duration, Instant};

use siltop_core::Snapshot;

use crate::replay::Player;
use crate::worker::{Cmd, Handle};

/// Where snapshots come from: the sampler thread, a recorded trace, or a
/// script that lets the app run without hardware.
pub enum SnapshotSource {
    Live(Handle),
    /// `siltop replay`: frames fall due on the main loop's own clock.
    Replay(Box<Player>),
    #[cfg_attr(not(test), allow(dead_code))]
    Scripted(VecDeque<Snapshot>),
}

impl SnapshotSource {
    pub fn set_interval(&self, interval: Duration) {
        if let Self::Live(handle) = self {
            handle.send(Cmd::Interval(interval));
        }
    }

    /// The battery saver setting; recorded sources ignore it.
    pub fn set_low_power(&self, mode: siltop_core::LowPower) {
        if let Self::Live(handle) = self {
            handle.send(Cmd::LowPower(mode));
        }
    }

    /// What the visible views read; recorded sources ignore it.
    pub fn set_needs(&self, needs: siltop_metrics::Needs) {
        if let Self::Live(handle) = self {
            handle.send(Cmd::Needs(needs));
        }
    }

    pub fn set_paused(&mut self, paused: bool, now: Instant) {
        match self {
            Self::Live(handle) => handle.send(Cmd::Paused(paused)),
            Self::Replay(player) => player.set_paused(paused, now),
            Self::Scripted(_) => {}
        }
    }

    pub fn player_mut(&mut self) -> Option<&mut Player> {
        match self {
            Self::Replay(player) => Some(player),
            _ => None,
        }
    }

    pub fn player(&self) -> Option<&Player> {
        match self {
            Self::Replay(player) => Some(player),
            _ => None,
        }
    }

    /// When a pulled source next has something; the loop wakes for it.
    pub fn deadline(&self) -> Option<Instant> {
        self.player().and_then(Player::deadline)
    }

    /// Frames a pulled source has due by `now`; live sources push instead.
    pub fn due(&mut self, now: Instant) -> Vec<Snapshot> {
        self.player_mut().map_or_else(Vec::new, |p| p.due(now))
    }

    #[cfg(test)]
    pub fn next_scripted(&mut self) -> Option<Snapshot> {
        match self {
            Self::Live(_) | Self::Replay(_) => None,
            Self::Scripted(queue) => queue.pop_front(),
        }
    }
}
