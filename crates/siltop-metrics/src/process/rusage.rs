//! `proc_pid_rusage(RUSAGE_INFO_V6)`: footprint, energy, disk I/O,
//! wakeups and children's CPU time for one pid.
//!
//! libc 0.2.189 stops at `rusage_info_v4`, so v6 is declared here; its size
//! is checked in a test. Times are Mach absolute ticks, converted with the
//! timebase by the caller.

/// `struct rusage_info_v6` from `<sys/resource.h>` (macOS 27 SDK).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_field_names)] // mirrors the SDK names
pub(crate) struct RusageInfoV6 {
    pub ri_uuid: [u8; 16],
    pub ri_user_time: u64,
    pub ri_system_time: u64,
    pub ri_pkg_idle_wkups: u64,
    pub ri_interrupt_wkups: u64,
    pub ri_pageins: u64,
    pub ri_wired_size: u64,
    pub ri_resident_size: u64,
    pub ri_phys_footprint: u64,
    pub ri_proc_start_abstime: u64,
    pub ri_proc_exit_abstime: u64,
    pub ri_child_user_time: u64,
    pub ri_child_system_time: u64,
    pub ri_child_pkg_idle_wkups: u64,
    pub ri_child_interrupt_wkups: u64,
    pub ri_child_pageins: u64,
    pub ri_child_elapsed_abstime: u64,
    pub ri_diskio_bytesread: u64,
    pub ri_diskio_byteswritten: u64,
    pub ri_cpu_time_qos: [u64; 7],
    pub ri_billed_system_time: u64,
    pub ri_serviced_system_time: u64,
    pub ri_logical_writes: u64,
    pub ri_lifetime_max_phys_footprint: u64,
    pub ri_instructions: u64,
    pub ri_cycles: u64,
    pub ri_billed_energy: u64,
    pub ri_serviced_energy: u64,
    pub ri_interval_max_phys_footprint: u64,
    pub ri_runnable_time: u64,
    pub ri_flags: u64,
    pub ri_user_ptime: u64,
    pub ri_system_ptime: u64,
    pub ri_pinstructions: u64,
    pub ri_pcycles: u64,
    pub ri_energy_nj: u64,
    pub ri_penergy_nj: u64,
    pub ri_tail: [u64; 14],
}

pub(crate) const RUSAGE_INFO_V6: i32 = 6;
pub(crate) const RUSAGE_V6_LEN: usize = 464;
const _: () = assert!(std::mem::size_of::<RusageInfoV6>() == RUSAGE_V6_LEN);

/// The counters kept between samples, in plain units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Usage {
    pub cpu_ns: u64,
    pub child_cpu_ns: u64,
    pub footprint: u64,
    pub resident: u64,
    pub energy_nj: u64,
    pub disk_read: u64,
    pub disk_written: u64,
    pub wakeups: u64,
}

impl Usage {
    /// `to_ns` converts Mach absolute ticks to nanoseconds.
    pub(crate) fn from_raw(raw: &RusageInfoV6, to_ns: impl Fn(u64) -> u64) -> Self {
        Self {
            cpu_ns: to_ns(raw.ri_user_time.saturating_add(raw.ri_system_time)),
            child_cpu_ns: to_ns(
                raw.ri_child_user_time
                    .saturating_add(raw.ri_child_system_time),
            ),
            footprint: raw.ri_phys_footprint,
            resident: raw.ri_resident_size,
            energy_nj: raw.ri_energy_nj,
            disk_read: raw.ri_diskio_bytesread,
            disk_written: raw.ri_diskio_byteswritten,
            wakeups: raw.ri_pkg_idle_wkups.saturating_add(raw.ri_interrupt_wkups),
        }
    }
}

/// Per-second rates between two samples of the same process run.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct Rates {
    pub energy_w: f32,
    pub disk_read_bps: f64,
    pub disk_write_bps: f64,
    pub wakeups_per_s: f32,
}

/// `None` for a zero interval. A counter that went backwards (a pid reused
/// without the start time changing, or a kernel reset) reads 0, not a
/// wrapped huge rate.
pub(crate) fn rates(prev: &Usage, now: &Usage, interval_ns: u128) -> Option<Rates> {
    if interval_ns == 0 {
        return None;
    }
    let secs = interval_ns as f64 / 1e9;
    let per_s = |a: u64, b: u64| b.saturating_sub(a) as f64 / secs;
    Some(Rates {
        energy_w: (per_s(prev.energy_nj, now.energy_nj) / 1e9) as f32,
        disk_read_bps: per_s(prev.disk_read, now.disk_read),
        disk_write_bps: per_s(prev.disk_written, now.disk_written),
        wakeups_per_s: per_s(prev.wakeups, now.wakeups) as f32,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    fn zeroed() -> RusageInfoV6 {
        // SAFETY: the struct is plain integers; all-zero is a valid value.
        unsafe { std::mem::zeroed() }
    }

    #[test]
    fn v6_layout_matches_the_sdk() {
        assert_eq!(size_of::<RusageInfoV6>(), RUSAGE_V6_LEN);
        assert_eq!(offset_of!(RusageInfoV6, ri_user_time), 16);
        assert_eq!(offset_of!(RusageInfoV6, ri_phys_footprint), 72);
        assert_eq!(offset_of!(RusageInfoV6, ri_diskio_bytesread), 144);
        assert_eq!(offset_of!(RusageInfoV6, ri_instructions), 248);
        assert_eq!(offset_of!(RusageInfoV6, ri_energy_nj), 336);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn v4_prefix_agrees_with_libc() {
        assert_eq!(
            offset_of!(libc::rusage_info_v4, ri_phys_footprint),
            offset_of!(RusageInfoV6, ri_phys_footprint)
        );
        assert_eq!(
            offset_of!(libc::rusage_info_v4, ri_diskio_byteswritten),
            offset_of!(RusageInfoV6, ri_diskio_byteswritten)
        );
        assert_eq!(
            offset_of!(libc::rusage_info_v4, ri_cycles),
            offset_of!(RusageInfoV6, ri_cycles)
        );
    }

    #[test]
    fn usage_converts_ticks_and_sums_wakeups() {
        let mut raw = zeroed();
        raw.ri_user_time = 24;
        raw.ri_system_time = 24;
        raw.ri_child_user_time = 3;
        raw.ri_child_system_time = 3;
        raw.ri_pkg_idle_wkups = 10;
        raw.ri_interrupt_wkups = 5;
        raw.ri_phys_footprint = 7 << 20;
        raw.ri_resident_size = 9 << 20;
        let u = Usage::from_raw(&raw, |t| t * 125 / 3);
        assert_eq!(u.cpu_ns, 2000);
        assert_eq!(u.child_cpu_ns, 250);
        assert_eq!(u.wakeups, 15);
        assert_eq!((u.footprint, u.resident), (7 << 20, 9 << 20));
    }

    #[test]
    fn usage_saturates_instead_of_wrapping() {
        let mut raw = zeroed();
        raw.ri_user_time = u64::MAX;
        raw.ri_system_time = 5;
        raw.ri_pkg_idle_wkups = u64::MAX;
        raw.ri_interrupt_wkups = 1;
        let u = Usage::from_raw(&raw, |t| t);
        assert_eq!((u.cpu_ns, u.wakeups), (u64::MAX, u64::MAX));
    }

    #[test]
    fn rates_over_one_second() {
        let prev = Usage::default();
        let now = Usage {
            energy_nj: 6_000_000_000,
            disk_read: 1 << 20,
            disk_written: 2 << 20,
            wakeups: 150,
            ..Usage::default()
        };
        let r = rates(&prev, &now, 1_000_000_000).unwrap();
        assert!((r.energy_w - 6.0).abs() < 1e-4, "{r:?}");
        assert!((r.disk_read_bps - 1_048_576.0).abs() < 1e-6);
        assert!((r.disk_write_bps - 2_097_152.0).abs() < 1e-6);
        assert!((r.wakeups_per_s - 150.0).abs() < 1e-4);
    }

    #[test]
    fn rates_scale_with_interval_and_reject_zero() {
        let now = Usage {
            energy_nj: 1_000_000_000,
            ..Usage::default()
        };
        let r = rates(&Usage::default(), &now, 500_000_000).unwrap();
        assert!((r.energy_w - 2.0).abs() < 1e-4);
        assert!(rates(&Usage::default(), &now, 0).is_none());
    }

    #[test]
    fn counters_going_backwards_read_zero_not_wrapped() {
        let prev = Usage {
            energy_nj: 10,
            disk_read: 10,
            disk_written: 10,
            wakeups: 10,
            ..Usage::default()
        };
        let r = rates(&prev, &Usage::default(), 1_000_000_000).unwrap();
        assert_eq!(r, Rates::default());
    }

    #[test]
    fn rate_sweep_is_finite_and_non_negative() {
        for i in 0..500_u64 {
            let prev = Usage {
                energy_nj: i * 7919,
                disk_read: i * 104_729,
                wakeups: i,
                ..Usage::default()
            };
            let now = Usage {
                energy_nj: (i * 7919).wrapping_add(i * i * 13),
                disk_read: u64::MAX - i,
                wakeups: i / 2,
                ..Usage::default()
            };
            let interval = u128::from(i + 1) * 1_000_000;
            let r = rates(&prev, &now, interval).unwrap();
            assert!(r.energy_w.is_finite() && r.energy_w >= 0.0);
            assert!(r.disk_read_bps.is_finite() && r.disk_read_bps >= 0.0);
            assert!(r.wakeups_per_s.is_finite() && r.wakeups_per_s >= 0.0);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_own_rusage_has_footprint_and_cpu() {
        let u = super::super::macos::rusage(std::process::id()).unwrap();
        assert!(u.footprint > 0);
        assert!(u.cpu_ns > 0);
        assert!(u.resident > 0);
    }
}
