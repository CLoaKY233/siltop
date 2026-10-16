/// One row of the process table.
///
/// Values the kernel refused to share (another user's process without
/// root: `EPERM`) are `None` and `restricted` is set; they are never
/// reported as a made-up zero.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Process {
    pub pid: u32,
    pub ppid: u32,
    /// Display name: argv-aware (`python train.py` shows `train.py`).
    pub name: String,
    /// CPU in per-core percent: 100 is one fully busy core, so the ceiling
    /// is `100 × Snapshot::process_ncpu`. `None` when unreadable.
    pub cpu: Option<f32>,
    /// Physical footprint (what Activity Monitor and `top` call memory).
    pub mem_bytes: Option<u64>,
    pub threads: Option<u32>,
    /// GPU busy percent from the AGX user clients' accumulated GPU time
    /// (refreshed every 2 s); 0 for a process holding no GPU client.
    pub gpu: f32,
    pub user: String,
    pub uid: u32,
    pub command: Option<String>,
    /// Full argv, capped at 4 KiB, when readable.
    pub args: Vec<String>,
    /// `"stopped"`, `"zombie"`, or empty (running and sleeping are not
    /// distinguished: `p_stat` reads SRUN for nearly every process).
    pub status: &'static str,
    pub start_unix: i64,
    /// Recent per-pid CPU % samples (per-core percent), oldest first.
    pub cpu_spark: Vec<f32>,
    /// The kernel denied task and rusage info (another user's process).
    pub restricted: bool,
    /// Resident set size, for the dossier (memory above is the footprint).
    pub rss_bytes: Option<u64>,
    /// Energy rate from `ri_energy_nj` deltas.
    pub energy_w: Option<f32>,
    pub disk_read_bps: Option<f64>,
    pub disk_write_bps: Option<f64>,
    /// Package-idle plus interrupt wakeups per second.
    pub wakeups_per_s: Option<f32>,
    /// CPU time of reaped children (user + system).
    pub child_cpu_ns: Option<u64>,
}

impl Process {
    /// Whether this row's kernel data was withheld.
    #[must_use]
    pub const fn is_restricted(&self) -> bool {
        self.restricted
    }
}
