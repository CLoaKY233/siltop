//! Things the user can do to one process: signal it, renice it, move it
//! to the Darwin background band.
//!
//! Every action names its target by pid *and* start time, so a pid the
//! kernel reused for another process is never touched. Failures are typed
//! (`ActionError`) so the UI can dim a verb it knows will be refused
//! instead of arming a confirm that cannot succeed.

use std::fmt;

use siltop_core::Process;

/// The signals the picker offers, in the order it lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Signal {
    Term,
    Int,
    Hup,
    Quit,
    Stop,
    Cont,
    Usr1,
    Usr2,
    Kill,
}

impl Signal {
    /// Every signal, in picker order: polite first, KILL last.
    pub const ALL: [Self; 9] = [
        Self::Term,
        Self::Int,
        Self::Hup,
        Self::Quit,
        Self::Stop,
        Self::Cont,
        Self::Usr1,
        Self::Usr2,
        Self::Kill,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Term => "TERM",
            Self::Int => "INT",
            Self::Hup => "HUP",
            Self::Quit => "QUIT",
            Self::Stop => "STOP",
            Self::Cont => "CONT",
            Self::Usr1 => "USR1",
            Self::Usr2 => "USR2",
            Self::Kill => "KILL",
        }
    }

    /// What the signal does, in words a non-Unix user understands.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Term => "ask to quit (lets it save and clean up)",
            Self::Int => "interrupt, like pressing ctrl-c",
            Self::Hup => "hang up; many daemons reload their config",
            Self::Quit => "quit and leave a crash report",
            Self::Stop => "pause the process (freeze it in place)",
            Self::Cont => "resume a paused process",
            Self::Usr1 => "app-defined signal 1",
            Self::Usr2 => "app-defined signal 2",
            Self::Kill => "force quit now (no chance to save)",
        }
    }

    /// Signals that end or freeze the process ask for confirmation;
    /// CONT and the app-defined ones do not.
    #[must_use]
    pub const fn needs_confirm(self) -> bool {
        matches!(
            self,
            Self::Term | Self::Int | Self::Quit | Self::Stop | Self::Kill | Self::Hup
        )
    }

    /// Signals whose default action ends the process. STOP freezes it and
    /// HUP often only reloads a daemon, so the process is still there.
    #[must_use]
    pub const fn ends_process(self) -> bool {
        matches!(self, Self::Term | Self::Int | Self::Quit | Self::Kill)
    }

    pub(crate) fn raw(self) -> libc::c_int {
        match self {
            Self::Term => libc::SIGTERM,
            Self::Int => libc::SIGINT,
            Self::Hup => libc::SIGHUP,
            Self::Quit => libc::SIGQUIT,
            Self::Stop => libc::SIGSTOP,
            Self::Cont => libc::SIGCONT,
            Self::Usr1 => libc::SIGUSR1,
            Self::Usr2 => libc::SIGUSR2,
            Self::Kill => libc::SIGKILL,
        }
    }
}

/// The live identity of `pid` (its current start time), or `None` when
/// no such process exists.
#[must_use]
pub fn live_target(pid: u32) -> Option<KillTarget> {
    #[cfg(target_os = "macos")]
    {
        super::macos::start_sec(pid).map(|start_unix| KillTarget { pid, start_unix })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        None
    }
}

/// Who an action is meant for: the pid plus the start time the list showed,
/// so a pid the kernel reused for another process is never signalled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KillTarget {
    pub pid: u32,
    /// `Process::start_unix` when the target was chosen.
    pub start_unix: i64,
}

impl KillTarget {
    #[must_use]
    pub const fn of(proc: &Process) -> Self {
        Self {
            pid: proc.pid,
            start_unix: proc.start_unix,
        }
    }
}

/// Why an action was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionError {
    /// The process is not ours to touch (EPERM); `uid` is its owner when
    /// the kernel said.
    NotPermitted {
        pid: u32,
        uid: Option<u32>,
    },
    /// setpriority(2) said EACCES: the process is ours, but only an admin
    /// may lower a nice value, even back toward where it started.
    PriorityNeedsAdmin {
        pid: u32,
    },
    /// The process is gone.
    Exited {
        pid: u32,
    },
    /// The pid now names a different process than the one chosen.
    Reused {
        pid: u32,
    },
    /// pid 0 or out of range.
    InvalidPid {
        pid: u32,
    },
    Unsupported {
        pid: u32,
    },
    Os {
        pid: u32,
        errno: i32,
    },
}

impl ActionError {
    #[must_use]
    pub const fn pid(&self) -> u32 {
        match *self {
            Self::NotPermitted { pid, .. }
            | Self::PriorityNeedsAdmin { pid }
            | Self::Exited { pid }
            | Self::Reused { pid }
            | Self::InvalidPid { pid }
            | Self::Unsupported { pid }
            | Self::Os { pid, .. } => pid,
        }
    }

    #[must_use]
    pub const fn is_not_permitted(&self) -> bool {
        matches!(
            self,
            Self::NotPermitted { .. } | Self::PriorityNeedsAdmin { .. }
        )
    }

    fn from_errno(pid: u32, errno: i32) -> Self {
        match errno {
            libc::EPERM | libc::EACCES => Self::NotPermitted { pid, uid: None },
            libc::ESRCH => Self::Exited { pid },
            libc::EINVAL => Self::InvalidPid { pid },
            _ => Self::Os { pid, errno },
        }
    }

    /// setpriority(2) says EACCES (not EPERM) when a non-root caller lowers
    /// the nice value of its own process: a priority rule, not an ownership
    /// one, so it gets its own message.
    fn from_nice_errno(pid: u32, errno: i32) -> Self {
        match errno {
            libc::EACCES => Self::PriorityNeedsAdmin { pid },
            _ => Self::from_errno(pid, errno),
        }
    }

    fn last_os(pid: u32) -> Self {
        let errno = std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO);
        Self::from_errno(pid, errno)
    }
}

impl fmt::Display for ActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotPermitted { pid, .. } => {
                write!(f, "process {pid}: owned by another user; needs admin")
            }
            Self::PriorityNeedsAdmin { pid } => {
                write!(f, "process {pid}: raising priority needs admin")
            }
            Self::Exited { pid } => write!(f, "process {pid}: already exited"),
            Self::Reused { pid } => write!(
                f,
                "process {pid}: pid now belongs to another process; not signalled"
            ),
            Self::InvalidPid { pid } => write!(f, "process {pid}: invalid pid"),
            Self::Unsupported { pid } => {
                write!(f, "process {pid}: not supported on this platform")
            }
            Self::Os { pid, errno } => write!(
                f,
                "process {pid}: {}",
                std::io::Error::from_raw_os_error(*errno)
            ),
        }
    }
}

impl std::error::Error for ActionError {}

impl From<ActionError> for siltop_core::Error {
    fn from(err: ActionError) -> Self {
        Self::process(err.pid(), err.to_string())
    }
}

pub type ActionResult<T = ()> = std::result::Result<T, ActionError>;

/// `Ok` only when the live start time is the one the target was chosen by.
pub(crate) fn check_identity(target: KillTarget, live_start: Option<i64>) -> ActionResult {
    match live_start {
        None => Err(ActionError::Exited { pid: target.pid }),
        Some(start) if start != target.start_unix => Err(ActionError::Reused { pid: target.pid }),
        Some(_) => Ok(()),
    }
}

fn raw_pid(pid: u32) -> ActionResult<libc::c_int> {
    match libc::c_int::try_from(pid) {
        Ok(raw) if raw > 0 => Ok(raw),
        _ => Err(ActionError::InvalidPid { pid }),
    }
}

/// Names the kernel never lets anyone stop without breaking the session.
const CRITICAL: [&str; 5] = [
    "launchd",
    "kernel_task",
    "WindowServer",
    "loginwindow",
    "logd",
];

/// Whether ending `name` (or pid 0/1) would take the whole session down.
#[must_use]
pub fn is_critical(pid: u32, name: &str) -> bool {
    pid <= 1 || CRITICAL.contains(&name)
}

/// Signals `target` after checking the pid still has the start time it was
/// chosen by (a frozen list can outlive its processes).
pub fn send_signal(target: KillTarget, signal: Signal) -> ActionResult {
    let raw = raw_pid(target.pid)?;
    verify(target)?;
    // SAFETY: kill(2) on a validated positive pid.
    let rc = unsafe { libc::kill(raw, signal.raw()) };
    if rc == 0 {
        Ok(())
    } else {
        Err(ActionError::last_os(target.pid))
    }
}

/// Probes whether `target` may be signalled without delivering anything
/// (`kill(pid, 0)`), so a verb that would fail with EPERM is never armed.
pub fn can_signal(target: KillTarget) -> ActionResult {
    let raw = raw_pid(target.pid)?;
    verify(target)?;
    // SAFETY: signal 0 only runs the permission and existence checks.
    let rc = unsafe { libc::kill(raw, 0) };
    if rc == 0 {
        Ok(())
    } else {
        Err(ActionError::last_os(target.pid))
    }
}

/// Signals every target in order; returns how many were signalled and the
/// failures, so "signal tagged (n)" can report a partial success honestly.
pub fn send_signal_all(targets: &[KillTarget], signal: Signal) -> (usize, Vec<ActionError>) {
    let mut sent = 0;
    let mut failed = Vec::new();
    for &target in targets {
        match send_signal(target, signal) {
            Ok(()) => sent += 1,
            Err(err) => failed.push(err),
        }
    }
    (sent, failed)
}

/// The pid's current nice value (-20..=20).
pub fn nice(target: KillTarget) -> ActionResult<i32> {
    let raw = raw_pid(target.pid)?;
    verify(target)?;
    clear_errno();
    // SAFETY: getpriority(2) on a validated pid; -1 is ambiguous, so errno
    // was cleared first and is checked after.
    let value = unsafe { libc::getpriority(libc::PRIO_PROCESS, raw as libc::id_t) };
    if value == -1 && errno() != 0 {
        return Err(ActionError::last_os(target.pid));
    }
    Ok(value)
}

/// Sets the pid's nice value, clamped to -20..=20. Lowering it below the
/// current value needs root; the kernel says EACCES and we say so.
pub fn set_nice(target: KillTarget, value: i32) -> ActionResult {
    let raw = raw_pid(target.pid)?;
    verify(target)?;
    let value = value.clamp(-20, 20);
    // SAFETY: setpriority(2) on a validated pid with a clamped value.
    let rc = unsafe { libc::setpriority(libc::PRIO_PROCESS, raw as libc::id_t, value) };
    if rc == 0 {
        Ok(())
    } else {
        Err(ActionError::from_nice_errno(target.pid, errno()))
    }
}

/// Moves the pid into (or out of) the Darwin background band: throttled
/// CPU and I/O, E-cores only.
pub fn set_background(target: KillTarget, on: bool) -> ActionResult {
    let raw = raw_pid(target.pid)?;
    verify(target)?;
    #[cfg(target_os = "macos")]
    {
        let prio = if on { libc::PRIO_DARWIN_BG } else { 0 };
        // SAFETY: setpriority(2) with the Darwin process-band selector on
        // a validated pid.
        let rc = unsafe { libc::setpriority(libc::PRIO_DARWIN_PROCESS, raw as libc::id_t, prio) };
        if rc == 0 {
            Ok(())
        } else {
            Err(ActionError::last_os(target.pid))
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (raw, on);
        Err(ActionError::Unsupported { pid: target.pid })
    }
}

/// `target` plus every descendant in `procs`, children after parents, so a
/// tree kill stops the parent before it can respawn what we kill.
#[must_use]
pub fn tree_targets(root: KillTarget, procs: &[Process]) -> Vec<KillTarget> {
    let mut out = vec![root];
    let mut i = 0;
    while let Some(&parent) = out.get(i) {
        for p in procs {
            if p.ppid == parent.pid && p.pid != parent.pid && !out.iter().any(|t| t.pid == p.pid) {
                out.push(KillTarget::of(p));
            }
        }
        i += 1;
    }
    out
}

fn verify(target: KillTarget) -> ActionResult {
    #[cfg(target_os = "macos")]
    {
        check_identity(target, super::macos::start_sec(target.pid))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = check_identity;
        Err(ActionError::Unsupported { pid: target.pid })
    }
}

#[cfg(target_os = "macos")]
fn errno_ptr() -> *mut libc::c_int {
    // SAFETY: __error() returns this thread's errno slot; always valid.
    unsafe { libc::__error() }
}

#[cfg(not(target_os = "macos"))]
fn errno_ptr() -> *mut libc::c_int {
    // SAFETY: __errno_location() returns this thread's errno slot.
    unsafe { libc::__errno_location() }
}

fn clear_errno() {
    // SAFETY: the pointer is this thread's errno slot.
    unsafe { *errno_ptr() = 0 };
}

fn errno() -> i32 {
    // SAFETY: the pointer is this thread's errno slot.
    unsafe { *errno_ptr() }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn target(pid: u32, start_unix: i64) -> KillTarget {
        KillTarget { pid, start_unix }
    }

    #[test]
    fn every_signal_maps_to_its_libc_number() {
        let raws: Vec<_> = Signal::ALL.iter().map(|s| s.raw()).collect();
        assert_eq!(
            raws,
            [
                libc::SIGTERM,
                libc::SIGINT,
                libc::SIGHUP,
                libc::SIGQUIT,
                libc::SIGSTOP,
                libc::SIGCONT,
                libc::SIGUSR1,
                libc::SIGUSR2,
                libc::SIGKILL
            ]
        );
    }

    #[test]
    fn every_signal_has_a_label_and_plain_description() {
        for s in Signal::ALL {
            assert!(!s.label().is_empty());
            assert!(s.describe().len() > 8, "{s:?}");
            assert!(!s.describe().contains("SIG"), "plain words: {s:?}");
        }
        assert!(Signal::Stop.describe().contains("pause"));
        assert!(Signal::Cont.describe().contains("resume"));
    }

    #[test]
    fn kill_is_listed_last_and_cont_skips_confirm() {
        assert_eq!(Signal::ALL.last(), Some(&Signal::Kill));
        assert!(Signal::Kill.needs_confirm());
        assert!(!Signal::Cont.needs_confirm());
    }

    #[test]
    fn rv_kill_checks_start_time() {
        let t = target(42, 1_700_000_000);
        assert!(check_identity(t, Some(1_700_000_000)).is_ok());
        let reused = check_identity(t, Some(1_700_000_050)).expect_err("pid reused");
        assert!(reused.to_string().contains("another process"), "{reused}");
        let gone = check_identity(t, None).expect_err("exited");
        assert_eq!(gone, ActionError::Exited { pid: 42 });
    }

    #[test]
    fn kill_target_copies_pid_and_start() {
        let proc = Process {
            pid: 7,
            start_unix: 99,
            ..Process::default()
        };
        assert_eq!(KillTarget::of(&proc), target(7, 99));
    }

    #[test]
    fn send_signal_rejects_pid_zero_and_overflow() {
        assert_eq!(
            send_signal(target(0, 0), Signal::Term),
            Err(ActionError::InvalidPid { pid: 0 })
        );
        assert_eq!(
            send_signal(target(u32::MAX, 0), Signal::Int),
            Err(ActionError::InvalidPid { pid: u32::MAX })
        );
    }

    #[test]
    fn errno_maps_to_typed_errors() {
        assert!(ActionError::from_errno(5, libc::EPERM).is_not_permitted());
        assert!(ActionError::from_errno(5, libc::EACCES).is_not_permitted());
        assert_eq!(
            ActionError::from_errno(5, libc::ESRCH),
            ActionError::Exited { pid: 5 }
        );
        assert_eq!(ActionError::from_errno(5, libc::EIO).pid(), 5);
    }

    #[test]
    fn renice_eacces_on_own_process_is_not_reported_as_another_users() {
        let own = ActionError::from_nice_errno(5, libc::EACCES);
        assert_eq!(own, ActionError::PriorityNeedsAdmin { pid: 5 });
        let text = own.to_string();
        assert!(text.contains("raising priority needs admin"), "{text}");
        assert!(!text.contains("another user"), "{text}");
        assert!(own.is_not_permitted());
        assert_eq!(
            ActionError::from_nice_errno(5, libc::EPERM),
            ActionError::NotPermitted { pid: 5, uid: None },
            "EPERM is still someone else's process"
        );
    }

    #[test]
    fn not_permitted_message_says_needs_admin() {
        let msg = ActionError::NotPermitted { pid: 88, uid: None }.to_string();
        assert!(msg.contains("needs admin"), "{msg}");
        let core: siltop_core::Error = ActionError::Exited { pid: 3 }.into();
        assert!(core.to_string().contains("exited"), "{core}");
    }

    #[test]
    fn critical_names_are_recognised() {
        for name in ["launchd", "kernel_task", "WindowServer", "loginwindow"] {
            assert!(is_critical(500, name), "{name}");
        }
        assert!(is_critical(1, "anything"));
        assert!(is_critical(0, "anything"));
        assert!(!is_critical(500, "Safari"));
    }

    fn proc(pid: u32, parent: u32) -> Process {
        Process {
            pid,
            ppid: parent,
            start_unix: i64::from(pid),
            ..Process::default()
        }
    }

    #[test]
    fn tree_targets_are_root_then_descendants_exactly() {
        let procs = [
            proc(1, 0),
            proc(10, 1),
            proc(11, 10),
            proc(12, 10),
            proc(13, 11),
            proc(20, 1),
        ];
        let tree: Vec<u32> = tree_targets(KillTarget::of(&procs[1]), &procs)
            .iter()
            .map(|t| t.pid)
            .collect();
        assert_eq!(tree, [10, 11, 12, 13], "siblings of the root stay out");
    }

    #[test]
    fn tree_targets_survive_a_ppid_cycle() {
        // A reused pid can briefly make a cycle; never loop forever.
        let procs = [proc(5, 6), proc(6, 5)];
        let tree = tree_targets(KillTarget::of(&procs[0]), &procs);
        assert_eq!(tree.len(), 2);
    }

    #[cfg(target_os = "macos")]
    mod live {
        use super::*;
        use std::process::{Child, Command};

        fn spawn_sleeper() -> (Child, KillTarget) {
            let child = Command::new("/bin/sleep").arg("30").spawn().expect("spawn");
            let pid = child.id();
            let start = super::super::super::macos::start_sec(pid).expect("listed");
            (child, target(pid, start))
        }

        fn ps_stat(pid: u32) -> String {
            let out = Command::new("/bin/ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .expect("ps");
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        }

        fn ps_nice(pid: u32) -> i32 {
            let out = Command::new("/bin/ps")
                .args(["-o", "nice=", "-p", &pid.to_string()])
                .output()
                .expect("ps");
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse()
                .expect("nice")
        }

        fn wait_for(pid: u32, want: char) -> String {
            for _ in 0..50 {
                let stat = ps_stat(pid);
                if stat.starts_with(want) {
                    return stat;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            ps_stat(pid)
        }

        #[test]
        fn stop_then_cont_pauses_and_resumes_a_child() {
            let (mut child, t) = spawn_sleeper();
            send_signal(t, Signal::Stop).expect("stop");
            let stopped = wait_for(t.pid, 'T');
            assert!(stopped.starts_with('T'), "paused: {stopped}");
            send_signal(t, Signal::Cont).expect("cont");
            let resumed = wait_for(t.pid, 'S');
            assert!(!resumed.starts_with('T'), "resumed: {resumed}");
            let _ = child.kill();
            let _ = child.wait();
        }

        #[test]
        fn renice_up_is_visible_to_ps_and_getpriority() {
            let (mut child, t) = spawn_sleeper();
            let before = nice(t).expect("nice");
            set_nice(t, before + 5).expect("renice");
            assert_eq!(nice(t).expect("nice"), before + 5);
            assert_eq!(ps_nice(t.pid), before + 5);
            let _ = child.kill();
            let _ = child.wait();
        }

        #[test]
        fn background_toggle_succeeds_then_refuses_an_exited_pid() {
            let (mut child, t) = spawn_sleeper();
            set_background(t, true).expect("bg on");
            set_background(t, false).expect("bg off");
            let _ = child.kill();
            let _ = child.wait();
            assert_eq!(
                set_background(t, true),
                Err(ActionError::Exited { pid: t.pid }),
                "a reaped child is never touched"
            );
            let _ = child.kill();
            let _ = child.wait();
        }

        #[test]
        fn tagged_term_hits_exactly_the_tagged_set() {
            let (mut a, ta) = spawn_sleeper();
            let (mut b, tb) = spawn_sleeper();
            let (mut c, tc) = spawn_sleeper();
            let (sent, failed) = send_signal_all(&[ta, tc], Signal::Term);
            assert_eq!((sent, failed.len()), (2, 0));
            let sa = a.wait().expect("a");
            let sc = c.wait().expect("c");
            assert!(!sa.success() && !sc.success());
            assert!(b.try_wait().expect("b").is_none(), "untagged b lives");
            assert!(can_signal(tb).is_ok());
            let _ = b.kill();
            let _ = b.wait();
        }

        #[test]
        fn rv_eperm_on_launchd_is_typed_not_permitted() {
            // SAFETY: geteuid(2) has no preconditions.
            if unsafe { libc::geteuid() } == 0 {
                return;
            }
            let init = live_target(1).expect("launchd listed");
            let err = can_signal(init).expect_err("launchd is root's");
            assert!(err.is_not_permitted(), "{err:?}");
        }

        #[test]
        fn rv_kill_reused_pid_is_not_signalled() {
            // Refusal must happen before kill(2): SIGINT would end the run.
            let pid = std::process::id();
            let live = super::super::super::macos::start_sec(pid).expect("own pid");
            let err = send_signal(target(pid, live - 1), Signal::Int).expect_err("stale");
            assert_eq!(err, ActionError::Reused { pid });
        }

        #[test]
        fn exited_child_is_typed_exited() {
            let (mut child, t) = spawn_sleeper();
            let _ = child.kill();
            let _ = child.wait();
            assert_eq!(
                send_signal(t, Signal::Term),
                Err(ActionError::Exited { pid: t.pid })
            );
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn send_signal_stub_reports_unsupported() {
        let err = send_signal(target(1, 0), Signal::Term).expect_err("stub");
        assert!(err.to_string().contains("not supported"), "{err}");
    }
}
