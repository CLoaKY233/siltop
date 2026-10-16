//! `siltop record`: sample headlessly into a `.ptrace`.
//!
//! Every frame is one whole `write` on an unbuffered file, so Ctrl-C, a
//! crash or a full disk tears at most the last frame, and `replay` keeps
//! everything before it.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use siltop_core::trace::{TRACE_EXT, TraceHeader, TraceWriter};
use siltop_core::{Error, Result, Snapshot, bytes_short};

/// What one recording run is asked to do.
#[derive(Debug, Clone, Copy)]
pub struct Plan {
    pub interval: Duration,
    /// `None` records until `stop` answers true.
    pub duration: Option<Duration>,
    pub started_unix_ms: u64,
}

/// Where `record` writes without `--out`: the working directory.
pub fn default_out(unix_s: u64) -> PathBuf {
    PathBuf::from(format!("siltop-{unix_s}.{TRACE_EXT}"))
}

/// Opens `path` for a new trace, refusing to overwrite an earlier one.
/// Only its owner may read it: a trace shows what this user was running.
/// A failure reads as one plain line naming the path.
pub fn create(path: &Path) -> Result<std::fs::File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(path)
        .map_err(|err| Error::from(io::Error::new(err.kind(), create_error(path, &err))))
}

fn create_error(path: &Path, err: &io::Error) -> String {
    let shown = path.display();
    match err.kind() {
        io::ErrorKind::AlreadyExists => format!("{shown} already exists; pass a different --out"),
        io::ErrorKind::NotFound => {
            let folder = path
                .parent()
                .filter(|dir| !dir.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            format!("no such folder: {}", folder.display())
        }
        _ => format!("{shown}: {}", crate::io_reason(err)),
    }
}

/// Primes rates (that first tick is never recorded, as `--json`), then
/// records one frame per interval until the duration or `stop`.
pub fn record<T, S, W>(
    tick: &mut T,
    wait: &mut S,
    stop: &dyn Fn() -> bool,
    out: W,
    plan: Plan,
) -> Result<TraceWriter<W>>
where
    T: FnMut() -> Result<Snapshot>,
    S: FnMut(Duration),
    W: Write,
{
    let prime = tick()?;
    let header = TraceHeader {
        interval: plan.interval,
        started_unix_ms: plan.started_unix_ms,
        note: format!("{} · siltop {}", prime.soc.name, crate::cli::VERSION),
    };
    let mut writer = TraceWriter::new(out, &header)?;
    let mut first: Option<Instant> = None;
    while !stop() {
        wait(plan.interval);
        // A stop during the wait records nothing more.
        if stop() {
            break;
        }
        let snapshot = tick()?;
        let start = *first.get_or_insert(snapshot.collected_at);
        let offset = snapshot.collected_at.saturating_duration_since(start);
        if plan.duration.is_some_and(|limit| offset > limit) {
            break;
        }
        writer.push(offset, &snapshot)?;
    }
    writer.flush()?;
    Ok(writer)
}

/// The line printed when a recording ends.
pub fn summary<W: Write>(writer: &TraceWriter<W>, path: &Path) -> String {
    format!(
        "recorded {} frames ({}) to {}; play it with `siltop replay {}`",
        writer.frames(),
        bytes_short(writer.bytes()),
        path.display(),
        path.display()
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use siltop_core::trace::read_trace;
    use std::cell::Cell;

    /// A fake sampler whose clock advances one interval per tick.
    fn fake(interval: Duration) -> impl FnMut() -> Result<Snapshot> {
        let t0 = Instant::now();
        let mut n = 0_u32;
        move || {
            let mut s = Snapshot::empty();
            s.soc.name = String::from("Apple M5 Pro");
            s.collected_at = t0 + interval * n;
            s.cpu.scaled = n as f32 / 100.0;
            n += 1;
            Ok(s)
        }
    }

    fn plan(duration: Option<Duration>) -> Plan {
        Plan {
            interval: Duration::from_secs(1),
            duration,
            started_unix_ms: 42,
        }
    }

    #[test]
    fn records_frames_until_the_duration_then_replays_them() {
        let mut tick = fake(Duration::from_secs(1));
        let mut waited = Duration::ZERO;
        let mut wait = |d: Duration| waited += d;
        let never = || false;
        let writer = record(
            &mut tick,
            &mut wait,
            &never,
            Vec::new(),
            plan(Some(Duration::from_secs(4))),
        )
        .unwrap();
        assert_eq!(writer.frames(), 5, "offsets 0..=4 s");
        let trace = read_trace(&writer.into_inner(), Instant::now()).unwrap();
        assert_eq!(trace.header.started_unix_ms, 42);
        assert!(trace.header.note.starts_with("Apple M5 Pro · siltop "));
        assert_eq!(trace.span(), Duration::from_secs(4));
        // The priming tick (cpu 0) is never recorded.
        assert!((trace.frame(0).unwrap().snapshot.cpu.scaled - 0.01).abs() < 1e-6);
        assert_eq!(waited, Duration::from_secs(6));
    }

    /// Regression: Ctrl-C arriving mid-wait still recorded one more frame.
    #[test]
    fn stop_during_a_wait_ends_the_recording_without_another_frame() {
        let mut tick = fake(Duration::from_secs(1));
        let waits = Cell::new(0);
        let mut wait = |_| waits.set(waits.get() + 1);
        // Ctrl-C lands during the fourth wait.
        let stop = || waits.get() > 3;
        let writer = record(&mut tick, &mut wait, &stop, Vec::new(), plan(None)).unwrap();
        assert_eq!(writer.frames(), 3);
    }

    #[test]
    fn a_sampler_error_stops_the_recording_with_that_error() {
        let mut n = 0;
        let mut tick = || {
            n += 1;
            if n > 2 {
                Err(Error::system("sensor thread died"))
            } else {
                Ok(Snapshot::empty())
            }
        };
        let err = record(&mut tick, &mut |_| {}, &|| false, Vec::new(), plan(None))
            .err()
            .unwrap();
        assert!(err.to_string().contains("sensor thread died"));
    }

    #[test]
    fn create_never_overwrites_an_earlier_trace() {
        let path = std::env::temp_dir().join(format!("siltop-rec-{}.ptrace", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert!(create(&path).is_ok());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "traces are private to their owner");
        }
        let err = create(&path).unwrap_err().to_string();
        assert!(err.contains("exists"), "{err}");
        let _ = std::fs::remove_file(&path);
        assert_eq!(default_out(7), PathBuf::from("siltop-7.ptrace"));
    }

    #[test]
    fn summary_names_the_file_and_how_to_play_it() {
        let mut w = TraceWriter::new(
            Vec::new(),
            &TraceHeader {
                interval: Duration::from_secs(1),
                started_unix_ms: 0,
                note: String::new(),
            },
        )
        .unwrap();
        w.push(Duration::ZERO, &Snapshot::empty()).unwrap();
        let line = summary(&w, Path::new("a.ptrace"));
        assert!(line.starts_with("recorded 1 frames ("), "{line}");
        assert!(line.ends_with("`siltop replay a.ptrace`"));
    }
}
