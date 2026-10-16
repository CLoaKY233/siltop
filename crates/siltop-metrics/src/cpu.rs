use std::time::Instant;

use siltop_core::{
    Cluster, ClusterKind, CollectorId, CoreSample, CpuCluster, CpuSnapshot, Result, Snapshot,
    SocInfo,
};

use crate::collector::{Collector, Outcome};
use crate::dvfs::mhz_ratio;
use crate::ioreport::{ClusterFreq, CpuFreq, FreqFeed};

pub(crate) struct CpuCollector {
    prev: Option<Vec<[u32; 4]>>,
    soc: SocInfo,
    freq: FreqFeed,
}

impl CpuCollector {
    pub(crate) fn new(soc: SocInfo) -> Self {
        Self {
            prev: None,
            soc,
            freq: FreqFeed::default(),
        }
    }

    /// Frequency-weights the Mach busy figures with `IOReport` residency.
    pub(crate) fn with_freq(mut self, freq: FreqFeed) -> Self {
        self.freq = freq;
        self
    }

    /// `None` until a previous sample gives the ratios a baseline.
    pub(crate) fn sample(&mut self) -> Result<Option<CpuSnapshot>> {
        #[cfg(target_os = "macos")]
        {
            macos::sample(self)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(None)
        }
    }
}

impl Collector for CpuCollector {
    fn id(&self) -> CollectorId {
        CollectorId::Cpu
    }

    fn collect(&mut self, now: Instant, snap: &mut Snapshot) -> Result<Outcome> {
        if cfg!(not(target_os = "macos")) {
            return Ok(Outcome::Absent);
        }
        let Some(mut cpu) = self.sample()? else {
            return Ok(Outcome::Baseline);
        };
        assign_core_roles(&mut cpu, &self.soc);
        self.freq.note_cpu_load(cpu.active);
        if let Some(freq) = self.freq.cpu(now) {
            apply_freq(&mut cpu, &freq);
        }
        snap.cpu = cpu;
        Ok(Outcome::Fresh)
    }
}

pub(crate) fn assign_core_roles(cpu: &mut CpuSnapshot, soc: &SocInfo) {
    let n = cpu.cores.len();
    for (i, core) in cpu.cores.iter_mut().enumerate() {
        if let Some((kind, index)) = soc.role_for(i, n) {
            core.kind = kind;
            core.index = index;
        }
    }
    cpu.e_cluster = summarize_cluster(&cpu.cores, ClusterKind::Efficiency);
    cpu.p_cluster = summarize_cluster(&cpu.cores, ClusterKind::Performance);
    cpu.s_cluster = summarize_cluster(&cpu.cores, ClusterKind::Super);
}

/// The kind a DVFS cluster name implies when the registry gave no kinds.
fn kind_from_name(name: &str) -> ClusterKind {
    if name.starts_with('E') {
        ClusterKind::Efficiency
    } else {
        ClusterKind::Performance
    }
}

/// Hybrid headline: Mach busy at the cheap cadence × the latest `IOReport`
/// MHz ratio (`max(avg, table[0]) / table_max`) of the core's cluster. An
/// approximation of pure residency-scaled busy that moves every tick while
/// residency is sampled every [`crate::ioreport::PMGR_PERIOD`]. A core with
/// no reading of its own (the registry could not tie its channel to a Mach
/// CPU) takes the mean ratio of the unmapped clusters of its kind.
pub(crate) fn apply_freq(cpu: &mut CpuSnapshot, freq: &CpuFreq) {
    let cluster_ratio =
        |c: &ClusterFreq| mhz_ratio(c.mhz.unwrap_or(c.min_mhz), c.min_mhz, c.max_mhz);
    for (i, core) in cpu.cores.iter_mut().enumerate() {
        let Some(reading) = freq.cores.iter().find(|c| c.cpu == i) else {
            let ratios: Vec<f32> = freq
                .clusters
                .iter()
                .filter(|c| c.cpus.is_empty() && kind_from_name(&c.name) == core.kind)
                .map(cluster_ratio)
                .collect();
            if !ratios.is_empty() {
                core.scaled = core.active * ratios.iter().sum::<f32>() / ratios.len() as f32;
            }
            continue;
        };
        let Some(cluster) = freq.clusters.get(reading.cluster) else {
            continue;
        };
        core.freq_mhz = reading.mhz;
        let mhz = reading.mhz.or(cluster.mhz).unwrap_or(cluster.min_mhz);
        core.scaled = core.active * mhz_ratio(mhz, cluster.min_mhz, cluster.max_mhz);
    }
    cpu.clusters = freq
        .clusters
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let cores: Vec<&CoreSample> = c.cpus.iter().filter_map(|&n| cpu.cores.get(n)).collect();
            let n = cores.len() as f32;
            let (active, scaled) = if cores.is_empty() {
                (c.active, c.scaled)
            } else {
                (
                    cores.iter().map(|k| k.active).sum::<f32>() / n,
                    cores.iter().map(|k| k.scaled).sum::<f32>() / n,
                )
            };
            CpuCluster {
                id: c.id.unwrap_or(u8::try_from(i).unwrap_or(u8::MAX)),
                kind: cores
                    .first()
                    .map_or_else(|| kind_from_name(&c.name), |k| k.kind),
                name: c.name.clone(),
                active,
                scaled,
                freq_mhz: c.mhz,
                max_mhz: c.max_mhz,
            }
        })
        .collect();
    if !cpu.cores.is_empty() {
        cpu.scaled = cpu.cores.iter().map(|c| c.scaled).sum::<f32>() / cpu.cores.len() as f32;
    }
    cpu.freq_mhz = weighted_mhz(cpu.clusters.iter());
    cpu.e_cluster = summarize_cluster(&cpu.cores, ClusterKind::Efficiency);
    cpu.p_cluster = summarize_cluster(&cpu.cores, ClusterKind::Performance);
    cpu.s_cluster = summarize_cluster(&cpu.cores, ClusterKind::Super);
    for summary in [&mut cpu.e_cluster, &mut cpu.p_cluster, &mut cpu.s_cluster]
        .into_iter()
        .flatten()
    {
        let kind = summary.kind;
        summary.freq_mhz =
            weighted_mhz(cpu.clusters.iter().filter(|c| c.kind == kind)).unwrap_or(0);
    }
}

/// MHz of several clusters, weighted by how busy each was; the plain top
/// clock when all are idle. `None` when no cluster has a clock.
pub(crate) fn weighted_mhz<'a>(clusters: impl Iterator<Item = &'a CpuCluster>) -> Option<u32> {
    let mut weight = 0.0_f64;
    let mut sum = 0.0_f64;
    let mut top = None;
    for c in clusters {
        let Some(mhz) = c.freq_mhz else {
            continue;
        };
        top = top.max(Some(mhz));
        weight += f64::from(c.active);
        sum += f64::from(c.active) * f64::from(mhz);
    }
    if weight > 0.0 {
        Some((sum / weight).round() as u32)
    } else {
        top
    }
}

fn summarize_cluster(cores: &[CoreSample], kind: ClusterKind) -> Option<Cluster> {
    let mut n = 0_u32;
    let mut sum_scaled = 0.0_f32;
    let mut sum_active = 0.0_f32;
    for core in cores {
        if core.kind == kind {
            n += 1;
            sum_scaled += core.scaled;
            sum_active += core.active;
        }
    }
    if n == 0 {
        return None;
    }
    Some(Cluster {
        kind,
        scaled: sum_scaled / n as f32,
        active: sum_active / n as f32,
        freq_mhz: 0,
    })
}

const USER: usize = 0;
const SYSTEM: usize = 1;
const IDLE: usize = 2;
const NICE: usize = 3;

fn tick_delta(prev: &[u32; 4], now: &[u32; 4]) -> [u64; 4] {
    [
        u64::from(now[USER].wrapping_sub(prev[USER])),
        u64::from(now[SYSTEM].wrapping_sub(prev[SYSTEM])),
        u64::from(now[IDLE].wrapping_sub(prev[IDLE])),
        u64::from(now[NICE].wrapping_sub(prev[NICE])),
    ]
}

fn ticks_total(delta: &[u64; 4]) -> u64 {
    delta[USER] + delta[SYSTEM] + delta[IDLE] + delta[NICE]
}

fn ticks_busy(delta: &[u64; 4]) -> u64 {
    delta[USER] + delta[SYSTEM] + delta[NICE]
}

/// Active (non-idle) ratio from two `cpu_ticks` snapshots.
pub(crate) fn core_active_ratio(prev: &[u32; 4], now: &[u32; 4]) -> f32 {
    let delta = tick_delta(prev, now);
    let total = ticks_total(&delta);
    if total == 0 {
        0.0
    } else {
        ticks_busy(&delta) as f32 / total as f32
    }
}

/// Overall active ratio across matching per-core tick arrays.
pub(crate) fn overall_active_ratio(prev: &[[u32; 4]], now: &[[u32; 4]]) -> f32 {
    if prev.len() != now.len() || prev.is_empty() {
        return 0.0;
    }
    let mut busy = 0_u64;
    let mut total = 0_u64;
    for (p, n) in prev.iter().zip(now.iter()) {
        let delta = tick_delta(p, n);
        busy += ticks_busy(&delta);
        total += ticks_total(&delta);
    }
    if total == 0 {
        0.0
    } else {
        busy as f32 / total as f32
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{CpuCollector, core_active_ratio, overall_active_ratio};
    use siltop_core::{ClusterKind, CoreSample, CpuSnapshot, Error, Result};
    use std::mem::size_of;
    use std::ptr;

    pub(super) fn sample(collector: &mut CpuCollector) -> Result<Option<CpuSnapshot>> {
        let now = load_ticks()?;
        let prev = collector.prev.replace(now.clone());
        let Some(prev) = prev.filter(|p| p.len() == now.len()) else {
            return Ok(None);
        };
        let overall = overall_active_ratio(&prev, &now);
        Ok(Some(snapshot_from_ratios(overall, &prev, &now)))
    }

    fn snapshot_from_ratios(overall: f32, prev: &[[u32; 4]], now: &[[u32; 4]]) -> CpuSnapshot {
        let cores = now
            .iter()
            .enumerate()
            .map(|(i, ticks)| {
                let active = prev.get(i).map_or(0.0, |p| core_active_ratio(p, ticks));
                CoreSample::new(ClusterKind::Performance, i as u16, active)
            })
            .collect();
        CpuSnapshot {
            scaled: overall,
            active: overall,
            cores,
            ..CpuSnapshot::default()
        }
    }

    fn load_ticks() -> Result<Vec<[u32; 4]>> {
        let mut cpu_count: libc::natural_t = 0;
        let mut info: libc::processor_info_array_t = ptr::null_mut();
        let mut info_count: libc::mach_msg_type_number_t = 0;
        #[allow(deprecated)]
        let kr = unsafe {
            // SAFETY: documented Mach host query; out pointers are valid locals.
            libc::host_processor_info(
                libc::mach_host_self(),
                libc::PROCESSOR_CPU_LOAD_INFO,
                &raw mut cpu_count,
                &raw mut info,
                &raw mut info_count,
            )
        };
        if kr != libc::KERN_SUCCESS {
            return Err(Error::system(format!("host_processor_info: {kr}")));
        }
        let ticks = copy_ticks(info, cpu_count, info_count);
        deallocate_info(info, info_count);
        Ok(ticks)
    }

    fn copy_ticks(
        info: libc::processor_info_array_t,
        cpu_count: libc::natural_t,
        info_count: libc::mach_msg_type_number_t,
    ) -> Vec<[u32; 4]> {
        if info.is_null() || cpu_count == 0 {
            return Vec::new();
        }
        let states = libc::CPU_STATE_MAX as usize;
        let available = info_count as usize;
        let ncpu = (cpu_count as usize).min(available / states);
        let mut out = Vec::with_capacity(ncpu);
        for i in 0..ncpu {
            let base = i * states;
            // SAFETY: kernel filled `info_count` integers; `base + 3` is in range.
            let ticks = unsafe {
                [
                    *info.add(base) as u32,
                    *info.add(base + 1) as u32,
                    *info.add(base + 2) as u32,
                    *info.add(base + 3) as u32,
                ]
            };
            out.push(ticks);
        }
        out
    }

    fn deallocate_info(
        info: libc::processor_info_array_t,
        info_count: libc::mach_msg_type_number_t,
    ) {
        if info.is_null() {
            return;
        }
        let bytes = (info_count as usize).saturating_mul(size_of::<libc::integer_t>());
        #[allow(deprecated)]
        unsafe {
            // SAFETY: buffer came from host_processor_info; size is the returned count.
            libc::vm_deallocate(
                libc::mach_task_self(),
                info as libc::vm_address_t,
                bytes as libc::vm_size_t,
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn ratio_from_two_tick_arrays() {
        let prev = vec![[10, 5, 80, 5], [0, 0, 100, 0]];
        let now = vec![[20, 15, 90, 5], [10, 0, 110, 0]];
        // core0: d=(10,10,10,0) busy=20 total=30 → 2/3
        // core1: d=(10,0,10,0) busy=10 total=20 → 0.5
        // overall busy=30 total=50 → 0.6
        assert!((core_active_ratio(&prev[0], &now[0]) - 20.0 / 30.0).abs() < f32::EPSILON);
        assert!((core_active_ratio(&prev[1], &now[1]) - 0.5).abs() < f32::EPSILON);
        assert!((overall_active_ratio(&prev, &now) - 0.6).abs() < f32::EPSILON);
    }

    #[test]
    fn zero_delta_is_idle() {
        let ticks = [4, 1, 90, 1];
        assert!((core_active_ratio(&ticks, &ticks) - 0.0).abs() < f32::EPSILON);
        assert!((overall_active_ratio(&[ticks], &[ticks]) - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn length_mismatch_is_zero() {
        assert!(
            (overall_active_ratio(&[[1, 0, 0, 0]], &[[1, 0, 0, 0], [1, 0, 0, 0]]) - 0.0).abs()
                < f32::EPSILON
        );
    }

    #[test]
    fn wrapping_ticks() {
        let prev = [u32::MAX - 4, 0, 10, 0];
        let now = [5, 0, 20, 0];
        let ratio = core_active_ratio(&prev, &now);
        assert!(ratio > 0.4 && ratio < 0.6, "{ratio}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_first_sample_is_only_a_baseline() {
        let mut cpu = CpuCollector::new(SocInfo::default());
        assert!(cpu.sample().expect("cpu1").is_none(), "no made-up 0%");
        std::thread::sleep(std::time::Duration::from_millis(40));
        let second = cpu.sample().expect("cpu2").expect("ratios");
        assert!(!second.cores.is_empty());
        assert!((0.0..=1.0).contains(&second.active));
        assert!((0.0..=1.0).contains(&second.scaled));
    }

    #[test]
    fn assign_roles_uses_registry_kinds() {
        let mut cpu = CpuSnapshot {
            cores: (0..4)
                .map(|i| CoreSample::new(ClusterKind::Performance, i, 0.1 * (f32::from(i) + 1.0)))
                .collect(),
            ..CpuSnapshot::default()
        };
        let soc = SocInfo {
            e_cores: 2,
            p_cores: 2,
            core_kinds: vec![
                ClusterKind::Efficiency,
                ClusterKind::Efficiency,
                ClusterKind::Performance,
                ClusterKind::Performance,
            ],
            ..SocInfo::default()
        };
        assign_core_roles(&mut cpu, &soc);
        assert_eq!(cpu.cores[0].kind, ClusterKind::Efficiency);
        assert_eq!(cpu.cores[0].index, 0);
        assert_eq!(cpu.cores[1].index, 1);
        assert_eq!(cpu.cores[2].kind, ClusterKind::Performance);
        assert_eq!(cpu.cores[2].index, 0);
        assert!(cpu.e_cluster.is_some());
        assert!(cpu.p_cluster.is_some());
        assert!(cpu.s_cluster.is_none());
    }

    fn freq_fixture(mcpu0: Option<u32>, mcpu1: Option<u32>, pcpu: Option<u32>) -> CpuFreq {
        use crate::ioreport::{ClusterFreq, CoreFreq};
        let cluster = |name: &str, id: u8, min: u32, max: u32, mhz: Option<u32>| ClusterFreq {
            name: String::from(name),
            id: Some(id),
            min_mhz: min,
            max_mhz: max,
            active: 1.0,
            scaled: 1.0,
            mhz,
            cpus: (usize::from(id) * 6..usize::from(id) * 6 + 6).collect(),
        };
        let clusters = vec![
            cluster("MCPU0", 0, 1344, 4380, mcpu0),
            cluster("MCPU1", 1, 1344, 4380, mcpu1),
            cluster("PCPU", 2, 1308, 4608, pcpu),
        ];
        let cores = (0..18)
            .map(|cpu| CoreFreq {
                cpu,
                cluster: cpu / 6,
                active: 1.0,
                mhz: clusters[cpu / 6].mhz,
            })
            .collect();
        CpuFreq {
            at: Instant::now(),
            clusters,
            cores,
        }
    }

    fn m5pro_cpu(active: f32) -> CpuSnapshot {
        let mut cpu = CpuSnapshot {
            cores: (0..18)
                .map(|i| CoreSample::new(ClusterKind::Performance, i, active))
                .collect(),
            active,
            scaled: active,
            ..CpuSnapshot::default()
        };
        let mut kinds = vec![ClusterKind::Performance; 12];
        kinds.extend([ClusterKind::Super; 6]);
        let soc = SocInfo {
            p_cores: 12,
            s_cores: 6,
            core_kinds: kinds,
            ..SocInfo::default()
        };
        assign_core_roles(&mut cpu, &soc);
        cpu
    }

    #[test]
    fn d31_full_load_fills_clusters_mhz_and_scaled() {
        let mut cpu = m5pro_cpu(1.0);
        apply_freq(&mut cpu, &freq_fixture(Some(4380), Some(4380), Some(4104)));
        let names: Vec<(&str, ClusterKind, Option<u32>)> = cpu
            .clusters
            .iter()
            .map(|c| (c.name.as_str(), c.kind, c.freq_mhz))
            .collect();
        assert_eq!(
            names,
            vec![
                ("MCPU0", ClusterKind::Performance, Some(4380)),
                ("MCPU1", ClusterKind::Performance, Some(4380)),
                ("PCPU", ClusterKind::Super, Some(4104)),
            ]
        );
        assert!((cpu.clusters[0].scaled - 1.0).abs() < 1e-6);
        assert!((cpu.clusters[2].scaled - 4104.0 / 4608.0).abs() < 1e-5);
        assert_eq!(cpu.cores[12].freq_mhz, Some(4104));
        assert_eq!(cpu.p_cluster.unwrap().freq_mhz, 4380);
        assert_eq!(cpu.s_cluster.unwrap().freq_mhz, 4104);
        let want = (12.0 + 6.0 * 4104.0 / 4608.0) / 18.0;
        assert!((cpu.scaled - want).abs() < 1e-5, "{}", cpu.scaled);
        assert!((cpu.active - 1.0).abs() < 1e-6, "active stays Mach busy");
    }

    #[test]
    fn hybrid_scaled_is_mach_busy_times_mhz_ratio() {
        for busy in [0.0_f32, 0.1, 0.25, 0.5, 0.9, 1.0] {
            let mut cpu = m5pro_cpu(busy);
            apply_freq(&mut cpu, &freq_fixture(Some(1344), Some(2190), Some(4608)));
            let c = &cpu.clusters;
            assert!((c[0].scaled - busy * 1344.0 / 4380.0).abs() < 1e-5);
            assert!((c[1].scaled - busy * 2190.0 / 4380.0).abs() < 1e-5);
            assert!((c[2].scaled - busy).abs() < 1e-5);
            assert!(cpu.scaled <= cpu.active + 1e-6);
            assert!(c.iter().all(|k| (k.active - busy).abs() < 1e-6));
        }
    }

    #[test]
    fn two_performance_clusters_average_into_the_legacy_summary() {
        let mut cpu = m5pro_cpu(0.5);
        // Only MCPU0 busy: the P summary's clock leans on it.
        for core in &mut cpu.cores[..6] {
            core.active = 1.0;
        }
        apply_freq(&mut cpu, &freq_fixture(Some(4380), Some(1344), None));
        let p = cpu.p_cluster.unwrap();
        let expected: f64 = (1.0 * 4380.0 + 0.5 * 1344.0) / 1.5;
        assert_eq!(p.freq_mhz, expected.round() as u32);
        assert_eq!(
            cpu.s_cluster.unwrap().freq_mhz,
            0,
            "no clock is 0 in the legacy u32"
        );
        assert_eq!(cpu.clusters[2].freq_mhz, None);
    }

    #[test]
    fn an_idle_cluster_scales_at_its_floor_not_zero_or_one() {
        let mut cpu = m5pro_cpu(0.2);
        let mut freq = freq_fixture(Some(4380), Some(4380), None);
        for core in &mut freq.cores[12..] {
            core.mhz = None;
        }
        apply_freq(&mut cpu, &freq);
        let s = &cpu.clusters[2];
        assert!(
            (s.scaled - 0.2 * 1308.0 / 4608.0).abs() < 1e-5,
            "{}",
            s.scaled
        );
        assert_eq!(cpu.cores[12].freq_mhz, None);
    }

    #[test]
    fn weighted_mhz_table() {
        let c = |active: f32, mhz: Option<u32>| CpuCluster {
            id: 0,
            kind: ClusterKind::Performance,
            name: String::new(),
            active,
            scaled: active,
            freq_mhz: mhz,
            max_mhz: 4380,
        };
        let cases = [
            (vec![], None),
            (vec![c(0.5, None)], None),
            (vec![c(0.0, Some(1344)), c(0.0, Some(4380))], Some(4380)),
            (vec![c(1.0, Some(1000)), c(1.0, Some(3000))], Some(2000)),
            (vec![c(0.9, Some(4000)), c(0.1, Some(1000))], Some(3700)),
            (vec![c(1.0, Some(2000)), c(1.0, None)], Some(2000)),
        ];
        for (clusters, want) in cases {
            assert_eq!(weighted_mhz(clusters.iter()), want, "{clusters:?}");
        }
    }

    #[test]
    fn freq_for_fewer_cores_than_mach_reports_leaves_the_rest_alone() {
        let mut cpu = m5pro_cpu(0.6);
        let mut freq = freq_fixture(Some(4380), Some(4380), Some(4608));
        freq.cores.truncate(3);
        apply_freq(&mut cpu, &freq);
        assert_eq!(cpu.cores[0].freq_mhz, Some(4380));
        assert_eq!(cpu.cores[17].freq_mhz, None);
        assert!(
            (cpu.cores[17].scaled - 0.6).abs() < 1e-6,
            "no data: scaled = active"
        );
    }

    #[test]
    fn m1_layout_without_per_core_readings_still_weights_the_headline() {
        use crate::ioreport::ClusterFreq;
        // An M2 Air: 4 E + 4 P at 30 % Mach busy, E at 912 of 2424 MHz, P
        // idle at its floor; no channel maps to a Mach CPU.
        let mut cpu = CpuSnapshot {
            cores: (0..8)
                .map(|i| CoreSample::new(ClusterKind::Performance, i, 0.3))
                .collect(),
            active: 0.3,
            scaled: 0.3,
            ..CpuSnapshot::default()
        };
        let soc = SocInfo {
            e_cores: 4,
            p_cores: 4,
            ..SocInfo::default()
        };
        assign_core_roles(&mut cpu, &soc);
        let cluster = |name: &str, min: u32, max: u32, mhz: Option<u32>| ClusterFreq {
            name: String::from(name),
            id: None,
            min_mhz: min,
            max_mhz: max,
            active: 0.3,
            scaled: 0.1,
            mhz,
            cpus: Vec::new(),
        };
        let freq = CpuFreq {
            at: Instant::now(),
            clusters: vec![
                cluster("ECPU", 600, 2424, Some(912)),
                cluster("PCPU", 660, 3504, None),
            ],
            cores: Vec::new(),
        };
        apply_freq(&mut cpu, &freq);
        let e = cpu.e_cluster.unwrap();
        let p = cpu.p_cluster.unwrap();
        assert!(
            (e.scaled - 0.3 * 912.0 / 2424.0).abs() < 1e-5,
            "{}",
            e.scaled
        );
        assert!(
            (p.scaled - 0.3 * 660.0 / 3504.0).abs() < 1e-5,
            "{}",
            p.scaled
        );
        assert!(cpu.scaled < cpu.active, "{} < {}", cpu.scaled, cpu.active);
        assert!((cpu.scaled - e.scaled.midpoint(p.scaled)).abs() < 1e-6);
    }

    #[test]
    fn kind_falls_back_to_the_cluster_name() {
        assert_eq!(kind_from_name("ECPU"), ClusterKind::Efficiency);
        assert_eq!(kind_from_name("PCPU"), ClusterKind::Performance);
        assert_eq!(kind_from_name("MCPU1"), ClusterKind::Performance);
        assert_eq!(kind_from_name(""), ClusterKind::Performance);
    }
}
