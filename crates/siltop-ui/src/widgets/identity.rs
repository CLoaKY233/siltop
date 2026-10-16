//! The machine's identity: chip, core split, GPU cores and memory. It has
//! one home per screen, the expanded CPU view's meta column (and settings →
//! about), so no panel repeats it.

use siltop_core::Snapshot;

/// `SocInfo`'s placeholder before the chip is read.
const UNKNOWN_CHIP: &str = "unknown";

/// `8P+4E`, `6S+12P`: largest cluster kind first, absent kinds skipped.
#[must_use]
pub(crate) fn core_counts(snap: &Snapshot) -> Option<String> {
    let soc = &snap.soc;
    let parts: Vec<String> = [(soc.s_cores, 'S'), (soc.p_cores, 'P'), (soc.e_cores, 'E')]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, kind)| format!("{n}{kind}"))
        .collect();
    (!parts.is_empty()).then(|| parts.join("+"))
}

/// Labelled identity facts, absent ones left out: never a fake 0.
#[must_use]
pub(crate) fn facts(snap: &Snapshot) -> Vec<(&'static str, String)> {
    let soc = &snap.soc;
    let gib = soc.memory_bytes >> 30;
    [
        (!soc.name.is_empty() && soc.name != UNKNOWN_CHIP).then(|| ("chip", soc.name.clone())),
        core_counts(snap).map(|cores| ("cores", cores)),
        (soc.gpu_cores > 0).then(|| ("gpu", format!("{}-core GPU", soc.gpu_cores))),
        (gib > 0).then(|| ("memory", format!("{gib} GB"))),
    ]
    .into_iter()
    .flatten()
    .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn facts_skip_absent_hardware_instead_of_printing_zero() {
        let snap = Snapshot::empty();
        assert!(facts(&snap).is_empty(), "{:?}", facts(&snap));
    }

    #[test]
    fn facts_name_every_part_in_order() {
        let mut snap = Snapshot::empty();
        snap.soc.name = String::from("Apple M5 Pro");
        snap.soc.s_cores = 6;
        snap.soc.p_cores = 12;
        snap.soc.gpu_cores = 20;
        snap.soc.memory_bytes = 24 << 30;
        let values: Vec<String> = facts(&snap).into_iter().map(|(_, v)| v).collect();
        assert_eq!(values, ["Apple M5 Pro", "6S+12P", "20-core GPU", "24 GB"]);
    }
}
