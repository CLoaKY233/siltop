#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskVolume {
    pub name: String,
    pub mount: String,
    pub used_bytes: u64,
    pub total_bytes: u64,
}

impl DiskVolume {
    #[must_use]
    pub fn ratio(&self) -> f32 {
        if self.total_bytes == 0 {
            0.0
        } else {
            self.used_bytes as f32 / self.total_bytes as f32
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DiskSnapshot {
    pub volumes: Vec<DiskVolume>,
    pub read_bps: u64,
    pub write_bps: u64,
    /// Per block-storage device rates, keyed by `IORegistry` entry id.
    pub devices: Vec<DiskDevice>,
}

/// One `IOBlockStorageDriver`'s I/O rate in bytes per second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiskDevice {
    pub registry_id: u64,
    pub read_bps: u64,
    pub write_bps: u64,
}

impl DiskSnapshot {
    #[must_use]
    pub fn primary(&self) -> Option<&DiskVolume> {
        self.volumes
            .iter()
            .find(|v| v.mount == "/")
            .or_else(|| self.volumes.first())
    }

    #[must_use]
    pub fn used_ratio(&self) -> f32 {
        self.primary().map_or(0.0, DiskVolume::ratio)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_primary_prefers_root() {
        let snap = DiskSnapshot {
            volumes: vec![
                DiskVolume {
                    name: String::from("Data"),
                    mount: String::from("/System/Volumes/Data"),
                    used_bytes: 10,
                    total_bytes: 100,
                },
                DiskVolume {
                    name: String::from("Macintosh HD"),
                    mount: String::from("/"),
                    used_bytes: 40,
                    total_bytes: 100,
                },
            ],
            read_bps: 0,
            write_bps: 0,
            devices: Vec::new(),
        };
        assert_eq!(snap.primary().map(|v| v.mount.as_str()), Some("/"));
        assert!((snap.used_ratio() - 0.4).abs() < f32::EPSILON);
    }
}
