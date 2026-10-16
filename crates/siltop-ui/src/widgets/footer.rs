//! The one chrome row, at the bottom. Left: the quiet `siltop ≡` menu
//! button and the contextual key chips. Middle: the verdict (or a toast).
//! Right: one status segment, power · system watts · sampling state.
//! [`layout`] positions everything once; painting and clicks both read it.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use siltop_core::{
    ChargeState, PowerSource, Snapshot, TimeRemaining, Verdict, VerdictKind, watts_display,
};
use unicode_width::UnicodeWidthStr;

use crate::chips::{self, Chip};
use crate::layout::Hit;
use crate::theme::Theme;
use crate::titles::{Keep, Title};
use crate::widgets::{AppView, power};

/// How a status toast reads: news, something to notice, or a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToastKind {
    #[default]
    Info,
    Warn,
    Error,
}

/// The menu button, wordmark first; narrow windows keep just the `≡`.
pub(super) const BRAND: &str = "siltop ≡";
const BRAND_SHORT: &str = "≡";
/// Below this width the wordmark gives its room to the chips.
const BRAND_MIN_WIDTH: u16 = 50;
/// The status never shrinks below this for a toast (`● 500 ms`).
const STATUS_MIN: u16 = 10;
/// A toast cut to less than this says nothing; it waits for a wider row.
const TOAST_MIN: usize = 8;
/// Columns kept clear between neighbouring footer groups.
const GAP: u16 = 2;

/// Everything on the footer row, positioned once.
struct Footer {
    brand: Option<(Rect, &'static str)>,
    lead: Option<String>,
    chips: Vec<(Rect, Chip)>,
    /// The verdict headline, when it fits whole between chips and status.
    verdict: Option<(Rect, String)>,
    toast: Option<(u16, String)>,
    /// Right-aligned status, already fitted to its room.
    status: Option<(u16, Line<'static>)>,
    /// The update badge inside the status line, when it survived fitting.
    update: Option<Rect>,
}

fn width_of(text: &str) -> u16 {
    u16::try_from(text.width()).unwrap_or(u16::MAX)
}

/// `500 ms`, `1 s`, `2.5 s`: the one spelling of a sampling interval, for
/// the footer, the interval toast and the settings row alike.
#[must_use]
pub fn interval_label(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms.is_multiple_of(1000) {
        format!("{} s", ms / 1000)
    } else {
        format!("{}.{} s", ms / 1000, (ms % 1000) / 100)
    }
}

/// `AC 80%`, `AC 80% ▲`, `74% ▼ 11W 3h12m` (short: `74% ▼`). `None` on
/// Macs without a battery, so desktops show no power token at all. With a
/// system-watts token beside it the drain is that same draw, so the battery
/// token drops its watts: `74% ▼ 3h12m  11W` (one fact, one home).
fn push_battery(status: &mut Title, snap: &Snapshot, system_shown: bool, theme: &Theme) {
    let bat = &snap.battery;
    if !bat.present {
        return;
    }
    let pct = bat.percent.map(|p| format!("{p}%"));
    if bat.source == PowerSource::Battery {
        let pct = pct.unwrap_or_default();
        let watts = power::battery_flow(bat)
            .filter(|_| !system_shown)
            .map(|w| format!(" {}", watts_display(w.abs())))
            .unwrap_or_default();
        let left = match bat.time_remaining {
            TimeRemaining::Known(d) => format!(" {}", power::duration_short(d)),
            _ => String::new(),
        };
        let low = bat.percent.is_some_and(|p| p <= 20);
        let style = if low { theme.warn_style() } else { theme.fg() };
        let short = format!("{pct} ▼").trim_start().to_owned();
        let full = format!("{pct} ▼{watts}{left}").trim_start().to_owned();
        status.push_short(Keep::Mid, full, short, style);
    } else {
        let source = bat.source.label().unwrap_or("AC");
        let arrow = if bat.state == ChargeState::Charging {
            " ▲"
        } else {
            ""
        };
        let text = match pct {
            Some(pct) => format!("{source} {pct}{arrow}"),
            None => format!("{source}{arrow}"),
        };
        status.push(Keep::Mid, text, theme.dim());
    }
}

/// The right-hand status: power, system watts, sampling state. Tokens drop
/// by priority through [`Title::fit`], the one fitter; the state goes last.
fn status_title(view: &AppView<'_>, theme: &Theme) -> Title {
    let mut status = Title::bare();
    if let Some(note) = view.update_note {
        status.push(Keep::High, note.to_owned(), theme.warn_style());
    }
    let system = view
        .snapshot
        .power
        .system
        .and_then(siltop_core::Reading::usable)
        .filter(|w| power::shows_watts(*w));
    push_battery(&mut status, view.snapshot, system.is_some(), theme);
    if let Some(w) = system {
        status.push(Keep::Low, watts_display(w), theme.dim());
    }
    if view.stopped {
        status.push_short(
            Keep::Always,
            "■ stopped · restart siltop",
            "■ stopped",
            Style::default().fg(theme.crit).add_modifier(Modifier::BOLD),
        );
    } else if view.frozen {
        let full = match view.paused_at {
            Some(at) => format!("❚❚ paused {at}"),
            None => String::from("❚❚ paused"),
        };
        status.push_short(Keep::Always, full, "❚❚ paused", theme.warn_style());
    } else if view.battery_saver {
        let cadence = format!("● {}", interval_label(view.interval_ms));
        let full = format!("{cadence} · battery saver");
        status.push_short(Keep::Always, full, cadence, theme.dim());
    } else {
        status.push(
            Keep::Always,
            format!("● {}", interval_label(view.interval_ms)),
            theme.dim(),
        );
    }
    status
}

fn verdict_style(verdict: &Verdict, theme: &Theme) -> Style {
    match verdict.kind {
        VerdictKind::Normal => theme.dim(),
        VerdictKind::Throttling => Style::default().fg(theme.crit).add_modifier(Modifier::BOLD),
        VerdictKind::Busy | VerdictKind::BatteryDrain | VerdictKind::MemoryTight => {
            theme.warn_style()
        }
    }
}

fn essential_width(chips: &[Chip]) -> u16 {
    chips
        .iter()
        .filter(|c| c.essential)
        .map(|c| c.width().saturating_add(GAP))
        .fold(0, u16::saturating_add)
}

/// A placed toast: its column and its (possibly cut) text.
type Toast = (u16, String);

/// Places the chips and the toast between `left_end` and the status at
/// `status_x`. News outranks hints: a toast that does not fit whole takes
/// the room of the optional chips, rightmost first, and only then is cut
/// with a `…`, always a gap short of the status.
fn chips_and_toast(
    wanted: &[Chip],
    chip_area: Rect,
    left_end: u16,
    toast: Option<&str>,
    status_x: u16,
) -> (Vec<(Rect, Chip)>, Option<Toast>) {
    let chips_end = |placed: &[(Rect, Chip)]| {
        placed
            .last()
            .map_or(left_end, |(rect, _)| rect.x.saturating_add(rect.width))
    };
    let end = status_x.saturating_sub(GAP);
    let room = |placed: &[(Rect, Chip)]| {
        usize::from(end.saturating_sub(chips_end(placed).saturating_add(GAP)))
    };
    let mut kept = wanted.to_vec();
    let mut chips = chips::place(&kept, chip_area, left_end);
    let Some(toast) = toast else {
        return (chips, None);
    };
    while toast.width() > room(&chips) {
        let Some(drop) = kept.iter().rposition(|c| !c.essential) else {
            break;
        };
        kept.remove(drop);
        chips = chips::place(&kept, chip_area, left_end);
    }
    let room = room(&chips);
    let placed = (room >= TOAST_MIN).then(|| {
        let text = ellipsize(toast, room);
        let x = end
            .saturating_sub(width_of(&text))
            .max(chips_end(&chips).saturating_add(GAP));
        (x, text)
    });
    (chips, placed)
}

fn layout(view: &AppView<'_>, area: Rect) -> Footer {
    let theme = view.theme;
    let right = area.x.saturating_add(area.width);
    let confirm = view.confirm_kill;
    let wanted = chips::footer_chips(view, area);
    // The question yields to its own y/n answers, never the other way round:
    // the critical note's reason goes first, then the names.
    let lead = confirm.then(|| {
        let room = right
            .saturating_sub(area.x)
            .saturating_sub(essential_width(&wanted));
        confirm_who(view, usize::from(room))
    });
    let brand = (!confirm).then(|| {
        let text = if area.width >= BRAND_MIN_WIDTH {
            BRAND
        } else {
            BRAND_SHORT
        };
        (
            Rect::new(area.x.saturating_add(1), area.y, width_of(text), 1),
            text,
        )
    });
    let left_end = match (&brand, &lead) {
        (Some((r, _)), _) => r.x.saturating_add(r.width),
        (None, Some(lead)) => area.x.saturating_add(width_of(lead)),
        (None, None) => area.x,
    };
    // The status yields to the essential chips, never the other way round.
    let status = (!confirm).then(|| {
        let room = right
            .saturating_sub(left_end)
            .saturating_sub(essential_width(&wanted))
            .saturating_sub(GAP + 1);
        // A toast is news: the status gives it room down to its one
        // always-kept token (the sampling state).
        let toast = view.status.map_or(0, |t| width_of(t).saturating_add(GAP));
        let room = room.saturating_sub(toast).max(STATUS_MIN.min(room));
        let line = status_title(view, theme).fit(usize::from(room), theme);
        let w = u16::try_from(line.width()).unwrap_or(u16::MAX);
        (right.saturating_sub(w).saturating_sub(1), line)
    });
    let status = status.filter(|(_, line)| line.width() > 0);
    let update = status.as_ref().and_then(|(x, line)| {
        let note = view.update_note?;
        let mut at = *x;
        for span in &line.spans {
            let w = width_of(span.content.as_ref());
            if span.content.contains(note) {
                return Some(Rect::new(at, area.y, w, 1));
            }
            at = at.saturating_add(w);
        }
        None
    });
    let limit = status
        .as_ref()
        .map_or(right, |(x, _)| x.saturating_sub(GAP));
    let chip_area = Rect {
        width: limit.saturating_sub(area.x),
        ..area
    };
    let status_x = status.as_ref().map_or(right, |(x, _)| *x);
    let toast = view.status.filter(|_| !confirm);
    let (chips, toast) = chips_and_toast(&wanted, chip_area, left_end, toast, status_x);
    let chips_end = chips
        .last()
        .map_or(left_end, |(rect, _)| rect.x.saturating_add(rect.width));
    let gap_start = chips_end.saturating_add(GAP + 1);
    let gap_end = limit.saturating_sub(GAP);
    // A toast is news and takes the slot; the verdict comes back after it.
    let verdict = view
        .verdict
        .filter(|_| toast.is_none() && !confirm)
        .and_then(|v| {
            let w = width_of(&v.headline);
            (gap_start.saturating_add(w) <= gap_end).then(|| {
                let x = gap_end.saturating_sub(w);
                (Rect::new(x, area.y, w, 1), v.headline.clone())
            })
        });
    Footer {
        brand,
        lead,
        chips,
        verdict,
        toast,
        status,
        update,
    }
}

fn pad_to(spans: &mut Vec<Span<'static>>, cursor: &mut u16, x: u16) {
    if x > *cursor {
        spans.push(Span::raw(" ".repeat(usize::from(x - *cursor))));
        *cursor = x;
    }
}

pub(super) fn render(frame: &mut Frame, area: Rect, view: &AppView<'_>, theme: &Theme) {
    let footer = layout(view, area);
    let mut spans = Vec::new();
    let mut cursor = area.x;
    if let Some(lead) = footer.lead {
        cursor = cursor.saturating_add(width_of(&lead));
        spans.push(Span::styled(lead, theme.title()));
    }
    if let Some((rect, text)) = footer.brand {
        pad_to(&mut spans, &mut cursor, rect.x);
        spans.push(Span::styled(text, theme.dim()));
        cursor = rect.x.saturating_add(rect.width);
    }
    spans.extend(chips::spans(&footer.chips, cursor, theme));
    if let Some((rect, _)) = footer.chips.last() {
        cursor = rect.x.saturating_add(rect.width);
    }
    if let Some((x, text)) = footer.toast {
        pad_to(&mut spans, &mut cursor, x);
        cursor = cursor.saturating_add(width_of(&text));
        spans.push(Span::styled(text, toast_style(view.status_kind, theme)));
    }
    if let (Some((rect, text)), Some(verdict)) = (footer.verdict, view.verdict) {
        pad_to(&mut spans, &mut cursor, rect.x);
        cursor = rect.x.saturating_add(rect.width);
        spans.push(Span::styled(text, verdict_style(verdict, theme)));
    }
    if let Some((x, line)) = footer.status {
        pad_to(&mut spans, &mut cursor, x);
        spans.extend(line.spans);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn truncate(text: &str, room: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > room {
            break;
        }
        used += w;
        out.push(c);
    }
    out
}

/// `text` cut to `room` cells, ending in `…` when anything was cut.
pub(crate) fn ellipsize(text: &str, room: usize) -> String {
    if text.width() <= room {
        return text.to_owned();
    }
    let mut out = truncate(text, room.saturating_sub(1));
    if room > 0 {
        out.push('…');
    }
    out
}

fn toast_style(kind: ToastKind, theme: &Theme) -> Style {
    match kind {
        ToastKind::Info => theme.fg(),
        ToastKind::Warn => theme.warn_style(),
        ToastKind::Error => Style::default().fg(theme.crit).add_modifier(Modifier::BOLD),
    }
}

#[must_use]
pub fn footer_hit(view: &AppView<'_>, area: Rect, col: u16, row: u16) -> Option<Hit> {
    footer_target(view, area, col, row).map(|(_, hit)| hit)
}

/// The footer button under the pointer and its rect: the brand, a busy
/// verdict, or a chip. Clicks and hover both read this.
#[must_use]
pub(super) fn footer_target(
    view: &AppView<'_>,
    area: Rect,
    col: u16,
    row: u16,
) -> Option<(Rect, Hit)> {
    if row != area.y || view.zen_quiet() {
        return None;
    }
    let footer = layout(view, area);
    let on = |r: Rect| col >= r.x && col < r.x.saturating_add(r.width);
    if let Some((r, _)) = footer.brand.filter(|(r, _)| on(*r)) {
        return Some((r, Hit::Menu));
    }
    if let Some(r) = footer.update.filter(|r| on(*r)) {
        return Some((r, Hit::CheckUpdate));
    }
    if let Some((r, _)) = footer.verdict.as_ref().filter(|(r, _)| on(*r))
        && view.verdict.is_some_and(|v| v.pid.is_some())
    {
        return Some((*r, Hit::Verdict));
    }
    chips::target(&footer.chips, col, row)
}

/// macOS wording: TERM is "Quit", KILL is "Force quit".
#[must_use]
pub(super) fn signal_verb(signal: &str) -> &str {
    match signal {
        "" | "TERM" => "Quit",
        "KILL" => "Force quit",
        "INT" => "Interrupt",
        other => other,
    }
}

/// The confirmation question, fitted to `room` cells. The names are cut
/// first; the pid and the closing `?` always survive.
fn confirm_who(view: &AppView<'_>, room: usize) -> String {
    let verb = signal_verb(view.confirm_signal);
    let name_of = |pid: u32| {
        view.snapshot
            .processes
            .iter()
            .find(|p| p.pid == pid)
            .map(|p| p.name.as_str())
            .filter(|n| !n.is_empty())
    };
    let (head, tail) = if view.proc.armed.len() > 1 {
        // A set (tags or an app group): name them, each name once with its
        // count, so five helpers read as one name ×5.
        let mut names: Vec<(&str, usize)> = Vec::new();
        for name in view.proc.armed.iter().filter_map(|pid| name_of(*pid)) {
            match names.iter_mut().find(|(seen, _)| *seen == name) {
                Some((_, count)) => *count += 1,
                None => names.push((name, 1)),
            }
        }
        let names: Vec<String> = names
            .into_iter()
            .map(|(name, count)| match count {
                1 => name.to_owned(),
                n => format!("{name} ×{n}"),
            })
            .collect();
        let count = view.proc.armed.len();
        let critical = match view.confirm_critical {
            0 => String::new(),
            n => format!(" ({n} critical: can log you out)"),
        };
        (
            format!(" {verb} {count} processes{critical}: {}", names.join(", ")),
            String::from("?"),
        )
    } else {
        match view.confirm_pid {
            None => (String::new(), format!(" {verb}?")),
            Some(pid) => match name_of(pid) {
                Some(name) => (format!(" {verb} {name}"), format!(" ({pid})?")),
                None => (String::new(), format!(" {verb} pid {pid}?")),
            },
        }
    };
    let room_for_head = room.saturating_sub(tail.width());
    let question = if room_for_head == 0 && !head.is_empty() {
        ellipsize(&format!("{head}{tail}"), room)
    } else {
        format!("{}{tail}", ellipsize(&head, room_for_head))
    };
    // A lone critical target says why after the `?`; that reason is cut
    // before anything in the question itself.
    if view.proc.armed.len() <= 1 && view.confirm_critical > 0 {
        let left = room.saturating_sub(question.width());
        return format!(
            "{question}{}",
            truncate(" critical: ending it can log you out", left)
        );
    }
    question
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::layout::Panel;
    use crate::widgets::Focus;
    use crate::widgets::tests_support::{Fixture, fixture, process};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use siltop_core::{PowerSource, Surface, TimeRemaining, Verdict, VerdictKind};

    fn painted(view: &AppView<'_>, width: u16) -> String {
        painted_on(view, width, 1)
    }

    /// The footer as the last row of a `width` x `height` window.
    fn painted_on(view: &AppView<'_>, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let y = height - 1;
        terminal
            .draw(|frame| render(frame, Rect::new(0, y, width, 1), view, &Theme::default()))
            .unwrap();
        let buf = terminal.backend().buffer();
        (0..width).map(|x| buf[(x, y)].symbol()).collect()
    }

    fn contexts() -> Vec<(&'static str, Fixture)> {
        let mut out = Vec::new();
        let mut home = fixture("");
        home.focus = Focus::Processes;
        out.push(("home", home));
        let mut glance = fixture("");
        glance.surface = Surface::Glance;
        out.push(("glance", glance));
        let mut expanded = fixture("");
        expanded.expanded = Some(Panel::Cpu);
        out.push(("expanded", expanded));
        let mut detail = fixture("");
        detail.detail_pid = Some(904);
        out.push(("detail", detail));
        let mut frozen = fixture("");
        frozen.frozen = true;
        out.push(("frozen", frozen));
        let mut confirm = fixture("");
        confirm.confirm_kill = true;
        confirm.confirm_pid = Some(904);
        out.push(("confirm", confirm));
        let mut search = fixture("xc");
        search.searching = true;
        search.focus = Focus::Search;
        out.push(("search", search));
        out
    }

    #[test]
    fn rv_a_stopped_sampler_is_not_shown_as_live() {
        let fx = fixture("");
        let mut view = fx.view();
        view.stopped = true;
        let wide = painted(&view, 200);
        assert!(wide.contains("■ stopped · restart siltop"), "{wide}");
        assert!(!wide.contains('●'), "no live cadence marker: {wide}");
        view.frozen = true;
        assert!(
            painted(&view, 200).contains("■ stopped"),
            "stopped outranks paused"
        );
    }

    #[test]
    fn rv_battery_saver_names_why_the_cadence_is_slower() {
        let fx = fixture("");
        let mut view = fx.view();
        view.interval_ms = 2000;
        view.battery_saver = true;
        let wide = painted(&view, 200);
        assert!(wide.contains("● 2 s · battery saver"), "{wide}");
        view.battery_saver = false;
        let plain = painted(&view, 200);
        assert!(
            plain.contains("● 2 s") && !plain.contains("saver"),
            "{plain}"
        );
    }

    #[test]
    fn clicking_the_centre_of_every_drawn_chip_runs_its_action() {
        for width in [60_u16, 80, 120, 200] {
            for (name, fx) in contexts() {
                let view = fx.view();
                let area = Rect::new(0, 0, width, 1);
                let text = painted(&view, width);
                let chips = chips::footer_chips(&view, area);
                let mut found = 0;
                for chip in &chips {
                    let label = format!("{} {}", chip.key, chip.verb);
                    let Some(at) = text.find(&label) else {
                        assert!(
                            !chip.essential || name == "confirm",
                            "{name} {width}: {label} missing in {text:?}"
                        );
                        continue;
                    };
                    found += 1;
                    let col = u16::try_from(text[..at].chars().count()).unwrap();
                    let centre = col + u16::try_from(label.chars().count()).unwrap() / 2;
                    assert_eq!(
                        footer_hit(&view, area, centre, 0),
                        Some(chip.action),
                        "{name} {width}: centre of {label:?} in {text:?}"
                    );
                }
                assert!(found >= 2, "{name} {width}: {text:?}");
            }
        }
    }

    fn busy() -> Verdict {
        Verdict {
            kind: VerdictKind::Busy,
            headline: String::from("Busy: Xcode using 3.1 cores"),
            evidence: Vec::new(),
            pid: Some(904),
        }
    }

    #[test]
    fn brand_opens_the_menu_wherever_it_is_drawn() {
        for width in 20..=220_u16 {
            let fx = fixture("");
            let view = fx.view();
            let text = painted(&view, width);
            let area = Rect::new(0, 0, width, 1);
            let label = if width >= BRAND_MIN_WIDTH {
                BRAND
            } else {
                "≡"
            };
            let at = text
                .find(label)
                .unwrap_or_else(|| panic!("{width}: {text:?}"));
            let col = u16::try_from(text[..at].chars().count()).unwrap();
            for c in col..col + width_of(label) {
                assert_eq!(footer_hit(&view, area, c, 0), Some(Hit::Menu), "{width}");
            }
            assert_ne!(footer_hit(&view, area, 0, 0), Some(Hit::Menu), "{width}");
        }
    }

    #[test]
    fn home_footer_offers_settings() {
        let fx = fixture("");
        let text = painted(&fx.view(), 160);
        assert!(text.contains("s settings"), "{text:?}");
    }

    /// Regression (owner feedback): nothing on the footer overlaps, and
    /// every chip is clicked where it is painted, at every width.
    #[test]
    fn footer_paint_and_hit_agree_for_every_chip_at_every_width() {
        for width in 20..=220_u16 {
            for (name, fx) in contexts() {
                let mut view = fx.view();
                let verdict = busy();
                view.verdict = Some(&verdict);
                let area = Rect::new(0, 0, width, 1);
                let footer = layout(&view, area);
                let mut spans: Vec<Rect> = footer.chips.iter().map(|(r, _)| *r).collect();
                spans.extend(footer.brand.map(|(r, _)| r));
                spans.extend(footer.verdict.as_ref().map(|(r, _)| *r));
                if let Some((x, line)) = &footer.status {
                    let w = u16::try_from(line.width()).unwrap();
                    spans.push(Rect::new(*x, 0, w, 1));
                    assert!(x + w <= width, "{name} {width}: status overflows");
                }
                spans.sort_by_key(|r| r.x);
                // Regression: at 60 columns `q quit AC 80%` ran together.
                for pair in spans.windows(2) {
                    assert!(
                        pair[0].x + pair[0].width + GAP <= pair[1].x,
                        "{name} {width}: {pair:?}"
                    );
                }
                let text = painted(&view, width);
                let cells: Vec<&str> = (0..width)
                    .map(|x| {
                        let at = usize::from(x);
                        text.char_indices()
                            .nth(at)
                            .map_or("", |(i, c)| &text[i..i + c.len_utf8()])
                    })
                    .collect();
                for (rect, chip) in &footer.chips {
                    let drawn: String =
                        cells[usize::from(rect.x)..usize::from(rect.x + rect.width)].concat();
                    assert_eq!(
                        drawn,
                        format!("{} {}", chip.key, chip.verb),
                        "{name} {width}"
                    );
                    for c in rect.x..rect.x + rect.width {
                        assert_eq!(
                            footer_hit(&view, area, c, 0),
                            Some(chip.action),
                            "{name} {width}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn verdict_slot_drops_before_the_status() {
        let fx = fixture("");
        let mut view = fx.view();
        let verdict = busy();
        view.verdict = Some(&verdict);
        let wide = painted(&view, 200);
        assert!(wide.contains("Busy: Xcode using 3.1 cores"), "{wide:?}");
        assert!(wide.contains("● 1 s"), "{wide:?}");
        let mut saw_status_without_verdict = false;
        for width in 20..=200_u16 {
            let text = painted(&view, width);
            let has_verdict = text.contains("Busy:");
            let has_status = text.contains("● 1 s");
            assert!(
                !has_verdict || has_status,
                "{width}: verdict outlived status {text:?}"
            );
            saw_status_without_verdict |= has_status && !has_verdict;
        }
        assert!(saw_status_without_verdict);
    }

    #[test]
    fn busy_verdict_click_selects_and_normal_verdict_is_inert() {
        let fx = fixture("");
        let mut view = fx.view();
        let verdict = busy();
        view.verdict = Some(&verdict);
        let text = painted(&view, 200);
        let at = u16::try_from(text.find("Busy:").unwrap()).unwrap();
        let area = Rect::new(0, 0, 200, 1);
        assert_eq!(footer_hit(&view, area, at + 2, 0), Some(Hit::Verdict));
        let normal = Verdict::normal();
        view.verdict = Some(&normal);
        let text = painted(&view, 200);
        let at = u16::try_from(text.find("All normal").unwrap()).unwrap();
        assert_eq!(footer_hit(&view, area, at + 2, 0), None);
    }

    #[test]
    fn verdict_ink_follows_its_severity() {
        let theme = Theme::default();
        assert_eq!(verdict_style(&Verdict::normal(), &theme), theme.dim());
        assert_eq!(verdict_style(&busy(), &theme), theme.warn_style());
        let hot = Verdict {
            kind: VerdictKind::Throttling,
            ..busy()
        };
        assert_eq!(verdict_style(&hot, &theme).fg, Some(theme.crit));
    }

    #[test]
    fn a_toast_takes_the_verdict_slot() {
        let fx = fixture("");
        let mut view = fx.view();
        let verdict = busy();
        view.verdict = Some(&verdict);
        view.status = Some("saved");
        let text = painted(&view, 200);
        assert!(
            text.contains("saved") && !text.contains("Busy:"),
            "{text:?}"
        );
    }

    #[test]
    fn desktops_show_no_power_token_and_laptops_do() {
        let mut fx = fixture("");
        fx.snap.battery.present = false;
        let text = painted(&fx.view(), 200);
        assert!(!text.contains("AC") && !text.contains('%'), "{text:?}");
        fx.snap.battery.present = true;
        fx.snap.battery.source = PowerSource::Battery;
        fx.snap.battery.percent = Some(74);
        fx.snap.battery.watts = Some(-11.0);
        fx.snap.battery.time_remaining =
            TimeRemaining::Known(std::time::Duration::from_mins(3 * 60 + 12));
        let text = painted(&fx.view(), 200);
        assert!(text.contains("74% ▼ 11W 3h12m  ● 1 s"), "{text:?}");
        fx.snap.battery.source = PowerSource::Ac;
        fx.snap.battery.percent = Some(80);
        let text = painted(&fx.view(), 200);
        assert!(text.contains("AC 80%"), "{text:?}");
    }

    #[test]
    fn interval_labels() {
        assert_eq!(interval_label(250), "250 ms");
        assert_eq!(interval_label(1000), "1 s");
        assert_eq!(interval_label(2500), "2.5 s");
        assert_eq!(interval_label(0), "0 ms");
    }

    /// Regression: the status read `79% ▼ 7.1W 8h05m  7.1W  ● 500 ms`, the
    /// same draw twice. The watts token is the draw's one home.
    #[test]
    fn footer_says_the_battery_draw_once() {
        let mut fx = fixture("");
        fx.snap.battery.present = true;
        fx.snap.battery.source = PowerSource::Battery;
        fx.snap.battery.percent = Some(79);
        fx.snap.battery.watts = Some(-7.1);
        fx.snap.battery.time_remaining =
            TimeRemaining::Known(std::time::Duration::from_mins(8 * 60 + 5));
        fx.snap.power.system = Some(siltop_core::Reading::measured(7.1));
        for width in 40..=200_u16 {
            let text = painted(&fx.view(), width);
            assert!(text.matches("7.1W").count() <= 1, "{width}: {text:?}");
        }
        let text = painted(&fx.view(), 200);
        assert!(text.contains("79% ▼ 8h05m  7.1W  ● 1 s"), "{text:?}");
        // without a system reading the battery token keeps the draw
        fx.snap.power.system = None;
        let text = painted(&fx.view(), 200);
        assert!(text.contains("79% ▼ 7.1W 8h05m  ● 1 s"), "{text:?}");
    }

    /// Regression: the footer used `> 0.05` for the battery and `> 0.0`
    /// for system watts while the power box used `>= 0.05`, so a 0.03 W
    /// reading printed `0.0W` here and nothing there.
    #[test]
    fn footer_and_power_box_share_one_idle_floor() {
        let mut fx = fixture("");
        fx.snap.battery.present = true;
        fx.snap.battery.source = PowerSource::Battery;
        fx.snap.battery.percent = Some(79);
        fx.snap.battery.watts = Some(-0.05);
        let text = painted(&fx.view(), 200);
        assert!(
            text.contains("79% ▼ 0.1W"),
            "the floor itself shows: {text:?}"
        );
        fx.snap.battery.watts = None;
        fx.snap.power.system = Some(siltop_core::Reading::measured(0.03));
        let text = painted(&fx.view(), 200);
        assert!(!text.contains("0.0W"), "an idle rail is absent: {text:?}");
    }

    #[test]
    fn narrow_status_drops_power_before_the_sampling_state() {
        let mut fx = fixture("");
        fx.snap.battery.present = true;
        fx.snap.battery.source = PowerSource::Battery;
        fx.snap.battery.percent = Some(74);
        fx.snap.battery.watts = Some(-11.0);
        for width in 20..=200_u16 {
            let text = painted(&fx.view(), width);
            if text.contains("74%") {
                assert!(text.contains("● 1 s"), "{width}: {text:?}");
            }
        }
        let text = painted(&fx.view(), 60);
        assert!(text.contains("● 1 s"), "{text:?}");
    }

    #[test]
    fn help_and_quit_are_always_present_outside_confirm() {
        for (name, fx) in contexts() {
            if name == "confirm" {
                continue;
            }
            let text = painted(&fx.view(), 60);
            assert!(text.contains("? help"), "{name}: {text:?}");
            // in the dossier `q` closes the card, so its footer says that
            let way_out = if name == "detail" {
                "esc close"
            } else {
                "q quit"
            };
            assert!(text.contains(way_out), "{name}: {text:?}");
        }
    }

    #[test]
    fn expanded_footer_hits_the_chip_it_draws() {
        let mut fx = fixture("");
        fx.expanded = Some(Panel::Cpu);
        fx.snap.sensors.cpu_c = Some(42.0);
        let view = fx.view();
        let text = painted(&view, 120);
        let esc = u16::try_from(text.find("esc home").unwrap()).unwrap();
        let tab = u16::try_from(text.find("tab related").unwrap()).unwrap();
        let area = Rect::new(0, 0, 120, 1);
        assert_eq!(footer_hit(&view, area, esc + 1, 0), Some(Hit::ExpandClose));
        assert_eq!(footer_hit(&view, area, tab + 1, 0), Some(Hit::HopNext));
    }

    #[test]
    fn confirm_prompt_names_the_target_and_takes_clicks() {
        let mut fx = fixture("");
        fx.confirm_kill = true;
        fx.confirm_pid = Some(904);
        fx.confirm_signal = "KILL";
        let view = fx.view();
        let text = painted(&view, 80);
        assert!(text.contains("Force quit Xcode (904)?"), "{text:?}");
        let yes = u16::try_from(text.find("y yes").unwrap()).unwrap();
        let no = u16::try_from(text.find("n cancel").unwrap()).unwrap();
        let area = Rect::new(0, 0, 80, 1);
        assert_eq!(footer_hit(&view, area, yes + 2, 0), Some(Hit::ConfirmYes));
        assert_eq!(footer_hit(&view, area, no + 2, 0), Some(Hit::ConfirmNo));
    }

    /// Regression: a long armed set pushed the `?` and both y/n chips off the
    /// footer, so nothing said how to answer or cancel.
    #[test]
    fn confirm_for_a_set_keeps_its_question_and_answers_at_every_width() {
        let mut fx = fixture("");
        fx.snap.processes = (1..=5)
            .map(|pid| process(2240 + pid, "Brave Browser Helper (Renderer)", 1.0))
            .collect();
        fx.proc.armed = (1..=5).map(|pid| 2240 + pid).collect();
        fx.confirm_kill = true;
        let view = fx.view();
        for width in [40_u16, 60, 80, 100, 120, 160] {
            let text = painted(&view, width);
            assert!(text.contains("Quit 5 processes: B"), "{width}: {text:?}");
            assert!(text.contains("?  y yes  n cancel"), "{width}: {text:?}");
            let area = Rect::new(0, 0, width, 1);
            let yes = u16::try_from(text.find("y yes").unwrap()).unwrap();
            let no = u16::try_from(text.find("n cancel").unwrap()).unwrap();
            assert_eq!(footer_hit(&view, area, yes, 0), Some(Hit::ConfirmYes));
            assert_eq!(footer_hit(&view, area, no, 0), Some(Hit::ConfirmNo));
        }
        let wide = painted(&view, 120);
        assert!(
            wide.contains("Brave Browser Helper (Renderer) ×5?"),
            "{wide:?}"
        );
    }

    #[test]
    fn confirm_for_one_long_name_keeps_its_pid_and_answers() {
        let mut fx = fixture("");
        fx.snap.processes = vec![process(2240, "com.apple.WebKit.WebContent.xpc.helper", 1.0)];
        fx.confirm_kill = true;
        fx.confirm_pid = Some(2240);
        fx.confirm_signal = "KILL";
        let text = painted(&fx.view(), 60);
        assert!(text.contains("… (2240)?  y yes  n cancel"), "{text:?}");
        assert!(text.starts_with(" Force quit com.apple"), "{text:?}");
    }

    #[test]
    fn confirm_prompt_warns_about_a_critical_process() {
        let mut fx = fixture("");
        fx.confirm_kill = true;
        fx.confirm_pid = Some(904);
        let view = AppView {
            confirm_critical: 1,
            ..fx.view()
        };
        let text = painted(&view, 100);
        assert!(
            text.contains("Quit Xcode (904)? critical: ending it can log you out"),
            "{text:?}"
        );
        assert!(text.contains("y yes"), "{text:?}");
        let text = painted(&fx.view(), 100);
        assert!(!text.contains("critical"), "{text:?}");
    }

    #[test]
    fn a_long_confirm_prompt_is_cut_before_the_yes_and_cancel_chips() {
        let mut fx = fixture("");
        fx.confirm_kill = true;
        fx.confirm_pid = Some(904);
        let view = AppView {
            confirm_critical: 1,
            ..fx.view()
        };
        let text = painted(&view, 60);
        assert!(text.contains("Quit Xcode (904)? critical"), "{text:?}");
        let yes = u16::try_from(text.find("y yes").unwrap()).unwrap();
        let no = u16::try_from(text.find("n cancel").unwrap()).unwrap();
        let area = Rect::new(0, 0, 60, 1);
        assert_eq!(footer_hit(&view, area, yes + 2, 0), Some(Hit::ConfirmYes));
        assert_eq!(footer_hit(&view, area, no + 2, 0), Some(Hit::ConfirmNo));
    }

    #[test]
    fn toast_sits_right_and_keeps_the_chips() {
        let mut fx = fixture("");
        fx.focus = Focus::Cpu;
        let mut view = fx.view();
        view.status = Some("sent TERM to Xcode (904)");
        let text = painted(&view, 80);
        assert!(
            text.contains("? help") && text.contains("q quit"),
            "{text:?}"
        );
        let toast = text.find("sent TERM to Xcode (904)").unwrap();
        let segment = text.find("● ").unwrap();
        assert!(toast < segment, "toast sits left of the segment: {text:?}");
        assert!(text.trim_end().ends_with("● 1 s"), "{text:?}");
    }

    #[test]
    fn kill_chip_only_over_the_process_table() {
        let mut fx = fixture("");
        fx.focus = Focus::Processes;
        assert!(painted(&fx.view(), 80).contains("x kill"));
        fx.focus = Focus::Mem;
        assert!(!painted(&fx.view(), 80).contains("x kill"));
    }

    #[test]
    fn paused_and_work_chips_are_clickable() {
        let mut fx = fixture("");
        fx.frozen = true;
        fx.surface = Surface::Glance;
        let view = fx.view();
        let text = painted_on(&view, 80, 24);
        let area = Rect::new(0, 23, 80, 1);
        let at = |label: &str| u16::try_from(text.find(label).unwrap()).unwrap();
        assert_eq!(
            footer_hit(&view, area, at("p resume") + 2, 23),
            Some(Hit::Pause)
        );
        assert_eq!(
            footer_hit(&view, area, at("v work") + 2, 23),
            Some(Hit::Work)
        );
    }
}
