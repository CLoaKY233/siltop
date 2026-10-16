use super::ClusterKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocInfo {
    pub name: String,
    pub e_cores: u8,
    pub p_cores: u8,
    pub s_cores: u8,
    pub gpu_cores: u8,
    pub memory_bytes: u64,
    /// Mach index → cluster, from `IORegistry` `cluster-type`. Empty if unknown.
    pub core_kinds: Vec<ClusterKind>,
}

impl Default for SocInfo {
    fn default() -> Self {
        Self {
            name: String::from("unknown"),
            e_cores: 0,
            p_cores: 0,
            s_cores: 0,
            gpu_cores: 0,
            memory_bytes: 0,
            core_kinds: Vec::new(),
        }
    }
}

impl SocInfo {
    /// Cluster and local index for Mach core `mach_index`.
    /// Prefers `core_kinds`. Falls back to E then P then S when counts cover `total`.
    #[must_use]
    pub fn role_for(&self, mach_index: usize, total: usize) -> Option<(ClusterKind, u16)> {
        role_for_core(
            &self.core_kinds,
            self.e_cores,
            self.p_cores,
            self.s_cores,
            mach_index,
            total,
        )
    }
}

/// Apple Silicon Mach order when `IORegistry` is missing: E, then P, then S.
#[must_use]
pub fn linear_core_role(
    e_cores: u8,
    p_cores: u8,
    s_cores: u8,
    mach_index: usize,
    total: usize,
) -> Option<(ClusterKind, u16)> {
    let e = usize::from(e_cores);
    let p = usize::from(p_cores);
    let s = usize::from(s_cores);
    if e + p + s != total || mach_index >= total {
        return None;
    }
    if mach_index < e {
        Some((ClusterKind::Efficiency, mach_index as u16))
    } else if mach_index < e + p {
        Some((ClusterKind::Performance, (mach_index - e) as u16))
    } else {
        Some((ClusterKind::Super, (mach_index - e - p) as u16))
    }
}

#[must_use]
pub fn role_for_core(
    core_kinds: &[ClusterKind],
    e_cores: u8,
    p_cores: u8,
    s_cores: u8,
    mach_index: usize,
    total: usize,
) -> Option<(ClusterKind, u16)> {
    if total == 0 || mach_index >= total {
        return None;
    }
    if core_kinds.len() == total {
        let kind = *core_kinds.get(mach_index)?;
        let index = core_kinds
            .iter()
            .take(mach_index)
            .filter(|k| **k == kind)
            .count() as u16;
        return Some((kind, index));
    }
    linear_core_role(e_cores, p_cores, s_cores, mach_index, total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_roles_are_e_then_p_then_s() {
        assert_eq!(
            linear_core_role(4, 8, 0, 0, 12),
            Some((ClusterKind::Efficiency, 0))
        );
        assert_eq!(
            linear_core_role(4, 8, 0, 3, 12),
            Some((ClusterKind::Efficiency, 3))
        );
        assert_eq!(
            linear_core_role(4, 8, 0, 4, 12),
            Some((ClusterKind::Performance, 0))
        );
        assert_eq!(
            linear_core_role(0, 12, 6, 11, 18),
            Some((ClusterKind::Performance, 11))
        );
        assert_eq!(
            linear_core_role(0, 12, 6, 12, 18),
            Some((ClusterKind::Super, 0))
        );
        assert_eq!(linear_core_role(4, 8, 0, 0, 10), None);
    }

    #[test]
    fn registry_kinds_win_over_linear() {
        let kinds = vec![
            ClusterKind::Performance,
            ClusterKind::Performance,
            ClusterKind::Super,
        ];
        assert_eq!(
            role_for_core(&kinds, 4, 8, 0, 2, 3),
            Some((ClusterKind::Super, 0))
        );
        assert_eq!(
            role_for_core(&kinds, 4, 8, 0, 1, 3),
            Some((ClusterKind::Performance, 1))
        );
    }
}
