use siltop_core::ClusterKind;

pub(crate) fn core_kinds(has_super_level: bool) -> Vec<ClusterKind> {
    #[cfg(target_os = "macos")]
    {
        macos::core_kinds(has_super_level)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = has_super_level;
        Vec::new()
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::super::zones::cluster_letter_kind;
    use crate::iokit::{self, CfKey, IoObject};
    use siltop_core::ClusterKind;

    struct Keys {
        name: CfKey,
        cluster_type: CfKey,
        logical_id: CfKey,
    }

    pub(super) fn core_kinds(has_super_level: bool) -> Vec<ClusterKind> {
        let Some(keys) = keys() else {
            return Vec::new();
        };
        let found: Vec<(usize, char)> = iokit::services(c"IOPlatformDevice")
            .iter()
            .filter_map(|service| read_cpu(service, &keys))
            .collect();
        assemble(found, has_super_level)
    }

    fn keys() -> Option<Keys> {
        Some(Keys {
            name: CfKey::new(c"name")?,
            cluster_type: CfKey::new(c"cluster-type")?,
            logical_id: CfKey::new(c"logical-cpu-id")?,
        })
    }

    fn read_cpu(service: &IoObject, keys: &Keys) -> Option<(usize, char)> {
        let name =
            iokit::property(service, &keys.name).and_then(|v| iokit::cf_string(v.borrow()))?;
        if !name.starts_with("cpu") {
            return None;
        }
        let letter = iokit::property(service, &keys.cluster_type)
            .and_then(|v| iokit::cf_string(v.borrow()))?
            .chars()
            .next()?;
        let logical = iokit::property(service, &keys.logical_id)
            .and_then(|v| iokit::cf_i64(v.borrow()))
            .and_then(|n| usize::try_from(n).ok())
            .or_else(|| parse_cpu_index(&name))?;
        Some((logical, letter))
    }

    fn assemble(found: Vec<(usize, char)>, has_super_level: bool) -> Vec<ClusterKind> {
        if found.is_empty() {
            return Vec::new();
        }
        let letters: String = found.iter().map(|(_, c)| c.to_ascii_uppercase()).collect();
        let mut slots: Vec<Option<ClusterKind>> = Vec::new();
        for (index, letter) in found {
            let Some(kind) = cluster_letter_kind(letter, &letters, has_super_level) else {
                return Vec::new();
            };
            if index >= slots.len() {
                slots.resize(index + 1, None);
            }
            if let Some(slot) = slots.get_mut(index) {
                *slot = Some(kind);
            }
        }
        if slots.is_empty() || slots.iter().any(Option::is_none) {
            return Vec::new();
        }
        slots.into_iter().flatten().collect()
    }

    fn parse_cpu_index(name: &str) -> Option<usize> {
        name.strip_prefix("cpu")?.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn live_kinds_cover_every_core_or_none() {
        let kinds = core_kinds(true);
        if kinds.is_empty() {
            return;
        }
        assert!(!kinds.is_empty());
        assert!(kinds.iter().all(|k| ClusterKind::ALL.contains(k)));
    }
}
