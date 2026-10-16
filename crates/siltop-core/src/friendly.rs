//! Plain-English names for the macOS daemons people ask about most. The
//! process table shows them dim after the real name, so `mds_stores` reads
//! as "Spotlight indexing" without hiding what the kernel calls it.

/// Sorted by process name so lookups can binary-search.
const FRIENDLY: &[(&str, &str)] = &[
    ("WindowServer", "Window server"),
    ("backupd", "Time Machine"),
    ("bird", "iCloud Drive sync"),
    ("cloudd", "iCloud sync"),
    ("coreaudiod", "Audio"),
    ("coreduetd", "Usage predictions"),
    ("corespotlightd", "Spotlight"),
    ("fileproviderd", "File provider sync"),
    ("kernel_task", "Kernel"),
    ("launchd", "Service launcher"),
    ("locationd", "Location services"),
    ("logd", "System log"),
    ("mDNSResponder", "Bonjour / DNS"),
    ("mds", "Spotlight"),
    ("mds_stores", "Spotlight indexing"),
    ("mdworker", "Spotlight importer"),
    ("mdworker_shared", "Spotlight importer"),
    ("mediaanalysisd", "Photos analysis"),
    ("nsurlsessiond", "Background downloads"),
    ("photoanalysisd", "Photos analysis"),
    ("photolibraryd", "Photos library"),
    ("powerd", "Power management"),
    ("softwareupdated", "Software update"),
    ("syspolicyd", "Gatekeeper"),
    ("trustd", "Certificate checks"),
];

/// The friendly name for a process, when it is a well-known daemon.
#[must_use]
pub fn friendly_name(name: &str) -> Option<&'static str> {
    FRIENDLY
        .binary_search_by(|(key, _)| (*key).cmp(name))
        .ok()
        .and_then(|i| FRIENDLY.get(i))
        .map(|(_, friendly)| *friendly)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_unique_for_binary_search() {
        for pair in FRIENDLY.windows(2) {
            assert!(pair[0].0 < pair[1].0, "{} before {}", pair[0].0, pair[1].0);
        }
    }

    #[test]
    fn every_entry_is_found() {
        for (name, friendly) in FRIENDLY {
            assert_eq!(friendly_name(name), Some(*friendly), "{name}");
        }
    }

    #[test]
    fn common_daemons_read_plainly_and_unknowns_stay_silent() {
        assert_eq!(friendly_name("mds_stores"), Some("Spotlight indexing"));
        assert_eq!(friendly_name("WindowServer"), Some("Window server"));
        assert_eq!(friendly_name("kernel_task"), Some("Kernel"));
        assert_eq!(friendly_name("windowserver"), None, "exact match only");
        assert_eq!(friendly_name("Xcode"), None);
        assert_eq!(friendly_name(""), None);
    }
}
