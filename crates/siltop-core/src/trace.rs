//! A recorded session: a compact, versioned binary trace of snapshots.
//!
//! ```text
//! magic "PTRACE\0\0" | version u16 LE | header record | frame records ...
//! record = payload length u32 LE | CRC-32 of payload u32 LE | payload
//! ```
//!
//! A frame's payload is the time since the previous frame (varint ms) and
//! then one slot per section (13 of them): `0` repeats the previous frame's
//! section byte for byte, `1` carries a fresh copy. The chip, the battery,
//! the volumes and the fans rarely change, so most frames only carry the
//! rates. Process lists are cut to the [`TRACE_PROCS`] busiest, without
//! their command lines.
//!
//! A read trace keeps each frame's section bytes, shared between frames
//! that repeat them, and decodes a frame only when asked for it.
//!
//! Readers stop at the first torn or corrupt record and keep everything
//! before it: a recording killed mid-write still replays. Pure: no clock
//! and no files (callers pass bytes and the instant frame 0 plays at).

use std::io::{self, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::snapshot::{
    Adapter, AwakeAssertion, BatterySnapshot, ChargeState, Cluster, ClusterKind, CollectorId,
    CoreSample, CpuCluster, CpuSnapshot, DiskDevice, DiskSnapshot, DiskVolume, FanMetric,
    FanSnapshot, GpuSnapshot, Health, HealthReport, IfaceRate, MemorySnapshot, NetworkSnapshot,
    PowerSnapshot, PowerSource, Pressure, Process, Reading, ReadingSource, Sampled,
    SensorsSnapshot, Snapshot, SocInfo, TempReading, Thermal, Throttle, TimeRemaining,
};

pub const TRACE_MAGIC: [u8; 8] = *b"PTRACE\0\0";
/// Bumped whenever a section's layout changes; readers refuse others.
pub const TRACE_VERSION: u16 = 1;
/// Processes kept per frame, busiest CPU first.
pub const TRACE_PROCS: usize = 32;
/// The file extension `record` writes.
pub const TRACE_EXT: &str = "ptrace";
/// Largest record a reader accepts; anything bigger is corruption.
const MAX_RECORD: usize = 16 * 1024 * 1024;

const CRC_TABLE: [u32; 256] = {
    let mut table = [0_u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 == 0 {
                c >> 1
            } else {
                0xEDB8_8320 ^ (c >> 1)
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

/// CRC-32 (IEEE, the zlib one).
#[must_use]
pub fn crc32(bytes: &[u8]) -> u32 {
    !bytes.iter().fold(!0_u32, |crc, &b| {
        CRC_TABLE[((crc ^ u32::from(b)) & 0xFF) as usize] ^ (crc >> 8)
    })
}

// ---------------------------------------------------------------- wire ----

/// A byte cursor that answers `None` instead of reading past the end.
struct Dec<'a> {
    bytes: &'a [u8],
}

impl<'a> Dec<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if n > self.bytes.len() {
            return None;
        }
        let (head, tail) = self.bytes.split_at(n);
        self.bytes = tail;
        Some(head)
    }

    fn byte(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn varint(&mut self) -> Option<u64> {
        let mut value = 0_u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            value |= u64::from(b & 0x7F) << shift;
            if b & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }

    /// A length prefix, refused when it claims more than the bytes left
    /// (every element takes at least one byte), so junk cannot allocate.
    fn len(&mut self) -> Option<usize> {
        usize::try_from(self.varint()?)
            .ok()
            .filter(|n| *n <= self.bytes.len())
    }
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let low = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(low);
            return;
        }
        out.push(low | 0x80);
    }
}

/// A value with a fixed binary form in the trace.
trait Wire: Sized {
    fn put(&self, out: &mut Vec<u8>);
    fn get(input: &mut Dec<'_>) -> Option<Self>;
}

macro_rules! wire_uint {
    ($($ty:ty),*) => {$(
        impl Wire for $ty {
            fn put(&self, out: &mut Vec<u8>) {
                put_varint(out, u64::from(*self));
            }
            fn get(input: &mut Dec<'_>) -> Option<Self> {
                <$ty>::try_from(input.varint()?).ok()
            }
        }
    )*};
}
wire_uint!(u8, u16, u32, u64);

macro_rules! wire_sint {
    ($($ty:ty),*) => {$(
        impl Wire for $ty {
            fn put(&self, out: &mut Vec<u8>) {
                let v = i64::from(*self);
                put_varint(out, ((v << 1) ^ (v >> 63)) as u64);
            }
            fn get(input: &mut Dec<'_>) -> Option<Self> {
                let z = input.varint()?;
                let v = (z >> 1) as i64 ^ -((z & 1) as i64);
                <$ty>::try_from(v).ok()
            }
        }
    )*};
}
wire_sint!(i32, i64);

impl Wire for f32 {
    fn put(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_le_bytes());
    }
    fn get(input: &mut Dec<'_>) -> Option<Self> {
        Some(Self::from_le_bytes(input.take(4)?.try_into().ok()?))
    }
}

impl Wire for f64 {
    fn put(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_le_bytes());
    }
    fn get(input: &mut Dec<'_>) -> Option<Self> {
        Some(Self::from_le_bytes(input.take(8)?.try_into().ok()?))
    }
}

impl Wire for bool {
    fn put(&self, out: &mut Vec<u8>) {
        out.push(u8::from(*self));
    }
    fn get(input: &mut Dec<'_>) -> Option<Self> {
        match input.byte()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
}

impl Wire for String {
    fn put(&self, out: &mut Vec<u8>) {
        put_varint(out, self.len() as u64);
        out.extend_from_slice(self.as_bytes());
    }
    fn get(input: &mut Dec<'_>) -> Option<Self> {
        let n = input.len()?;
        String::from_utf8(input.take(n)?.to_vec()).ok()
    }
}

impl Wire for Duration {
    fn put(&self, out: &mut Vec<u8>) {
        self.as_secs().put(out);
        self.subsec_nanos().put(out);
    }
    fn get(input: &mut Dec<'_>) -> Option<Self> {
        let secs = u64::get(input)?;
        let nanos = u32::get(input)?;
        (nanos < 1_000_000_000).then(|| Self::new(secs, nanos))
    }
}

impl<T: Wire> Wire for Option<T> {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            None => out.push(0),
            Some(value) => {
                out.push(1);
                value.put(out);
            }
        }
    }
    fn get(input: &mut Dec<'_>) -> Option<Self> {
        match input.byte()? {
            0 => Some(None),
            1 => T::get(input).map(Some),
            _ => None,
        }
    }
}

impl<T: Wire> Wire for Vec<T> {
    fn put(&self, out: &mut Vec<u8>) {
        put_varint(out, self.len() as u64);
        for item in self {
            item.put(out);
        }
    }
    fn get(input: &mut Dec<'_>) -> Option<Self> {
        let n = input.len()?;
        (0..n).map(|_| T::get(input)).collect()
    }
}

/// Every field, in order. Adding a field to a snapshot type fails to
/// compile here until the trace learns it (and [`TRACE_VERSION`] moves).
macro_rules! wire_struct {
    ($ty:ty { $($field:ident),* $(,)? }) => {
        impl Wire for $ty {
            fn put(&self, out: &mut Vec<u8>) {
                $(self.$field.put(out);)*
            }
            fn get(input: &mut Dec<'_>) -> Option<Self> {
                Some(Self { $($field: Wire::get(input)?,)* })
            }
        }
    };
}

/// A fieldless enum as its position in the listed variants.
macro_rules! wire_enum {
    ($ty:ident { $($variant:ident),* $(,)? }) => {
        impl Wire for $ty {
            fn put(&self, out: &mut Vec<u8>) {
                const ALL: &[$ty] = &[$($ty::$variant),*];
                let index = ALL.iter().position(|v| v == self).unwrap_or(0);
                out.push(index as u8);
            }
            fn get(input: &mut Dec<'_>) -> Option<Self> {
                const ALL: &[$ty] = &[$($ty::$variant),*];
                ALL.get(usize::from(input.byte()?)).copied()
            }
        }
    };
}

wire_enum!(ClusterKind {
    Efficiency,
    Performance,
    Super
});
wire_enum!(Pressure {
    Nominal,
    Warn,
    Critical
});
wire_enum!(Thermal {
    Nominal,
    Fair,
    Serious,
    Critical
});
wire_enum!(PowerSource {
    Unknown,
    Ac,
    Battery,
    Ups
});
wire_enum!(ChargeState {
    Unknown,
    Charging,
    Discharging,
    Full,
    Held
});
wire_enum!(CollectorId {
    Cpu,
    Memory,
    Gpu,
    Network,
    Disk,
    Processes,
    Sensors,
    Fans,
    Thermal,
    Battery
});

impl Wire for ReadingSource {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            Self::Measured => out.push(0),
            Self::Held { age } => {
                out.push(1);
                age.put(out);
            }
            Self::Estimated => out.push(2),
        }
    }
    fn get(input: &mut Dec<'_>) -> Option<Self> {
        match input.byte()? {
            0 => Some(Self::Measured),
            1 => Some(Self::Held {
                age: Duration::get(input)?,
            }),
            2 => Some(Self::Estimated),
            _ => None,
        }
    }
}

impl Wire for TimeRemaining {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            Self::Unknown => out.push(0),
            Self::Unlimited => out.push(1),
            Self::Known(left) => {
                out.push(2);
                left.put(out);
            }
        }
    }
    fn get(input: &mut Dec<'_>) -> Option<Self> {
        match input.byte()? {
            0 => Some(Self::Unknown),
            1 => Some(Self::Unlimited),
            2 => Duration::get(input).map(Self::Known),
            _ => None,
        }
    }
}

impl Wire for Health {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            Self::Ok => out.push(0),
            Self::Pending => out.push(1),
            Self::Stale(age) => {
                out.push(2);
                age.put(out);
            }
            Self::Failed(why) => {
                out.push(3);
                why.put(out);
            }
            Self::Absent => out.push(4),
        }
    }
    fn get(input: &mut Dec<'_>) -> Option<Self> {
        match input.byte()? {
            0 => Some(Self::Ok),
            1 => Some(Self::Pending),
            2 => Duration::get(input).map(Self::Stale),
            3 => String::get(input).map(Self::Failed),
            4 => Some(Self::Absent),
            _ => None,
        }
    }
}

impl Wire for HealthReport {
    fn put(&self, out: &mut Vec<u8>) {
        let entries: Vec<_> = self.iter().collect();
        put_varint(out, entries.len() as u64);
        for (id, health) in entries {
            id.put(out);
            health.put(out);
        }
    }
    fn get(input: &mut Dec<'_>) -> Option<Self> {
        let mut report = Self::default();
        for _ in 0..input.len()? {
            let id = CollectorId::get(input)?;
            report.set(id, Health::get(input)?);
        }
        Some(report)
    }
}

/// `Process::status` is a static word; unknown spellings read back empty.
const STATUS_WORDS: [&str; 3] = ["", "stopped", "zombie"];

impl Wire for Process {
    fn put(&self, out: &mut Vec<u8>) {
        self.pid.put(out);
        self.ppid.put(out);
        self.name.put(out);
        self.cpu.put(out);
        self.mem_bytes.put(out);
        self.threads.put(out);
        self.gpu.put(out);
        self.user.put(out);
        self.uid.put(out);
        self.command.put(out);
        self.args.put(out);
        let status = STATUS_WORDS.iter().position(|w| *w == self.status);
        out.push(status.unwrap_or(0) as u8);
        self.start_unix.put(out);
        self.cpu_spark.put(out);
        self.restricted.put(out);
        self.rss_bytes.put(out);
        self.energy_w.put(out);
        self.disk_read_bps.put(out);
        self.disk_write_bps.put(out);
        self.wakeups_per_s.put(out);
        self.child_cpu_ns.put(out);
    }
    fn get(input: &mut Dec<'_>) -> Option<Self> {
        Some(Self {
            pid: Wire::get(input)?,
            ppid: Wire::get(input)?,
            name: Wire::get(input)?,
            cpu: Wire::get(input)?,
            mem_bytes: Wire::get(input)?,
            threads: Wire::get(input)?,
            gpu: Wire::get(input)?,
            user: Wire::get(input)?,
            uid: Wire::get(input)?,
            command: Wire::get(input)?,
            args: Wire::get(input)?,
            status: STATUS_WORDS
                .get(usize::from(input.byte()?))
                .copied()
                .unwrap_or(""),
            start_unix: Wire::get(input)?,
            cpu_spark: Wire::get(input)?,
            restricted: Wire::get(input)?,
            rss_bytes: Wire::get(input)?,
            energy_w: Wire::get(input)?,
            disk_read_bps: Wire::get(input)?,
            disk_write_bps: Wire::get(input)?,
            wakeups_per_s: Wire::get(input)?,
            child_cpu_ns: Wire::get(input)?,
        })
    }
}

wire_struct!(SocInfo {
    name,
    e_cores,
    p_cores,
    s_cores,
    gpu_cores,
    memory_bytes,
    core_kinds
});
wire_struct!(Cluster {
    kind,
    scaled,
    active,
    freq_mhz
});
wire_struct!(CpuCluster {
    id,
    kind,
    name,
    active,
    scaled,
    freq_mhz,
    max_mhz
});
wire_struct!(CoreSample {
    kind,
    index,
    scaled,
    active,
    freq_mhz
});
wire_struct!(CpuSnapshot {
    scaled,
    active,
    watts,
    freq_mhz,
    temp_c,
    e_cluster,
    p_cluster,
    s_cluster,
    cores,
    clusters
});
wire_struct!(MemorySnapshot {
    used_bytes,
    total_bytes,
    wired_bytes,
    compressed_bytes,
    cache_bytes,
    swap_used_bytes,
    swap_total_bytes,
    pressure,
    swap_in_bps,
    swap_out_bps,
    free_percent
});
wire_struct!(GpuSnapshot {
    scaled,
    active,
    watts,
    freq_mhz,
    max_mhz,
    renderer_busy,
    ane_watts,
    temp_c,
    mem_in_use_bytes,
    mem_alloc_bytes
});
wire_struct!(IfaceRate {
    name,
    rx_bps,
    tx_bps,
    rx_bytes,
    tx_bytes,
    link_bps,
    primary
});
wire_struct!(NetworkSnapshot {
    iface,
    rx_bps,
    tx_bps,
    primary,
    ifaces
});
wire_struct!(DiskVolume {
    name,
    mount,
    used_bytes,
    total_bytes
});
wire_struct!(DiskDevice {
    registry_id,
    read_bps,
    write_bps
});
wire_struct!(DiskSnapshot {
    volumes,
    read_bps,
    write_bps,
    devices
});
wire_struct!(FanMetric { name, rpm, max_rpm });
wire_struct!(FanSnapshot { fans });
wire_struct!(TempReading { name, celsius });
wire_struct!(SensorsSnapshot {
    cpu_c,
    gpu_c,
    hotspot_c,
    e_c,
    p_c,
    s_c,
    readings
});
wire_struct!(Reading { watts, source });
wire_struct!(Throttle {
    gpu_cltm,
    gpu_power_limited,
    cpms_reduction
});
wire_struct!(PowerSnapshot {
    system,
    cpu,
    gpu,
    ane,
    dram,
    throttle
});
wire_struct!(Adapter { watts, name });
wire_struct!(AwakeAssertion {
    pid,
    process,
    kind,
    name
});
wire_struct!(BatterySnapshot {
    present,
    source,
    percent,
    state,
    external_connected,
    is_charging,
    fully_charged,
    voltage_mv,
    amperage_ma,
    watts,
    cycle_count,
    design_cycle_count,
    design_capacity_mah,
    nominal_capacity_mah,
    full_charge_capacity_mah,
    health_pct,
    time_to_empty,
    time_to_full,
    time_remaining,
    adapter,
    not_charging_reason,
    thermally_limited,
    low_power_mode,
    temp_c,
    awake
});
wire_struct!(Sampled { procs, sensors });

// ------------------------------------------------------------ sections ----

type PutSection = fn(&Snapshot, &mut Vec<u8>);

/// Decodes one field in place.
fn read<T: Wire>(d: &mut Dec<'_>, slot: &mut T) -> Option<()> {
    *slot = T::get(d)?;
    Some(())
}
type GetSection = fn(&mut Dec<'_>, &mut Snapshot) -> Option<()>;

/// The busiest [`TRACE_PROCS`] processes; unreadable CPU sorts last.
fn top_procs(procs: &[Process]) -> Vec<Process> {
    let mut sorted: Vec<&Process> = procs.iter().collect();
    sorted.sort_by(|a, b| {
        b.cpu
            .unwrap_or(f32::NEG_INFINITY)
            .total_cmp(&a.cpu.unwrap_or(f32::NEG_INFINITY))
    });
    sorted
        .into_iter()
        .take(TRACE_PROCS)
        .map(shareable)
        .collect()
}

/// `p` as a trace stores it. Traces get attached to bug reports, so the
/// command line (which can carry passwords and tokens) is left out, as
/// `--json` leaves it out, and the executable keeps only its file name.
fn shareable(p: &Process) -> Process {
    let command = p.command.as_deref().and_then(|c| c.rsplit('/').next());
    Process {
        command: command.map(str::to_owned),
        args: Vec::new(),
        ..p.clone()
    }
}

/// A snapshot split into independently repeatable slices, slow-changing
/// ones apart from the rates. Order is the wire order: append only.
const SECTIONS: [(PutSection, GetSection); 13] = [
    (|s, o| s.soc.put(o), |d, s| read(d, &mut s.soc)),
    (|s, o| s.cpu.put(o), |d, s| read(d, &mut s.cpu)),
    (|s, o| s.memory.put(o), |d, s| read(d, &mut s.memory)),
    (|s, o| s.gpu.put(o), |d, s| read(d, &mut s.gpu)),
    (|s, o| s.network.put(o), |d, s| read(d, &mut s.network)),
    (|s, o| s.disk.put(o), |d, s| read(d, &mut s.disk)),
    (|s, o| s.fans.put(o), |d, s| read(d, &mut s.fans)),
    (|s, o| s.sensors.put(o), |d, s| read(d, &mut s.sensors)),
    (
        |s, o| {
            top_procs(&s.processes).put(o);
            s.process_ncpu.put(o);
            s.unattributed_cpu.put(o);
        },
        |d, s| {
            s.processes = Wire::get(d)?;
            s.process_ncpu = Wire::get(d)?;
            s.unattributed_cpu = Wire::get(d)?;
            Some(())
        },
    ),
    (
        |s, o| {
            s.thermal.put(o);
            s.power.put(o);
        },
        |d, s| {
            s.thermal = Wire::get(d)?;
            s.power = Wire::get(d)?;
            Some(())
        },
    ),
    (|s, o| s.battery.put(o), |d, s| read(d, &mut s.battery)),
    (
        |s, o| {
            s.status.put(o);
            s.sampled.put(o);
            s.primed.put(o);
            s.settled.put(o);
        },
        |d, s| {
            s.status = Wire::get(d)?;
            s.sampled = Wire::get(d)?;
            s.primed = Wire::get(d)?;
            s.settled = Wire::get(d)?;
            Some(())
        },
    ),
    (|s, o| s.health.put(o), |d, s| read(d, &mut s.health)),
];

const SAME: u8 = 0;
const FRESH: u8 = 1;

// -------------------------------------------------------------- header ----

/// What a trace says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceHeader {
    /// The sampling interval the recording ran at.
    pub interval: Duration,
    /// Wall-clock unix milliseconds at frame 0.
    pub started_unix_ms: u64,
    /// Free text, such as the chip and the version that recorded it.
    pub note: String,
}

impl TraceHeader {
    fn payload(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.interval.put(&mut out);
        self.started_unix_ms.put(&mut out);
        self.note.put(&mut out);
        out
    }

    fn parse(payload: &[u8]) -> Option<Self> {
        let mut d = Dec { bytes: payload };
        Some(Self {
            interval: Wire::get(&mut d)?,
            started_unix_ms: Wire::get(&mut d)?,
            note: Wire::get(&mut d)?,
        })
    }
}

fn frame_record(out: &mut Vec<u8>, payload: &[u8]) {
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&crc32(payload).to_le_bytes());
    out.extend_from_slice(payload);
}

// -------------------------------------------------------------- writer ----

/// Appends frames to any `Write`; each [`TraceWriter::push`] writes one
/// whole record, so a crash tears at most the last one.
pub struct TraceWriter<W: Write> {
    out: W,
    previous: Vec<Vec<u8>>,
    last_offset: Duration,
    frames: u64,
    bytes: u64,
}

impl<W: Write> TraceWriter<W> {
    pub fn new(mut out: W, header: &TraceHeader) -> io::Result<Self> {
        let mut head = Vec::from(TRACE_MAGIC);
        head.extend_from_slice(&TRACE_VERSION.to_le_bytes());
        frame_record(&mut head, &header.payload());
        out.write_all(&head)?;
        Ok(Self {
            out,
            previous: Vec::new(),
            last_offset: Duration::ZERO,
            frames: 0,
            bytes: head.len() as u64,
        })
    }

    /// One frame, `offset` after frame 0. Offsets never run backwards: an
    /// earlier one is stored as a repeat of the previous instant.
    pub fn push(&mut self, offset: Duration, snapshot: &Snapshot) -> io::Result<()> {
        let offset = offset.max(self.last_offset);
        let mut payload = Vec::new();
        let step = offset.saturating_sub(self.last_offset);
        put_varint(
            &mut payload,
            u64::try_from(step.as_millis()).unwrap_or(u64::MAX),
        );
        let mut sections = Vec::with_capacity(SECTIONS.len());
        for (i, (put, _)) in SECTIONS.iter().enumerate() {
            let mut bytes = Vec::new();
            put(snapshot, &mut bytes);
            if self.previous.get(i) == Some(&bytes) {
                payload.push(SAME);
            } else {
                payload.push(FRESH);
                put_varint(&mut payload, bytes.len() as u64);
                payload.extend_from_slice(&bytes);
            }
            sections.push(bytes);
        }
        let mut record = Vec::with_capacity(payload.len() + 8);
        frame_record(&mut record, &payload);
        self.out.write_all(&record)?;
        self.previous = sections;
        // Round like the reader does, so deltas never drift.
        self.last_offset += Duration::from_millis(step.as_millis() as u64);
        self.frames += 1;
        self.bytes += record.len() as u64;
        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }

    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.frames
    }

    /// Bytes written so far, header included.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn into_inner(self) -> W {
        self.out
    }
}

// -------------------------------------------------------------- reader ----

/// Longest recording a reader accepts; a later offset is corruption. It
/// keeps every `epoch + offset` (and a player's slowest deadline) far
/// inside `Instant`'s range, so crafted steps cannot overflow it.
pub const MAX_SPAN: Duration = Duration::from_hours(366 * 24);

/// One replayable moment.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// Time since frame 0.
    pub offset: Duration,
    pub snapshot: Snapshot,
}

/// Every section's wire bytes for one frame. A repeated section shares
/// its bytes with the frame before, and a frame that repeats every
/// section shares the whole list, so a trace costs about its file size
/// in memory however long it runs.
type Sections = Arc<[Arc<[u8]>]>;

/// A frame as stored: its offset and its sections.
#[derive(Debug, Clone, PartialEq)]
struct Stored {
    offset: Duration,
    sections: Sections,
}

/// A checked trace; frames decode into snapshots on demand.
#[derive(Debug, Clone, PartialEq)]
pub struct Trace {
    pub header: TraceHeader,
    frames: Vec<Stored>,
    /// Frame 0's `collected_at`.
    epoch: Instant,
    /// Bytes after the last good frame were torn or corrupt and skipped.
    pub truncated: bool,
}

impl Trace {
    /// Encodes `frames` as [`TraceWriter`] would and reads them back.
    pub fn from_frames(
        header: &TraceHeader,
        frames: impl IntoIterator<Item = Frame>,
        epoch: Instant,
    ) -> io::Result<Self> {
        let mut writer = TraceWriter::new(Vec::new(), header)?;
        for frame in frames {
            writer.push(frame.offset, &frame.snapshot)?;
        }
        read_trace(&writer.into_inner(), epoch).map_err(io::Error::other)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Frame `index`'s time since frame 0.
    #[must_use]
    pub fn offset(&self, index: usize) -> Option<Duration> {
        self.frames.get(index).map(|f| f.offset)
    }

    /// The last frame at or before `at` (frame 0 before it starts).
    #[must_use]
    pub fn index_at(&self, at: Duration) -> usize {
        self.frames
            .partition_point(|f| f.offset <= at)
            .saturating_sub(1)
    }

    /// Frame `index`, decoded; `collected_at` is `epoch` plus its offset.
    #[must_use]
    pub fn frame(&self, index: usize) -> Option<Frame> {
        let stored = self.frames.get(index)?;
        let mut snapshot = Snapshot::empty();
        for ((_, get), bytes) in SECTIONS.iter().zip(stored.sections.iter()) {
            get(&mut Dec { bytes }, &mut snapshot)?;
        }
        // `read_trace` checked this sum when it accepted the frame.
        snapshot.collected_at = self.epoch.checked_add(stored.offset)?;
        Some(Frame {
            offset: stored.offset,
            snapshot,
        })
    }

    /// Time from the first frame to the last.
    #[must_use]
    pub fn span(&self) -> Duration {
        self.frames.last().map_or(Duration::ZERO, |f| f.offset)
    }
}

/// Why a trace could not be opened at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceError {
    NotATrace,
    Version(u16),
    BadHeader,
}

impl std::fmt::Display for TraceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotATrace => f.write_str("not a siltop trace"),
            Self::Version(v) => write!(
                f,
                "trace version {v} is not supported (this build reads {TRACE_VERSION})"
            ),
            Self::BadHeader => f.write_str("the trace header is damaged"),
        }
    }
}

impl std::error::Error for TraceError {}

/// The next record's payload, or `None` if it is torn or fails its CRC.
fn next_record<'a>(d: &mut Dec<'a>) -> Option<&'a [u8]> {
    let len = u32::from_le_bytes(d.take(4)?.try_into().ok()?) as usize;
    let crc = u32::from_le_bytes(d.take(4)?.try_into().ok()?);
    if len > MAX_RECORD {
        return None;
    }
    let payload = d.take(len)?;
    (crc32(payload) == crc).then_some(payload)
}

/// Checks one frame against the previous one's sections and returns its
/// step and sections. Fresh sections are decoded into `scratch` once, so
/// every kept frame decodes again later.
fn check_frame(
    payload: &[u8],
    prior: Option<&Sections>,
    scratch: &mut Snapshot,
) -> Option<(Duration, Sections)> {
    let previous = prior.map_or(&[][..], |p| &p[..]);
    let mut d = Dec { bytes: payload };
    let step = Duration::from_millis(d.varint()?);
    let mut sections = Vec::with_capacity(SECTIONS.len());
    for (i, (_, get)) in SECTIONS.iter().enumerate() {
        let bytes = match d.byte()? {
            SAME => Arc::clone(previous.get(i)?),
            FRESH => {
                let n = d.len()?;
                let bytes = d.take(n)?;
                let mut section = Dec { bytes };
                get(&mut section, scratch)?;
                if !section.bytes.is_empty() {
                    return None;
                }
                // Bytes equal to the last copy share it, as a repeat would.
                match previous.get(i) {
                    Some(last) if **last == *bytes => Arc::clone(last),
                    _ => Arc::from(bytes),
                }
            }
            _ => return None,
        };
        sections.push(bytes);
    }
    if !d.bytes.is_empty() {
        return None;
    }
    let unchanged = previous.len() == sections.len()
        && previous
            .iter()
            .zip(&sections)
            .all(|(a, b)| Arc::ptr_eq(a, b));
    let sections = match prior {
        Some(prior) if unchanged => Arc::clone(prior),
        _ => sections.into(),
    };
    Some((step, sections))
}

/// Checks `bytes`; frame 0 plays at `epoch` and every `collected_at` is
/// `epoch` plus that frame's offset. A frame past [`MAX_SPAN`] counts as
/// corrupt, like a torn one.
pub fn read_trace(bytes: &[u8], epoch: Instant) -> Result<Trace, TraceError> {
    let mut d = Dec { bytes };
    if d.take(TRACE_MAGIC.len()) != Some(&TRACE_MAGIC[..]) {
        return Err(TraceError::NotATrace);
    }
    let version = d
        .take(2)
        .and_then(|b| b.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or(TraceError::NotATrace)?;
    if version != TRACE_VERSION {
        return Err(TraceError::Version(version));
    }
    let header = next_record(&mut d)
        .and_then(TraceHeader::parse)
        .ok_or(TraceError::BadHeader)?;
    let mut trace = Trace {
        header,
        frames: Vec::new(),
        epoch,
        truncated: false,
    };
    let mut scratch = Snapshot::empty();
    while !d.bytes.is_empty() {
        let previous = trace.frames.last().map(|f| &f.sections);
        let last = trace.span();
        let next = next_record(&mut d)
            .and_then(|p| check_frame(p, previous, &mut scratch))
            .and_then(|(step, sections)| {
                let offset = last.checked_add(step).filter(|at| *at <= MAX_SPAN)?;
                epoch.checked_add(offset)?;
                Some(Stored { offset, sections })
            });
        let Some(stored) = next else {
            trace.truncated = true;
            break;
        };
        trace.frames.push(stored);
    }
    Ok(trace)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn header() -> TraceHeader {
        TraceHeader {
            interval: Duration::from_millis(500),
            started_unix_ms: 1_790_000_000_000,
            note: String::from("Apple M5 Pro · siltop test"),
        }
    }

    fn proc(pid: u32, cpu: Option<f32>) -> Process {
        Process {
            pid,
            ppid: 1,
            name: format!("proc{pid}"),
            cpu,
            mem_bytes: Some(u64::from(pid) * 1024),
            user: String::from("jane"),
            args: vec![String::from("/bin/proc"), String::from("--flag")],
            status: if pid.is_multiple_of(7) { "zombie" } else { "" },
            start_unix: -5,
            cpu_spark: vec![1.0, 2.5],
            energy_w: Some(0.25),
            disk_read_bps: Some(12.5),
            ..Process::default()
        }
    }

    /// A snapshot touching every section, with values that differ by `i`.
    fn rich(i: u32) -> Snapshot {
        let mut s = Snapshot::empty();
        s.soc.name = String::from("Apple M5 Pro");
        s.soc.core_kinds = vec![ClusterKind::Efficiency, ClusterKind::Super];
        s.cpu.scaled = 0.1 * i as f32;
        s.cpu.watts = Some(3.5);
        s.cpu.e_cluster = Some(Cluster {
            kind: ClusterKind::Efficiency,
            scaled: 0.2,
            active: 0.3,
            freq_mhz: 1200,
        });
        s.cpu.cores = vec![CoreSample {
            kind: ClusterKind::Super,
            index: 3,
            scaled: 0.5,
            active: 0.6,
            freq_mhz: Some(4000),
        }];
        s.memory.used_bytes = 8 << 30;
        s.memory.total_bytes = 24 << 30;
        s.memory.pressure = Pressure::Warn;
        s.gpu = Some(GpuSnapshot {
            scaled: 0.4,
            temp_c: Some(55.0),
            ..GpuSnapshot::default()
        });
        s.network.iface = String::from("en0");
        s.network.rx_bps = u64::from(i) * 1000;
        s.disk.volumes = vec![DiskVolume {
            name: String::from("Macintosh HD"),
            mount: String::from("/"),
            used_bytes: 1,
            total_bytes: 2,
        }];
        s.fans.fans = vec![FanMetric {
            name: String::from("Left"),
            rpm: 1800,
            max_rpm: 6000,
        }];
        s.sensors.cpu_c = Some(61.5);
        s.sensors.readings = vec![TempReading {
            name: String::from("Tp01"),
            celsius: 60.0,
        }];
        s.processes = (1..=5)
            .map(|p| proc(p, Some(p as f32 * i as f32)))
            .collect();
        s.process_ncpu = 18;
        s.unattributed_cpu = Some(12.0);
        s.thermal = Thermal::Fair;
        s.power.system = Some(Reading {
            watts: 21.0,
            source: ReadingSource::Held {
                age: Duration::from_millis(1500),
            },
        });
        s.power.throttle.cpms_reduction = Some(0.1);
        s.battery.present = true;
        s.battery.source = PowerSource::Battery;
        s.battery.state = ChargeState::Held;
        s.battery.percent = Some(80);
        s.battery.amperage_ma = Some(-1500);
        s.battery.time_remaining = TimeRemaining::Known(Duration::from_secs(3600));
        s.battery.adapter = Some(Adapter {
            watts: Some(140),
            name: None,
        });
        s.battery.awake = vec![AwakeAssertion {
            pid: 42,
            process: String::from("caffeinate"),
            kind: String::from("PreventUserIdleSystemSleep"),
            name: Some(String::from("build")),
        }];
        s.status = Some(String::from("sampling"));
        s.sampled = Sampled {
            procs: i.is_multiple_of(2),
            sensors: true,
        };
        s.primed = true;
        s.health.set(CollectorId::Fans, Health::Absent);
        s.health
            .set(CollectorId::Cpu, Health::Stale(Duration::from_secs(3)));
        s.health
            .set(CollectorId::Battery, Health::Failed(String::from("io")));
        s
    }

    fn write(frames: &[(u64, Snapshot)]) -> Vec<u8> {
        let mut w = TraceWriter::new(Vec::new(), &header()).unwrap();
        for (ms, snap) in frames {
            w.push(Duration::from_millis(*ms), snap).unwrap();
        }
        w.into_inner()
    }

    fn assert_same(got: &Snapshot, want: &Snapshot, epoch: Instant, offset_ms: u64) {
        let mut want = want.clone();
        want.collected_at = epoch + Duration::from_millis(offset_ms);
        want.processes = top_procs(&want.processes);
        assert_eq!(got, &want);
    }

    #[test]
    fn crc32_matches_the_zlib_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn varint_and_zigzag_round_trip_at_the_edges() {
        let mut out = Vec::new();
        let unsigned = [0, 1, 127, 128, 300, u64::from(u32::MAX), u64::MAX];
        let signed = [0, -1, 1, i64::MIN, i64::MAX, -1_790_000_000];
        for v in unsigned {
            v.put(&mut out);
        }
        for v in signed {
            v.put(&mut out);
        }
        let mut d = Dec { bytes: &out };
        for v in unsigned {
            assert_eq!(u64::get(&mut d), Some(v));
        }
        for v in signed {
            assert_eq!(i64::get(&mut d), Some(v));
        }
        assert!(d.bytes.is_empty());
    }

    #[test]
    fn narrow_ints_refuse_values_that_do_not_fit() {
        let mut out = Vec::new();
        300_u64.put(&mut out);
        assert_eq!(u8::get(&mut Dec { bytes: &out }), None);
        assert_eq!(u16::get(&mut Dec { bytes: &out }), Some(300));
    }

    #[test]
    fn round_trip_keeps_every_field() {
        let frames: Vec<(u64, Snapshot)> = (0..6).map(|i| (u64::from(i) * 500, rich(i))).collect();
        let epoch = Instant::now();
        let trace = read_trace(&write(&frames), epoch).unwrap();
        assert!(!trace.truncated);
        assert_eq!(trace.header, header());
        assert_eq!(trace.len(), 6);
        for (i, (ms, want)) in frames.iter().enumerate() {
            let frame = trace.frame(i).unwrap();
            assert_eq!(frame.offset, Duration::from_millis(*ms));
            assert_same(&frame.snapshot, want, epoch, *ms);
        }
        assert_eq!(trace.span(), Duration::from_millis(2500));
    }

    #[test]
    fn empty_snapshot_round_trips() {
        let epoch = Instant::now();
        let trace = read_trace(&write(&[(0, Snapshot::empty())]), epoch).unwrap();
        assert_same(
            &trace.frame(0).unwrap().snapshot,
            &Snapshot::empty(),
            epoch,
            0,
        );
    }

    #[test]
    fn unchanged_sections_are_sent_once() {
        let one = write(&[(0, rich(1))]);
        let repeated = write(&[(0, rich(1)), (500, rich(1)), (1000, rich(1))]);
        let per_repeat = (repeated.len() - one.len()) / 2;
        // Offset varint + 13 SAME tags + the 8-byte record frame.
        assert!(per_repeat <= 2 + SECTIONS.len() + 8, "{per_repeat}");
        let trace = read_trace(&repeated, Instant::now()).unwrap();
        assert_eq!(trace.len(), 3);
        assert_eq!(trace.frame(2).unwrap().snapshot.soc, rich(1).soc);
    }

    #[test]
    fn a_changed_section_after_a_repeat_decodes_on_the_right_base() {
        let mut changed = rich(1);
        changed.battery.percent = Some(79);
        let frames = [(0, rich(1)), (500, rich(1)), (1000, changed.clone())];
        let epoch = Instant::now();
        let trace = read_trace(&write(&frames), epoch).unwrap();
        assert_same(&trace.frame(2).unwrap().snapshot, &changed, epoch, 1000);
    }

    #[test]
    fn process_list_is_capped_to_the_busiest() {
        let mut s = rich(1);
        s.processes = (0..100_u32).map(|p| proc(p, Some(p as f32))).collect();
        s.processes.push(proc(500, None));
        let trace = read_trace(&write(&[(0, s)]), Instant::now()).unwrap();
        let kept = &trace.frame(0).unwrap().snapshot.processes;
        assert_eq!(kept.len(), TRACE_PROCS);
        assert_eq!(kept[0].pid, 99);
        assert!(
            kept.iter().all(|p| p.cpu.is_some()),
            "unreadable sorts last"
        );
    }

    #[test]
    fn unknown_status_word_reads_back_empty() {
        let mut s = Snapshot::empty();
        let mut p = proc(1, Some(1.0));
        p.status = "weird";
        s.processes = vec![p];
        let trace = read_trace(&write(&[(0, s)]), Instant::now()).unwrap();
        assert_eq!(trace.frame(0).unwrap().snapshot.processes[0].status, "");
    }

    #[test]
    fn offsets_never_run_backwards() {
        let trace = read_trace(
            &write(&[(1000, rich(1)), (400, rich(2)), (1500, rich(3))]),
            Instant::now(),
        )
        .unwrap();
        let offsets: Vec<u64> = (0..trace.len())
            .map(|i| trace.offset(i).unwrap().as_millis() as u64)
            .collect();
        assert_eq!(offsets, [1000, 1000, 1500]);
    }

    /// A recording killed mid-write replays up to its last whole frame,
    /// wherever the tear lands.
    #[test]
    fn torn_tail_keeps_every_whole_frame_at_every_cut() {
        let frames: Vec<(u64, Snapshot)> = (0..4).map(|i| (u64::from(i) * 500, rich(i))).collect();
        let full = write(&frames);
        let whole: Vec<usize> = (0..=4).map(|n| write(&frames[..n]).len()).collect();
        for cut in whole[0]..full.len() {
            let trace = read_trace(&full[..cut], Instant::now()).unwrap();
            let expect = whole.iter().filter(|len| **len <= cut).count() - 1;
            assert_eq!(trace.len(), expect, "cut at {cut}");
            assert_eq!(trace.truncated, !whole.contains(&cut), "cut at {cut}");
        }
    }

    #[test]
    fn corrupt_frame_stops_the_read_but_keeps_earlier_frames() {
        let frames: Vec<(u64, Snapshot)> = (0..3).map(|i| (u64::from(i) * 500, rich(i))).collect();
        let mut bytes = write(&frames);
        let two = write(&frames[..2]).len();
        bytes[two + 12] ^= 0xFF;
        let trace = read_trace(&bytes, Instant::now()).unwrap();
        assert_eq!(trace.len(), 2);
        assert!(trace.truncated);
    }

    #[test]
    fn huge_length_prefix_is_corruption_not_an_allocation() {
        let mut bytes = write(&[]);
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        let trace = read_trace(&bytes, Instant::now()).unwrap();
        assert!(trace.truncated && trace.is_empty());
        let mut d = Dec {
            bytes: &[0xFF, 0xFF, 0x03],
        };
        assert_eq!(d.len(), None, "claims more elements than bytes left");
    }

    #[test]
    fn open_errors_are_specific() {
        assert_eq!(
            read_trace(b"hello", Instant::now()),
            Err(TraceError::NotATrace)
        );
        let mut bytes = write(&[]);
        bytes[8] = 9;
        assert_eq!(
            read_trace(&bytes, Instant::now()),
            Err(TraceError::Version(9))
        );
        let mut bytes = write(&[]);
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        assert_eq!(
            read_trace(&bytes, Instant::now()),
            Err(TraceError::BadHeader)
        );
        assert!(TraceError::Version(9).to_string().contains("version 9"));
    }

    #[test]
    fn writer_counts_frames_and_bytes() {
        let mut w = TraceWriter::new(Vec::new(), &header()).unwrap();
        w.push(Duration::ZERO, &rich(1)).unwrap();
        w.push(Duration::from_secs(1), &rich(2)).unwrap();
        assert_eq!(w.frames(), 2);
        let bytes = w.bytes();
        assert_eq!(bytes, w.into_inner().len() as u64);
    }

    /// A frame record that repeats every section after `step_ms`.
    fn repeat_record(step_ms: u64) -> Vec<u8> {
        let mut payload = Vec::new();
        put_varint(&mut payload, step_ms);
        payload.extend([SAME; SECTIONS.len()]);
        let mut record = Vec::new();
        frame_record(&mut record, &payload);
        record
    }

    /// A shared `.ptrace` whose steps overflow `Duration` or `Instant`
    /// used to panic `siltop replay`; now it is a corrupt tail.
    #[test]
    fn crafted_huge_steps_are_a_corrupt_tail_not_an_overflow_panic() {
        for steps in [
            vec![u64::MAX],
            vec![u64::MAX; 1100],
            vec![1, u64::MAX / 2, u64::MAX / 2],
            vec![MAX_SPAN.as_millis() as u64 + 1],
        ] {
            let mut bytes = write(&[(0, rich(1))]);
            for step in &steps {
                bytes.extend(repeat_record(*step));
            }
            let trace = read_trace(&bytes, Instant::now()).unwrap();
            assert!(trace.truncated, "{steps:?}");
            assert!(trace.span() <= MAX_SPAN, "{steps:?}");
            assert!(trace.frame(trace.len() - 1).is_some(), "{steps:?}");
        }
        let mut bytes = write(&[(0, rich(1))]);
        bytes.extend(repeat_record(MAX_SPAN.as_millis() as u64));
        let trace = read_trace(&bytes, Instant::now()).unwrap();
        assert!(!trace.truncated && trace.span() == MAX_SPAN);
    }

    /// Random damage behind a valid CRC never panics the reader, and every
    /// frame it keeps decodes.
    #[test]
    fn mutated_frames_with_valid_crcs_never_panic() {
        let clean = write(&[(0, rich(1)), (500, rich(2))]);
        let body = write(&[]).len();
        let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..2000 {
            let mut d = Dec {
                bytes: &clean[body..],
            };
            let mut payload = next_record(&mut d).unwrap().to_vec();
            for _ in 0..=next() % 4 {
                let at = (next() % payload.len() as u64) as usize;
                payload[at] = next() as u8;
            }
            let mut bytes = clean[..body].to_vec();
            frame_record(&mut bytes, &payload);
            let trace = read_trace(&bytes, Instant::now()).unwrap();
            for i in 0..trace.len() {
                assert!(trace.frame(i).is_some());
            }
        }
    }

    /// Replay used to clone a whole snapshot per frame, so a big section
    /// repeated by one-byte records blew up to gigabytes of RAM.
    #[test]
    fn repeated_sections_share_their_bytes_across_frames() {
        let mut s = rich(1);
        s.processes = (0..40).map(|p| proc(p, Some(1.0))).collect();
        let mut bytes = write(&[(0, s)]);
        for _ in 0..1000 {
            bytes.extend(repeat_record(500));
        }
        let trace = read_trace(&bytes, Instant::now()).unwrap();
        assert_eq!(trace.len(), 1001);
        let (first, last) = (&trace.frames[0], &trace.frames[1000]);
        assert!(Arc::ptr_eq(&first.sections, &last.sections));
        assert_eq!(
            trace.frame(1000).unwrap().snapshot.processes,
            trace.frame(0).unwrap().snapshot.processes
        );
    }

    /// A crafted file can re-send identical bytes as fresh copies instead
    /// of repeats; they share memory just the same.
    #[test]
    fn fresh_copies_of_unchanged_sections_share_their_bytes() {
        let first = write(&[(0, rich(1))]);
        let sections = read_trace(&first, Instant::now()).unwrap().frames[0]
            .sections
            .clone();
        let mut payload = Vec::new();
        put_varint(&mut payload, 500);
        for bytes in sections.iter() {
            payload.push(FRESH);
            put_varint(&mut payload, bytes.len() as u64);
            payload.extend_from_slice(bytes);
        }
        let mut bytes = first;
        frame_record(&mut bytes, &payload);
        let trace = read_trace(&bytes, Instant::now()).unwrap();
        assert!(!trace.truncated && trace.len() == 2);
        assert!(Arc::ptr_eq(
            &trace.frames[0].sections,
            &trace.frames[1].sections
        ));
    }

    /// Traces get attached to bug reports, so argv (passwords, tokens)
    /// and the executable's directory never reach the file.
    #[test]
    fn traces_leave_out_command_lines() {
        let mut s = Snapshot::empty();
        let mut p = proc(1, Some(1.0));
        p.command = Some(String::from("/opt/tools/bin/psql"));
        p.args = vec![String::from("psql"), String::from("postgres://a:S3cr3t@db")];
        s.processes = vec![p];
        let bytes = write(&[(0, s)]);
        assert!(!bytes.windows(6).any(|w| w == b"S3cr3t"));
        assert!(!bytes.windows(10).any(|w| w == b"/opt/tools"));
        let trace = read_trace(&bytes, Instant::now()).unwrap();
        let got = trace.frame(0).unwrap().snapshot;
        assert_eq!(got.processes[0].command.as_deref(), Some("psql"));
        assert!(got.processes[0].args.is_empty());
    }

    #[test]
    fn from_frames_round_trips_and_index_at_finds_the_frame_showing() {
        let epoch = Instant::now();
        let frames = (0..3).map(|i| Frame {
            offset: Duration::from_secs(u64::from(i)),
            snapshot: rich(i),
        });
        let trace = Trace::from_frames(&header(), frames, epoch).unwrap();
        assert_eq!(trace.len(), 3);
        assert_same(&trace.frame(2).unwrap().snapshot, &rich(2), epoch, 2000);
        assert_eq!(trace.index_at(Duration::ZERO), 0);
        assert_eq!(trace.index_at(Duration::from_millis(1999)), 1);
        assert_eq!(trace.index_at(Duration::from_secs(60)), 2);
        assert_eq!(trace.frame(3), None);
    }
}
