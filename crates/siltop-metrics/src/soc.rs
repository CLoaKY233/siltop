use siltop_core::SocInfo;

pub(crate) fn info() -> SocInfo {
    #[cfg(target_os = "macos")]
    {
        macos::info()
    }
    #[cfg(not(target_os = "macos"))]
    {
        SocInfo::default()
    }
}

/// Map unnamed `hw.nperflevels` counts onto E/P/S. Lower N is faster.
/// Named levels (`Super` / `Performance` / `Efficiency`) win when present.
pub(crate) fn cluster_counts(nlevels: u32, level_cpus: [u8; 3]) -> (u8, u8, u8) {
    match nlevels {
        3 => (level_cpus[2], level_cpus[1], level_cpus[0]),
        2 => (level_cpus[1], level_cpus[0], 0),
        1 => (0, level_cpus[0], 0),
        _ => (0, 0, 0),
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::cluster_counts;
    use crate::iokit::{self, CfKey};
    use crate::sys;
    use siltop_core::SocInfo;

    pub(super) fn info() -> SocInfo {
        let name = sys::sysctl_string(c"machdep.cpu.brand_string")
            .or_else(|| sys::sysctl_string(c"hw.model"))
            .unwrap_or_else(fallback_name);
        let memory_bytes = sys::sysctl_u64(c"hw.memsize").unwrap_or(0);
        let nlevels = sys::sysctl_u64(c"hw.nperflevels")
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0);
        let named = named_levels(nlevels);
        let (e_cores, mut p_cores, s_cores) = if named.is_empty() {
            cluster_counts(
                nlevels,
                [
                    logical_level(c"hw.perflevel0.logicalcpu"),
                    logical_level(c"hw.perflevel1.logicalcpu"),
                    logical_level(c"hw.perflevel2.logicalcpu"),
                ],
            )
        } else {
            crate::zones::counts_from_levels(&named)
        };
        if e_cores == 0 && p_cores == 0 && s_cores == 0 {
            let phys = sys::sysctl_u64(c"hw.physicalcpu")
                .or_else(|| sys::sysctl_u64(c"hw.logicalcpu"))
                .unwrap_or(0);
            p_cores = u8::try_from(phys.min(u64::from(u8::MAX))).unwrap_or(u8::MAX);
        }
        if e_cores == 0 && p_cores == 0 && s_cores == 0 {
            p_cores = u8::try_from(sys::logical_cpus().min(u32::from(u8::MAX))).unwrap_or(1);
        }
        let core_kinds = crate::topology::core_kinds(s_cores > 0);
        SocInfo {
            name,
            e_cores,
            p_cores,
            s_cores,
            gpu_cores: gpu_core_count().unwrap_or(0),
            memory_bytes,
            core_kinds,
        }
    }

    fn named_levels(nlevels: u32) -> Vec<(siltop_core::ClusterKind, u8)> {
        const KEYS: [(&std::ffi::CStr, &std::ffi::CStr); 3] = [
            (c"hw.perflevel0.name", c"hw.perflevel0.logicalcpu"),
            (c"hw.perflevel1.name", c"hw.perflevel1.logicalcpu"),
            (c"hw.perflevel2.name", c"hw.perflevel2.logicalcpu"),
        ];
        let mut out = Vec::new();
        for (i, (name_key, count_key)) in KEYS.iter().enumerate() {
            if i >= nlevels as usize {
                break;
            }
            let Some(kind) =
                sys::sysctl_string(name_key).and_then(|n| crate::zones::kind_from_level_name(&n))
            else {
                return Vec::new();
            };
            out.push((kind, logical_level(count_key)));
        }
        out
    }

    fn logical_level(name: &std::ffi::CStr) -> u8 {
        sys::sysctl_u64(name)
            .and_then(|n| u8::try_from(n).ok())
            .unwrap_or(0)
    }

    /// `gpu-core-count` on the AGX accelerator service (20 on an M5 Pro).
    fn gpu_core_count() -> Option<u8> {
        let key = CfKey::new(c"gpu-core-count")?;
        [c"AGXAccelerator", c"IOAccelerator"]
            .into_iter()
            .flat_map(iokit::services)
            .find_map(|service| {
                let value = iokit::property(&service, &key)?;
                iokit::cf_i64(value.borrow())
            })
            .and_then(|n| u8::try_from(n).ok())
            .filter(|n| *n > 0)
    }

    fn fallback_name() -> String {
        if sys::sysctl_u64(c"hw.optional.arm64") == Some(1) {
            String::from("Apple Silicon")
        } else {
            String::from("Mac")
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn cluster_counts_two_and_three_level() {
        assert_eq!(cluster_counts(2, [6, 4, 0]), (4, 6, 0));
        assert_eq!(cluster_counts(3, [2, 6, 4]), (4, 6, 2));
        assert_eq!(cluster_counts(1, [8, 0, 0]), (0, 8, 0));
        assert_eq!(cluster_counts(0, [0, 0, 0]), (0, 0, 0));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_soc_is_named() {
        let soc = info();
        assert!(!soc.name.is_empty());
        assert_ne!(soc.name, "unknown");
        assert!(soc.memory_bytes > 0);
        if crate::sys::sysctl_string(c"hw.model").as_deref() == Some("Mac17,8") {
            assert_eq!(soc.gpu_cores, 20, "M5 Pro test machine");
        }
        let total = u16::from(soc.e_cores) + u16::from(soc.p_cores) + u16::from(soc.s_cores);
        assert!(total > 0, "no cores");
        if !soc.core_kinds.is_empty() {
            assert_eq!(soc.core_kinds.len(), usize::from(total));
            for kind in siltop_core::ClusterKind::ALL {
                let mapped = soc.core_kinds.iter().filter(|k| **k == kind).count();
                let counted = match kind {
                    siltop_core::ClusterKind::Efficiency => usize::from(soc.e_cores),
                    siltop_core::ClusterKind::Performance => usize::from(soc.p_cores),
                    siltop_core::ClusterKind::Super => usize::from(soc.s_cores),
                };
                assert_eq!(mapped, counted, "{kind:?} kinds={mapped} counts={counted}");
            }
        }
    }
}
