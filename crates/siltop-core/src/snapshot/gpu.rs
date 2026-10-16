#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct GpuSnapshot {
    pub scaled: f32,
    pub active: f32,
    pub watts: Option<f32>,
    /// Effective MHz from GPUPH residency.
    pub freq_mhz: Option<u32>,
    /// The top of the GPU DVFS table.
    pub max_mhz: Option<u32>,
    /// `IOAccelerator` "Device Utilization %", 0..=1: time the renderer was
    /// busy at any clock. The headline (`scaled`) is frequency-weighted.
    pub renderer_busy: Option<f32>,
    pub ane_watts: Option<f32>,
    pub temp_c: Option<f32>,
    /// Unified memory the GPU driver has in use ("In use system memory").
    pub mem_in_use_bytes: Option<u64>,
    /// Unified memory the GPU driver has allocated ("Alloc system memory").
    pub mem_alloc_bytes: Option<u64>,
}
