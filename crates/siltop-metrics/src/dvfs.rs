//! DVFS frequency tables and residency math.
//!
//! Tables come from the `IORegistry` once at start-up: pmgr `perf-domains`
//! (28-byte records, byte0 = voltage-states index, bytes 12..28 = the name
//! the `IOReport` complex channel uses), `acc-clusters` (8-byte records,
//! byte0 = voltage-states index, **byte1 = logical cluster id**) and
//! `voltage-states{N}-sram` (little-endian `(u32 freq, u32 mV)` pairs). The
//! GPU table is sgx `perf-states`, taking the first `gpu-num-perf-states`
//! non-zero entries. Chips without `perf-domains` fall back to macmon's
//! fixed layout (`voltage-states1` = E, `voltage-states5` = P,
//! `voltage-states11` = the second P cluster on Max/Ultra), and
//! [`link_cluster_ids`] ties those tables to the `cpuN` nodes by
//! `cluster-type`.
//!
//! Everything here is pure: property bytes and residencies in, tables and
//! ratios out, so the math is tested on bytes captured from a Mac17,8.

/// One CPU cluster's DVFS table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClusterTable {
    /// The `IOReport` complex channel name, e.g. `MCPU0` or `PCPU`.
    pub(crate) name: String,
    /// The logical cluster id (`cluster-id` on the `cpuN` nodes), when known.
    pub(crate) id: Option<u8>,
    /// Frequencies in MHz, lowest state first.
    pub(crate) mhz: Vec<u32>,
}

impl ClusterTable {
    pub(crate) fn max_mhz(&self) -> u32 {
        self.mhz.iter().copied().max().unwrap_or(0)
    }
}

/// Little-endian `(u32 freq, u32 voltage)` pairs to MHz. The unit is Hz when
/// the largest raw value is at least 1e7 (M1–M3 CPU, every GPU table) and
/// kHz otherwise (M4+ CPU `-sram` tables).
pub(crate) fn freq_table_mhz(raw: &[u8]) -> Vec<u32> {
    let freqs: Vec<u32> = raw
        .as_chunks::<8>()
        .0
        .iter()
        .filter_map(|pair| pair.get(..4)?.try_into().ok().map(u32::from_le_bytes))
        .collect();
    let max = freqs.iter().copied().max().unwrap_or(0);
    let div: u64 = if max >= 10_000_000 { 1_000_000 } else { 1_000 };
    freqs
        .into_iter()
        .map(|f| u32::try_from((u64::from(f) + div / 2) / div).unwrap_or(u32::MAX))
        .collect()
}

/// A plausible DVFS table: at least two states, strictly ascending, each
/// between 100 MHz and 10 GHz. Rejects filler bytes and the non-frequency
/// plain tables on M5.
fn plausible(mhz: &[u32]) -> bool {
    mhz.len() >= 2
        && mhz.iter().all(|f| (100..=10_000).contains(f))
        && mhz.windows(2).all(|w| w[0] < w[1])
}

/// Prefers `voltage-states{N}-sram`: on M5 the plain table holds values that
/// are not frequencies.
fn cluster_mhz(prop: &dyn Fn(&str) -> Option<Vec<u8>>, index: u8) -> Option<Vec<u32>> {
    [
        format!("voltage-states{index}-sram"),
        format!("voltage-states{index}"),
    ]
    .iter()
    .filter_map(|key| prop(key))
    .map(|raw| freq_table_mhz(&raw))
    .map(|mhz| mhz.into_iter().filter(|f| *f > 0).collect::<Vec<_>>())
    .find(|mhz| plausible(mhz))
}

fn perf_domains(raw: &[u8]) -> Vec<(u8, String)> {
    raw.as_chunks::<28>()
        .0
        .iter()
        .filter_map(|record| {
            let name = record.get(12..28)?;
            let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
            let name = String::from_utf8_lossy(name.get(..end)?).into_owned();
            (!name.is_empty()).then_some((*record.first()?, name))
        })
        .collect()
}

/// CPU cluster tables from pmgr properties (`prop` returns a property's
/// bytes). Empty when nothing plausible is found.
pub(crate) fn cpu_tables(prop: &dyn Fn(&str) -> Option<Vec<u8>>) -> Vec<ClusterTable> {
    let domains = prop("perf-domains").map_or_else(Vec::new, |raw| perf_domains(&raw));
    let clusters = prop("acc-clusters").unwrap_or_default();
    let found: Vec<ClusterTable> = clusters
        .as_chunks::<8>()
        .0
        .iter()
        .filter_map(|record| {
            let index = *record.first()?;
            let id = *record.get(1)?;
            let name = domains.iter().find(|(i, _)| *i == index)?.1.clone();
            Some(ClusterTable {
                name,
                id: Some(id),
                mhz: cluster_mhz(prop, index)?,
            })
        })
        .collect();
    if !found.is_empty() {
        return found;
    }
    // M1–M4 layout (macmon): E cluster in table 1, P cluster in table 5,
    // the second P cluster of a Max/Ultra in table 11.
    [("ECPU", 1), ("PCPU", 5), ("PCPU1", 11)]
        .into_iter()
        .filter_map(|(name, index)| {
            Some(ClusterTable {
                name: String::from(name),
                id: None,
                mhz: cluster_mhz(prop, index)?,
            })
        })
        .collect()
}

/// The GPU table from sgx properties: the first `gpu-num-perf-states`
/// non-zero entries of `perf-states` (row 0 is "off"), falling back to
/// pmgr `voltage-states9` via `pmgr`.
pub(crate) fn gpu_table(
    sgx: &dyn Fn(&str) -> Option<Vec<u8>>,
    pmgr: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Vec<u32> {
    let count = sgx("gpu-num-perf-states")
        .and_then(|b| b.get(..4)?.try_into().ok().map(u32::from_le_bytes))
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n > 0)
        .unwrap_or(usize::MAX);
    let from = |raw: Vec<u8>| -> Vec<u32> {
        freq_table_mhz(&raw)
            .into_iter()
            .filter(|f| *f > 0)
            .take(count)
            .collect()
    };
    let table = sgx("perf-states").map(from).unwrap_or_default();
    if plausible(&table) {
        return table;
    }
    let fallback = pmgr("voltage-states9-sram")
        .or_else(|| pmgr("voltage-states9"))
        .map(from)
        .unwrap_or_default();
    if plausible(&fallback) {
        fallback
    } else {
        Vec::new()
    }
}

/// A `cpuN` registry node: which cluster a Mach CPU belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CpuNode {
    /// `logical-cpu-id`, equal to the `host_processor_info` index.
    pub(crate) logical: usize,
    pub(crate) cluster_id: u8,
    /// `cluster-core-id`: the core's index inside its cluster.
    pub(crate) core: u8,
    /// The `cluster-type` letter (`E`, `P`, `M`), when the node has one.
    pub(crate) letter: Option<char>,
}

/// Gives the id-less fallback tables (`ECPU`, `PCPU`, `PCPU1`) the logical
/// cluster id of the matching `cpuN` nodes, so their core channels resolve
/// to Mach CPUs. The table's first letter picks the `cluster-type` and its
/// numeric suffix the n-th such cluster, in ascending `cluster-id` order.
/// Tables that already carry an id, or that no node matches, are left alone.
pub(crate) fn link_cluster_ids(tables: &mut [ClusterTable], nodes: &[CpuNode]) {
    for table in tables.iter_mut().filter(|t| t.id.is_none()) {
        let Some(letter) = table.name.chars().next() else {
            continue;
        };
        let nth: usize = match table.name.trim_start_matches(|c: char| !c.is_ascii_digit()) {
            "" => 0,
            digits => match digits.parse() {
                Ok(n) => n,
                Err(_) => continue,
            },
        };
        let mut ids: Vec<u8> = nodes
            .iter()
            .filter(|n| n.letter.is_some_and(|l| l.eq_ignore_ascii_case(&letter)))
            .map(|n| n.cluster_id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        table.id = ids.get(nth).copied();
    }
}

/// The Mach CPU an `IOReport` core channel (`MCPU03`, `PCPU5`) measures.
/// The channel is the longest matching complex name plus the in-cluster
/// core index. Returns the table index and, when the registry nodes say
/// which one it is, the Mach CPU.
pub(crate) fn map_core_channel(
    channel: &str,
    tables: &[ClusterTable],
    nodes: &[CpuNode],
) -> Option<(usize, Option<usize>)> {
    let (table_index, table, core) = tables
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            let rest = channel.strip_prefix(t.name.as_str())?;
            let core: u8 = (!rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
                .then(|| rest.parse().ok())??;
            Some((i, t, core))
        })
        .max_by_key(|(_, t, _)| t.name.len())?;
    let logical = table.id.and_then(|id| {
        nodes
            .iter()
            .find(|n| n.cluster_id == id && n.core == core)
            .map(|n| n.logical)
    });
    Some((table_index, logical))
}

/// One state channel's residencies, as `(state name, ticks)`.
pub(crate) type States<'a> = &'a [(String, i64)];

/// Frequency-weighted activity of one channel or cluster.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct Residency {
    /// Ticks in any state, idle included.
    pub(crate) total: f64,
    /// Ticks in a state that maps onto the table.
    pub(crate) active: f64,
    /// Σ ticks × MHz over active states.
    pub(crate) weighted: f64,
    /// Ticks in a non-idle state beyond the table (GPUPH P14/P15).
    pub(crate) unmapped: f64,
}

/// DOWN, IDLE and OFF are the inactive states; the rest map in order onto
/// the frequency table (`V0P14` → table[0], GPUPH `P1` → table[0]).
pub(crate) fn is_idle_state(name: &str) -> bool {
    matches!(name, "DOWN" | "IDLE" | "OFF")
}

pub(crate) fn residency(states: States<'_>, table: &[u32]) -> Residency {
    let mut out = Residency::default();
    let mut k = 0_usize;
    for (name, ticks) in states {
        let ticks = (*ticks).max(0) as f64;
        out.total += ticks;
        if is_idle_state(name) {
            continue;
        }
        match table.get(k) {
            Some(mhz) => {
                out.active += ticks;
                out.weighted += ticks * f64::from(*mhz);
            }
            None => out.unmapped += ticks,
        }
        k += 1;
    }
    out
}

impl Residency {
    pub(crate) fn add(self, other: Self) -> Self {
        Self {
            total: self.total + other.total,
            active: self.active + other.active,
            weighted: self.weighted + other.weighted,
            unmapped: self.unmapped + other.unmapped,
        }
    }

    /// Share of time in an active state, 0..=1.
    pub(crate) fn active_ratio(&self) -> f32 {
        if self.total > 0.0 {
            (self.active / self.total).clamp(0.0, 1.0) as f32
        } else {
            0.0
        }
    }

    /// Average MHz while active; `None` when the channel never ran.
    pub(crate) fn avg_mhz(&self) -> Option<u32> {
        (self.active > 0.0).then(|| (self.weighted / self.active).round() as u32)
    }

    /// Frequency-weighted busy: `max(avg, table[0]) × active / table_max`.
    pub(crate) fn scaled(&self, table: &[u32]) -> f32 {
        let active = self.active_ratio();
        match (self.avg_mhz(), table.first(), table.iter().max()) {
            (Some(avg), Some(min), Some(max)) if *max > 0 => {
                (active * mhz_ratio(avg, *min, *max)).clamp(0.0, 1.0)
            }
            _ => 0.0,
        }
    }
}

/// `max(avg, min) / max`, the factor that turns active time into
/// frequency-weighted busy.
pub(crate) fn mhz_ratio(avg: u32, min: u32, max: u32) -> f32 {
    if max == 0 {
        return 1.0;
    }
    (avg.max(min) as f32 / max as f32).clamp(0.0, 1.0)
}

/// Residency counters tick at 24 MHz. A delta whose total exceeds 1.5× the
/// ticks the window could hold is a bogus first delta and is dropped.
pub(crate) const TICKS_PER_SEC: f64 = 24e6;

pub(crate) fn residency_is_sane(total_ticks: f64, dt_secs: f64) -> bool {
    dt_secs > 0.0 && total_ticks >= 0.0 && total_ticks <= 1.5 * dt_secs * TICKS_PER_SEC
}

/// Reads the CPU tables, the `cpuN` nodes and the GPU table from the
/// registry. Empty vectors when a part is missing.
#[cfg(target_os = "macos")]
pub(crate) fn discover() -> (Vec<ClusterTable>, Vec<CpuNode>, Vec<u32>) {
    use crate::iokit::{self, CfKey, IoObject};

    fn named(name: &str) -> Option<IoObject> {
        iokit::services(c"AppleARMIODevice")
            .into_iter()
            .find(|s| s.name().as_deref() == Some(name))
    }
    fn bytes(entry: Option<&IoObject>, key: &str) -> Option<Vec<u8>> {
        let key = CfKey::new(&std::ffi::CString::new(key).ok()?)?;
        let value = iokit::property(entry?, &key)?;
        iokit::cf_data(value.borrow()).map(<[u8]>::to_vec)
    }
    fn le_u32(entry: &IoObject, key: &CfKey) -> Option<u32> {
        let value = iokit::property(entry, key)?;
        let v = value.borrow();
        iokit::cf_data(v)
            .and_then(|b| b.get(..4)?.try_into().ok().map(u32::from_le_bytes))
            .or_else(|| iokit::cf_i64(v).and_then(|n| u32::try_from(n).ok()))
    }

    let pmgr = named("pmgr");
    let sgx = named("sgx");
    let pmgr_prop = |k: &str| bytes(pmgr.as_ref(), k);
    let sgx_prop = |k: &str| bytes(sgx.as_ref(), k);
    let mut tables = cpu_tables(&pmgr_prop);
    let gpu = gpu_table(&sgx_prop, &pmgr_prop);
    let nodes = (|| {
        let name = CfKey::new(c"name")?;
        let kind = CfKey::new(c"cluster-type")?;
        let cluster = CfKey::new(c"cluster-id")?;
        let core = CfKey::new(c"cluster-core-id")?;
        let logical = CfKey::new(c"logical-cpu-id")?;
        Some(
            iokit::services(c"IOPlatformDevice")
                .iter()
                .filter(|s| {
                    iokit::property(s, &name)
                        .and_then(|v| iokit::cf_string(v.borrow()))
                        .is_some_and(|n| n.starts_with("cpu"))
                })
                .filter_map(|s| {
                    Some(CpuNode {
                        logical: usize::try_from(le_u32(s, &logical)?).ok()?,
                        cluster_id: u8::try_from(le_u32(s, &cluster)?).ok()?,
                        core: u8::try_from(le_u32(s, &core)?).ok()?,
                        letter: iokit::property(s, &kind)
                            .and_then(|v| iokit::cf_string(v.borrow()))
                            .and_then(|t| t.chars().next()),
                    })
                })
                .collect::<Vec<_>>(),
        )
    })()
    .unwrap_or_default();
    link_cluster_ids(&mut tables, &nodes);
    (tables, nodes, gpu)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::type_complexity
)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;

    const PMGR: &str = include_str!("fixtures/m5pro_pmgr.hex");
    const SGX: &str = include_str!("fixtures/m5pro_sgx.hex");

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    pub(crate) fn fixture(text: &str) -> HashMap<String, Vec<u8>> {
        text.lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .map(|l| {
                let (k, v) = l.split_once('\t').unwrap();
                (k.to_string(), hex(v.trim()))
            })
            .collect()
    }

    fn lookup(map: &HashMap<String, Vec<u8>>) -> impl Fn(&str) -> Option<Vec<u8>> + '_ {
        |key| map.get(key).cloned()
    }

    pub(crate) fn m5pro_tables() -> (Vec<ClusterTable>, Vec<u32>) {
        let pmgr = fixture(PMGR);
        let sgx = fixture(SGX);
        (
            cpu_tables(&lookup(&pmgr)),
            gpu_table(&lookup(&sgx), &lookup(&pmgr)),
        )
    }

    const MCPU: [u32; 15] = [
        1344, 1644, 1992, 2304, 2652, 2964, 3240, 3504, 3696, 3876, 4044, 4176, 4284, 4308, 4380,
    ];
    const PCPU: [u32; 20] = [
        1308, 1620, 1980, 2292, 2580, 2880, 3180, 3432, 3648, 3828, 3984, 4104, 4188, 4236, 4284,
        4308, 4332, 4428, 4512, 4608,
    ];
    const GPU: [u32; 13] = [
        338, 486, 636, 796, 888, 988, 1084, 1182, 1278, 1374, 1470, 1578, 1620,
    ];

    #[test]
    fn m5pro_fixture_yields_three_clusters_with_ids() {
        let (cpu, _) = m5pro_tables();
        let summary: Vec<(&str, Option<u8>, usize)> = cpu
            .iter()
            .map(|t| (t.name.as_str(), t.id, t.mhz.len()))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("MCPU0", Some(0), 15),
                ("MCPU1", Some(1), 15),
                ("PCPU", Some(2), 20)
            ]
        );
    }

    #[test]
    fn m5pro_mcpu_is_15_states_1344_to_4380() {
        let (cpu, _) = m5pro_tables();
        assert_eq!(cpu[0].mhz, MCPU);
        assert_eq!(cpu[1].mhz, MCPU);
        assert_eq!(cpu[0].max_mhz(), 4380);
    }

    #[test]
    fn m5pro_pcpu_is_20_states_1308_to_4608() {
        let (cpu, _) = m5pro_tables();
        assert_eq!(cpu[2].mhz, PCPU);
        assert_eq!(cpu[2].max_mhz(), 4608);
    }

    #[test]
    fn m5pro_gpu_is_13_states_338_to_1620() {
        let (_, gpu) = m5pro_tables();
        assert_eq!(gpu, GPU);
    }

    #[test]
    fn gpu_falls_back_to_pmgr_table_9() {
        let pmgr = fixture(PMGR);
        let none = |_: &str| None;
        assert_eq!(gpu_table(&none, &lookup(&pmgr)), GPU);
        assert!(gpu_table(&none, &none).is_empty());
    }

    #[test]
    fn gpu_count_caps_the_concatenated_second_table() {
        let sgx = fixture(SGX);
        let raw = sgx.get("perf-states").unwrap().clone();
        // Without the count both concatenated tables would appear (26 entries,
        // not ascending) and the table would be rejected.
        let no_count = |k: &str| (k == "perf-states").then(|| raw.clone());
        assert!(gpu_table(&no_count, &|_| None).is_empty());
    }

    #[test]
    fn plain_voltage_states_on_m5_are_not_frequencies() {
        let pmgr = fixture(PMGR);
        // Drop the -sram tables: the plain ones decode to nonsense and must be
        // rejected rather than shown as ~50 MHz.
        let no_sram = |k: &str| {
            if k.ends_with("-sram") {
                None
            } else {
                pmgr.get(k).cloned()
            }
        };
        let cpu = cpu_tables(&no_sram);
        assert!(
            cpu.iter().all(|t| t.mhz.iter().all(|f| *f >= 100)),
            "{cpu:?}"
        );
        assert!(!cpu.iter().any(|t| t.name == "PCPU"), "{cpu:?}");
    }

    #[test]
    fn perf_domains_decode_names_and_indices() {
        let pmgr = fixture(PMGR);
        let domains = perf_domains(pmgr.get("perf-domains").unwrap());
        let find = |n: &str| domains.iter().find(|(_, name)| name == n).map(|(i, _)| *i);
        assert_eq!(find("PCPU"), Some(5));
        assert_eq!(find("MCPU0"), Some(22));
        assert_eq!(find("MCPU1"), Some(23));
        assert_eq!(find("ANE"), Some(8));
        assert_eq!(domains.len(), 12);
    }

    #[test]
    fn acc_clusters_byte1_is_the_cluster_id_not_the_type() {
        // A synthetic record set whose ids are out of table order.
        let mut pmgr = fixture(PMGR);
        pmgr.insert(
            String::from("acc-clusters"),
            vec![5, 7, 0, 0, 0, 0, 0, 0, 22, 3, 0, 0, 0, 0, 0, 0],
        );
        let cpu = cpu_tables(&lookup(&pmgr));
        assert_eq!(cpu[0].name, "PCPU");
        assert_eq!(cpu[0].id, Some(7));
        assert_eq!(cpu[1].name, "MCPU0");
        assert_eq!(cpu[1].id, Some(3));
    }

    fn pairs(freqs: &[u32]) -> Vec<u8> {
        freqs
            .iter()
            .flat_map(|f| {
                let mut b = f.to_le_bytes().to_vec();
                b.extend_from_slice(&800_u32.to_le_bytes());
                b
            })
            .collect()
    }

    #[test]
    fn m1_layout_falls_back_to_tables_1_and_5() {
        let e = pairs(&[600_000_000, 972_000_000, 2_064_000_000]);
        let p = pairs(&[600_000_000, 828_000_000, 3_204_000_000]);
        let prop = |k: &str| match k {
            "voltage-states1" => Some(e.clone()),
            "voltage-states5" => Some(p.clone()),
            _ => None,
        };
        let cpu = cpu_tables(&prop);
        assert_eq!(cpu.len(), 2);
        assert_eq!(cpu[0].name, "ECPU");
        assert_eq!(cpu[0].mhz, [600, 972, 2064]);
        assert_eq!(cpu[1].mhz, [600, 828, 3204]);
        assert_eq!(cpu[1].id, None);
    }

    #[test]
    fn empty_or_garbage_registry_gives_no_tables() {
        let none = |_: &str| None;
        assert!(cpu_tables(&none).is_empty());
        let junk = |_: &str| Some(vec![0xff; 13]);
        assert!(cpu_tables(&junk).is_empty());
        let repeated = |_: &str| Some(vec![0xff; 64]);
        assert!(
            cpu_tables(&repeated).is_empty(),
            "flat filler is not a table"
        );
        let short = |_: &str| Some(vec![1, 2, 3]);
        assert!(cpu_tables(&short).is_empty());
        assert!(gpu_table(&short, &short).is_empty());
    }

    #[test]
    fn hz_khz_rule_table() {
        let cases: &[(&[u32], &[u32])] = &[
            (&[1_344_000, 4_380_000], &[1344, 4380]),      // kHz
            (&[338_000_000, 1_620_000_000], &[338, 1620]), // Hz
            (&[9_999_999], &[10_000]),                     // just below the Hz cut: kHz
            (&[10_000_000], &[10]),                        // at the cut: Hz
            (&[0, 338_000_000], &[0, 338]),                // off row kept here
            (&[u32::MAX], &[4295]),                        // no overflow
            (&[], &[]),
        ];
        for (raw, want) in cases {
            assert_eq!(freq_table_mhz(&pairs(raw)), *want, "{raw:?}");
        }
        // A trailing partial record is ignored.
        let mut raw = pairs(&[1_344_000]);
        raw.extend_from_slice(&[1, 2, 3]);
        assert_eq!(freq_table_mhz(&raw), [1344]);
    }

    fn states(list: &[(&str, i64)]) -> Vec<(String, i64)> {
        list.iter().map(|(n, t)| (String::from(*n), *t)).collect()
    }

    /// MCPU00 captured on the Mac17,8 at idle over 500 ms.
    fn mcpu00_idle() -> Vec<(String, i64)> {
        states(&[
            ("DOWN", 0),
            ("IDLE", 8_504_110),
            ("V0P14", 3_029_247),
            ("V1P13", 361_081),
            ("V2P12", 143_550),
            ("V3P11", 113_407),
            ("V4P10", 72_465),
            ("V5P9", 0),
            ("V6P8", 0),
            ("V7P7", 0),
            ("V8P6", 0),
            ("V9P5", 0),
            ("V10P4", 0),
            ("V11P3", 0),
            ("V12P2", 0),
            ("V13P1", 0),
            ("V14P0", 2_338),
        ])
    }

    #[test]
    fn captured_idle_core_residency() {
        let r = residency(&mcpu00_idle(), &MCPU);
        assert_eq!(r.unmapped, 0.0);
        let active = r.active_ratio();
        assert!((active - 0.3044).abs() < 0.001, "{active}");
        let mhz = r.avg_mhz().unwrap();
        assert!((1400..1460).contains(&mhz), "{mhz}");
        let scaled = r.scaled(&MCPU);
        assert!(scaled < active && scaled > 0.08, "{scaled}");
        assert!(residency_is_sane(r.total, 0.5));
    }

    #[test]
    fn idle_state_names_are_skipped() {
        for name in ["DOWN", "IDLE", "OFF"] {
            assert!(is_idle_state(name));
            let r = residency(&states(&[(name, 100), ("V0P2", 100)]), &[1000, 2000]);
            assert!((r.active_ratio() - 0.5).abs() < 1e-6, "{name}");
            assert_eq!(r.avg_mhz(), Some(1000));
        }
        for name in ["idle", "Idle", "V0P0", "P1", "DOWNX", ""] {
            assert!(!is_idle_state(name), "{name}");
        }
    }

    #[test]
    fn residency_math_table() {
        // (states, table, active, avg MHz, scaled)
        let t = [1000_u32, 2000, 4000];
        let cases: Vec<(Vec<(String, i64)>, f32, Option<u32>, f32)> = vec![
            (states(&[("IDLE", 100)]), 0.0, None, 0.0),
            (states(&[("IDLE", 0), ("V0", 0)]), 0.0, None, 0.0),
            (
                states(&[("V0", 0), ("V1", 0), ("V2", 100)]),
                1.0,
                Some(4000),
                1.0,
            ),
            // Mapping is positional: the first non-idle state is table[0]
            // whatever its name says.
            (states(&[("V2", 100)]), 1.0, Some(1000), 0.25),
            (states(&[("V0", 100)]), 1.0, Some(1000), 0.25),
            (
                states(&[("IDLE", 50), ("V0", 0), ("V1", 50)]),
                0.5,
                Some(2000),
                0.25,
            ),
            (
                states(&[("V0", 50), ("V1", 0), ("V2", 50)]),
                1.0,
                Some(2500),
                0.625,
            ),
            (
                states(&[("DOWN", 25), ("IDLE", 25), ("V0", 0), ("V1", 0), ("V2", 50)]),
                0.5,
                Some(4000),
                0.5,
            ),
            (states(&[("IDLE", 100), ("V0", -5)]), 0.0, None, 0.0),
        ];
        for (s, active, mhz, scaled) in cases {
            let r = residency(&s, &t);
            assert!((r.active_ratio() - active).abs() < 1e-6, "{s:?}");
            assert_eq!(r.avg_mhz(), mhz, "{s:?}");
            assert!(
                (r.scaled(&t) - scaled).abs() < 1e-6,
                "{s:?} {}",
                r.scaled(&t)
            );
        }
    }

    #[test]
    fn gpuph_maps_p_k_to_table_k_minus_1_and_ignores_p14_p15() {
        let mut s = vec![(String::from("OFF"), 0)];
        for k in 1..=15 {
            s.push((format!("P{k}"), 0));
        }
        // All residency in P13 → 1620 MHz (confirmed under full GPU load).
        s[13].1 = 1000;
        let r = residency(&s, &GPU);
        assert_eq!(r.avg_mhz(), Some(1620));
        assert!((r.scaled(&GPU) - 1.0).abs() < 1e-6);
        // P1 → 338 MHz.
        s[13].1 = 0;
        s[1].1 = 1000;
        assert_eq!(residency(&s, &GPU).avg_mhz(), Some(338));
        // P14/P15 are beyond the table: not active, reported as unmapped.
        s[1].1 = 0;
        s[14].1 = 400;
        s[15].1 = 100;
        let r = residency(&s, &GPU);
        assert_eq!(r.unmapped, 500.0);
        assert_eq!(r.active_ratio(), 0.0);
        assert_eq!(r.avg_mhz(), None);
    }

    #[test]
    fn captured_gpuph_idle_is_p1() {
        let mut s = vec![
            (String::from("OFF"), 11_001_568),
            (String::from("P1"), 1_128_136),
        ];
        for k in 2..=15 {
            s.push((format!("P{k}"), 0));
        }
        let r = residency(&s, &GPU);
        assert!((r.active_ratio() - 0.093).abs() < 0.001);
        assert_eq!(r.avg_mhz(), Some(338));
        assert!((r.scaled(&GPU) - 0.093 * 338.0 / 1620.0).abs() < 0.001);
    }

    #[test]
    fn scaled_never_exceeds_active_and_stays_in_range_sweep() {
        // Property sweep: 2000 pseudo-random residency vectors.
        let mut seed = 0x2545_f491_u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..2000 {
            let n = (next() % 20) as usize;
            let mut s = vec![(String::from("IDLE"), (next() % 1_000_000) as i64)];
            for k in 0..n {
                s.push((format!("V{k}"), (next() % 1_000_000) as i64));
            }
            let r = residency(&s, &PCPU);
            let active = r.active_ratio();
            let scaled = r.scaled(&PCPU);
            assert!((0.0..=1.0).contains(&active));
            assert!((0.0..=1.0).contains(&scaled));
            assert!(scaled <= active + 1e-6, "{scaled} > {active}");
            if let Some(mhz) = r.avg_mhz() {
                assert!((1308..=4608).contains(&mhz), "{mhz}");
                assert!(scaled >= active * 1308.0 / 4608.0 - 1e-5);
            }
        }
    }

    #[test]
    fn cluster_sum_weights_mhz_by_activity() {
        let a = residency(&states(&[("IDLE", 90), ("V0", 10)]), &[1000, 3000]);
        let b = residency(
            &states(&[("IDLE", 10), ("V0", 0), ("V1", 90)]),
            &[1000, 3000],
        );
        let c = a.add(b);
        assert!((c.active_ratio() - 0.5).abs() < 1e-6, "mean of the cores");
        assert_eq!(c.avg_mhz(), Some(2800));
    }

    #[test]
    fn mhz_ratio_floors_at_the_lowest_state() {
        assert!((mhz_ratio(0, 1344, 4380) - 1344.0 / 4380.0).abs() < 1e-6);
        assert!((mhz_ratio(4380, 1344, 4380) - 1.0).abs() < 1e-6);
        assert!((mhz_ratio(9999, 1344, 4380) - 1.0).abs() < 1e-6, "clamped");
        assert!(
            (mhz_ratio(5, 0, 0) - 1.0).abs() < 1e-6,
            "no table: no scaling"
        );
    }

    #[test]
    fn sanity_drop_rejects_bogus_first_delta() {
        // 2.0e9 ticks in a 0.5 s window was seen once after subscribing.
        assert!(!residency_is_sane(2.0e9, 0.5));
        assert!(residency_is_sane(12.0e6, 0.5));
        assert!(residency_is_sane(18.0e6, 0.5), "exactly 1.5x");
        assert!(!residency_is_sane(18.1e6, 0.5));
        assert!(!residency_is_sane(1.0, 0.0), "zero window");
        assert!(!residency_is_sane(-1.0, 1.0));
        // A full 1 s window over 18 cores' worth of per-channel ticks is fine.
        assert!(residency_is_sane(24.0e6, 1.0));
    }

    fn m5pro_nodes() -> Vec<CpuNode> {
        (0..18)
            .map(|i| CpuNode {
                logical: i,
                cluster_id: (i / 6) as u8,
                core: (i % 6) as u8,
                letter: Some(if i < 12 { 'M' } else { 'P' }),
            })
            .collect()
    }

    #[test]
    fn core_channels_map_to_mach_cpus() {
        let (cpu, _) = m5pro_tables();
        let nodes = m5pro_nodes();
        let cases = [
            ("MCPU00", Some((0, Some(0)))),
            ("MCPU05", Some((0, Some(5)))),
            ("MCPU10", Some((1, Some(6)))),
            ("MCPU15", Some((1, Some(11)))),
            ("PCPU0", Some((2, Some(12)))),
            ("PCPU5", Some((2, Some(17)))),
            ("PCPU6", Some((2, None))),
            ("PCPU", None),
            ("MCPU0", None),
            ("MCPU2", None),
            ("ECPU0", None),
            ("PCPUx", None),
            ("", None),
            ("MCPU0é", None),
        ];
        for (name, want) in cases {
            assert_eq!(map_core_channel(name, &cpu, &nodes), want, "{name}");
        }
    }

    fn m1_tables() -> Vec<ClusterTable> {
        ["ECPU", "PCPU"]
            .into_iter()
            .map(|name| ClusterTable {
                name: String::from(name),
                id: None,
                mhz: vec![600, 3204],
            })
            .collect()
    }

    #[test]
    fn ultra_die_prefixed_core_channels_degrade_to_absent_not_misfiled() {
        // Ultra names its per-core channels per die; without per-die tables
        // they must not land in die 0's cluster.
        let tables = m1_tables();
        for name in ["DIE_0_PCPU0", "DIE_1_ECPU3", "DIE_1_PCPU12"] {
            assert_eq!(map_core_channel(name, &tables, &[]), None, "{name}");
        }
    }

    #[test]
    fn unknown_future_core_channels_never_map() {
        let tables = m1_tables();
        for name in ["XCPU0", "ECPU", "ECPU-1", "PCPU 1", "", "ECPU0x", "ecpu0"] {
            assert_eq!(map_core_channel(name, &tables, &[]), None, "{name:?}");
        }
        assert_eq!(map_core_channel("PCPU0", &[], &[]), None, "no tables");
    }

    #[test]
    fn m1_to_m4_layout_with_only_a_p_table_keeps_what_exists() {
        let p = pairs(&[600_000_000, 3_504_000_000]);
        let prop = |k: &str| (k == "voltage-states5").then(|| p.clone());
        let cpu = cpu_tables(&prop);
        assert_eq!(cpu.len(), 1);
        assert_eq!(cpu[0].name, "PCPU");
    }

    #[test]
    fn family_frequency_ranges_decode_through_the_m1_layout() {
        // (chip, unit per MHz, E top, P top): macmon-documented top states.
        // M1–M3 store Hz; a top state over 4.29 GHz cannot fit a u32 in
        // Hz, so M4 and later store kHz.
        for (chip, per_mhz, e_top, p_top) in [
            ("M1", 1_000_000_u32, 2_064_u32, 3_204_u32),
            ("M2", 1_000_000, 2_424, 3_504),
            ("M3", 1_000_000, 2_748, 4_056),
            ("M4", 1_000, 2_892, 4_512),
        ] {
            let (e_hz, p_hz) = (e_top * per_mhz, p_top * per_mhz);
            let e = pairs(&[600 * per_mhz, e_hz]);
            let p = pairs(&[600 * per_mhz, p_hz]);
            let prop = |k: &str| match k {
                "voltage-states1" => Some(e.clone()),
                "voltage-states5" => Some(p.clone()),
                _ => None,
            };
            let cpu = cpu_tables(&prop);
            assert_eq!(cpu.len(), 2, "{chip}");
            assert_eq!(cpu[0].max_mhz(), e_top, "{chip}");
            assert_eq!(cpu[1].max_mhz(), p_top, "{chip}");
        }
    }

    #[test]
    fn m1_layout_tables_link_to_cpu_nodes_by_cluster_type() {
        // An M1 Max: 2 E cores (cluster 0), two 4-core P clusters (1, 2).
        let fallback = |name: &str| ClusterTable {
            name: String::from(name),
            id: None,
            mhz: vec![600, 3228],
        };
        let mut tables = vec![fallback("ECPU"), fallback("PCPU"), fallback("PCPU1")];
        let node = |logical: usize, cluster_id: u8, core: u8, letter: char| CpuNode {
            logical,
            cluster_id,
            core,
            letter: Some(letter),
        };
        let mut nodes = vec![node(0, 0, 0, 'E'), node(1, 0, 1, 'E')];
        nodes.extend((0..4).map(|c| node(2 + usize::from(c), 1, c, 'P')));
        nodes.extend((0..4).map(|c| node(6 + usize::from(c), 2, c, 'P')));
        link_cluster_ids(&mut tables, &nodes);
        let ids: Vec<Option<u8>> = tables.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![Some(0), Some(1), Some(2)]);
        assert_eq!(
            map_core_channel("ECPU1", &tables, &nodes),
            Some((0, Some(1)))
        );
        assert_eq!(
            map_core_channel("PCPU3", &tables, &nodes),
            Some((1, Some(5)))
        );
        assert_eq!(
            map_core_channel("PCPU10", &tables, &nodes),
            Some((2, Some(6)))
        );

        // No second P cluster, no letters, or an id already set: untouched.
        let mut lone = vec![fallback("PCPU1")];
        link_cluster_ids(&mut lone, &nodes[..6]);
        assert_eq!(lone[0].id, None);
        let mut unlabeled = vec![fallback("ECPU")];
        let blind: Vec<CpuNode> = nodes
            .iter()
            .map(|n| CpuNode { letter: None, ..*n })
            .collect();
        link_cluster_ids(&mut unlabeled, &blind);
        assert_eq!(unlabeled[0].id, None);
        let (mut m5, _) = m5pro_tables();
        let before = m5.clone();
        link_cluster_ids(&mut m5, &m5pro_nodes());
        assert_eq!(m5, before);
    }

    #[test]
    fn m1_layout_reads_the_second_p_cluster_from_table_11() {
        let e = pairs(&[600_000_000, 2_064_000_000]);
        let p = pairs(&[600_000_000, 3_228_000_000]);
        let prop = |k: &str| match k {
            "voltage-states1" => Some(e.clone()),
            "voltage-states5" | "voltage-states11" => Some(p.clone()),
            _ => None,
        };
        let names: Vec<String> = cpu_tables(&prop).into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["ECPU", "PCPU", "PCPU1"]);
    }

    #[test]
    fn channels_without_registry_ids_still_find_their_cluster() {
        let tables = vec![
            ClusterTable {
                name: String::from("ECPU"),
                id: None,
                mhz: vec![600, 2064],
            },
            ClusterTable {
                name: String::from("PCPU"),
                id: None,
                mhz: vec![600, 3204],
            },
        ];
        assert_eq!(map_core_channel("ECPU3", &tables, &[]), Some((0, None)));
        assert_eq!(map_core_channel("PCPU10", &tables, &[]), Some((1, None)));
        assert_eq!(map_core_channel("PCPU", &tables, &[]), None);
        assert_eq!(map_core_channel("GPUPH", &tables, &[]), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_registry_matches_the_fixture_on_this_machine_kind() {
        let (tables, nodes, gpu) = discover();
        if tables.is_empty() {
            return;
        }
        assert!(tables.iter().all(|t| plausible(&t.mhz)), "{tables:?}");
        assert!(gpu.is_empty() || plausible(&gpu));
        let mut logical: Vec<usize> = nodes.iter().map(|n| n.logical).collect();
        logical.sort_unstable();
        logical.dedup();
        assert_eq!(logical.len(), nodes.len(), "distinct logical ids");
        if tables.iter().any(|t| t.name == "MCPU0") {
            // A Mac17,8-class chip: live bytes decode like the fixture.
            let (fixture, fixture_gpu) = m5pro_tables();
            assert_eq!(tables, fixture);
            assert_eq!(gpu, fixture_gpu);
            assert_eq!(nodes.len(), 18);
        }
    }

    #[test]
    fn every_m5pro_core_channel_maps_to_a_distinct_cpu() {
        let (cpu, _) = m5pro_tables();
        let nodes = m5pro_nodes();
        let names: Vec<String> = (0..6)
            .map(|i| format!("MCPU0{i}"))
            .chain((0..6).map(|i| format!("MCPU1{i}")))
            .chain((0..6).map(|i| format!("PCPU{i}")))
            .collect();
        let mut seen: Vec<usize> = names
            .iter()
            .map(|n| map_core_channel(n, &cpu, &nodes).unwrap().1.unwrap())
            .collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..18).collect::<Vec<_>>());
    }
}
