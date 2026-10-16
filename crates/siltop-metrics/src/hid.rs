//! HID temperature services, used only for zones the SMC does not cover.
//!
//! The first pass reads every service except the dead and constant ones
//! (`zones::hid_skipped`) and keeps those that returned a temperature; later
//! passes read only the kept services for the zones still wanted, and the
//! full pass is repeated every [`REPROBE`].

use std::time::{Duration, Instant};

use crate::zones::{Plausibility, Tagged, TempZone};

pub(crate) const REPROBE: Duration = Duration::from_secs(60);

pub(crate) struct HidSensors {
    #[cfg(target_os = "macos")]
    inner: Option<macos::Client>,
    probed_at: Option<Instant>,
}

impl HidSensors {
    pub(crate) fn new() -> Self {
        Self {
            #[cfg(target_os = "macos")]
            inner: None,
            probed_at: None,
        }
    }

    /// Readings for the `wanted` zones. With nothing wanted, HID is never
    /// touched.
    pub(crate) fn sample(
        &mut self,
        wanted: &[TempZone],
        now: Instant,
        filter: &mut Plausibility,
    ) -> Vec<Tagged> {
        if wanted.is_empty() {
            return Vec::new();
        }
        let reprobe = self
            .probed_at
            .is_none_or(|t| now.saturating_duration_since(t) >= REPROBE);
        if reprobe {
            self.probed_at = Some(now);
            filter.forget_hid();
        }
        #[cfg(target_os = "macos")]
        {
            if self.inner.is_none() {
                self.inner = macos::Client::open();
            }
            let Some(client) = self.inner.as_mut() else {
                return Vec::new();
            };
            if reprobe {
                client.probe();
            }
            client.read(wanted, filter)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = reprobe;
            Vec::new()
        }
    }

    #[cfg(test)]
    pub(crate) fn kept(&self) -> usize {
        #[cfg(target_os = "macos")]
        {
            self.inner.as_ref().map_or(0, macos::Client::kept)
        }
        #[cfg(not(target_os = "macos"))]
        {
            0
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use crate::iokit::{self, CfKey, CfOwned};
    use crate::zones::{self, Plausibility, SensorId, Tagged, TempZone};

    struct Kept {
        service: CfOwned,
        name: String,
        zone: TempZone,
    }

    pub(super) struct Client {
        system: CfOwned,
        product: CfKey,
        kept: Vec<Kept>,
    }

    impl Client {
        pub(super) fn open() -> Option<Self> {
            let matching = iokit::cf_dict_numbers(&[
                (c"PrimaryUsagePage", 0xff00),
                (c"PrimaryUsage", 0x0005),
            ])?;
            Some(Self {
                system: iokit::hid_client(&matching)?,
                product: CfKey::new(c"Product")?,
                kept: Vec::new(),
            })
        }

        #[cfg(test)]
        pub(super) fn kept(&self) -> usize {
            self.kept.len()
        }

        /// Reads every live service once and keeps those with a temperature.
        pub(super) fn probe(&mut self) {
            self.kept.clear();
            let Some(services) = iokit::hid_services(&self.system) else {
                return;
            };
            let mut named = Vec::new();
            for service in iokit::cf_array_items(services.borrow()) {
                let name = iokit::hid_property(service, &self.product).unwrap_or_default();
                if zones::hid_skipped(&name) {
                    continue;
                }
                let live = iokit::hid_temperature(service)
                    .is_some_and(|t| zones::PLAUSIBLE_C.contains(&(t as f32)));
                if live {
                    named.push((CfOwned::retain(service), name));
                }
            }
            let has_macc = zones::hid_has_macc(named.iter().map(|(_, n)| n.as_str()));
            self.kept = named
                .into_iter()
                .map(|(service, name)| Kept {
                    zone: zones::hid_temp_zone(&name, has_macc),
                    service,
                    name,
                })
                .collect();
        }

        pub(super) fn read(&self, wanted: &[TempZone], filter: &mut Plausibility) -> Vec<Tagged> {
            self.kept
                .iter()
                .enumerate()
                .filter(|(_, kept)| wanted.contains(&kept.zone))
                .filter_map(|(i, kept)| {
                    let raw = iokit::hid_temperature(kept.service.borrow())? as f32;
                    let celsius = filter.admit(SensorId::Hid(i), raw)?;
                    Some(Tagged::new(kept.zone, kept.name.clone(), celsius))
                })
                .collect()
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn nothing_wanted_touches_nothing() {
        let mut hid = HidSensors::new();
        let mut filter = Plausibility::default();
        assert!(hid.sample(&[], Instant::now(), &mut filter).is_empty());
        assert_eq!(hid.kept(), 0);
        assert!(hid.probed_at.is_none());
    }

    #[test]
    fn wanted_zones_read_back_plausible_values() {
        let _io = crate::sampler::io_gate();
        let mut hid = HidSensors::new();
        let mut filter = Plausibility::default();
        let all = [
            TempZone::Cpu,
            TempZone::Gpu,
            TempZone::Battery,
            TempZone::Nand,
            TempZone::Other,
        ];
        let now = Instant::now();
        for reading in hid.sample(&all, now, &mut filter) {
            assert!(
                crate::zones::PLAUSIBLE_C.contains(&reading.celsius),
                "{reading:?}"
            );
            assert!(!crate::zones::hid_skipped(&reading.name), "{reading:?}");
        }
        let kept = hid.kept();
        let _ = hid.sample(&all, now + Duration::from_secs(2), &mut filter);
        assert_eq!(hid.kept(), kept, "no re-probe inside the window");
    }
}
