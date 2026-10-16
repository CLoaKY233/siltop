use std::time::Instant;

use siltop_core::{CollectorId, Error, GpuSnapshot, Result, Snapshot};

use crate::collector::{Collector, Outcome};
use crate::ioreport::{FreqFeed, GpuFreq};

pub(crate) struct GpuCollector {
    #[cfg(target_os = "macos")]
    inner: Option<macos::Reader>,
    freq: FreqFeed,
}

impl GpuCollector {
    pub(crate) fn new() -> Self {
        Self {
            #[cfg(target_os = "macos")]
            inner: macos::Reader::new(),
            freq: FreqFeed::default(),
        }
    }

    /// Takes the headline, MHz and watts from `IOReport` GPUPH residency.
    pub(crate) fn with_freq(mut self, freq: FreqFeed) -> Self {
        self.freq = freq;
        self
    }

    pub(crate) fn sample(&mut self) -> Option<GpuSnapshot> {
        #[cfg(target_os = "macos")]
        {
            self.inner.as_mut()?.sample()
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }
}

impl Collector for GpuCollector {
    fn id(&self) -> CollectorId {
        CollectorId::Gpu
    }

    /// No accelerator at all is absent; losing one we had read is a failure.
    fn collect(&mut self, now: Instant, snap: &mut Snapshot) -> Result<Outcome> {
        let freq = self.freq.gpu(now);
        let sample = self.sample();
        if let Some(busy) = sample.as_ref().map(|g| g.active) {
            self.freq.note_gpu_busy(busy);
        }
        match merge_freq(sample, freq) {
            Some(gpu) => {
                snap.gpu = Some(gpu);
                Ok(Outcome::Fresh)
            }
            None if snap.gpu.is_some() => Err(Error::system("GPU statistics unavailable")),
            None => Ok(Outcome::Absent),
        }
    }
}

/// GPUPH residency replaces the `IOAccelerator` active-time headline, which
/// then moves to `renderer_busy` (the btop #1833 trap: 48% "utilization"
/// at 589 MHz is 17% of the GPU).
pub(crate) fn merge_freq(
    sample: Option<GpuSnapshot>,
    freq: Option<GpuFreq>,
) -> Option<GpuSnapshot> {
    let Some(freq) = freq else {
        return sample.map(|g| GpuSnapshot {
            renderer_busy: Some(g.active),
            ..g
        });
    };
    let base = sample.unwrap_or_default();
    Some(GpuSnapshot {
        renderer_busy: sample.map(|g| g.active),
        active: freq.active,
        scaled: freq.scaled,
        freq_mhz: freq.mhz,
        max_mhz: (freq.max_mhz > 0).then_some(freq.max_mhz),
        watts: freq.watts.or(base.watts),
        ..base
    })
}

#[cfg(target_os = "macos")]
mod macos {
    use siltop_core::GpuSnapshot;

    use crate::iokit::{self, CfKey, CfRef, IoObject};

    pub(super) struct Keys {
        stats: CfKey,
        util: [CfKey; 3],
        mem_in_use: CfKey,
        mem_alloc: CfKey,
    }

    impl Keys {
        pub(super) fn new() -> Option<Self> {
            Some(Self {
                stats: CfKey::new(c"PerformanceStatistics")?,
                util: [
                    CfKey::new(c"Device Utilization %")?,
                    CfKey::new(c"GPU Activity(%)")?,
                    CfKey::new(c"Renderer Utilization %")?,
                ],
                mem_in_use: CfKey::new(c"In use system memory")?,
                mem_alloc: CfKey::new(c"Alloc system memory")?,
            })
        }
    }

    pub(super) struct Reader {
        services: Vec<IoObject>,
        keys: Keys,
    }

    impl Reader {
        pub(super) fn new() -> Option<Self> {
            Some(Self {
                services: Vec::new(),
                keys: Keys::new()?,
            })
        }

        pub(super) fn sample(&mut self) -> Option<GpuSnapshot> {
            if self.services.is_empty() {
                self.rematch();
            }
            let mut best = self.busiest();
            if best.is_none() {
                self.rematch();
                best = self.busiest();
            }
            best
        }

        fn rematch(&mut self) {
            self.services = iokit::services(c"IOAccelerator");
        }

        fn busiest(&self) -> Option<GpuSnapshot> {
            self.services
                .iter()
                .filter_map(|service| {
                    let stats = iokit::property(service, &self.keys.stats)?;
                    from_stats(stats.borrow(), &self.keys)
                })
                .reduce(|a, b| if b.active > a.active { b } else { a })
        }
    }

    /// One `PerformanceStatistics` dictionary as a snapshot.
    pub(super) fn from_stats(stats: CfRef<'_>, keys: &Keys) -> Option<GpuSnapshot> {
        let util = keys.util.iter().find_map(|key| {
            iokit::cf_dict_get(stats, key)
                .and_then(iokit::cf_number)
                .filter(|v| *v >= 0.0)
        })?;
        let bytes = |key: &CfKey| {
            iokit::cf_dict_get(stats, key)
                .and_then(iokit::cf_i64)
                .and_then(|n| u64::try_from(n).ok())
        };
        let ratio = (util as f32 / 100.0).clamp(0.0, 1.0);
        Some(GpuSnapshot {
            scaled: ratio,
            active: ratio,
            mem_in_use_bytes: bytes(&keys.mem_in_use),
            mem_alloc_bytes: bytes(&keys.mem_alloc),
            ..GpuSnapshot::default()
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn memory_comes_from_performance_statistics() {
        use crate::iokit;
        let keys = macos::Keys::new().expect("keys");
        let stats = iokit::cf_dict_numbers(&[
            (c"Device Utilization %", 39),
            (c"Renderer Utilization %", 12),
            (c"In use system memory", 731_283_456),
            (c"Alloc system memory", 4_012_113_920),
        ])
        .expect("dict");
        let snap = macos::from_stats(stats.borrow(), &keys).expect("snapshot");
        assert!((snap.active - 0.39).abs() < 1e-6);
        assert_eq!(snap.mem_in_use_bytes, Some(731_283_456));
        assert_eq!(snap.mem_alloc_bytes, Some(4_012_113_920));

        let bare = iokit::cf_dict_numbers(&[(c"GPU Activity(%)", 250)]).expect("dict");
        let snap = macos::from_stats(bare.borrow(), &keys).expect("snapshot");
        assert!((snap.active - 1.0).abs() < f32::EPSILON, "clamped");
        assert_eq!(snap.mem_in_use_bytes, None);
        assert_eq!(snap.mem_alloc_bytes, None);

        let none = iokit::cf_dict_numbers(&[(c"Tiler Utilization %", 5)]).expect("dict");
        assert!(macos::from_stats(none.borrow(), &keys).is_none());
    }

    #[test]
    fn sample_is_a_ratio_or_absent() {
        let mut gpu = GpuCollector::new();
        if let Some(snap) = gpu.sample() {
            assert!((0.0..=1.0).contains(&snap.active));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "manual: cargo test -p siltop-metrics --release -- --ignored --nocapture gpu_sample_cost"]
    fn gpu_sample_cost() {
        let mut gpu = GpuCollector::new();
        let _ = gpu.sample();
        let rounds = 2000;
        let cpu0 = crate::sys::thread_cpu_secs();
        for _ in 0..rounds {
            let _ = gpu.sample();
        }
        let per_ms = (crate::sys::thread_cpu_secs() - cpu0) / f64::from(rounds) * 1e3;
        eprintln!("gpu sample {per_ms:.4} ms CPU");
        assert!(per_ms <= 0.05, "gpu sample {per_ms:.4} ms");
    }

    fn freq(active: f32, scaled: f32, mhz: Option<u32>, watts: Option<f32>) -> GpuFreq {
        GpuFreq {
            at: Instant::now(),
            active,
            scaled,
            mhz,
            max_mhz: 1620,
            watts,
            unmapped: false,
        }
    }

    #[test]
    fn d32_gpuph_replaces_the_active_time_headline() {
        let accel = GpuSnapshot {
            scaled: 0.48,
            active: 0.48,
            mem_in_use_bytes: Some(7),
            ..GpuSnapshot::default()
        };
        let merged = merge_freq(Some(accel), Some(freq(0.30, 0.17, Some(589), Some(1.5)))).unwrap();
        assert!((merged.scaled - 0.17).abs() < 1e-6);
        assert!((merged.active - 0.30).abs() < 1e-6);
        assert_eq!(merged.renderer_busy, Some(0.48));
        assert_eq!(merged.freq_mhz, Some(589));
        assert_eq!(merged.max_mhz, Some(1620));
        assert_eq!(merged.watts, Some(1.5));
        assert_eq!(merged.mem_in_use_bytes, Some(7), "accelerator fields kept");
    }

    #[test]
    fn without_ioreport_the_accelerator_figure_stays_and_is_labelled() {
        let accel = GpuSnapshot {
            scaled: 0.4,
            active: 0.4,
            ..GpuSnapshot::default()
        };
        let merged = merge_freq(Some(accel), None).unwrap();
        assert!((merged.scaled - 0.4).abs() < 1e-6);
        assert_eq!(merged.renderer_busy, Some(0.4));
        assert_eq!(merged.freq_mhz, None);
        assert_eq!(merged.watts, None, "absent, never 0 W");
        assert!(merge_freq(None, None).is_none());
    }

    #[test]
    fn ioreport_alone_still_yields_a_gpu() {
        let merged = merge_freq(None, Some(freq(0.1, 0.02, Some(338), None))).unwrap();
        assert_eq!(merged.renderer_busy, None);
        assert_eq!(merged.freq_mhz, Some(338));
        assert_eq!(merged.watts, None);
    }
}
