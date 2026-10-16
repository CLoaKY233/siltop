//! Stop and pause control shared by the background sampling threads (the
//! SMC sensor thread and the `IOReport` thread).
//!
//! The owner holds a [`Background`]; the thread body gets a [`Pacer`] and
//! sleeps through it. Dropping the owner stops and joins the thread. A pause
//! parks the thread on a blocking receive, so a frozen app costs no
//! `IOKit` or `IOReport` wakeups at all.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ctl {
    Pause,
    Resume,
    /// What to sample changed: wake now, but never out of a pause.
    Poke,
}

/// Why [`Pacer::wait`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wake {
    /// The period elapsed.
    Due,
    /// A pause ended; schedules should restart from now.
    Resumed,
    /// The owner is gone: return from the thread body.
    Stop,
}

/// The thread side: sleeps, honouring pause and stop.
pub(crate) struct Pacer(Receiver<Ctl>);

impl Pacer {
    /// Sleeps up to `timeout`, or through a pause.
    pub(crate) fn wait(&self, timeout: Duration) -> Wake {
        match self.0.recv_timeout(timeout) {
            Err(RecvTimeoutError::Timeout) => Wake::Due,
            Err(RecvTimeoutError::Disconnected) => Wake::Stop,
            Ok(Ctl::Resume | Ctl::Poke) => Wake::Resumed,
            Ok(Ctl::Pause) => self.parked(),
        }
    }

    fn parked(&self) -> Wake {
        loop {
            match self.0.recv() {
                Ok(Ctl::Resume) => return Wake::Resumed,
                Ok(Ctl::Pause | Ctl::Poke) => {}
                Err(_) => return Wake::Stop,
            }
        }
    }
}

/// The owner side of a background thread; stops and joins it on drop.
pub(crate) struct Background {
    ctl: Option<Sender<Ctl>>,
    join: Option<JoinHandle<()>>,
}

impl Background {
    /// Runs `body` on a new named thread; `None` if the OS refused.
    pub(crate) fn spawn<F>(name: &str, body: F) -> Option<Self>
    where
        F: FnOnce(Pacer) + Send + 'static,
    {
        let (ctl, rx) = mpsc::channel();
        let join = thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || body(Pacer(rx)))
            .ok()?;
        Some(Self {
            ctl: Some(ctl),
            join: Some(join),
        })
    }

    /// Parks (`true`) or wakes (`false`) the thread. A thread that already
    /// exited ignores it.
    pub(crate) fn set_paused(&self, paused: bool) {
        if let Some(ctl) = &self.ctl {
            let _ = ctl.send(if paused { Ctl::Pause } else { Ctl::Resume });
        }
    }
}

impl Background {
    /// Wakes a sleeping thread so it re-reads what to sample; a paused
    /// thread stays parked (the resume restarts its schedules anyway).
    pub(crate) fn poke(&self) {
        if let Some(ctl) = &self.ctl {
            let _ = ctl.send(Ctl::Poke);
        }
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        drop(self.ctl.take());
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    fn counting(period: Duration) -> (Background, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&count);
        let bg = Background::spawn("pacer-test", move |pacer| {
            loop {
                seen.fetch_add(1, Ordering::SeqCst);
                if pacer.wait(period) == Wake::Stop {
                    return;
                }
            }
        })
        .unwrap();
        (bg, count)
    }

    #[test]
    fn a_paused_thread_does_no_work() {
        let (bg, count) = counting(Duration::from_millis(2));
        thread::sleep(Duration::from_millis(30));
        bg.set_paused(true);
        thread::sleep(Duration::from_millis(20));
        let parked = count.load(Ordering::SeqCst);
        thread::sleep(Duration::from_millis(60));
        assert!(
            count.load(Ordering::SeqCst) <= parked + 1,
            "a paused thread kept sampling"
        );
        bg.set_paused(false);
        thread::sleep(Duration::from_millis(40));
        assert!(
            count.load(Ordering::SeqCst) > parked + 1,
            "resume restarts it"
        );
    }

    #[test]
    fn drop_stops_a_parked_thread_promptly() {
        let (bg, _) = counting(Duration::from_secs(60));
        bg.set_paused(true);
        let started = Instant::now();
        drop(bg);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn drop_stops_a_sleeping_thread_promptly() {
        let (bg, count) = counting(Duration::from_secs(60));
        thread::sleep(Duration::from_millis(10));
        let started = Instant::now();
        drop(bg);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn wake_reasons_are_reported() {
        let (tx, rx) = mpsc::channel();
        let pacer = Pacer(rx);
        assert_eq!(pacer.wait(Duration::from_millis(1)), Wake::Due);
        tx.send(Ctl::Resume).unwrap();
        assert_eq!(pacer.wait(Duration::from_secs(5)), Wake::Resumed);
        tx.send(Ctl::Pause).unwrap();
        tx.send(Ctl::Pause).unwrap();
        tx.send(Ctl::Resume).unwrap();
        assert_eq!(pacer.wait(Duration::from_secs(5)), Wake::Resumed);
        tx.send(Ctl::Pause).unwrap();
        drop(tx);
        assert_eq!(pacer.wait(Duration::from_secs(5)), Wake::Stop);
    }

    #[test]
    fn a_poke_wakes_a_sleeper_at_once() {
        let (tx, rx) = mpsc::channel();
        let pacer = Pacer(rx);
        tx.send(Ctl::Poke).unwrap();
        assert_eq!(pacer.wait(Duration::from_secs(60)), Wake::Resumed);
    }

    #[test]
    fn rv_a_poke_never_unpauses_a_frozen_thread() {
        let (tx, rx) = mpsc::channel();
        let pacer = Pacer(rx);
        tx.send(Ctl::Pause).unwrap();
        tx.send(Ctl::Poke).unwrap();
        drop(tx);
        // Still parked after the poke: only the owner going away ends it.
        assert_eq!(pacer.wait(Duration::from_secs(60)), Wake::Stop);
    }
}
