use std::collections::HashMap;

use siltop_core::{ClusterKind, SensorsSnapshot, TempReading};

/// What one temperature sensor measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TempZone {
    Cluster(ClusterKind),
    /// A die sensor not tied to one cluster.
    Cpu,
    Gpu,
    /// A hardware maximum over a bank of die sensors.
    Hotspot,
    Battery,
    Nand,
    Other,
}

/// One plausible reading with the sensor's own name.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Tagged {
    pub(crate) zone: TempZone,
    pub(crate) name: String,
    pub(crate) celsius: f32,
}

impl Tagged {
    pub(crate) fn new(zone: TempZone, name: impl Into<String>, celsius: f32) -> Self {
        Self {
            zone,
            name: name.into(),
            celsius,
        }
    }
}

const MAX_EXTRAS: usize = 6;

/// HID services that never carry a live temperature: `PMU tdev*` return
/// about −9201 (24 services, ~41 ms per pass on an M5 Pro), `PMU tcal` is a
/// constant 51.82 calibration value, and `als-temp` returns no event.
pub(crate) fn hid_skipped(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("tdev") || n.contains("tcal") || n.starts_with("als")
}

pub(crate) fn hid_has_macc<I, S>(names: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    names
        .into_iter()
        .any(|n| n.as_ref().to_ascii_lowercase().contains("macc"))
}

pub(crate) fn hid_temp_zone(name: &str, has_macc: bool) -> TempZone {
    let p = name.to_ascii_lowercase();
    if p.contains("eacc") {
        TempZone::Cluster(ClusterKind::Efficiency)
    } else if p.contains("macc") {
        TempZone::Cluster(ClusterKind::Performance)
    } else if p.contains("sacc") {
        TempZone::Cluster(ClusterKind::Super)
    } else if p.contains("pacc") {
        if has_macc {
            TempZone::Cluster(ClusterKind::Super)
        } else {
            TempZone::Cluster(ClusterKind::Performance)
        }
    } else if p.contains("gpu") {
        TempZone::Gpu
    } else if p.contains("gas gauge") || p.contains("battery") {
        TempZone::Battery
    } else if p.contains("nand") {
        TempZone::Nand
    } else if p.contains("pmu tdie")
        || p.contains("soc mtr")
        || p.contains("pmgr")
        || p.contains("cpu")
    {
        TempZone::Cpu
    } else {
        TempZone::Other
    }
}

/// `IORegistry` `cluster-type` letter. `P` is Super when `M` exists or a Super perflevel exists without an `S` letter.
pub(crate) fn cluster_letter_kind(
    letter: char,
    letters: &str,
    has_super_level: bool,
) -> Option<ClusterKind> {
    match letter.to_ascii_uppercase() {
        'E' => Some(ClusterKind::Efficiency),
        'S' => Some(ClusterKind::Super),
        'P' if letters.contains('M') || (has_super_level && !letters.contains('S')) => {
            Some(ClusterKind::Super)
        }
        'M' | 'P' => Some(ClusterKind::Performance),
        _ => None,
    }
}

pub(crate) fn kind_from_level_name(name: &str) -> Option<ClusterKind> {
    let n = name.to_ascii_lowercase();
    if n.contains("super") {
        Some(ClusterKind::Super)
    } else if n.contains("efficien") {
        Some(ClusterKind::Efficiency)
    } else if n.contains("perform") {
        Some(ClusterKind::Performance)
    } else {
        None
    }
}

pub(crate) fn counts_from_levels(levels: &[(ClusterKind, u8)]) -> (u8, u8, u8) {
    let mut e = 0_u8;
    let mut p = 0_u8;
    let mut s = 0_u8;
    for (kind, n) in levels {
        match kind {
            ClusterKind::Efficiency => e = e.saturating_add(*n),
            ClusterKind::Performance => p = p.saturating_add(*n),
            ClusterKind::Super => s = s.saturating_add(*n),
        }
    }
    (e, p, s)
}

/// Zones this machine should report: its clusters and the GPU.
pub(crate) fn expected_zones(e_cores: u8, p_cores: u8, s_cores: u8) -> Vec<TempZone> {
    let mut zones = Vec::new();
    for (kind, n) in [
        (ClusterKind::Efficiency, e_cores),
        (ClusterKind::Performance, p_cores),
        (ClusterKind::Super, s_cores),
    ] {
        if n > 0 {
            zones.push(TempZone::Cluster(kind));
        }
    }
    zones.push(TempZone::Gpu);
    zones.extend([TempZone::Battery, TempZone::Nand]);
    zones
}

/// Expected zones that `have` does not cover.
pub(crate) fn missing_zones(have: &[Tagged], expected: &[TempZone]) -> Vec<TempZone> {
    expected
        .iter()
        .copied()
        .filter(|zone| !have.iter().any(|t| t.zone == *zone))
        .collect()
}

/// Sensor identity for the plausibility filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SensorId {
    Smc(u32),
    Hid(usize),
}

/// Keeps readings inside 10–130 °C and holds back a single-sample jump of
/// more than 25 °C for one sample: if the next sample returns to the old
/// level the spike is dropped, if it stays the new level is accepted.
#[derive(Debug, Default)]
pub(crate) struct Plausibility {
    last: HashMap<SensorId, f32>,
    pending: HashMap<SensorId, f32>,
}

pub(crate) const PLAUSIBLE_C: std::ops::RangeInclusive<f32> = 10.0..=130.0;
const MAX_JUMP_C: f32 = 25.0;

impl Plausibility {
    pub(crate) fn admit(&mut self, id: SensorId, celsius: f32) -> Option<f32> {
        if !celsius.is_finite() || !PLAUSIBLE_C.contains(&celsius) {
            return None;
        }
        let Some(&last) = self.last.get(&id) else {
            self.last.insert(id, celsius);
            return Some(celsius);
        };
        let settled = (celsius - last).abs() <= MAX_JUMP_C
            || self
                .pending
                .get(&id)
                .is_some_and(|p| (celsius - p).abs() <= MAX_JUMP_C);
        if settled {
            self.pending.remove(&id);
            self.last.insert(id, celsius);
            Some(celsius)
        } else {
            self.pending.insert(id, celsius);
            Some(last)
        }
    }

    pub(crate) fn forget_hid(&mut self) {
        self.last.retain(|id, _| matches!(id, SensorId::Smc(_)));
        self.pending.retain(|id, _| matches!(id, SensorId::Smc(_)));
    }
}

/// Builds the snapshot from tagged readings: zone means, a CPU average over
/// cluster sensors (or die sensors when no cluster is known), a hotspot that
/// is the hottest die reading (hardware maxima, clusters, CPU and GPU; never
/// battery or NAND), and extras deduplicated by name before the cap.
pub(crate) fn assemble(readings: &[Tagged]) -> SensorsSnapshot {
    let values = |pick: &dyn Fn(TempZone) -> bool| -> Vec<f32> {
        readings
            .iter()
            .filter(|t| pick(t.zone))
            .map(|t| t.celsius)
            .collect()
    };
    let cluster = |kind: ClusterKind| mean(&values(&|z| z == TempZone::Cluster(kind)));
    let clusters = values(&|z| matches!(z, TempZone::Cluster(_)));
    let die = values(&|z| {
        matches!(
            z,
            TempZone::Hotspot | TempZone::Cluster(_) | TempZone::Cpu | TempZone::Gpu
        )
    });
    let mut snap = SensorsSnapshot {
        e_c: cluster(ClusterKind::Efficiency),
        p_c: cluster(ClusterKind::Performance),
        s_c: cluster(ClusterKind::Super),
        gpu_c: mean(&values(&|z| z == TempZone::Gpu)),
        cpu_c: mean(&clusters).or_else(|| mean(&values(&|z| z == TempZone::Cpu))),
        hotspot_c: die.into_iter().reduce(f32::max),
        readings: Vec::new(),
    };
    snap.readings = zone_readings(&snap);
    snap.readings.extend(extras(readings));
    snap
}

fn zone_readings(snap: &SensorsSnapshot) -> Vec<TempReading> {
    let mut out = Vec::new();
    push_reading(&mut out, "efficiency", snap.e_c);
    push_reading(&mut out, "performance", snap.p_c);
    push_reading(&mut out, "super", snap.s_c);
    if snap.e_c.is_none() && snap.p_c.is_none() && snap.s_c.is_none() {
        push_reading(&mut out, "cpu", snap.cpu_c);
    }
    push_reading(&mut out, "gpu", snap.gpu_c);
    out
}

fn extras(readings: &[Tagged]) -> Vec<TempReading> {
    let mut groups: Vec<(String, Vec<f32>)> = Vec::new();
    for tagged in readings {
        let Some(name) = friendly_name(tagged) else {
            continue;
        };
        match groups.iter_mut().find(|(n, _)| *n == name) {
            Some((_, temps)) => temps.push(tagged.celsius),
            None => groups.push((name, vec![tagged.celsius])),
        }
    }
    groups
        .into_iter()
        .filter_map(|(name, temps)| mean(&temps).map(|celsius| TempReading { name, celsius }))
        .take(MAX_EXTRAS)
        .collect()
}

/// The label an extra reading is shown under, or `None` for zone sensors.
fn friendly_name(tagged: &Tagged) -> Option<String> {
    match tagged.zone {
        TempZone::Battery => Some(String::from("battery")),
        TempZone::Nand => Some(String::from("nand")),
        TempZone::Other => {
            let name = tagged.name.trim().to_ascii_lowercase();
            (!name.is_empty()).then_some(name)
        }
        _ => None,
    }
}

fn push_reading(out: &mut Vec<TempReading>, name: &str, temp: Option<f32>) {
    if let Some(celsius) = temp {
        out.push(TempReading {
            name: name.to_owned(),
            celsius,
        });
    }
}

fn mean(values: &[f32]) -> Option<f32> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f32>() / values.len() as f32)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const E: TempZone = TempZone::Cluster(ClusterKind::Efficiency);
    const P: TempZone = TempZone::Cluster(ClusterKind::Performance);
    const S: TempZone = TempZone::Cluster(ClusterKind::Super);

    fn t(zone: TempZone, name: &str, c: f32) -> Tagged {
        Tagged::new(zone, name, c)
    }

    #[test]
    fn hid_acc_names() {
        assert_eq!(hid_temp_zone("eACC MTR Temp Sensor1", false), E);
        assert_eq!(hid_temp_zone("pACC MTR Temp Sensor2", false), P);
        assert_eq!(hid_temp_zone("pACC MTR Temp Sensor2", true), S);
        assert_eq!(hid_temp_zone("mACC MTR Temp Sensor0", true), P);
        assert_eq!(hid_temp_zone("PMU tdie3", false), TempZone::Cpu);
        assert_eq!(hid_temp_zone("GPU MTR Temp Sensor1", false), TempZone::Gpu);
        assert_eq!(hid_temp_zone("NAND CH0 temp", false), TempZone::Nand);
        assert_eq!(hid_temp_zone("gas gauge battery", false), TempZone::Battery);
        assert_eq!(hid_temp_zone("ANE MTR Temp", false), TempZone::Other);
        assert!(hid_has_macc(["pACC", "mACC 1"]));
        assert!(!hid_has_macc(["pACC", "eACC"]));
    }

    #[test]
    fn dead_and_constant_hid_services_are_skipped() {
        for name in ["PMU tdev1", "PMU tdev8", "PMU tcal", "als-temp"] {
            assert!(hid_skipped(name), "{name}");
        }
        for name in ["PMU tdie3", "gas gauge battery", "NAND CH0 temp"] {
            assert!(!hid_skipped(name), "{name}");
        }
    }

    #[test]
    fn cluster_letters() {
        assert_eq!(
            cluster_letter_kind('E', "EP", false),
            Some(ClusterKind::Efficiency)
        );
        assert_eq!(
            cluster_letter_kind('P', "EP", false),
            Some(ClusterKind::Performance)
        );
        assert_eq!(
            cluster_letter_kind('M', "MP", true),
            Some(ClusterKind::Performance)
        );
        assert_eq!(
            cluster_letter_kind('P', "MP", true),
            Some(ClusterKind::Super)
        );
        assert_eq!(
            cluster_letter_kind('P', "EP", true),
            Some(ClusterKind::Super)
        );
        assert_eq!(
            cluster_letter_kind('P', "EPS", true),
            Some(ClusterKind::Performance)
        );
        assert_eq!(cluster_letter_kind('X', "EP", false), None);
    }

    #[test]
    fn named_perflevels() {
        assert_eq!(kind_from_level_name("Super"), Some(ClusterKind::Super));
        assert_eq!(
            kind_from_level_name("Performance"),
            Some(ClusterKind::Performance)
        );
        assert_eq!(
            kind_from_level_name("Efficiency"),
            Some(ClusterKind::Efficiency)
        );
        assert_eq!(
            counts_from_levels(&[(ClusterKind::Super, 6), (ClusterKind::Performance, 12),]),
            (0, 12, 6)
        );
    }

    #[test]
    fn cluster_sensors_define_the_cpu_average() {
        let snap = assemble(&[
            t(E, "eACC MTR 1", 34.0),
            t(E, "eACC MTR 2", 38.0),
            t(P, "pACC MTR 1", 50.0),
            t(TempZone::Cpu, "PMU tdie3", 90.0),
            t(TempZone::Gpu, "GPU MTR 1", 47.0),
        ]);
        assert_eq!(snap.e_c, Some(36.0));
        assert_eq!(snap.p_c, Some(50.0));
        assert_eq!(snap.gpu_c, Some(47.0));
        assert_eq!(snap.cpu_c, Some(122.0 / 3.0));
        assert_eq!(snap.hotspot_c, Some(90.0));
        assert!(snap.readings.iter().any(|r| r.name == "efficiency"));
        assert!(!snap.readings.iter().any(|r| r.name.contains("tdie")));
    }

    #[test]
    fn die_sensors_stand_in_for_a_missing_cluster_split() {
        let snap = assemble(&[
            t(TempZone::Cpu, "Tp01", 44.0),
            t(TempZone::Cpu, "Tp05", 46.0),
            t(TempZone::Gpu, "Tg0D", 51.0),
        ]);
        assert_eq!(snap.e_c, None);
        assert_eq!(snap.cpu_c, Some(45.0));
        assert_eq!(snap.gpu_c, Some(51.0));
        assert_eq!(snap.readings[0].name, "cpu");
    }

    #[test]
    fn hotspot_is_the_hottest_die_reading() {
        let snap = assemble(&[
            t(S, "Tp00", 41.0),
            t(TempZone::Hotspot, "TPDX", 43.3),
            t(TempZone::Hotspot, "TRDX", 38.9),
            t(TempZone::Battery, "TB0T", 32.0),
        ]);
        assert_eq!(snap.hotspot_c, Some(43.3), "a hardware maximum can lead");
        let snap = assemble(&[
            t(S, "Tp00", 44.3),
            t(TempZone::Hotspot, "TPDX", 42.9),
            t(TempZone::Gpu, "Tg08", 42.6),
        ]);
        assert_eq!(snap.hotspot_c, Some(44.3), "never below a zone reading");
        let snap = assemble(&[t(S, "Tp00", 41.0), t(TempZone::Battery, "TB0T", 60.0)]);
        assert_eq!(snap.hotspot_c, Some(41.0), "battery is never the hotspot");
    }

    #[test]
    fn extras_dedupe_before_the_cap() {
        let mut readings = vec![t(TempZone::Cpu, "PMU tdie1", 40.0)];
        for i in 0..6 {
            readings.push(t(TempZone::Battery, "gas gauge battery", 32.0 + i as f32));
        }
        readings.push(t(TempZone::Nand, "NAND CH0 temp", 34.0));
        let snap = assemble(&readings);
        let names: Vec<&str> = snap.readings.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["cpu", "battery", "nand"]);
        let battery = snap.readings.iter().find(|r| r.name == "battery").unwrap();
        assert!((battery.celsius - 34.5).abs() < f32::EPSILON);
    }

    #[test]
    fn extras_are_capped_after_dedupe() {
        let readings: Vec<Tagged> = (0..9)
            .map(|i| t(TempZone::Other, &format!("sensor {i}"), 30.0))
            .collect();
        assert_eq!(assemble(&readings).readings.len(), MAX_EXTRAS);
    }

    #[test]
    fn missing_zones_drive_the_hid_fallback() {
        let expected = expected_zones(0, 12, 6);
        let m5 = [
            t(S, "Tp00", 41.0),
            t(P, "Tp0O", 40.0),
            t(TempZone::Gpu, "Tg08", 39.0),
            t(TempZone::Battery, "TB0T", 32.0),
            t(TempZone::Nand, "TH0x", 33.0),
        ];
        assert!(missing_zones(&m5, &expected).is_empty());
        let no_gpu = [t(S, "Tp00", 41.0), t(P, "Tp0O", 40.0)];
        assert_eq!(
            missing_zones(&no_gpu, &expected),
            vec![TempZone::Gpu, TempZone::Battery, TempZone::Nand]
        );
        assert_eq!(missing_zones(&[], &expected), expected);
    }

    #[test]
    fn plausibility_drops_out_of_range_and_one_sample_spikes() {
        let mut filter = Plausibility::default();
        let id = SensorId::Smc(1);
        assert_eq!(filter.admit(id, 5.0), None);
        assert_eq!(filter.admit(id, 140.0), None);
        assert_eq!(filter.admit(id, f32::NAN), None);
        assert_eq!(filter.admit(id, 40.0), Some(40.0));
        assert_eq!(filter.admit(id, 90.0), Some(40.0), "held");
        assert_eq!(filter.admit(id, 41.0), Some(41.0), "spike reverted");
        assert_eq!(filter.admit(id, 70.0), Some(41.0), "held");
        assert_eq!(filter.admit(id, 72.0), Some(72.0), "jump persisted");
        assert_eq!(filter.admit(id, 80.0), Some(80.0));
        assert_eq!(filter.admit(SensorId::Hid(0), 51.8), Some(51.8));
        filter.forget_hid();
        assert_eq!(filter.admit(SensorId::Hid(0), 20.0), Some(20.0));
        assert_eq!(filter.admit(id, 81.0), Some(81.0), "SMC state kept");
    }
}
