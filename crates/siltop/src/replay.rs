//! `siltop replay <FILE>`: a recorded trace played through the full TUI.
//!
//! The [`Player`] is pure: it maps wall time onto trace time at the chosen
//! speed and hands the app whichever frames are due. `p` pauses (through
//! the app's own freeze), `+`/`-` step the speed and `←`/`→` scrub by a
//! twentieth of the recording. A trace can travel with a bug report and
//! anyone can open it.

use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use siltop_core::trace::{TRACE_MAGIC, Trace, TraceError, read_trace};
use siltop_core::{Error, Result, Snapshot, bytes_short};

use crate::cli::{INTERVAL_MAX, INTERVAL_MIN};

/// Playback rates `+` and `-` step through.
pub const SPEEDS: [f32; 7] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0];
const NORMAL_SPEED: usize = 2;
/// A scrub moves this share of the recording, and never less than a frame.
const SCRUB_PARTS: u32 = 20;
/// Cells in the text scrub bar.
const BAR_CELLS: usize = 12;
/// Largest file `replay` opens (days of recording at the default rate);
/// a read trace costs about its size in memory.
const MAX_TRACE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// A replay error: the file and what is wrong with it, with no category
/// prefix, since the user named the file and only it matters.
fn file_error(path: &Path, err: &dyn std::fmt::Display) -> Error {
    let shown = siltop_core::into_printable(path.display().to_string());
    Error::Io(std::io::Error::other(format!("{shown}: {err}")))
}

/// Opens `path` as a replay would, without reading the whole recording:
/// run before the terminal is touched, so a wrong path is one plain line
/// even without a terminal.
pub fn check(path: &Path) -> Result<()> {
    let failed = |err: &dyn std::fmt::Display| file_error(path, err);
    let file = std::fs::File::open(path).map_err(|err| failed(&crate::io_reason(&err)))?;
    if file.metadata().is_ok_and(|meta| meta.is_dir()) {
        return Err(failed(&"is a directory, not a recording"));
    }
    read_capped(file, TRACE_MAGIC.len() as u64)
        .map(drop)
        .or_else(|err| {
            // The cap is the magic itself, so only a bad start is an error.
            if err.starts_with("larger than") {
                Ok(())
            } else {
                Err(failed(&err))
            }
        })
}

/// Reads and checks a trace file; a torn tail is kept and reported.
pub fn load(path: &Path, epoch: Instant) -> Result<(Trace, Option<String>)> {
    let failed = |err: &dyn std::fmt::Display| file_error(path, err);
    let file = std::fs::File::open(path).map_err(|err| failed(&crate::io_reason(&err)))?;
    let bytes = read_capped(file, MAX_TRACE_BYTES).map_err(|err| failed(&err))?;
    let trace = read_trace(&bytes, epoch).map_err(|err| failed(&err))?;
    if trace.is_empty() {
        return Err(failed(&"the recording has no frames"));
    }
    let warning = trace.truncated.then(|| {
        format!(
            "the recording ends early (damaged tail); playing {} frames",
            trace.len()
        )
    });
    Ok((trace, warning))
}

/// All of `reader`, stopping early at a stream that does not open like a
/// trace (`/dev/zero`) or runs past `limit` bytes.
fn read_capped(mut reader: impl Read, limit: u64) -> std::result::Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let magic = TRACE_MAGIC.len() as u64;
    (&mut reader)
        .take(magic)
        .read_to_end(&mut bytes)
        .map_err(|err| err.to_string())?;
    if bytes != TRACE_MAGIC {
        return Err(TraceError::NotATrace.to_string());
    }
    reader
        .take(limit.saturating_sub(magic).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|err| err.to_string())?;
    if bytes.len() as u64 > limit {
        return Err(format!(
            "larger than {}, too big to replay",
            bytes_short(limit)
        ));
    }
    Ok(bytes)
}

/// The interval `trace` was recorded at, held to the range `record`
/// accepts: a crafted header must not reach the session or a scrub.
pub fn interval(trace: &Trace) -> Duration {
    trace.header.interval.clamp(INTERVAL_MIN, INTERVAL_MAX)
}

/// Plays frames against the wall clock, decoding each as it falls due.
#[derive(Debug)]
pub struct Player {
    trace: Trace,
    /// The recording's interval, held to the range `record` accepts.
    interval: Duration,
    /// The next frame to hand out.
    cursor: usize,
    speed: usize,
    paused: bool,
    /// Wall instant at which trace time `anchor.1` played.
    anchor: (Instant, Duration),
}

impl Player {
    pub fn new(trace: Trace, now: Instant) -> Self {
        Self {
            interval: interval(&trace),
            trace,
            cursor: 0,
            speed: NORMAL_SPEED,
            paused: false,
            anchor: (now, Duration::ZERO),
        }
    }

    pub const fn speed(&self) -> f32 {
        SPEEDS[self.speed]
    }

    #[cfg(test)]
    pub const fn paused(&self) -> bool {
        self.paused
    }

    pub fn finished(&self) -> bool {
        self.cursor >= self.trace.len()
    }

    fn span(&self) -> Duration {
        self.trace.span()
    }

    /// Trace time playing at `now`, held while paused and at the end.
    pub fn position(&self, now: Instant) -> Duration {
        if self.paused {
            return self.anchor.1;
        }
        let wall = now.saturating_duration_since(self.anchor.0);
        self.anchor
            .1
            .saturating_add(wall.mul_f32(self.speed()))
            .min(self.span())
    }

    /// When the next frame falls due; `None` while paused or finished.
    pub fn deadline(&self) -> Option<Instant> {
        if self.paused {
            return None;
        }
        let next = self.trace.offset(self.cursor)?;
        let ahead = next.saturating_sub(self.anchor.1);
        // Offsets stay within `MAX_SPAN`, so this sum never fails.
        self.anchor.0.checked_add(ahead.div_f32(self.speed()))
    }

    /// Every frame due by `now`, in order, stamped `now`.
    pub fn due(&mut self, now: Instant) -> Vec<Snapshot> {
        let mut out = Vec::new();
        while self.deadline().is_some_and(|at| at <= now) {
            if let Some(frame) = self.trace.frame(self.cursor) {
                let mut snapshot = frame.snapshot;
                snapshot.collected_at = now;
                out.push(snapshot);
            }
            self.cursor += 1;
        }
        out
    }

    pub fn set_paused(&mut self, paused: bool, now: Instant) {
        if paused != self.paused {
            self.anchor = (now, self.position(now));
            self.paused = paused;
        }
    }

    /// One step along [`SPEEDS`]; playback continues from where it is.
    pub fn step_speed(&mut self, faster: bool, now: Instant) {
        self.anchor = (now, self.position(now));
        self.speed = if faster {
            (self.speed + 1).min(SPEEDS.len() - 1)
        } else {
            self.speed.saturating_sub(1)
        };
    }

    /// Jumps `parts` twentieths of the recording (negative is back) and
    /// returns the frame now showing, so a paused view moves too.
    pub fn scrub(&mut self, parts: i32, now: Instant) -> Option<Snapshot> {
        let step = (self.span() / SCRUB_PARTS).max(self.interval);
        let here = self.position(now);
        let distance = step.saturating_mul(parts.unsigned_abs());
        let target = if parts < 0 {
            here.saturating_sub(distance)
        } else {
            here.saturating_add(distance).min(self.span())
        };
        let index = self.trace.index_at(target);
        let frame = self.trace.frame(index)?;
        self.anchor = (now, frame.offset);
        self.cursor = index + 1;
        let mut snapshot = frame.snapshot;
        snapshot.collected_at = now;
        Some(snapshot)
    }

    /// `▶ 2× ━━━━━━●───────── 01:23 / 10:00`, for the footer.
    pub fn status(&self, now: Instant) -> String {
        let span = self.span();
        let here = self.position(now);
        let state = if self.finished() {
            "■"
        } else if self.paused {
            "❚❚"
        } else {
            "▶"
        };
        let filled = if span.is_zero() {
            BAR_CELLS
        } else {
            ((here.as_secs_f64() / span.as_secs_f64()) * BAR_CELLS as f64).round() as usize
        }
        .min(BAR_CELLS);
        let bar: String = (0..=BAR_CELLS)
            .map(|cell| match cell.cmp(&filled) {
                std::cmp::Ordering::Less => '━',
                std::cmp::Ordering::Equal => '●',
                std::cmp::Ordering::Greater => '─',
            })
            .collect();
        format!(
            "replay {state} {} {bar} {} / {}",
            speed_label(self.speed()),
            clock(here),
            clock(span)
        )
    }
}

fn speed_label(speed: f32) -> String {
    if speed >= 1.0 {
        format!("{speed:.0}×")
    } else {
        format!("{speed}×")
    }
}

/// `mm:ss`, or `h:mm:ss` past an hour.
fn clock(at: Duration) -> String {
    let secs = at.as_secs();
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{:02}:{:02}", secs / 60, secs % 60)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use siltop_core::trace::{Frame, TraceHeader};

    const SEC: Duration = Duration::from_secs(1);

    /// Ten frames a second apart; frame `i` has `cpu.scaled == i / 10`.
    fn player(now: Instant) -> Player {
        let frames = (0..10_u32).map(|i| {
            let mut snapshot = Snapshot::empty();
            snapshot.cpu.scaled = i as f32 / 10.0;
            Frame {
                offset: SEC * i,
                snapshot,
            }
        });
        let header = TraceHeader {
            interval: SEC,
            started_unix_ms: 0,
            note: String::new(),
        };
        Player::new(Trace::from_frames(&header, frames, now).unwrap(), now)
    }

    fn cpus(snaps: &[Snapshot]) -> Vec<u32> {
        snaps
            .iter()
            .map(|s| (s.cpu.scaled * 10.0).round() as u32)
            .collect()
    }

    #[test]
    fn frames_fall_due_on_the_recorded_timing() {
        let t0 = Instant::now();
        let mut p = player(t0);
        assert_eq!(cpus(&p.due(t0)), [0]);
        assert_eq!(p.deadline(), Some(t0 + SEC));
        assert!(p.due(t0 + SEC / 2).is_empty());
        assert_eq!(cpus(&p.due(t0 + SEC * 3)), [1, 2, 3]);
        let late = p.due(t0 + SEC * 60);
        assert_eq!(late.len(), 6);
        assert!(late.iter().all(|s| s.collected_at == t0 + SEC * 60));
        assert!(p.finished());
        assert_eq!(p.deadline(), None, "nothing left to wake for");
    }

    #[test]
    fn pause_holds_position_and_resume_continues_from_it() {
        let t0 = Instant::now();
        let mut p = player(t0);
        p.due(t0 + SEC * 2);
        p.set_paused(true, t0 + SEC * 2 + SEC / 2);
        assert_eq!(p.deadline(), None);
        assert!(p.due(t0 + SEC * 30).is_empty());
        assert_eq!(p.position(t0 + SEC * 30), SEC * 2 + SEC / 2);
        p.set_paused(false, t0 + SEC * 30);
        assert_eq!(p.deadline(), Some(t0 + SEC * 30 + SEC / 2));
        assert_eq!(cpus(&p.due(t0 + SEC * 31)), [3]);
    }

    #[test]
    fn speed_steps_are_clamped_and_scale_the_clock() {
        let t0 = Instant::now();
        let mut p = player(t0);
        p.due(t0);
        p.step_speed(true, t0);
        assert!((p.speed() - 2.0).abs() < f32::EPSILON);
        assert_eq!(p.deadline(), Some(t0 + SEC / 2));
        for _ in 0..20 {
            p.step_speed(true, t0);
        }
        assert!((p.speed() - 16.0).abs() < f32::EPSILON);
        for _ in 0..20 {
            p.step_speed(false, t0);
        }
        assert!((p.speed() - 0.25).abs() < f32::EPSILON);
        assert_eq!(p.deadline(), Some(t0 + SEC * 4));
    }

    #[test]
    fn scrub_jumps_both_ways_and_clamps_to_the_ends() {
        let t0 = Instant::now();
        let mut p = player(t0);
        p.due(t0 + SEC * 5);
        // 9 s span / 20 is under the 1 s interval, so a step is 1 s.
        let back = p.scrub(-2, t0 + SEC * 5).unwrap();
        assert_eq!(cpus(&[back]), [3]);
        assert_eq!(cpus(&p.due(t0 + SEC * 6)), [4]);
        assert_eq!(cpus(&[p.scrub(-100, t0 + SEC * 6).unwrap()]), [0]);
        assert_eq!(cpus(&[p.scrub(100, t0 + SEC * 6).unwrap()]), [9]);
        assert!(p.finished());
    }

    /// A crafted header interval used to overflow `Duration` on the
    /// first scrub; it is held to the range `record` accepts.
    #[test]
    fn a_crafted_interval_cannot_overflow_a_scrub() {
        let t0 = Instant::now();
        let header = TraceHeader {
            interval: Duration::MAX,
            started_unix_ms: 0,
            note: String::new(),
        };
        let frames = (0..3).map(|i| Frame {
            offset: SEC * i,
            snapshot: Snapshot::empty(),
        });
        let mut p = Player::new(Trace::from_frames(&header, frames, t0).unwrap(), t0);
        assert_eq!(p.interval, INTERVAL_MAX);
        assert_eq!(interval(&p.trace), INTERVAL_MAX);
        assert!(p.scrub(i32::MAX, t0).is_some());
        assert!(p.scrub(i32::MIN, t0).is_some());
        assert_eq!(p.position(t0), Duration::ZERO);
    }

    #[test]
    fn scrub_while_paused_shows_the_frame_and_stays_paused() {
        let t0 = Instant::now();
        let mut p = player(t0);
        p.due(t0);
        p.set_paused(true, t0);
        let shown = p.scrub(3, t0 + SEC).unwrap();
        assert_eq!(cpus(&[shown]), [3]);
        assert!(p.paused());
        assert_eq!(p.position(t0 + SEC * 10), SEC * 3);
        assert!(p.due(t0 + SEC * 10).is_empty());
    }

    #[test]
    fn status_line_shows_state_speed_bar_and_clock() {
        let t0 = Instant::now();
        let mut p = player(t0);
        assert_eq!(p.status(t0), "replay ▶ 1× ●──────────── 00:00 / 00:09");
        p.due(t0 + SEC * 9);
        assert!(p.status(t0 + SEC * 9).starts_with("replay ■ 1× ━━━━"));
        assert!(p.status(t0 + SEC * 9).ends_with("● 00:09 / 00:09"));
        let mut p = player(t0);
        p.step_speed(false, t0);
        p.set_paused(true, t0);
        assert!(p.status(t0).starts_with("replay ❚❚ 0.5× "));
        assert_eq!(clock(Duration::from_secs(3723)), "1:02:03");
    }

    /// `replay /dev/zero` (or a huge crafted file) used to read without
    /// bound; foreign streams stop at the magic and big ones at the cap.
    #[test]
    fn reads_stop_at_foreign_magic_and_at_the_size_cap() {
        let zero = read_capped(std::io::repeat(0), MAX_TRACE_BYTES).unwrap_err();
        assert_eq!(zero, "not a siltop trace");
        let endless = TRACE_MAGIC.chain(std::io::repeat(0));
        let big = read_capped(endless, 4096).unwrap_err();
        assert_eq!(big, "larger than 4K, too big to replay");
        let fits = TRACE_MAGIC.chain(&[1_u8; 8][..]);
        assert_eq!(read_capped(fits, 16).unwrap().len(), 16);
    }

    #[test]
    fn load_reports_missing_and_foreign_files() {
        let dir = std::env::temp_dir().join(format!("siltop-replay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let missing = load(&dir.join("nope.ptrace"), Instant::now()).unwrap_err();
        assert!(missing.to_string().contains("nope.ptrace"));
        let foreign = dir.join("foreign.ptrace");
        std::fs::write(&foreign, b"not a trace").unwrap();
        let err = load(&foreign, Instant::now()).unwrap_err();
        assert!(err.to_string().contains("not a siltop trace"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
