//! History on disk: append-only segment files of fixed-width records.
//!
//! Layout under `$XDG_STATE_HOME/siltop/history/` (else
//! `~/.local/state/siltop/history/`):
//!
//! ```text
//! day/<segment start unix s>.seg     6 s buckets, one file per 4 h
//! month/<segment start unix s>.seg   144 s buckets, one file per 3 days
//! ```
//!
//! Each file is a 16-byte header (magic, version, tier, series count, record
//! length) then records of exactly [`RECORD_LEN`] bytes, each ending in a
//! CRC-32 of the bytes before it. A crash can leave at most a torn last
//! record: readers skip it, the writer truncates it away before appending.
//! A record whose CRC fails is skipped alone; the rest of the file stands.
//!
//! Without a home directory there is no [`DiskStore`]: [`Recorder::new`]
//! hands back a session-only [`MemoryStore`].

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use siltop_core::store::{
    APP_N, Accumulator, Bucket, HistoryStore, MemoryStore, Named, Rollup, SERIES_COUNT, Series,
    Stat, TOP_N, Tier, TopProc, window,
};
use siltop_core::{Result, Snapshot, crc32};

const MAGIC: [u8; 8] = *b"PLTYHST\0";
const HEADER_LEN: usize = 16;
/// Process names are cut (on a char boundary) to this many bytes.
const NAME_LEN: usize = 16;
const TOP_LEN: usize = 4 + 4 + NAME_LEN;
/// One exact-energy app slot: processes folded, watts, name.
const APP_LEN: usize = 4 + 4 + NAME_LEN;

/// One on-disk record format. Each version only adds series columns and
/// trailing fields, so every version reads with the same decoder, told by
/// its layout how many columns and which trailing fields it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    version: u16,
    series: usize,
    /// Carries the exact per-app energy list and its "other" row.
    apps: bool,
    /// Carries the seconds the bucket actually recorded.
    covered: bool,
}

impl Layout {
    /// 1.0: fourteen series and the three top-5 lists.
    const V1: Self = Self {
        version: 1,
        series: Series::V1_COUNT,
        apps: false,
        covered: false,
    };
    /// 1.1: battery, power source, thermal state and health series, plus
    /// the exact per-app energy list.
    const V2: Self = Self {
        version: 2,
        series: Series::V2_COUNT,
        apps: true,
        covered: false,
    };
    /// 1.1: the CPU-hot share series and each bucket's recorded seconds, so
    /// reports stop counting a partly filled bucket as whole.
    const V3: Self = Self {
        version: 3,
        series: SERIES_COUNT,
        apps: true,
        covered: true,
    };
    /// Every layout a reader accepts, newest first.
    const KNOWN: [Self; 3] = [Self::V3, Self::V2, Self::V1];

    /// t, samples, series (mean, max), three top lists, apps, covered, CRC.
    const fn record_len(self) -> usize {
        let apps = if self.apps { APP_N * APP_LEN + 4 } else { 0 };
        let covered = if self.covered { 4 } else { 0 };
        8 + 4 + self.series * 8 + 3 * TOP_N * TOP_LEN + apps + covered + 4
    }

    fn header(self, tier: Tier) -> [u8; HEADER_LEN] {
        let mut h = [0_u8; HEADER_LEN];
        h[..8].copy_from_slice(&MAGIC);
        h[8..10].copy_from_slice(&self.version.to_le_bytes());
        h[10] = tier.id();
        h[11] = self.series as u8;
        h[12..16].copy_from_slice(&(self.record_len() as u32).to_le_bytes());
        h
    }

    /// The layout a file's first bytes announce for `tier`, if any.
    fn of(bytes: &[u8], tier: Tier) -> Option<Self> {
        let head = bytes.get(..HEADER_LEN)?;
        Self::KNOWN.into_iter().find(|l| head == l.header(tier))
    }
}

/// What this build writes.
const CURRENT: Layout = Layout::V3;
/// Bytes per record in the current layout.
pub const RECORD_LEN: usize = CURRENT.record_len();
/// Default ceiling on everything under the history directory.
pub const SIZE_CAP: u64 = 64 * 1024 * 1024;

/// Seconds one segment file covers at `tier`.
const fn segment_span(tier: Tier) -> u64 {
    match tier {
        Tier::Live | Tier::Day => 4 * 3600,
        Tier::Month => 3 * 86_400,
    }
}

/// Where saved history lives, if there is a home to put it in.
pub fn default_root() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("state"))
        })?;
    Some(base.join("siltop").join("history"))
}

/// Wall-clock unix seconds; 0 if the clock reads before 1970.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn header(tier: Tier) -> [u8; HEADER_LEN] {
    CURRENT.header(tier)
}

/// Creates `dir` and any missing parents readable by their owner only.
fn private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// The longest prefix of `name` that fits `max` bytes without splitting a char.
fn clip_utf8(name: &str, max: usize) -> &str {
    let mut end = name.len().min(max);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

/// `name` clipped into a zero-padded [`NAME_LEN`] field.
fn put_name(out: &mut Vec<u8>, name: &str) {
    let mut field = [0_u8; NAME_LEN];
    let clipped = clip_utf8(name, NAME_LEN).as_bytes();
    field[..clipped.len()].copy_from_slice(clipped);
    out.extend_from_slice(&field);
}

/// One bucket as a current-layout record. Absent series and empty slots
/// are NaN, so a reader can tell them from a real zero.
pub fn encode(bucket: &Bucket) -> Vec<u8> {
    encode_as(bucket, CURRENT)
}

fn encode_as(bucket: &Bucket, layout: Layout) -> Vec<u8> {
    let mut out = Vec::with_capacity(layout.record_len());
    out.extend_from_slice(&bucket.t.to_le_bytes());
    out.extend_from_slice(&bucket.samples.to_le_bytes());
    for stat in bucket.series.iter().take(layout.series) {
        let (mean, max) = stat.map_or((f32::NAN, f32::NAN), |s| (s.mean, s.max));
        out.extend_from_slice(&mean.to_le_bytes());
        out.extend_from_slice(&max.to_le_bytes());
    }
    for list in [&bucket.top_cpu, &bucket.top_energy, &bucket.top_mem] {
        for slot in 0..TOP_N {
            let (pid, value, name) = list
                .get(slot)
                .map_or((0, f32::NAN, ""), |p| (p.pid, p.value, p.name.as_str()));
            out.extend_from_slice(&pid.to_le_bytes());
            out.extend_from_slice(&value.to_le_bytes());
            put_name(&mut out, name);
        }
    }
    if layout.apps {
        for slot in 0..APP_N {
            let (count, value, name) = bucket.apps.get(slot).map_or((0, f32::NAN, ""), |a| {
                (
                    u32::try_from(a.count).unwrap_or(u32::MAX),
                    a.value,
                    a.name.as_str(),
                )
            });
            out.extend_from_slice(&count.to_le_bytes());
            out.extend_from_slice(&value.to_le_bytes());
            put_name(&mut out, name);
        }
        out.extend_from_slice(&bucket.apps_other.unwrap_or(f32::NAN).to_le_bytes());
    }
    if layout.covered {
        // 0 = unknown: a stored bucket always covers at least a second.
        out.extend_from_slice(&bucket.covered_s.unwrap_or(0).to_le_bytes());
    }
    let crc = crc32(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    out
}

/// Little cursor over a record; every read is in bounds by construction.
struct Cursor<'a>(&'a [u8]);

impl Cursor<'_> {
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0_u8; N];
        let n = N.min(self.0.len());
        out[..n].copy_from_slice(&self.0[..n]);
        self.0 = &self.0[n..];
        out
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take())
    }
    fn f32(&mut self) -> f32 {
        f32::from_le_bytes(self.take())
    }
    fn name(&mut self) -> String {
        let raw: [u8; NAME_LEN] = self.take();
        let len = raw.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
        // History written before names were cleaned at collection may hold
        // escape sequences; `report` and `export` print what this returns.
        siltop_core::into_printable(String::from_utf8_lossy(&raw[..len]).into_owned())
    }
}

/// The bucket in one current-layout record, or `None` if it is short or
/// its CRC fails.
#[cfg(test)] // readers go through `parse_segment`, which picks the layout
pub fn decode(record: &[u8]) -> Option<Bucket> {
    decode_as(record, CURRENT)
}

/// Decodes a `layout` record; series it predates stay absent.
fn decode_as(record: &[u8], layout: Layout) -> Option<Bucket> {
    let len = layout.record_len();
    if record.len() != len {
        return None;
    }
    let (body, tail) = record.split_at(len - 4);
    let mut crc = [0_u8; 4];
    crc.copy_from_slice(tail);
    if crc32(body) != u32::from_le_bytes(crc) {
        return None;
    }
    let mut c = Cursor(body);
    let t = u64::from_le_bytes(c.take());
    let mut bucket = Bucket::empty(t);
    bucket.samples = c.u32();
    for slot in bucket.series.iter_mut().take(layout.series) {
        let (mean, max) = (c.f32(), c.f32());
        *slot = (!mean.is_nan()).then_some(Stat { mean, max });
    }
    for list in [
        &mut bucket.top_cpu,
        &mut bucket.top_energy,
        &mut bucket.top_mem,
    ] {
        for _ in 0..TOP_N {
            let (pid, value, name) = (c.u32(), c.f32(), c.name());
            if !value.is_nan() {
                list.push(TopProc { pid, name, value });
            }
        }
    }
    if layout.apps {
        for _ in 0..APP_N {
            let (count, value, name) = (c.u32(), c.f32(), c.name());
            if !value.is_nan() {
                bucket.apps.push(Named {
                    name,
                    value,
                    count: count as usize,
                });
            }
        }
        let other = c.f32();
        bucket.apps_other = (!other.is_nan()).then_some(other);
    }
    if layout.covered {
        bucket.covered_s = Some(c.u32()).filter(|&s| s > 0);
    }
    Some(bucket)
}

/// What one segment file held.
#[derive(Debug, Default)]
pub struct SegmentRead {
    pub buckets: Vec<Bucket>,
    /// Whole records whose CRC failed.
    pub corrupt: usize,
    /// Bytes of a torn last record.
    pub torn: usize,
}

/// The records of a segment already in memory, in whatever known layout
/// its header names; an unknown header yields nothing rather than garbage.
fn parse_segment(bytes: &[u8], tier: Tier) -> SegmentRead {
    let mut out = SegmentRead::default();
    let Some(layout) = Layout::of(bytes, tier) else {
        return out;
    };
    let records = bytes[HEADER_LEN..].chunks_exact(layout.record_len());
    out.torn = records.remainder().len();
    for record in records {
        match decode_as(record, layout) {
            Some(bucket) => out.buckets.push(bucket),
            None => out.corrupt += 1,
        }
    }
    out
}

/// Reads a segment written by this or any earlier version.
pub fn read_segment(path: &Path, tier: Tier) -> io::Result<SegmentRead> {
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    Ok(parse_segment(&bytes, tier))
}

/// `(segment start, path)` for every segment of `tier`, oldest first.
fn segments(root: &Path, tier: Tier) -> Vec<(u64, PathBuf)> {
    let Ok(dir) = fs::read_dir(root.join(tier.label())) else {
        return Vec::new();
    };
    let mut out: Vec<(u64, PathBuf)> = dir
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let start = path
                .file_name()?
                .to_str()?
                .strip_suffix(".seg")?
                .parse()
                .ok()?;
            Some((start, path))
        })
        .collect();
    out.sort_unstable_by_key(|(start, _)| *start);
    out
}

/// The segment a tier is currently appending to.
#[derive(Debug)]
struct Open {
    start: u64,
    file: File,
}

/// The single-writer lock file under the history root.
const LOCK: &str = ".lock";

/// The on-disk store: the Month rollup in memory, everything else on disk.
///
/// One history root has one writer. The agent and any number of dashboards
/// all sample the same Mac, so every process that appends first takes an
/// advisory lock on [`LOCK`]; the others still read the shared files but
/// append nothing, and retry the lock on each bucket so one takes over
/// when the writer exits.
#[derive(Debug)]
pub struct DiskStore {
    root: PathBuf,
    cap: u64,
    rollup: Rollup,
    open: [Option<Open>; 2],
    /// Held while this store is the root's writer.
    lock: Option<File>,
    /// The newest Day bucket on disk when last recovered: a writer never
    /// appends one at or before it (the last writer may already have
    /// flushed that bucket).
    newest_day: Option<u64>,
}

impl DiskStore {
    /// Opens (creating) `root` and recovers the open Month span from the
    /// newest Day segment, so a restart does not lose it. Reading takes no
    /// lock; the first [`HistoryStore::append`] does.
    pub fn open(root: &Path, cap: u64) -> Result<Self> {
        for tier in Tier::STORED {
            private_dir(&root.join(tier.label()))?;
        }
        // Which apps ran when is nobody else's business, and a home
        // directory is often readable by every local account. This also
        // narrows a history directory an older version created 0755. Best
        // effort: a root owned by another account (say, made under sudo)
        // cannot be narrowed, and must not stop `report` reading it.
        #[cfg(unix)]
        let _ = fs::set_permissions(root, std::os::unix::fs::PermissionsExt::from_mode(0o700));
        let mut store = Self {
            root: root.to_path_buf(),
            cap,
            rollup: Rollup::new(Tier::Month),
            open: [None, None],
            lock: None,
            newest_day: None,
        };
        store.recover()?;
        Ok(store)
    }

    /// Rebuilds the open Month span from the newest Day segment.
    fn recover(&mut self) -> Result<()> {
        self.rollup = Rollup::new(Tier::Month);
        self.open = [None, None];
        self.newest_day = None;
        let month_end = self
            .newest(Tier::Month)
            .map_or(0, |b| b.t + Tier::Month.span_s());
        if let Some((_, path)) = segments(&self.root, Tier::Day).pop() {
            let day = read_segment(&path, Tier::Day)?.buckets;
            if let Some(last) = day.last() {
                self.newest_day = Some(last.t);
                let span = Tier::Month.floor(last.t).max(month_end);
                for bucket in day.into_iter().filter(|b| b.t >= span) {
                    self.rollup.push(bucket);
                }
            }
        }
        Ok(())
    }

    /// Whether this store is the root's writer, taking the lock if it is
    /// free. Winning it re-reads the tail another writer may have added.
    fn claim(&mut self) -> Result<bool> {
        if self.lock.is_some() {
            return Ok(true);
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.root.join(LOCK))?;
        match file.try_lock() {
            // A filesystem without advisory locks cannot arbitrate; write.
            Ok(()) | Err(std::fs::TryLockError::Error(_)) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Ok(false),
        }
        self.lock = Some(file);
        self.recover()?;
        Ok(true)
    }

    fn newest(&self, tier: Tier) -> Option<Bucket> {
        let (_, path) = segments(&self.root, tier).pop()?;
        read_segment(&path, tier).ok()?.buckets.pop()
    }

    fn slot(tier: Tier) -> usize {
        usize::from(tier == Tier::Month)
    }

    /// Appends one record, rotating to a new segment when `t` leaves the
    /// current one. Returns whether a new segment was started.
    fn write(&mut self, tier: Tier, bucket: &Bucket) -> Result<bool> {
        let start = bucket.t - bucket.t % segment_span(tier);
        let slot = Self::slot(tier);
        let rotated = self.open[slot].as_ref().is_none_or(|o| o.start != start);
        if rotated {
            let path = self.root.join(tier.label()).join(format!("{start}.seg"));
            self.open[slot] = Some(Open {
                start,
                file: open_segment(&path, tier)?,
            });
        }
        if let Some(open) = self.open[slot].as_mut() {
            open.file.write_all(&encode(bucket))?;
        }
        Ok(rotated)
    }

    /// Drops segments past retention, then the oldest until under the cap
    /// (Day before Month: the coarse tier is the one that answers "last week").
    fn sweep(&mut self, now: u64) {
        let mut all = Vec::new();
        for tier in Tier::STORED {
            for (start, path) in segments(&self.root, tier) {
                let end = start + segment_span(tier);
                let current = self.open[Self::slot(tier)]
                    .as_ref()
                    .is_some_and(|o| o.start == start);
                if !current && !tier.keeps(end.saturating_sub(tier.span_s()), now) {
                    let _ = fs::remove_file(&path);
                    continue;
                }
                let size = fs::metadata(&path).map_or(0, |m| m.len());
                all.push((tier, start, current, size, path));
            }
        }
        let mut total: u64 = all.iter().map(|(.., size, _)| *size).sum();
        for (_, _, current, size, path) in &all {
            if total <= self.cap {
                break;
            }
            if !current && fs::remove_file(path).is_ok() {
                total -= size;
            }
        }
    }

    /// Bytes currently on disk under the root.
    #[cfg(test)] // read by the history UI next; tests only for now
    pub fn bytes(&self) -> u64 {
        Tier::STORED
            .into_iter()
            .flat_map(|tier| segments(&self.root, tier))
            .map(|(_, path)| fs::metadata(path).map_or(0, |m| m.len()))
            .sum()
    }
}

/// Opens a segment for append: a new or foreign file gets a fresh header
/// (a foreign one is kept aside as `.bad`), an older layout is rewritten in
/// the current one first (so the segment open at upgrade time keeps its
/// history), and a torn tail is cut off.
fn open_segment(path: &Path, tier: Tier) -> io::Result<File> {
    let bytes = fs::read(path).ok();
    let layout = bytes.as_deref().and_then(|b| Layout::of(b, tier));
    match (layout, bytes.as_deref()) {
        (Some(l), Some(old)) if l != CURRENT => upgrade(path, tier, old)?,
        (None, _) if path.exists() => fs::rename(path, path.with_extension("bad"))?,
        _ => {}
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)?;
    match (layout, bytes) {
        (Some(CURRENT), Some(bytes)) => {
            let whole = HEADER_LEN + (bytes.len() - HEADER_LEN) / RECORD_LEN * RECORD_LEN;
            if whole != bytes.len() {
                file.set_len(whole as u64)?;
            }
        }
        (Some(_), _) => {}
        _ => file.write_all(&header(tier))?,
    }
    Ok(file)
}

/// Rewrites an older-layout segment in the current layout, via a temporary
/// file renamed over it, so a crash mid-way leaves the old file whole.
fn upgrade(path: &Path, tier: Tier, old: &[u8]) -> io::Result<()> {
    let mut bytes = header(tier).to_vec();
    for bucket in parse_segment(old, tier).buckets {
        bytes.extend_from_slice(&encode(&bucket));
    }
    let tmp = path.with_extension("upgrade");
    fs::write(&tmp, &bytes)?;
    fs::rename(&tmp, path)
}

impl HistoryStore for DiskStore {
    /// Writes only as the root's writer, and never a bucket at or before
    /// the newest one on disk: another process is (or was) recording it.
    /// A non-writer still rolls its own buckets up, so its open Month span
    /// follows the writer's instead of freezing at the one it opened with.
    fn append(&mut self, bucket: Bucket) -> Result<()> {
        let writer = self.claim()?;
        // Only the disk's newest at recovery gates: this recorder's own
        // later buckets may repeat a start after a wall clock steps back,
        // and the reader folds those into one.
        if self.newest_day.is_some_and(|t| bucket.t <= t) {
            return Ok(());
        }
        if !writer {
            self.rollup.push(bucket);
            return Ok(());
        }
        let now = bucket.t;
        let mut rotated = self.write(Tier::Day, &bucket)?;
        if let Some(coarse) = self.rollup.push(bucket) {
            rotated |= self.write(Tier::Month, &coarse)?;
        }
        if rotated {
            self.sweep(now);
        }
        Ok(())
    }

    fn buckets(&self, from: u64, to: u64, tier: Tier) -> Result<Vec<Bucket>> {
        let stored = tier.stored();
        let span = segment_span(stored);
        let mut out = Vec::new();
        for (start, path) in segments(&self.root, stored) {
            if start + span <= from || start >= to {
                continue;
            }
            out.extend(read_segment(&path, stored)?.buckets);
        }
        // Two writers before the lock existed left each bucket twice, and a
        // non-writer's open span can trail the writer's close by a bucket:
        // the stored copy (first) wins.
        let mut out = window(&out, self.rollup.pending(), from, to, tier);
        out.dedup_by_key(|b| b.t);
        Ok(out)
    }

    fn persistent(&self) -> bool {
        true
    }
}

/// The main loop's history writer: folds every snapshot into the open 6 s
/// bucket and hands each closed bucket to the store.
pub struct Recorder {
    acc: Accumulator,
    store: Box<dyn HistoryStore>,
}

impl Default for Recorder {
    fn default() -> Self {
        Self::session()
    }
}

impl Recorder {
    /// Session-only history: no home directory, tests, and the fallback
    /// after a disk error.
    pub fn session() -> Self {
        Self {
            acc: Accumulator::default(),
            store: Box::new(MemoryStore::new()),
        }
    }

    /// The disk store under `root`, or the session without one. Returns a
    /// warning when the disk store could not open.
    pub fn new(root: Option<&Path>) -> (Self, Option<String>) {
        let Some(root) = root else {
            return (Self::session(), None);
        };
        match DiskStore::open(root, SIZE_CAP) {
            Ok(store) => (
                Self {
                    acc: Accumulator::default(),
                    store: Box::new(store),
                },
                None,
            ),
            Err(err) => (
                Self::session(),
                Some(format!("history kept for this session only: {err}")),
            ),
        }
    }

    pub fn persistent(&self) -> bool {
        self.store.persistent()
    }

    pub fn store(&self) -> &dyn HistoryStore {
        self.store.as_ref()
    }

    /// Snapshots waiting in the open bucket.
    #[cfg(test)] // read by the history UI next; tests only for now
    pub fn open_samples(&self) -> u32 {
        self.acc.samples()
    }

    /// Adds one snapshot taken at unix second `now`. On a write error the
    /// recorder falls back to the session store and says so once.
    pub fn observe(&mut self, snapshot: &Snapshot, now: u64) -> Option<String> {
        let bucket = self.acc.add(snapshot, now)?;
        self.store.append(bucket).err().map(|err| {
            self.store = Box::new(MemoryStore::new());
            format!("history kept for this session only: {err}")
        })
    }
}

impl Drop for Recorder {
    /// Keeps the last partial bucket: a quit never drops up to 6 s.
    fn drop(&mut self) {
        if let Some(bucket) = self.acc.close() {
            let _ = self.store.append(bucket);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use siltop_core::store::Report;

    /// Three Day records written by an independent v1 encoder (the 1.0
    /// format, byte for byte): what a 1.0 user's history looks like.
    const V1_FIXTURE: &[u8] = include_bytes!("fixtures/history-v1-day.seg");
    const V1_START: u64 = 1_789_992_000;

    #[allow(clippy::unnecessary_wraps)] // reads as a series slot at call sites
    fn stat(mean: f32, max: f32) -> Option<Stat> {
        Some(Stat { mean, max })
    }

    /// A v2 bucket carrying every new column and the exact app list.
    fn bucket_v2(t: u64) -> Bucket {
        let mut b = bucket(t, 0.4);
        b.series[Series::BatteryPct.index()] = stat(81.0, 82.0);
        b.series[Series::BatteryW.index()] = stat(-7.5, -2.0);
        b.series[Series::OnAc.index()] = stat(0.0, 0.0);
        b.series[Series::ThermalState.index()] = stat(1.5, 2.0);
        b.series[Series::Throttled.index()] = stat(0.5, 1.0);
        b.series[Series::BatteryHealth.index()] = stat(97.0, 97.0);
        b.apps = vec![
            Named {
                name: "cargo".into(),
                value: 4.5,
                count: 12,
            },
            Named {
                name: "Safari".into(),
                value: 1.25,
                count: 3,
            },
        ];
        b.apps_other = Some(0.75);
        b
    }

    fn tempdir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "siltop-history-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn bucket(t: u64, cpu: f32) -> Bucket {
        let mut b = Bucket::empty(t);
        b.samples = 6;
        b.series[Series::Cpu.index()] = Some(Stat {
            mean: cpu,
            max: cpu + 0.1,
        });
        b.series[Series::Mem.index()] = Some(Stat {
            mean: 0.0,
            max: 0.0,
        });
        b.top_cpu = vec![
            TopProc {
                pid: 42,
                name: "WindowServer".into(),
                value: 31.5,
            },
            TopProc {
                pid: 7,
                name: "kernel_task".into(),
                value: 4.0,
            },
        ];
        b.top_energy = vec![TopProc {
            pid: 42,
            name: "WindowServer".into(),
            value: 1.25,
        }];
        b
    }

    #[test]
    fn record_len_is_fixed_and_encode_honours_it() {
        assert_eq!(RECORD_LEN, 8 + 4 + 21 * 8 + 15 * 24 + 8 * 24 + 4 + 4 + 4);
        assert_eq!(
            Layout::V2.record_len(),
            8 + 4 + 20 * 8 + 15 * 24 + 8 * 24 + 4 + 4
        );
        assert_eq!(Layout::V1.record_len(), 8 + 4 + 14 * 8 + 15 * 24 + 4);
        assert_eq!(encode(&bucket(0, 0.5)).len(), RECORD_LEN);
        assert_eq!(encode(&Bucket::empty(0)).len(), RECORD_LEN);
    }

    #[test]
    fn encode_decode_round_trips() {
        let b = bucket(1_790_000_004, 0.25);
        assert_eq!(decode(&encode(&b)), Some(b));
    }

    /// Regression (UI-DESIGN §2): an absent series comes back absent and a
    /// real zero comes back zero.
    #[test]
    fn absent_and_zero_survive_the_round_trip_distinctly() {
        let back = decode(&encode(&bucket(0, 0.5))).unwrap();
        assert_eq!(back.get(Series::Gpu), None);
        assert_eq!(back.get(Series::Mem).map(|s| s.mean), Some(0.0));
    }

    #[test]
    fn long_and_multibyte_names_are_clipped_on_a_char_boundary() {
        let mut b = bucket(0, 0.1);
        b.top_mem = vec![TopProc {
            pid: 1,
            name: "com.apple.WebKit.WebContent".into(),
            value: 9.0,
        }];
        b.top_energy = vec![TopProc {
            pid: 2,
            name: "日本語のプロセス".into(),
            value: 1.0,
        }];
        let back = decode(&encode(&b)).unwrap();
        assert_eq!(back.top_mem[0].name, "com.apple.WebKit");
        assert_eq!(back.top_energy[0].name, "日本語のプ");
        assert_eq!(clip_utf8("abc", 16), "abc");
    }

    #[test]
    fn names_saved_with_escape_sequences_are_read_back_printable() {
        // Regression: history from before names were cleaned at collection
        // replayed `\e]2;PWNED\a` into `report` and `export --csv`.
        let mut b = bucket(0, 0.1);
        b.top_mem = vec![TopProc {
            pid: 1,
            name: "\u{1b}]2;PWNED\u{7}".into(),
            value: 9.0,
        }];
        let back = decode(&encode(&b)).unwrap();
        assert_eq!(back.top_mem[0].name, "\u{FFFD}]2;PWNED\u{FFFD}");
    }

    /// Property: flipping any single byte of a record is caught by the CRC.
    #[test]
    fn crc_catches_every_single_byte_flip() {
        let record = encode(&bucket(600, 0.75));
        for i in 0..RECORD_LEN {
            let mut bad = record.clone();
            bad[i] ^= 0x5A;
            assert_eq!(decode(&bad), None, "byte {i}");
        }
        assert!(decode(&record[..RECORD_LEN - 1]).is_none());
    }

    #[test]
    fn a_torn_tail_is_tolerated_on_read_and_cut_before_the_next_append() {
        let root = tempdir("torn");
        let mut store = DiskStore::open(&root, SIZE_CAP).unwrap();
        store.append(bucket(6, 0.1)).unwrap();
        store.append(bucket(12, 0.2)).unwrap();
        drop(store);
        let (_, path) = segments(&root, Tier::Day).pop().unwrap();
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&encode(&bucket(18, 0.3))[..100]).unwrap();
        drop(file);
        let read = read_segment(&path, Tier::Day).unwrap();
        assert_eq!((read.buckets.len(), read.torn), (2, 100));

        let mut store = DiskStore::open(&root, SIZE_CAP).unwrap();
        store.append(bucket(24, 0.4)).unwrap();
        let read = read_segment(&path, Tier::Day).unwrap();
        let ts: Vec<u64> = read.buckets.iter().map(|b| b.t).collect();
        assert_eq!((ts, read.torn, read.corrupt), (vec![6, 12, 24], 0, 0));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_corrupt_record_is_skipped_and_its_neighbours_survive() {
        let root = tempdir("corrupt");
        let mut store = DiskStore::open(&root, SIZE_CAP).unwrap();
        for t in [6, 12, 18] {
            store.append(bucket(t, 0.1)).unwrap();
        }
        drop(store);
        let (_, path) = segments(&root, Tier::Day).pop().unwrap();
        let mut bytes = fs::read(&path).unwrap();
        bytes[HEADER_LEN + RECORD_LEN + 20] ^= 0xFF;
        fs::write(&path, bytes).unwrap();
        let read = read_segment(&path, Tier::Day).unwrap();
        let ts: Vec<u64> = read.buckets.iter().map(|b| b.t).collect();
        assert_eq!((ts, read.corrupt), (vec![6, 18], 1));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_foreign_header_is_set_aside_not_appended_to() {
        let root = tempdir("foreign");
        let dir = root.join("day");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("0.seg"), b"not a siltop file at all").unwrap();
        let mut store = DiskStore::open(&root, SIZE_CAP).unwrap();
        store.append(bucket(6, 0.5)).unwrap();
        assert!(dir.join("0.bad").exists());
        let back = store.buckets(0, 100, Tier::Day).unwrap();
        assert_eq!(back.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn disk_store_rolls_up_month_and_answers_ranges() {
        let root = tempdir("rollup");
        let mut store = DiskStore::open(&root, SIZE_CAP).unwrap();
        for i in 0..60 {
            store.append(bucket(i * 6, 0.5)).unwrap();
        }
        assert!(store.persistent());
        assert_eq!(
            store.range(Series::Cpu, 0, 360, Tier::Day).unwrap().len(),
            60
        );
        let month: Vec<u64> = store
            .buckets(0, u64::MAX, Tier::Month)
            .unwrap()
            .iter()
            .map(|b| b.t)
            .collect();
        assert_eq!(month, [0, 144, 288]);
        fs::remove_dir_all(root).unwrap();
    }

    /// Regression: a restart mid-span must not lose the open Month bucket.
    #[test]
    fn reopening_recovers_the_open_month_span() {
        let root = tempdir("recover");
        let mut store = DiskStore::open(&root, SIZE_CAP).unwrap();
        for i in 0..30 {
            store.append(bucket(i * 6, 0.5)).unwrap();
        }
        drop(store);
        let mut store = DiskStore::open(&root, SIZE_CAP).unwrap();
        // 144..180 was open; one more span closes it with all six buckets.
        store.append(bucket(288, 0.5)).unwrap();
        let month = store.buckets(144, 145, Tier::Month).unwrap();
        assert_eq!(month[0].samples, 6 * 6);
        fs::remove_dir_all(root).unwrap();
    }

    /// Fake clock: a week of appends keeps about a day of Day segments and
    /// every Month segment.
    #[test]
    fn retention_deletes_old_day_segments_with_a_fake_clock() {
        let root = tempdir("retention");
        let mut store = DiskStore::open(&root, SIZE_CAP).unwrap();
        let end = 7 * 86_400;
        let mut t = 0;
        while t < end {
            // One bucket per minute keeps the test quick; rotation is by t.
            store.append(bucket(t, 0.3)).unwrap();
            t += 60;
        }
        let day = segments(&root, Tier::Day);
        let oldest = day.first().unwrap().0;
        assert!(
            end - oldest <= 86_400 + 2 * segment_span(Tier::Day),
            "{oldest}"
        );
        let month = segments(&root, Tier::Month);
        assert_eq!(month.first().unwrap().0, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn size_cap_evicts_oldest_but_keeps_the_open_segments() {
        let root = tempdir("cap");
        let cap = (HEADER_LEN + 200 * RECORD_LEN) as u64;
        let mut store = DiskStore::open(&root, cap).unwrap();
        for i in 0..6_000 {
            store.append(bucket(i * 6, 0.3)).unwrap();
        }
        // The cap is checked at rotation; the open segments may run over.
        let open: u64 = store
            .open
            .iter()
            .flatten()
            .map(|o| o.file.metadata().unwrap().len())
            .sum();
        assert!(store.bytes() <= cap + open, "{}", store.bytes());
        assert!(!segments(&root, Tier::Day).is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn writes_segments_and_flushes_the_partial_bucket_on_drop() {
        let root = tempdir("disk");
        let (mut rec, warning) = Recorder::new(Some(&root));
        assert!(warning.is_none() && rec.persistent());
        let mut snap = Snapshot::empty();
        snap.primed = true;
        snap.cpu.scaled = 0.5;
        for now in 0..9 {
            rec.observe(&snap, now);
        }
        drop(rec);
        let (_, path) = segments(&root, Tier::Day).pop().unwrap();
        let ts: Vec<u64> = read_segment(&path, Tier::Day)
            .unwrap()
            .buckets
            .iter()
            .map(|b| b.t)
            .collect();
        assert_eq!(ts, [0, 6]);
        fs::remove_dir_all(root).unwrap();
    }

    /// Every Day `t` written to disk, raw: no read-side dedupe.
    fn day_ts_on_disk(root: &Path) -> Vec<u64> {
        segments(root, Tier::Day)
            .iter()
            .flat_map(|(_, path)| read_segment(path, Tier::Day).unwrap().buckets)
            .map(|b| b.t)
            .collect()
    }

    /// Regression: the agent and the dashboard each appended every 6 s
    /// bucket to the same segments, so reports counted time and energy
    /// twice.
    #[test]
    fn agent_and_dashboard_on_one_root_write_each_bucket_once() {
        let root = tempdir("two-writers");
        let (mut agent, _) = Recorder::new(Some(&root));
        let (mut dashboard, _) = Recorder::new(Some(&root));
        let mut snap = Snapshot::empty();
        snap.primed = true;
        snap.cpu.scaled = 0.5;
        for now in 0..300 {
            agent.observe(&snap, now);
            dashboard.observe(&snap, now);
        }
        let expected: Vec<u64> = (0..49).map(|i| i * 6).collect();
        assert_eq!(day_ts_on_disk(&root), expected);
        // Every closed Day bucket is in the Month tier exactly once.
        let month = agent.store().buckets(0, u64::MAX, Tier::Month).unwrap();
        assert_eq!(month.iter().map(|b| b.samples).sum::<u32>(), 294);
        drop((agent, dashboard));
        fs::remove_dir_all(root).unwrap();
    }

    /// Regression: when the writer quits mid-bucket, the next process takes
    /// over without writing that bucket (or an older one) again.
    #[test]
    fn a_reader_takes_over_when_the_writer_exits_without_a_duplicate() {
        let root = tempdir("handover");
        let (mut first, _) = Recorder::new(Some(&root));
        let (mut second, _) = Recorder::new(Some(&root));
        let mut snap = Snapshot::empty();
        snap.primed = true;
        for now in 0..16 {
            first.observe(&snap, now);
            second.observe(&snap, now);
        }
        drop(first); // flushes the partial bucket 12
        for now in 16..40 {
            second.observe(&snap, now);
        }
        assert_eq!(day_ts_on_disk(&root), [0, 6, 12, 18, 24, 30]);
        drop(second);
        assert_eq!(day_ts_on_disk(&root), [0, 6, 12, 18, 24, 30, 36]);
        fs::remove_dir_all(root).unwrap();
    }

    /// Regression: a dashboard that is not the writer kept the open Month
    /// span it recovered at start, so once the agent closed that span the
    /// dashboard's Month timeline showed it twice (stale partial + stored).
    #[test]
    fn a_non_writers_month_view_matches_the_writers() {
        let root = tempdir("non-writer-month");
        let (mut agent, _) = Recorder::new(Some(&root));
        let mut snap = Snapshot::empty();
        snap.primed = true;
        for now in 0..200 {
            agent.observe(&snap, now);
        }
        let (mut dashboard, _) = Recorder::new(Some(&root));
        for now in 200..400 {
            agent.observe(&snap, now);
            dashboard.observe(&snap, now);
        }
        let month = |rec: &Recorder| -> Vec<u64> {
            let buckets = rec.store().buckets(0, u64::MAX, Tier::Month).unwrap();
            buckets.iter().map(|b| b.t).collect()
        };
        assert_eq!(month(&agent), [0, 144, 288]);
        assert_eq!(month(&dashboard), month(&agent));
        drop((agent, dashboard));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reads_drop_duplicate_buckets_left_by_two_unlocked_writers() {
        let root = tempdir("dedupe");
        let mut store = DiskStore::open(&root, SIZE_CAP).unwrap();
        for t in [0, 6, 12] {
            store.append(bucket(t, 0.5)).unwrap();
            // What a second, unlocked 1.1 pre-release writer added.
            let open = store.open[0].as_mut().unwrap();
            open.file.write_all(&encode(&bucket(t, 0.5))).unwrap();
        }
        assert_eq!(day_ts_on_disk(&root), [0, 0, 6, 6, 12, 12]);
        let ts: Vec<u64> = store
            .buckets(0, u64::MAX, Tier::Day)
            .unwrap()
            .iter()
            .map(|b| b.t)
            .collect();
        assert_eq!(ts, [0, 6, 12]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn without_a_home_history_stays_in_the_session() {
        let (rec, warning) = Recorder::new(None);
        assert!(!rec.persistent() && warning.is_none());
    }

    #[test]
    fn an_unwritable_root_falls_back_with_one_warning() {
        let root = tempdir("unwritable");
        let file = root.join("plain-file");
        fs::write(&file, b"x").unwrap();
        let (rec, warning) = Recorder::new(Some(&file));
        assert!(!rec.persistent());
        assert!(warning.unwrap().contains("session only"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn default_root_is_under_state_home() {
        if let Some(root) = default_root() {
            assert!(root.ends_with("siltop/history"), "{}", root.display());
        }
    }

    #[test]
    fn v2_round_trips_battery_thermal_and_exact_apps() {
        let b = bucket_v2(V1_START);
        assert_eq!(decode(&encode(&b)), Some(b));
    }

    /// v3 keeps the hot share and the recorded seconds; 0 s reads unknown.
    #[test]
    fn v3_round_trips_the_hot_share_and_covered_seconds() {
        let mut b = bucket_v2(V1_START);
        b.series[Series::Hot.index()] = stat(0.25, 1.0);
        b.covered_s = Some(18);
        assert_eq!(decode(&encode(&b)), Some(b.clone()));
        b.covered_s = None;
        assert_eq!(decode(&encode(&b)).unwrap().covered_s, None);
    }

    /// A 1.1 pre-release (v2) segment still reads, with the v3 columns
    /// absent, and appending upgrades it in place.
    #[test]
    fn a_v2_segment_reads_and_upgrades_to_v3() {
        let root = tempdir("v2-upgrade");
        let dir = root.join("day");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{V1_START}.seg"));
        let mut bytes = Layout::V2.header(Tier::Day).to_vec();
        bytes.extend_from_slice(&encode_as(&bucket_v2(V1_START), Layout::V2));
        fs::write(&path, &bytes).unwrap();
        let read = parse_segment(&bytes, Tier::Day);
        assert_eq!(read.buckets, [bucket_v2(V1_START)]);
        assert_eq!(read.buckets[0].get(Series::Hot), None);
        let mut store = DiskStore::open(&root, SIZE_CAP).unwrap();
        store.append(bucket_v2(V1_START + 6)).unwrap();
        let bytes = fs::read(&path).unwrap();
        assert_eq!(Layout::of(&bytes, Tier::Day), Some(CURRENT));
        assert_eq!(store.buckets(0, u64::MAX, Tier::Day).unwrap().len(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    /// Regression: a wall clock stepped back re-records times already on
    /// disk; the store answers one bucket per start (both runs merged) and
    /// the report counts that time once.
    #[test]
    fn a_clock_stepped_back_neither_drops_nor_double_counts_history() {
        let root = tempdir("clock-back");
        let mut store = DiskStore::open(&root, SIZE_CAP).unwrap();
        let t0 = V1_START;
        for (t, cpu) in [(t0, 0.2), (t0 + 6, 0.4), (t0, 0.6), (t0 + 6, 0.8)] {
            let mut b = bucket(t, cpu);
            b.covered_s = Some(6);
            b.series[Series::SysPowerW.index()] = stat(36.0, 36.0);
            store.append(b).unwrap();
        }
        let back = store.buckets(0, u64::MAX, Tier::Day).unwrap();
        let ts: Vec<u64> = back.iter().map(|b| b.t).collect();
        assert_eq!(ts, [t0, t0 + 6]);
        let cpu = back[0].get(Series::Cpu).unwrap().mean;
        assert!((cpu - 0.4).abs() < 1e-6, "both runs kept: {cpu}");
        let r = Report::build(&back, Tier::Day, t0, t0 + 12, 5);
        assert_eq!(r.recorded_s, 12);
        let wh = r.system_wh.unwrap();
        assert!((wh - 36.0 * 12.0 / 3600.0).abs() < 1e-9, "{wh}");
        fs::remove_dir_all(root).unwrap();
    }

    /// Regression: "no process energy measured" and "every app is listed"
    /// are different; only the first is absent.
    #[test]
    fn an_absent_other_row_stays_absent_and_zero_stays_zero() {
        let mut b = bucket(0, 0.1);
        assert_eq!(decode(&encode(&b)).unwrap().apps_other, None);
        b.apps_other = Some(0.0);
        assert_eq!(decode(&encode(&b)).unwrap().apps_other, Some(0.0));
    }

    #[test]
    fn the_v1_encoder_matches_the_fixture_byte_for_byte() {
        let mut bytes = Layout::V1.header(Tier::Day).to_vec();
        for back in parse_segment(V1_FIXTURE, Tier::Day).buckets {
            bytes.extend_from_slice(&encode_as(&back, Layout::V1));
        }
        assert_eq!(bytes, V1_FIXTURE);
    }

    #[test]
    fn a_v1_fixture_segment_reads_with_new_series_absent() {
        let read = parse_segment(V1_FIXTURE, Tier::Day);
        assert_eq!((read.buckets.len(), read.corrupt, read.torn), (3, 0, 0));
        let b = &read.buckets[1];
        assert_eq!((b.t, b.samples), (V1_START + 6, 6));
        assert_eq!(b.get(Series::Cpu), stat(0.25, 0.5));
        assert_eq!(b.get(Series::CpuTempC), stat(62.0, 70.0));
        assert_eq!(b.get(Series::SysPowerW), stat(5.0, 9.0));
        assert_eq!(b.top_energy[0].name, "cargo");
        assert!(b.top_mem.is_empty());
        for s in &Series::ALL[Series::V1_COUNT..] {
            assert_eq!(b.get(*s), None, "{s:?} must be absent, not 0");
        }
        assert!(b.apps.is_empty() && b.apps_other.is_none());
    }

    #[test]
    fn a_v1_segment_with_a_torn_tail_still_reads() {
        let torn = &V1_FIXTURE[..V1_FIXTURE.len() - 10];
        let read = parse_segment(torn, Tier::Day);
        assert_eq!((read.buckets.len(), read.torn), (2, 478));
    }

    #[test]
    fn a_v1_header_for_another_tier_is_not_read() {
        assert!(parse_segment(V1_FIXTURE, Tier::Month).buckets.is_empty());
    }

    #[test]
    fn history_directories_are_readable_by_their_owner_only() {
        // Regression: history was created 0755 under a home directory every
        // local account can often read, exposing which apps ran when.
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempdir("private");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        let _store = DiskStore::open(&root, SIZE_CAP).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&root), 0o700, "an older 0755 root is narrowed");
        for tier in Tier::STORED {
            assert_eq!(mode(&root.join(tier.label())), 0o700, "{tier:?}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    /// 1.0 to 1.1: existing history keeps loading through the store.
    #[test]
    fn a_disk_store_answers_from_v1_history() {
        let root = tempdir("v1-read");
        fs::create_dir_all(root.join("day")).unwrap();
        fs::write(root.join("day").join(format!("{V1_START}.seg")), V1_FIXTURE).unwrap();
        let store = DiskStore::open(&root, SIZE_CAP).unwrap();
        let back = store.buckets(V1_START, V1_START + 60, Tier::Day).unwrap();
        assert_eq!(back.len(), 3);
        let temps = store
            .range(Series::CpuTempC, 0, u64::MAX, Tier::Day)
            .unwrap();
        assert_eq!(temps.len(), 3);
        fs::remove_dir_all(root).unwrap();
    }

    /// Regression: the segment open at upgrade time is a v1 file; appending
    /// v2 records to it (or setting it aside as foreign) would lose it.
    #[test]
    fn appending_to_a_v1_segment_upgrades_it_and_keeps_its_history() {
        let root = tempdir("v1-upgrade");
        let dir = root.join("day");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{V1_START}.seg"));
        fs::write(&path, V1_FIXTURE).unwrap();
        let mut store = DiskStore::open(&root, SIZE_CAP).unwrap();
        store.append(bucket_v2(V1_START + 18)).unwrap();
        assert!(!dir.join(format!("{V1_START}.bad")).exists());
        assert!(!path.with_extension("upgrade").exists());
        let bytes = fs::read(&path).unwrap();
        assert_eq!(Layout::of(&bytes, Tier::Day), Some(CURRENT));
        assert_eq!(bytes.len(), HEADER_LEN + 4 * RECORD_LEN);
        let back = store.buckets(0, u64::MAX, Tier::Day).unwrap();
        let ts: Vec<u64> = back.iter().map(|b| b.t).collect();
        assert_eq!(ts, [V1_START, V1_START + 6, V1_START + 12, V1_START + 18]);
        assert_eq!(back[1].get(Series::CpuTempC), stat(62.0, 70.0));
        assert_eq!(back[3], bucket_v2(V1_START + 18));
        fs::remove_dir_all(root).unwrap();
    }
}
