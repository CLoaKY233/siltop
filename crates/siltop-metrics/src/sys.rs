//! Shared macOS sysctl helpers.

use std::ffi::CStr;

pub(crate) fn sysctl_u64(name: &CStr) -> Option<u64> {
    let mut buf = [0u8; 8];
    let mut len = buf.len();
    let rc = unsafe {
        // SAFETY: name is a valid C string; kernel writes at most `len` bytes into `buf`.
        libc::sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    match len {
        4 => {
            let raw = buf.get(..4)?.try_into().ok()?;
            Some(u32::from_ne_bytes(raw).into())
        }
        8 => Some(u64::from_ne_bytes(buf)),
        _ => None,
    }
}

pub(crate) fn sysctl_i32(name: &CStr) -> Option<i32> {
    let val = sysctl_u64(name)?;
    i32::try_from(val).ok()
}

pub(crate) fn sysctl_string(name: &CStr) -> Option<String> {
    let mut len = 0usize;
    let probe = unsafe {
        // SAFETY: query-only; oldp is null so the kernel returns the required length.
        libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::null_mut(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if probe != 0 || len == 0 {
        return None;
    }
    let mut buf = vec![0u8; len];
    let mut got = buf.len();
    let rc = unsafe {
        // SAFETY: `buf` is writable for `got` bytes.
        libc::sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            &raw mut got,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || got == 0 {
        return None;
    }
    buf.truncate(got);
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let text = String::from_utf8_lossy(&buf[..end]).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

pub(crate) fn page_size() -> u64 {
    let n = unsafe {
        // SAFETY: sysconf is a documented query with no pointer args.
        libc::sysconf(libc::_SC_PAGESIZE)
    };
    if n > 0 { n as u64 } else { 4096 }
}

pub(crate) fn logical_cpus() -> u32 {
    sysctl_u64(c"hw.logicalcpu")
        .or_else(|| sysctl_u64(c"hw.ncpu"))
        .and_then(|n| u32::try_from(n).ok())
        .filter(|&n| n > 0)
        .unwrap_or(1)
}

pub(crate) fn mach_ticks_to_ns(ticks: u64) -> u64 {
    let (numer, denom) = mach_timebase();
    crate::power::ticks_to_ns(ticks, numer, denom).unwrap_or(u64::MAX)
}

/// Mach absolute time in ns: the clock `IOReport` driver timestamps use.
/// It stops while the Mac sleeps.
pub(crate) fn mach_now_ns() -> u64 {
    mach_ticks_to_ns(unsafe {
        // SAFETY: no arguments; reads a monotonic clock.
        mach_absolute_time()
    })
}

/// Mach continuous time in ns: keeps running while the Mac sleeps.
pub(crate) fn mach_continuous_ns() -> u64 {
    mach_ticks_to_ns(unsafe {
        // SAFETY: no arguments; reads a monotonic clock.
        mach_continuous_time()
    })
}

#[repr(C)]
struct MachTimebase {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    fn mach_timebase_info(info: *mut MachTimebase) -> libc::kern_return_t;
    fn mach_absolute_time() -> u64;
    fn mach_continuous_time() -> u64;
}

fn mach_timebase() -> (u32, u32) {
    static BASE: std::sync::OnceLock<(u32, u32)> = std::sync::OnceLock::new();
    *BASE.get_or_init(|| {
        let mut info = MachTimebase { numer: 0, denom: 0 };
        let kr = unsafe {
            // SAFETY: writes into a local MachTimebase.
            mach_timebase_info(&raw mut info)
        };
        if kr == libc::KERN_SUCCESS && info.numer > 0 && info.denom > 0 {
            (info.numer, info.denom)
        } else {
            (1, 1)
        }
    })
}

/// CPU seconds used by the calling thread, for cost tests.
#[cfg(test)]
pub(crate) fn thread_cpu_secs() -> f64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let rc = unsafe {
        // SAFETY: `ts` is a writable timespec out-parameter.
        libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut ts)
    };
    if rc != 0 {
        return 0.0;
    }
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timebase_is_sane() {
        let (numer, denom) = mach_timebase();
        assert!(numer > 0);
        assert!(denom > 0);
        assert_eq!(mach_ticks_to_ns(0), 0);
        let one = mach_ticks_to_ns(1_000_000);
        assert!(one > 0);
    }

    #[test]
    fn continuous_time_never_trails_absolute_time() {
        let abs = mach_now_ns();
        let cont = mach_continuous_ns();
        assert!(abs > 0);
        assert!(cont >= abs, "continuous time includes sleep");
        assert!(mach_now_ns() >= abs, "monotonic");
    }
}
