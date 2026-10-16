//! On-demand deep look at ONE pid for the dossier: threads, open files,
//! sockets (with listening ports) and the working directory.
//!
//! This walks every fd and thread of the process, so it is only ever run
//! for the pid the user opened, never for the whole list.

use siltop_core::Inspection;
#[cfg(target_os = "macos")]
use siltop_core::{OpenFile, Socket, ThreadInfo};

use super::actions::{ActionError, ActionResult};

/// Threads, files, sockets and cwd of `pid`. A process we may not read
/// comes back with `denied` set rather than as an error, so the dossier
/// can still show the table row; a missing pid is `Exited`.
pub fn inspect(pid: u32) -> ActionResult<Inspection> {
    let raw = match libc::c_int::try_from(pid) {
        Ok(raw) if raw > 0 => raw,
        _ => return Err(ActionError::InvalidPid { pid }),
    };
    #[cfg(target_os = "macos")]
    {
        macos::inspect(pid, raw)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = raw;
        Err(ActionError::Unsupported { pid })
    }
}

/// `proc_fileinfo` precedes every per-fd record.
#[cfg(target_os = "macos")]
const FILEINFO_LEN: usize = 24;
/// Byte offsets inside `socket_info` (after `proc_fileinfo`), from
/// `<sys/proc_info.h>`: `vinfo_stat` is 136 bytes.
#[cfg(target_os = "macos")]
mod sockinfo {
    pub(super) const OPTIONS: usize = 164;
    pub(super) const FAMILY: usize = 160;
    pub(super) const KIND: usize = 232;
    pub(super) const PROTO: usize = 240;
    /// `in_sockinfo.insi_fport` then `insi_lport`, network order in the
    /// low 16 bits of an `int`.
    pub(super) const FPORT: usize = PROTO;
    pub(super) const LPORT: usize = PROTO + 4;
    pub(super) const SOCKINFO_IN: i32 = 1;
    pub(super) const SOCKINFO_TCP: i32 = 2;
    pub(super) const SOCKINFO_UN: i32 = 3;
    pub(super) const SOCKINFO_NDRV: i32 = 4;
    pub(super) const SOCKINFO_KERN_EVENT: i32 = 5;
    pub(super) const SOCKINFO_KERN_CTL: i32 = 6;
    pub(super) const SOCKINFO_VSOCK: i32 = 7;
    pub(super) const SO_ACCEPTCONN: i16 = 0x0002;
    pub(super) const LEN: usize = 1024;
}

#[cfg(target_os = "macos")]
fn read_i32(buf: &[u8], at: usize) -> Option<i32> {
    buf.get(at..at + 4)?.try_into().ok().map(i32::from_ne_bytes)
}

#[cfg(target_os = "macos")]
fn read_i16(buf: &[u8], at: usize) -> Option<i16> {
    buf.get(at..at + 2)?.try_into().ok().map(i16::from_ne_bytes)
}

/// A port stored as a network-order `u16` in the low half of an `int`.
#[cfg(target_os = "macos")]
fn net_port(raw: i32) -> Option<u16> {
    let port = u16::from_be(
        raw.to_ne_bytes()
            .first_chunk::<2>()
            .map_or(0, |b| u16::from_ne_bytes(*b)),
    );
    (port != 0).then_some(port)
}

/// Parses a `socket_fdinfo` buffer (as `PROC_PIDFDSOCKETINFO` fills it).
#[cfg(target_os = "macos")]
fn parse_socket(fd: i32, buf: &[u8]) -> Option<Socket> {
    let at = |off: usize| FILEINFO_LEN + off;
    let kind_raw = read_i32(buf, at(sockinfo::KIND))?;
    let options = read_i16(buf, at(sockinfo::OPTIONS))?;
    let family = read_i32(buf, at(sockinfo::FAMILY))?;
    let inet = matches!(kind_raw, sockinfo::SOCKINFO_IN | sockinfo::SOCKINFO_TCP)
        && (family == libc::AF_INET || family == libc::AF_INET6);
    let kind = match kind_raw {
        sockinfo::SOCKINFO_TCP => "tcp",
        sockinfo::SOCKINFO_IN => "udp",
        sockinfo::SOCKINFO_UN => "unix",
        sockinfo::SOCKINFO_NDRV => "ndrv",
        sockinfo::SOCKINFO_KERN_EVENT => "kevt",
        sockinfo::SOCKINFO_KERN_CTL => "kctl",
        sockinfo::SOCKINFO_VSOCK => "vsock",
        _ => "other",
    };
    let (local_port, remote_port) = if inet {
        (
            read_i32(buf, at(sockinfo::LPORT)).and_then(net_port),
            read_i32(buf, at(sockinfo::FPORT)).and_then(net_port),
        )
    } else {
        (None, None)
    };
    Some(Socket {
        fd,
        kind,
        local_port,
        remote_port,
        listening: kind == "tcp" && options & sockinfo::SO_ACCEPTCONN != 0,
    })
}

#[cfg(target_os = "macos")]
fn c_string(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(bytes.get(..end).unwrap_or_default()).into_owned()
}

#[cfg(target_os = "macos")]
mod macos {
    use std::mem::{MaybeUninit, size_of};

    use super::{
        ActionError, ActionResult, FILEINFO_LEN, Inspection, OpenFile, ThreadInfo, c_string,
        parse_socket, sockinfo,
    };

    /// Kernel thread ids (the ones `pthread_threadid_np` and Instruments
    /// show), not the per-process thread handles `PROC_PIDLISTTHREADS`
    /// returns.
    const PROC_PIDLISTTHREADIDS: libc::c_int = 28;
    /// `proc_threadinfo` looked up by kernel thread id.
    const PROC_PIDTHREADID64INFO: libc::c_int = 15;
    const PROC_PIDFDVNODEPATHINFO: libc::c_int = 2;
    const PROC_PIDFDSOCKETINFO: libc::c_int = 3;
    /// Kernel CPU-usage scale: 1000 is one core fully busy.
    const TH_USAGE_SCALE: f32 = 1000.0;
    /// Upper bound so a runaway process cannot make us allocate forever.
    const MAX_ITEMS: usize = 16_384;

    fn errno() -> i32 {
        std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO)
    }

    pub(super) fn inspect(pid: u32, raw: libc::c_int) -> ActionResult<Inspection> {
        let mut out = Inspection {
            pid,
            ..Inspection::default()
        };
        match cwd(raw) {
            Ok(cwd) => out.cwd = cwd,
            Err(libc::ESRCH) => return Err(ActionError::Exited { pid }),
            Err(libc::EPERM | libc::EACCES) => out.denied = true,
            Err(_) => {}
        }
        match threads(raw) {
            Ok(threads) => out.threads = threads,
            Err(libc::ESRCH) => return Err(ActionError::Exited { pid }),
            Err(_) => out.denied = true,
        }
        if let Ok(fds) = list_fds(raw) {
            for fd in fds {
                let (num, kind) = (fd.proc_fd, fd.proc_fdtype as libc::c_int);
                if kind == libc::PROX_FDTYPE_VNODE {
                    if let Some(path) = fd_path(raw, num) {
                        out.files.push(OpenFile { fd: num, path });
                    }
                } else if kind == libc::PROX_FDTYPE_SOCKET
                    && let Some(sock) = fd_socket(raw, num)
                {
                    out.sockets.push(sock);
                }
            }
        } else {
            out.denied = true;
        }
        Ok(out)
    }

    fn cwd(raw: libc::c_int) -> Result<Option<String>, i32> {
        let mut info = MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
        let want = size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
        // SAFETY: `info` is writable for `want` bytes and zero-initialised.
        let got = unsafe {
            libc::proc_pidinfo(
                raw,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                info.as_mut_ptr().cast(),
                want,
            )
        };
        if got != want {
            return Err(if got <= 0 { errno() } else { libc::EIO });
        }
        // SAFETY: the kernel filled all `want` bytes.
        let info = unsafe { info.assume_init() };
        // SAFETY: `vip_path` is a plain byte array of MAXPATHLEN.
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                info.pvi_cdir.vip_path.as_ptr().cast::<u8>(),
                size_of_val(&info.pvi_cdir.vip_path),
            )
        };
        let path = c_string(bytes);
        Ok((!path.is_empty()).then_some(path))
    }

    /// Calls a list-returning `proc_pidinfo` flavor, growing the buffer
    /// until the result fits.
    fn list<T: Copy>(raw: libc::c_int, flavor: libc::c_int) -> Result<Vec<T>, i32> {
        let mut cap = 64usize;
        loop {
            let mut buf: Vec<T> = Vec::with_capacity(cap);
            let bytes = (cap * size_of::<T>()) as libc::c_int;
            // SAFETY: `buf` has room for `cap` items (`bytes` bytes); the
            // kernel writes at most `bytes`.
            let got = unsafe { libc::proc_pidinfo(raw, flavor, 0, buf.as_mut_ptr().cast(), bytes) };
            if got <= 0 {
                return Err(errno());
            }
            let n = got as usize / size_of::<T>();
            if n < cap || cap >= MAX_ITEMS {
                // SAFETY: the kernel initialised the first `n` items.
                unsafe { buf.set_len(n.min(cap)) };
                return Ok(buf);
            }
            cap *= 4;
        }
    }

    fn threads(raw: libc::c_int) -> Result<Vec<ThreadInfo>, i32> {
        let ids: Vec<u64> = list(raw, PROC_PIDLISTTHREADIDS)?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let mut info = MaybeUninit::<libc::proc_threadinfo>::zeroed();
            let want = size_of::<libc::proc_threadinfo>() as libc::c_int;
            // SAFETY: `info` is writable for `want` bytes; `id` is a thread
            // id the kernel just listed.
            let got = unsafe {
                libc::proc_pidinfo(
                    raw,
                    PROC_PIDTHREADID64INFO,
                    id,
                    info.as_mut_ptr().cast(),
                    want,
                )
            };
            if got != want {
                continue; // the thread exited between list and read
            }
            // SAFETY: fully written by the kernel.
            let info = unsafe { info.assume_init() };
            // SAFETY: `pth_name` is a fixed c_char array.
            let name: &[u8] = unsafe {
                std::slice::from_raw_parts(info.pth_name.as_ptr().cast::<u8>(), info.pth_name.len())
            };
            out.push(ThreadInfo {
                id,
                name: c_string(name),
                cpu_ns: info.pth_user_time.saturating_add(info.pth_system_time),
                cpu_pct: info.pth_cpu_usage as f32 / TH_USAGE_SCALE * 100.0,
            });
        }
        Ok(out)
    }

    fn list_fds(raw: libc::c_int) -> Result<Vec<libc::proc_fdinfo>, i32> {
        list(raw, libc::PROC_PIDLISTFDS)
    }

    fn fd_info(raw: libc::c_int, fd: i32, flavor: libc::c_int, buf: &mut [u8]) -> Option<usize> {
        let len = libc::c_int::try_from(buf.len()).ok()?;
        // SAFETY: `buf` is writable for `len` bytes.
        let got = unsafe { libc::proc_pidfdinfo(raw, fd, flavor, buf.as_mut_ptr().cast(), len) };
        usize::try_from(got).ok().filter(|&n| n > FILEINFO_LEN)
    }

    fn fd_path(raw: libc::c_int, fd: i32) -> Option<String> {
        // vnode_fdinfowithpath: proc_fileinfo, vnode_info, path[MAXPATHLEN].
        let at = FILEINFO_LEN + size_of::<libc::vnode_info>();
        let mut buf = vec![0u8; at + libc::PATH_MAX as usize];
        fd_info(raw, fd, PROC_PIDFDVNODEPATHINFO, &mut buf)?;
        let path = c_string(buf.get(at..)?);
        (!path.is_empty()).then_some(path)
    }

    fn fd_socket(raw: libc::c_int, fd: i32) -> Option<siltop_core::Socket> {
        let mut buf = vec![0u8; sockinfo::LEN];
        let got = fd_info(raw, fd, PROC_PIDFDSOCKETINFO, &mut buf)?;
        parse_socket(fd, buf.get(..got)?)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn inspect_rejects_pid_zero() {
        assert_eq!(inspect(0), Err(ActionError::InvalidPid { pid: 0 }));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn inspect_own_pid_lists_threads_and_cwd() {
        let me = inspect(std::process::id()).expect("own pid");
        assert!(!me.threads.is_empty(), "at least the test thread");
        let cwd = me.cwd.expect("cwd");
        let want = std::env::current_dir().expect("cwd");
        assert_eq!(
            std::fs::canonicalize(&cwd).ok(),
            std::fs::canonicalize(want).ok()
        );
        assert!(!me.denied);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn inspect_sees_our_listening_port_and_open_file() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let file = std::env::current_exe().expect("exe");
        let _held = std::fs::File::open(&file).expect("open");
        let me = inspect(std::process::id()).expect("own pid");
        assert!(me.listening_ports().contains(&port), "{:?}", me.sockets);
        let name = file.file_name().and_then(|n| n.to_str()).expect("name");
        assert!(
            me.files.iter().any(|f| f.path.ends_with(name)),
            "{:?}",
            me.files
        );
    }

    /// Regression: ids were per-process thread handles (one was 0), which
    /// no other tool shows. They are kernel thread ids now.
    #[cfg(target_os = "macos")]
    #[test]
    fn thread_ids_are_kernel_tids_not_handles() {
        let mut tid = 0u64;
        // SAFETY: `pthread_self()` is this live thread; `tid` is writable.
        let rc = unsafe { libc::pthread_threadid_np(libc::pthread_self(), &raw mut tid) };
        assert_eq!(rc, 0);
        let me = inspect(std::process::id()).expect("own pid");
        assert!(
            me.threads.iter().any(|t| t.id == tid),
            "tid {tid} in {:?}",
            me.threads.iter().map(|t| t.id).collect::<Vec<_>>()
        );
        assert!(me.threads.iter().all(|t| t.id != 0), "no zero handle");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn socket_kinds_are_named_not_other() {
        let mut buf = vec![0u8; sockinfo::LEN];
        let at = FILEINFO_LEN + sockinfo::KIND;
        buf[at..at + 4].copy_from_slice(&sockinfo::SOCKINFO_KERN_CTL.to_ne_bytes());
        let sock = parse_socket(7, &buf).expect("parsed");
        assert_eq!(sock.kind, "kctl");
        assert_eq!((sock.local_port, sock.remote_port), (None, None));
        assert!(!sock.listening);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn inspect_missing_pid_is_exited() {
        assert_eq!(
            inspect(99_999_999),
            Err(ActionError::Exited { pid: 99_999_999 })
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn inspect_launchd_as_user_is_denied_not_an_error() {
        // SAFETY: geteuid(2) has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let init = inspect(1).expect("launchd exists");
        assert!(init.denied);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn net_port_reads_network_order() {
        let raw = i32::from_ne_bytes([0x1f, 0x90, 0, 0]); // 8080 big-endian
        assert_eq!(net_port(raw), Some(8080));
        assert_eq!(net_port(0), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_socket_handles_a_short_buffer() {
        assert_eq!(parse_socket(3, &[0u8; 40]), None);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn inspect_stub_is_unsupported() {
        assert_eq!(inspect(1), Err(ActionError::Unsupported { pid: 1 }));
    }
}
