//! Golden screens: the whole app painted from fixed fixtures, one per Mac
//! shape in [`FAMILY`] (the M5 Pro is the default). No clock, no hardware,
//! no RNG: every value comes from integer arithmetic, so a render is
//! byte-identical on every run and platform.
//!
//! Snapshots pin what each screen looks like; the family properties at the
//! bottom pin what must hold on every Mac at every size (nothing absent is
//! drawn as zero, fanless Macs show no fans, desktops no battery, cluster
//! names follow the chip, no core disappears from the list).
//!
//! A diff here is a visible change. Bless it on purpose with
//! `cargo insta review` (or `INSTA_UPDATE=always cargo test -p siltop-ui golden`)
//! and show the diff in the PR.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use siltop_core::SeriesId;
use std::fmt::Write as _;
use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use siltop_core::{
    BatterySnapshot, ChargeState, Cluster, ClusterKind, CoreSample, DiskSnapshot, DiskVolume,
    FanMetric, FanSnapshot, GpuSnapshot, History, MemorySnapshot, NetworkSnapshot, PowerSource,
    Process, SensorsSnapshot, Snapshot, SocInfo, Surface, TempReading,
};

use super::render_app;
use super::tests_support::{Fixture, fixture};
use super::{Anchor, AppView, Focus};
use crate::layout::Panel;

const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;
const CHEAP: Duration = Duration::from_millis(500);
const CHEAP_SAMPLES: usize = 900;
const SENSOR_SAMPLES: usize = 225;
const DOSSIER_PID: u32 = 51_234;

/// Park's LCG: deterministic jitter in 0..1 without floats until the end.
struct Jitter(u32);

impl Jitter {
    fn next(&mut self) -> f32 {
        self.0 = ((u64::from(self.0) * 48_271) % 2_147_483_647) as u32;
        (self.0 % 10_000) as f32 / 10_000.0
    }
}

/// 0..1..0 over `period` samples.
fn triangle(i: usize, period: usize) -> f32 {
    let half = period / 2;
    let t = i % period;
    let up = if t < half { t } else { period - t };
    up as f32 / half as f32
}

fn history(period: Duration, len: usize, value: impl Fn(usize) -> f32) -> History {
    let mut h = History::for_period(period);
    for i in 0..len {
        h.push(value(i));
    }
    h
}

fn cheap(seed: u32, value: impl Fn(usize, f32) -> f32) -> History {
    let mut jitter = Jitter(seed);
    let noise: Vec<f32> = (0..CHEAP_SAMPLES).map(|_| jitter.next()).collect();
    history(CHEAP, CHEAP_SAMPLES, |i| value(i, noise[i]))
}

fn sensor(seed: u32, value: impl Fn(usize, f32) -> f32) -> History {
    let mut jitter = Jitter(seed);
    let noise: Vec<f32> = (0..SENSOR_SAMPLES).map(|_| jitter.next()).collect();
    history(Duration::from_secs(2), SENSOR_SAMPLES, |i| {
        value(i, noise[i])
    })
}

/// A build burst two thirds of the way in, on top of a slow triangle.
fn load(i: usize, n: f32, base: f32, swing: f32) -> f32 {
    let burst = if (560..640).contains(&i) { 0.35 } else { 0.0 };
    base + swing * triangle(i, 240) + 0.06 * n + burst
}

const PROCS: &[(u32, u32, &str, f32, u64, u32, &str)] = &[
    (1, 0, "launchd", 0.3, 14, 4, "root"),
    (412, 1, "WindowServer", 14.2, 412, 24, "_windowserver"),
    (88, 1, "kernelmanagerd", 0.1, 6, 3, "root"),
    (301, 1, "mds_stores", 3.7, 96, 9, "root"),
    (302, 1, "mds", 0.9, 48, 7, "root"),
    (517, 1, "coreaudiod", 1.1, 22, 11, "_coreaudiod"),
    (640, 1, "bluetoothd", 0.4, 18, 6, "root"),
    (701, 1, "powerd", 0.2, 5, 3, "root"),
    (1180, 1, "Finder", 0.5, 132, 9, "dev"),
    (1204, 1, "Dock", 0.2, 71, 5, "dev"),
    (1302, 1, "ControlCenter", 0.6, 64, 8, "dev"),
    (2210, 1, "Brave Browser", 6.4, 488, 41, "dev"),
    (
        2231,
        2210,
        "Brave Browser Helper (GPU)",
        9.8,
        301,
        19,
        "dev",
    ),
    (
        2240,
        2210,
        "Brave Browser Helper (Renderer)",
        12.1,
        402,
        22,
        "dev",
    ),
    (
        2244,
        2210,
        "Brave Browser Helper (Renderer)",
        2.6,
        188,
        18,
        "dev",
    ),
    (
        2251,
        2210,
        "Brave Browser Helper (Renderer)",
        0.8,
        96,
        16,
        "dev",
    ),
    (3102, 1, "Terminal", 1.9, 118, 7, "dev"),
    (3150, 3102, "zsh", 0.0, 4, 1, "dev"),
    (3188, 3150, "cargo", 4.4, 72, 12, "dev"),
    (DOSSIER_PID, 3188, "rustc", 98.6, 1_210, 18, "dev"),
    (51_240, 3188, "rustc", 87.3, 942, 16, "dev"),
    (51_266, 3188, "rustc", 41.5, 610, 14, "dev"),
    (4012, 1, "zed", 5.2, 739, 38, "dev"),
    (4040, 4012, "rust-analyzer", 22.7, 2_540, 26, "dev"),
    (4501, 1, "Slack", 1.4, 356, 29, "dev"),
    (4522, 4501, "Slack Helper (Renderer)", 0.9, 244, 17, "dev"),
    (4801, 1, "Music", 0.7, 180, 21, "dev"),
    (5003, 1, "photoanalysisd", 2.1, 210, 6, "dev"),
    (5120, 1, "Spotlight", 0.1, 58, 5, "dev"),
    (5301, 1, "cloudd", 0.3, 41, 8, "dev"),
    (5410, 1, "trustd", 0.2, 17, 3, "root"),
    (5520, 1, "syspolicyd", 0.4, 33, 5, "root"),
    (6001, 1, "ollama", 0.0, 58, 12, "dev"),
    (6020, 6001, "ollama-runner", 31.9, 9_812, 20, "dev"),
    (7002, 3150, "siltop", 0.6, 15, 4, "dev"),
    (7310, 1, "softwareupdated", 0.1, 29, 4, "root"),
    (7422, 1, "backupd", 1.6, 51, 7, "root"),
    (7560, 1, "nsurlsessiond", 0.2, 24, 6, "dev"),
    (7801, 1, "Mail", 0.4, 198, 15, "dev"),
    (7902, 1, "Notes", 0.1, 122, 9, "dev"),
    (8103, 1, "Preview", 0.0, 84, 6, "dev"),
    (8250, 1, "Xcode", 3.3, 1_440, 34, "dev"),
    (8301, 8250, "SourceKitService", 7.9, 880, 11, "dev"),
    (8420, 1, "Simulator", 2.8, 402, 18, "dev"),
    (9011, 1, "sharingd", 0.2, 27, 5, "dev"),
    (9120, 1, "locationd", 0.1, 19, 5, "_locationd"),
];

fn processes() -> Vec<Process> {
    let mut list = PROCS
        .iter()
        .map(|&(pid, ppid, name, cpu, mem_mib, threads, user)| Process {
            pid,
            ppid,
            name: name.to_owned(),
            cpu: Some(cpu),
            mem_bytes: Some(mem_mib * MIB),
            threads: Some(threads),
            user: user.to_owned(),
            status: "sleeping",
            ..Process::default()
        })
        .collect::<Vec<_>>();
    if let Some(rustc) = list.iter_mut().find(|p| p.pid == DOSSIER_PID) {
        rustc.command = Some(String::from(
            "/Users/dev/.rustup/toolchains/stable-aarch64-apple-darwin/bin/rustc \
             --crate-name siltop_ui --edition=2024 crates/siltop-ui/src/lib.rs",
        ));
        rustc.status = "running";
        // The dossier reads the wall clock; a start in the future pins it to 0s.
        rustc.start_unix = i64::from(i32::MAX) * 4;
        rustc.cpu_spark = (0..40_u16).map(|i| 60.0 + f32::from(i * 13 % 40)).collect();
    }
    list
}

use ClusterKind::{Efficiency as E, Performance as P, Super as S};

/// One member of the Mac family: what the silicon and the chassis carry.
/// Every golden fixture is built from one of these, so a new Mac shape is
/// one table row, not a new builder.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Mac {
    /// Snapshot-name prefix.
    label: &'static str,
    chip: &'static str,
    /// One die's clusters in Mach order; repeated `dies` times.
    die: &'static [(ClusterKind, u8)],
    dies: u8,
    gpu_cores: u8,
    mem_gib: u64,
    /// 0 on fanless Macs.
    fans: usize,
    /// False on desktops.
    battery: bool,
}

impl Mac {
    fn core_kinds(self) -> Vec<ClusterKind> {
        (0..self.dies)
            .flat_map(|_| self.die)
            .flat_map(|&(kind, n)| std::iter::repeat_n(kind, usize::from(n)))
            .collect()
    }

    fn count(self, kind: ClusterKind) -> u8 {
        let per_die: u8 = self.die.iter().filter(|c| c.0 == kind).map(|c| c.1).sum();
        per_die * self.dies
    }

    fn has(self, kind: ClusterKind) -> bool {
        self.count(kind) > 0
    }

    fn total_cores(self) -> usize {
        [E, P, S].map(|k| usize::from(self.count(k))).iter().sum()
    }

    pub(crate) fn fixture(self) -> Fixture {
        let mut fx = fixture("");
        silicon(&mut fx.snap, self);
        chassis(&mut fx.snap, self);
        io(&mut fx.snap, self.mem_gib);
        fx.snap.processes = processes();
        histories(&mut fx, self);
        fx
    }
}

/// Mac17,8: 12 Performance (Mach 0-11) + 6 Super (Mach 12-17), 20-core GPU,
/// 24 GB, two fans, a battery.
const MBP_M5_PRO: Mac = Mac {
    label: "mbp_m5_pro",
    chip: "M5 Pro",
    die: &[(P, 12), (S, 6)],
    dies: 1,
    gpu_cores: 20,
    mem_gib: 24,
    fans: 2,
    battery: true,
};
/// Fanless laptop, first-generation E+P layout.
const AIR_M2: Mac = Mac {
    label: "air_m2",
    chip: "M2",
    die: &[(E, 4), (P, 4)],
    dies: 1,
    gpu_cores: 8,
    mem_gib: 16,
    fans: 0,
    battery: true,
};
const AIR_M4: Mac = Mac {
    label: "air_m4",
    chip: "M4",
    die: &[(E, 4), (P, 6)],
    dies: 1,
    gpu_cores: 10,
    mem_gib: 16,
    fans: 0,
    battery: true,
};
const MBP_M3_MAX: Mac = Mac {
    label: "mbp_m3_max",
    chip: "M3 Max",
    die: &[(E, 4), (P, 12)],
    dies: 1,
    gpu_cores: 40,
    mem_gib: 48,
    fans: 2,
    battery: true,
};
/// Desktop: one fan, no battery.
const MINI_M4: Mac = Mac {
    label: "mini_m4",
    chip: "M4",
    die: &[(E, 4), (P, 6)],
    dies: 1,
    gpu_cores: 10,
    mem_gib: 16,
    fans: 1,
    battery: false,
};
/// Two M3 Max dies: Mach order is die 0's E then P cores, then die 1's.
const STUDIO_M3_ULTRA: Mac = Mac {
    label: "studio_m3_ultra",
    chip: "M3 Ultra",
    die: &[(E, 4), (P, 12)],
    dies: 2,
    gpu_cores: 80,
    mem_gib: 96,
    fans: 1,
    battery: false,
};
/// Desktop with neither fans nor a battery.
const IMAC_M4: Mac = Mac {
    label: "imac_m4",
    chip: "M4",
    die: &[(E, 4), (P, 6)],
    dies: 1,
    gpu_cores: 10,
    mem_gib: 16,
    fans: 0,
    battery: false,
};

/// The whole family, current machine first.
const FAMILY: [Mac; 7] = [
    MBP_M5_PRO,
    AIR_M2,
    AIR_M4,
    MBP_M3_MAX,
    MINI_M4,
    STUDIO_M3_ULTRA,
    IMAC_M4,
];

pub(crate) fn m5_pro() -> Fixture {
    MBP_M5_PRO.fixture()
}

/// Per-kind shape of the fixture load.
struct KindLoad {
    base: f32,
    step: f32,
    /// Walks core indices so neighbours differ.
    stride: u16,
    /// How much of the busy time is frequency-scaled.
    scale: f32,
    cluster: Cluster,
}

fn kind_load(kind: ClusterKind) -> KindLoad {
    let load = |base, step, stride, scale, (scaled, active, freq_mhz)| KindLoad {
        base,
        step,
        stride,
        scale,
        cluster: Cluster {
            kind,
            scaled,
            active,
            freq_mhz,
        },
    };
    match kind {
        P => load(0.08, 0.035, 7, 0.8, (0.21, 0.29, 1_344)),
        S => load(0.22, 0.11, 5, 0.9, (0.52, 0.58, 4_380)),
        E => load(0.05, 0.04, 3, 0.7, (0.12, 0.15, 1_020)),
    }
}

/// One sample per Mach core, roles from `SocInfo::role_for` so the Ultra's
/// second die continues each kind's numbering.
fn cores(soc: &SocInfo, mac: Mac) -> Vec<CoreSample> {
    let total = mac.total_cores();
    (0..total)
        .filter_map(|mach| soc.role_for(mach, total))
        .map(|(kind, index)| {
            let load = kind_load(kind);
            let n = u16::from(mac.count(kind)).max(1);
            let busy = load.base + f32::from(index * load.stride % n) * load.step;
            CoreSample {
                kind,
                index,
                scaled: busy * load.scale,
                active: busy,
                freq_mhz: None,
            }
        })
        .collect()
}

fn silicon(snap: &mut Snapshot, mac: Mac) {
    snap.soc = SocInfo {
        name: String::from(mac.chip),
        e_cores: mac.count(E),
        p_cores: mac.count(P),
        s_cores: mac.count(S),
        gpu_cores: mac.gpu_cores,
        memory_bytes: mac.mem_gib * GIB,
        core_kinds: mac.core_kinds(),
    };
    snap.cpu.scaled = 0.34;
    snap.cpu.active = 0.47;
    snap.cpu.watts = Some(11.6);
    snap.cpu.freq_mhz = Some(3_912);
    snap.cpu.temp_c = Some(58.0);
    let cluster = |kind| mac.has(kind).then(|| kind_load(kind).cluster);
    snap.cpu.e_cluster = cluster(E);
    snap.cpu.p_cluster = cluster(P);
    snap.cpu.s_cluster = cluster(S);
    snap.cpu.cores = cores(&snap.soc, mac);
    snap.gpu = Some(GpuSnapshot {
        scaled: 0.17,
        active: 0.48,
        watts: Some(1.5),
        freq_mhz: Some(589),
        max_mhz: Some(1_620),
        renderer_busy: Some(0.48),
        ane_watts: None,
        temp_c: Some(47.0),
        mem_in_use_bytes: None,
        mem_alloc_bytes: None,
    });
}

/// Fans, zone sensors and the battery: what the enclosure has.
fn chassis(snap: &mut Snapshot, mac: Mac) {
    const FANS: [(u16, u16); 2] = [(1_714, 6_550), (1_859, 7_150)];
    snap.fans = FanSnapshot {
        fans: FANS
            .iter()
            .take(mac.fans)
            .enumerate()
            .map(|(i, &(rpm, max_rpm))| FanMetric {
                name: format!("Fan {}", i + 1),
                rpm,
                max_rpm,
            })
            .collect(),
    };
    let zone = |kind, c| mac.has(kind).then_some(c);
    let reading = |name: &str, celsius| TempReading {
        name: name.to_owned(),
        celsius,
    };
    snap.sensors = SensorsSnapshot {
        cpu_c: Some(58.0),
        gpu_c: Some(47.0),
        hotspot_c: Some(64.0),
        e_c: zone(E, 49.0),
        p_c: zone(P, 54.0),
        s_c: zone(S, 63.0),
        readings: std::iter::once(reading("nand", 38.0))
            .chain(mac.battery.then(|| reading("battery", 31.0)))
            .collect(),
    };
    snap.battery = if mac.battery {
        BatterySnapshot {
            present: true,
            source: PowerSource::Battery,
            percent: Some(82),
            state: ChargeState::Discharging,
            ..BatterySnapshot::default()
        }
    } else {
        BatterySnapshot::default()
    };
}

fn io(snap: &mut Snapshot, mem_gib: u64) {
    snap.memory = MemorySnapshot {
        // 15.2 of every 24 GB in use, whatever the machine has.
        used_bytes: (15 * GIB + 205 * MIB) * mem_gib / 24,
        total_bytes: mem_gib * GIB,
        wired_bytes: 3 * GIB + 102 * MIB,
        compressed_bytes: GIB + 410 * MIB,
        cache_bytes: 5 * GIB + 307 * MIB,
        swap_used_bytes: 820 * MIB,
        swap_total_bytes: 2 * GIB,
        pressure: siltop_core::Pressure::Nominal,
        ..MemorySnapshot::default()
    };
    snap.network = NetworkSnapshot {
        iface: String::from("en0"),
        rx_bps: 1_240_000,
        tx_bps: 86_000,
        ..NetworkSnapshot::default()
    };
    snap.disk = DiskSnapshot {
        volumes: vec![DiskVolume {
            name: String::from("Macintosh HD"),
            mount: String::from("/"),
            used_bytes: 494_384_000_000,
            total_bytes: 994_662_584_320,
        }],
        read_bps: 3_400_000,
        write_bps: 780_000,
        devices: Vec::new(),
    };
}

fn histories(fx: &mut Fixture, mac: Mac) {
    *fx.histories.get_mut(SeriesId::Cpu) = cheap(11, |i, n| load(i, n, 0.12, 0.22));
    *fx.histories.get_mut(SeriesId::Gpu) = cheap(23, |i, n| 0.04 + 0.18 * triangle(i, 90) * n);
    *fx.histories.get_mut(SeriesId::Mem) = cheap(37, |i, n| {
        0.55 + 0.08 * (i as f32 / CHEAP_SAMPLES as f32) + 0.01 * n
    });
    *fx.histories.get_mut(SeriesId::NetRx) =
        cheap(41, |i, n| 20_000.0 + 2_400_000.0 * triangle(i, 150) * n);
    *fx.histories.get_mut(SeriesId::NetTx) =
        cheap(43, |i, n| 8_000.0 + 180_000.0 * triangle(i, 110) * n);
    *fx.histories.get_mut(SeriesId::DiskRead) =
        cheap(53, |i, n| 3_600_000.0 * triangle(i, 70) * n * n);
    *fx.histories.get_mut(SeriesId::DiskWrite) = cheap(59, |i, n| 900_000.0 * triangle(i, 50) * n);
    *fx.histories.get_mut(SeriesId::Disk) = {
        let (read, write) = (
            fx.histories.get(SeriesId::DiskRead).iter(),
            fx.histories.get(SeriesId::DiskWrite).iter(),
        );
        let mut h = History::for_period(CHEAP);
        for (r, w) in read.zip(write) {
            h.push(r + w);
        }
        h
    };
    *fx.histories.get_mut(SeriesId::CpuTemp) =
        sensor(71, |i, n| 46.0 + 12.0 * triangle(i, 60) + 2.0 * n);
    *fx.histories.get_mut(SeriesId::GpuTemp) =
        sensor(73, |i, n| 41.0 + 6.0 * triangle(i, 80) + 1.5 * n);
    // A kind the chip lacks keeps an empty history, like the live App.
    let zone = |kind, h: fn() -> History| {
        if mac.has(kind) {
            h()
        } else {
            History::default()
        }
    };
    *fx.histories.get_mut(SeriesId::ELoad) = zone(E, || cheap(101, |i, n| load(i, n, 0.04, 0.1)));
    *fx.histories.get_mut(SeriesId::PLoad) = zone(P, || cheap(61, |i, n| load(i, n, 0.08, 0.16)));
    *fx.histories.get_mut(SeriesId::SLoad) = zone(S, || cheap(67, |i, n| load(i, n, 0.18, 0.3)));
    *fx.histories.get_mut(SeriesId::ETemp) = zone(E, || {
        sensor(103, |i, n| 40.0 + 8.0 * triangle(i, 60) + 2.0 * n)
    });
    *fx.histories.get_mut(SeriesId::PTemp) = zone(P, || {
        sensor(79, |i, n| 44.0 + 10.0 * triangle(i, 60) + 2.0 * n)
    });
    *fx.histories.get_mut(SeriesId::STemp) = zone(S, || {
        sensor(83, |i, n| 49.0 + 14.0 * triangle(i, 60) + 2.0 * n)
    });
    let fan_curves: [fn() -> History; 2] = [
        || sensor(89, |i, n| 1_600.0 + 160.0 * triangle(i, 100) + 40.0 * n),
        || sensor(97, |i, n| 1_720.0 + 180.0 * triangle(i, 100) + 40.0 * n),
    ];
    *fx.histories.fans_mut() = fan_curves.iter().take(mac.fans).map(|f| f()).collect();
    if mac.battery {
        // Draining from 90% toward the snapshot's 82%.
        *fx.histories.get_mut(SeriesId::BatteryPct) =
            sensor(107, |i, _| 0.90 - 0.08 * (i as f32 / CHEAP_SAMPLES as f32));
    }
}

fn paint(view: &AppView<'_>, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render_app(frame, view)).unwrap();
    terminal.backend().buffer().clone()
}

/// One `|row|` line per terminal row; the bars keep trailing blanks visible.
fn text(buf: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buf.area.height {
        out.push('|');
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push_str("|\n");
    }
    out
}

/// Text rows interleaved with a style row: each cell's style as a key into
/// the legend at the bottom, keys handed out in reading order.
fn styled(buf: &Buffer) -> String {
    const KEYS: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut legend: Vec<String> = Vec::new();
    let mut out = String::new();
    for y in 0..buf.area.height {
        let mut row = String::from("|");
        let mut styles = String::from("|");
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            row.push_str(cell.symbol());
            let style = format!("fg {:?}  bg {:?}  {:?}", cell.fg, cell.bg, cell.modifier);
            let index = legend.iter().position(|s| *s == style).unwrap_or_else(|| {
                legend.push(style);
                legend.len() - 1
            });
            styles.push(KEYS.chars().nth(index).unwrap_or('?'));
        }
        let _ = writeln!(out, "{row}|");
        let _ = writeln!(out, "{styles}|");
    }
    out.push('\n');
    for (key, style) in KEYS.chars().zip(&legend) {
        let _ = writeln!(out, "{key}  {style}");
    }
    out
}

fn golden(name: &str, fx: &Fixture, width: u16, height: u16) {
    let mut settings = insta::Settings::clone_current();
    settings.set_omit_expression(true);
    settings.set_description(format!("{name}: {width}x{height}"));
    settings.bind(|| {
        insta::assert_snapshot!(name, text(&paint(&fx.view(), width, height)));
    });
}

macro_rules! surface_goldens {
    ($surface:expr => $($name:ident($width:literal, $height:literal)),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                let mut fx = m5_pro();
                fx.surface = $surface;
                golden(stringify!($name), &fx, $width, $height);
            }
        )*
    };
}

macro_rules! expanded_goldens {
    ($($name:ident($panel:expr, $width:literal, $height:literal)),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                let mut fx = m5_pro();
                fx.expanded = Some($panel);
                fx.focus = Focus::from_panel($panel);
                golden(stringify!($name), &fx, $width, $height);
            }
        )*
    };
}

surface_goldens!(Surface::Work =>
    work_60x16(60, 16),
    work_80x24(80, 24),
    work_100x30(100, 30),
    work_120x40(120, 40),
    work_160x45(160, 45),
    work_200x60(200, 60),
);

surface_goldens!(Surface::Glance =>
    glance_60x16(60, 16),
    glance_80x24(80, 24),
    glance_100x30(100, 30),
    glance_120x40(120, 40),
    glance_160x45(160, 45),
    glance_200x60(200, 60),
);

expanded_goldens!(
    expanded_cpu_120x40(Panel::Cpu, 120, 40),
    expanded_cpu_80x24(Panel::Cpu, 80, 24),
    expanded_gpu_120x40(Panel::Gpu, 120, 40),
    expanded_gpu_80x24(Panel::Gpu, 80, 24),
    expanded_mem_120x40(Panel::Mem, 120, 40),
    expanded_mem_80x24(Panel::Mem, 80, 24),
    expanded_net_120x40(Panel::Net, 120, 40),
    expanded_net_80x24(Panel::Net, 80, 24),
    expanded_disk_120x40(Panel::Disk, 120, 40),
    expanded_disk_80x24(Panel::Disk, 80, 24),
    expanded_sens_120x40(Panel::Fans, 120, 40),
    expanded_sens_80x24(Panel::Fans, 80, 24),
    expanded_power_120x40(Panel::Power, 120, 40),
    expanded_power_80x24(Panel::Power, 80, 24),
    expanded_proc_120x40(Panel::Processes, 120, 40),
    expanded_proc_80x24(Panel::Processes, 80, 24),
);

/// Every `-2m` / `-45s` left-edge time label painted on `screen`.
fn window_labels(screen: &str) -> Vec<String> {
    screen
        .split(|c: char| !(c == '\u{2212}' || c.is_ascii_alphanumeric()))
        .filter_map(|word| word.strip_prefix('\u{2212}'))
        .filter(|rest| {
            rest.strip_suffix(['s', 'm'])
                .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        })
        .map(|rest| format!("\u{2212}{rest}"))
        .collect()
}

/// Regression: compact graphs in one view showed different windows (cpu
/// -34s beside net -69s at 160x45) because live zoom tracked plot width.
#[test]
fn compact_panels_showed_different_time_windows() {
    for (surface, width, height) in [(Surface::Work, 160, 45), (Surface::Glance, 120, 40)] {
        let mut fx = m5_pro();
        fx.surface = surface;
        let labels = window_labels(&text(&paint(&fx.view(), width, height)));
        assert!(
            labels.len() >= 3,
            "{surface:?} {width}x{height}: {labels:?}"
        );
        assert!(
            labels.iter().all(|l| l == "\u{2212}2m"),
            "{surface:?} {width}x{height}: {labels:?}"
        );
    }
}

#[test]
fn z_moves_every_compact_window_together() {
    use siltop_core::Zoom;
    let fx = m5_pro();
    for zoom in Zoom::ALL {
        let view = AppView {
            zoom: Some(zoom),
            ..fx.view()
        };
        let want = crate::chrome::span_label(zoom.span(CHEAP, 0));
        let labels = window_labels(&text(&paint(&view, 160, 45)));
        assert!(labels.len() >= 3, "{zoom:?}: {labels:?}");
        assert!(labels.iter().all(|l| *l == want), "{zoom:?}: {labels:?}");
    }
}

#[test]
fn help_120x40() {
    let mut fx = m5_pro();
    fx.help = true;
    golden("help_120x40", &fx, 120, 40);
}

#[test]
fn settings_120x40() {
    let mut fx = m5_pro();
    fx.settings = true;
    golden("settings_120x40", &fx, 120, 40);
}

#[test]
fn dossier_120x40() {
    let mut fx = m5_pro();
    fx.proc.anchor = Anchor::Pinned(DOSSIER_PID);
    fx.detail_pid = Some(DOSSIER_PID);
    golden("dossier_120x40", &fx, 120, 40);
}

#[test]
fn confirm_120x40() {
    let mut fx = m5_pro();
    fx.proc.anchor = Anchor::Pinned(DOSSIER_PID);
    fx.confirm_kill = true;
    fx.confirm_pid = Some(DOSSIER_PID);
    golden("confirm_120x40", &fx, 120, 40);
}

#[test]
fn search_120x40() {
    let mut fx = m5_pro();
    fx.searching = true;
    fx.focus = Focus::Search;
    fx.proc.filter = String::from("brave");
    golden("search_120x40", &fx, 120, 40);
}

#[test]
fn work_80x24_styled() {
    let fx = m5_pro();
    let mut settings = insta::Settings::clone_current();
    settings.set_omit_expression(true);
    settings.set_description("work_80x24 with a style key per cell; legend at the bottom");
    settings.bind(|| {
        insta::assert_snapshot!("work_80x24_styled", styled(&paint(&fx.view(), 80, 24)));
    });
}

#[test]
fn renders_are_repeatable() {
    let first = text(&paint(&m5_pro().view(), 160, 45));
    let second = text(&paint(&m5_pro().view(), 160, 45));
    assert_eq!(first, second);
}

fn golden_view(name: &str, view: &AppView<'_>, width: u16, height: u16) {
    let mut settings = insta::Settings::clone_current();
    settings.set_omit_expression(true);
    settings.set_description(format!("{name}: {width}x{height}"));
    settings.bind(|| {
        insta::assert_snapshot!(name, text(&paint(view, width, height)));
    });
}

surface_goldens!(Surface::Glance =>
    glance_40x12(40, 12),
    glance_30x8(30, 8),
);

surface_goldens!(Surface::Work =>
    work_300x90(300, 90),
);

#[test]
fn work_120x40_frozen() {
    let mut fx = m5_pro();
    fx.frozen = true;
    golden("work_120x40_frozen", &fx, 120, 40);
}

#[test]
fn work_80x24_warming() {
    let mut fx = m5_pro();
    fx.ready = false;
    golden("work_80x24_warming", &fx, 80, 24);
}

#[test]
fn work_120x40_tree() {
    let mut fx = m5_pro();
    fx.show_tree = true;
    golden("work_120x40_tree", &fx, 120, 40);
}

#[test]
fn work_120x40_no_gpu() {
    let mut fx = m5_pro();
    fx.snap.gpu = None;
    golden("work_120x40_no_gpu", &fx, 120, 40);
}

#[test]
fn work_120x40_hidden_panels() {
    let fx = m5_pro();
    let mut view = fx.view();
    view.show_gpu = false;
    view.show_disk = false;
    view.show_fans = false;
    golden_view("work_120x40_hidden_panels", &view, 120, 40);
}

#[test]
fn glance_80x24_io_only() {
    let mut fx = m5_pro();
    fx.surface = Surface::Glance;
    let mut view = fx.view();
    view.show_gpu = false;
    view.show_fans = false;
    golden_view("glance_80x24_io_only", &view, 80, 24);
}

#[test]
fn work_120x40_threads_sorted_by_pid() {
    let fx = m5_pro();
    let mut view = fx.view();
    view.show_threads = true;
    view.sort = siltop_core::ProcSort::Pid;
    view.sort_reversed = true;
    view.status = Some("sorted by pid");
    golden_view("work_120x40_threads_sorted_by_pid", &view, 120, 40);
}

#[test]
fn work_80x24_ansi16_styled() {
    let mut fx = m5_pro();
    fx.theme = crate::theme::Theme::auto(None, crate::theme::ColorDepth::Ansi16);
    let mut buf = paint(&fx.view(), 80, 24);
    fx.theme.finish(&mut buf);
    let mut settings = insta::Settings::clone_current();
    settings.set_omit_expression(true);
    settings.set_description("work_80x24 lowered to 16 colours; legend at the bottom");
    settings.bind(|| {
        insta::assert_snapshot!("work_80x24_ansi16_styled", styled(&buf));
    });
}

#[test]
fn every_theme_paints_the_same_text() {
    // Themes are colour only: the glyphs on screen never change with them.
    let base = text(&paint(&m5_pro().view(), 120, 40));
    for name in crate::theme::Theme::builtins() {
        let mut fx = m5_pro();
        fx.theme = crate::theme::Theme::by_name(name).unwrap();
        assert_eq!(text(&paint(&fx.view(), 120, 40)), base, "{name}");
    }
}

#[test]
fn lowered_depths_leave_no_rgb_in_any_view() {
    use crate::theme::{ColorDepth, Theme};
    for depth in [ColorDepth::Ansi256, ColorDepth::Ansi16, ColorDepth::NoColor] {
        for expanded in [None, Some(Panel::Cpu), Some(Panel::Processes)] {
            let mut fx = m5_pro();
            fx.expanded = expanded;
            fx.theme = Theme::default().with_depth(depth);
            let mut buf = paint(&fx.view(), 100, 30);
            fx.theme.finish(&mut buf);
            for cell in &buf.content {
                assert!(
                    !matches!(cell.fg, ratatui::style::Color::Rgb(..))
                        && !matches!(cell.bg, ratatui::style::Color::Rgb(..)),
                    "{depth:?} {expanded:?} left rgb"
                );
            }
        }
    }
}

/// Every size, both surfaces: no panic, and the frame stays inside the
/// buffer (`TestBackend` panics on an out-of-range write).
fn render_sweep(widths: std::ops::RangeInclusive<u16>) {
    let fx = m5_pro();
    for width in widths.step_by(9) {
        for height in (1..=70_u16).step_by(6) {
            for surface in [Surface::Work, Surface::Glance] {
                let mut view = fx.view();
                view.surface = surface;
                let buf = paint(&view, width, height);
                assert_eq!(buf.area.width, width);
                assert_eq!(buf.area.height, height);
            }
        }
    }
}

#[test]
fn render_any_size_narrow() {
    render_sweep(1..=100);
}

#[test]
fn render_any_size_wide() {
    render_sweep(101..=240);
}

#[test]
fn overlays_survive_tiny_terminals() {
    for (width, height) in [
        (1_u16, 1_u16),
        (10, 3),
        (20, 6),
        (40, 10),
        (59, 15),
        (60, 16),
    ] {
        for overlay in 0..5 {
            let mut fx = m5_pro();
            match overlay {
                0 => fx.help = true,
                1 => fx.settings = true,
                2 => {
                    fx.proc.anchor = Anchor::Pinned(DOSSIER_PID);
                    fx.confirm_kill = true;
                    fx.confirm_pid = Some(DOSSIER_PID);
                }
                3 => fx.detail_pid = Some(DOSSIER_PID),
                _ => {
                    fx.searching = true;
                    fx.focus = Focus::Search;
                    fx.proc.filter = String::from("\u{fc}\u{1f986}\u{6f22}\u{5b57}");
                }
            }
            let _ = paint(&fx.view(), width, height);
        }
    }
}

#[test]
fn expanded_panels_render_at_every_size() {
    for panel in Panel::ALL {
        for (width, height) in [
            (1_u16, 1_u16),
            (12, 4),
            (30, 10),
            (60, 16),
            (97, 33),
            (250, 70),
        ] {
            let mut fx = m5_pro();
            fx.expanded = Some(panel);
            fx.focus = Focus::from_panel(panel);
            let _ = paint(&fx.view(), width, height);
        }
    }
}

#[test]
fn empty_snapshot_renders_every_view() {
    // A cold start: no histories, no processes, nothing present.
    for surface in [Surface::Work, Surface::Glance] {
        let mut fx = fixture("");
        fx.snap = Snapshot::empty();
        fx.surface = surface;
        fx.ready = false;
        for expanded in std::iter::once(None).chain(Panel::ALL.map(Some)) {
            fx.expanded = expanded;
            let body = text(&paint(&fx.view(), 100, 30));
            assert!(!body.contains("NaN"), "{surface:?} {expanded:?}");
            assert!(!body.contains("inf"), "{surface:?} {expanded:?}");
        }
    }
}

#[test]
fn no_view_prints_nan_or_inf() {
    let mut views = Vec::new();
    for surface in [Surface::Work, Surface::Glance] {
        let mut fx = m5_pro();
        fx.surface = surface;
        views.push((format!("{surface:?}"), fx));
    }
    for panel in Panel::ALL {
        let mut fx = m5_pro();
        fx.expanded = Some(panel);
        views.push((format!("{panel:?}"), fx));
    }
    for (name, fx) in views {
        let body = text(&paint(&fx.view(), 160, 45));
        assert!(!body.contains("NaN") && !body.contains("inf"), "{name}");
    }
}

// ---------------------------------------------------------------------------
// The Mac family: every chassis and chip shape, painted and checked.
// ---------------------------------------------------------------------------

/// What is on screen: a surface, or one panel expanded.
#[derive(Debug, Clone, Copy)]
enum Screen {
    Home(Surface),
    Expanded(Panel),
}

impl Screen {
    fn dress(self, mac: Mac) -> Fixture {
        let mut fx = mac.fixture();
        match self {
            Self::Home(surface) => fx.surface = surface,
            Self::Expanded(panel) => {
                fx.expanded = Some(panel);
                fx.focus = Focus::from_panel(panel);
            }
        }
        fx
    }
}

const CPU: Screen = Screen::Expanded(Panel::Cpu);
const HEAT: Screen = Screen::Expanded(Panel::Fans);

/// The blessed screens per Mac.
const FAMILY_GOLDENS: [(&str, Screen, u16, u16); 6] = [
    ("work_80x24", Screen::Home(Surface::Work), 80, 24),
    ("work_160x45", Screen::Home(Surface::Work), 160, 45),
    ("glance_80x24", Screen::Home(Surface::Glance), 80, 24),
    ("glance_160x45", Screen::Home(Surface::Glance), 160, 45),
    ("cpu_120x40", CPU, 120, 40),
    ("heat_120x40", HEAT, 120, 40),
];

/// The property sweep: more screens and sizes than are blessed.
const SWEEP_SCREENS: [Screen; 5] = [
    Screen::Home(Surface::Work),
    Screen::Home(Surface::Glance),
    CPU,
    HEAT,
    Screen::Expanded(Panel::Gpu),
];
const SWEEP_SIZES: [(u16, u16); 4] = [(80, 24), (120, 40), (160, 45), (200, 60)];

/// One painted frame and where it came from.
struct Shot {
    mac: Mac,
    screen: Screen,
    width: u16,
    height: u16,
    buf: Buffer,
    text: String,
    plan: crate::layout::LayoutPlan,
}

impl Shot {
    fn take(
        mac: Mac,
        screen: Screen,
        (width, height): (u16, u16),
        tweak: fn(&mut Fixture),
    ) -> Self {
        let mut fx = screen.dress(mac);
        tweak(&mut fx);
        let view = fx.view();
        let buf = paint(&view, width, height);
        let plan = super::plan(buf.area, view.surface, view.flags());
        Self {
            mac,
            screen,
            width,
            height,
            text: text(&buf),
            buf,
            plan,
        }
    }

    fn lower(&self) -> String {
        self.text.to_lowercase()
    }

    fn is_cpu(&self) -> bool {
        matches!(self.screen, Screen::Expanded(Panel::Cpu))
    }
}

impl std::fmt::Display for Shot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {:?} {}x{}\n{}",
            self.mac.label, self.screen, self.width, self.height, self.text
        )
    }
}

/// Every Mac x every sweep screen x every sweep size, after `tweak`.
fn sweep_with(tweak: fn(&mut Fixture)) -> impl Iterator<Item = Shot> {
    FAMILY.into_iter().flat_map(move |mac| {
        SWEEP_SCREENS.into_iter().flat_map(move |screen| {
            SWEEP_SIZES
                .into_iter()
                .map(move |size| Shot::take(mac, screen, size, tweak))
        })
    })
}

fn sweep() -> impl Iterator<Item = Shot> {
    sweep_with(|_| {})
}

fn family_golden(mac: Mac) {
    for (screen_name, screen, width, height) in FAMILY_GOLDENS {
        let name = format!("family_{}_{screen_name}", mac.label);
        golden(&name, &screen.dress(mac), width, height);
    }
}

macro_rules! family_goldens {
    ($($name:ident => $mac:expr),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                family_golden($mac);
            }
        )*
    };
}

// The M5 Pro's blessed screens are the legacy `work_*` / `glance_*` /
// `expanded_*` goldens above; it still runs every property below.
family_goldens!(
    family_air_m2 => AIR_M2,
    family_air_m4 => AIR_M4,
    family_mbp_m3_max => MBP_M3_MAX,
    family_mini_m4 => MINI_M4,
    family_studio_m3_ultra => STUDIO_M3_ULTRA,
    family_imac_m4 => IMAC_M4,
);

/// Core-list rows in a CPU-expanded frame: `(kind tag, index)` per row
/// shaped `│P10  34% ━━━`, plus the `+N more` marker if one is painted.
fn core_rows_on_screen(text: &str) -> (Vec<(char, u16)>, Option<usize>) {
    let mut rows = Vec::new();
    let mut more = None;
    for line in text.lines() {
        let Some(body) = line.strip_prefix("|│") else {
            continue;
        };
        if let Some(n) = body
            .strip_prefix('+')
            .and_then(|rest| rest.split_once(" more"))
            .and_then(|(n, _)| n.parse().ok())
        {
            more = Some(n);
            continue;
        }
        let mut chars = body.chars();
        let Some(tag) = chars.next().filter(|c| matches!(c, 'E' | 'P' | 'S')) else {
            continue;
        };
        let rest = chars.as_str();
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        let after = &rest[digits.len()..];
        let percent_follows = after.starts_with(' ')
            && after
                .split_whitespace()
                .next()
                .is_some_and(|t| t.ends_with('%'));
        if let (Ok(index), true) = (digits.parse(), percent_follows) {
            rows.push((tag, index));
        }
    }
    (rows, more)
}

/// A token like `0W`, `0.0W`, `0MHz`, `0.0G`: a zero reading with a unit.
fn zero_unit_tokens(text: &str) -> Vec<String> {
    const UNITS: [&str; 5] = ["W", "mW", "MHz", "GHz", "G"];
    text.split(|c: char| c.is_whitespace() || !c.is_ascii() || c == '|')
        .filter(|token| {
            UNITS.iter().any(|unit| {
                token.strip_suffix(unit).is_some_and(|number| {
                    !number.is_empty()
                        && number.chars().all(|c| c == '0' || c == '.')
                        && number.starts_with('0')
                })
            })
        })
        .map(str::to_owned)
        .collect()
}

/// `e 49°`-style zone tokens for `kind` in the sens title and strips.
fn zone_token(text: &str, kind: ClusterKind) -> bool {
    let needle = format!(" {} ", kind.tag().to_lowercase());
    text.match_indices(&needle).any(|(at, _)| {
        let rest = &text[at + needle.len()..];
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        digits > 0 && rest[digits..].starts_with('°')
    })
}

/// The words a UI uses for one cluster kind.
fn kind_words(kind: ClusterKind) -> [&'static str; 2] {
    match kind {
        E => ["efficiency", "eff zone"],
        P => ["performance", "perf zone"],
        S => ["super", "super zone"],
    }
}

#[test]
fn family_fixtures_are_internally_consistent() {
    for mac in FAMILY {
        let fx = mac.fixture();
        let soc = &fx.snap.soc;
        let total = mac.total_cores();
        assert_eq!(soc.core_kinds.len(), total, "{}", mac.label);
        assert_eq!(fx.snap.cpu.cores.len(), total, "{}", mac.label);
        assert_eq!(
            usize::from(soc.e_cores) + usize::from(soc.p_cores) + usize::from(soc.s_cores),
            total,
            "{}",
            mac.label
        );
        assert_eq!(fx.snap.fans.fans.len(), mac.fans, "{}", mac.label);
        assert_eq!(fx.histories.fans().len(), mac.fans, "{}", mac.label);
        assert_eq!(fx.snap.fans.is_present(), mac.fans > 0, "{}", mac.label);
        assert_eq!(fx.snap.battery.present, mac.battery, "{}", mac.label);
        for kind in [E, P, S] {
            let has = mac.has(kind);
            let cluster = match kind {
                E => fx.snap.cpu.e_cluster,
                P => fx.snap.cpu.p_cluster,
                S => fx.snap.cpu.s_cluster,
            };
            assert_eq!(cluster.is_some(), has, "{} {kind:?}", mac.label);
            assert_eq!(
                fx.snap.sensors.zone_temp(kind).is_some(),
                has,
                "{}",
                mac.label
            );
            assert_eq!(!fx.view().cluster_load_history(kind).is_empty(), has);
            assert_eq!(!fx.view().zone_temp_history(kind).is_empty(), has);
        }
    }
}

#[test]
fn every_core_has_a_unique_role_per_kind() {
    for mac in FAMILY {
        let cores = mac.fixture().snap.cpu.cores;
        for kind in [E, P, S] {
            let mut indices: Vec<u16> = cores
                .iter()
                .filter(|c| c.kind == kind)
                .map(|c| c.index)
                .collect();
            indices.sort_unstable();
            let want: Vec<u16> = (0..u16::from(mac.count(kind))).collect();
            assert_eq!(indices, want, "{} {kind:?}", mac.label);
        }
    }
}

#[test]
fn ultra_second_die_continues_each_kinds_numbering() {
    // Mach order on an Ultra: die 0 (E0-3, P0-11), then die 1 (E4-7, P12-23).
    let cores = STUDIO_M3_ULTRA.fixture().snap.cpu.cores;
    let roles: Vec<(ClusterKind, u16)> = cores.iter().map(|c| (c.kind, c.index)).collect();
    assert_eq!(roles[0], (E, 0));
    assert_eq!(roles[4], (P, 0));
    assert_eq!(roles[16], (E, 4));
    assert_eq!(roles[20], (P, 12));
    assert_eq!(roles[31], (P, 23));
}

#[test]
fn m5_pro_fixture_is_the_family_row() {
    // The legacy builder and the family table describe the same machine.
    let fx = m5_pro();
    assert_eq!(fx.snap.soc.name, "M5 Pro");
    assert_eq!(
        (
            fx.snap.soc.p_cores,
            fx.snap.soc.s_cores,
            fx.snap.soc.e_cores
        ),
        (12, 6, 0)
    );
    assert_eq!(fx.snap.soc.gpu_cores, 20);
    assert_eq!(fx.snap.memory.total_bytes, 24 * GIB);
    assert_eq!(fx.snap.memory.used_bytes, 15 * GIB + 205 * MIB);
}

#[test]
fn no_panel_shows_an_empty_body() {
    for shot in sweep() {
        for panel in Panel::ALL {
            let Some(rect) = shot.plan.panel(panel) else {
                continue;
            };
            if rect.width < 3 || rect.height < 3 {
                continue;
            }
            let blank = (rect.y + 1..rect.bottom() - 1).all(|y| {
                (rect.x + 1..rect.right() - 1).all(|x| shot.buf[(x, y)].symbol().trim().is_empty())
            });
            assert!(!blank, "{panel:?} body is empty on {shot}");
        }
    }
}

#[test]
fn fanless_macs_never_show_a_fan_or_rpm() {
    for shot in sweep().filter(|s| s.mac.fans == 0) {
        let lower = shot.lower();
        assert!(!lower.contains("rpm"), "rpm on {shot}");
        assert!(!lower.contains("fan"), "fan on {shot}");
    }
}

#[test]
fn fanned_macs_show_their_rpm_somewhere() {
    for mac in FAMILY.into_iter().filter(|m| m.fans > 0) {
        let shot = Shot::take(mac, HEAT, (160, 45), |_| {});
        assert!(shot.text.contains("rpm"), "{shot}");
    }
}

/// Regression: fan speeds read `1714 rpm` in the expanded cells but
/// `1714rpm` in the heat title, and fan cells were the only capitalised
/// cell labels (`Fan 1` beside `perf zone`).
#[test]
fn fan_speed_reads_one_way_everywhere() {
    for mac in FAMILY.into_iter().filter(|m| m.fans > 0) {
        for size in [(80, 24), (120, 40), (160, 45)] {
            let shot = Shot::take(mac, HEAT, size, |_| {});
            assert!(!shot.text.contains(" rpm"), "{shot}");
            assert!(!shot.text.contains("╭ Fan"), "{shot}");
        }
    }
}

#[test]
fn heat_view_has_one_fan_pane_per_fan() {
    for mac in FAMILY {
        let shot = Shot::take(mac, HEAT, (160, 45), |_| {});
        for n in 1..=2 {
            let pane = format!("╭ fan {n} ");
            assert_eq!(
                shot.text.contains(&pane),
                n <= mac.fans,
                "Fan {n} on {shot}"
            );
        }
    }
}

#[test]
fn desktops_never_show_a_battery() {
    for shot in sweep().filter(|s| !s.mac.battery) {
        assert!(!shot.lower().contains("batt"), "battery on {shot}");
    }
}

#[test]
fn zero_unit_scanner_finds_what_it_should() {
    assert_eq!(
        zero_unit_tokens("cpu 0W 0.0W 12W 0MHz 0.0G 24.0G 10W"),
        ["0W", "0.0W", "0MHz", "0.0G"]
    );
    assert!(zero_unit_tokens("0B/s 0b 0% 40° 1.5W 589MHz").is_empty());
}

#[test]
fn present_power_and_clocks_are_printed() {
    // The scanner's other half: with readings present, the units do show.
    // CPU watts live in the cpu title; the expanded frame is context only.
    let shot = Shot::take(AIR_M2, Screen::Home(Surface::Glance), (160, 45), |_| {});
    assert!(shot.text.contains("12W"), "{shot}");
    let gpu = Shot::take(AIR_M2, Screen::Expanded(Panel::Gpu), (120, 40), |_| {});
    assert!(gpu.text.contains("589MHz"), "{gpu}");
}

/// Power and clocks the machine does not report: every watt and clock
/// reading absent, cluster clocks unknown (0 means not sampled yet).
fn unreported(fx: &mut Fixture) {
    fx.snap.cpu.watts = None;
    fx.snap.cpu.freq_mhz = None;
    for cluster in [
        &mut fx.snap.cpu.e_cluster,
        &mut fx.snap.cpu.p_cluster,
        &mut fx.snap.cpu.s_cluster,
    ]
    .into_iter()
    .flatten()
    {
        cluster.freq_mhz = 0;
    }
    if let Some(gpu) = fx.snap.gpu.as_mut() {
        gpu.watts = None;
        gpu.freq_mhz = None;
        gpu.max_mhz = None;
        gpu.ane_watts = None;
    }
    fx.snap.power = siltop_core::PowerSnapshot::default();
}

#[test]
fn absent_power_and_clocks_never_print_zero() {
    for shot in sweep_with(unreported) {
        assert_eq!(zero_unit_tokens(&shot.text), Vec::<String>::new(), "{shot}");
    }
}

#[test]
fn absent_power_prints_no_watts_at_all() {
    for shot in sweep_with(unreported) {
        let watts = shot
            .text
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '.')
            .any(|t| t.ends_with('W') && t.len() > 1 && t[..t.len() - 1].parse::<f32>().is_ok());
        assert!(!watts, "watts on {shot}");
    }
}

#[test]
fn absent_cluster_kinds_are_never_named() {
    for shot in sweep() {
        let lower = shot.lower();
        for kind in [E, P, S].into_iter().filter(|k| !shot.mac.has(*k)) {
            for word in kind_words(kind) {
                assert!(!lower.contains(word), "{kind:?} '{word}' on {shot}");
            }
            assert!(!zone_token(&lower, kind), "{kind:?} zone token on {shot}");
        }
        let (rows, _) = core_rows_on_screen(&shot.text);
        for (tag, index) in rows {
            let kind = match tag {
                'E' => E,
                'P' => P,
                _ => S,
            };
            assert!(shot.mac.has(kind), "core row {tag}{index} on {shot}");
        }
    }
}

#[test]
fn present_cluster_kinds_are_named_in_the_cpu_view() {
    for shot in sweep().filter(|s| s.is_cpu() && s.width >= 120) {
        let lower = shot.lower();
        for kind in [E, P, S].into_iter().filter(|k| shot.mac.has(*k)) {
            assert!(
                lower.contains(kind_words(kind)[0]),
                "{kind:?} missing on {shot}"
            );
        }
    }
}

#[test]
fn cpu_title_clocks_follow_the_chip() {
    // `S 4.4G · P 1.3G` on the M5 Pro, `P 1.3G · E 1.0G` on an M1-M4.
    for mac in FAMILY {
        let shot = Shot::take(mac, Screen::Home(Surface::Glance), (160, 45), |_| {});
        for kind in [E, P, S] {
            let clock = format!(
                "{} {}",
                kind.tag(),
                crate::titles::ghz_display(kind_load(kind).cluster.freq_mhz)
            );
            assert_eq!(
                shot.text.contains(&clock),
                mac.has(kind),
                "{clock} on {shot}"
            );
        }
    }
}

/// `6S+12P`: the core split as the top bar writes it.
fn core_split(mac: Mac) -> String {
    [S, P, E]
        .into_iter()
        .filter(|k| mac.has(*k))
        .map(|k| format!("{}{}", mac.count(k), k.tag()))
        .collect::<Vec<_>>()
        .join("+")
}

/// The chip, core split and GPU core count on screen, each counted once
/// per occurrence (`20-core GPU` in the bar, `20-core` or `20c` in a panel).
fn identity_counts(mac: Mac, text: &str) -> [usize; 3] {
    let gpu = [
        format!("{}-core", mac.gpu_cores),
        format!("│{}c", mac.gpu_cores),
    ];
    [
        text.matches(mac.chip).count(),
        text.matches(&core_split(mac)).count(),
        gpu.iter().map(|g| text.matches(g.as_str()).count()).sum(),
    ]
}

/// Regression: the expanded CPU view repeated the top bar's identity
/// (`cpu 6S+12P` in the frame, `Apple M5 Pro` / `12P + 6S` in the meta
/// column). With the bar gone, the CPU meta column is identity's one home:
/// chip, split and GPU cores exactly once there, the GPU view keeps only
/// its own core count, and home screens carry none of it.
#[test]
fn identity_appears_once_per_screen() {
    for mac in FAMILY {
        for size in [(200, 50), (120, 40)] {
            let cpu = Shot::take(mac, CPU, size, |_| {});
            assert_eq!(identity_counts(mac, &cpu.text), [1, 1, 1], "{cpu}");
            let gpu = Shot::take(mac, Screen::Expanded(Panel::Gpu), size, |_| {});
            assert_eq!(identity_counts(mac, &gpu.text), [0, 0, 1], "{gpu}");
        }
    }
}

/// The chip is never on two rows of one screen, at any width.
#[test]
fn identity_is_never_repeated_at_any_width() {
    for mac in FAMILY {
        for width in (60..=220).step_by(4) {
            let shot = Shot::take(mac, CPU, (width, 40), |_| {});
            let [chip, split, gpu] = identity_counts(mac, &shot.text);
            assert!(chip <= 1 && split <= 1 && gpu <= 1, "{shot}");
        }
    }
}

/// Regression: `paint_cores` stopped at the pane's last row, so an Ultra
/// (32 cores) or any Mac at 80x24 lost cores without a word.
#[test]
fn core_list_never_drops_cores_silently() {
    for shot in sweep().filter(Shot::is_cpu) {
        let (rows, more) = core_rows_on_screen(&shot.text);
        let total = shot.mac.total_cores();
        if rows.is_empty() && more.is_none() {
            continue; // No core list at this size at all.
        }
        assert_eq!(rows.len() + more.unwrap_or(0), total, "{shot}");
        assert!(more.is_none() || rows.len() < total, "{shot}");
        let mut unique = rows.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), rows.len(), "duplicate core row on {shot}");
    }
}

#[test]
fn ultra_core_list_overflows_with_a_marker() {
    let shot = Shot::take(STUDIO_M3_ULTRA, CPU, (120, 40), |_| {});
    let (rows, more) = core_rows_on_screen(&shot.text);
    assert!(more.is_some_and(|n| n > 0), "{shot}");
    assert_eq!(rows.len() + more.unwrap_or(0), 32, "{shot}");
}

#[test]
fn small_macs_list_every_core_when_there_is_room() {
    let shot = Shot::take(AIR_M2, CPU, (120, 40), |_| {});
    let (rows, more) = core_rows_on_screen(&shot.text);
    assert_eq!(more, None, "{shot}");
    assert_eq!(rows.len(), 8, "{shot}");
}

#[test]
fn fanless_sens_panel_still_reports_heat() {
    for mac in FAMILY.into_iter().filter(|m| m.fans == 0) {
        let shot = Shot::take(mac, Screen::Home(Surface::Glance), (120, 40), |_| {});
        let sens = shot
            .text
            .lines()
            .find(|l| l.contains("╭ heat"))
            .unwrap_or_default();
        assert!(sens.contains('°'), "{shot}");
    }
}

#[test]
fn family_never_prints_nan_or_inf() {
    for shot in sweep() {
        assert!(
            !shot.text.contains("NaN") && !shot.text.contains("inf"),
            "{shot}"
        );
    }
}

#[test]
fn family_renders_are_repeatable() {
    for mac in FAMILY {
        let first = Shot::take(mac, CPU, (160, 45), |_| {});
        let second = Shot::take(mac, CPU, (160, 45), |_| {});
        assert_eq!(first.text, second.text, "{}", mac.label);
    }
}

#[test]
fn family_survives_tiny_terminals() {
    for mac in FAMILY {
        for screen in SWEEP_SCREENS {
            for size in [(1, 1), (12, 4), (30, 10), (59, 15), (97, 33)] {
                let shot = Shot::take(mac, screen, size, |_| {});
                assert_eq!((shot.buf.area.width, shot.buf.area.height), size);
            }
        }
    }
}

/// Regression: core labels were `P9  29%` and `P10  68%`, so each bar began
/// one cell further right than the last and the list read ragged.
#[test]
fn core_bars_start_in_one_column() {
    for shot in sweep().filter(Shot::is_cpu) {
        let starts: Vec<usize> = shot
            .text
            .lines()
            .filter(|line| core_rows_on_screen(line).0.len() == 1)
            .filter_map(|line| {
                let body: String = line.chars().skip(2).collect();
                body.chars()
                    .position(|c| crate::chrome::is_bar_fill(c) || c == crate::chrome::BAR_TRACK)
            })
            .collect();
        assert!(
            starts.windows(2).all(|w| w[0] == w[1]),
            "{starts:?} on {shot}"
        );
    }
}

#[test]
fn mem_title_shows_each_macs_ram() {
    for mac in FAMILY {
        let shot = Shot::take(mac, Screen::Home(Surface::Glance), (120, 40), |_| {});
        let total = format!("/{}.0G", mac.mem_gib);
        assert!(shot.text.contains(&total), "'{total}' on {shot}");
    }
}

/// No temperature sensor and no fan answered: a VM, or a chip whose
/// tables we do not know yet.
fn no_sensors(fx: &mut Fixture) {
    fx.snap.sensors = SensorsSnapshot::default();
    fx.snap.fans = FanSnapshot::default();
    fx.snap.cpu.temp_c = None;
    if let Some(gpu) = fx.snap.gpu.as_mut() {
        gpu.temp_c = None;
    }
    for id in [
        SeriesId::CpuTemp,
        SeriesId::GpuTemp,
        SeriesId::ETemp,
        SeriesId::PTemp,
        SeriesId::STemp,
    ] {
        *fx.histories.get_mut(id) = History::default();
    }
    fx.histories.fans_mut().clear();
}

#[test]
fn macs_without_sensors_drop_the_sens_panel() {
    // Regression: `LayoutFlags::visible` ignored `has_fans`, so a Mac with
    // no heat sensors kept an empty `sens —` box. (The heat view itself is
    // unreachable then: the app never expands a hidden panel.)
    for shot in
        sweep_with(no_sensors).filter(|s| !matches!(s.screen, Screen::Expanded(Panel::Fans)))
    {
        assert_eq!(shot.plan.fans, None, "{shot}");
        assert!(!shot.text.contains('°'), "a temperature on {shot}");
        assert!(!shot.lower().contains("rpm"), "{shot}");
    }
}

#[test]
fn macs_without_a_gpu_reading_drop_the_gpu_panel() {
    fn no_gpu(fx: &mut Fixture) {
        fx.snap.gpu = None;
        *fx.histories.get_mut(SeriesId::Gpu) = History::default();
    }
    for shot in sweep_with(no_gpu).filter(|s| !matches!(s.screen, Screen::Expanded(Panel::Gpu))) {
        assert_eq!(shot.plan.gpu, None, "{shot}");
        assert!(!shot.text.contains("gpu  17%"), "{shot}");
    }
}

#[test]
fn linear_role_fallback_paints_like_registry_kinds() {
    // Without the registry's per-core kinds, roles come from the E/P/S
    // counts in Mach order; the screen must not change.
    for mac in FAMILY {
        let registry = Shot::take(mac, CPU, (160, 45), |_| {});
        let mut fx = CPU.dress(mac);
        fx.snap.soc.core_kinds.clear();
        fx.snap.cpu.cores = cores(&fx.snap.soc, mac);
        assert_eq!(fx.snap.cpu.cores.len(), mac.total_cores(), "{}", mac.label);
        assert_eq!(
            text(&paint(&fx.view(), 160, 45)),
            registry.text,
            "{}",
            mac.label
        );
    }
}

#[test]
fn every_theme_paints_the_same_text_on_every_mac() {
    for mac in FAMILY {
        let base = Shot::take(mac, Screen::Home(Surface::Glance), (100, 30), |_| {});
        for name in crate::theme::Theme::builtins() {
            let mut fx = mac.fixture();
            fx.surface = Surface::Glance;
            fx.theme = crate::theme::Theme::by_name(name).unwrap();
            assert_eq!(
                text(&paint(&fx.view(), 100, 30)),
                base.text,
                "{name} {}",
                mac.label
            );
        }
    }
}

#[test]
fn heat_view_readings_list_the_battery_only_on_laptops() {
    // The sensor list is data, not chrome: a desktop never grows a
    // battery reading, a laptop keeps one.
    for mac in FAMILY {
        let fx = mac.fixture();
        let has = fx.snap.sensors.readings.iter().any(|r| r.name == "battery");
        assert_eq!(has, mac.battery, "{}", mac.label);
    }
}

/// Regression: the expanded disk's volumes cell was a fixed 4 rows, so two
/// mounted volumes showed only `+2 more` and no volume at any size.
#[test]
fn expanded_disk_lists_volumes_once_two_are_mounted() {
    for (width, height) in [(40, 20), (80, 24), (120, 40), (200, 70)] {
        let mut fx = m5_pro();
        fx.expanded = Some(Panel::Disk);
        fx.focus = Focus::from_panel(Panel::Disk);
        let boot = fx.snap.disk.volumes[0].clone();
        fx.snap.disk.volumes.push(DiskVolume {
            name: String::from("A Rather Long External Drive Name"),
            mount: String::from("/Volumes/External"),
            ..boot
        });
        let screen = text(&paint(&fx.view(), width, height));
        assert!(
            screen.contains("Macintosh HD"),
            "{width}x{height}: the boot volume is listed\n{screen}"
        );
        assert!(
            screen.contains("460.4G / 926.4G"),
            "{width}x{height}: used / total is never clipped\n{screen}"
        );
        if height >= 24 {
            assert!(!screen.contains("more"), "{width}x{height}\n{screen}");
            assert!(screen.contains("A Rather"), "{width}x{height}\n{screen}");
        }
    }
}
