//! `struct kinfo_proc` parsing from the `KERN_PROC_ALL` sysctl buffer.
//!
//! One sysctl returns every pid with its uid, parent, start time, `p_stat`
//! and `comm`, even for other users' processes, so none of those need a
//! per-pid `proc_pidinfo` call. Offsets are for the LP64 layout (arm64 and
//! `x86_64` agree) and are checked against `libc::kinfo_proc` in tests.

/// `sizeof(struct kinfo_proc)`.
pub(crate) const KINFO_PROC_LEN: usize = 648;
/// `kp_proc.p_un.p_starttime` (`struct timeval`).
pub(crate) const OFF_START_SEC: usize = 0;
pub(crate) const OFF_START_USEC: usize = 8;
/// `kp_proc.p_stat` (one byte).
pub(crate) const OFF_STAT: usize = 36;
pub(crate) const OFF_PID: usize = 40;
/// `kp_proc.p_comm` (`MAXCOMLEN + 1` bytes).
pub(crate) const OFF_COMM: usize = 243;
pub(crate) const COMM_LEN: usize = 17;
/// `kp_eproc.e_ucred.cr_uid`: the effective uid.
pub(crate) const OFF_UID: usize = 420;
/// `kp_eproc.e_ppid`.
pub(crate) const OFF_PPID: usize = 560;

/// `p_stat` values from `<sys/proc.h>`.
#[cfg(test)]
pub(crate) const SIDL: u8 = 1;
#[cfg(test)]
pub(crate) const SRUN: u8 = 2;
#[cfg(test)]
pub(crate) const SSLEEP: u8 = 3;
pub(crate) const SSTOP: u8 = 4;
pub(crate) const SZOMB: u8 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Kinfo {
    pub pid: u32,
    pub ppid: u32,
    pub uid: u32,
    /// Start time as microseconds since the Unix epoch.
    pub start_us: i64,
    pub stat: u8,
    pub comm: String,
}

impl Kinfo {
    pub(crate) const fn start_sec(&self) -> i64 {
        self.start_us.div_euclid(1_000_000)
    }
}

/// Every whole record in `buf`; a trailing partial record is ignored.
pub(crate) fn parse_all(buf: &[u8]) -> Vec<Kinfo> {
    buf.as_chunks::<KINFO_PROC_LEN>()
        .0
        .iter()
        .filter_map(parse_one)
        .collect()
}

/// pid 0 (`kernel_task`) is kept: it is usually the busiest process.
pub(crate) fn parse_one(chunk: &[u8; KINFO_PROC_LEN]) -> Option<Kinfo> {
    let pid = u32::try_from(i32_at(chunk, OFF_PID)?).ok()?;
    let parent = u32::try_from(i32_at(chunk, OFF_PPID).unwrap_or(0).max(0)).unwrap_or(0);
    let uid = u32_at(chunk, OFF_UID).unwrap_or(0);
    let sec = i64_at(chunk, OFF_START_SEC).unwrap_or(0);
    let usec = i32_at(chunk, OFF_START_USEC).unwrap_or(0).clamp(0, 999_999);
    let start_us = sec
        .saturating_mul(1_000_000)
        .saturating_add(i64::from(usec));
    Some(Kinfo {
        pid,
        ppid: parent,
        uid,
        start_us,
        stat: chunk[OFF_STAT],
        comm: comm_at(chunk, OFF_COMM),
    })
}

/// Only states `p_stat` reports truthfully: nearly every live process
/// reads `SRUN`, so running and sleeping are not distinguished (D26).
pub(crate) const fn state_word(stat: u8) -> &'static str {
    match stat {
        SSTOP => "stopped",
        SZOMB => "zombie",
        _ => "",
    }
}

fn i32_at(buf: &[u8], off: usize) -> Option<i32> {
    buf.get(off..off + 4)?
        .try_into()
        .ok()
        .map(i32::from_ne_bytes)
}

fn u32_at(buf: &[u8], off: usize) -> Option<u32> {
    buf.get(off..off + 4)?
        .try_into()
        .ok()
        .map(u32::from_ne_bytes)
}

fn i64_at(buf: &[u8], off: usize) -> Option<i64> {
    buf.get(off..off + 8)?
        .try_into()
        .ok()
        .map(i64::from_ne_bytes)
}

fn comm_at(buf: &[u8], off: usize) -> String {
    let Some(raw) = buf.get(off..off + COMM_LEN) else {
        return String::new();
    };
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).into_owned()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
pub(crate) mod tests {
    use super::*;

    /// Builds one synthetic record with the fields this parser reads.
    #[allow(clippy::similar_names)]
    pub(crate) fn record(
        pid: i32,
        ppid: i32,
        uid: u32,
        start: (i64, i32),
        stat: u8,
        comm: &[u8],
    ) -> [u8; KINFO_PROC_LEN] {
        let mut r = [0_u8; KINFO_PROC_LEN];
        r[OFF_START_SEC..OFF_START_SEC + 8].copy_from_slice(&start.0.to_ne_bytes());
        r[OFF_START_USEC..OFF_START_USEC + 4].copy_from_slice(&start.1.to_ne_bytes());
        r[OFF_STAT] = stat;
        r[OFF_PID..OFF_PID + 4].copy_from_slice(&pid.to_ne_bytes());
        r[OFF_PPID..OFF_PPID + 4].copy_from_slice(&ppid.to_ne_bytes());
        r[OFF_UID..OFF_UID + 4].copy_from_slice(&uid.to_ne_bytes());
        let n = comm.len().min(COMM_LEN);
        r[OFF_COMM..OFF_COMM + n].copy_from_slice(&comm[..n]);
        r
    }

    /// libc 0.2.189 has no `kinfo_proc`, so the layout is checked against
    /// the kernel: `KERN_PROC_PID` for this process must be exactly one
    /// 648-byte record whose fields match getpid/getuid/getppid.
    #[cfg(target_os = "macos")]
    #[test]
    fn d23_kinfo_offsets_match_the_kernel() {
        let pid = i32::try_from(std::process::id()).unwrap();
        let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid];
        let mut buf = [0_u8; KINFO_PROC_LEN * 2];
        let mut got = buf.len();
        // SAFETY: `buf` is writable for `got` bytes; mib is a valid name.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                4,
                buf.as_mut_ptr().cast(),
                &raw mut got,
                std::ptr::null_mut(),
                0,
            )
        };
        assert_eq!(rc, 0);
        assert_eq!(got, KINFO_PROC_LEN, "struct size");
        let k = parse_one(buf[..KINFO_PROC_LEN].try_into().unwrap()).unwrap();
        assert_eq!(k.pid, std::process::id());
        // SAFETY: these calls have no preconditions.
        let (euid, parent) = unsafe { (libc::geteuid(), libc::getppid()) };
        assert_eq!(k.uid, euid, "cr_uid at 420");
        assert_eq!(k.ppid, u32::try_from(parent).unwrap());
        let exe = std::env::current_exe().unwrap();
        let file = exe.file_name().unwrap().to_string_lossy().into_owned();
        assert!(file.starts_with(&k.comm), "comm {} vs {file}", k.comm);
        assert!(k.start_sec() > 1_600_000_000);
    }

    #[test]
    fn parses_every_field_of_a_record() {
        let r = record(
            4242,
            1,
            501,
            (1_700_000_000, 250_000),
            SSLEEP,
            b"WindowServer",
        );
        let k = parse_one(&r).unwrap();
        assert_eq!(k.pid, 4242);
        assert_eq!(k.ppid, 1);
        assert_eq!(k.uid, 501);
        assert_eq!(k.start_us, 1_700_000_000_250_000);
        assert_eq!(k.start_sec(), 1_700_000_000);
        assert_eq!(k.stat, SSLEEP);
        assert_eq!(k.comm, "WindowServer");
    }

    #[test]
    fn d23_kernel_task_pid_zero_is_kept() {
        let r = record(0, 0, 0, (1_690_000_000, 0), SRUN, b"kernel_task");
        let k = parse_one(&r).unwrap();
        assert_eq!((k.pid, k.uid, k.comm.as_str()), (0, 0, "kernel_task"));
    }

    #[test]
    fn negative_pid_is_dropped_and_negative_ppid_clamps() {
        assert!(parse_one(&record(-1, 0, 0, (0, 0), SRUN, b"x")).is_none());
        let k = parse_one(&record(9, -5, 0, (0, 0), SRUN, b"x")).unwrap();
        assert_eq!(k.ppid, 0);
    }

    #[test]
    fn comm_is_cut_at_nul_and_capped_at_seventeen_bytes() {
        let k = parse_one(&record(2, 1, 0, (0, 0), 0, b"MTLCompilerServiceXYZ")).unwrap();
        assert_eq!(k.comm, "MTLCompilerServic");
        let k = parse_one(&record(2, 1, 0, (0, 0), 0, b"")).unwrap();
        assert_eq!(k.comm, "");
        let k = parse_one(&record(2, 1, 0, (0, 0), 0, b"ab\0cd")).unwrap();
        assert_eq!(k.comm, "ab");
    }

    #[test]
    fn invalid_utf8_comm_is_lossy_not_an_error() {
        let k = parse_one(&record(2, 1, 0, (0, 0), 0, &[0xff, b'a', 0xfe])).unwrap();
        assert_eq!(k.comm, "\u{fffd}a\u{fffd}");
    }

    #[test]
    fn nobody_uid_round_trips() {
        let k = parse_one(&record(2, 1, u32::MAX - 1, (0, 0), 0, b"x")).unwrap();
        assert_eq!(k.uid, 4_294_967_294);
    }

    #[test]
    fn bogus_usec_is_clamped() {
        let k = parse_one(&record(2, 1, 0, (10, 5_000_000), 0, b"x")).unwrap();
        assert_eq!(k.start_us, 10_999_999);
        let k = parse_one(&record(2, 1, 0, (10, -3), 0, b"x")).unwrap();
        assert_eq!(k.start_us, 10_000_000);
        let k = parse_one(&record(2, 1, 0, (i64::MAX, 1), 0, b"x")).unwrap();
        assert_eq!(k.start_us, i64::MAX, "saturates, never wraps");
    }

    #[test]
    fn parse_all_walks_records_and_ignores_a_partial_tail() {
        let mut buf = Vec::new();
        for pid in 0..300 {
            buf.extend_from_slice(&record(pid, 1, 501, (0, 0), SSLEEP, b"p"));
        }
        buf.extend_from_slice(&[7_u8; KINFO_PROC_LEN - 1]);
        let all = parse_all(&buf);
        assert_eq!(all.len(), 300);
        assert!(all.iter().enumerate().all(|(i, k)| k.pid as usize == i));
        assert!(parse_all(&[]).is_empty());
    }

    #[test]
    fn d26_state_is_only_stopped_or_zombie() {
        let table = [
            (0, ""),
            (SIDL, ""),
            (SRUN, ""),
            (SSLEEP, ""),
            (SSTOP, "stopped"),
            (SZOMB, "zombie"),
            (6, ""),
            (255, ""),
        ];
        for (stat, want) in table {
            assert_eq!(state_word(stat), want, "p_stat {stat}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_kinfo_sweep_has_self_launchd_and_kernel_task() {
        let all = super::super::macos::kinfo_all().unwrap();
        assert!(all.len() > 50);
        let me = all.iter().find(|k| k.pid == std::process::id()).unwrap();
        // SAFETY: getuid has no preconditions.
        assert_eq!(me.uid, unsafe { libc::getuid() });
        let launchd = all.iter().find(|k| k.pid == 1).unwrap();
        assert_eq!((launchd.uid, launchd.comm.as_str()), (0, "launchd"));
        assert!(all.iter().any(|k| k.pid == 0), "kernel_task present");
        let now_us = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros(),
        )
        .unwrap();
        for k in &all {
            assert!(
                k.start_us <= now_us + 1_000_000,
                "{k:?} starts in the future"
            );
        }
    }
}
