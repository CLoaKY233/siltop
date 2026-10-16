//! Disk capacity (one line per APFS container, real volume names) and I/O
//! rates from per-device `IOBlockStorageDriver` statistics (D17, D33).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use siltop_core::{CollectorId, DiskDevice, DiskVolume, Result, Snapshot};

use crate::cadence::Pace;
use crate::collector::{Collector, Outcome};

/// How often block-storage drivers are rematched (hot-plug latency).
pub(crate) const REMATCH_EVERY: Duration = Duration::from_secs(2);

/// How often the mounted volumes are re-read.
const VOLUMES_EVERY: Duration = Duration::from_secs(2);

/// `MNT_DONTBROWSE` from `<sys/mount.h>`.
pub(crate) const MNT_DONTBROWSE: u32 = 0x0010_0000;

const SKIP_FSTYPES: &[&str] = &["devfs", "autofs", "fdesc"];

const SKIP_MOUNT_PREFIXES: &[&str] = &[
    "/dev",
    "/System/Volumes/VM",
    "/System/Volumes/Preboot",
    "/System/Volumes/Update",
    "/System/Volumes/xarts",
    "/System/Volumes/iSCPreboot",
    "/System/Volumes/Hardware",
];

/// Per-device cumulative byte counters and the rate state built on them.
#[derive(Debug, Default)]
pub(crate) struct DeviceRates {
    prev: HashMap<u64, (u64, u64)>,
}

impl DeviceRates {
    /// Advance by one sample of `(registry_id, read_bytes, write_bytes)`
    /// taken `dt` seconds after the previous one. A device seen for the
    /// first time reads 0 (no lifetime-total spike); a counter that goes
    /// backwards reads 0; a device missing from this sample keeps its
    /// previous counters so a transient read failure never spikes.
    pub(crate) fn advance(&mut self, samples: &[(u64, u64, u64)], dt: f64) -> Vec<DiskDevice> {
        let per_sec = |delta: u64| {
            if !dt.is_finite() || dt <= 0.0 {
                return 0;
            }
            let r = delta as f64 / dt;
            if r.is_finite() { r as u64 } else { 0 }
        };
        samples
            .iter()
            .map(|&(id, read, write)| {
                let (read_bps, write_bps) = self.prev.get(&id).map_or((0, 0), |&(r0, w0)| {
                    (
                        per_sec(read.saturating_sub(r0)),
                        per_sec(write.saturating_sub(w0)),
                    )
                });
                self.prev.insert(id, (read, write));
                DiskDevice {
                    registry_id: id,
                    read_bps,
                    write_bps,
                }
            })
            .collect()
    }
}

/// The `Physical Interconnect Location` of a file-backed disk image
/// (`.dmg`, sparsebundle; its interconnect is "Virtual Interface"). Its
/// I/O is also counted on the physical disk that holds the image file, so
/// summing both would count every byte twice. A RAM disk (location "RAM")
/// or a VM's virtual disk has no such backing disk here and still counts.
const FILE_BACKED: &str = "File";

/// Whether a block device's I/O belongs in the totals, from its `Physical
/// Interconnect Location`: everything but a file-backed disk image. A
/// device that reports none is counted.
pub(crate) fn counted(location: Option<&str>) -> bool {
    location != Some(FILE_BACKED)
}

/// Sum per-device rates into the headline read/write rates.
pub(crate) fn totals(devices: &[DiskDevice]) -> (u64, u64) {
    devices.iter().fold((0, 0), |(r, w), d| {
        (r.saturating_add(d.read_bps), w.saturating_add(d.write_bps))
    })
}

pub(crate) struct DiskCollector {
    rates: DeviceRates,
    last: Option<Instant>,
    volumes: Vec<DiskVolume>,
    volumes_at: Pace,
    #[cfg(target_os = "macos")]
    names: VolNames,
    #[cfg(target_os = "macos")]
    io: Option<macos::IoReader>,
}

impl DiskCollector {
    pub(crate) fn new() -> Self {
        Self {
            rates: DeviceRates::default(),
            last: None,
            volumes: Vec::new(),
            volumes_at: Pace::default(),
            #[cfg(target_os = "macos")]
            names: VolNames::default(),
            #[cfg(target_os = "macos")]
            io: macos::IoReader::new(),
        }
    }

    fn refresh_volumes(&mut self, now: Instant) {
        if !self.volumes_at.due(now, VOLUMES_EVERY) {
            return;
        }
        self.volumes_at.ran(now);
        #[cfg(target_os = "macos")]
        {
            self.volumes = macos::volumes(&mut self.names);
        }
    }

    /// Per-device rates, or `None` while only a baseline exists. A tick
    /// with no counters at all is an error, never a zero rate.
    fn rates(&mut self, now: Instant) -> Result<Option<Vec<DiskDevice>>> {
        #[cfg(target_os = "macos")]
        {
            let samples = self
                .io
                .as_mut()
                .map(|io| io.samples(now))
                .filter(|s| !s.is_empty())
                .ok_or_else(|| siltop_core::Error::system("no block storage statistics"))?;
            Ok(self.advance(&samples, now))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = now;
            Ok(None)
        }
    }

    /// One step over already-read `(registry_id, read, write)` counters:
    /// `None` on the first (baseline) sample.
    fn advance(&mut self, samples: &[(u64, u64, u64)], now: Instant) -> Option<Vec<DiskDevice>> {
        let dt = self
            .last
            .map(|t| now.saturating_duration_since(t).as_secs_f64());
        self.last = Some(now);
        let devices = self.rates.advance(samples, dt.unwrap_or(0.0));
        dt.map(|_| devices)
    }
}

impl Collector for DiskCollector {
    fn id(&self) -> CollectorId {
        CollectorId::Disk
    }

    fn collect(&mut self, now: Instant, snap: &mut Snapshot) -> Result<Outcome> {
        if cfg!(not(target_os = "macos")) {
            return Ok(Outcome::Absent);
        }
        self.refresh_volumes(now);
        let devices = self.rates(now)?;
        snap.disk.volumes.clone_from(&self.volumes);
        Ok(match devices {
            Some(devices) => {
                (snap.disk.read_bps, snap.disk.write_bps) = totals(&devices);
                snap.disk.devices = devices;
                Outcome::Fresh
            }
            None => Outcome::Baseline,
        })
    }
}

fn is_apfs_or_hfs(fstype: &str) -> bool {
    fstype.eq_ignore_ascii_case("apfs") || fstype.eq_ignore_ascii_case("hfs")
}

fn skipped_fstype(fstype: &str) -> bool {
    SKIP_FSTYPES
        .iter()
        .any(|skip| fstype.eq_ignore_ascii_case(skip))
}

fn skipped_mount(mount: &str) -> bool {
    SKIP_MOUNT_PREFIXES.iter().any(|prefix| {
        mount
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

fn is_user_volume(mount: &str) -> bool {
    mount
        .strip_prefix("/Volumes/")
        .is_some_and(|rest| !rest.is_empty())
}

fn keep_volume(fstype: &str, mount: &str) -> bool {
    if skipped_fstype(fstype) || skipped_mount(mount) || mount.is_empty() {
        return false;
    }
    mount == "/" || is_user_volume(mount) || is_apfs_or_hfs(fstype)
}

/// [`keep_volume`] plus the Finder's own rule: `MNT_DONTBROWSE` volumes
/// (Recovery, Data, VM…) are hidden, except `/` (D33).
pub(crate) fn keep_mount(fstype: &str, mount: &str, flags: u32) -> bool {
    if flags & MNT_DONTBROWSE != 0 && mount != "/" {
        return false;
    }
    keep_volume(fstype, mount)
}

/// `MNT_LOCAL` from `<sys/mount.h>`.
pub(crate) const MNT_LOCAL: u32 = 0x0000_1000;

/// Volume display names, looked up once per mount. `getattrlist` goes to
/// the filesystem, and on an unreachable SMB or hard NFS share it blocks
/// (the classic `df` hang), freezing every metric on the sampler thread;
/// the name cannot change while mounted anyway.
#[derive(Debug, Default)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct VolNames {
    names: HashMap<(String, String), String>,
    seen: HashMap<(String, String), String>,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl VolNames {
    /// The name of `mount` (device `from`): cached, or `lookup` for a local
    /// mount not seen before (a failed lookup is retried next refresh). A
    /// network mount is never asked; it is named after its path.
    pub(crate) fn name(
        &mut self,
        mount: &str,
        from: &str,
        flags: u32,
        lookup: impl FnOnce(&str) -> Option<String>,
    ) -> String {
        let key = (mount.to_owned(), from.to_owned());
        let name = match self.names.remove(&key) {
            Some(name) => name,
            None if flags & MNT_LOCAL == 0 => volume_name(mount),
            None => match lookup(mount) {
                Some(name) => name,
                None => return volume_name(mount),
            },
        };
        self.seen.insert(key, name.clone());
        name
    }

    /// Ends a refresh: mounts not named since the last one are forgotten.
    pub(crate) fn sweep(&mut self) {
        self.names = std::mem::take(&mut self.seen);
    }
}

/// Fallback display name when `getattrlist` has none: the last path
/// component, or `/` for the root.
fn volume_name(mount: &str) -> String {
    mount
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(mount)
        .to_owned()
}

/// The APFS container (whole disk) of a `/dev/diskNsM[sK]` device, e.g.
/// `/dev/disk3s1s1` → `disk3`. `None` for anything not under `/dev/disk`.
pub(crate) fn container_of(from: &str) -> Option<&str> {
    let rest = from.strip_prefix("/dev/")?;
    let digits = rest.strip_prefix("disk")?;
    let n = digits.bytes().take_while(u8::is_ascii_digit).count();
    if n == 0 {
        return None;
    }
    rest.get(..4 + n)
}

/// One line per container: APFS volumes that share a container share its
/// capacity, so keep the root (or the first seen) and drop the rest.
/// Non-APFS volumes, or ones without a container, are always kept.
pub(crate) fn dedupe_containers(volumes: Vec<(Option<String>, DiskVolume)>) -> Vec<DiskVolume> {
    let mut out: Vec<(Option<String>, DiskVolume)> = Vec::new();
    for (key, vol) in volumes {
        let slot = key
            .as_ref()
            .and_then(|k| out.iter().position(|(ok, _)| ok.as_ref() == Some(k)));
        match slot {
            Some(i) if vol.mount == "/" => out[i] = (key, vol),
            Some(_) => {}
            None => out.push((key, vol)),
        }
    }
    out.into_iter().map(|(_, v)| v).collect()
}

/// Parse a `getattrlist(ATTR_CMN_… | ATTR_VOL_NAME)` reply buffer:
/// `u32 length`, then an `attrreference_t { i32 offset; u32 length }` whose
/// offset is relative to the reference itself; the name is NUL-terminated.
pub(crate) fn parse_vol_name(buf: &[u8]) -> Option<String> {
    let total = usize::try_from(u32::from_ne_bytes(buf.get(0..4)?.try_into().ok()?)).ok()?;
    let buf = buf.get(..total.min(buf.len()))?;
    let off = i32::from_ne_bytes(buf.get(4..8)?.try_into().ok()?);
    let len = usize::try_from(u32::from_ne_bytes(buf.get(8..12)?.try_into().ok()?)).ok()?;
    let start = 4_usize.checked_add(usize::try_from(off).ok()?)?;
    let raw = buf.get(start..start.checked_add(len)?)?;
    let n = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let name = std::str::from_utf8(&raw[..n]).ok()?;
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{REMATCH_EVERY, counted};
    use super::{
        VolNames, container_of, dedupe_containers, is_apfs_or_hfs, keep_mount, parse_vol_name,
    };
    use crate::cadence::Pace;
    use crate::iokit::{self, CfKey, IoObject};
    use siltop_core::DiskVolume;
    use std::ffi::CString;
    use std::ptr;
    use std::time::Instant;

    /// Matched `IOBlockStorageDriver`s with their registry entry ids.
    pub(super) struct IoReader {
        pub(super) services: Vec<(IoObject, u64)>,
        matched_at: Pace,
        stats: CfKey,
        read: CfKey,
        write: CfKey,
        protocol: CfKey,
        location: CfKey,
    }

    impl IoReader {
        pub(super) fn new() -> Option<Self> {
            Some(Self {
                services: Vec::new(),
                matched_at: Pace::default(),
                stats: CfKey::new(c"Statistics")?,
                read: CfKey::new(c"Bytes (Read)")?,
                write: CfKey::new(c"Bytes (Write)")?,
                protocol: CfKey::new(c"Protocol Characteristics")?,
                location: CfKey::new(c"Physical Interconnect Location")?,
            })
        }

        /// Re-enumerate drivers; the ports are replaced but registry ids
        /// are stable, so rate state keyed by id carries over without a
        /// spike. A driver without an id is dropped (and released), and so
        /// is a file-backed disk image's: its bytes already count on the
        /// physical disk that holds the file.
        fn rematch(&mut self) {
            self.services = iokit::services(c"IOBlockStorageDriver")
                .into_iter()
                .filter(|driver| counted(self.location_of(driver).as_deref()))
                .filter_map(|service| {
                    let id = service.registry_id()?;
                    Some((service, id))
                })
                .collect();
        }

        /// The `Physical Interconnect Location` of the block device under
        /// `driver`: "Internal", "External", "File", "RAM", ...
        pub(super) fn location_of(&self, driver: &IoObject) -> Option<String> {
            let device = iokit::parent(driver, c"IOService")?;
            let protocol = iokit::property(&device, &self.protocol)?;
            iokit::cf_dict_get(protocol.borrow(), &self.location).and_then(iokit::cf_string)
        }

        /// `(registry_id, read_bytes, write_bytes)` for every driver that
        /// answered, rematching every [`REMATCH_EVERY`] for hot-plug.
        pub(super) fn samples(&mut self, now: Instant) -> Vec<(u64, u64, u64)> {
            if self.matched_at.due(now, REMATCH_EVERY) || self.services.is_empty() {
                self.rematch();
                self.matched_at.ran(now);
            }
            self.services
                .iter()
                .filter_map(|(service, id)| self.bytes_for(service).map(|(r, w)| (*id, r, w)))
                .collect()
        }

        fn bytes_for(&self, service: &IoObject) -> Option<(u64, u64)> {
            let stats = iokit::property(service, &self.stats)?;
            let stats = stats.borrow();
            let counter = |key: &CfKey| {
                iokit::cf_dict_get(stats, key)
                    .and_then(iokit::cf_i64)
                    .and_then(|n| u64::try_from(n).ok())
            };
            match (counter(&self.read), counter(&self.write)) {
                (None, None) => None,
                (read, write) => Some((read.unwrap_or(0), write.unwrap_or(0))),
            }
        }
    }

    pub(super) fn volumes(names: &mut VolNames) -> Vec<DiskVolume> {
        let mut buf: *mut libc::statfs = ptr::null_mut();
        let n = unsafe {
            // SAFETY: `buf` is an out-pointer; getmntinfo writes a libc-owned array.
            libc::getmntinfo(&raw mut buf, libc::MNT_NOWAIT)
        };
        if n <= 0 || buf.is_null() {
            return Vec::new();
        }
        let entries = unsafe {
            // SAFETY: getmntinfo returned `n` statfs records in a static buffer.
            std::slice::from_raw_parts(buf, usize::try_from(n).unwrap_or(0))
        };
        let mut volumes = Vec::new();
        for fs in entries {
            let mount = c_fixed_str(&fs.f_mntonname);
            let fstype = c_fixed_str(&fs.f_fstypename);
            if !keep_mount(&fstype, &mount, fs.f_flags) {
                continue;
            }
            let from = c_fixed_str(&fs.f_mntfromname);
            let key = is_apfs_or_hfs(&fstype)
                .then(|| container_of(&from).map(str::to_owned))
                .flatten();
            let bsize = block_size(fs);
            let total_bytes = fs.f_blocks.saturating_mul(bsize);
            let used_bytes = fs.f_blocks.saturating_sub(fs.f_bfree).saturating_mul(bsize);
            let name = names.name(&mount, &from, fs.f_flags, vol_name);
            volumes.push((
                key,
                DiskVolume {
                    name: siltop_core::into_printable(name),
                    mount: siltop_core::into_printable(mount),
                    used_bytes,
                    total_bytes,
                },
            ));
        }
        names.sweep();
        dedupe_containers(volumes)
    }

    /// The user-visible volume name ("Macintosh HD") via `getattrlist`.
    pub(super) fn vol_name(mount: &str) -> Option<String> {
        let path = CString::new(mount).ok()?;
        let mut attrs = libc::attrlist {
            bitmapcount: libc::ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: 0,
            volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_NAME,
            dirattr: 0,
            fileattr: 0,
            forkattr: 0,
        };
        let mut buf = [0_u8; 4 + 8 + 1024];
        let rc = unsafe {
            // SAFETY: `path` is a valid C string, `attrs` a valid attrlist and
            // `buf` writable for its full length.
            libc::getattrlist(
                path.as_ptr(),
                (&raw mut attrs).cast(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                0,
            )
        };
        if rc != 0 {
            return None;
        }
        parse_vol_name(&buf)
    }

    fn block_size(fs: &libc::statfs) -> u64 {
        if fs.f_bsize > 0 {
            u64::from(fs.f_bsize)
        } else {
            u64::try_from(fs.f_iosize).unwrap_or(0)
        }
    }

    fn c_fixed_str(buf: &[libc::c_char]) -> String {
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let bytes: Vec<u8> = buf[..n].iter().map(|&c| c as u8).collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn vol(name: &str, mount: &str) -> DiskVolume {
        DiskVolume {
            name: String::from(name),
            mount: String::from(mount),
            used_bytes: 1,
            total_bytes: 2,
        }
    }

    /// `mount` output captured on this machine (Mac17,8, macOS 27):
    /// (from, mount, fstype, nobrowse).
    const LIVE_MOUNTS: &[(&str, &str, &str, bool)] = &[
        ("/dev/disk3s1s1", "/", "apfs", false),
        ("devfs", "/dev", "devfs", true),
        ("/dev/disk3s6", "/System/Volumes/VM", "apfs", true),
        ("/dev/disk3s2", "/System/Volumes/Preboot", "apfs", true),
        ("/dev/disk3s4", "/System/Volumes/Update", "apfs", true),
        ("/dev/disk1s2", "/System/Volumes/xarts", "apfs", true),
        ("/dev/disk1s1", "/System/Volumes/iSCPreboot", "apfs", true),
        ("/dev/disk1s3", "/System/Volumes/Hardware", "apfs", true),
        ("/dev/disk3s5", "/System/Volumes/Data", "apfs", true),
        ("map auto_home", "/System/Volumes/Data/home", "autofs", true),
        ("/dev/disk3s3", "/Volumes/Recovery", "apfs", true),
    ];

    fn pipeline(mounts: &[(&str, &str, &str, bool)]) -> Vec<DiskVolume> {
        let kept = mounts
            .iter()
            .filter(|(_, m, t, nb)| keep_mount(t, m, if *nb { MNT_DONTBROWSE } else { 0 }))
            .map(|(from, m, t, _)| {
                let key = is_apfs_or_hfs(t)
                    .then(|| container_of(from).map(str::to_owned))
                    .flatten();
                (key, vol(&volume_name(m), m))
            })
            .collect();
        dedupe_containers(kept)
    }

    #[test]
    fn d33_recovery_and_hidden_volumes_are_gone() {
        let out = pipeline(LIVE_MOUNTS);
        let mounts: Vec<_> = out.iter().map(|v| v.mount.as_str()).collect();
        assert_eq!(mounts, ["/"]);
    }

    #[test]
    fn d33_nobrowse_filter_table() {
        let table = [
            ("apfs", "/", MNT_DONTBROWSE, true),
            ("apfs", "/", 0, true),
            ("apfs", "/Volumes/Recovery", MNT_DONTBROWSE, false),
            ("apfs", "/Volumes/Recovery", 0, true),
            ("apfs", "/System/Volumes/Data", MNT_DONTBROWSE, false),
            ("exfat", "/Volumes/USB", 0, true),
            ("exfat", "/Volumes/USB", MNT_DONTBROWSE | 0x1, false),
            ("msdos", "/Volumes/CARD", 0x1, true),
        ];
        for (fstype, mount, flags, want) in table {
            assert_eq!(keep_mount(fstype, mount, flags), want, "{mount} {flags:#x}");
        }
    }

    #[test]
    fn container_of_table() {
        let table = [
            ("/dev/disk3s1s1", Some("disk3")),
            ("/dev/disk3s5", Some("disk3")),
            ("/dev/disk12s2", Some("disk12")),
            ("/dev/disk4", Some("disk4")),
            ("/dev/disk", None),
            ("/dev/diskX", None),
            ("devfs", None),
            ("map auto_home", None),
            ("//user@server/share", None),
            ("", None),
        ];
        for (from, want) in table {
            assert_eq!(container_of(from), want, "{from}");
        }
    }

    #[test]
    fn container_dedupe_prefers_root_and_keeps_externals() {
        let out = dedupe_containers(vec![
            (Some("disk3".into()), vol("Data", "/System/Volumes/Data")),
            (Some("disk3".into()), vol("Macintosh HD", "/")),
            (Some("disk5".into()), vol("T7", "/Volumes/T7")),
            (Some("disk5".into()), vol("T7 Backup", "/Volumes/T7 Backup")),
            (None, vol("USB", "/Volumes/USB")),
            (None, vol("CARD", "/Volumes/CARD")),
        ]);
        let mounts: Vec<_> = out.iter().map(|v| v.mount.as_str()).collect();
        assert_eq!(
            mounts,
            ["/", "/Volumes/T7", "/Volumes/USB", "/Volumes/CARD"]
        );
    }

    #[test]
    fn container_dedupe_empty_and_single() {
        assert!(dedupe_containers(Vec::new()).is_empty());
        let one = dedupe_containers(vec![(Some("disk3".into()), vol("Data", "/x"))]);
        assert_eq!(one.len(), 1);
    }

    #[test]
    fn container_dedupe_sweep_one_line_per_key() {
        let mut input = Vec::new();
        for i in 0..200_u32 {
            let key = format!("disk{}", i % 7);
            input.push((Some(key), vol("v", &format!("/Volumes/v{i}"))));
        }
        let out = dedupe_containers(input);
        assert_eq!(out.len(), 7);
        assert_eq!(out[0].mount, "/Volumes/v0");
    }

    #[test]
    fn keep_root_and_user_volumes() {
        assert!(keep_volume("apfs", "/"));
        assert!(keep_volume("hfs", "/"));
        assert!(keep_volume("apfs", "/Volumes/External"));
        assert!(keep_volume("exfat", "/Volumes/USB"));
        assert!(keep_volume("APFS", "/"));
        assert!(keep_volume("apfs", "/System/Volumes/Data"));
    }

    #[test]
    fn skip_virtual_and_system_mounts() {
        assert!(!keep_volume("devfs", "/dev"));
        assert!(!keep_volume("autofs", "/System/Volumes/Data/home"));
        assert!(!keep_volume("fdesc", "/dev"));
        assert!(!keep_volume("apfs", "/dev"));
        assert!(!keep_volume("apfs", "/dev/disk3s1"));
        assert!(!keep_volume("apfs", "/System/Volumes/VM"));
        assert!(!keep_volume("apfs", "/System/Volumes/Preboot"));
        assert!(!keep_volume("apfs", "/System/Volumes/Update"));
        assert!(!keep_volume("apfs", "/System/Volumes/Update/mnt1"));
        assert!(!keep_volume("apfs", "/System/Volumes/xarts"));
        assert!(!keep_volume("apfs", "/System/Volumes/iSCPreboot"));
        assert!(!keep_volume("apfs", "/System/Volumes/Hardware"));
        assert!(!keep_volume("nfs", "/mnt/server"));
        assert!(!keep_volume("apfs", ""));
    }

    #[test]
    fn a_network_mount_is_never_asked_for_its_name() {
        let mut names = VolNames::default();
        let name = names.name("/Volumes/Projects", "//nas/Projects", 0, |_| {
            panic!("getattrlist on a network share can hang the sampler")
        });
        assert_eq!(name, "Projects");
    }

    #[test]
    fn a_local_volume_name_is_looked_up_once_per_mount() {
        let mut names = VolNames::default();
        let mut asked = 0;
        for _ in 0..3 {
            let name = names.name("/", "/dev/disk3s1s1", MNT_LOCAL, |_| {
                asked += 1;
                Some(String::from("Macintosh HD"))
            });
            assert_eq!(name, "Macintosh HD");
            names.sweep();
        }
        assert_eq!(asked, 1);
        // A failed lookup falls back to the path and is retried.
        let failed = names.name("/Volumes/USB", "/dev/disk9s1", MNT_LOCAL, |_| None);
        assert_eq!(failed, "USB");
        names.sweep();
        let retried = names.name("/Volumes/USB", "/dev/disk9s1", MNT_LOCAL, |_| {
            Some(String::from("Backup"))
        });
        assert_eq!(retried, "Backup");
        // A mount gone for one refresh is forgotten; a new device there is asked.
        names.sweep();
        names.sweep();
        let again = names.name("/Volumes/USB", "/dev/disk9s1", MNT_LOCAL, |_| {
            Some(String::from("Renamed"))
        });
        assert_eq!(again, "Renamed");
    }

    #[test]
    fn volume_name_fallback_from_mount() {
        assert_eq!(volume_name("/"), "/");
        assert_eq!(volume_name("/Volumes/External SSD"), "External SSD");
        assert_eq!(volume_name("/System/Volumes/Data"), "Data");
        assert_eq!(volume_name("/Volumes/Ünïcödé 💾"), "Ünïcödé 💾");
    }

    fn attr_buf(name: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        let total = u32::try_from(12 + name.len()).unwrap();
        buf.extend_from_slice(&total.to_ne_bytes());
        buf.extend_from_slice(&8_i32.to_ne_bytes());
        buf.extend_from_slice(&u32::try_from(name.len()).unwrap().to_ne_bytes());
        buf.extend_from_slice(name);
        buf
    }

    #[test]
    fn parse_vol_name_real_and_unicode() {
        assert_eq!(
            parse_vol_name(&attr_buf(b"Macintosh HD\0")).as_deref(),
            Some("Macintosh HD")
        );
        let uni = "Ünïcödé 💾\0";
        assert_eq!(
            parse_vol_name(&attr_buf(uni.as_bytes())).as_deref(),
            Some("Ünïcödé 💾")
        );
    }

    #[test]
    fn parse_vol_name_rejects_bad_buffers() {
        assert_eq!(parse_vol_name(&[]), None);
        assert_eq!(parse_vol_name(&attr_buf(b"\0")), None);
        let mut oob = attr_buf(b"Macintosh HD\0");
        oob[4..8].copy_from_slice(&1000_i32.to_ne_bytes());
        assert_eq!(parse_vol_name(&oob), None);
        let mut neg = attr_buf(b"Macintosh HD\0");
        neg[4..8].copy_from_slice(&(-8_i32).to_ne_bytes());
        assert_eq!(parse_vol_name(&neg), None);
        let mut huge = attr_buf(b"Macintosh HD\0");
        huge[8..12].copy_from_slice(&u32::MAX.to_ne_bytes());
        assert_eq!(parse_vol_name(&huge), None);
        let mut bad_utf8 = attr_buf(b"\xff\xfe\0");
        bad_utf8[0] = 15;
        assert_eq!(parse_vol_name(&bad_utf8), None);
    }

    #[test]
    fn parse_vol_name_garbage_sweep_never_panics() {
        let mut x = 0x2545_F491_u32;
        for len in 0..300 {
            let buf: Vec<u8> = (0..len)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    x.to_le_bytes()[1]
                })
                .collect();
            let _ = parse_vol_name(&buf);
        }
    }

    #[test]
    fn d17_new_device_first_delta_is_zero() {
        let mut st = DeviceRates::default();
        // The internal SSD's lifetime total is 2.58 TB: no spike on first sight.
        let d = st.advance(&[(0x1000_0042, 2_580_000_000_000, 1_900_000_000_000)], 0.5);
        assert_eq!((d[0].read_bps, d[0].write_bps), (0, 0));
        let d = st.advance(&[(0x1000_0042, 2_580_000_001_000, 1_900_000_000_000)], 0.5);
        assert_eq!(d[0].read_bps, 2000);
    }

    #[test]
    fn d17_hot_plugged_drive_does_not_spike_total() {
        let mut st = DeviceRates::default();
        st.advance(&[(1, 1000, 1000)], 1.0);
        let d = st.advance(&[(1, 2000, 1000), (2, 9_000_000_000_000, 5)], 1.0);
        assert_eq!(totals(&d), (1000, 0));
    }

    #[test]
    fn d17_missing_sample_keeps_previous_no_spike_on_return() {
        let mut st = DeviceRates::default();
        st.advance(&[(1, 1000, 0), (2, 5_000_000, 0)], 1.0);
        let d = st.advance(&[(1, 2000, 0)], 1.0);
        assert_eq!(totals(&d), (1000, 0));
        let d = st.advance(&[(1, 3000, 0), (2, 5_000_500, 0)], 1.0);
        assert_eq!(totals(&d), (1500, 0));
    }

    #[test]
    fn d17_rematch_reorders_devices_without_spike() {
        let mut st = DeviceRates::default();
        st.advance(&[(1, 100, 100), (2, 200, 200)], 1.0);
        let d = st.advance(&[(2, 300, 200), (1, 100, 150)], 1.0);
        let by_id: HashMap<u64, _> = d.iter().map(|x| (x.registry_id, *x)).collect();
        assert_eq!(by_id[&2].read_bps, 100);
        assert_eq!(by_id[&1].write_bps, 50);
    }

    #[test]
    fn counter_going_backwards_reads_zero() {
        let mut st = DeviceRates::default();
        st.advance(&[(1, 1_000_000, 1_000_000)], 1.0);
        let d = st.advance(&[(1, 10, 10)], 1.0);
        assert_eq!(totals(&d), (0, 0));
        let d = st.advance(&[(1, 20, 30)], 1.0);
        assert_eq!(totals(&d), (10, 20));
    }

    #[test]
    fn device_rates_bad_dt_is_zero() {
        for dt in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let mut st = DeviceRates::default();
            st.advance(&[(1, 0, 0)], 1.0);
            let d = st.advance(&[(1, 1000, 1000)], dt);
            assert_eq!(totals(&d), (0, 0), "dt {dt}");
        }
    }

    #[test]
    fn device_rates_sweep_totals_equal_sum_of_deltas() {
        let mut st = DeviceRates::default();
        let mut counters = [0_u64; 5];
        st.advance(
            &counters
                .iter()
                .enumerate()
                .map(|(i, &c)| (i as u64, c, c))
                .collect::<Vec<_>>(),
            1.0,
        );
        for step in 1..300_u64 {
            let mut want = 0;
            for (i, c) in counters.iter_mut().enumerate() {
                let inc = (step * (i as u64 + 1) * 4099) % 100_000;
                *c += inc;
                want += inc;
            }
            let samples: Vec<_> = counters
                .iter()
                .enumerate()
                .map(|(i, &c)| (i as u64, c, c / 2))
                .collect();
            let d = st.advance(&samples, 1.0);
            assert_eq!(totals(&d).0, want, "step {step}");
        }
    }

    #[test]
    fn disk_image_io_is_not_counted_twice_in_the_totals() {
        // A mounted .dmg's device reports "Virtual Interface" at location
        // "File"; its bytes also land on the SSD that holds the image file.
        assert!(!counted(Some("File")));
        // A RAM disk is "Virtual Interface" too, but its I/O touches no
        // other disk, so it is real throughput and not a double count.
        for other in ["Internal", "External", "RAM"] {
            assert!(counted(Some(other)), "{other}");
        }
        assert!(counted(None), "unknown location still counts");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_live_reader_matches_no_disk_image_driver() {
        let Some(mut io) = macos::IoReader::new() else {
            return;
        };
        let _ = io.samples(Instant::now());
        for (driver, _) in &io.services {
            assert!(counted(io.location_of(driver).as_deref()));
        }
    }

    #[test]
    fn totals_saturate() {
        let d = [
            DiskDevice {
                registry_id: 1,
                read_bps: u64::MAX,
                write_bps: 1,
            },
            DiskDevice {
                registry_id: 2,
                read_bps: 5,
                write_bps: u64::MAX,
            },
        ];
        assert_eq!(totals(&d), (u64::MAX, u64::MAX));
        assert_eq!(totals(&[]), (0, 0));
    }

    #[test]
    fn first_sample_is_only_a_baseline() {
        let mut col = DiskCollector::new();
        let mut snap = Snapshot::empty();
        let outcome = col.collect(Instant::now(), &mut snap).expect("disk");
        let expected = if cfg!(target_os = "macos") {
            Outcome::Baseline
        } else {
            Outcome::Absent
        };
        assert_eq!(outcome, expected);
        assert!(snap.disk.devices.is_empty(), "no made-up 0 B/s devices");
    }

    #[test]
    fn baseline_then_unchanged_counters_are_zero_rate() {
        let mut col = DiskCollector::new();
        let t0 = Instant::now();
        let samples = [(7, 2_580_000_000_000, 1_900_000_000_000), (9, 5, 5)];
        assert!(col.advance(&samples, t0).is_none());
        let d = col.advance(&samples, t0 + Duration::from_secs(1)).unwrap();
        assert_eq!(d.len(), 2);
        assert_eq!(totals(&d), (0, 0));
        assert!(d.iter().all(|d| d.read_bps == 0 && d.write_bps == 0));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_volumes_one_per_container_with_real_root_name() {
        let mut col = DiskCollector::new();
        let mut snap = Snapshot::empty();
        let t0 = Instant::now();
        let _ = col.collect(t0, &mut snap);
        let fresh = col.collect(t0 + Duration::from_millis(500), &mut snap);
        let snap = snap.disk;
        assert!(!snap.volumes.iter().any(|v| v.mount == "/Volumes/Recovery"));
        assert!(
            !snap
                .volumes
                .iter()
                .any(|v| v.mount == "/System/Volumes/Data")
        );
        if let Some(primary) = snap.primary() {
            assert!(primary.total_bytes > 0, "primary volume has no size");
            assert_eq!(primary.mount, "/");
            assert_ne!(primary.name, "/", "getattrlist name missing");
        }
        // A VM without block-storage statistics errors instead of faking 0.
        if !matches!(fresh, Ok(Outcome::Fresh)) {
            return;
        }
        assert!(!snap.devices.is_empty(), "no IOBlockStorageDriver");
        let ids: std::collections::HashSet<_> =
            snap.devices.iter().map(|d| d.registry_id).collect();
        assert_eq!(ids.len(), snap.devices.len(), "registry ids unique");
    }
}
