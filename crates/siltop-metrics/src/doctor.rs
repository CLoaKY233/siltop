//! Data for `siltop doctor`: what this Mac exposes, as plain values.
//! Nothing here is on the sampling path; each call opens and closes its own
//! clients, so never call it while a [`crate::Sampler`] is alive.

use std::collections::BTreeMap;

/// One SMC key and its four-character data type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmcKey {
    pub name: String,
    pub data_type: String,
    pub size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SmcInventory {
    /// Every key the SMC enumerated.
    pub keys: Vec<SmcKey>,
}

impl SmcInventory {
    /// Key count per data type, most common first, then by name.
    #[must_use]
    pub fn type_histogram(&self) -> Vec<(String, usize)> {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for key in &self.keys {
            *counts.entry(key.data_type.as_str()).or_default() += 1;
        }
        let mut out: Vec<(String, usize)> = counts
            .into_iter()
            .map(|(name, n)| (name.to_owned(), n))
            .collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }

    /// Keys whose name starts with `prefix` (e.g. `"T"` for temperatures).
    #[must_use]
    pub fn with_prefix(&self, prefix: &str) -> usize {
        self.keys
            .iter()
            .filter(|k| k.name.starts_with(prefix))
            .count()
    }
}

/// How many processes this user can read full task info for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProcReadability {
    pub total: usize,
    pub readable: usize,
}

impl ProcReadability {
    #[must_use]
    pub const fn denied(self) -> usize {
        self.total.saturating_sub(self.readable)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Diagnostics {
    pub model: Option<String>,
    pub brand: Option<String>,
    pub os_version: Option<String>,
    pub os_build: Option<String>,
    /// A curated SMC temperature table exists for this model or chip.
    pub chip_table: bool,
    /// `None` when the SMC could not be opened.
    pub smc: Option<SmcInventory>,
    /// HID temperature service product names with their counts.
    pub hid_products: Vec<(String, usize)>,
    pub procs: ProcReadability,
}

/// Count names, sorted by name.
#[must_use]
pub(crate) fn tally(names: impl IntoIterator<Item = String>) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for name in names {
        let name = if name.is_empty() {
            String::from("(unnamed)")
        } else {
            name
        };
        *counts.entry(name).or_default() += 1;
    }
    counts.into_iter().collect()
}

/// Everything `doctor` reports. Off macOS every field is absent.
#[must_use]
pub fn diagnostics() -> Diagnostics {
    #[cfg(target_os = "macos")]
    {
        macos::diagnostics()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Diagnostics::default()
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::mem::{MaybeUninit, size_of};

    use super::{Diagnostics, ProcReadability, SmcInventory, SmcKey, tally};
    use crate::iokit::{self, CfKey};
    use crate::smc::{Client, key_name};
    use crate::{smc_keys, sys};

    pub(super) fn diagnostics() -> Diagnostics {
        let model = sys::sysctl_string(c"hw.model");
        let brand = sys::sysctl_string(c"machdep.cpu.brand_string");
        let chip_table = smc_keys::chip_for(
            model.as_deref().unwrap_or(""),
            brand.as_deref().unwrap_or(""),
        )
        .is_some();
        Diagnostics {
            os_version: sys::sysctl_string(c"kern.osproductversion"),
            os_build: sys::sysctl_string(c"kern.osversion"),
            model,
            brand,
            chip_table,
            smc: smc_inventory(),
            hid_products: hid_products(),
            procs: proc_readability(),
        }
    }

    fn smc_inventory() -> Option<SmcInventory> {
        let mut client = Client::open()?;
        let names = client.all_keys();
        let keys = names
            .into_iter()
            .map(|key| {
                let info = client.info(key);
                SmcKey {
                    name: key_name(key),
                    data_type: info.map_or_else(|| String::from("?"), |i| key_name(i.data_type)),
                    size: info.map_or(0, |i| i.size),
                }
            })
            .collect();
        Some(SmcInventory { keys })
    }

    fn hid_products() -> Vec<(String, usize)> {
        let Some(matching) =
            iokit::cf_dict_numbers(&[(c"PrimaryUsagePage", 0xff00), (c"PrimaryUsage", 0x0005)])
        else {
            return Vec::new();
        };
        let Some(client) = iokit::hid_client(&matching) else {
            return Vec::new();
        };
        let (Some(services), Some(product)) =
            (iokit::hid_services(&client), CfKey::new(c"Product"))
        else {
            return Vec::new();
        };
        tally(
            iokit::cf_array_items(services.borrow())
                .into_iter()
                .map(|service| iokit::hid_property(service, &product).unwrap_or_default()),
        )
    }

    fn proc_readability() -> ProcReadability {
        let mut pids = vec![0 as libc::c_int; 8192];
        let bytes = (pids.len() * size_of::<libc::c_int>()) as libc::c_int;
        let n = unsafe {
            // SAFETY: `pids` has room for `bytes` bytes of pids.
            libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes)
        };
        let Ok(n) = usize::try_from(n) else {
            return ProcReadability::default();
        };
        pids.truncate(n.min(pids.len()));
        let want = size_of::<libc::proc_taskinfo>() as libc::c_int;
        let readable = pids
            .iter()
            .filter(|pid| **pid > 0)
            .filter(|pid| {
                let mut info = MaybeUninit::<libc::proc_taskinfo>::uninit();
                let got = unsafe {
                    // SAFETY: PROC_PIDTASKINFO writes at most `want` bytes into `info`.
                    libc::proc_pidinfo(
                        **pid,
                        libc::PROC_PIDTASKINFO,
                        0,
                        info.as_mut_ptr().cast(),
                        want,
                    )
                };
                got == want
            })
            .count();
        ProcReadability {
            total: pids.iter().filter(|pid| **pid > 0).count(),
            readable,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn key(name: &str, data_type: &str) -> SmcKey {
        SmcKey {
            name: String::from(name),
            data_type: String::from(data_type),
            size: 4,
        }
    }

    #[test]
    fn histogram_orders_by_count_then_name() {
        let inv = SmcInventory {
            keys: vec![
                key("Tp01", "flt"),
                key("Tp02", "flt"),
                key("F0Ac", "fpe2"),
                key("#KEY", "ui32"),
                key("Tg0D", "flt"),
                key("B0AC", "si16"),
            ],
        };
        assert_eq!(
            inv.type_histogram(),
            vec![
                (String::from("flt"), 3),
                (String::from("fpe2"), 1),
                (String::from("si16"), 1),
                (String::from("ui32"), 1),
            ]
        );
        assert_eq!(inv.with_prefix("T"), 3);
        assert_eq!(inv.with_prefix("Z"), 0);
    }

    #[test]
    fn empty_inventory_is_empty_histogram() {
        assert!(SmcInventory::default().type_histogram().is_empty());
    }

    #[test]
    fn tally_counts_and_names_the_unnamed() {
        let got = tally(
            ["PMU tdie1", "", "PMU tdie1", "NAND CH0 temp"]
                .into_iter()
                .map(String::from),
        );
        assert_eq!(
            got,
            vec![
                (String::from("(unnamed)"), 1),
                (String::from("NAND CH0 temp"), 1),
                (String::from("PMU tdie1"), 2),
            ]
        );
    }

    #[test]
    fn denied_never_underflows() {
        let r = ProcReadability {
            total: 3,
            readable: 5,
        };
        assert_eq!(r.denied(), 0);
        let r = ProcReadability {
            total: 700,
            readable: 420,
        };
        assert_eq!(r.denied(), 280);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn stub_reports_absent_not_zero() {
        let d = diagnostics();
        assert!(d.model.is_none());
        assert!(d.smc.is_none());
    }
}
