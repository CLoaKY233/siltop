use std::fmt::Write as _;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use siltop_core::{Inspection, Process, bytes_short};

use crate::chips::{self, Chip};
use crate::chrome::{cell, push_kv, push_token};
use crate::layout::Hit;
use crate::theme::Theme;
use crate::widgets::AppView;

/// The dossier's pages: `tab`/`shift-tab` or `1`–`4` switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DossierTab {
    #[default]
    Overview,
    Threads,
    Files,
    Ports,
}

impl DossierTab {
    pub const ALL: [Self; 4] = [Self::Overview, Self::Threads, Self::Files, Self::Ports];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Threads => "threads",
            Self::Files => "files",
            Self::Ports => "ports",
        }
    }

    /// Tab `n` counted from 1, as the digit keys name them.
    #[must_use]
    pub fn nth(n: u8) -> Option<Self> {
        Self::ALL.get(usize::from(n).checked_sub(1)?).copied()
    }

    /// The next tab (`step` 1) or the previous one (-1), wrapping.
    #[must_use]
    pub fn step(self, step: i32) -> Self {
        let at = Self::ALL.iter().position(|t| *t == self).unwrap_or(0);
        let n = Self::ALL.len() as i32;
        let next = (i32::try_from(at).unwrap_or(0) + step).rem_euclid(n);
        Self::ALL[usize::try_from(next).unwrap_or(0)]
    }

    /// Whether the page reads the on-demand inspection of the pid.
    #[must_use]
    pub const fn inspects(self) -> bool {
        !matches!(self, Self::Overview)
    }
}

/// Narrowest card that still shows its border, a label and one chip.
pub(super) const CARD_MIN_WIDTH: u16 = 22;
/// Border, the action row, border.
pub(super) const CARD_MIN_HEIGHT: u16 = 3;

/// The dossier card inside `area`: inset one cell when there is room, and
/// empty (nothing drawn, nothing clickable) when the pane is too small for
/// a card. Never extends past `area`.
#[must_use]
pub fn detail_rect(area: Rect) -> Rect {
    if area.width < CARD_MIN_WIDTH || area.height < CARD_MIN_HEIGHT {
        return Rect::new(area.x, area.y, 0, 0);
    }
    let pad = u16::from(area.width >= CARD_MIN_WIDTH + 2 && area.height > CARD_MIN_HEIGHT);
    let room = area.height.saturating_sub(pad);
    let height = (area.height.saturating_mul(3) / 4).clamp(16.min(room), room);
    Rect {
        x: area.x.saturating_add(pad),
        y: area.y.saturating_add(pad),
        width: area.width.saturating_sub(pad.saturating_mul(2)),
        height,
    }
    .intersection(area)
}

/// The action row is always the last row inside the card's border, so its
/// click targets never drift with the layout above it.
fn action_row(card: Rect) -> Rect {
    Rect {
        x: card.x.saturating_add(1),
        y: card.y.saturating_add(card.height.saturating_sub(2)),
        width: card.width.saturating_sub(2),
        height: 1,
    }
}

fn placed_actions(card: Rect, read_only: bool) -> Vec<(Rect, Chip)> {
    let row = action_row(card);
    chips::place(&chips::dossier(read_only), row, row.x)
}

/// The card's action buttons in `area`; a replay (`read_only`) shows only
/// `esc close`.
#[must_use]
pub fn detail_actions(area: Rect, read_only: bool) -> Vec<(Rect, Hit)> {
    let card = detail_rect(area);
    if card.is_empty() {
        return Vec::new();
    }
    placed_actions(card, read_only)
        .into_iter()
        .map(|(rect, chip)| (rect, chip.action))
        .collect()
}

pub(super) fn render_detail(
    frame: &mut Frame,
    area: Rect,
    view: &AppView<'_>,
    pid: u32,
    theme: &Theme,
) {
    let Some(proc) = view.snapshot.processes.iter().find(|p| p.pid == pid) else {
        return;
    };
    let rect = detail_rect(area);
    if rect.is_empty() {
        return;
    }
    frame.render_widget(ratatui::widgets::Clear, rect);
    let title = Line::from(vec![
        Span::styled(" process ", theme.dim()),
        Span::styled(format!("{} ", proc.name), theme.title()),
        Span::styled(format!("{} ", proc.pid), theme.dim()),
    ]);
    let block = ratatui::widgets::Block::bordered()
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(title)
        .border_style(theme.border(true));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let tabs = Rect {
        height: inner.height.min(1),
        ..inner
    };
    let body = Rect {
        y: inner.y.saturating_add(1),
        height: inner.height.saturating_sub(2),
        ..inner
    };
    let tab = view.proc.tab;
    let inspection = view.proc.inspection.as_ref().filter(|i| i.pid == proc.pid);
    frame.render_widget(
        Paragraph::new(tab_line(tab, inspection, usize::from(tabs.width), theme)),
        tabs,
    );
    if body.height > 0 {
        if tab.inspects() && view.read_only {
            let line = Line::from(Span::styled(REPLAY_PAGE_NOTE, theme.dim()));
            frame.render_widget(
                Paragraph::new(line).wrap(ratatui::widgets::Wrap { trim: false }),
                body,
            );
        } else if tab.inspects() {
            render_page(frame, body, tab, inspection, view.proc.page_offset, theme);
        } else {
            render_dossier(frame, body, view, proc, theme);
        }
    }
    if let Some(close) = detail_close(area) {
        frame
            .buffer_mut()
            .set_string(close.x, close.y, crate::layout::CORNER_CLOSE, theme.dim());
    }
    let actions = placed_actions(rect, view.read_only);
    let row = action_row(rect);
    frame.render_widget(Paragraph::new(chips::line(&actions, row.x, theme)), row);
}

/// How many rows a page lists, once the pid has been read.
fn tab_count(tab: DossierTab, inspection: Option<&Inspection>) -> Option<usize> {
    let ins = inspection.filter(|i| !i.denied && !i.recorded)?;
    match tab {
        DossierTab::Overview => None,
        DossierTab::Threads => Some(ins.threads.len()),
        DossierTab::Files => Some(ins.files.len()),
        DossierTab::Ports => Some(ins.sockets.iter().filter(|s| is_network(s)).count()),
    }
}

/// A TCP or UDP socket: one with ports. Unix and kernel sockets are
/// counted in the ports page's note, not listed as ports.
fn is_network(socket: &siltop_core::Socket) -> bool {
    matches!(socket.kind, "tcp" | "udp")
}

/// `•1 overview  2 threads 14  3 files 52  4 ports 3`: the open tab bold
/// and marked with `•`, so it reads without colour. Counts appear once
/// the pid has been read and are dropped whole when the row is too narrow.
fn tab_labels(open: DossierTab, inspection: Option<&Inspection>, width: usize) -> Vec<String> {
    let label = |i: usize, tab: DossierTab, counts: bool| {
        let mark = if tab == open { "•" } else { " " };
        match tab_count(tab, inspection).filter(|_| counts) {
            Some(n) => format!("{mark}{} {} {n} ", i + 1, tab.label()),
            None => format!("{mark}{} {} ", i + 1, tab.label()),
        }
    };
    let texts = |counts: bool| -> Vec<String> {
        DossierTab::ALL
            .into_iter()
            .enumerate()
            .map(|(i, tab)| label(i, tab, counts))
            .collect()
    };
    let labels = texts(true);
    if labels.iter().map(|l| l.chars().count()).sum::<usize>() > width {
        texts(false)
    } else {
        labels
    }
}

fn tab_line(
    open: DossierTab,
    inspection: Option<&Inspection>,
    width: usize,
    theme: &Theme,
) -> Line<'static> {
    let spans = DossierTab::ALL
        .into_iter()
        .zip(tab_labels(open, inspection, width))
        .map(|(tab, text)| {
            let style = if tab == open {
                theme.title().add_modifier(ratatui::style::Modifier::BOLD)
            } else {
                theme.dim()
            };
            Span::styled(text, style)
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

/// The card's tab row inside `rect`.
fn tabs_row(rect: Rect) -> Rect {
    let inner = rect.inner(ratatui::layout::Margin::new(1, 1));
    Rect {
        height: inner.height.min(1),
        ..inner
    }
}

/// Each tab's label on the dossier in `area`, where it is drawn: a click
/// there opens that page.
#[must_use]
pub fn detail_tabs(area: Rect, view: &AppView<'_>) -> Vec<(Rect, DossierTab)> {
    let card = detail_rect(area);
    let row = tabs_row(card);
    if card.is_empty() || row.height == 0 {
        return Vec::new();
    }
    let inspection = view
        .proc
        .inspection
        .as_ref()
        .filter(|i| Some(i.pid) == view.detail_pid);
    let mut x = row.x;
    let mut out = Vec::new();
    for (tab, text) in DossierTab::ALL.into_iter().zip(tab_labels(
        view.proc.tab,
        inspection,
        usize::from(row.width),
    )) {
        let w = u16::try_from(text.chars().count()).unwrap_or(0);
        if x.saturating_add(w) > row.right() {
            break;
        }
        // the label without its trailing space
        out.push((Rect::new(x, row.y, w.saturating_sub(1), 1), tab));
        x = x.saturating_add(w);
    }
    out
}

/// The `[×]` on the dossier card's top border.
#[must_use]
pub fn detail_close(area: Rect) -> Option<Rect> {
    let card = detail_rect(area);
    let w = crate::layout::CORNER_W;
    (card.width > w + 12).then(|| Rect::new(card.right().saturating_sub(w + 1), card.y, w, 1))
}

/// A row's ink: busy rows in the foreground, idle ones dim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ink {
    Fg,
    Dim,
}

/// A threads, files or ports page: an optional summary line, the column
/// head, then the rows that scroll under them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Page {
    note: Option<String>,
    head: String,
    rows: Vec<(String, Ink)>,
    /// What an empty list says instead of a blank page.
    empty: &'static str,
}

impl Page {
    /// Lines the note and the column head take above the rows.
    fn chrome(&self) -> usize {
        1 + usize::from(self.note.is_some())
    }
}

/// A replay recorded no threads, files or sockets, and reading the live
/// pid would describe whatever process has that number now.
const REPLAY_PAGE_NOTE: &str = " not in a replay: threads, files and ports are read live";

/// What a threads, files or ports page shows for `inspection`.
enum PageBody {
    Reading,
    Denied,
    Recorded,
    List(Page),
}

fn port(p: Option<u16>) -> String {
    p.map_or_else(|| String::from(super::table::UNKNOWN), |p| p.to_string())
}

fn page_body(tab: DossierTab, inspection: Option<&Inspection>, width: usize) -> PageBody {
    let Some(ins) = inspection else {
        return PageBody::Reading;
    };
    if ins.recorded {
        return PageBody::Recorded;
    }
    if ins.denied {
        return PageBody::Denied;
    }
    match tab {
        DossierTab::Overview => PageBody::Reading,
        DossierTab::Threads => PageBody::List(threads_page(ins)),
        DossierTab::Files => PageBody::List(files_page(ins, width)),
        DossierTab::Ports => PageBody::List(ports_page(ins)),
    }
}

/// Busiest thread first, then the one with the most cpu time.
fn threads_page(ins: &Inspection) -> Page {
    let mut threads: Vec<_> = ins.threads.iter().collect();
    threads.sort_by(|a, b| {
        b.cpu_pct
            .total_cmp(&a.cpu_pct)
            .then(b.cpu_ns.cmp(&a.cpu_ns))
            .then(a.id.cmp(&b.id))
    });
    Page {
        note: None,
        head: format!("{:>10} {:>6} {:>9}  {}", "tid", "cpu%", "time", "name"),
        rows: threads
            .iter()
            .map(|t| {
                let secs = t.cpu_ns / 1_000_000_000;
                let name = if t.name.is_empty() {
                    super::table::UNKNOWN
                } else {
                    &t.name
                };
                let text = format!(
                    "{:>10} {:>6.1} {:>6}:{:02}  {name}",
                    t.id,
                    t.cpu_pct,
                    secs / 60,
                    secs % 60
                );
                let ink = if t.cpu_pct >= 0.05 { Ink::Fg } else { Ink::Dim };
                (text, ink)
            })
            .collect(),
        empty: "no threads visible",
    }
}

fn files_page(ins: &Inspection, width: usize) -> Page {
    Page {
        note: ins
            .cwd
            .as_deref()
            .map(|c| format!("cwd {}", elide_middle(c, width.saturating_sub(5)))),
        head: format!("{:>4}  {}", "fd", "path"),
        rows: ins
            .files
            .iter()
            .map(|f| {
                let path = elide_middle(&f.path, width.saturating_sub(7));
                (format!("{:>4}  {path}", f.fd), Ink::Fg)
            })
            .collect(),
        empty: "no open files",
    }
}

/// TCP and UDP sockets, listeners first; unix and kernel sockets are
/// only counted in the note.
fn ports_page(ins: &Inspection) -> Page {
    let listening = ins.listening_ports();
    let (mut sockets, local): (Vec<_>, Vec<_>) = ins.sockets.iter().partition(|s| is_network(s));
    // listeners first, then bound before unbound, then fd
    sockets.sort_by_key(|s| (!s.listening, s.local_port.is_none(), s.fd));
    let mut note = if listening.is_empty() {
        String::from("nothing listening")
    } else {
        let ports: Vec<String> = listening.iter().map(u16::to_string).collect();
        format!("listening on {}", ports.join(", "))
    };
    match local.len() {
        0 => {}
        1 => note.push_str(" · 1 local socket"),
        n => {
            let _ = write!(note, " · {n} local sockets");
        }
    }
    Page {
        note: Some(note),
        head: format!(
            "{:>4}  {:<5} {:>6}   {:<6} {}",
            "fd", "proto", "local", "remote", "state"
        ),
        rows: sockets
            .iter()
            .map(|s| {
                let state = if s.listening { "listen" } else { "" };
                let text = format!(
                    "{:>4}  {:<5} {:>6} → {:<6} {state}",
                    s.fd,
                    s.kind,
                    port(s.local_port),
                    port(s.remote_port)
                );
                let ink = if s.local_port.is_some() {
                    Ink::Fg
                } else {
                    Ink::Dim
                };
                (text, ink)
            })
            .collect(),
        empty: "no open ports",
    }
}

/// The body rect of the dossier card in the process pane `pane`: inside
/// the border, below the tab strip, above the action row.
fn body_rect(pane: Rect) -> Rect {
    let card = detail_rect(pane);
    let inner = card.inner(ratatui::layout::Margin::new(1, 1));
    Rect {
        y: inner.y.saturating_add(1),
        height: inner.height.saturating_sub(2),
        ..inner
    }
}

/// Rows a page lists in total and rows its body shows at once.
fn page_span(page: &Page, body_height: u16) -> (usize, usize) {
    let visible = usize::from(body_height).saturating_sub(page.chrome());
    (page.rows.len(), visible)
}

/// The furthest a page scrolls: its last row sits on the bottom line.
fn page_max_offset(page: &Page, body_height: u16) -> usize {
    let (len, visible) = page_span(page, body_height);
    len.saturating_sub(visible)
}

impl super::ProcView {
    /// Scrolls the open threads, files or ports page by `delta` rows,
    /// clamped so the last row never leaves the bottom of the card in the
    /// process pane `pane`. The overview does not scroll.
    pub fn scroll_page(&mut self, delta: i64, pane: Rect) {
        let body = body_rect(pane);
        let max = match page_body(
            self.tab,
            self.inspection.as_ref(),
            usize::from(body.width).saturating_sub(1),
        ) {
            PageBody::List(list) if self.tab.inspects() => page_max_offset(&list, body.height),
            _ => 0,
        };
        let at = i64::try_from(self.page_offset).unwrap_or(i64::MAX);
        let to = at.saturating_add(delta).max(0);
        self.page_offset = usize::try_from(to).unwrap_or(usize::MAX).min(max);
    }

    /// Rows one page-key press moves on the open page in `pane`.
    #[must_use]
    pub fn page_step(&self, pane: Rect) -> usize {
        usize::from(body_rect(pane).height).saturating_sub(3).max(1)
    }
}

/// Paints a threads, files or ports page: the note, the column head with
/// an `↑N ↓M` cue for rows scrolled off, then the visible window.
fn render_page(
    frame: &mut Frame,
    area: Rect,
    tab: DossierTab,
    inspection: Option<&Inspection>,
    offset: usize,
    theme: &Theme,
) {
    let width = usize::from(area.width);
    let fit = |text: &str, w: usize| super::table::fit(text, u16::try_from(w).unwrap_or(u16::MAX));
    let note = |text: &str| Line::from(Span::styled(fit(&format!(" {text}"), width), theme.dim()));
    // a whole-page note wraps: it is the page's only content
    let wrapped = |text: &str| {
        Paragraph::new(Span::styled(format!(" {text}"), theme.dim()))
            .wrap(ratatui::widgets::Wrap { trim: false })
    };
    let page = match page_body(tab, inspection, width.saturating_sub(1)) {
        PageBody::Reading => {
            frame.render_widget(wrapped("reading…"), area);
            return;
        }
        PageBody::Denied => {
            let text = "not readable: another user's process (needs admin)";
            frame.render_widget(wrapped(text), area);
            return;
        }
        PageBody::Recorded => {
            let text = "live only: a replay or the demo has no threads, files or ports";
            frame.render_widget(wrapped(text), area);
            return;
        }
        PageBody::List(page) => page,
    };
    let (len, visible) = page_span(&page, area.height);
    let offset = offset.min(page_max_offset(&page, area.height));
    let mut lines = Vec::with_capacity(visible + page.chrome());
    if let Some(text) = &page.note {
        lines.push(note(text));
    }
    let below = len.saturating_sub(offset + visible);
    let cue = match (offset, below) {
        (0, 0) => String::new(),
        (0, b) => format!("↓{b} "),
        (a, 0) => format!("↑{a} "),
        (a, b) => format!("↑{a} ↓{b} "),
    };
    let cue_w = cue.chars().count();
    let head = fit(&page.head, width.saturating_sub(cue_w + 1));
    let gap = width.saturating_sub(head.chars().count() + cue_w);
    lines.push(Line::from(vec![
        Span::styled(head, theme.title()),
        Span::raw(" ".repeat(gap)),
        Span::styled(cue, theme.dim()),
    ]));
    if page.rows.is_empty() {
        lines.push(note(page.empty));
    }
    lines.extend(
        page.rows
            .iter()
            .skip(offset)
            .take(visible)
            .map(|(text, ink)| {
                let style = match ink {
                    Ink::Fg => theme.fg(),
                    Ink::Dim => theme.dim(),
                };
                Line::from(Span::styled(fit(text, width), style))
            }),
    );
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_dossier(
    frame: &mut Frame,
    area: Rect,
    view: &AppView<'_>,
    proc: &Process,
    theme: &Theme,
) {
    let show_family = area.height >= 11;
    let show_command = area.height >= 7;
    // A spark cell needs one plot row inside its border, or it is an
    // empty box: only when the fixed bands leave three rows.
    let fixed = 5 + 3 * (u16::from(show_command) + u16::from(show_family));
    let show_spark = !proc.cpu_spark.is_empty() && area.height >= fixed + 3;
    let mut weights = vec![Constraint::Length(5)];
    if show_command {
        weights.push(Constraint::Length(3));
    }
    if show_spark {
        weights.push(Constraint::Fill(2));
    }
    if show_family {
        weights.push(Constraint::Length(3));
    }
    let rows = Layout::vertical(weights).split(area);
    let bands = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).split(rows[0]);
    paint_identity(frame, bands[0], view, proc, theme);
    let cpu_div = super::columns::cpu_divisor(view.config.cpu_unit, view.snapshot.process_ncpu);
    paint_live(frame, bands[1], proc, cpu_div, theme);
    let mut i = 1;
    if show_command {
        paint_command(frame, rows[i], proc, theme);
        i += 1;
    }
    if show_spark {
        let plot = cell(frame, rows[i], "cpu", theme);
        render_cpu_spark(frame, plot, &proc.cpu_spark, theme);
        i += 1;
    }
    if show_family && let Some(family) = rows.get(i) {
        paint_family(frame, *family, view, proc, theme);
    }
}

/// The table's `started` text, or [`UNKNOWN`](super::table::UNKNOWN).
fn started_ago(start_unix: i64, now_unix: i64) -> String {
    super::columns::started(start_unix, now_unix)
        .unwrap_or_else(|| String::from(super::table::UNKNOWN))
}

fn elide_middle(path: &str, max: usize) -> String {
    if path.chars().count() <= max {
        return path.to_owned();
    }
    let keep = max.saturating_sub(1) / 2;
    let head: String = path.chars().take(keep).collect();
    let tail: String = path.chars().skip(path.chars().count() - keep).collect();
    format!("{head}…{tail}")
}

fn paint_identity(
    frame: &mut Frame,
    area: Rect,
    view: &AppView<'_>,
    proc: &Process,
    theme: &Theme,
) {
    let parent = view
        .snapshot
        .processes
        .iter()
        .find(|p| p.pid == proc.ppid)
        .map_or_else(|| String::from("—"), |p| p.name.clone());
    // The live sampler names only stopped and zombie processes (Darwin
    // reports the rest as runnable); say what the CPU shows instead of `—`.
    let status = match (proc.status, proc.cpu) {
        ("", Some(cpu)) if cpu >= 0.5 => String::from("running"),
        ("", Some(_)) => String::from("sleeping"),
        ("", None) => String::from("—"),
        (word, _) => word.to_owned(),
    };
    let inner = cell(frame, area, "identity", theme);
    let mut spans = Vec::new();
    push_token(&mut spans, proc.name.clone(), big_style(theme));
    push_token(&mut spans, format!("@{}", proc.user), theme.dim());
    push_kv(&mut spans, theme, "state", status, theme.title());
    push_kv(
        &mut spans,
        theme,
        "up",
        started_ago(proc.start_unix, super::table::now_unix()),
        theme.fg(),
    );
    push_kv(&mut spans, theme, "pid", proc.pid.to_string(), theme.fg());
    push_kv(&mut spans, theme, "ppid", parent, theme.dim());
    frame.render_widget(
        Paragraph::new(wrap_spans(spans, usize::from(inner.width.max(1)))),
        inner,
    );
}

/// `cpu_div` scales the per-core percent the way the table's cpu column
/// does (see `columns::CellCtx`), so the two never disagree.
fn paint_live(frame: &mut Frame, area: Rect, proc: &Process, cpu_div: f32, theme: &Theme) {
    let inner = cell(frame, area, "live", theme);
    let cpu = proc.cpu.filter(|c| c.is_finite()).map(|c| c / cpu_div);
    let mut spans = Vec::new();
    push_kv(
        &mut spans,
        theme,
        "cpu",
        match cpu {
            Some(cpu) => format!("{cpu:>5.1}%"),
            None => format!("{:>6}", super::table::UNKNOWN),
        },
        if proc.cpu.is_some() {
            theme.cpu()
        } else {
            theme.dim()
        },
    );
    push_kv(
        &mut spans,
        theme,
        "mem",
        format!(
            "{:>7}",
            proc.mem_bytes
                .map_or_else(|| String::from(super::table::UNKNOWN), bytes_short)
        ),
        theme.fg(),
    );
    push_kv(
        &mut spans,
        theme,
        "threads",
        format!(
            "{:>4}",
            proc.threads
                .map_or_else(|| String::from(super::table::UNKNOWN), |t| t.to_string())
        ),
        theme.fg(),
    );
    frame.render_widget(
        Paragraph::new(wrap_spans(spans, usize::from(inner.width.max(1)))),
        inner,
    );
    // An unknown cpu draws no bar: an empty track would read as idle.
    if let Some(cpu) = cpu
        && inner.height >= 2
    {
        let bar = Rect {
            x: inner.x,
            y: inner.y.saturating_add(inner.height.saturating_sub(1)),
            width: inner.width,
            height: 1,
        };
        // A full bar is one busy core, or the whole chip under `total`.
        let fill = (cpu / 100.0).clamp(0.0, 1.0);
        crate::chrome::render_fill_bar(frame, bar, fill, theme.cpu, theme);
    }
}

fn paint_command(frame: &mut Frame, area: Rect, proc: &Process, theme: &Theme) {
    let inner = cell(frame, area, "command", theme);
    let text = proc.command.as_deref().unwrap_or("—");
    let max = usize::from(inner.width.max(1));
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            elide_middle(text, max),
            theme.fg(),
        ))),
        inner,
    );
}

fn paint_family(frame: &mut Frame, area: Rect, view: &AppView<'_>, proc: &Process, theme: &Theme) {
    let children: Vec<&Process> = view
        .snapshot
        .processes
        .iter()
        .filter(|p| p.ppid == proc.pid)
        .collect();
    let inner = cell(frame, area, "family", theme);
    let kids: String = if children.is_empty() {
        String::from("—")
    } else {
        children
            .iter()
            .take(4)
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>()
            .join("  ")
    };
    let mut spans = Vec::new();
    push_kv(
        &mut spans,
        theme,
        "children",
        format!("{:>3}", children.len()),
        theme.fg(),
    );
    if !children.is_empty() {
        push_token(&mut spans, kids, theme.dim());
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), inner);
}

fn render_cpu_spark(frame: &mut Frame, area: Rect, samples: &[f32], theme: &Theme) {
    let area = area.intersection(frame.area());
    if area.is_empty() || samples.is_empty() {
        return;
    }
    let mut history = siltop_core::History::with_capacity(samples.len().max(1));
    for sample in samples {
        history.push((*sample / 100.0).clamp(0.0, 1.0));
    }
    crate::chrome::render_scaled_graph(
        frame,
        area,
        crate::chrome::Graph {
            history: &history,
            ramp: theme.cpu,
            theme,
            scale: siltop_core::Scale::Fixed(1.0),
            axis: crate::chrome::Axis::None,
            ink: crate::chrome::GraphInk::Flat,
            time: siltop_core::TimeAxis::live(&history),
        },
    );
}

/// Wraps `spans` to `width`, never between a key and its value: a
/// separator and a `key  ` span travel with the span after them, so a
/// line never ends on `pid` with the number on the next.
fn wrap_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Line<'static>> {
    let glues = |span: &Span<'_>| span.content.trim().is_empty() || span.content.ends_with("  ");
    let mut units: Vec<Vec<Span<'static>>> = Vec::new();
    for span in spans {
        match units.last_mut() {
            Some(unit) if unit.last().is_some_and(glues) => unit.push(span),
            _ => units.push(vec![span]),
        }
    }
    let mut lines = vec![Line::default()];
    let mut used = 1usize;
    for mut unit in units {
        let len: usize = unit.iter().map(|s| s.content.chars().count()).sum();
        if used > 1 && used + len > width {
            lines.push(Line::default());
            used = 1;
            unit.retain(|s| !s.content.trim().is_empty() || s.content.is_empty());
            if let Some(first) = unit.first_mut() {
                first.content = first.content.trim_start().to_owned().into();
            }
        }
        for span in unit {
            push_to_last(&mut lines, span, &mut used);
        }
    }
    lines
}

fn push_to_last(lines: &mut [Line<'static>], span: Span<'static>, used: &mut usize) {
    let len = span.content.chars().count();
    if let Some(last) = lines.last_mut() {
        last.spans.push(span);
    }
    *used += len;
}

fn big_style(theme: &Theme) -> Style {
    use ratatui::style::Modifier;
    theme.title().add_modifier(Modifier::BOLD)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::widgets::ProcView;
    use crate::widgets::tests_support::fixture;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn paint(fx: &crate::widgets::tests_support::Fixture, w: u16, h: u16) -> (String, Rect) {
        paint_view(&fx.view(), w, h)
    }

    fn paint_view(view: &AppView<'_>, w: u16, h: u16) -> (String, Rect) {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        let area = Rect::new(0, 0, w, h);
        terminal
            .draw(|frame| super::super::render(frame, frame.area(), view, &Theme::default()))
            .unwrap();
        let buf = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        (text, area)
    }

    #[test]
    fn started_ago_steps() {
        assert_eq!(started_ago(100, 140), "40s");
        assert_eq!(started_ago(1, 181), "3m");
        assert_eq!(started_ago(1, 7_201), "2h00");
    }

    /// Regression: the dossier said `up 5h3m` beside the table's `5h03`,
    /// and `up 20720d` for an unknown start the table showed as `—`.
    #[test]
    fn dossier_age_matches_the_started_column() {
        let now = 1_790_000_000;
        let start = now - (5 * 3600 + 3 * 60);
        assert_eq!(started_ago(start, now), "5h03");
        assert_eq!(started_ago(0, now), super::super::table::UNKNOWN);
    }

    #[test]
    fn rv_started_ago_never_overflows() {
        for (start, now) in [
            (i64::MIN, 1_700_000_000),
            (i64::MAX, 0),
            (0, i64::MAX),
            (i64::MIN, i64::MAX),
        ] {
            let text = started_ago(start, now);
            assert!(!text.starts_with('-'), "{start} {now}: {text}");
        }
        assert_eq!(started_ago(200, 100), "0s", "a start in the future is now");
    }

    #[test]
    fn elide_keeps_head_and_tail() {
        let path = "/Applications/Xcode.app/Contents/MacOS/Xcode";
        let short = elide_middle(path, 16);
        assert!(short.contains('…'), "{short}");
        assert!(short.starts_with("/App"), "{short}");
        assert!(short.ends_with("code"), "{short}");
        assert_eq!(elide_middle("short", 16), "short");
    }

    #[test]
    fn d23_restricted_dossier_shows_dashes_not_zeros() {
        let mut fx = fixture("");
        let proc = &mut fx.snap.processes[0];
        proc.cpu = None;
        proc.mem_bytes = None;
        proc.threads = None;
        proc.restricted = true;
        fx.detail_pid = Some(proc.pid);
        let (text, _) = paint(&fx, 80, 24);
        assert!(text.contains('—'), "{text}");
        assert!(!text.contains(" 0B"), "{text}");
        assert!(!text.contains("0.0%"), "{text}");
        let live = text
            .lines()
            .position(|l| l.contains("╭ live"))
            .expect("live band");
        let track = text.lines().nth(live + 3).unwrap_or_default();
        assert!(
            !track.contains("────────────"),
            "no empty fill track for an unknown cpu:\n{text}"
        );
    }

    #[test]
    fn multi_core_cpu_shows_above_one_hundred() {
        let mut fx = fixture("");
        fx.snap.processes[0].cpu = Some(412.5);
        fx.detail_pid = Some(fx.snap.processes[0].pid);
        let (text, _) = paint(&fx, 80, 24);
        assert!(text.contains("412.5%"), "{text}");
    }

    /// Regression: `cpu_unit = "total"` was saved but never read, so the
    /// table and the dossier kept showing per-core percent.
    #[test]
    fn cpu_unit_total_shows_a_share_of_the_whole_chip() {
        let mut fx = fixture("");
        fx.snap.process_ncpu = 16;
        fx.snap.processes[0].cpu = Some(400.0);
        let (text, _) = paint(&fx, 80, 24);
        assert!(text.contains("cpu%") && text.contains("400.0"), "{text}");
        fx.config.cpu_unit = siltop_core::CpuUnit::Total;
        let (text, _) = paint(&fx, 80, 24);
        assert!(
            text.contains("chip%"),
            "the header names the scale:\n{text}"
        );
        assert!(text.contains("25.0") && !text.contains("400.0"), "{text}");
        fx.detail_pid = Some(fx.snap.processes[0].pid);
        let (text, _) = paint(&fx, 80, 24);
        assert!(text.contains(" 25.0%"), "the dossier agrees:\n{text}");
        fx.snap.process_ncpu = 0;
        let (text, _) = paint(&fx, 80, 24);
        assert!(
            text.contains("400.0%"),
            "unknown cpu count stays per-core:\n{text}"
        );
    }

    /// Regression: at 80x24 in the work layout the cpu spark got two rows,
    /// its border and nothing inside: an empty box.
    #[test]
    fn rv_spark_cell_is_never_an_empty_box() {
        let mut fx = fixture("");
        fx.snap.processes[0].cpu_spark = vec![3.0, 9.0, 4.0, 40.0];
        fx.detail_pid = Some(fx.snap.processes[0].pid);
        for h in 10..=40_u16 {
            let (text, _) = paint(&fx, 44, h);
            let lines = lines_of(&text);
            if let Some(top) = lines.iter().position(|l| l.contains("╭ cpu")) {
                let next = lines.get(top + 1).copied().unwrap_or_default();
                assert!(!next.contains("╰─"), "h={h}: empty spark box:\n{text}");
            }
        }
    }

    #[test]
    fn detail_paints_identity_and_actions() {
        let mut fx = fixture("");
        fx.snap.processes[0].user = String::from("cloaky");
        fx.snap.processes[0].command = Some(String::from("/Applications/Xcode.app/Xcode"));
        fx.snap.processes[0].status = "running";
        fx.snap.processes[0].start_unix = 1_700_000_000;
        fx.snap.processes[0].cpu_spark = vec![1.0, 4.0, 12.0, 8.0];
        fx.detail_pid = Some(904);
        let (text, _) = paint(&fx, 80, 24);
        for want in [
            "Xcode",
            "@cloaky",
            "running",
            "identity",
            "live",
            "command",
            "x quit",
            "K force quit",
            "i interrupt",
            "esc close",
        ] {
            assert!(text.contains(want), "missing {want}: {text}");
        }
        assert!(!text.contains("%%"), "{text}");
    }

    #[test]
    fn dossier_fills_most_of_the_pane() {
        let rect = detail_rect(Rect::new(0, 0, 80, 24));
        assert!(rect.height >= 16, "height {}", rect.height);
        assert!(rect.width >= 70, "width {}", rect.width);
        assert_eq!(rect.x, 1);
        assert_eq!(rect.y, 1);
    }

    #[test]
    fn action_hits_land_on_painted_chips_with_or_without_a_spark() {
        for spark in [Vec::new(), vec![3.0, 9.0, 4.0]] {
            for (w, h) in [(80_u16, 24_u16), (60, 16), (120, 40)] {
                let mut fx = fixture("");
                fx.snap.processes[0].cpu_spark.clone_from(&spark);
                fx.detail_pid = Some(904);
                let (text, area) = paint(&fx, w, h);
                let lines: Vec<&str> = text.lines().collect();
                for (rect, hit) in detail_actions(area, false) {
                    let row: String = lines[usize::from(rect.y)]
                        .chars()
                        .skip(usize::from(rect.x))
                        .take(usize::from(rect.width))
                        .collect();
                    let chip = chips::DOSSIER.iter().find(|c| c.action == hit).unwrap();
                    assert_eq!(
                        row,
                        format!("{} {}", chip.key, chip.verb),
                        "{w}x{h} spark={}",
                        !spark.is_empty()
                    );
                }
            }
        }
    }

    /// Every card and every chip stays inside the pane it was given.
    #[test]
    fn rv_detail_rect_never_leaves_the_pane() {
        for x in [0_u16, 7] {
            for w in 0..=90_u16 {
                for h in 0..=40_u16 {
                    let area = Rect::new(x, 2, w, h);
                    let card = detail_rect(area);
                    assert_eq!(card.intersection(area), card, "{w}x{h}");
                    if w < CARD_MIN_WIDTH || h < CARD_MIN_HEIGHT {
                        assert!(card.is_empty(), "{w}x{h}: no card");
                    } else {
                        assert!(card.width >= CARD_MIN_WIDTH - 2, "{w}x{h}");
                    }
                    for (rect, _) in detail_actions(area, false) {
                        assert_eq!(rect.intersection(card), rect, "{w}x{h} chip");
                    }
                }
            }
        }
    }

    #[test]
    fn rv_dossier_narrow_no_panic() {
        let mut fx = fixture("");
        fx.snap.processes[0].cpu_spark = vec![3.0, 9.0, 4.0, 40.0];
        fx.snap.processes[0].command = Some(String::from("/usr/bin/some-long-command --flag"));
        fx.detail_pid = Some(fx.snap.processes[0].pid);
        for w in 1..=40_u16 {
            for h in [1_u16, 2, 3, 4, 9, 16, 24, 40, 70] {
                let _ = paint(&fx, w, h);
            }
        }
    }

    #[test]
    fn rv_dossier_is_hidden_below_the_minimum_width() {
        let mut fx = fixture("");
        fx.detail_pid = Some(fx.snap.processes[0].pid);
        let (text, _) = paint(&fx, CARD_MIN_WIDTH - 1, 30);
        assert!(!text.contains("esc close"), "{text}");
        assert!(detail_actions(Rect::new(0, 0, CARD_MIN_WIDTH - 1, 30), false).is_empty());
    }

    fn inspected_fixture(tab: DossierTab) -> crate::widgets::tests_support::Fixture {
        use siltop_core::{OpenFile, Socket, ThreadInfo};
        let mut fx = fixture("");
        let pid = fx.snap.processes[0].pid;
        fx.detail_pid = Some(pid);
        fx.proc.tab = tab;
        fx.proc.inspection = Some(Inspection {
            pid,
            threads: vec![
                ThreadInfo {
                    id: 101,
                    name: String::from("main"),
                    cpu_ns: 75_000_000_000,
                    cpu_pct: 2.5,
                },
                ThreadInfo {
                    id: 102,
                    name: String::from("com.apple.render"),
                    cpu_ns: 1_000_000_000,
                    cpu_pct: 40.0,
                },
            ],
            files: vec![OpenFile {
                fd: 3,
                path: String::from("/Users/me/Library/Caches/some/very/long/path/to/a/file.db"),
            }],
            sockets: vec![Socket {
                fd: 9,
                kind: "tcp",
                local_port: Some(8080),
                remote_port: None,
                listening: true,
            }],
            cwd: Some(String::from("/Users/me/src")),
            denied: false,
            recorded: false,
        });
        fx
    }

    #[test]
    fn every_tab_fits_80x24_with_its_strip_and_actions() {
        for tab in DossierTab::ALL {
            let fx = inspected_fixture(tab);
            let (text, _) = paint(&fx, 80, 24);
            assert!(text.contains("1 overview"), "{tab:?}:\n{text}");
            assert!(text.contains("4 ports"), "{tab:?}:\n{text}");
            assert!(
                text.contains(&format!(
                    "•{}",
                    DossierTab::ALL.iter().position(|t| *t == tab).unwrap() + 1
                )),
                "{tab:?}: the open tab is marked without colour:\n{text}"
            );
            let hits = detail_actions(Rect::new(0, 0, 80, 24), false);
            assert!(!hits.is_empty(), "{tab:?}: actions stay clickable");
        }
    }

    #[test]
    fn threads_page_lists_the_busiest_thread_first() {
        let (text, _) = paint(&inspected_fixture(DossierTab::Threads), 80, 24);
        let busy = text.find("com.apple.render").expect("render thread listed");
        let main = text.find(" main").expect("main thread listed");
        assert!(busy < main, "sorted by cpu:\n{text}");
        assert!(
            text.contains("2 threads 2"),
            "count in the tab label: {text}"
        );
        assert!(text.contains("1:15"), "cpu time as m:ss:\n{text}");
    }

    #[test]
    fn files_and_ports_pages_show_paths_cwd_and_listeners() {
        let (files, _) = paint(&inspected_fixture(DossierTab::Files), 80, 24);
        assert!(files.contains("cwd /Users/me/src"), "{files}");
        assert!(
            files.contains("file.db"),
            "path tail survives elision:\n{files}"
        );
        let (ports, _) = paint(&inspected_fixture(DossierTab::Ports), 80, 24);
        assert!(ports.contains("listening on 8080"), "{ports}");
        assert!(ports.contains("tcp"), "{ports}");
    }

    #[test]
    fn replay_pages_never_show_a_live_inspection() {
        let fx = inspected_fixture(DossierTab::Threads);
        let view = AppView {
            read_only: true,
            ..fx.view()
        };
        let (text, _) = paint_view(&view, 80, 24);
        assert!(text.contains("not in a replay"), "{text}");
        assert!(!text.contains("com.apple.render"), "no live list: {text}");
    }

    #[test]
    fn recorded_pid_pages_say_live_only_and_show_no_counts() {
        for tab in [DossierTab::Threads, DossierTab::Files, DossierTab::Ports] {
            let mut fx = inspected_fixture(tab);
            if let Some(ins) = fx.proc.inspection.as_mut() {
                ins.recorded = true;
            }
            let (text, _) = paint(&fx, 100, 24);
            assert!(text.contains("live only"), "{text}");
            assert!(
                !text.contains("com.apple.render") && !text.contains("8080"),
                "a recorded pid lists nothing read here:\n{text}"
            );
            assert!(!text.contains("threads 2"), "no counts:\n{text}");
        }
    }

    #[test]
    fn pages_say_reading_or_denied_instead_of_an_empty_list() {
        let mut fx = inspected_fixture(DossierTab::Threads);
        fx.proc.inspection = None;
        let (text, _) = paint(&fx, 80, 24);
        assert!(text.contains("reading…"), "{text}");
        let mut fx = inspected_fixture(DossierTab::Files);
        if let Some(ins) = fx.proc.inspection.as_mut() {
            ins.denied = true;
        }
        let (text, _) = paint(&fx, 80, 24);
        assert!(text.contains("needs admin"), "{text}");
        // a stale inspection of another pid is never shown
        let mut fx = inspected_fixture(DossierTab::Threads);
        if let Some(ins) = fx.proc.inspection.as_mut() {
            ins.pid += 1;
        }
        let (text, _) = paint(&fx, 80, 24);
        assert!(
            text.contains("reading…") && !text.contains("com.apple.render"),
            "{text}"
        );
    }

    #[test]
    fn tab_steps_wrap_and_digits_pick() {
        assert_eq!(DossierTab::Overview.step(-1), DossierTab::Ports);
        assert_eq!(DossierTab::Ports.step(1), DossierTab::Overview);
        assert_eq!(DossierTab::nth(3), Some(DossierTab::Files));
        assert_eq!(DossierTab::nth(0), None);
        assert_eq!(DossierTab::nth(5), None);
        assert!(!DossierTab::Overview.inspects());
    }

    #[test]
    fn rv_tabbed_dossier_never_panics_small() {
        for tab in DossierTab::ALL {
            for w in [22, 30, 40, 60, 80] {
                for h in [3, 5, 8, 12, 24] {
                    let _ = paint(&inspected_fixture(tab), w, h);
                }
            }
        }
    }

    fn with_files(n: i32) -> crate::widgets::tests_support::Fixture {
        let mut fx = inspected_fixture(DossierTab::Files);
        if let Some(ins) = fx.proc.inspection.as_mut() {
            ins.files = (0..n)
                .map(|fd| siltop_core::OpenFile {
                    fd,
                    path: format!("/tmp/file-{fd:03}"),
                })
                .collect();
        }
        fx
    }

    fn lines_of(text: &str) -> Vec<&str> {
        text.lines().collect()
    }

    #[test]
    fn tab_labels_carry_counts_once_the_pid_is_read() {
        let (text, _) = paint(&inspected_fixture(DossierTab::Files), 80, 24);
        for want in ["2 threads 2", "3 files 1", "4 ports 1"] {
            assert!(text.contains(want), "missing {want}:\n{text}");
        }
        assert!(!text.contains("overview 0"), "the overview has no count");
        let mut fx = inspected_fixture(DossierTab::Threads);
        fx.proc.inspection = None;
        let (text, _) = paint(&fx, 80, 24);
        assert!(
            text.contains("2 threads  "),
            "no count before a read:\n{text}"
        );
    }

    #[test]
    fn tab_counts_are_dropped_whole_when_narrow() {
        let fx = with_files(1234);
        let ins = fx.proc.inspection.as_ref();
        let theme = Theme::default();
        let text = |w: usize| -> String {
            tab_line(DossierTab::Files, ins, w, &theme)
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect()
        };
        assert!(text(80).contains("files 1234"), "{}", text(80));
        let narrow = text(40);
        assert!(!narrow.contains("1234"), "{narrow}");
        assert!(narrow.contains("•3 files"), "{narrow}");
        assert!(!narrow.contains("files 12"), "never a cut count: {narrow}");
    }

    #[test]
    fn denied_inspection_shows_no_counts() {
        let mut fx = inspected_fixture(DossierTab::Ports);
        if let Some(ins) = fx.proc.inspection.as_mut() {
            ins.denied = true;
        }
        let (text, _) = paint(&fx, 80, 24);
        assert!(!text.contains("ports 1"), "{text}");
        assert!(text.contains("needs admin"), "{text}");
    }

    #[test]
    fn empty_pages_say_so_in_words() {
        for (tab, want) in [
            (DossierTab::Threads, "no threads visible"),
            (DossierTab::Files, "no open files"),
            (DossierTab::Ports, "no open ports"),
        ] {
            let mut fx = inspected_fixture(tab);
            if let Some(ins) = fx.proc.inspection.as_mut() {
                ins.threads.clear();
                ins.files.clear();
                ins.sockets.clear();
            }
            let (text, _) = paint(&fx, 80, 24);
            assert!(text.contains(want), "{tab:?}:\n{text}");
        }
    }

    #[test]
    fn ports_page_says_nothing_listening_instead_of_a_blank_list() {
        let mut fx = inspected_fixture(DossierTab::Ports);
        if let Some(ins) = fx.proc.inspection.as_mut() {
            ins.sockets[0].listening = false;
        }
        let (text, _) = paint(&fx, 80, 24);
        assert!(text.contains("nothing listening"), "{text}");
        assert!(!text.contains("listening on"), "{text}");
    }

    #[test]
    fn ports_page_lists_listeners_first() {
        use siltop_core::Socket;
        let mut fx = inspected_fixture(DossierTab::Ports);
        if let Some(ins) = fx.proc.inspection.as_mut() {
            ins.sockets = vec![
                Socket {
                    fd: 4,
                    kind: "unix",
                    ..Socket::default()
                },
                Socket {
                    fd: 5,
                    kind: "tcp",
                    local_port: Some(51_000),
                    remote_port: Some(443),
                    listening: false,
                },
                Socket {
                    fd: 12,
                    kind: "tcp",
                    local_port: Some(3000),
                    remote_port: None,
                    listening: true,
                },
            ];
        }
        let (text, _) = paint(&fx, 80, 24);
        let at = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("{needle}:\n{text}"))
        };
        assert!(at("  3000") < at("51000"), "{text}");
        assert!(text.contains("51000 → 443"), "{text}");
        assert!(text.contains("· 1 local socket"), "{text}");
        assert!(
            !text.contains("  unix"),
            "a unix socket is not a port:\n{text}"
        );
        assert!(
            text.contains("4 ports 2"),
            "only network sockets count:\n{text}"
        );
    }

    #[test]
    fn rv_only_local_sockets_is_no_open_ports() {
        let mut fx = inspected_fixture(DossierTab::Ports);
        if let Some(ins) = fx.proc.inspection.as_mut() {
            ins.sockets = vec![siltop_core::Socket {
                fd: 15,
                kind: "kctl",
                ..siltop_core::Socket::default()
            }];
        }
        let (text, _) = paint(&fx, 80, 24);
        assert!(text.contains("no open ports"), "{text}");
        assert!(!text.contains("— → —"), "no dash-arrow-dash row:\n{text}");
        assert!(text.contains("4 ports 0"), "{text}");
    }

    /// Regression: the identity band wrapped `pid` onto one line and its
    /// number onto the next.
    #[test]
    fn rv_identity_never_splits_a_key_from_its_value() {
        let theme = Theme::default();
        let mut spans = Vec::new();
        push_token(&mut spans, String::from("dataaccessd"), theme.title());
        push_kv(&mut spans, &theme, "up", String::from("2d"), theme.fg());
        push_kv(&mut spans, &theme, "pid", String::from("86483"), theme.fg());
        push_kv(
            &mut spans,
            &theme,
            "ppid",
            String::from("launchd"),
            theme.dim(),
        );
        for width in 8..40 {
            for line in wrap_spans(spans.clone(), width) {
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                let trimmed = text.trim_end();
                for key in ["up", "pid", "ppid"] {
                    assert!(!trimmed.ends_with(key), "w={width}: {text:?}");
                }
                assert!(!text.starts_with(' '), "w={width}: {text:?}");
            }
        }
    }

    #[test]
    fn page_heads_share_a_right_edge_with_their_numbers() {
        let end = |line: &str, needle: &str| {
            let at = line.find(needle).expect(needle);
            line[..at].chars().count() + needle.chars().count()
        };
        let (text, _) = paint(&inspected_fixture(DossierTab::Threads), 80, 24);
        let lines = lines_of(&text);
        let head = lines.iter().find(|l| l.contains("cpu%")).expect("head");
        let row = lines
            .iter()
            .find(|l| l.contains("com.apple.render"))
            .expect("row");
        assert_eq!(end(head, "cpu%"), end(row, "40.0"), "\n{head}\n{row}");
        assert_eq!(end(head, "tid"), end(row, "102"), "\n{head}\n{row}");
        let (ports, _) = paint(&inspected_fixture(DossierTab::Ports), 80, 24);
        let lines = lines_of(&ports);
        let head = lines.iter().find(|l| l.contains("proto")).expect("head");
        let row = lines.iter().find(|l| l.contains("tcp")).expect("row");
        assert_eq!(end(head, "local"), end(row, "8080"), "\n{head}\n{row}");
        assert_eq!(end(head, " fd"), end(row, "   9"), "\n{head}\n{row}");
    }

    #[test]
    fn scroll_page_stops_with_the_last_row_on_the_bottom_line() {
        let pane = Rect::new(0, 0, 80, 24);
        let mut fx = with_files(200);
        fx.proc.scroll_page(i64::MAX, pane);
        let body = body_rect(pane);
        // the cwd note and the column head take two lines
        let visible = usize::from(body.height) - 2;
        assert_eq!(fx.proc.page_offset, 200 - visible);
        let (text, _) = paint(&fx, 80, 24);
        let lines = lines_of(&text);
        let last = lines[usize::from(body.y + body.height - 1)];
        assert!(last.contains("/tmp/file-199"), "{text}");
        assert!(text.contains(&format!("↑{}", 200 - visible)), "{text}");
        assert!(!text.contains('↓'), "nothing below the end:\n{text}");
        fx.proc.scroll_page(i64::MIN, pane);
        assert_eq!(fx.proc.page_offset, 0);
        let (text, _) = paint(&fx, 80, 24);
        assert!(text.contains(&format!("↓{}", 200 - visible)), "{text}");
        assert!(text.contains("/tmp/file-000"), "{text}");
    }

    #[test]
    fn scroll_page_moves_one_row_at_a_time_and_never_below_zero() {
        let pane = Rect::new(0, 0, 80, 24);
        let mut fx = with_files(50);
        fx.proc.scroll_page(-3, pane);
        assert_eq!(fx.proc.page_offset, 0);
        fx.proc.scroll_page(1, pane);
        fx.proc.scroll_page(1, pane);
        assert_eq!(fx.proc.page_offset, 2);
        let (text, _) = paint(&fx, 80, 24);
        assert!(!text.contains("/tmp/file-001"), "scrolled past:\n{text}");
        assert!(text.contains("/tmp/file-002"), "{text}");
        assert!(text.contains("↑2 ↓"), "both cues:\n{text}");
    }

    #[test]
    fn a_short_list_never_scrolls() {
        let pane = Rect::new(0, 0, 80, 24);
        let mut fx = inspected_fixture(DossierTab::Threads);
        fx.proc.scroll_page(10, pane);
        assert_eq!(fx.proc.page_offset, 0);
        fx.proc.tab = DossierTab::Overview;
        fx.proc.page_offset = 7;
        fx.proc.scroll_page(1, pane);
        assert_eq!(fx.proc.page_offset, 0, "the overview does not scroll");
    }

    #[test]
    fn rv_stale_offset_past_a_shrunk_list_still_shows_rows() {
        let mut fx = with_files(5);
        fx.proc.page_offset = 500;
        let (text, _) = paint(&fx, 80, 24);
        assert!(text.contains("/tmp/file-000"), "clamped at paint:\n{text}");
        assert!(text.contains("/tmp/file-004"), "{text}");
    }

    #[test]
    fn every_tab_with_a_long_list_fits_80x24_inside_the_card() {
        for tab in [DossierTab::Threads, DossierTab::Files, DossierTab::Ports] {
            let mut fx = with_files(300);
            fx.proc.tab = tab;
            if let Some(ins) = fx.proc.inspection.as_mut() {
                ins.threads = (0..300)
                    .map(|id| siltop_core::ThreadInfo {
                        id,
                        name: format!("worker-{id}"),
                        ..siltop_core::ThreadInfo::default()
                    })
                    .collect();
                ins.sockets = (0..300)
                    .map(|fd| siltop_core::Socket {
                        fd,
                        kind: "tcp",
                        local_port: Some(40_000),
                        ..siltop_core::Socket::default()
                    })
                    .collect();
            }
            let (text, area) = paint(&fx, 80, 24);
            let lines = lines_of(&text);
            let card = detail_rect(area);
            let actions = lines[usize::from(card.y + card.height - 2)];
            assert!(actions.contains("esc close"), "{tab:?}: {actions}");
            let bottom = lines[usize::from(card.y + card.height - 1)];
            assert!(bottom.contains('╰'), "{tab:?}: border intact: {bottom}");
            assert!(text.contains('↓'), "{tab:?}: more below:\n{text}");
            assert!(text.contains("300"), "{tab:?}: count in the label:\n{text}");
        }
    }

    #[test]
    fn page_step_is_a_screenful_less_context() {
        let view = ProcView::default();
        let body = body_rect(Rect::new(0, 0, 80, 24));
        assert_eq!(
            view.page_step(Rect::new(0, 0, 80, 24)),
            usize::from(body.height) - 3
        );
        assert_eq!(view.page_step(Rect::new(0, 0, 30, 4)), 1, "never zero");
    }
}
