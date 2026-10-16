//! HUP, TERM and INT end the loop cleanly, so the terminal is restored and
//! a pending config save is flushed. A second signal while the first is
//! still being handled exits at once (the loop may be wedged).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use signal_hook::consts::{SIGCONT, SIGHUP, SIGINT, SIGTERM, SIGTSTP};
use signal_hook::iterator::Signals;
use siltop_core::{Error, Result};

use crate::msg::AppMsg;

pub const SHUTDOWN_SIGNALS: [i32; 3] = [SIGHUP, SIGTERM, SIGINT];

/// Set once a shutdown signal has arrived.
#[derive(Debug, Clone, Default)]
pub struct Shutdown(Arc<AtomicBool>);

impl Shutdown {
    pub fn requested(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    fn request(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Installs the handlers; each signal sets the flag and wakes the loop.
pub fn install(tx: Sender<AppMsg>) -> Result<Shutdown> {
    let shutdown = Shutdown::default();
    for signal in SHUTDOWN_SIGNALS {
        signal_hook::flag::register_conditional_shutdown(signal, 1, Arc::clone(&shutdown.0))
            .map_err(|err| Error::system(format!("signal handler: {err}")))?;
    }
    let mut signals = Signals::new(SHUTDOWN_SIGNALS)
        .map_err(|err| Error::system(format!("signal handler: {err}")))?;
    let flag = shutdown.clone();
    std::thread::Builder::new()
        .name(String::from("siltop-signals"))
        .spawn(move || {
            for _ in signals.forever() {
                flag.request();
                if tx.send(AppMsg::Timer).is_err() {
                    break;
                }
            }
        })
        .map_err(|err| Error::system(format!("signal thread: {err}")))?;
    Ok(shutdown)
}

/// Job control for the dashboard: SIGTSTP asks the loop to put the
/// terminal back and stop, SIGCONT to take the screen again and redraw it
/// whole (the shell drew over it meanwhile).
pub fn install_job_control(tx: Sender<AppMsg>) -> Result<()> {
    let mut signals = Signals::new([SIGTSTP, SIGCONT])
        .map_err(|err| Error::system(format!("signal handler: {err}")))?;
    std::thread::Builder::new()
        .name(String::from("siltop-job-control"))
        .spawn(move || {
            for signal in signals.forever() {
                let msg = if signal == SIGTSTP {
                    AppMsg::Suspend
                } else {
                    AppMsg::Continued
                };
                if tx.send(msg).is_err() {
                    break;
                }
            }
        })
        .map(|_| ())
        .map_err(|err| Error::system(format!("signal thread: {err}")))
}

/// Stops this process the way Ctrl-Z would (SIGSTOP cannot be caught, so
/// our own SIGTSTP handler is not in the way); returns once a SIGCONT
/// (`fg`) wakes it.
pub fn stop_self() {
    // SAFETY: raise(3) only delivers a signal to this process.
    unsafe {
        libc::raise(libc::SIGSTOP);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::time::Duration;

    #[test]
    fn covers_hangup_terminate_and_interrupt() {
        assert!(SHUTDOWN_SIGNALS.contains(&libc::SIGHUP));
        assert!(SHUTDOWN_SIGNALS.contains(&libc::SIGTERM));
        assert!(SHUTDOWN_SIGNALS.contains(&libc::SIGINT));
    }

    #[test]
    fn fresh_flag_is_clear_and_request_sticks() {
        let flag = Shutdown::default();
        assert!(!flag.requested());
        let copy = flag.clone();
        copy.request();
        assert!(flag.requested(), "clones share one flag");
    }

    /// Raises a real SIGHUP at this process and checks the loop is woken.
    /// SIGHUP is only ever raised here, so it cannot hit another test.
    #[test]
    fn hup_sets_the_flag_and_wakes_the_loop() {
        let (tx, rx) = channel();
        let shutdown = install(tx).unwrap();
        assert!(!shutdown.requested());
        // SAFETY: raise(3) only delivers a signal to this process; the
        // handler installed above turns it into a flag and a message.
        let rc = unsafe { libc::raise(libc::SIGHUP) };
        assert_eq!(rc, 0);
        let msg = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(msg, AppMsg::Timer));
        assert!(shutdown.requested());
    }
}
