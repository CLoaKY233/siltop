//! The process table's column registry: what each column is called, how
//! wide it draws, which way it aligns, how early it gives up its cells as
//! the table narrows, and which key sorts it. Paint and header hit-testing
//! both place columns through [`layout`], so a click always lands on the
//! column that was drawn.

use siltop_core::{CpuUnit, PowerSource, ProcSort, Process, byte_rate, bytes_short, watts_display};

use crate::braille::braille_cell;
use crate::widgets::AppView;

/// Blank cells between two columns.
pub(super) const GAP: u16 = 2;
/// The name never shrinks below this while any optional column is shown.
pub(super) const NAME_MIN: u16 = 12;
/// Room the name keeps before the table spends cells on lower-priority
/// columns (pid, threads, ...): cpu and mem only need [`NAME_MIN`].
const NAME_WANT: u16 = 16;
/// Priorities at or below this only need [`NAME_MIN`] of name.
const ESSENTIAL: u8 = 1;
/// From this priority on (disk, rss, wakeups, ...) a column only joins
/// while the name keeps [`NAME_ROOMY`] cells: a readable name beats a
/// detail column.
const SECONDARY: u8 = 5;
const NAME_ROOMY: u16 = 28;
/// Table width from which the per-row CPU trend column is offered.
pub(super) const SPARK_FROM: u16 = 96;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum ProcColumn {
    Pid,
    Name,
    User,
    State,
    Cpu,
    Spark,
    Gpu,
    Mem,
    Rss,
    Threads,
    Power,
    DiskRead,
    DiskWrite,
    Wakeups,
    Started,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Align {
    Left,
    Right,
}

/// One column's fixed facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Spec {
    pub label: &'static str,
    /// Cells, headroom for the sort arrow included. The name ignores it:
    /// it takes whatever the fixed columns leave.
    pub width: u16,
    pub align: Align,
    /// Lower keeps its place longer as the table narrows.
    pub priority: u8,
    pub sort: Option<ProcSort>,
}

const fn spec(
    label: &'static str,
    width: u16,
    align: Align,
    priority: u8,
    sort: Option<ProcSort>,
) -> Spec {
    Spec {
        label,
        width,
        align,
        priority,
        sort,
    }
}

impl ProcColumn {
    /// Display order, left to right.
    pub(super) const ALL: [Self; 15] = [
        Self::Pid,
        Self::Name,
        Self::User,
        Self::State,
        Self::Cpu,
        Self::Spark,
        Self::Gpu,
        Self::Mem,
        Self::Rss,
        Self::Threads,
        Self::Power,
        Self::DiskRead,
        Self::DiskWrite,
        Self::Wakeups,
        Self::Started,
    ];

    #[must_use]
    pub(super) const fn spec(self) -> Spec {
        use Align::{Left, Right};
        match self {
            Self::Pid => spec("pid", 7, Right, 2, Some(ProcSort::Pid)),
            Self::Name => spec("name", 0, Left, 0, Some(ProcSort::Name)),
            Self::User => spec("user", 9, Left, 4, Some(ProcSort::User)),
            Self::State => spec("state", 6, Left, 8, Some(ProcSort::State)),
            Self::Cpu => spec("cpu%", 7, Right, 0, Some(ProcSort::Cpu)),
            Self::Spark => spec("trend", 10, Left, 4, None),
            Self::Gpu => spec("gpu%", 6, Right, 3, Some(ProcSort::Gpu)),
            Self::Mem => spec("mem", 8, Right, 1, Some(ProcSort::Mem)),
            Self::Rss => spec("rss", 8, Right, 6, Some(ProcSort::Rss)),
            Self::Threads => spec("thr", 5, Right, 3, Some(ProcSort::Threads)),
            Self::Power => spec("power", 7, Right, 3, Some(ProcSort::Power)),
            Self::DiskRead => spec("read", 9, Right, 5, Some(ProcSort::DiskRead)),
            Self::DiskWrite => spec("write", 9, Right, 5, Some(ProcSort::DiskWrite)),
            Self::Wakeups => spec("wake/s", 7, Right, 7, Some(ProcSort::Wakeups)),
            Self::Started => spec("started", 8, Right, 9, Some(ProcSort::Started)),
        }
    }
}

/// Which situational columns exist before width is considered. A column
/// no listed process has a value for hides rather than drawing a stack of
/// `—`: absent data never takes a user's cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct Show {
    /// The user asked for the thread count.
    pub threads: bool,
    /// Running on battery, where per-process energy is what matters.
    pub battery: bool,
    /// Bit `col as u32` is set when some process has a value for `col`.
    pub present: u32,
}

impl Show {
    /// Everything on: the widest table a view could ask for.
    #[cfg(test)]
    pub(super) const ALL: Self = Self {
        threads: true,
        battery: true,
        present: u32::MAX,
    };

    /// This snapshot's columns plus every sticky one seen before it
    /// ([`ProcView::columns_seen`](super::ProcView::columns_seen)), so the
    /// set does not change from tick to tick under the pointer.
    #[must_use]
    pub(super) fn for_view(view: &AppView<'_>) -> Self {
        let mut show = Self::for_snapshot(
            &view.snapshot.processes,
            view.snapshot.battery.source,
            view.show_threads,
        );
        show.present |= view.proc.columns_seen;
        show
    }

    #[must_use]
    pub(super) fn for_snapshot(processes: &[Process], source: PowerSource, threads: bool) -> Self {
        Self {
            threads,
            battery: source == PowerSource::Battery,
            present: present(processes),
        }
    }

    const fn has(self, col: ProcColumn) -> bool {
        self.present & col.bit() != 0
    }

    const fn allows(self, col: ProcColumn, width: u16) -> bool {
        match col {
            ProcColumn::Pid | ProcColumn::Name | ProcColumn::Cpu | ProcColumn::Mem => true,
            ProcColumn::Threads => self.threads && self.has(col),
            ProcColumn::Power => self.battery && self.has(col),
            ProcColumn::Spark => width >= SPARK_FROM && self.has(col),
            _ => self.has(col),
        }
    }
}

/// Bit `col as u32` for every column some process has a value for.
#[must_use]
pub(super) fn present(processes: &[Process]) -> u32 {
    processes.iter().fold(0_u32, |bits, p| {
        ProcColumn::ALL
            .into_iter()
            .filter(|&col| has_value(col, p))
            .fold(bits, |bits, col| bits | col.bit())
    })
}

/// The columns [`present`] finds that stay once seen: all but the state
/// column, which only earns its cells while something is stopped.
#[must_use]
pub(super) fn sticky(processes: &[Process]) -> u32 {
    present(processes) & !ProcColumn::State.bit()
}

/// Whether `proc` has something worth a column under `col`. The state
/// column only earns its cells while something is stopped or a zombie.
/// Pid, name, cpu and mem always draw.
fn has_value(col: ProcColumn, p: &Process) -> bool {
    match col {
        ProcColumn::State => matches!(p.status, "stopped" | "zombie"),
        ProcColumn::User => !p.user.is_empty(),
        ProcColumn::Spark => !p.cpu_spark.is_empty(),
        // The GPU walk reads every client without privileges, so a
        // restricted row's share (WindowServer's, most often) is real.
        ProcColumn::Gpu => p.gpu > 0.0,
        ProcColumn::Rss => p.rss_bytes.is_some(),
        ProcColumn::Threads => p.threads.is_some(),
        ProcColumn::Power => p.energy_w.is_some(),
        ProcColumn::DiskRead => p.disk_read_bps.is_some(),
        ProcColumn::DiskWrite => p.disk_write_bps.is_some(),
        ProcColumn::Wakeups => p.wakeups_per_s.is_some(),
        ProcColumn::Started => p.start_unix > 0,
        ProcColumn::Pid | ProcColumn::Name | ProcColumn::Cpu | ProcColumn::Mem => true,
    }
}

impl ProcColumn {
    #[must_use]
    pub(super) const fn bit(self) -> u32 {
        1 << self as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Placed {
    pub col: ProcColumn,
    pub x: u16,
    pub width: u16,
}

/// Column positions for a table `width` cells wide. Columns join in
/// priority order while the name keeps its room, and stop at the first one
/// that does not fit, so a narrow low-priority column never jumps ahead of
/// a wider, more important one. The name takes every cell left over.
#[must_use]
pub(super) fn layout(width: u16, show: Show) -> Vec<Placed> {
    let mut ranked: Vec<ProcColumn> = ProcColumn::ALL
        .into_iter()
        .filter(|&c| c != ProcColumn::Name && show.allows(c, width))
        .collect();
    ranked.sort_by_key(|c| c.spec().priority);
    let mut used: u16 = 0;
    let mut chosen = Vec::with_capacity(ranked.len());
    for col in ranked {
        let spec = col.spec();
        let reserve = match spec.priority {
            0..=ESSENTIAL => NAME_MIN,
            p if p < SECONDARY => NAME_WANT,
            _ => NAME_ROOMY,
        };
        let need = used.saturating_add(spec.width + GAP);
        if need.saturating_add(reserve) > width {
            break;
        }
        used = need;
        chosen.push(col);
    }
    let name_w = width.saturating_sub(used).max(1);
    let mut x: u16 = 0;
    ProcColumn::ALL
        .into_iter()
        .filter(|c| *c == ProcColumn::Name || chosen.contains(c))
        .map(|col| {
            let w = if col == ProcColumn::Name {
                name_w
            } else {
                col.spec().width
            };
            let placed = Placed { col, x, width: w };
            x = x.saturating_add(w + GAP);
            placed
        })
        .collect()
}

/// The column under `x` (relative to the table's left edge).
#[must_use]
pub(super) fn column_at(width: u16, show: Show, x: u16) -> Option<ProcColumn> {
    layout(width, show)
        .into_iter()
        .find(|p| x >= p.x && x < p.x.saturating_add(p.width))
        .map(|p| p.col)
}

/// How a cell is coloured; the table maps it onto theme styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tone {
    Fg,
    Dim,
    Cpu,
    Gpu,
    Warn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Cell {
    pub text: String,
    pub tone: Tone,
}

impl Cell {
    fn new(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }

    fn unknown() -> Self {
        Self::new(UNKNOWN, Tone::Dim)
    }

    /// A number that is dim when it reads as nothing.
    fn quantity(text: String, zero: bool, tone: Tone) -> Self {
        let tone = if zero { Tone::Dim } else { tone };
        Self { text, tone }
    }
}

/// What a value the kernel withheld renders as: never a made-up zero.
pub(super) const UNKNOWN: &str = "—";

/// A CPU percent at one decimal, or [`UNKNOWN`].
#[must_use]
pub(super) fn cpu_text(cpu: Option<f32>) -> String {
    cpu.filter(|c| c.is_finite())
        .map_or_else(|| String::from(UNKNOWN), |c| format!("{:.1}", c.max(0.0)))
}

/// What every cell of one frame reads besides its process.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct CellCtx {
    /// Ages the start time.
    pub(super) now_unix: i64,
    /// What a per-core percent is divided by: 1 under `cpu_unit = "core"`,
    /// the logical CPU count under `"total"`, where 100% is the whole chip.
    pub(super) cpu_div: f32,
}

impl CellCtx {
    /// Per-core percent at `now_unix`.
    #[cfg(test)]
    #[must_use]
    pub(super) const fn at(now_unix: i64) -> Self {
        Self {
            now_unix,
            cpu_div: 1.0,
        }
    }

    /// The view's cpu unit over the snapshot's logical CPUs.
    #[must_use]
    pub(super) fn for_view(view: &AppView<'_>, now_unix: i64) -> Self {
        Self {
            now_unix,
            cpu_div: cpu_divisor(view.config.cpu_unit, view.snapshot.process_ncpu),
        }
    }
}

/// See [`CellCtx::cpu_div`]. An unknown CPU count (0) stays per-core
/// rather than dividing by nothing.
#[must_use]
pub(super) fn cpu_divisor(unit: CpuUnit, ncpu: u32) -> f32 {
    match unit {
        CpuUnit::Total if ncpu > 0 => ncpu as f32,
        _ => 1.0,
    }
}

/// The cpu column's header: `cpu%` per core, `chip%` as a share of the
/// whole chip, so the two scales never read alike.
#[must_use]
pub(super) const fn cpu_label(unit: CpuUnit) -> &'static str {
    match unit {
        CpuUnit::Core => "cpu%",
        CpuUnit::Total => "chip%",
    }
}

/// The text and tone of every column but the name and the trend, which
/// the table paints itself.
#[must_use]
pub(super) fn cell(col: ProcColumn, proc: &Process, ctx: CellCtx) -> Cell {
    let bytes =
        |v: Option<u64>| v.map_or_else(Cell::unknown, |b| Cell::new(bytes_short(b), Tone::Fg));
    let rate = |v: Option<f64>| {
        v.filter(|r| r.is_finite()).map_or_else(Cell::unknown, |r| {
            let r = r.max(0.0).round() as u64;
            Cell::quantity(byte_rate(r), r == 0, Tone::Fg)
        })
    };
    match col {
        ProcColumn::Pid => Cell::new(proc.pid.to_string(), Tone::Dim),
        ProcColumn::Name | ProcColumn::Spark => Cell::new("", Tone::Fg),
        ProcColumn::User if proc.user.is_empty() => Cell::unknown(),
        ProcColumn::User => Cell::new(proc.user.clone(), Tone::Dim),
        ProcColumn::State => match proc.status {
            "stopped" => Cell::new("stop", Tone::Warn),
            "zombie" => Cell::new("zomb", Tone::Warn),
            _ => Cell::new("·", Tone::Dim),
        },
        ProcColumn::Cpu => {
            let text = cpu_text(proc.cpu.map(|c| c / ctx.cpu_div));
            let quiet = proc.cpu.is_none() || text == "0.0";
            Cell::quantity(text, quiet, Tone::Cpu)
        }
        ProcColumn::Gpu => {
            let text = format!("{:.1}", proc.gpu.max(0.0));
            let quiet = text == "0.0";
            Cell::quantity(text, quiet, Tone::Gpu)
        }
        ProcColumn::Mem => bytes(proc.mem_bytes),
        ProcColumn::Rss => bytes(proc.rss_bytes),
        ProcColumn::Threads => proc
            .threads
            .map_or_else(Cell::unknown, |t| Cell::new(t.to_string(), Tone::Dim)),
        ProcColumn::Power => {
            proc.energy_w
                .filter(|w| w.is_finite())
                .map_or_else(Cell::unknown, |w| {
                    let text = watts_display(w.max(0.0));
                    Cell::quantity(text, w < 0.05, Tone::Fg)
                })
        }
        ProcColumn::DiskRead => rate(proc.disk_read_bps),
        ProcColumn::DiskWrite => rate(proc.disk_write_bps),
        ProcColumn::Wakeups => {
            proc.wakeups_per_s
                .filter(|w| w.is_finite())
                .map_or_else(Cell::unknown, |w| {
                    let w = w.max(0.0).round();
                    Cell::quantity(format!("{w:.0}"), w == 0.0, Tone::Fg)
                })
        }
        ProcColumn::Started => started(proc.start_unix, ctx.now_unix)
            .map_or_else(Cell::unknown, |text| Cell::new(text, Tone::Dim)),
    }
}

/// How long ago a process started, as [`age`]; `None` when the start is
/// unknown (0, as in an old trace). The table and the dossier both read
/// it, so one process never shows two ages.
#[must_use]
pub(super) fn started(start_unix: i64, now_unix: i64) -> Option<String> {
    (start_unix > 0).then(|| age(now_unix.saturating_sub(start_unix)))
}

/// Compact age: `42s`, `17m`, `5h03`, `12d`.
#[must_use]
pub(super) fn age(secs: i64) -> String {
    let secs = secs.max(0);
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h{:02}", secs / 3600, secs % 3600 / 60),
        _ => format!("{}d", secs / 86_400),
    }
}

/// A one-row braille trend of the newest samples that fit `width` cells,
/// two samples per cell, scaled so one full core fills a cell. Right
/// aligned: the newest sample sits against the column's right edge.
#[must_use]
pub(super) fn micro_spark(samples: &[f32], width: u16) -> String {
    let cap = usize::from(width) * 2;
    let recent = &samples[samples.len().saturating_sub(cap)..];
    let level = |v: f32| -> u8 {
        if !v.is_finite() || v <= 0.0 {
            0
        } else {
            ((v / 100.0).clamp(0.0, 1.0) * 4.0).ceil() as u8
        }
    };
    let mut cells: Vec<char> = recent
        .rchunks(2)
        .map(|pair| match pair {
            [a, b] => braille_cell(level(*a), level(*b)),
            [b] => braille_cell(0, level(*b)),
            _ => ' ',
        })
        .collect();
    cells.reverse();
    let pad = usize::from(width).saturating_sub(cells.len());
    std::iter::repeat_n(' ', pad).chain(cells).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    fn has(width: u16, show: Show, col: ProcColumn) -> bool {
        layout(width, show).iter().any(|p| p.col == col)
    }

    #[test]
    fn column_drop_sweep_keeps_the_name_and_never_overflows() {
        for show in [Show::default(), Show::ALL] {
            for width in 21..=240_u16 {
                let cols = layout(width, show);
                let name = cols
                    .iter()
                    .find(|p| p.col == ProcColumn::Name)
                    .expect("name column");
                assert!(name.width >= NAME_MIN, "{width}: name {}", name.width);
                let last = cols.last().unwrap();
                assert!(last.x + last.width <= width, "{width} overflows");
                for pair in cols.windows(2) {
                    assert_eq!(pair[0].x + pair[0].width + GAP, pair[1].x, "{width}");
                }
            }
        }
    }

    #[test]
    fn columns_drop_in_priority_order_as_the_table_narrows() {
        for show in [Show::default(), Show::ALL] {
            let mut prev: Option<Vec<ProcColumn>> = None;
            for width in (21..=240_u16).rev() {
                let now: Vec<ProcColumn> = layout(width, show).iter().map(|p| p.col).collect();
                // Crossing the trend's width gate frees its cells for others.
                let gate = width + 1 == SPARK_FROM;
                if let Some(prev) = prev.as_ref().filter(|_| !gate) {
                    for col in &now {
                        assert!(prev.contains(col), "{col:?} appeared at {width}");
                    }
                    let worst_kept = now.iter().map(|c| c.spec().priority).max().unwrap_or(0);
                    let gated = |c: &&ProcColumn| **c != ProcColumn::Spark;
                    for gone in prev.iter().filter(gated).filter(|c| !now.contains(c)) {
                        assert!(
                            gone.spec().priority >= worst_kept,
                            "{gone:?} dropped before a lower-ranked column at {width}"
                        );
                    }
                }
                prev = Some(now);
            }
        }
    }

    #[test]
    fn pid_then_mem_drop_as_the_table_narrows() {
        let show = Show::default();
        assert!(has(44, show, ProcColumn::Pid));
        assert!(!has(43, show, ProcColumn::Pid));
        assert!(has(31, show, ProcColumn::Mem));
        assert!(!has(30, show, ProcColumn::Mem));
        assert!(has(21, show, ProcColumn::Cpu));
        let threads = Show {
            threads: true,
            battery: false,
            present: ProcColumn::Threads.bit(),
        };
        assert!(!has(50, threads, ProcColumn::Threads));
        assert!(has(51, threads, ProcColumn::Threads));
    }

    #[test]
    fn situational_columns_need_their_situation() {
        let wide = 240;
        let off = Show::default();
        for col in [ProcColumn::Threads, ProcColumn::Gpu, ProcColumn::Power] {
            assert!(!has(wide, off, col), "{col:?} hidden when off");
            assert!(has(wide, Show::ALL, col), "{col:?} shown when on");
        }
        assert!(!has(SPARK_FROM - 1, Show::ALL, ProcColumn::Spark));
        assert!(has(SPARK_FROM + 60, Show::ALL, ProcColumn::Spark));
    }

    #[test]
    fn gpu_shows_when_any_process_uses_it_and_power_on_battery() {
        let mut procs = vec![Process::default(), Process::default()];
        let wide = 240;
        let show = Show::for_snapshot(&procs, PowerSource::Battery, true);
        for col in [ProcColumn::Gpu, ProcColumn::Power, ProcColumn::Threads] {
            assert!(!has(wide, show, col), "{col:?}: no data, no column");
        }
        procs[1].gpu = 3.0;
        procs[1].energy_w = Some(0.4);
        procs[0].threads = Some(4);
        let show = Show::for_snapshot(&procs, PowerSource::Battery, true);
        for col in [ProcColumn::Gpu, ProcColumn::Power, ProcColumn::Threads] {
            assert!(has(wide, show, col), "{col:?}");
        }
        let show = Show::for_snapshot(&procs, PowerSource::Ac, false);
        assert!(!has(wide, show, ProcColumn::Power), "power only on battery");
        assert!(
            !has(wide, show, ProcColumn::Threads),
            "threads only on request"
        );
    }

    #[test]
    fn regression_all_dash_columns_hide() {
        // Every row restricted: rss/disk/wakeups/started have no values.
        let procs = vec![
            Process {
                restricted: true,
                ..Process::default()
            };
            3
        ];
        let show = Show::for_snapshot(&procs, PowerSource::Battery, true);
        let cols: Vec<ProcColumn> = layout(240, show).iter().map(|p| p.col).collect();
        assert_eq!(
            cols,
            [
                ProcColumn::Pid,
                ProcColumn::Name,
                ProcColumn::Cpu,
                ProcColumn::Mem
            ]
        );
    }

    #[test]
    fn regression_state_column_only_while_something_is_stopped() {
        let mut procs = vec![Process::default(); 2];
        let show = |procs: &[Process]| Show::for_snapshot(procs, PowerSource::Ac, false);
        assert!(
            !has(240, show(&procs), ProcColumn::State),
            "a column of dots"
        );
        procs[1].status = "zombie";
        assert!(has(240, show(&procs), ProcColumn::State));
    }

    #[test]
    fn every_sortable_header_fits_its_arrow() {
        for col in ProcColumn::ALL {
            let spec = col.spec();
            if col != ProcColumn::Name {
                assert!(
                    spec.label.width() < usize::from(spec.width),
                    "{col:?}: {} + arrow in {}",
                    spec.label,
                    spec.width
                );
            }
        }
        let keys: Vec<ProcSort> = ProcColumn::ALL
            .iter()
            .filter_map(|c| c.spec().sort)
            .collect();
        for sort in ProcSort::ALL {
            assert!(keys.contains(&sort), "{sort:?} has a column");
        }
    }

    #[test]
    fn column_at_matches_painted_columns() {
        for width in [40_u16, 60, 90, 160] {
            for placed in layout(width, Show::ALL) {
                let mid = placed.x + placed.width / 2;
                assert_eq!(column_at(width, Show::ALL, mid), Some(placed.col));
                if placed.x > 0 {
                    assert_eq!(column_at(width, Show::ALL, placed.x - 1), None, "gap");
                }
            }
        }
    }

    #[test]
    fn d23_restricted_cells_are_dim_dashes_never_zero() {
        let proc = Process {
            restricted: true,
            gpu: 0.0,
            ..Process::default()
        };
        for col in [
            ProcColumn::Cpu,
            ProcColumn::Mem,
            ProcColumn::Rss,
            ProcColumn::Threads,
            ProcColumn::Power,
            ProcColumn::DiskRead,
            ProcColumn::DiskWrite,
            ProcColumn::Wakeups,
            ProcColumn::Started,
            ProcColumn::User,
        ] {
            assert_eq!(cell(col, &proc, CellCtx::at(0)), Cell::unknown(), "{col:?}");
        }
    }

    #[test]
    fn restricted_rows_still_show_their_unprivileged_gpu_share() {
        let windowserver = Process {
            restricted: true,
            gpu: 4.9,
            ..Process::default()
        };
        assert_eq!(
            cell(ProcColumn::Gpu, &windowserver, CellCtx::at(0)),
            Cell::new("4.9", Tone::Gpu)
        );
        let show = Show::for_snapshot(std::slice::from_ref(&windowserver), PowerSource::Ac, false);
        assert!(
            has(240, show, ProcColumn::Gpu),
            "the column earns its place"
        );
        let idle = Process {
            gpu: 0.0,
            ..windowserver
        };
        assert_ne!(
            cell(ProcColumn::Gpu, &idle, CellCtx::at(0)),
            Cell::unknown(),
            "a real zero"
        );
    }

    #[test]
    fn readable_cells_format_their_units() {
        let proc = Process {
            pid: 42,
            user: String::from("cloaky"),
            status: "zombie",
            cpu: Some(0.0),
            gpu: 12.34,
            rss_bytes: Some(3 << 20),
            energy_w: Some(1.25),
            disk_read_bps: Some(0.0),
            disk_write_bps: Some(2_000_000.0),
            wakeups_per_s: Some(14.6),
            start_unix: 1000,
            ..Process::default()
        };
        let text = |col| cell(col, &proc, CellCtx::at(1000 + 7260));
        assert_eq!(text(ProcColumn::Pid).text, "42");
        assert_eq!(text(ProcColumn::State), Cell::new("zomb", Tone::Warn));
        assert_eq!(text(ProcColumn::Cpu), Cell::new("0.0", Tone::Dim));
        assert_eq!(text(ProcColumn::Gpu), Cell::new("12.3", Tone::Gpu));
        assert_eq!(text(ProcColumn::Rss).text, "3M");
        assert_eq!(text(ProcColumn::DiskRead), Cell::new("0", Tone::Dim));
        assert_eq!(text(ProcColumn::DiskWrite).text, "2.0MB/s");
        assert_eq!(text(ProcColumn::Wakeups).text, "15");
        assert_eq!(text(ProcColumn::Started).text, "2h01");
        assert!(text(ProcColumn::Power).text.ends_with('W'));
    }

    #[test]
    fn age_reads_compactly() {
        assert_eq!(age(-5), "0s");
        assert_eq!(age(42), "42s");
        assert_eq!(age(17 * 60 + 3), "17m");
        assert_eq!(age(5 * 3600 + 3 * 60), "5h03");
        assert_eq!(age(12 * 86_400 + 5), "12d");
    }

    #[test]
    fn micro_spark_is_right_aligned_and_exactly_as_wide_as_asked() {
        assert_eq!(micro_spark(&[], 4), "    ");
        let s = micro_spark(&[100.0, 100.0], 4);
        assert_eq!(s.chars().count(), 4);
        assert_eq!(s.chars().last(), Some(braille_cell(4, 4)));
        let many: Vec<f32> = (0..50).map(|i| i as f32).collect();
        assert_eq!(micro_spark(&many, 8).chars().count(), 8);
        let idle = micro_spark(&[0.0, 0.0, f32::NAN, -3.0], 2);
        assert!(idle.chars().all(|c| c == braille_cell(0, 0)), "{idle:?}");
        assert_eq!(micro_spark(&[0.5], 1), braille_cell(0, 1).to_string());
    }
}
