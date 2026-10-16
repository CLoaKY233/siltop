use ratatui::layout::{Constraint, Layout, Rect};
use siltop_core::{PROC_RATIO_MAX, PROC_RATIO_MIN, ProcSort, Surface};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Panel {
    Cpu,
    Gpu,
    Mem,
    Net,
    Disk,
    Fans,
    Power,
    Processes,
}

impl Panel {
    pub const ALL: [Self; 8] = [
        Self::Cpu,
        Self::Gpu,
        Self::Mem,
        Self::Net,
        Self::Disk,
        Self::Fans,
        Self::Power,
        Self::Processes,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Gpu => "gpu",
            Self::Mem => "mem",
            Self::Net => "net",
            Self::Disk => "disk",
            Self::Fans => "heat",
            Self::Power => "power",
            Self::Processes => "proc",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutFlags {
    pub show_gpu: bool,
    pub show_net: bool,
    pub show_disk: bool,
    pub show_fans: bool,
    pub show_power: bool,
    pub has_gpu: bool,
    pub has_fans: bool,
    /// A battery, or a system power reading on a desktop.
    pub has_power: bool,
    pub has_disk: bool,
    pub expanded: Option<Panel>,
    pub proc_ratio: u16,
    /// Zen with nothing to say: the graphs take the footer's row too.
    pub no_footer: bool,
}

impl Default for LayoutFlags {
    fn default() -> Self {
        Self {
            show_gpu: true,
            show_net: true,
            show_disk: true,
            show_fans: true,
            show_power: true,
            has_gpu: true,
            has_fans: true,
            has_power: true,
            has_disk: true,
            expanded: None,
            proc_ratio: siltop_core::PROC_RATIO_DEFAULT,
            no_footer: false,
        }
    }
}

impl LayoutFlags {
    #[must_use]
    pub fn visible(self, panel: Panel) -> bool {
        match panel {
            Panel::Cpu | Panel::Mem | Panel::Processes => true,
            Panel::Gpu => self.show_gpu && self.has_gpu,
            Panel::Net => self.show_net,
            Panel::Disk => self.show_disk && self.has_disk,
            Panel::Fans => self.show_fans && self.has_fans,
            Panel::Power => self.show_power && self.has_power,
        }
    }

    #[must_use]
    pub fn visible_panels(self) -> Vec<Panel> {
        Panel::ALL
            .into_iter()
            .filter(|p| self.visible(*p))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutPlan {
    pub surface: Surface,
    pub cpu: Option<Rect>,
    pub gpu: Option<Rect>,
    pub mem: Option<Rect>,
    pub net: Option<Rect>,
    pub disk: Option<Rect>,
    pub fans: Option<Rect>,
    pub power: Option<Rect>,
    pub processes: Option<Rect>,
    pub split: Option<Rect>,
    pub footer: Rect,
    pub expanded: Option<Panel>,
}

impl LayoutPlan {
    #[must_use]
    pub fn panel(&self, panel: Panel) -> Option<Rect> {
        match panel {
            Panel::Cpu => self.cpu,
            Panel::Gpu => self.gpu,
            Panel::Mem => self.mem,
            Panel::Net => self.net,
            Panel::Disk => self.disk,
            Panel::Fans => self.fans,
            Panel::Power => self.power,
            Panel::Processes => self.processes,
        }
    }

    /// Where a frame of `area` draws its `[↗]` / `[×]` mark: the one
    /// geometry both `chrome::panel_block` and clicks read.
    #[must_use]
    pub fn corner_hit(area: Rect) -> Option<Rect> {
        if area.width < CORNER_MIN_WIDTH || area.height == 0 {
            return None;
        }
        Some(Rect {
            x: area
                .x
                .saturating_add(area.width.saturating_sub(CORNER_W + 1)),
            y: area.y,
            width: CORNER_W,
            height: 1,
        })
    }

    #[must_use]
    pub fn close_hit(&self) -> Option<Rect> {
        let panel = self.expanded?;
        Self::corner_hit(self.panel(panel)?)
    }

    #[must_use]
    pub fn expand_hit(&self, panel: Panel) -> Option<Rect> {
        if self.expanded.is_some() {
            return None;
        }
        Self::corner_hit(self.panel(panel)?)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Panel(Panel),
    Expand(Panel),
    ExpandClose,
    Search,
    /// The kept filter's `✕`: clears it.
    FilterClear,
    SortBy(ProcSort),
    /// Index into the process model (scroll offset already applied).
    ProcRow(usize),
    Split,
    Help,
    Settings,
    /// Settings, opened on the appearance page.
    Themes,
    /// Settings, opened on the about page.
    About,
    /// The update dialog, from the footer badge or the main menu.
    CheckUpdate,
    /// The Timeline page (long history).
    Timeline,
    /// The footer's `siltop ≡`: opens the main menu.
    Menu,
    /// The footer's verdict: a busy verdict selects its process.
    Verdict,
    Kill,
    Quit,
    Work,
    Pause,
    HopNext,
    ConfirmYes,
    ConfirmNo,
    DetailTerm,
    DetailKill,
    DetailInterrupt,
    DetailClose,
}

/// The corner marks, bracketed so they read as buttons.
pub const CORNER_EXPAND: &str = "[↗]";
pub const CORNER_CLOSE: &str = "[×]";
/// The `[×]` on a popup card's top border, one in from its right corner;
/// `None` on a card too narrow to spare it beside its title.
#[must_use]
pub fn card_close(card: Rect) -> Option<Rect> {
    (card.width > CORNER_W + 8 && card.height > 0)
        .then(|| Rect::new(card.x + card.width - CORNER_W - 1, card.y, CORNER_W, 1))
}

/// Paints [`card_close`]'s mark, when the card has room for it.
pub(crate) fn paint_card_close(
    frame: &mut ratatui::Frame,
    card: Rect,
    theme: &crate::theme::Theme,
) {
    if let Some(r) = card_close(card) {
        frame
            .buffer_mut()
            .set_string(r.x, r.y, CORNER_CLOSE, theme.dim());
    }
}

/// Columns a corner mark takes, one in from the frame's right corner.
pub(crate) const CORNER_W: u16 = 3;
/// Narrower frames have no room for a mark beside their corners.
const CORNER_MIN_WIDTH: u16 = CORNER_W + 2;

/// Auto-surface and the planner share one gate (24 proc + 36 metrics).
const WORK_MIN_WIDTH: u16 = siltop_core::WORK_MIN_COLS;
const WORK_MIN_HEIGHT: u16 = siltop_core::WORK_MIN_ROWS;

/// Whether the Work surface can show in `area`; below this it falls back
/// to Glance, so asking for Work there changes nothing on screen.
#[must_use]
pub fn work_fits(area: Rect) -> bool {
    area.height >= WORK_MIN_HEIGHT && area.width >= WORK_MIN_WIDTH
}

/// What to tell the user who asks for Work where it cannot show.
pub const WORK_TOO_SMALL: &str = "work needs 60×16";

/// The panels `Tab` hops between from an expanded `panel`: its related
/// ring, less the members this machine or config hides. Fewer than two
/// means Tab has nowhere to go.
#[must_use]
pub fn hop_ring(flags: LayoutFlags, panel: Panel) -> Vec<Panel> {
    let ring: &[Panel] = match panel {
        Panel::Cpu | Panel::Gpu | Panel::Fans | Panel::Power => {
            &[Panel::Cpu, Panel::Gpu, Panel::Fans, Panel::Power]
        }
        Panel::Net | Panel::Disk => &[Panel::Net, Panel::Disk],
        Panel::Mem | Panel::Processes => &[Panel::Mem, Panel::Processes],
    };
    ring.iter().copied().filter(|p| flags.visible(*p)).collect()
}

pub fn plan(area: Rect, surface: Surface, flags: LayoutFlags) -> LayoutPlan {
    let surface = match surface {
        Surface::Work if work_fits(area) => Surface::Work,
        Surface::Glance | Surface::Work => Surface::Glance,
    };

    // Graphs own every row but the footer: chrome lives at the bottom.
    // A quiet Zen gives them that row too (the footer sits past the end).
    let footer_h = u16::from(!flags.no_footer);
    let chunks = Layout::vertical([Constraint::Fill(1), Constraint::Length(footer_h)]).split(area);
    let body = chunks[0];
    let footer = chunks[1];

    match (flags.expanded, surface) {
        (Some(panel), _) => expanded_plan(surface, body, footer, panel),
        (None, Surface::Glance) => glance_plan(body, footer, flags),
        (None, Surface::Work) => work_plan(body, footer, flags),
    }
}

fn empty_plan(surface: Surface, footer: Rect) -> LayoutPlan {
    LayoutPlan {
        surface,
        cpu: None,
        gpu: None,
        mem: None,
        net: None,
        disk: None,
        fans: None,
        power: None,
        processes: None,
        split: None,
        footer,
        expanded: None,
    }
}

fn expanded_plan(surface: Surface, body: Rect, footer: Rect, panel: Panel) -> LayoutPlan {
    let mut planned = empty_plan(surface, footer);
    planned.expanded = Some(panel);
    match panel {
        Panel::Cpu => planned.cpu = Some(body),
        Panel::Gpu => planned.gpu = Some(body),
        Panel::Mem => planned.mem = Some(body),
        Panel::Net => planned.net = Some(body),
        Panel::Disk => planned.disk = Some(body),
        Panel::Fans => planned.fans = Some(body),
        Panel::Power => planned.power = Some(body),
        Panel::Processes => planned.processes = Some(body),
    }
    planned
}

/// The hero CPU graph never drops below this many rows while strips show.
const GLANCE_HERO_MIN: u16 = 5;
/// A strip is a bordered one-row graph at its smallest.
const GLANCE_STRIP_MIN: u16 = 3;
/// Strips stacked one per row want at least this many rows each; below it
/// they pair up two to a row instead (when the width allows).
const GLANCE_STRIP_COMFORT: u16 = 6;
/// Two strips side by side need this many columns between them.
const GLANCE_PAIR_MIN_WIDTH: u16 = 60;
/// From this width the strips always form a two-column grid.
const GLANCE_GRID_MIN_WIDTH: u16 = 140;
/// Wide bodies shorter than this set the hero beside the strips, not above.
const GLANCE_SIDE_MAX_HEIGHT: u16 = 24;
/// The secondary panels, in reading order under (or beside) the hero.
const GLANCE_STRIPS: [Panel; 6] = [
    Panel::Gpu,
    Panel::Mem,
    Panel::Net,
    Panel::Disk,
    Panel::Fans,
    Panel::Power,
];

/// Two fifths of `height`, the hero's usual share of a stacked Glance body.
fn two_fifths(height: u16) -> u16 {
    u16::try_from(u32::from(height) * 2 / 5).unwrap_or(u16::MAX)
}

/// The hero's rows over `strip_rows` rows of strips: two fifths of the body,
/// or an equal share when there are so few strip rows that one would
/// outgrow the hero, and never under [`GLANCE_HERO_MIN`]. No strips, all hero.
#[must_use]
pub(crate) fn glance_hero_height(body_height: u16, strip_rows: u16) -> u16 {
    if strip_rows == 0 {
        return body_height;
    }
    two_fifths(body_height)
        .max(body_height.div_ceil(strip_rows.saturating_add(1)))
        .max(GLANCE_HERO_MIN)
        .min(body_height)
}

/// Columns in the strip grid: two on wide bodies, or when one strip per
/// row would squeeze each below [`GLANCE_STRIP_COMFORT`] rows.
fn glance_columns(width: u16, avail: u16, strips: usize) -> usize {
    if strips < 2 || width < GLANCE_PAIR_MIN_WIDTH {
        1
    } else if width >= GLANCE_GRID_MIN_WIDTH
        || usize::from(avail) < strips * usize::from(GLANCE_STRIP_COMFORT)
    {
        2
    } else {
        1
    }
}

fn glance_plan(body: Rect, footer: Rect, flags: LayoutFlags) -> LayoutPlan {
    let mut planned = empty_plan(Surface::Glance, footer);
    let strips: Vec<Panel> = GLANCE_STRIPS
        .into_iter()
        .filter(|p| flags.visible(*p))
        .collect();
    let side = body.width >= GLANCE_GRID_MIN_WIDTH && body.height < GLANCE_SIDE_MAX_HEIGHT;
    // u32: the product overflows u16 on absurdly wide hosts
    let side_hero_w = u16::try_from(u32::from(body.width) * 9 / 20).unwrap_or(u16::MAX);
    let (cols, room) = if side {
        let cols = if body.width - side_hero_w >= GLANCE_PAIR_MIN_WIDTH {
            2
        } else {
            1
        };
        (cols, body.height)
    } else {
        let avail = body.height - two_fifths(body.height);
        let cols = glance_columns(body.width, avail, strips.len());
        (cols, body.height.saturating_sub(GLANCE_HERO_MIN))
    };
    // strips that cannot keep their minimum height drop from the end
    let rows = usize::from(room / GLANCE_STRIP_MIN).min(strips.len().div_ceil(cols));
    let strips = &strips[..strips.len().min(rows * cols)];
    if strips.is_empty() {
        planned.cpu = Some(body);
        return planned;
    }
    let rows = u16::try_from(rows).unwrap_or(u16::MAX);
    let [hero, rest] = if side {
        cut(body, &[side_hero_w, body.width - side_hero_w], Axis::X)
    } else {
        let fit = body.height - rows * GLANCE_STRIP_MIN;
        let hero_h = glance_hero_height(body.height, rows).min(fit);
        cut(body, &[hero_h, body.height - hero_h], Axis::Y)
    };
    planned.cpu = Some(hero);
    grid(&mut planned, rest, strips, cols);
    planned
}

/// Lays `panels` over `area` row by row, `cols` to a row; rows share the
/// height evenly and a lone last panel spans the full width.
fn grid(planned: &mut LayoutPlan, area: Rect, panels: &[Panel], cols: usize) {
    let rows: Vec<&[Panel]> = panels.chunks(cols.max(1)).collect();
    let heights = share_out(area.height, &vec![Share::even(); rows.len()]);
    for (row, rect) in rows.into_iter().zip(cut_n(area, &heights, Axis::Y)) {
        assign_row(planned, rect, row.to_vec());
    }
}

/// One slice of a line of cells: its weight in the share-out and the size
/// it keeps whenever the total can pay every slice's minimum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Share {
    weight: u16,
    min: u16,
}

impl Share {
    const fn even() -> Self {
        Self { weight: 1, min: 0 }
    }
}

/// Splits `total` cells across `parts` by weight. When every minimum fits,
/// parts that would fall short are lifted to their minimum and the rest is
/// shared again; leftover cells go to the largest remainders (earliest
/// first), so equal weights differ by at most one and the sizes sum to `total`.
fn share_out(total: u16, parts: &[Share]) -> Vec<u16> {
    let mut sizes = vec![0_u16; parts.len()];
    let mut fixed = vec![false; parts.len()];
    let mins_fit = parts.iter().map(|p| u32::from(p.min)).sum::<u32>() <= u32::from(total);
    loop {
        let spent: u32 = (0..parts.len())
            .filter(|i| fixed[*i])
            .map(|i| u32::from(sizes[i]))
            .sum();
        let free = u32::from(total).saturating_sub(spent);
        let open: Vec<usize> = (0..parts.len()).filter(|i| !fixed[*i]).collect();
        let weight: u32 = open
            .iter()
            .map(|i| u32::from(parts[*i].weight.max(1)))
            .sum();
        if weight == 0 {
            return sizes;
        }
        let exact = |i: usize| free * u32::from(parts[i].weight.max(1));
        let short: Vec<usize> = open
            .iter()
            .copied()
            .filter(|i| mins_fit && exact(*i) / weight < u32::from(parts[*i].min))
            .collect();
        if short.is_empty() {
            let mut given = 0;
            for &i in &open {
                sizes[i] = u16::try_from(exact(i) / weight).unwrap_or(u16::MAX);
                given += u32::from(sizes[i]);
            }
            let mut order = open.clone();
            order.sort_by_key(|i| std::cmp::Reverse(exact(*i) % weight));
            for i in order.into_iter().take((free - given) as usize) {
                sizes[i] += 1;
            }
            return sizes;
        }
        for i in short {
            sizes[i] = parts[i].min;
            fixed[i] = true;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    X,
    Y,
}

/// Cuts `area` into consecutive slices of `sizes` cells along `axis`;
/// slices past the edge come back empty rather than spilling out.
fn cut_n(area: Rect, sizes: &[u16], axis: Axis) -> Vec<Rect> {
    let (start, span) = match axis {
        Axis::X => (area.x, area.width),
        Axis::Y => (area.y, area.height),
    };
    let end = start.saturating_add(span);
    let mut at = start;
    sizes
        .iter()
        .map(|size| {
            let from = at.min(end);
            at = at.saturating_add(*size);
            let len = at.min(end) - from;
            match axis {
                Axis::X => Rect {
                    x: from,
                    width: len,
                    ..area
                },
                Axis::Y => Rect {
                    y: from,
                    height: len,
                    ..area
                },
            }
        })
        .collect()
}

fn cut<const N: usize>(area: Rect, sizes: &[u16; N], axis: Axis) -> [Rect; N] {
    let mut out = [Rect::default(); N];
    for (slot, rect) in out.iter_mut().zip(cut_n(area, sizes, axis)) {
        *slot = rect;
    }
    out
}

/// Work's left rail: the cpu/gpu hero row leads, the mem/heat, net/disk and
/// power rows keep five rows each whenever the rail is tall enough to pay
/// for it.
const WORK_HERO: Share = Share { weight: 5, min: 6 };
const WORK_ROW: Share = Share { weight: 3, min: 5 };
/// Three boxes share one rail row only when each keeps about thirty columns,
/// enough for a title and its headline number.
const WORK_THREE_UP_MIN_WIDTH: u16 = 90;

fn work_plan(body: Rect, footer: Rect, flags: LayoutFlags) -> LayoutPlan {
    let ratio = flags.proc_ratio.clamp(PROC_RATIO_MIN, PROC_RATIO_MAX);
    // u32: `width * ratio` overflows u16 past ~910 columns
    let proc_w = (u32::from(body.width) * u32::from(ratio) / 100) as u16;
    // keeps the clamp range from inverting below the gate
    let proc_w = proc_w.clamp(24, body.width.saturating_sub(36).max(24));
    let cols = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(proc_w),
    ])
    .split(body);
    let left = cols[0];
    let split = cols[1];
    let right = cols[2];

    let rows = work_rows(flags, left);
    let shares: Vec<Share> = rows.iter().map(|(share, _)| *share).collect();
    let heights = share_out(left.height, &shares);

    let mut planned = empty_plan(Surface::Work, footer);
    for ((_, panels), rect) in rows.into_iter().zip(cut_n(left, &heights, Axis::Y)) {
        assign_row(&mut planned, rect, panels);
    }
    planned.split = Some(split);
    planned.processes = Some(right);
    planned
}

/// The visible panels of one row, in order.
fn shown(flags: LayoutFlags, row: &[Panel]) -> Vec<Panel> {
    row.iter().copied().filter(|p| flags.visible(*p)).collect()
}

fn hero_panels(flags: LayoutFlags) -> Vec<Panel> {
    shown(flags, &[Panel::Cpu, Panel::Gpu])
}

fn mid_panels(flags: LayoutFlags) -> Vec<Panel> {
    shown(flags, &[Panel::Mem, Panel::Fans])
}

/// Glance's io strip.
fn io_panels(flags: LayoutFlags) -> Vec<Panel> {
    shown(flags, &[Panel::Net, Panel::Disk])
}

/// Work's left-rail rows with their height shares. The power box takes the
/// io row's free slot beside net. When net and disk fill that row it gets a
/// row of its own, but only while `rail` still pays every row its minimum:
/// a shorter rail sets it third in the io row when that row has room for
/// three titles, and otherwise leaves it to Glance and its expanded view.
fn work_rows(flags: LayoutFlags, rail: Rect) -> Vec<(Share, Vec<Panel>)> {
    let mut io = io_panels(flags);
    let mut power = shown(flags, &[Panel::Power]);
    if io.len() < 2 {
        io.append(&mut power);
    }
    let mut rows: Vec<(Share, Vec<Panel>)> = [
        (WORK_HERO, hero_panels(flags)),
        (WORK_ROW, mid_panels(flags)),
        (WORK_ROW, io),
    ]
    .into_iter()
    .filter(|(_, panels)| !panels.is_empty())
    .collect();
    if power.is_empty() {
        return rows;
    }
    let mins: u16 = rows.iter().map(|(share, _)| share.min).sum();
    if rail.height >= mins.saturating_add(WORK_ROW.min) {
        rows.push((WORK_ROW, power));
    } else if rail.width >= WORK_THREE_UP_MIN_WIDTH
        && let Some((_, last)) = rows.last_mut()
    {
        last.append(&mut power);
    }
    rows
}

fn assign_row(planned: &mut LayoutPlan, area: Rect, panels: Vec<Panel>) {
    if panels.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }
    let constraints: Vec<Constraint> = panels.iter().map(|_| Constraint::Fill(1)).collect();
    let cols = Layout::horizontal(constraints).split(area);
    for (panel, rect) in panels.into_iter().zip(cols.iter().copied()) {
        set_panel(planned, panel, rect);
    }
}

fn set_panel(planned: &mut LayoutPlan, panel: Panel, rect: Rect) {
    match panel {
        Panel::Cpu => planned.cpu = Some(rect),
        Panel::Gpu => planned.gpu = Some(rect),
        Panel::Mem => planned.mem = Some(rect),
        Panel::Net => planned.net = Some(rect),
        Panel::Disk => planned.disk = Some(rect),
        Panel::Fans => planned.fans = Some(rect),
        Panel::Power => planned.power = Some(rect),
        Panel::Processes => planned.processes = Some(rect),
    }
}

#[must_use]
pub fn hit_test(
    area: Rect,
    surface: Surface,
    flags: LayoutFlags,
    col: u16,
    row: u16,
) -> Option<Hit> {
    let planned = plan(area, surface, flags);
    if row == planned.footer.y {
        return None;
    }
    if let Some(close) = planned.close_hit()
        && contains(close, col, row)
    {
        return Some(Hit::ExpandClose);
    }
    if planned.expanded.is_none() {
        for panel in Panel::ALL {
            if let Some(corner) = planned.expand_hit(panel)
                && contains(corner, col, row)
            {
                return Some(Hit::Expand(panel));
            }
        }
    }
    if let Some(split) = planned.split
        && contains(split, col, row)
    {
        return Some(Hit::Split);
    }
    for panel in Panel::ALL {
        if let Some(rect) = planned.panel(panel)
            && contains(rect, col, row)
        {
            return Some(Hit::Panel(panel));
        }
    }
    None
}

/// Is the cell at `col`,`row` inside `rect`? The one hit-test primitive.
pub(crate) fn contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn work_too_small_names_the_real_gate() {
        let gate = format!("{WORK_MIN_WIDTH}×{WORK_MIN_HEIGHT}");
        assert!(WORK_TOO_SMALL.ends_with(&gate), "{WORK_TOO_SMALL}");
        let at = Rect::new(0, 0, WORK_MIN_WIDTH, WORK_MIN_HEIGHT);
        assert!(work_fits(at));
        assert!(!work_fits(Rect {
            width: WORK_MIN_WIDTH - 1,
            ..at
        }));
        assert!(!work_fits(Rect {
            height: WORK_MIN_HEIGHT - 1,
            ..at
        }));
    }

    #[test]
    fn contains_is_half_open_on_both_axes() {
        let r = Rect::new(10, 5, 4, 3);
        assert!(contains(r, 10, 5));
        assert!(contains(r, 13, 7));
        assert!(!contains(r, 14, 5), "right edge is outside");
        assert!(!contains(r, 10, 8), "bottom edge is outside");
        assert!(!contains(r, 9, 5));
        assert!(!contains(r, 10, 4));
    }

    #[test]
    fn contains_saturates_at_the_far_edge() {
        let r = Rect::new(u16::MAX - 2, u16::MAX - 2, 10, 10);
        assert!(contains(r, u16::MAX - 1, u16::MAX - 1));
        assert!(!contains(r, 0, 0), "no wrap to the origin");
    }

    #[test]
    fn contains_empty_rect_holds_nothing() {
        let r = Rect::new(3, 3, 0, 5);
        assert!(!contains(r, 3, 3));
        assert!(!contains(Rect::new(3, 3, 5, 0), 3, 3));
    }

    fn flags() -> LayoutFlags {
        LayoutFlags {
            show_gpu: true,
            show_net: true,
            show_disk: true,
            show_fans: true,
            show_power: true,
            has_gpu: true,
            has_fans: true,
            has_disk: true,
            has_power: true,
            expanded: None,
            proc_ratio: 55,
            no_footer: false,
        }
    }

    #[test]
    fn expanded_still_fills_the_body() {
        let mut fs = flags();
        fs.expanded = Some(Panel::Cpu);
        let planned = plan(Rect::new(0, 0, 80, 24), Surface::Work, fs);
        let cpu = planned.cpu.unwrap_or_default();
        assert_eq!(cpu.width, 80);
        assert_eq!(cpu.height, 23, "only the footer takes a row");
    }

    #[test]
    fn work_separates_metric_boxes() {
        let planned = plan(Rect::new(0, 0, 140, 40), Surface::Work, flags());
        assert_eq!(planned.surface, Surface::Work);
        let cpu = planned.cpu.unwrap_or_default();
        let proc = planned.processes.unwrap_or_default();
        let gpu = planned.gpu.unwrap_or_default();
        let mem = planned.mem.unwrap_or_default();
        let fans = planned.fans.unwrap_or_default();
        let net = planned.net.unwrap_or_default();
        let disk = planned.disk.unwrap_or_default();
        assert!(cpu.height >= 8, "cpu {cpu:?}");
        assert_eq!(cpu.y, gpu.y, "cpu and gpu are equal hero tiles");
        assert_eq!(cpu.height, gpu.height);
        assert!(cpu.x < gpu.x);
        assert!(cpu.width + proc.width < 140);
        assert!(
            proc.height >= 20,
            "proc should be full left-column height {proc:?}"
        );
        assert!(proc.x > cpu.x + cpu.width, "proc on the right");
        assert!(planned.split.is_some());
        assert!(mem.height >= 5 && fans.height >= 5);
        assert_eq!(mem.y, fans.y);
        assert!(mem.x < fans.x);
        assert_eq!(net.y, disk.y);
        assert!(net.x < disk.x);
        assert!(cpu.y + cpu.height <= mem.y);
        assert!(mem.y + mem.height <= net.y);
        assert!(cpu.height > mem.height, "heroes taller than mem/sens");
        assert_eq!(proc.y, cpu.y);
        assert_eq!(planned.footer.height, 1);
        assert!(planned.expanded.is_none());
    }

    #[test]
    fn midsize_work_keeps_boxes() {
        let planned = plan(Rect::new(0, 0, 80, 24), Surface::Work, flags());
        assert_eq!(planned.surface, Surface::Work);
        assert!(planned.cpu.is_some());
        assert!(planned.gpu.is_some());
        assert!(planned.mem.is_some());
        assert!(planned.fans.is_some());
        assert!(planned.net.is_some());
        assert!(planned.disk.is_some());
        assert!(planned.processes.is_some());
        let cpu = planned.cpu.unwrap_or_default();
        let proc = planned.processes.unwrap_or_default();
        let gpu = planned.gpu.unwrap_or_default();
        assert!(cpu.height >= 6);
        assert!(proc.height >= 5);
        assert_eq!(cpu.height, gpu.height);
        assert!(cpu.height + 2 < 24, "cpu must not eat the screen");
    }

    #[test]
    fn hidden_flags_drop_boxes() {
        let mut flags = flags();
        flags.show_gpu = false;
        flags.show_fans = false;
        flags.show_disk = false;
        let planned = plan(Rect::new(0, 0, 120, 30), Surface::Work, flags);
        assert!(planned.gpu.is_none());
        assert!(planned.fans.is_none());
        assert!(planned.disk.is_none());
        assert!(planned.mem.is_some());
        assert!(planned.net.is_some());
    }

    #[test]
    fn removed_top_bar_gives_row_zero_back_to_the_graphs() {
        for height in 2..=60_u16 {
            for surface in [Surface::Work, Surface::Glance] {
                let planned = plan(Rect::new(0, 0, 100, height), surface, flags());
                let top = Panel::ALL
                    .into_iter()
                    .filter_map(|p| planned.panel(p))
                    .map(|r| r.y)
                    .min();
                assert_eq!(top, Some(0), "{height} {surface:?}");
                assert_eq!(planned.footer.y, height - 1, "{height}");
            }
        }
    }

    #[test]
    fn row_zero_is_a_panel_hit_now_the_bar_is_gone() {
        let area = Rect::new(0, 0, 120, 40);
        assert_eq!(
            hit_test(area, Surface::Work, flags(), 10, 0),
            Some(Hit::Panel(Panel::Cpu))
        );
    }

    #[test]
    fn work_minimum_keeps_a_tall_table() {
        let planned = plan(Rect::new(0, 0, 60, 16), Surface::Work, flags());
        assert_eq!(planned.surface, Surface::Work);
        assert!(planned.processes.is_some_and(|p| p.height >= 15));
    }

    #[test]
    fn corner_mark_sits_one_in_from_the_right_corner_for_every_width() {
        for width in 0..=200_u16 {
            let area = Rect::new(7, 3, width, 4);
            match LayoutPlan::corner_hit(area) {
                None => assert!(width < CORNER_MIN_WIDTH, "{width}"),
                Some(r) => {
                    assert_eq!(r.width, CORNER_W);
                    assert_eq!(r.y, 3);
                    assert_eq!(
                        r.x + r.width,
                        area.x + width - 1,
                        "{width}: right corner free"
                    );
                    assert!(r.x > area.x, "{width}: left corner free");
                }
            }
        }
        assert_eq!(unicode_width::UnicodeWidthStr::width(CORNER_CLOSE), 3);
        assert_eq!(unicode_width::UnicodeWidthStr::width(CORNER_EXPAND), 3);
    }

    #[test]
    fn expand_fills_body() {
        let mut flags = flags();
        flags.expanded = Some(Panel::Cpu);
        let planned = plan(Rect::new(0, 0, 80, 24), Surface::Work, flags);
        let cpu = planned.cpu.unwrap_or_default();
        assert_eq!(cpu, Rect::new(0, 0, 80, 23));
        assert!(planned.gpu.is_none());
        assert!(planned.processes.is_none());
        assert_eq!(planned.expanded, Some(Panel::Cpu));
        assert!(planned.close_hit().is_some());
    }

    #[test]
    fn glance_has_no_process_pane() {
        let planned = plan(Rect::new(0, 0, 80, 20), Surface::Glance, flags());
        assert_eq!(planned.surface, Surface::Glance);
        assert!(planned.processes.is_none());
        assert!(planned.cpu.is_some());
        assert_eq!(planned.footer.height, 1);
    }

    /// The body a plan lays panels in: everything above the footer.
    fn body_of(planned: &LayoutPlan, area: Rect) -> Rect {
        Rect::new(area.x, area.y, area.width, planned.footer.y - area.y)
    }

    fn strips_of(planned: &LayoutPlan) -> Vec<(Panel, Rect)> {
        GLANCE_STRIPS
            .into_iter()
            .filter_map(|p| planned.panel(p).map(|r| (p, r)))
            .collect()
    }

    fn glance(width: u16, height: u16, fs: LayoutFlags) -> (LayoutPlan, Rect) {
        let area = Rect::new(0, 0, width, height);
        let planned = plan(area, Surface::Glance, fs);
        let body = body_of(&planned, area);
        (planned, body)
    }

    #[test]
    fn glance_hero_height_is_two_fifths_with_a_floor() {
        for (body, rows, want) in [
            (0, 3, 0),
            (3, 3, 3),
            (5, 3, 5),
            (8, 1, 5),
            (12, 3, 5),
            (15, 3, 6),
            (38, 3, 15),
            (43, 3, 17),
            (43, 5, 17),
            (68, 3, 27),
            (88, 3, 35),
            (43, 1, 22),
            (43, 0, 43),
            (u16::MAX, 3, 26214),
        ] {
            assert_eq!(glance_hero_height(body, rows), want, "{body}/{rows}");
        }
    }

    #[test]
    fn glance_lone_strip_never_outgrows_the_hero() {
        // Every optional strip off leaves just mem; it must not dwarf the cpu.
        for height in 12..=90_u16 {
            let (planned, body) = glance(120, height, combo(0));
            let cpu = planned.cpu.unwrap();
            let mem = planned.mem.unwrap();
            assert!(cpu.height >= mem.height, "{height}: {cpu:?} {mem:?}");
            assert_eq!(cpu.height + mem.height, body.height, "{height}");
        }
    }

    #[test]
    fn regression_glance_cpu_hero_crushed_every_other_panel() {
        // Owner feedback 2026-09-23: at 160x45 the cpu hero took 30 rows and
        // gpu/mem/net/heat shrank to slivers. The hero now caps at two fifths.
        let (planned, body) = glance(160, 45, flags());
        let cpu = planned.cpu.unwrap();
        assert!(cpu.height * 100 <= body.height * 45, "{cpu:?} of {body:?}");
        for (panel, rect) in strips_of(&planned) {
            assert!(rect.height >= 7, "{panel:?} {rect:?}");
        }
    }

    #[test]
    fn glance_hero_never_passes_45_percent_from_30_rows() {
        for width in (40..=300_u16).step_by(7) {
            for height in (30..=90_u16).step_by(2) {
                let (planned, body) = glance(width, height, flags());
                let cpu = planned.cpu.unwrap();
                assert!(
                    u32::from(cpu.height) * 100 <= u32::from(body.height) * 45,
                    "{width}x{height}: {cpu:?} of {body:?}"
                );
            }
        }
    }

    #[test]
    fn glance_strips_share_height_within_one_row() {
        for width in (40..=300_u16).step_by(9) {
            for height in (12..=90_u16).step_by(3) {
                let (planned, _) = glance(width, height, flags());
                let heights: Vec<u16> = strips_of(&planned).iter().map(|(_, r)| r.height).collect();
                if let (Some(lo), Some(hi)) = (heights.iter().min(), heights.iter().max()) {
                    assert!(hi - lo <= 1, "{width}x{height}: {heights:?}");
                }
            }
        }
    }

    #[test]
    fn glance_no_panel_below_its_minimum() {
        for width in (40..=300_u16).step_by(5) {
            for height in (12..=90_u16).step_by(2) {
                let (planned, _) = glance(width, height, flags());
                let strips = strips_of(&planned);
                let cpu = planned.cpu.unwrap();
                if !strips.is_empty() {
                    assert!(cpu.height >= GLANCE_HERO_MIN, "{width}x{height} {cpu:?}");
                }
                for (panel, rect) in strips {
                    assert!(
                        rect.height >= GLANCE_STRIP_MIN,
                        "{width}x{height} {panel:?}"
                    );
                    assert!(rect.width >= 20, "{width}x{height} {panel:?} {rect:?}");
                }
            }
        }
    }

    /// The fixture with the power strip switched off: the five-strip Glance.
    fn no_power() -> LayoutFlags {
        LayoutFlags {
            show_power: false,
            ..flags()
        }
    }

    #[test]
    fn glance_is_complete_from_80x20() {
        for (fs, want) in [(flags(), 6), (no_power(), 5)] {
            for width in (80..=300_u16).step_by(11) {
                for height in (20..=90_u16).step_by(5) {
                    let (planned, _) = glance(width, height, fs);
                    assert_eq!(
                        strips_of(&planned).len(),
                        want,
                        "{width}x{height} lost a strip"
                    );
                }
            }
        }
    }

    #[test]
    fn glance_panels_tile_the_body_without_gaps() {
        for (width, height) in [
            (80, 20),
            (120, 40),
            (160, 45),
            (70, 60),
            (220, 20),
            (300, 90),
        ] {
            let (planned, body) = glance(width, height, flags());
            let mut cells = planned.cpu.unwrap().area();
            cells += strips_of(&planned)
                .iter()
                .map(|(_, r)| r.area())
                .sum::<u32>();
            assert_eq!(cells, body.area(), "{width}x{height}");
        }
    }

    #[test]
    fn glance_wide_lays_strips_in_a_two_column_grid() {
        // With power: a clean two-by-three grid, heat and power closing it.
        let (planned, _) = glance(160, 45, flags());
        let [gpu, mem, net, disk, fans, power] = [
            Panel::Gpu,
            Panel::Mem,
            Panel::Net,
            Panel::Disk,
            Panel::Fans,
            Panel::Power,
        ]
        .map(|p| planned.panel(p).unwrap());
        let cpu = planned.cpu.unwrap();
        assert_eq!((cpu.y, cpu.width), (0, 160), "hero spans the top");
        assert!(cpu.bottom() <= gpu.y);
        assert_eq!(
            (gpu.y, net.y, fans.y),
            (mem.y, disk.y, power.y),
            "pairs share a row"
        );
        assert!(gpu.x < mem.x && net.x < disk.x && fans.x < power.x);
        assert!(gpu.y < net.y && net.y < fans.y);
        for rect in [gpu, mem, net, disk, fans, power] {
            assert_eq!(rect.width, 80, "{rect:?} is half the width");
            assert!(rect.height >= 8, "{rect:?}");
        }
        assert_eq!(
            (gpu.x, net.x, fans.x),
            (0, 0, 0),
            "the columns line up down the grid"
        );

        // Without power heat is the odd one out and spans the width.
        let (planned, _) = glance(160, 45, no_power());
        let [gpu, mem, net, disk, fans] =
            [Panel::Gpu, Panel::Mem, Panel::Net, Panel::Disk, Panel::Fans]
                .map(|p| planned.panel(p).unwrap());
        assert_eq!(planned.cpu.unwrap().width, 160, "hero spans the top");
        assert_eq!((gpu.y, net.y), (mem.y, disk.y), "pairs share a row");
        assert!(gpu.x < mem.x && net.x < disk.x);
        assert!(gpu.y < net.y && net.y < fans.y);
        assert_eq!(fans.width, 160, "the lone last strip spans the width");
        assert_eq!(planned.power, None);
    }

    #[test]
    fn glance_even_strip_count_fills_every_grid_cell() {
        let mut four = no_power();
        four.has_gpu = false;
        for (fs, want) in [(four, 4), (flags(), 6)] {
            let (planned, _) = glance(200, 60, fs);
            let strips = strips_of(&planned);
            assert_eq!(strips.len(), want);
            for (panel, rect) in strips {
                assert!(rect.width < 110, "{panel:?} should share its row {rect:?}");
            }
        }
    }

    #[test]
    fn glance_tall_narrow_stacks_one_strip_per_row() {
        for (fs, want) in [(flags(), 6), (no_power(), 5)] {
            let (planned, _) = glance(70, 60, fs);
            let strips = strips_of(&planned);
            assert_eq!(strips.len(), want);
            for pair in strips.windows(2) {
                assert!(pair[0].1.bottom() <= pair[1].1.y, "{pair:?}");
            }
            for (panel, rect) in strips {
                assert_eq!(rect.width, 70, "{panel:?}");
                assert!(rect.height >= 6, "{panel:?} {rect:?}");
            }
        }
    }

    #[test]
    fn glance_short_wide_sets_hero_beside_the_strips() {
        let (planned, body) = glance(220, 20, flags());
        let cpu = planned.cpu.unwrap();
        assert_eq!(
            (cpu.y, cpu.height),
            (body.y, body.height),
            "hero is full height"
        );
        assert!(cpu.width < 110, "{cpu:?}");
        for (panel, rect) in strips_of(&planned) {
            assert!(rect.x >= cpu.right(), "{panel:?} beside the hero");
            assert!(rect.height >= 5, "{panel:?} {rect:?}");
        }
    }

    #[test]
    fn glance_side_by_side_only_on_short_wide_bodies() {
        for width in (40..=300_u16).step_by(13) {
            for height in (12..=90_u16).step_by(3) {
                let (planned, body) = glance(width, height, flags());
                let cpu = planned.cpu.unwrap();
                let side = cpu.width < body.width;
                let want = width >= GLANCE_GRID_MIN_WIDTH && body.height < GLANCE_SIDE_MAX_HEIGHT;
                assert_eq!(side, want, "{width}x{height} {cpu:?}");
            }
        }
    }

    #[test]
    fn glance_mid_width_pairs_strips_rather_than_squeeze_them() {
        // 120x40: one strip per row would leave each four rows; pairs get seven.
        let (planned, _) = glance(120, 40, flags());
        let gpu = planned.gpu.unwrap();
        let mem = planned.mem.unwrap();
        assert_eq!(gpu.y, mem.y);
        assert!(gpu.height >= 7, "{gpu:?}");
    }

    #[test]
    fn glance_strips_read_in_order() {
        for (width, height) in [(80, 24), (120, 40), (70, 60), (220, 20), (250, 70)] {
            let (planned, _) = glance(width, height, flags());
            let strips = strips_of(&planned);
            for pair in strips.windows(2) {
                let (a, b) = (pair[0].1, pair[1].1);
                assert!((a.y, a.x) < (b.y, b.x), "{width}x{height} {pair:?}");
            }
        }
    }

    #[test]
    fn glance_strips_grow_with_the_terminal() {
        let mut last = 0;
        for height in 30..=90_u16 {
            let (planned, _) = glance(160, height, flags());
            let fans = planned.fans.unwrap().height;
            assert!(fans + 1 >= last, "{height}: {fans} < {last}");
            last = fans;
        }
        assert!(last >= 15, "tall screens give strips real graphs: {last}");
    }

    #[test]
    fn glance_tiny_body_is_all_hero() {
        for height in 1..=9_u16 {
            let (planned, body) = glance(80, height, flags());
            if body.height < GLANCE_HERO_MIN + GLANCE_STRIP_MIN {
                assert_eq!(planned.cpu, Some(body), "{height}");
                assert!(strips_of(&planned).is_empty(), "{height}");
            }
        }
    }

    #[test]
    fn glance_short_bodies_drop_strips_from_the_end() {
        let (planned, _) = glance(50, 14, flags());
        let shown: Vec<Panel> = strips_of(&planned).into_iter().map(|(p, _)| p).collect();
        assert_eq!(shown, GLANCE_STRIPS[..shown.len()].to_vec());
        assert!(shown.len() < 5 && !shown.is_empty(), "{shown:?}");
    }

    #[test]
    fn glance_short_wide_hero_spans_its_side_without_strips_beside_it() {
        // Short and too narrow for side-by-side: stacked, hero at least the floor.
        let (planned, body) = glance(100, 12, flags());
        let cpu = planned.cpu.unwrap();
        assert_eq!(cpu.width, body.width);
        assert!(cpu.height >= GLANCE_HERO_MIN, "{cpu:?}");
    }

    #[test]
    fn glance_hit_test_agrees_with_rects_cell_by_cell() {
        for (width, height) in [(80, 20), (160, 45), (70, 60), (220, 20)] {
            let area = Rect::new(0, 0, width, height);
            let planned = plan(area, Surface::Glance, flags());
            let body = body_of(&planned, area);
            for row in body.y..body.bottom() {
                for col in body.x..body.right() {
                    let owner = Panel::ALL
                        .into_iter()
                        .find(|p| planned.panel(*p).is_some_and(|r| contains(r, col, row)));
                    let want = owner.map(|p| {
                        if planned.expand_hit(p).is_some_and(|c| contains(c, col, row)) {
                            Hit::Expand(p)
                        } else {
                            Hit::Panel(p)
                        }
                    });
                    let hit = hit_test(area, Surface::Glance, flags(), col, row);
                    assert_eq!(hit, want, "{width}x{height} at {col},{row}");
                }
            }
        }
    }

    #[test]
    fn glance_survives_enormous_terminals() {
        for (fs, want) in [(flags(), 6), (no_power(), 5)] {
            for (width, height) in [(4000_u16, 2000_u16), (u16::MAX, 300), (300, u16::MAX)] {
                let (planned, body) = glance(width, height, fs);
                let cpu = planned.cpu.unwrap();
                assert!(cpu.bottom() <= body.bottom() && cpu.right() <= body.right());
                assert_eq!(strips_of(&planned).len(), want, "{width}x{height}");
            }
        }
    }

    #[test]
    fn share_out_always_sums_to_the_total() {
        let shapes: [&[Share]; 5] = [
            &[Share::even()],
            &[Share::even(); 5],
            &[WORK_HERO, WORK_ROW, WORK_ROW],
            &[WORK_HERO, WORK_ROW],
            &[Share { weight: 0, min: 2 }, Share { weight: 7, min: 0 }],
        ];
        for parts in shapes {
            for total in 0..=300_u16 {
                let sizes = share_out(total, parts);
                assert_eq!(sizes.len(), parts.len());
                let sum: u32 = sizes.iter().map(|s| u32::from(*s)).sum();
                assert_eq!(sum, u32::from(total), "{total} {parts:?} -> {sizes:?}");
            }
        }
        assert!(share_out(40, &[]).is_empty());
    }

    #[test]
    fn share_out_even_parts_differ_by_at_most_one() {
        for n in 1..=6 {
            for total in 0..=120_u16 {
                let sizes = share_out(total, &vec![Share::even(); n]);
                let lo = sizes.iter().min().unwrap();
                let hi = sizes.iter().max().unwrap();
                assert!(hi - lo <= 1, "{total}/{n}: {sizes:?}");
            }
        }
    }

    #[test]
    fn share_out_lifts_minimums_only_when_all_fit() {
        let rail = [WORK_HERO, WORK_ROW, WORK_ROW];
        assert_eq!(share_out(16, &rail), vec![6, 5, 5]);
        assert_eq!(share_out(20, &rail), vec![9, 6, 5]);
        // 14 cannot pay 6 + 5 + 5, so it falls back to plain weights
        assert_eq!(share_out(14, &rail), vec![6, 4, 4]);
        assert_eq!(share_out(55, &rail), vec![25, 15, 15]);
    }

    #[test]
    fn cut_n_slices_are_contiguous_and_never_spill() {
        let area = Rect::new(3, 5, 40, 10);
        let rows = cut_n(area, &[4, 4, 4], Axis::Y);
        assert_eq!(rows[0], Rect::new(3, 5, 40, 4));
        assert_eq!(rows[1].y, rows[0].bottom());
        assert_eq!(rows[2], Rect::new(3, 13, 40, 2), "clipped at the edge");
        let cols = cut_n(area, &[30, 30], Axis::X);
        assert_eq!(cols[1], Rect::new(33, 5, 10, 10));
        assert_eq!(cut_n(area, &[50, 5], Axis::X)[1].width, 0);
    }

    #[test]
    fn work_secondary_rows_keep_five_rows_when_height_allows() {
        for fs in [flags(), no_power()] {
            for width in (60..=300_u16).step_by(10) {
                for height in 16..=90_u16 {
                    let area = Rect::new(0, 0, width, height);
                    let planned = plan(area, Surface::Work, fs);
                    let body = body_of(&planned, area);
                    let cpu = planned.cpu.unwrap();
                    for panel in [
                        Panel::Mem,
                        Panel::Fans,
                        Panel::Net,
                        Panel::Disk,
                        Panel::Power,
                    ] {
                        let Some(rect) = planned.panel(panel) else {
                            // only power ever steps aside for the others
                            assert_eq!(panel, Panel::Power, "{width}x{height} lost {panel:?}");
                            continue;
                        };
                        if body.height >= 16 {
                            assert!(rect.height >= 5, "{width}x{height} {panel:?} {rect:?}");
                        }
                        assert!(cpu.height >= rect.height, "{width}x{height} hero leads");
                    }
                }
            }
        }
    }

    /// Rows a rail needs to pay the hero and three five-row rows.
    const FOUR_ROW_RAIL: u16 = WORK_HERO.min + 3 * WORK_ROW.min;

    #[test]
    fn work_rail_rows_tile_the_rail() {
        for fs in [flags(), no_power()] {
            for height in 16..=90_u16 {
                let area = Rect::new(0, 0, 160, height);
                let planned = plan(area, Surface::Work, fs);
                let body = body_of(&planned, area);
                let cpu = planned.cpu.unwrap();
                let mem = planned.mem.unwrap();
                let net = planned.net.unwrap();
                assert_eq!(cpu.y, body.y, "{height}");
                assert_eq!(mem.y, cpu.bottom(), "{height}");
                assert_eq!(net.y, mem.bottom(), "{height}");
                // a 71-column rail is too narrow for three-up: power has its
                // own last row whenever the rail pays for four
                assert_eq!(
                    planned.power.is_some(),
                    fs.show_power && body.height >= FOUR_ROW_RAIL,
                    "{height}"
                );
                if let Some(power) = planned.power {
                    assert_eq!(power.y, net.bottom(), "{height}");
                }
                let last = planned.power.unwrap_or(net);
                assert_eq!(last.bottom(), body.bottom(), "{height}");
            }
        }
    }

    #[test]
    fn work_power_takes_a_fourth_row_only_when_the_rail_pays_for_it() {
        for width in (60..=300_u16).step_by(5) {
            for height in 16..=60_u16 {
                let area = Rect::new(0, 0, width, height);
                let planned = plan(area, Surface::Work, flags());
                let body = body_of(&planned, area);
                let rail = planned.split.unwrap().x - area.x;
                let (net, disk) = (planned.net.unwrap(), planned.disk.unwrap());
                let ctx = format!("{width}x{height} rail {rail}");
                match planned.power {
                    Some(power) if power.y == net.y => {
                        assert!(body.height < FOUR_ROW_RAIL, "{ctx}: own row fits");
                        assert!(rail >= WORK_THREE_UP_MIN_WIDTH, "{ctx}");
                        assert!(net.x < disk.x && disk.x < power.x, "{ctx}");
                        for r in [net, disk, power] {
                            assert!(r.width >= WORK_THREE_UP_MIN_WIDTH / 3, "{ctx} {r:?}");
                        }
                    }
                    Some(power) => {
                        assert!(body.height >= FOUR_ROW_RAIL, "{ctx}");
                        assert_eq!(
                            (power.x, power.y, power.width),
                            (net.x, net.bottom(), rail),
                            "{ctx}: a full-width last row"
                        );
                        assert_eq!(power.bottom(), body.bottom(), "{ctx}");
                    }
                    None => assert!(
                        body.height < FOUR_ROW_RAIL && rail < WORK_THREE_UP_MIN_WIDTH,
                        "{ctx}: power dropped with room to spare"
                    ),
                }
            }
        }
    }

    #[test]
    fn tiny_work_falls_back_to_glance() {
        let planned = plan(Rect::new(0, 0, 40, 8), Surface::Work, flags());
        assert_eq!(planned.surface, Surface::Glance);
        assert!(planned.processes.is_none());
    }

    #[test]
    fn work_between_gates_falls_back_instead_of_panicking() {
        // Regression: widths 40..59 used to reach work_plan and invert the
        // process-column clamp (min 24 > max width-36).
        for width in 40..60_u16 {
            for height in [16_u16, 20, 24, 31] {
                let planned = plan(Rect::new(0, 0, width, height), Surface::Work, flags());
                assert_eq!(
                    planned.surface,
                    Surface::Glance,
                    "{width}x{height} must be Glance"
                );
            }
        }
    }

    #[test]
    fn plan_and_hit_test_sweep_every_size_without_panic() {
        for width in (1..=120_u16).step_by(3) {
            for height in (1..=44_u16).step_by(3) {
                let area = Rect::new(0, 0, width, height);
                let mut fs = flags();
                let _ = plan(area, Surface::Work, fs);
                let _ = plan(area, Surface::Glance, fs);
                let _ = hit_test(area, Surface::Work, fs, width / 2, height / 2);
                for panel in Panel::ALL {
                    fs.expanded = Some(panel);
                    let planned = plan(area, Surface::Work, fs);
                    if let Some(rect) = planned.panel(panel) {
                        let _ = hit_test(area, Surface::Work, fs, rect.x, rect.y);
                    }
                }
            }
        }
    }

    #[test]
    fn click_box_hits_panel() {
        let area = Rect::new(0, 0, 120, 30);
        let planned = plan(area, Surface::Work, flags());
        let cpu = planned.cpu.unwrap_or_default();
        assert_eq!(
            hit_test(area, Surface::Work, flags(), cpu.x + 2, cpu.y + 1),
            Some(Hit::Panel(Panel::Cpu))
        );
        let corner = LayoutPlan::corner_hit(cpu).unwrap_or_default();
        assert_eq!(
            hit_test(area, Surface::Work, flags(), corner.x, corner.y),
            Some(Hit::Expand(Panel::Cpu))
        );
        let mem = planned.mem.unwrap_or_default();
        assert_eq!(
            hit_test(area, Surface::Work, flags(), mem.x + 2, mem.y + 1),
            Some(Hit::Panel(Panel::Mem))
        );
    }

    #[test]
    fn click_x_closes_expand() {
        let mut flags = flags();
        flags.expanded = Some(Panel::Mem);
        let area = Rect::new(0, 0, 80, 24);
        let planned = plan(area, Surface::Work, flags);
        let close = planned.close_hit().unwrap_or_default();
        assert_eq!(
            hit_test(area, Surface::Work, flags, close.x, close.y),
            Some(Hit::ExpandClose)
        );
    }

    #[test]
    fn visible_panels_respect_hardware() {
        let mut flags = flags();
        flags.has_fans = false;
        flags.has_gpu = false;
        // Regression: a Mac with no heat sensors kept an empty `sens —` box.
        let vis = flags.visible_panels();
        assert!(!vis.contains(&Panel::Fans));
        assert!(!vis.contains(&Panel::Gpu));
        assert!(vis.contains(&Panel::Cpu));
        assert!(vis.contains(&Panel::Processes));
    }

    const FLAG_BITS: u32 = 7;

    fn combo(bits: u32) -> LayoutFlags {
        LayoutFlags {
            show_gpu: bits & 1 != 0,
            show_net: bits & 2 != 0,
            show_disk: bits & 4 != 0,
            show_fans: bits & 8 != 0,
            has_gpu: bits & 16 != 0,
            has_fans: bits & 32 != 0,
            has_disk: bits & 64 != 0,
            show_power: bits & 8 != 0,
            has_power: bits & 32 != 0,
            expanded: None,
            proc_ratio: 55,
            no_footer: false,
        }
    }

    fn overlaps(a: Rect, b: Rect) -> bool {
        a.width > 0
            && a.height > 0
            && b.width > 0
            && b.height > 0
            && a.x < b.x + b.width
            && b.x < a.x + a.width
            && a.y < b.y + b.height
            && b.y < a.y + a.height
    }

    /// Every law a plan must keep, whatever the size or flags.
    fn check_plan(area: Rect, requested: Surface, fs: LayoutFlags) {
        check_plan_hits(area, requested, fs, true);
    }

    fn check_plan_hits(area: Rect, requested: Surface, fs: LayoutFlags, hits: bool) {
        let planned = plan(area, requested, fs);
        let ctx = format!("{area:?} {requested:?} {fs:?}");
        if requested == Surface::Work {
            assert_eq!(
                planned.surface,
                siltop_core::auto_surface(area.width, area.height),
                "layout gate disagrees with core gate: {ctx}"
            );
        } else {
            assert_eq!(planned.surface, Surface::Glance, "{ctx}");
        }
        assert_eq!(planned.footer.y, area.y + area.height - 1, "{ctx}");
        assert_eq!(planned.footer.height, 1, "{ctx}");
        assert_eq!(planned.footer.width, area.width, "{ctx}");

        let mut rects: Vec<(String, Rect)> = Panel::ALL
            .into_iter()
            .filter_map(|p| planned.panel(p).map(|r| (format!("{p:?}"), r)))
            .collect();
        if let Some(split) = planned.split {
            rects.push((String::from("split"), split));
        }
        for (name, r) in &rects {
            assert!(
                r.x >= area.x && r.right() <= area.right(),
                "{name} {r:?} {ctx}"
            );
            assert!(
                r.y >= area.y && r.bottom() <= planned.footer.y,
                "{name} {r:?} {ctx}"
            );
        }
        for (i, (an, a)) in rects.iter().enumerate() {
            for (bn, b) in rects.iter().skip(i + 1) {
                assert!(!overlaps(*a, *b), "{an} {a:?} overlaps {bn} {b:?} {ctx}");
            }
        }
        for p in Panel::ALL {
            if planned.panel(p).is_some() {
                assert!(fs.visible(p), "{p:?} placed while hidden {ctx}");
            }
        }
        match planned.surface {
            Surface::Work => {
                assert!(planned.processes.is_some(), "{ctx}");
                assert!(planned.split.is_some(), "{ctx}");
                assert!(planned.cpu.is_some() && planned.mem.is_some(), "{ctx}");
            }
            Surface::Glance => {
                assert!(planned.processes.is_none(), "{ctx}");
                assert!(planned.split.is_none(), "{ctx}");
                assert!(planned.cpu.is_some(), "{ctx}");
            }
        }
        if !hits {
            return;
        }
        // a click anywhere inside a panel names that panel (or its corner)
        for p in Panel::ALL {
            let Some(r) = planned.panel(p) else { continue };
            if r.width == 0 || r.height == 0 {
                continue;
            }
            for (col, row) in [
                (r.x + r.width / 2, r.y + r.height / 2),
                (r.x, r.bottom() - 1),
                (r.right() - 1, r.bottom() - 1),
            ] {
                let hit = hit_test(area, requested, fs, col, row);
                assert!(
                    hit == Some(Hit::Panel(p)) || hit == Some(Hit::Expand(p)),
                    "{p:?} at {col},{row} hit {hit:?} {ctx}"
                );
            }
        }
        assert_eq!(hit_test(area, requested, fs, 0, planned.footer.y), None);
        assert_eq!(
            hit_test(area, requested, fs, area.right() + 3, area.y),
            None,
            "{ctx}"
        );
    }

    /// 20x8 to 300x90, step 3, split in three so the test threads share it.
    /// Each distinct size is a fresh constraint solve, so hit-test a quarter
    /// of them; the rect laws run on all.
    fn sweep_sizes(widths: std::ops::RangeInclusive<u16>) {
        let reps = [combo(0x7f), combo(2 | 4 | 64)];
        for width in widths.step_by(3) {
            for height in (8..=90_u16).step_by(3) {
                let area = Rect::new(0, 0, width, height);
                let hits = (width / 3 + height / 3) % 4 == 0;
                for fs in reps {
                    check_plan_hits(area, Surface::Work, fs, hits);
                    check_plan_hits(area, Surface::Glance, fs, hits);
                }
            }
        }
    }

    #[test]
    fn plan_laws_hold_for_every_size_narrow() {
        sweep_sizes(20..=110);
    }

    #[test]
    fn plan_laws_hold_for_every_size_mid() {
        sweep_sizes(113..=203);
    }

    #[test]
    fn plan_laws_hold_for_every_size_wide() {
        sweep_sizes(206..=300);
    }

    #[test]
    fn plan_laws_hold_for_every_flag_combination() {
        for bits in 0..(1 << FLAG_BITS) {
            let fs = combo(bits);
            for width in [20_u16, 59, 60, 97, 143, 300] {
                for height in [8_u16, 15, 16, 24, 41] {
                    let area = Rect::new(0, 0, width, height);
                    check_plan(area, Surface::Work, fs);
                    check_plan(area, Surface::Glance, fs);
                }
            }
        }
    }

    #[test]
    fn plan_laws_hold_off_origin() {
        // Hosts may hand us a sub-rect; nothing may assume x = y = 0.
        for (x, y) in [(3_u16, 2_u16), (17, 9), (100, 40)] {
            for (w, h) in [(80_u16, 24_u16), (120, 40), (59, 30), (200, 60)] {
                check_plan(Rect::new(x, y, w, h), Surface::Work, flags());
                check_plan(Rect::new(x, y, w, h), Surface::Glance, flags());
            }
        }
    }

    #[test]
    fn every_ratio_keeps_the_laws() {
        for ratio in [0_u16, 1, 34, 35, 50, 55, 72, 73, 100, u16::MAX] {
            let mut fs = flags();
            fs.proc_ratio = ratio;
            for (w, h) in [(60_u16, 16_u16), (80, 24), (143, 41), (300, 90)] {
                check_plan(Rect::new(0, 0, w, h), Surface::Work, fs);
            }
        }
    }

    #[test]
    fn proc_column_widens_with_the_ratio() {
        let area = Rect::new(0, 0, 200, 50);
        let mut last = 0;
        for ratio in PROC_RATIO_MIN..=PROC_RATIO_MAX {
            let mut fs = flags();
            fs.proc_ratio = ratio;
            let proc = plan(area, Surface::Work, fs).processes.unwrap();
            assert!(proc.width >= last, "ratio {ratio}: {} < {last}", proc.width);
            last = proc.width;
        }
    }

    #[test]
    fn work_plan_wide_terminal_does_not_overflow() {
        // Regression: `width * ratio` in u16 overflowed (a debug panic, a
        // wrong split in release) once the terminal passed ~910 columns.
        for width in [910_u16, 911, 1000, 2000, 4000] {
            let planned = plan(Rect::new(0, 0, width, 40), Surface::Work, flags());
            assert_eq!(planned.surface, Surface::Work);
            let proc = planned.processes.unwrap();
            let want = u32::from(width) * 55 / 100;
            assert!(
                u32::from(proc.width).abs_diff(want) <= 1,
                "{width}: {proc:?}"
            );
        }
    }

    #[test]
    fn expanded_panel_owns_the_whole_body_at_every_size() {
        for width in (1..=300_u16).step_by(13) {
            for height in (2..=90_u16).step_by(7) {
                let area = Rect::new(0, 0, width, height);
                for panel in Panel::ALL {
                    let mut fs = flags();
                    fs.expanded = Some(panel);
                    for surface in [Surface::Work, Surface::Glance] {
                        let planned = plan(area, surface, fs);
                        assert_eq!(
                            planned.panel(panel),
                            Some(Rect::new(0, 0, width, height - 1))
                        );
                        for other in Panel::ALL.into_iter().filter(|p| *p != panel) {
                            assert_eq!(planned.panel(other), None, "{other:?} under {panel:?}");
                        }
                        assert!(planned.split.is_none());
                        assert_eq!(planned.expand_hit(panel), None, "no ↗ while expanded");
                        if let Some(close) = planned.close_hit() {
                            assert_eq!(
                                hit_test(area, surface, fs, close.x, close.y),
                                Some(Hit::ExpandClose)
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn corner_hit_is_three_cells_inside_the_top_border() {
        assert_eq!(LayoutPlan::corner_hit(Rect::new(0, 0, 4, 3)), None);
        assert_eq!(LayoutPlan::corner_hit(Rect::new(0, 0, 10, 0)), None);
        for width in 5..=200_u16 {
            let area = Rect::new(7, 3, width, 5);
            let c = LayoutPlan::corner_hit(area).unwrap();
            assert_eq!((c.width, c.height, c.y), (3, 1, 3));
            assert!(c.x > area.x, "corner must not take the left border");
            assert_eq!(c.right(), area.right() - 1, "stops before the ┐");
        }
    }

    #[test]
    fn power_gets_its_own_row_when_net_and_disk_fill_the_io_row() {
        // Regression: three boxes in one 53-column row cut every title to `…`.
        let planned = plan(Rect::new(0, 0, 120, 40), Surface::Work, flags());
        let (net, disk, power) = (
            planned.net.unwrap(),
            planned.disk.unwrap(),
            planned.power.unwrap(),
        );
        assert_eq!(net.y, disk.y);
        assert!(power.y >= net.bottom(), "{power:?} below {net:?}");
        assert_eq!(power.x, net.x);
        assert_eq!(power.width, net.width + disk.width);
    }

    #[test]
    fn power_sits_beside_net_in_work() {
        let mut f = flags();
        f.show_disk = false;
        let planned = plan(Rect::new(0, 0, 120, 40), Surface::Work, f);
        let (net, power) = (planned.net.unwrap(), planned.power.unwrap());
        assert_eq!(net.y, power.y, "same row");
        assert!(power.x > net.x);
    }

    #[test]
    fn power_shares_the_heat_strip_in_glance() {
        let planned = plan(Rect::new(0, 0, 120, 40), Surface::Glance, flags());
        let (fans, power) = (planned.fans.unwrap(), planned.power.unwrap());
        assert_eq!(fans.y, power.y, "same strip");
        assert_eq!(fans.height, power.height);
    }

    #[test]
    fn power_hidden_without_hardware_or_when_switched_off() {
        for surface in [Surface::Work, Surface::Glance] {
            let mut f = flags();
            f.has_power = false;
            assert_eq!(plan(Rect::new(0, 0, 120, 40), surface, f).power, None);
            let mut f = flags();
            f.show_power = false;
            assert_eq!(plan(Rect::new(0, 0, 120, 40), surface, f).power, None);
        }
    }

    #[test]
    fn power_alone_fills_the_heat_strip_on_fanless_sensorless_macs() {
        let mut f = flags();
        f.has_fans = false;
        let planned = plan(Rect::new(0, 0, 120, 40), Surface::Glance, f);
        let power = planned.power.unwrap();
        assert_eq!(power.width, 120, "{power:?}");
    }

    #[test]
    fn power_never_overlaps_and_hit_tests_to_itself_across_sizes() {
        for surface in [Surface::Work, Surface::Glance] {
            for w in (40..=240).step_by(13) {
                for h in (8..=70).step_by(5) {
                    let area = Rect::new(0, 0, w, h);
                    let planned = plan(area, surface, flags());
                    let Some(power) = planned.power else { continue };
                    assert!(power.right() <= w && power.bottom() <= h, "{w}x{h}");
                    for other in Panel::ALL.into_iter().filter(|p| *p != Panel::Power) {
                        if let Some(r) = planned.panel(other) {
                            assert!(!overlaps(power, r), "{w}x{h} {surface:?} {other:?}");
                        }
                    }
                    if power.width >= 3 && power.height >= 3 {
                        let (cx, cy) = (power.x + power.width / 2, power.y + power.height / 2);
                        assert_eq!(
                            hit_test(area, surface, flags(), cx, cy),
                            Some(Hit::Panel(Panel::Power)),
                            "{w}x{h} {surface:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn panel_labels_are_unique_and_short() {
        let labels: Vec<&str> = Panel::ALL.iter().map(|p| p.label()).collect();
        for (i, a) in labels.iter().enumerate() {
            assert!(a.len() <= 5 && !a.is_empty(), "{a}");
            assert!(!labels[i + 1..].contains(a), "duplicate {a}");
        }
    }

    #[test]
    fn split_is_one_column_between_metrics_and_processes() {
        for width in (60..=300_u16).step_by(7) {
            let planned = plan(Rect::new(0, 0, width, 30), Surface::Work, flags());
            let split = planned.split.unwrap();
            let proc = planned.processes.unwrap();
            assert_eq!(split.width, 1);
            assert_eq!(split.right(), proc.x, "{width}");
            assert!(proc.width >= 24, "{width}: proc {proc:?}");
            assert!(split.right() >= 36, "{width}: metrics + split {split:?}");
            assert_eq!(
                hit_test(
                    Rect::new(0, 0, width, 30),
                    Surface::Work,
                    flags(),
                    split.x,
                    10
                ),
                Some(Hit::Split)
            );
        }
    }
}
