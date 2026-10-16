#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClusterKind {
    Efficiency,
    Performance,
    Super,
}

impl ClusterKind {
    pub const ALL: [Self; 3] = [Self::Efficiency, Self::Performance, Self::Super];

    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Efficiency => "E",
            Self::Performance => "P",
            Self::Super => "S",
        }
    }

    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Efficiency => "efficiency",
            Self::Performance => "performance",
            Self::Super => "super",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cluster {
    pub kind: ClusterKind,
    pub scaled: f32,
    pub active: f32,
    pub freq_mhz: u32,
}

/// One cluster as the DVFS hardware sees it. A chip can have several of
/// one kind: the M5 Pro runs two independent Performance clusters.
#[derive(Debug, Clone, PartialEq)]
pub struct CpuCluster {
    /// The logical cluster id (`cluster-id` on the `cpuN` registry nodes).
    pub id: u8,
    pub kind: ClusterKind,
    /// The `IOReport` name, e.g. `MCPU0` or `PCPU`.
    pub name: String,
    /// Share of time not idle, 0..=1 (Mach ticks at the cheap cadence).
    pub active: f32,
    /// Frequency-weighted busy, 0..=1: `active` × the latest MHz ratio.
    pub scaled: f32,
    /// Effective (residency-weighted) MHz while active; `None` until the
    /// first residency sample, or while the cluster was powered down.
    pub freq_mhz: Option<u32>,
    /// The top of this cluster's DVFS table.
    pub max_mhz: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoreSample {
    pub kind: ClusterKind,
    pub index: u16,
    pub scaled: f32,
    pub active: f32,
    /// Effective MHz from `IOReport` residency; `None` when unknown.
    pub freq_mhz: Option<u32>,
}

impl CoreSample {
    /// A core with no frequency data: `scaled` equals `active`.
    #[must_use]
    pub const fn new(kind: ClusterKind, index: u16, active: f32) -> Self {
        Self {
            kind,
            index,
            scaled: active,
            active,
            freq_mhz: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CpuSnapshot {
    pub scaled: f32,
    pub active: f32,
    pub watts: Option<f32>,
    pub freq_mhz: Option<u32>,
    pub temp_c: Option<f32>,
    pub e_cluster: Option<Cluster>,
    pub p_cluster: Option<Cluster>,
    pub s_cluster: Option<Cluster>,
    pub cores: Vec<CoreSample>,
    /// Every DVFS cluster, in registry order. Empty when the chip's
    /// frequency tables are unknown; `e_/p_/s_cluster` stay filled either way.
    pub clusters: Vec<CpuCluster>,
}

impl Default for CpuSnapshot {
    fn default() -> Self {
        Self {
            scaled: 0.0,
            active: 0.0,
            watts: None,
            freq_mhz: None,
            temp_c: None,
            e_cluster: None,
            p_cluster: None,
            s_cluster: None,
            cores: Vec::new(),
            clusters: Vec::new(),
        }
    }
}
