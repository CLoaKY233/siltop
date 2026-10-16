use std::time::{Duration, Instant};

mod battery;
mod cpu;
mod disk;
mod gpu;
mod memory;
mod net;
mod power;
mod process;
mod sensors;
mod soc;

pub use battery::{
    Adapter, AwakeAssertion, BatterySnapshot, ChargeState, HoldReason, NOT_CHARGING_CHARGE_LIMIT,
    PowerSource, TimeRemaining, health_percent,
};
pub use cpu::{Cluster, ClusterKind, CoreSample, CpuCluster, CpuSnapshot};
pub use disk::{DiskDevice, DiskSnapshot, DiskVolume};
pub use gpu::GpuSnapshot;
pub use memory::{Composition, MemorySnapshot, Pressure};
pub use net::{IfaceRate, NetworkSnapshot};
pub use power::{POWER_STALE_AFTER, PowerSnapshot, Reading, ReadingSource, Throttle};
pub use process::Process;
pub use sensors::{FanMetric, FanSnapshot, SensorsSnapshot, TempReading, Thermal};
pub use soc::{SocInfo, linear_core_role, role_for_core};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sampled {
    pub procs: bool,
    pub sensors: bool,
}

impl Default for Sampled {
    fn default() -> Self {
        Self {
            procs: true,
            sensors: true,
        }
    }
}

/// One collector feeding the snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CollectorId {
    Cpu,
    Memory,
    Gpu,
    Network,
    Disk,
    Processes,
    Sensors,
    Fans,
    Thermal,
    Battery,
}

/// How trustworthy a collector's part of the snapshot is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Health {
    /// Fresh values from this collector's latest sample.
    #[default]
    Ok,
    /// No sample has arrived yet (start-up).
    Pending,
    /// The collector failed; its last good value is repeated and is this old.
    /// Never push a stale value into history.
    Stale(Duration),
    /// The collector has failed without ever producing a value.
    Failed(String),
    /// This machine has no source for it.
    Absent,
}

impl Health {
    /// Only fresh values belong in history.
    #[must_use]
    pub const fn is_fresh(&self) -> bool {
        matches!(self, Self::Ok)
    }
}

/// Health per collector. A collector that never reported is `Pending`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HealthReport {
    entries: Vec<(CollectorId, Health)>,
}

impl HealthReport {
    #[must_use]
    pub fn get(&self, id: CollectorId) -> &Health {
        const PENDING: &Health = &Health::Pending;
        self.entries
            .iter()
            .find(|(entry, _)| *entry == id)
            .map_or(PENDING, |(_, health)| health)
    }

    #[must_use]
    pub fn is_fresh(&self, id: CollectorId) -> bool {
        self.get(id).is_fresh()
    }

    pub fn set(&mut self, id: CollectorId, health: Health) {
        if let Some((_, slot)) = self.entries.iter_mut().find(|(entry, _)| *entry == id) {
            *slot = health;
        } else {
            self.entries.push((id, health));
            self.entries.sort_by_key(|(entry, _)| *entry);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (CollectorId, &Health)> {
        self.entries.iter().map(|(id, health)| (*id, health))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub collected_at: Instant,
    pub soc: SocInfo,
    pub cpu: CpuSnapshot,
    pub memory: MemorySnapshot,
    pub gpu: Option<GpuSnapshot>,
    pub network: NetworkSnapshot,
    pub disk: DiskSnapshot,
    pub fans: FanSnapshot,
    pub sensors: SensorsSnapshot,
    pub processes: Vec<Process>,
    /// Logical CPUs behind `Process::cpu` (per-core percent tops out at
    /// `100 × process_ncpu`).
    pub process_ncpu: u32,
    /// Machine CPU not accounted for by readable processes, in per-core
    /// percent: restricted rows, the kernel, and processes that came and
    /// went between samples. `None` until the process table is measured.
    pub unattributed_cpu: Option<f32>,
    pub thermal: Thermal,
    /// Package power by rail and throttle state, from the sensor thread.
    pub power: PowerSnapshot,
    pub status: Option<String>,
    pub sampled: Sampled,
    /// Every rate (CPU %, process CPU, network and disk throughput) was
    /// measured over a real interval. `false` means those numbers are not
    /// readings yet: show `…` and keep them out of history.
    pub primed: bool,
    /// Every slow source this Mac has (the `IOReport` frequency feed, the
    /// SMC sensor thread, the battery) has delivered its first reading or
    /// reported itself absent. Until then cluster MHz, power and battery
    /// may be `None` only because they have not arrived yet. Energy-counter
    /// rails are not waited for: their first rate needs two publications.
    pub settled: bool,
    pub health: HealthReport,
    /// Battery and power source; `present == false` on desktops.
    pub battery: BatterySnapshot,
}

impl Snapshot {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            collected_at: Instant::now(),
            soc: SocInfo::default(),
            cpu: CpuSnapshot::default(),
            memory: MemorySnapshot::default(),
            gpu: None,
            network: NetworkSnapshot::default(),
            disk: DiskSnapshot::default(),
            fans: FanSnapshot::default(),
            sensors: SensorsSnapshot::default(),
            processes: Vec::new(),
            process_ncpu: 0,
            unattributed_cpu: None,
            thermal: Thermal::Nominal,
            power: PowerSnapshot::default(),
            status: None,
            sampled: Sampled::default(),
            primed: false,
            settled: false,
            health: HealthReport::default(),
            battery: BatterySnapshot::default(),
        }
    }

    /// The CPU temperature every consumer shows: the collector's own, else
    /// the best SMC/HID sensor estimate.
    #[must_use]
    pub fn cpu_temp_c(&self) -> Option<f32> {
        self.cpu.temp_c.or_else(|| self.sensors.best_cpu_c())
    }

    /// The GPU temperature every consumer shows: the GPU's own, else the
    /// sensors' GPU die.
    #[must_use]
    pub fn gpu_temp_c(&self) -> Option<f32> {
        self.gpu.and_then(|g| g.temp_c).or(self.sensors.gpu_c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_snapshot_is_idle() {
        let snap = Snapshot::empty();
        assert!(snap.thermal.is_nominal());
        assert!(snap.processes.is_empty());
        assert!(snap.gpu.is_none());
        assert!(snap.disk.volumes.is_empty());
        assert!(snap.fans.fans.is_empty());
        assert!((snap.cpu.scaled - 0.0).abs() < f32::EPSILON);
        assert!(!snap.primed, "no rates before the first interval");
        assert_eq!(snap.health.get(CollectorId::Cpu), &Health::Pending);
    }

    #[test]
    fn health_report_sets_and_reads_back() {
        let mut report = HealthReport::default();
        report.set(CollectorId::Sensors, Health::Stale(Duration::from_secs(7)));
        report.set(CollectorId::Cpu, Health::Ok);
        report.set(CollectorId::Cpu, Health::Failed(String::from("boom")));
        assert_eq!(
            report.get(CollectorId::Cpu),
            &Health::Failed(String::from("boom"))
        );
        assert!(!report.is_fresh(CollectorId::Sensors));
        assert!(!report.is_fresh(CollectorId::Gpu), "never reported");
        let order: Vec<CollectorId> = report.iter().map(|(id, _)| id).collect();
        assert_eq!(order, vec![CollectorId::Cpu, CollectorId::Sensors]);
        assert!(Health::Ok.is_fresh());
        assert!(!Health::Absent.is_fresh());
    }
}
