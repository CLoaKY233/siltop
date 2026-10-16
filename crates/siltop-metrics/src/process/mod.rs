//! Process table: one `KERN_PROC_ALL` sweep plus two `proc_pid*` calls
//! per pid (task info for threads, rusage v6 for CPU time, footprint,
//! energy, disk and wakeups).
//!
//! Numbers the kernel withholds (other users' processes without root) are
//! `None` with `restricted` set, never a made-up zero (D23).

mod actions;
mod argv;
mod gpu_clients;
mod inspect;
mod kinfo;
mod rusage;

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use siltop_core::{CollectorId, GpuSnapshot, Process, Result, Snapshot, into_printable};

use crate::cadence::{LOW_POWER_PROCS, Pace, PowerMode, stretched};
use crate::collector::{Collector, Outcome};
use crate::ioreport::GPU_IDLE_ACTIVE;

pub use actions::{
    ActionError, ActionResult, KillTarget, Signal, can_signal, is_critical, live_target, nice,
    send_signal, send_signal_all, set_background, set_nice, tree_targets,
};
pub use inspect::inspect;

/// The process table is refreshed at most this often.
pub(crate) const PROCS_EVERY: Duration = Duration::from_secs(1);

/// A pid the kernel refused every call for (another user's process without
/// root) is asked again this often; its row is restricted either way.
pub(crate) const DENIED_REREAD: Duration = Duration::from_secs(10);

const SPARK_LEN: usize = 32;

/// What is remembered about one pid between samples.
#[derive(Debug, Clone, Default)]
struct CachedProc {
    start_us: i64,
    /// `p_comm` at the last sample: a change within one run means `exec()`.
    comm: String,
    name: String,
    command: Option<String>,
    args: Vec<String>,
    cpu_ns: Option<u64>,
    usage: Option<rusage::Usage>,
    threads: Option<u32>,
    spark: VecDeque<f32>,
    /// The kernel readings last asked for, and when.
    last: Readings,
    read_at: Option<Instant>,
}

/// Machine CPU busy share averaged over the process table's own window:
/// every tick since the last refresh. The unattributed remainder then
/// compares like with like, not one 500 ms tick of machine CPU against a
/// second of per-pid CPU.
#[derive(Debug, Default)]
struct BusyWindow {
    seen: Option<Instant>,
    busy_secs: f64,
    secs: f64,
}

impl BusyWindow {
    /// Adds the tick ending at `now`, whose machine busy share was `active`.
    fn add(&mut self, now: Instant, active: f32) {
        if let Some(seen) = self.seen {
            let dt = now.saturating_duration_since(seen).as_secs_f64();
            self.busy_secs += f64::from(active.clamp(0.0, 1.0)) * dt;
            self.secs += dt;
        }
        self.seen = Some(now);
    }

    /// The average since the last take (`None` before a whole tick), and
    /// a fresh window from here.
    fn take(&mut self) -> Option<f32> {
        let avg = (self.secs > 0.0).then(|| (self.busy_secs / self.secs) as f32);
        self.busy_secs = 0.0;
        self.secs = 0.0;
        avg
    }
}

pub(crate) struct ProcessCollector {
    cache: HashMap<u32, CachedProc>,
    names: HashMap<u32, String>,
    last: Option<Instant>,
    ncpu: u32,
    refreshed: Pace,
    baseline_only: bool,
    gpu: gpu_clients::GpuClients,
    mode: PowerMode,
    /// The GPU was idle at the last snapshot: the client walk can wait.
    gpu_idle: bool,
    busy: BusyWindow,
}

impl ProcessCollector {
    pub(crate) fn new() -> Self {
        Self {
            cache: HashMap::new(),
            names: HashMap::new(),
            last: None,
            ncpu: logical_cpus(),
            refreshed: Pace::default(),
            baseline_only: false,
            gpu: gpu_clients::GpuClients::default(),
            mode: PowerMode::default(),
            gpu_idle: false,
            busy: BusyWindow::default(),
        }
    }

    /// Follows the sampler's low-power flag.
    pub(crate) fn with_power(mut self, mode: PowerMode) -> Self {
        self.mode = mode;
        self
    }

    fn user_name(&mut self, uid: u32) -> String {
        self.names
            .entry(uid)
            .or_insert_with(|| lookup_uid(uid))
            .clone()
    }

    pub(crate) fn sample(&mut self) -> Result<Vec<Process>> {
        #[cfg(target_os = "macos")]
        {
            macos::sample(self)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(Vec::new())
        }
    }
}

impl Collector for ProcessCollector {
    fn id(&self) -> CollectorId {
        CollectorId::Processes
    }

    /// Refreshes the table every [`PROCS_EVERY`], and right after a
    /// baseline-only pass so real CPU shares follow at once.
    fn collect(&mut self, now: Instant, snap: &mut Snapshot) -> Result<Outcome> {
        snap.sampled.procs = false;
        if cfg!(not(target_os = "macos")) {
            return Ok(Outcome::Absent);
        }
        self.busy.add(now, snap.cpu.active);
        let every = procs_every(self.mode.low());
        let due = self.refreshed.due(now, every) || self.baseline_only;
        if !due {
            return Ok(Outcome::Fresh);
        }
        let baseline = self.last.is_none();
        self.gpu_idle = gpu_idle(snap.gpu.as_ref());
        snap.processes = self.sample()?;
        snap.process_ncpu = self.ncpu;
        let active = self.busy.take().unwrap_or(snap.cpu.active);
        snap.unattributed_cpu =
            (!baseline).then(|| unattributed_cpu(active, self.ncpu, &snap.processes));
        snap.sampled.procs = true;
        self.refreshed.ran(now);
        self.baseline_only = baseline;
        Ok(if baseline {
            Outcome::Baseline
        } else {
            Outcome::Fresh
        })
    }
}

/// The process-table refresh period, stretched on battery.
pub(crate) fn procs_every(low_power: bool) -> Duration {
    stretched(PROCS_EVERY, LOW_POWER_PROCS, low_power)
}

/// Every GPU signal says idle (GPUPH residency and the `IOAccelerator`
/// renderer share): per-process GPU shares are all near zero, so the
/// registry walk behind them can run at its idle cadence.
pub(crate) fn gpu_idle(gpu: Option<&GpuSnapshot>) -> bool {
    gpu.is_some_and(|g| {
        g.active < GPU_IDLE_ACTIVE && g.renderer_busy.unwrap_or(0.0) < GPU_IDLE_ACTIVE
    })
}

fn lookup_uid(uid: u32) -> String {
    #[cfg(target_os = "macos")]
    {
        macos::lookup_uid(uid)
    }
    #[cfg(not(target_os = "macos"))]
    {
        uid.to_string()
    }
}

fn logical_cpus() -> u32 {
    #[cfg(target_os = "macos")]
    {
        crate::sys::logical_cpus()
    }
    #[cfg(not(target_os = "macos"))]
    {
        1
    }
}

/// Per-core CPU percent: 100 is one core fully busy over the interval
/// (C04), so a process on four cores reads 400.
pub(crate) fn cpu_percent(delta_ns: u64, interval_ns: u128) -> Option<f32> {
    if interval_ns == 0 {
        return None;
    }
    Some(((delta_ns as f64) / (interval_ns as f64) * 100.0) as f32)
}

/// CPU of a pid seen for the first time (D25): its whole CPU time over its
/// whole life, so a `rustc` that started 300 ms ago and burned 290 ms reads
/// ~97 rather than a baseline zero. Capped at every core busy.
pub(crate) fn first_sample_cpu(cpu_ns: u64, age_us: i64, ncpu: u32) -> Option<f32> {
    let window = u128::try_from(age_us).ok()?.checked_mul(1_000)?;
    // Younger than 1 ms: the start stamp and the counters are too coarse.
    if window < 1_000_000 {
        return None;
    }
    let pct = cpu_percent(cpu_ns, window)?;
    Some(pct.min(100.0 * ncpu.max(1) as f32))
}

/// Machine CPU the readable rows do not explain, in per-core percent.
pub(crate) fn unattributed_cpu(active_ratio: f32, ncpu: u32, procs: &[Process]) -> f32 {
    let total = active_ratio.clamp(0.0, 1.0) * 100.0 * ncpu as f32;
    let seen: f32 = procs.iter().filter_map(|p| p.cpu).sum();
    (total - seen).max(0.0)
}

/// The thread count from the previous sample when it cannot have changed:
/// same run, no `exec()`, and not one tick of CPU time since. A thread is
/// only created or ended by a thread that runs, so a process whose CPU time
/// stood still (most of the ~800 on a Mac at any moment) keeps its count,
/// and its `PROC_PIDTASKINFO` call (~1–2 µs each) is skipped.
fn unchanged_threads(prev: Option<&CachedProc>, info: &kinfo::Kinfo, cpu_ns: u64) -> Option<u32> {
    let prev = prev.filter(|c| same_program(c, info))?;
    prev.threads.filter(|_| prev.cpu_ns == Some(cpu_ns))
}

/// `prev` still describes this pid: the same run (no pid reuse) and the
/// same program (no `exec()`).
fn same_program(prev: &CachedProc, info: &kinfo::Kinfo) -> bool {
    prev.start_us == info.start_us && prev.comm == info.comm
}

/// The readings to reuse instead of asking the kernel: a fully denied pid
/// inside [`DENIED_REREAD`] of its last read. `None` means read now. A
/// readable pid is always read: one that sat idle can peg a core at any
/// moment, and holding its old readings showed that as 0% for seconds.
/// [`unchanged_threads`] already skips the costlier task-info call.
/// Ages are taken give or take the refresh's wake `jitter`.
fn held_readings(
    prev: Option<&CachedProc>,
    info: &kinfo::Kinfo,
    now: Instant,
    jitter: Duration,
) -> Option<Readings> {
    let prev = prev.filter(|c| same_program(c, info) && c.last.fully_denied())?;
    let age = now
        .saturating_duration_since(prev.read_at?)
        .saturating_add(jitter);
    (age < DENIED_REREAD).then_some(prev.last)
}

/// The window a pid's counter deltas span: since its own last read, which
/// is longer than the sample interval after a hold. 0 (the baseline pass)
/// stays 0.
fn pid_interval(sample_ns: u128, read_at: Option<Instant>, now: Instant) -> u128 {
    match read_at {
        Some(at) if sample_ns > 0 => now.saturating_duration_since(at).as_nanos(),
        _ => sample_ns,
    }
}

/// Whether the identity (name, command, argv) must be resolved again.
/// A new run (start time changed) or an `exec()` (same run, new `comm`,
/// D14) invalidates it; CPU-time continuity survives an `exec()`.
fn needs_identity(prev: Option<&CachedProc>, start_us: i64, comm: &str) -> bool {
    prev.is_none_or(|c| c.start_us != start_us || c.comm != comm || c.name.is_empty())
}

pub(crate) fn preferred_name(pid: u32, argv: &[String], path: Option<&str>, comm: &str) -> String {
    if let Some(name) = argv::display_name(argv) {
        return name;
    }
    for candidate in [argv.first().map(String::as_str), path]
        .into_iter()
        .flatten()
    {
        let base = argv::basename(candidate);
        if !base.is_empty() {
            return base.to_owned();
        }
    }
    if !comm.is_empty() {
        return comm.to_owned();
    }
    format!("pid {pid}")
}

/// Per-pid readings for one sample, as the kernel returned them.
#[derive(Debug, Clone, Copy, Default)]
struct Readings {
    pub threads: Option<u32>,
    pub task_cpu_ns: Option<u64>,
    pub task_resident: Option<u64>,
    pub usage: Option<rusage::Usage>,
    /// Either call failed with `EPERM`.
    pub denied: bool,
}

impl Readings {
    /// The kernel refused everything: there is nothing to read again soon.
    fn fully_denied(&self) -> bool {
        self.denied && self.usage.is_none() && self.task_cpu_ns.is_none() && self.threads.is_none()
    }
}

/// The sample-wide inputs to [`build_row`].
#[derive(Debug, Clone, Copy)]
struct Tick {
    /// Since the previous sample; 0 on the baseline pass.
    interval_ns: u128,
    /// Wall clock, for the age of first-seen processes.
    now_us: i64,
    ncpu: u32,
}

/// Builds one row and the cache entry for the next sample. Pure: every
/// kernel reading comes in through `info`, `ident` and `read`.
fn build_row(
    info: &kinfo::Kinfo,
    prev: Option<CachedProc>,
    ident: Option<(String, Option<String>, Vec<String>)>,
    read: Readings,
    tick: Tick,
    user: String,
) -> (Process, CachedProc) {
    let same_run = prev.as_ref().is_some_and(|c| c.start_us == info.start_us);
    let prev = prev.filter(|_| same_run);
    let (name, command, args) = match ident {
        Some((name, command, args)) => (
            into_printable(name),
            command.map(into_printable),
            args.into_iter().map(into_printable).collect(),
        ),
        None => prev
            .as_ref()
            .map(|c| (c.name.clone(), c.command.clone(), c.args.clone()))
            .unwrap_or_default(),
    };

    let cpu_ns = read.usage.map(|u| u.cpu_ns).or(read.task_cpu_ns);
    let prev_ns = prev.as_ref().and_then(|c| c.cpu_ns);
    let cpu = match (cpu_ns, prev_ns) {
        (Some(total), Some(was)) => cpu_percent(total.saturating_sub(was), tick.interval_ns),
        (Some(total), None) if tick.interval_ns > 0 => {
            first_sample_cpu(total, tick.now_us.saturating_sub(info.start_us), tick.ncpu)
        }
        _ => None,
    };
    let rates = match (prev.as_ref().and_then(|c| c.usage), read.usage) {
        (Some(was), Some(now)) => rusage::rates(&was, &now, tick.interval_ns),
        _ => None,
    };

    let mut spark = prev.map(|c| c.spark).unwrap_or_default();
    if let Some(cpu) = cpu {
        spark.push_back(cpu);
        while spark.len() > SPARK_LEN {
            spark.pop_front();
        }
    }

    let row = Process {
        pid: info.pid,
        ppid: info.ppid,
        name: name.clone(),
        cpu,
        mem_bytes: read.usage.map(|u| u.footprint),
        threads: read.threads,
        gpu: 0.0,
        user: into_printable(user),
        uid: info.uid,
        command: command.clone(),
        args: args.clone(),
        status: kinfo::state_word(info.stat),
        start_unix: info.start_sec(),
        cpu_spark: spark.iter().copied().collect(),
        restricted: read.denied,
        rss_bytes: read.usage.map(|u| u.resident).or(read.task_resident),
        energy_w: rates.map(|r| r.energy_w),
        disk_read_bps: rates.map(|r| r.disk_read_bps),
        disk_write_bps: rates.map(|r| r.disk_write_bps),
        wakeups_per_s: rates.map(|r| r.wakeups_per_s),
        child_cpu_ns: read.usage.map(|u| u.child_cpu_ns),
    };
    let cached = CachedProc {
        start_us: info.start_us,
        comm: info.comm.clone(),
        name,
        command,
        args,
        cpu_ns,
        usage: read.usage,
        threads: read.threads,
        spark,
        last: read,
        read_at: None,
    };
    (row, cached)
}

#[cfg(target_os = "macos")]
mod macos {
    use super::argv::{self, Procargs};
    use super::kinfo::{self, KINFO_PROC_LEN, Kinfo};
    use super::rusage::{self, RusageInfoV6, Usage};
    use super::{
        CachedProc, ProcessCollector, Readings, Tick, build_row, held_readings, needs_identity,
        pid_interval, unchanged_threads,
    };
    use siltop_core::{Error, Process, Result};
    use std::collections::HashSet;
    use std::ffi::CStr;
    use std::mem::{MaybeUninit, size_of};
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    /// A kernel call's result with its `errno`.
    type Os<T> = std::result::Result<T, i32>;

    fn code<T>(r: &Os<T>) -> Option<i32> {
        r.as_ref().err().copied()
    }

    pub(super) fn sample(collector: &mut ProcessCollector) -> Result<Vec<Process>> {
        let now = Instant::now();
        let interval_ns = collector
            .last
            .map_or(0, |prev| now.saturating_duration_since(prev).as_nanos());
        collector.last = Some(now);
        let now_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX));

        let jitter = collector.refreshed.slack();
        if collector.gpu.due(now, collector.gpu_idle) {
            collector.gpu.update(now, super::gpu_clients::read_totals());
        }
        let infos = kinfo_all()?;
        let live: HashSet<u32> = infos.iter().map(|k| k.pid).collect();
        collector.cache.retain(|pid, _| live.contains(pid));

        let tick = Tick {
            interval_ns,
            now_us,
            ncpu: collector.ncpu.max(1),
        };
        let mut out = Vec::with_capacity(infos.len());
        for info in &infos {
            let prev = collector.cache.remove(&info.pid);
            let held = held_readings(prev.as_ref(), info, now, jitter);
            let read_at = prev.as_ref().and_then(|c| c.read_at);
            let read = match held {
                Some(read) => read,
                None => match readings(info, prev.as_ref()) {
                    Some(read) => read,
                    // Exited between the sweep and the per-pid calls.
                    None => continue,
                },
            };
            let ident = needs_identity(prev.as_ref(), info.start_us, &info.comm)
                .then(|| resolve_identity(info));
            let user = collector.user_name(info.uid);
            let row_tick = Tick {
                interval_ns: pid_interval(tick.interval_ns, read_at, now),
                ..tick
            };
            let (mut row, mut cached) = build_row(info, prev, ident, read, row_tick, user);
            cached.read_at = if held.is_some() { read_at } else { Some(now) };
            row.gpu = collector.gpu.share(info.pid);
            collector.cache.insert(info.pid, cached);
            out.push(row);
        }
        Ok(out)
    }

    /// `None` when the process is gone (`ESRCH` from both calls). The task
    /// info call is skipped when rusage shows the thread count cannot have
    /// changed since `prev`.
    fn readings(info: &Kinfo, prev: Option<&CachedProc>) -> Option<Readings> {
        let pid = info.pid;
        let usage = rusage(pid);
        if let Ok(u) = &usage
            && let Some(threads) = unchanged_threads(prev, info, u.cpu_ns)
        {
            return Some(Readings {
                threads: Some(threads),
                usage: Some(*u),
                ..Readings::default()
            });
        }
        let task = task_info(pid);
        if code(&task) == Some(libc::ESRCH) && code(&usage) == Some(libc::ESRCH) {
            return None;
        }
        let task_ok = task.as_ref().ok();
        Some(Readings {
            threads: task_ok.map(|t| u32::try_from(t.pti_threadnum.max(0)).unwrap_or(0)),
            task_cpu_ns: task_ok.map(|t| {
                crate::sys::mach_ticks_to_ns(t.pti_total_user.saturating_add(t.pti_total_system))
            }),
            task_resident: task_ok.map(|t| t.pti_resident_size),
            usage: usage.as_ref().ok().copied(),
            denied: code(&task) == Some(libc::EPERM) || code(&usage) == Some(libc::EPERM),
        })
    }

    pub(super) fn kinfo_all() -> Result<Vec<Kinfo>> {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_ALL, 0];
        let mut buf = Vec::new();
        for _ in 0..4 {
            let mut needed = 0usize;
            let probe = unsafe {
                // SAFETY: size probe; oldp is null.
                libc::sysctl(
                    mib.as_mut_ptr(),
                    mib.len() as libc::c_uint,
                    std::ptr::null_mut(),
                    &raw mut needed,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if probe != 0 {
                return Err(Error::system(format!(
                    "KERN_PROC_ALL size: {}",
                    std::io::Error::last_os_error()
                )));
            }
            let cap = needed
                .saturating_add(needed / 4)
                .saturating_add(KINFO_PROC_LEN);
            buf.resize(cap, 0);
            let mut got = buf.len();
            let rc = unsafe {
                // SAFETY: `buf` is writable for `got` bytes.
                libc::sysctl(
                    mib.as_mut_ptr(),
                    mib.len() as libc::c_uint,
                    buf.as_mut_ptr().cast(),
                    &raw mut got,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if rc == 0 {
                buf.truncate(got);
                return Ok(kinfo::parse_all(&buf));
            }
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::ENOMEM) {
                return Err(Error::system(format!("KERN_PROC_ALL: {err}")));
            }
        }
        Err(Error::system("KERN_PROC_ALL: table grew too fast"))
    }

    fn resolve_identity(info: &Kinfo) -> (String, Option<String>, Vec<String>) {
        let args = procargs(info.pid).unwrap_or_default();
        let path = pid_path(info.pid)
            .or_else(|| Some(args.exec_path.clone()).filter(|p| p.starts_with('/')));
        let command = path.clone().or_else(|| args.argv.first().cloned());
        let name = super::preferred_name(info.pid, &args.argv, path.as_deref(), &info.comm);
        (name, command, args.argv)
    }

    pub(super) fn lookup_uid(uid: u32) -> String {
        const BUF_LEN: usize = 2048;
        let mut pwd = MaybeUninit::<libc::passwd>::uninit();
        let mut buf = [0_u8; BUF_LEN];
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = unsafe {
            // SAFETY: pwd/buf are writable locals; the kernel fills `result`.
            libc::getpwuid_r(
                uid,
                pwd.as_mut_ptr(),
                buf.as_mut_ptr().cast(),
                BUF_LEN,
                &raw mut result,
            )
        };
        if rc == 0 && !result.is_null() {
            let pwd = unsafe {
                // SAFETY: getpwuid_r wrote a full passwd on success.
                pwd.assume_init()
            };
            let name = unsafe {
                // SAFETY: on success `pw_name` points at a NUL-terminated
                // string inside `buf`, which is still alive here.
                CStr::from_ptr(pwd.pw_name)
            }
            .to_string_lossy()
            .into_owned();
            if !name.is_empty() {
                return name;
            }
        }
        uid.to_string()
    }

    /// Full `KERN_PROCARGS2` buffer, parsed; argv is capped at 4 KiB.
    pub(super) fn procargs(pid: u32) -> Option<Procargs> {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
        let mut needed = 0usize;
        let probe = unsafe {
            // SAFETY: size probe; oldp is null.
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as libc::c_uint,
                std::ptr::null_mut(),
                &raw mut needed,
                std::ptr::null_mut(),
                0,
            )
        };
        if probe != 0 || needed < 8 {
            return None;
        }
        let mut buf = vec![0_u8; needed];
        let mut got = buf.len();
        let rc = unsafe {
            // SAFETY: `buf` is writable for `got` bytes.
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as libc::c_uint,
                buf.as_mut_ptr().cast(),
                &raw mut got,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || got < 8 {
            return None;
        }
        buf.truncate(got);
        argv::parse_procargs(&buf)
    }

    fn pid_path(pid: u32) -> Option<String> {
        let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let n = unsafe {
            // SAFETY: `buf` is a writable path buffer of the documented max size.
            libc::proc_pidpath(
                pid as libc::c_int,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
            )
        };
        if n <= 0 {
            return None;
        }
        let cstr = CStr::from_bytes_until_nul(&buf).ok()?;
        cstr.to_str().ok().map(str::to_owned)
    }

    fn errno() -> i32 {
        std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO)
    }

    fn task_info(pid: u32) -> Os<libc::proc_taskinfo> {
        let mut info = MaybeUninit::<libc::proc_taskinfo>::zeroed();
        let want = size_of::<libc::proc_taskinfo>() as libc::c_int;
        let n = unsafe {
            // SAFETY: PROC_PIDTASKINFO writes a proc_taskinfo into `info`.
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTASKINFO,
                0,
                info.as_mut_ptr().cast(),
                want,
            )
        };
        if n != want {
            return Err(errno());
        }
        Ok(unsafe {
            // SAFETY: kernel wrote a full proc_taskinfo.
            info.assume_init()
        })
    }

    pub(super) fn rusage(pid: u32) -> Os<Usage> {
        let mut info = MaybeUninit::<RusageInfoV6>::zeroed();
        let rc = unsafe {
            // SAFETY: RUSAGE_INFO_V6 writes one rusage_info_v6, which
            // `RusageInfoV6` mirrors (size asserted in tests).
            libc::proc_pid_rusage(
                pid as libc::c_int,
                rusage::RUSAGE_INFO_V6,
                info.as_mut_ptr().cast(),
            )
        };
        if rc != 0 {
            return Err(errno());
        }
        let raw = unsafe {
            // SAFETY: zero-initialised plain integers, filled by the kernel.
            info.assume_init()
        };
        Ok(Usage::from_raw(&raw, crate::sys::mach_ticks_to_ns))
    }

    /// The pid's start time in whole seconds, from one `KERN_PROC_PID`
    /// record; `None` when it no longer exists.
    pub(super) fn start_sec(pid: u32) -> Option<i64> {
        let raw = i32::try_from(pid).ok()?;
        let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, raw];
        let mut buf = [0u8; KINFO_PROC_LEN];
        let mut got = buf.len();
        let rc = unsafe {
            // SAFETY: `buf` is writable for `got` bytes; `mib` is a valid
            // four-element name for this sysctl.
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as libc::c_uint,
                buf.as_mut_ptr().cast(),
                &raw mut got,
                std::ptr::null_mut(),
                0,
            )
        };
        // A missing pid succeeds with zero bytes.
        (rc == 0 && got == KINFO_PROC_LEN)
            .then(|| kinfo::parse_one(&buf))
            .flatten()
            .filter(|info| info.pid == pid)
            .map(|info| info.start_sec())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::kinfo::{Kinfo, SRUN, SSTOP, SZOMB};
    use super::rusage::Usage;
    use super::*;

    const SEC: u128 = 1_000_000_000;

    #[test]
    fn battery_refreshes_the_table_every_four_seconds() {
        assert_eq!(procs_every(false), PROCS_EVERY);
        assert_eq!(procs_every(true), LOW_POWER_PROCS);
    }

    #[test]
    fn unattributed_cpu_averages_every_tick_since_the_last_refresh() {
        // 500 ms ticks at 100% then 0% busy, table refreshed after both:
        // the table's window was half busy, not idle as the last tick says.
        let t0 = Instant::now();
        let mut busy = BusyWindow::default();
        busy.add(t0, 0.3);
        busy.add(t0 + Duration::from_millis(500), 1.0);
        busy.add(t0 + Duration::from_millis(1000), 0.0);
        let active = busy.take().unwrap();
        assert!((active - 0.5).abs() < 1e-6, "{active}");
        assert!((unattributed_cpu(active, 4, &[]) - 200.0).abs() < 1e-3);
        assert_eq!(busy.take(), None, "the window restarts");
        busy.add(t0 + Duration::from_millis(1500), 0.25);
        assert!((busy.take().unwrap() - 0.25).abs() < 1e-6);
    }

    #[test]
    fn gpu_idle_needs_every_signal_quiet() {
        let quiet = GpuSnapshot {
            active: 0.0,
            renderer_busy: Some(0.01),
            ..GpuSnapshot::default()
        };
        assert!(gpu_idle(Some(&quiet)));
        let rendering = GpuSnapshot {
            renderer_busy: Some(0.40),
            ..quiet
        };
        assert!(!gpu_idle(Some(&rendering)), "IOAccelerator says busy");
        let clocked = GpuSnapshot {
            active: 0.30,
            ..quiet
        };
        assert!(!gpu_idle(Some(&clocked)), "GPUPH says busy");
        assert!(!gpu_idle(None), "no GPU reading: keep the normal walk");
    }

    #[test]
    fn a_low_power_collector_skips_refreshes_inside_three_seconds() {
        let mode = PowerMode::default();
        let mut collector = ProcessCollector::new().with_power(mode.clone());
        let t0 = Instant::now();
        collector.refreshed.ran(t0);
        let mut snap = Snapshot::empty();
        let at = t0 + Duration::from_millis(1500);
        mode.set(true);
        collector.collect(at, &mut snap).unwrap();
        assert!(!snap.sampled.procs, "on battery 1.5 s is not due");
        mode.set(false);
        if cfg!(target_os = "macos") {
            let _gate = crate::sampler::io_gate();
            collector.collect(at, &mut snap).unwrap();
            assert!(snap.sampled.procs, "on AC 1.5 s is due");
        }
    }

    fn info(pid: u32, start_us: i64, comm: &str) -> Kinfo {
        Kinfo {
            pid,
            ppid: 1,
            uid: 501,
            start_us,
            stat: SRUN,
            comm: comm.to_owned(),
        }
    }

    fn ident(name: &str) -> (String, Option<String>, Vec<String>) {
        (
            name.to_owned(),
            Some(format!("/bin/{name}")),
            vec![name.to_owned()],
        )
    }

    fn readable(cpu_ns: u64) -> Readings {
        Readings {
            threads: Some(4),
            task_cpu_ns: Some(cpu_ns),
            task_resident: Some(10 << 20),
            usage: Some(Usage {
                cpu_ns,
                footprint: 3 << 20,
                resident: 10 << 20,
                ..Usage::default()
            }),
            denied: false,
        }
    }

    fn denied() -> Readings {
        Readings {
            denied: true,
            ..Readings::default()
        }
    }

    /// One step of the per-pid state machine.
    fn step(
        k: &Kinfo,
        prev: Option<CachedProc>,
        read: Readings,
        interval: u128,
        now_us: i64,
    ) -> (Process, CachedProc) {
        let ident = needs_identity(prev.as_ref(), k.start_us, &k.comm).then(|| ident(&k.comm));
        let tick = Tick {
            interval_ns: interval,
            now_us,
            ncpu: 18,
        };
        build_row(k, prev, ident, read, tick, String::from("u"))
    }

    /// A process can name itself (or its argv) with terminal escapes; the
    /// row must carry only printable text so no output path can replay them.
    #[test]
    fn escape_sequences_in_identity_are_made_printable() {
        let evil = "x\u{1b}]2;RVBC\u{7}y";
        let tick = Tick {
            interval_ns: 1_000_000_000,
            now_us: 2_000_000,
            ncpu: 18,
        };
        let ident = Some((
            evil.to_owned(),
            Some(format!("/tmp/{evil}")),
            vec![evil.to_owned(), String::from("--flag\u{9b}2J")],
        ));
        let (row, cached) = build_row(
            &info(42, 100, "x"),
            None,
            ident,
            readable(0),
            tick,
            String::from("u\u{1b}"),
        );
        let fields = [
            row.name.as_str(),
            row.command.as_deref().unwrap_or(""),
            row.user.as_str(),
        ];
        for text in fields
            .iter()
            .copied()
            .chain(row.args.iter().map(String::as_str))
        {
            assert!(!text.chars().any(siltop_core::is_unsafe), "{text:?}");
        }
        assert_eq!(row.name, "x\u{FFFD}]2;RVBC\u{FFFD}y");
        assert_eq!(cached.name, row.name, "the cache keeps the clean name too");
    }

    #[test]
    fn c04_cpu_percent_is_per_core() {
        // 4 cores busy for a whole second reads 400, not 4/18 of 100.
        let pct = cpu_percent(4_000_000_000, SEC).unwrap();
        assert!((pct - 400.0).abs() < 1e-3, "{pct}");
        let pct = cpu_percent(200_000_000, SEC).unwrap();
        assert!((pct - 20.0).abs() < 1e-3);
        assert_eq!(cpu_percent(10, 0), None);
        assert_eq!(cpu_percent(0, SEC), Some(0.0));
    }

    #[test]
    fn d25_first_sample_cpu_comes_from_start_time() {
        // rustc started 300 ms ago and has burned 290 ms: ~97, not 0.
        let pct = first_sample_cpu(290_000_000, 300_000, 18).unwrap();
        assert!((pct - 96.67).abs() < 0.1, "{pct}");
        // A multithreaded link step at 5 cores for 2 s.
        let pct = first_sample_cpu(10_000_000_000, 2_000_000, 18).unwrap();
        assert!((pct - 500.0).abs() < 0.1);
    }

    #[test]
    fn first_sample_cpu_edges() {
        assert_eq!(first_sample_cpu(5, 0, 18), None, "zero age");
        assert_eq!(first_sample_cpu(5, 999, 18), None, "under 1 ms");
        assert_eq!(first_sample_cpu(5, -20, 18), None, "clock skew");
        let capped = first_sample_cpu(u64::MAX, 1_000_000, 18).unwrap();
        assert!((capped - 1800.0).abs() < 1e-3, "capped at every core");
        let capped = first_sample_cpu(u64::MAX, 1_000_000, 0).unwrap();
        assert!((capped - 100.0).abs() < 1e-3, "ncpu 0 treated as 1");
        assert_eq!(first_sample_cpu(0, i64::MAX, 18), Some(0.0));
    }

    #[test]
    fn first_sample_sweep_stays_in_range() {
        for i in 1..400_u64 {
            let age_us = i64::try_from(i * i * 37).unwrap();
            let cpu_ns = i * 7_919_000;
            if let Some(p) = first_sample_cpu(cpu_ns, age_us, 18) {
                assert!(p.is_finite() && (0.0..=1800.0).contains(&p), "{i}: {p}");
            }
        }
    }

    fn row(cpu: Option<f32>) -> Process {
        Process {
            cpu,
            ..Process::default()
        }
    }

    #[test]
    fn unattributed_cpu_is_total_minus_readable_rows() {
        // 18 cores at 50% = 900; readable rows explain 300.
        let procs = [row(Some(200.0)), row(Some(100.0)), row(None)];
        assert!((unattributed_cpu(0.5, 18, &procs) - 600.0).abs() < 1e-3);
        // Rows over-explaining (sampled over different windows) floor at 0.
        assert!(unattributed_cpu(0.01, 18, &procs).abs() < f32::EPSILON);
        assert!((unattributed_cpu(0.5, 18, &[]) - 900.0).abs() < 1e-3);
        // Out-of-range ratios clamp.
        assert!((unattributed_cpu(3.0, 2, &[]) - 200.0).abs() < 1e-3);
        assert!(unattributed_cpu(-1.0, 2, &[]).abs() < f32::EPSILON);
    }

    fn cached_threads(start_us: i64, comm: &str, cpu_ns: u64, threads: u32) -> CachedProc {
        CachedProc {
            start_us,
            comm: comm.to_owned(),
            name: comm.to_owned(),
            cpu_ns: Some(cpu_ns),
            threads: Some(threads),
            ..CachedProc::default()
        }
    }

    /// Two real reads of one idle run, then the cache entry a third sample
    /// would see `after` the second read.
    fn idle_entry(read: Readings) -> (CachedProc, Instant) {
        let k = info(9, 100, "mds");
        let (_, first) = step(&k, None, read, SEC, 1_000_000);
        let (_, mut second) = step(&k, Some(first), read, SEC, 2_000_000);
        let at = Instant::now();
        second.read_at = Some(at);
        (second, at)
    }

    #[test]
    fn an_idle_pid_that_starts_pegging_a_core_is_read_at_once_not_held() {
        let (entry, at) = idle_entry(readable(4_000));
        let k = info(9, 100, "mds");
        let soon = at + Duration::from_millis(500);
        assert!(held_readings(Some(&entry), &k, soon, Duration::ZERO).is_none());
        // The next sample reads the burst: a full core over that second.
        let (row, _) = step(
            &k,
            Some(entry),
            readable(4_000 + 1_000_000_000),
            SEC,
            3_000_000,
        );
        assert!(
            row.cpu.is_some_and(|c| (c - 100.0).abs() < 0.01),
            "{:?}",
            row.cpu
        );
    }

    #[test]
    fn denied_hold_is_invalidated_on_pid_reuse_and_exec() {
        let k = info(9, 100, "root-daemon");
        let (_, mut entry) = step(&k, None, denied(), SEC, 1_000_000);
        let at = Instant::now();
        entry.read_at = Some(at);
        let soon = at + Duration::from_millis(500);
        assert!(held_readings(Some(&entry), &k, soon, Duration::ZERO).is_some());
        assert!(
            held_readings(
                Some(&entry),
                &info(9, 101, "root-daemon"),
                soon,
                Duration::ZERO
            )
            .is_none()
        );
        assert!(held_readings(Some(&entry), &info(9, 100, "sh"), soon, Duration::ZERO).is_none());
        assert!(held_readings(None, &k, soon, Duration::ZERO).is_none());
    }

    #[test]
    fn a_fully_denied_pid_is_asked_again_only_every_denied_reread() {
        let k = info(9, 100, "root-daemon");
        let (_, mut entry) = step(&k, None, denied(), SEC, 1_000_000);
        let at = Instant::now();
        entry.read_at = Some(at);
        let held = held_readings(
            Some(&entry),
            &k,
            at + Duration::from_secs(3),
            Duration::ZERO,
        );
        assert!(held.is_some_and(|r| r.denied && r.usage.is_none()));
        assert!(held_readings(Some(&entry), &k, at + DENIED_REREAD, Duration::ZERO).is_none());
        let (row, _) = step(&k, Some(entry.clone()), entry.last, SEC, 2_000_000);
        assert!(row.restricted && row.cpu.is_none(), "D23: none, never zero");
    }

    #[test]
    fn deltas_after_a_hold_span_the_pid_own_window() {
        let at = Instant::now();
        let now = at + Duration::from_secs(3);
        assert_eq!(pid_interval(SEC, Some(at), now), 3 * SEC);
        assert_eq!(pid_interval(0, Some(at), now), 0, "baseline pass");
        assert_eq!(pid_interval(SEC, None, now), SEC, "first read");
    }

    #[test]
    fn idle_process_keeps_its_thread_count_without_a_task_info_call() {
        let prev = cached_threads(100, "mds", 5_000, 7);
        assert_eq!(
            unchanged_threads(Some(&prev), &info(9, 100, "mds"), 5_000),
            Some(7)
        );
    }

    #[test]
    fn thread_cache_is_invalidated_when_the_process_ran() {
        let prev = cached_threads(100, "mds", 5_000, 7);
        assert_eq!(
            unchanged_threads(Some(&prev), &info(9, 100, "mds"), 5_001),
            None
        );
    }

    #[test]
    fn thread_cache_is_invalidated_on_pid_reuse() {
        // Same pid, same CPU time by coincidence, but a new run.
        let prev = cached_threads(100, "mds", 5_000, 7);
        assert_eq!(
            unchanged_threads(Some(&prev), &info(9, 101, "mds"), 5_000),
            None
        );
    }

    #[test]
    fn thread_cache_is_invalidated_on_exec() {
        let prev = cached_threads(100, "bash", 5_000, 7);
        assert_eq!(
            unchanged_threads(Some(&prev), &info(9, 100, "yes"), 5_000),
            None
        );
    }

    #[test]
    fn thread_cache_needs_a_previous_count() {
        let mut prev = cached_threads(100, "mds", 5_000, 7);
        assert_eq!(unchanged_threads(None, &info(9, 100, "mds"), 5_000), None);
        prev.threads = None;
        assert_eq!(
            unchanged_threads(Some(&prev), &info(9, 100, "mds"), 5_000),
            None,
            "a denied task info is retried, never cached as a count"
        );
        prev = CachedProc {
            cpu_ns: None,
            ..cached_threads(100, "mds", 5_000, 7)
        };
        assert_eq!(
            unchanged_threads(Some(&prev), &info(9, 100, "mds"), 5_000),
            None
        );
    }

    #[test]
    fn build_row_carries_threads_into_the_cache() {
        let tick = Tick {
            interval_ns: SEC,
            now_us: 1_000_000,
            ncpu: 8,
        };
        let read = Readings {
            threads: Some(12),
            ..readable(4_000)
        };
        let (_, cached) = build_row(&info(9, 100, "mds"), None, None, read, tick, String::new());
        assert_eq!(cached.threads, Some(12));
    }

    #[test]
    fn d14_identity_is_resolved_again_after_exec_or_pid_reuse() {
        let cached = CachedProc {
            start_us: 100,
            comm: String::from("bash"),
            name: String::from("run.sh"),
            ..CachedProc::default()
        };
        assert!(needs_identity(None, 100, "bash"), "new pid");
        assert!(!needs_identity(Some(&cached), 100, "bash"), "same run");
        assert!(needs_identity(Some(&cached), 100, "yes"), "exec()");
        assert!(needs_identity(Some(&cached), 101, "bash"), "pid reuse");
        let unnamed = CachedProc {
            name: String::new(),
            ..cached
        };
        assert!(
            needs_identity(Some(&unnamed), 100, "bash"),
            "retry empty name"
        );
    }

    #[test]
    fn d14_exec_renames_but_keeps_cpu_continuity() {
        let t0 = 1_700_000_000_000_000;
        let k = info(700, t0, "bash");
        let (r1, c1) = step(&k, None, readable(0), 0, t0 + 1_000_000);
        assert_eq!(r1.name, "bash");
        // `exec yes`: same pid and start, new comm; CPU counters continue.
        let k2 = info(700, t0, "yes");
        let (r2, c2) = step(&k2, Some(c1), readable(900_000_000), SEC, t0 + 2_000_000);
        assert_eq!(r2.name, "yes");
        assert_eq!(r2.command.as_deref(), Some("/bin/yes"));
        let cpu = r2.cpu.unwrap();
        assert!((cpu - 90.0).abs() < 1e-3, "delta, not lifetime: {cpu}");
        assert_eq!(c2.comm, "yes");
        assert_eq!(r2.cpu_spark, vec![cpu], "spark survives exec");
    }

    #[test]
    fn identity_is_reused_within_a_run() {
        let k = info(701, 5, "bash");
        let (_, c1) = step(&k, None, readable(0), 0, 1_000_000);
        // No new identity passed: the cached one carries over.
        let tick = Tick {
            interval_ns: SEC,
            now_us: 2_000_000,
            ncpu: 18,
        };
        let (r, _) = build_row(&k, Some(c1), None, readable(0), tick, String::new());
        assert_eq!(r.name, "bash");
        assert_eq!(r.args, vec!["bash"]);
    }

    #[test]
    fn pid_reuse_starts_a_fresh_run() {
        let k = info(700, 1_000_000, "old");
        let (_, c1) = step(&k, None, readable(0), SEC, 1_500_000);
        let (_, c2) = step(&k, Some(c1), readable(5_000_000_000), SEC, 2_500_000);
        assert!(!c2.spark.is_empty());
        // Same pid, new start time 0.5 s ago with 0.25 s of CPU.
        let k2 = info(700, 5_000_000, "new");
        let (r, c3) = step(&k2, Some(c2), readable(250_000_000), SEC, 5_500_000);
        assert_eq!(r.name, "new");
        let cpu = r.cpu.unwrap();
        assert!((cpu - 50.0).abs() < 1e-3, "first-sample path: {cpu}");
        assert_eq!(c3.spark.len(), 1, "old spark dropped");
    }

    #[test]
    fn baseline_pass_has_no_cpu_reading() {
        let k = info(9, 0, "x");
        let (r, c) = step(&k, None, readable(5_000_000_000), 0, 10_000_000);
        assert_eq!(r.cpu, None);
        assert!(r.cpu_spark.is_empty());
        assert_eq!(c.cpu_ns, Some(5_000_000_000));
        assert_eq!(r.energy_w, None, "no rate without a previous sample");
    }

    #[test]
    fn d23_restricted_rows_are_none_never_zero() {
        let k = Kinfo {
            uid: 88,
            ..info(400, 0, "WindowServer")
        };
        for interval in [0, SEC] {
            let (r, c) = step(&k, None, denied(), interval, 10_000_000);
            assert!(r.restricted);
            assert_eq!(r.cpu, None);
            assert_eq!(r.mem_bytes, None);
            assert_eq!(r.threads, None);
            assert_eq!(r.rss_bytes, None);
            assert_eq!(r.energy_w, None);
            assert_eq!(r.uid, 88, "uid still known from kinfo");
            assert!(r.cpu_spark.is_empty(), "no invented spark points");
            assert_eq!(c.cpu_ns, None);
        }
    }

    #[test]
    fn denied_rusage_falls_back_to_task_info() {
        let k = info(10, 0, "x");
        let read = Readings {
            usage: None,
            denied: true,
            ..readable(1_000_000_000)
        };
        let (_, c) = step(&k, None, read, SEC, 10_000_000);
        let read = Readings {
            usage: None,
            denied: true,
            ..readable(1_500_000_000)
        };
        let (r, _) = step(&k, Some(c), read, SEC, 11_000_000);
        assert!((r.cpu.unwrap() - 50.0).abs() < 1e-3);
        assert_eq!(r.threads, Some(4));
        assert_eq!(r.mem_bytes, None, "no footprint without rusage");
        assert_eq!(r.rss_bytes, Some(10 << 20));
        assert!(r.restricted);
    }

    #[test]
    fn d24_memory_is_footprint_and_rss_is_separate() {
        let (r, _) = step(&info(10, 0, "x"), None, readable(0), SEC, 10_000_000);
        assert_eq!(r.mem_bytes, Some(3 << 20));
        assert_eq!(r.rss_bytes, Some(10 << 20));
    }

    #[test]
    fn rusage_rates_flow_into_the_row() {
        let k = info(11, 0, "yes");
        let (_, c) = step(&k, None, readable(0), SEC, 10_000_000);
        let mut read = readable(1_000_000_000);
        if let Some(u) = read.usage.as_mut() {
            u.energy_nj = 6_000_000_000;
            u.disk_written = 4096;
            u.wakeups = 20;
            u.child_cpu_ns = 7;
        }
        let (r, _) = step(&k, Some(c), read, SEC, 11_000_000);
        assert!((r.energy_w.unwrap() - 6.0).abs() < 1e-3);
        assert!((r.disk_write_bps.unwrap() - 4096.0).abs() < 1e-6);
        assert!(r.disk_read_bps.unwrap().abs() < f64::EPSILON);
        assert!((r.wakeups_per_s.unwrap() - 20.0).abs() < 1e-3);
        assert_eq!(r.child_cpu_ns, Some(7));
    }

    #[test]
    fn spark_is_capped_and_ordered_over_a_long_run() {
        let k = info(12, 0, "x");
        let mut cached = None;
        let mut cpu_ns = 0;
        for i in 0..100_u64 {
            cpu_ns += i * 10_000_000;
            let (r, c) = step(&k, cached.take(), readable(cpu_ns), SEC, 10_000_000);
            assert!(r.cpu_spark.len() <= SPARK_LEN);
            cached = Some(c);
        }
        let spark = cached.unwrap().spark;
        assert_eq!(spark.len(), SPARK_LEN);
        assert!((spark[SPARK_LEN - 1] - 99.0).abs() < 1e-3, "newest last");
        assert!((spark[0] - 68.0).abs() < 1e-3, "oldest first");
    }

    #[test]
    fn counter_going_backwards_reads_zero_cpu() {
        let k = info(13, 0, "x");
        let (_, c) = step(&k, None, readable(9_000_000_000), SEC, 10_000_000);
        let (r, _) = step(&k, Some(c), readable(1), SEC, 11_000_000);
        assert_eq!(r.cpu, Some(0.0));
    }

    #[test]
    fn d26_status_comes_from_p_stat_only() {
        let mut k = info(14, 0, "x");
        let (r, _) = step(&k, None, readable(0), SEC, 1);
        assert_eq!(r.status, "", "SRUN is not trusted");
        k.stat = SSTOP;
        assert_eq!(step(&k, None, readable(0), SEC, 1).0.status, "stopped");
        k.stat = SZOMB;
        assert_eq!(step(&k, None, readable(0), SEC, 1).0.status, "zombie");
    }

    #[test]
    fn preferred_name_order() {
        let v = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        // Regression: a versioned executable path used to name the row.
        assert_eq!(
            preferred_name(
                50,
                &v(&["claude"]),
                Some("/Users/x/.local/share/claude/versions/2.1.241"),
                "2.1.241"
            ),
            "claude"
        );
        assert_eq!(
            preferred_name(50, &v(&["python3", "train.py"]), None, "Python"),
            "train.py"
        );
        assert_eq!(
            preferred_name(50, &[], Some("/Applications/Safari.app/MacOS/Safari"), ""),
            "Safari"
        );
        assert_eq!(
            preferred_name(50, &[], None, "MTLCompilerServi"),
            "MTLCompilerServi"
        );
        assert_eq!(preferred_name(0, &[], None, "kernel_task"), "kernel_task");
        assert_eq!(preferred_name(50, &[], None, ""), "pid 50");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn stub_reports_absent() {
        let mut col = ProcessCollector::new();
        let mut snap = Snapshot::empty();
        let out = col.collect(Instant::now(), &mut snap).unwrap();
        assert!(matches!(out, Outcome::Absent));
        assert!(snap.processes.is_empty());
        assert_eq!(snap.unattributed_cpu, None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_process_list_is_truthful() {
        let mut col = ProcessCollector::new();
        let first = col.sample().expect("procs1");
        assert!(!first.is_empty());
        std::thread::sleep(Duration::from_millis(40));
        let second = col.sample().expect("procs2");
        let self_pid = std::process::id();
        let me = second.iter().find(|p| p.pid == self_pid).expect("self");
        assert!(!me.restricted);
        assert!(me.cpu.is_some_and(|c| c.is_finite() && c >= 0.0));
        assert!(me.mem_bytes.is_some_and(|m| m > 0), "footprint");
        assert!(me.threads.is_some_and(|t| t >= 1));
        assert!(!me.user.is_empty());
        let command = me.command.clone().expect("command captured");
        assert!(command.contains('/'), "full path: {command}");
        assert!(me.start_unix > 1_500_000_000, "sane start epoch");
        assert!(!me.args.is_empty());

        let kernel = second.iter().find(|p| p.pid == 0).expect("kernel_task row");
        assert_eq!(kernel.name, "kernel_task");
        assert_eq!(kernel.user, "root");

        for p in &second {
            assert!(p.cpu.is_none_or(|c| c.is_finite() && c >= 0.0), "{p:?}");
            if p.restricted {
                assert!(
                    p.mem_bytes.is_none(),
                    "restricted {} has no footprint",
                    p.pid
                );
            }
        }
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            let restricted = second.iter().filter(|p| p.restricted).count();
            let none_mem = second.iter().filter(|p| p.mem_bytes.is_none()).count();
            assert!(restricted > 0, "other users' pids are restricted");
            assert_eq!(restricted, none_mem, "every None is an EPERM pid");
            if let Some(ws) = second.iter().find(|p| p.name == "WindowServer") {
                assert!(
                    ws.restricted && ws.cpu.is_none(),
                    "WindowServer is — not 0.0"
                );
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_collect_fills_unattributed_and_ncpu() {
        let mut col = ProcessCollector::new();
        let mut snap = Snapshot::empty();
        snap.cpu.active = 0.5;
        let t0 = Instant::now();
        assert!(matches!(
            col.collect(t0, &mut snap).unwrap(),
            Outcome::Baseline
        ));
        assert_eq!(snap.unattributed_cpu, None);
        std::thread::sleep(Duration::from_millis(30));
        assert!(matches!(
            col.collect(t0, &mut snap).unwrap(),
            Outcome::Fresh
        ));
        assert!(snap.process_ncpu >= 1);
        let un = snap.unattributed_cpu.unwrap();
        assert!(un.is_finite() && un >= 0.0);
        assert!(un <= 50.0 * snap.process_ncpu as f32 + 1e-3);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn d14_live_exec_is_renamed_within_one_sample() {
        let mut child = std::process::Command::new("/bin/bash")
            .args(["-c", "sleep 0.4; exec /bin/sleep 5"])
            .spawn()
            .unwrap();
        let pid = child.id();
        let mut col = ProcessCollector::new();
        std::thread::sleep(Duration::from_millis(100));
        let before = col.sample().unwrap();
        let name_before = before.iter().find(|p| p.pid == pid).unwrap().name.clone();
        std::thread::sleep(Duration::from_millis(700));
        let after = col.sample().unwrap();
        let renamed = after.iter().find(|p| p.pid == pid).unwrap().clone();
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(name_before, "bash");
        assert_eq!(renamed.name, "sleep");
        assert_eq!(renamed.command.as_deref(), Some("/bin/sleep"));
        assert_eq!(renamed.args, vec!["/bin/sleep", "5"]);
    }

    /// Manual check against `top -l 1 -stats pid,mem`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "manual: cargo test -p siltop-metrics -- --ignored --nocapture print_table"]
    fn print_table() {
        let mut col = ProcessCollector::new();
        let _ = col.sample().unwrap();
        std::thread::sleep(Duration::from_millis(500));
        for p in col.sample().unwrap() {
            println!(
                "{} {:?} {:?} {:?} {} {:?} {}",
                p.pid, p.name, p.cpu, p.mem_bytes, p.restricted, p.energy_w, p.user
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn start_sec_is_none_for_a_missing_pid() {
        assert_eq!(macos::start_sec(u32::MAX), None);
        assert_eq!(macos::start_sec(99_999_999), None);
    }
}
