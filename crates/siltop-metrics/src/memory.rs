use std::time::Instant;

use siltop_core::{CollectorId, MemorySnapshot, Pressure, Result, Snapshot};

use crate::collector::{Collector, Outcome};

/// Memory sampler; keeps the previous swap counters for per-second rates.
#[derive(Default)]
pub(crate) struct MemoryCollector {
    prev_swap: Option<(Instant, SwapCounts)>,
}

impl Collector for MemoryCollector {
    fn id(&self) -> CollectorId {
        CollectorId::Memory
    }

    /// Always fresh: the levels are instantaneous, and only the swap rates
    /// wait for a baseline (they stay `None` until then).
    fn collect(&mut self, now: Instant, snap: &mut Snapshot) -> Result<Outcome> {
        if cfg!(not(target_os = "macos")) {
            return Ok(Outcome::Absent);
        }
        snap.memory = self.sample(now)?;
        Ok(Outcome::Fresh)
    }
}

impl MemoryCollector {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn sample(&mut self, now: Instant) -> Result<MemorySnapshot> {
        #[cfg(target_os = "macos")]
        {
            let (mut snap, counts) = macos::sample()?;
            let prev = self
                .prev_swap
                .map(|(t, c)| (now.saturating_duration_since(t).as_secs_f64(), c));
            (snap.swap_in_bps, snap.swap_out_bps) =
                swap_rates(prev, counts, crate::sys::page_size());
            self.prev_swap = Some((now, counts));
            Ok(snap)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = now;
            self.prev_swap = None;
            Ok(MemorySnapshot::default())
        }
    }
}

/// Cumulative pages swapped in and out (`vm_statistics64.swapins/swapouts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SwapCounts {
    pub ins: u64,
    pub outs: u64,
}

/// Swap in/out bytes per second from two cumulative page counts `dt`
/// seconds apart. `None` on the first sample or a non-positive interval;
/// a counter that goes backwards reads 0, never a spike.
pub(crate) fn swap_rates(
    prev: Option<(f64, SwapCounts)>,
    cur: SwapCounts,
    page: u64,
) -> (Option<u64>, Option<u64>) {
    let Some((dt, p)) = prev else {
        return (None, None);
    };
    if !dt.is_finite() || dt <= 0.0 {
        return (None, None);
    }
    let rate = |a: u64, b: u64| {
        let bytes = b.saturating_sub(a).saturating_mul(page);
        let r = bytes as f64 / dt;
        if r.is_finite() { r as u64 } else { 0 }
    };
    (Some(rate(p.ins, cur.ins)), Some(rate(p.outs, cur.outs)))
}

/// Cached (file-backed + purgeable) bytes, as Activity Monitor's "Cached
/// Files" (D36).
pub(crate) fn cache_bytes(external: u64, purgeable: u64, page_size: u64) -> u64 {
    external.saturating_add(purgeable).saturating_mul(page_size)
}

/// `kern.memorystatus_level` is a 0–100 free percentage; anything else is
/// treated as unavailable.
pub(crate) fn free_percent(level: Option<i32>) -> Option<u8> {
    level
        .filter(|l| (0..=100).contains(l))
        .and_then(|l| u8::try_from(l).ok())
}

#[derive(Clone, Copy)]
pub(crate) struct VmPages {
    pub active: u64,
    pub inactive: u64,
    pub wire: u64,
    pub speculative: u64,
    pub compressor: u64,
    pub purgeable: u64,
    pub external: u64,
}

/// Activity Monitor–style used pages × page size.
pub(crate) fn used_bytes(pages: VmPages, page_size: u64) -> u64 {
    let count = pages
        .active
        .saturating_add(pages.inactive)
        .saturating_add(pages.wire)
        .saturating_add(pages.speculative)
        .saturating_add(pages.compressor)
        .saturating_sub(pages.purgeable)
        .saturating_sub(pages.external);
    count.saturating_mul(page_size)
}

/// `kern.memorystatus_vm_pressure_level`: 1=Nominal, 2=Warn, 4=Critical.
pub(crate) fn pressure_from_level(level: Option<i32>) -> Pressure {
    match level {
        Some(2) => Pressure::Warn,
        Some(4) => Pressure::Critical,
        _ => Pressure::Nominal,
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{SwapCounts, cache_bytes, free_percent, pressure_from_level, used_bytes};
    use crate::sys;
    use siltop_core::{Error, MemorySnapshot, Result};
    use std::mem::{MaybeUninit, size_of};

    pub(super) fn sample() -> Result<(MemorySnapshot, SwapCounts)> {
        let page = sys::page_size();
        let total_bytes = sys::sysctl_u64(c"hw.memsize").unwrap_or(0);
        let vm = vm_stats()?;
        let pages = |n: libc::natural_t| u64::from(n);
        let used_bytes = used_bytes(
            super::VmPages {
                active: pages(vm.active_count),
                inactive: pages(vm.inactive_count),
                wire: pages(vm.wire_count),
                speculative: pages(vm.speculative_count),
                compressor: pages(vm.compressor_page_count),
                purgeable: pages(vm.purgeable_count),
                external: pages(vm.external_page_count),
            },
            page,
        );
        let (swap_used_bytes, swap_total_bytes) = swap_usage();
        let counts = SwapCounts {
            ins: vm.swapins,
            outs: vm.swapouts,
        };
        let snap = MemorySnapshot {
            used_bytes,
            total_bytes,
            wired_bytes: pages(vm.wire_count).saturating_mul(page),
            compressed_bytes: pages(vm.compressor_page_count).saturating_mul(page),
            cache_bytes: cache_bytes(
                pages(vm.external_page_count),
                pages(vm.purgeable_count),
                page,
            ),
            swap_used_bytes,
            swap_total_bytes,
            pressure: pressure_from_level(sys::sysctl_i32(c"kern.memorystatus_vm_pressure_level")),
            swap_in_bps: None,
            swap_out_bps: None,
            free_percent: free_percent(sys::sysctl_i32(c"kern.memorystatus_level")),
        };
        Ok((snap, counts))
    }

    fn vm_stats() -> Result<libc::vm_statistics64> {
        let mut vm = MaybeUninit::<libc::vm_statistics64>::zeroed();
        let mut count = libc::HOST_VM_INFO64_COUNT;
        #[allow(deprecated)]
        let kr = unsafe {
            // SAFETY: `vm` is a valid HOST_VM_INFO64 buffer; count is the libc slot count.
            libc::host_statistics64(
                libc::mach_host_self(),
                libc::HOST_VM_INFO64,
                vm.as_mut_ptr().cast(),
                &raw mut count,
            )
        };
        if kr != libc::KERN_SUCCESS {
            return Err(Error::system(format!("host_statistics64: {kr}")));
        }
        Ok(unsafe {
            // SAFETY: kernel wrote a vm_statistics64 on KERN_SUCCESS.
            vm.assume_init()
        })
    }

    fn swap_usage() -> (u64, u64) {
        let mut mib = [libc::CTL_VM, libc::VM_SWAPUSAGE];
        let mut xsw = MaybeUninit::<libc::xsw_usage>::zeroed();
        let mut len = size_of::<libc::xsw_usage>();
        let rc = unsafe {
            // SAFETY: MIB is VM_SWAPUSAGE; `xsw` is the documented xsw_usage out buffer.
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as libc::c_uint,
                xsw.as_mut_ptr().cast(),
                &raw mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return (0, 0);
        }
        let xsw = unsafe {
            // SAFETY: sysctl succeeded and wrote xsw_usage.
            xsw.assume_init()
        };
        (xsw.xsu_used, xsw.xsu_total)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn used_bytes_matches_research_formula() {
        let pages = VmPages {
            active: 10,
            inactive: 20,
            wire: 5,
            speculative: 2,
            compressor: 8,
            purgeable: 3,
            external: 4,
        };
        assert_eq!(used_bytes(pages, 4096), 38 * 4096);
    }

    #[test]
    fn used_bytes_saturates_when_subtracting() {
        let pages = VmPages {
            active: 1,
            inactive: 1,
            wire: 1,
            speculative: 0,
            compressor: 0,
            purgeable: 10,
            external: 10,
        };
        assert_eq!(used_bytes(pages, 4096), 0);
    }

    #[test]
    fn pressure_mapping() {
        assert_eq!(pressure_from_level(None), Pressure::Nominal);
        assert_eq!(pressure_from_level(Some(1)), Pressure::Nominal);
        assert_eq!(pressure_from_level(Some(2)), Pressure::Warn);
        assert_eq!(pressure_from_level(Some(4)), Pressure::Critical);
        assert_eq!(pressure_from_level(Some(99)), Pressure::Nominal);
    }

    #[test]
    fn d36_cache_includes_purgeable() {
        assert_eq!(cache_bytes(4, 3, 4096), 7 * 4096);
        assert_eq!(cache_bytes(0, 0, 16384), 0);
        assert_eq!(cache_bytes(u64::MAX, 1, 16384), u64::MAX);
    }

    #[test]
    fn free_percent_table() {
        let table = [
            (None, None),
            (Some(-1), None),
            (Some(0), Some(0)),
            (Some(72), Some(72)),
            (Some(100), Some(100)),
            (Some(101), None),
            (Some(i32::MAX), None),
            (Some(i32::MIN), None),
        ];
        for (level, want) in table {
            assert_eq!(free_percent(level), want, "{level:?}");
        }
    }

    #[test]
    fn swap_rates_first_sample_is_absent() {
        let cur = SwapCounts { ins: 10, outs: 20 };
        assert_eq!(swap_rates(None, cur, 16384), (None, None));
    }

    #[test]
    fn swap_rates_per_second_bytes() {
        let prev = SwapCounts { ins: 100, outs: 50 };
        let cur = SwapCounts { ins: 110, outs: 50 };
        assert_eq!(
            swap_rates(Some((0.5, prev)), cur, 16384),
            (Some(10 * 16384 * 2), Some(0))
        );
    }

    #[test]
    fn swap_rates_reset_and_bad_dt() {
        let prev = SwapCounts {
            ins: 1_000,
            outs: 1_000,
        };
        let cur = SwapCounts { ins: 5, outs: 5 };
        assert_eq!(
            swap_rates(Some((1.0, prev)), cur, 16384),
            (Some(0), Some(0))
        );
        for dt in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(swap_rates(Some((dt, prev)), cur, 16384), (None, None));
        }
    }

    #[test]
    fn swap_rates_sweep_is_monotone_in_delta() {
        let prev = SwapCounts::default();
        let mut last = 0;
        for n in 0..300_u64 {
            let cur = SwapCounts { ins: n, outs: n };
            let (i, o) = swap_rates(Some((1.0, prev)), cur, 16384);
            let i = i.unwrap();
            assert_eq!(Some(i), o);
            assert_eq!(i, n * 16384);
            assert!(i >= last);
            last = i;
        }
    }

    #[test]
    fn swap_rates_saturate_on_huge_counts() {
        let prev = SwapCounts::default();
        let cur = SwapCounts {
            ins: u64::MAX,
            outs: u64::MAX,
        };
        let (i, _) = swap_rates(Some((1e-9, prev)), cur, 16384);
        assert!(i.is_some());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_free_percent_and_swap_rates() {
        let mut col = MemoryCollector::new();
        let t0 = Instant::now();
        let first = col.sample(t0).expect("memory");
        assert_eq!(first.swap_in_bps, None);
        let pct = first.free_percent.expect("kern.memorystatus_level");
        assert!(pct <= 100);
        assert!(first.cache_bytes <= first.total_bytes);
        let second = col
            .sample(t0 + std::time::Duration::from_secs(1))
            .expect("memory");
        assert!(second.swap_in_bps.is_some() && second.swap_out_bps.is_some());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_free_percent_within_two_points_of_memory_pressure() {
        let Ok(out) = std::process::Command::new("memory_pressure").output() else {
            return;
        };
        let ours = MemoryCollector::new()
            .sample(Instant::now())
            .expect("memory")
            .free_percent;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let theirs: Option<i32> = text
            .lines()
            .find(|l| l.contains("free percentage"))
            .and_then(|l| l.rsplit(':').next())
            .and_then(|v| v.trim().trim_end_matches('%').parse().ok());
        if let (Some(ours), Some(theirs)) = (ours, theirs) {
            assert!((i32::from(ours) - theirs).abs() <= 2, "{ours} vs {theirs}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_memory_totals() {
        let mem = MemoryCollector::new()
            .sample(Instant::now())
            .expect("memory");
        assert!(mem.total_bytes > 0, "hw.memsize");
        assert!(mem.used_bytes > 0, "used pages");
        assert!(mem.used_bytes <= mem.total_bytes.saturating_mul(2));
    }
}
