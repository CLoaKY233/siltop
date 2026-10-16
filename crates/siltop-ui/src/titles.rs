//! One title builder per panel. Compact cells, expanded views and Glance all
//! read the same token list, and [`Title::fit`] is the only place a title is
//! cut to width: whole tokens go, lowest priority first, and a `…` says so.
//! A number is never split.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use siltop_core::{
    FanMetric, NetUnit, Pressure, byte_rate, bytes_short, celsius, net_rate, percent_display,
    ready_pct, watts_display,
};

use crate::layout::Panel;
use crate::theme::Theme;
use crate::widgets::AppView;
use crate::widgets::power::shows_watts;

/// Columns the two rounded corners and the `[↗]` mark take around a title.
pub(crate) const TITLE_CHROME: usize = 5;

/// Gap between tokens.
const GAP: &str = "  ";
/// Shown after the last kept token when anything was dropped.
const MORE: &str = "…";

/// Fixed widths so a token keeps its place while its digits change.
const PCT_W: usize = 4;
const TEMP_W: usize = 3;
const WATT_W: usize = 4;
const RATE_W: usize = 8;
/// `1398MHz`: the widest GPU clock token.
const MHZ_W: usize = 7;

/// How hard a token holds on when the title is too wide. Lower goes first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Keep {
    Low,
    Mid,
    High,
    Top,
    /// The headline: dropped only when nothing else is left to drop.
    Always,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Token {
    pub text: String,
    /// A narrower wording tried before the token is dropped (`peak 71°` → `71°`).
    pub short: Option<String>,
    /// Columns the token is right-aligned in while there is room, so its
    /// neighbours stay put as digits change. 0 for words.
    pub pad: usize,
    pub style: Style,
    pub keep: Keep,
}

impl Token {
    fn shown(&self, form: Form) -> Option<&str> {
        match form {
            Form::Full => Some(&self.text),
            Form::Short => Some(self.short.as_deref().unwrap_or(&self.text)),
            Form::Hidden => None,
        }
    }
}

/// How a token appears in a fitted title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    Full,
    Short,
    Hidden,
}

/// A fitted layout: each token's form, and whether fixed widths are kept.
struct Fitted {
    forms: Vec<Form>,
    padded: bool,
    /// The `…` marker; the headline outranks it when space runs out.
    more: bool,
}

/// A panel label and its prioritised value tokens, in display order.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Title {
    label: &'static str,
    tokens: Vec<Token>,
    /// No label, padding or `…`: a bare run of tokens such as the footer's
    /// status segment, fitted by the same rules as every title.
    bare: bool,
}

impl Title {
    #[must_use]
    pub(crate) fn new(label: &'static str) -> Self {
        Self {
            label,
            tokens: Vec::new(),
            bare: false,
        }
    }

    /// A label-less token run: [`Title::fit`] keeps the richest set that
    /// fits, with no frame padding and no `…`.
    #[must_use]
    pub(crate) fn bare() -> Self {
        Self {
            bare: true,
            ..Self::new("")
        }
    }

    /// Adds a word token; empty text is ignored so optional facts stay absent.
    pub(crate) fn push(&mut self, keep: Keep, text: impl Into<String>, style: Style) -> &mut Self {
        self.push_token(keep, text.into(), None, 0, style)
    }

    /// Adds a number right-aligned in `pad` columns while there is room.
    pub(crate) fn push_fixed(
        &mut self,
        keep: Keep,
        text: impl Into<String>,
        pad: usize,
        style: Style,
    ) -> &mut Self {
        self.push_token(keep, text.into(), None, pad, style)
    }

    /// Adds a token with a narrower fallback wording.
    pub(crate) fn push_short(
        &mut self,
        keep: Keep,
        text: impl Into<String>,
        short: impl Into<String>,
        style: Style,
    ) -> &mut Self {
        self.push_token(keep, text.into(), Some(short.into()), 0, style)
    }

    fn push_token(
        &mut self,
        keep: Keep,
        text: String,
        short: Option<String>,
        pad: usize,
        style: Style,
    ) -> &mut Self {
        if !text.trim().is_empty() {
            self.tokens.push(Token {
                text,
                short,
                pad,
                style,
                keep,
            });
        }
        self
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    /// Every token, however wide, at its fixed width.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn full(&self, theme: &Theme) -> Line<'static> {
        self.render(&self.everything(), theme)
    }

    /// The richest line that fits in `budget` columns.
    #[must_use]
    pub(crate) fn fit(&self, budget: usize, theme: &Theme) -> Line<'static> {
        self.render(&self.fitted(budget), theme)
    }

    /// Whether every token ranked `floor` or above survives, in full, in
    /// `budget` columns.
    #[must_use]
    pub(crate) fn keeps(&self, budget: usize, floor: Keep) -> bool {
        self.fitted(budget)
            .forms
            .iter()
            .zip(&self.tokens)
            .all(|(form, t)| *form == Form::Full || t.keep < floor)
    }

    fn everything(&self) -> Fitted {
        Fitted {
            forms: vec![Form::Full; self.tokens.len()],
            padded: true,
            more: !self.bare,
        }
    }

    /// How each token appears in `budget` columns. Fixed widths go first;
    /// then the weakest visible token (latest first among equals) gives way
    /// one step at a time: to its short wording if it has one, then out.
    fn fitted(&self, budget: usize) -> Fitted {
        let mut fit = self.everything();
        if self.width(&fit) <= budget {
            return fit;
        }
        fit.padded = false;
        while self.width(&fit) > budget {
            let victim = fit
                .forms
                .iter()
                .enumerate()
                .filter(|(_, f)| **f != Form::Hidden)
                .map(|(i, _)| i)
                .min_by_key(|&i| (self.tokens[i].keep, std::cmp::Reverse(i)));
            let Some(i) = victim else { break };
            if self.tokens[i].keep == Keep::Always && fit.more {
                fit.more = false;
                continue;
            }
            fit.forms[i] = match (fit.forms[i], &self.tokens[i].short) {
                (Form::Full, Some(_)) => Form::Short,
                _ => Form::Hidden,
            };
        }
        fit
    }

    /// Visible tokens with the columns each takes.
    fn visible<'a>(&'a self, fit: &'a Fitted) -> impl Iterator<Item = (&'a Token, &'a str, usize)> {
        self.tokens
            .iter()
            .zip(&fit.forms)
            .filter_map(move |(t, f)| {
                let text = t.shown(*f)?;
                let cols = text.chars().count();
                let cols = if fit.padded { cols.max(t.pad) } else { cols };
                Some((t, text, cols))
            })
    }

    fn width(&self, fit: &Fitted) -> usize {
        let body: usize = self.visible(fit).map(|(_, _, cols)| GAP.len() + cols).sum();
        if self.bare {
            return body.saturating_sub(GAP.len());
        }
        let more = if fit.more && fit.forms.contains(&Form::Hidden) {
            2
        } else {
            0
        };
        // " label" + tokens + " …" + one trailing space
        1 + self.label.chars().count() + body + more + 1
    }

    fn render(&self, fit: &Fitted, theme: &Theme) -> Line<'static> {
        let mut spans = Vec::new();
        if !self.bare {
            spans.push(Span::styled(format!(" {}", self.label), theme.dim()));
        }
        for (i, (token, text, cols)) in self.visible(fit).enumerate() {
            if i > 0 || !self.bare {
                spans.push(Span::raw(GAP));
            }
            spans.push(Span::styled(format!("{text:>cols$}"), token.style));
        }
        if self.bare {
            return Line::from(spans);
        }
        if fit.more && fit.forms.contains(&Form::Hidden) {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(MORE, theme.dim()));
        }
        spans.push(Span::raw(" "));
        Line::from(spans)
    }
}

/// Columns a title may use inside a frame `width` wide that also carries
/// `reserve` columns of right-hand labels (axis ceiling).
#[must_use]
pub(crate) fn budget(width: u16, reserve: usize) -> usize {
    usize::from(width).saturating_sub(TITLE_CHROME + reserve)
}

/// [`budget`] for `panel`'s frame, which also leaves room for the icon
/// [`crate::chrome::panel_block`] puts in front of the title.
#[must_use]
pub(crate) fn panel_budget(panel: Panel, width: u16, reserve: usize, theme: &Theme) -> usize {
    budget(width, reserve + theme.icons.panel_width(panel))
}

/// The builder for `panel`'s compact / Glance title.
#[must_use]
pub(crate) fn for_panel(panel: Panel, view: &AppView<'_>, theme: &Theme) -> Title {
    match panel {
        Panel::Cpu => cpu(view, theme),
        Panel::Gpu => gpu(view, theme),
        Panel::Mem => mem(view, theme),
        Panel::Net => net(view, theme),
        Panel::Disk => disk(view, theme),
        Panel::Fans => heat(view, theme),
        Panel::Power => power(view, theme),
        Panel::Processes => proc(view, view.snapshot.processes.len(), 0, theme),
    }
}

/// `power  38W  cpu 18W  gpu 4W  80% held`: system watts headline, the
/// rails behind it, then the battery. Held or estimated values read `≈`.
#[must_use]
pub(crate) fn power(view: &AppView<'_>, theme: &Theme) -> Title {
    use crate::widgets::power as p;
    let snap = view.snapshot;
    let mut t = Title::new("power");
    let ink = |w: &p::Watts, live: Style| if w.approx { theme.dim() } else { live };
    let system = p::watts(snap.power.system);
    if let Some(w) = &system {
        // A narrow box keeps the draw as whole watts (`8W`) over nothing.
        let whole = snap
            .power
            .system
            .and_then(siltop_core::Reading::usable)
            .map(|watts| format!("{}{:.0}W", if w.approx { "≈" } else { "" }, watts.round()));
        let short = whole.filter(|whole| whole.chars().count() < w.text.chars().count());
        t.push_token(
            Keep::Always,
            w.text.clone(),
            short,
            WATT_W,
            ink(w, theme.title()),
        );
    }
    if let Some(age) = p::held_age(snap.power.system) {
        t.push(Keep::Low, age, theme.dim());
    }
    for (i, (name, w)) in p::breakdown(&snap.power).into_iter().enumerate() {
        let keep = if i == 0 { Keep::Mid } else { Keep::Low };
        t.push(keep, format!("{name} {}", w.text), ink(&w, theme.fg()));
    }
    if let Some(bat) = p::battery_summary(&snap.battery, p::Flow::beside_system(system.is_some())) {
        let low = snap.battery.state == siltop_core::ChargeState::Discharging
            && snap.battery.percent.is_some_and(|p| p <= 20);
        let style = if low { theme.warn_style() } else { theme.fg() };
        t.push(Keep::Top, bat, style);
    }
    t
}

/// A rate token: `↓1.2MB/s`, with zero as a dim `↓0`.
/// A narrow box keeps the number and its prefix (`↓1.2M`) before it drops
/// the token: the box's own label says what is counted.
fn rate_token(title: &mut Title, keep: Keep, arrow: &str, text: &str, live: Style, theme: &Theme) {
    let style = if text == "0" { theme.dim() } else { live };
    let pad = RATE_W + arrow.chars().count();
    // Only the headline: a title never mixes `↓155KB/s` with `↑11K`.
    let short = ["B/s", "b/s"]
        .iter()
        .find_map(|unit| text.strip_suffix(unit))
        .filter(|_| keep == Keep::Always)
        .map(|head| format!("{arrow}{head}"));
    title.push_token(keep, format!("{arrow}{text}"), short, pad, style);
}

/// CPU die temperature: the cpu title is its one home.
#[must_use]
pub(crate) fn cpu_temp_c(view: &AppView<'_>) -> Option<f32> {
    view.snapshot
        .cpu
        .temp_c
        .or_else(|| view.snapshot.sensors.best_cpu_c())
}

#[must_use]
pub(crate) fn gpu_temp_c(view: &AppView<'_>) -> Option<f32> {
    view.snapshot
        .gpu
        .and_then(|g| g.temp_c)
        .or_else(|| view.snapshot.sensors.named_gpu_c())
}

/// The dim `busy` token appears only when time-busy and frequency-weighted
/// busy tell different stories (more than 10 points apart).
pub(crate) const BUSY_GAP: f32 = 0.10;

#[must_use]
pub(crate) fn cpu(view: &AppView<'_>, theme: &Theme) -> Title {
    let cpu = &view.snapshot.cpu;
    let mut t = Title::new("cpu");
    t.push_fixed(
        Keep::Always,
        ready_pct(view.ready, cpu.scaled),
        PCT_W,
        theme.title(),
    );
    if view.ready {
        if let Some(clocks) = cluster_clocks(cpu) {
            t.push(Keep::Low, clocks, theme.dim());
        }
        if (cpu.scaled - cpu.active).abs() > BUSY_GAP {
            t.push(
                Keep::Mid,
                format!("busy {}", percent_display(cpu.active)),
                theme.dim(),
            );
        }
        if let Some(w) = cpu.watts.filter(|w| shows_watts(*w)) {
            t.push_fixed(Keep::High, watts_display(w), WATT_W, theme.cpu());
        }
    }
    if let Some(c) = cpu_temp_c(view) {
        t.push_fixed(Keep::Top, celsius(c), TEMP_W, theme.temp(c));
    }
    if let Some(word) = view.snapshot.thermal.word() {
        t.push(Keep::Top, word, theme.thermal(view.snapshot.thermal));
    }
    t
}

#[must_use]
pub(crate) fn gpu(view: &AppView<'_>, theme: &Theme) -> Title {
    let mut t = Title::new("gpu");
    match view.snapshot.gpu {
        None => {
            t.push(Keep::Always, "—", theme.dim());
        }
        Some(gpu) => gpu_load_tokens(&mut t, view, gpu, theme),
    }
    if let Some(c) = gpu_temp_c(view) {
        t.push_fixed(Keep::Top, celsius(c), TEMP_W, theme.temp(c));
    }
    t
}

fn gpu_load_tokens(
    t: &mut Title,
    view: &AppView<'_>,
    gpu: siltop_core::GpuSnapshot,
    theme: &Theme,
) {
    t.push_fixed(
        Keep::Always,
        ready_pct(view.ready, gpu.scaled),
        PCT_W,
        theme.title(),
    );
    if view.ready {
        if let Some(mhz) = gpu.freq_mhz.filter(|mhz| *mhz > 0) {
            t.push_fixed(Keep::Low, format!("{mhz}MHz"), MHZ_W, theme.dim());
        }
        if let Some(busy) = gpu.renderer_busy
            && (busy - gpu.scaled).abs() > BUSY_GAP
        {
            t.push(
                Keep::Mid,
                format!("busy {}", percent_display(busy)),
                theme.dim(),
            );
        }
        if let Some(w) = gpu.watts.filter(|w| shows_watts(*w)) {
            t.push_fixed(Keep::High, watts_display(w), WATT_W, theme.gpu());
        }
    }
}

/// `17.1G/24.0G`: RAM in binary G like Activity Monitor, one token so the
/// pair is never split.
#[must_use]
pub(crate) fn mem(view: &AppView<'_>, theme: &Theme) -> Title {
    let m = &view.snapshot.memory;
    let mut t = Title::new("mem");
    let used = bytes_short(m.used_bytes);
    // Padded by the fitter, not the format string, so a tight title drops
    // the pad with every other number's (`mem  9.6G/24.0G`, never a
    // stray leading space).
    let pair = format!("{used}/{}", bytes_short(m.total_bytes));
    let pad = pair.chars().count() + 5_usize.saturating_sub(used.chars().count());
    t.push_token(Keep::Always, pair, Some(used), pad, theme.title());
    if m.pressure != Pressure::Nominal {
        t.push_short(
            Keep::Top,
            format!("● {}", m.pressure.label()),
            "●",
            theme.pressure(m.pressure),
        );
    }
    if let Some(swap) = swap_token(m) {
        t.push(Keep::Mid, swap, swap_style(m, theme));
    }
    t
}

/// Swap warns only while it matters: under memory pressure, or while it is
/// growing. A few hundred MB parked at nominal pressure is normal on macOS
/// and stays dim, so the title is quiet until the machine isn't.
fn swap_style(m: &siltop_core::MemorySnapshot, theme: &Theme) -> Style {
    if m.pressure != Pressure::Nominal || m.swap_trend() == Some(true) {
        theme.warn_style()
    } else {
        theme.dim()
    }
}

/// `swap 0.8G ↑`: only once swap holds something; the arrow says which way
/// it is moving this tick and is left off when idle.
#[must_use]
pub(crate) fn swap_token(m: &siltop_core::MemorySnapshot) -> Option<String> {
    if !m.swap_in_use() {
        return None;
    }
    let arrow = match m.swap_trend() {
        Some(true) => " ↑",
        Some(false) => " ↓",
        None => "",
    };
    Some(format!("swap {}{arrow}", bytes_short(m.swap_used_bytes)))
}

#[must_use]
pub(crate) fn net(view: &AppView<'_>, theme: &Theme) -> Title {
    let n = &view.snapshot.network;
    let mut t = Title::new("net");
    if !n.iface.is_empty() {
        t.push(Keep::Low, n.iface.clone(), theme.dim());
    }
    rate_token(
        &mut t,
        Keep::Always,
        "↓",
        &net_rate(n.rx_bps, view.net_unit),
        theme.title(),
        theme,
    );
    rate_token(
        &mut t,
        Keep::Top,
        "↑",
        &net_rate(n.tx_bps, view.net_unit),
        theme.title(),
        theme,
    );
    t
}

/// Read and write rates; capacity lives in the expanded view.
#[must_use]
pub(crate) fn disk(view: &AppView<'_>, theme: &Theme) -> Title {
    let d = &view.snapshot.disk;
    let mut t = Title::new("disk");
    if d.volumes.is_empty() {
        t.push(Keep::Always, "—", theme.dim());
        return t;
    }
    rate_token(
        &mut t,
        Keep::Always,
        "r ",
        &byte_rate(d.read_bps),
        theme.title(),
        theme,
    );
    rate_token(
        &mut t,
        Keep::Top,
        "w ",
        &byte_rate(d.write_bps),
        theme.title(),
        theme,
    );
    t
}

/// Fans the machine actually has; empty on fanless Macs.
#[must_use]
pub(crate) fn present_fans<'a>(view: &'a AppView<'_>) -> &'a [FanMetric] {
    if view.snapshot.fans.is_present() {
        &view.snapshot.fans.fans
    } else {
        &[]
    }
}

/// `1714rpm`: glued like every other unit (`12W`, `589MHz`, `58°`), so a fan
/// speed reads the same in the heat title and the expanded fan cells.
#[must_use]
pub(crate) fn rpm_display(rpm: u16) -> String {
    format!("{rpm}rpm")
}

/// `fans idle` when every fan is stopped, else `1714·1859rpm`. `None` on a
/// fanless Mac so no fan token is ever drawn there.
#[must_use]
pub(crate) fn fan_text(fans: &[FanMetric]) -> Option<String> {
    if fans.is_empty() {
        return None;
    }
    if fans.iter().all(|f| f.rpm == 0) {
        return Some(String::from("fans idle"));
    }
    let speeds: Vec<String> = fans.iter().map(|f| f.rpm.to_string()).collect();
    // one unit for the run: `1714·1859rpm`
    Some(format!("{}rpm", speeds.join("·")))
}

/// Hottest reading on the machine: the hotspot, named zones and every raw
/// sensor. The CPU die figure is left out; it lives in the cpu title.
#[must_use]
pub(crate) fn peak_c(view: &AppView<'_>) -> Option<f32> {
    let s = &view.snapshot.sensors;
    [s.hotspot_c, s.s_c, s.p_c, s.e_c, s.gpu_c]
        .into_iter()
        .flatten()
        .chain(s.readings.iter().map(|r| r.celsius))
        .max_by(f32::total_cmp)
}

/// Peak and GPU °C plus fans. CPU °C lives in the cpu title.
#[must_use]
pub(crate) fn heat(view: &AppView<'_>, theme: &Theme) -> Title {
    let mut t = Title::new("heat");
    if let Some(c) = peak_c(view) {
        t.push_short(
            Keep::Always,
            format!("peak {}", celsius(c)),
            celsius(c),
            theme.temp(c),
        );
    }
    if let Some(c) = view.snapshot.sensors.gpu_c {
        t.push(Keep::Mid, format!("gpu {}", celsius(c)), theme.temp(c));
    }
    if let Some(text) = fan_text(present_fans(view)) {
        let style = if text == "fans idle" {
            theme.dim()
        } else {
            theme.title()
        };
        t.push(Keep::Top, text, style);
    }
    if t.tokens.is_empty() {
        t.push(Keep::Always, "—", theme.dim());
    }
    t
}

/// `proc 909`, or `proc 22/909` while a filter hides some.
#[must_use]
pub(crate) fn proc(view: &AppView<'_>, shown: usize, offset: usize, theme: &Theme) -> Title {
    let total = view.snapshot.processes.len();
    let mut t = Title::new("proc");
    let count = if view.proc.filter.is_empty() {
        total.to_string()
    } else {
        format!("{shown}/{total}")
    };
    t.push(Keep::Always, count, theme.title());
    if offset > 0 {
        t.push(Keep::Low, format!("↑ {offset}"), theme.dim());
    }
    t
}

/// `1.3G`: one decimal of GHz.
#[must_use]
pub(crate) fn ghz_display(mhz: u32) -> String {
    format!("{:.1}G", f64::from(mhz) / 1000.0)
}

/// `S 4.3G · P 3.1G`: each cluster kind's effective clock, fastest kind
/// first. `None` when no cluster has a clock yet.
#[must_use]
pub(crate) fn cluster_clocks(cpu: &siltop_core::CpuSnapshot) -> Option<String> {
    let parts: Vec<String> = [&cpu.s_cluster, &cpu.p_cluster, &cpu.e_cluster]
        .into_iter()
        .flatten()
        .filter(|c| c.freq_mhz > 0)
        .map(|c| format!("{} {}", c.kind.tag(), ghz_display(c.freq_mhz)))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// Expanded outer frame: context, not numbers (the cells carry those).
#[must_use]
pub(crate) fn context(panel: Panel, view: &AppView<'_>, theme: &Theme) -> Title {
    let soc = &view.snapshot.soc;
    let mut t = Title::new(panel.label());
    match panel {
        Panel::Cpu => {
            // The core split lives in the meta column (widgets::identity).
            if let Some(word) = view.snapshot.thermal.word() {
                t.push(Keep::Top, word, theme.thermal(view.snapshot.thermal));
            }
            if let Some(line) = crate::widgets::power::throttle_line(&view.snapshot.power) {
                t.push(Keep::Top, line, theme.warn_style());
            }
        }
        Panel::Power => {
            if let Some(source) = crate::widgets::power::source_words(&view.snapshot.battery) {
                t.push(Keep::Top, source, theme.dim());
            }
        }
        Panel::Gpu if soc.gpu_cores > 0 => {
            t.push(Keep::Top, format!("{}-core", soc.gpu_cores), theme.dim());
        }
        Panel::Net if !view.snapshot.network.iface.is_empty() => {
            t.push(Keep::Top, view.snapshot.network.iface.clone(), theme.dim());
            t.push(
                Keep::Mid,
                match view.net_unit {
                    NetUnit::Bytes => "bytes/s",
                    NetUnit::Bits => "bits/s",
                },
                theme.dim(),
            );
        }
        Panel::Disk => {
            if let Some(vol) = view.snapshot.disk.primary() {
                t.push(Keep::Top, vol.name.clone(), theme.dim());
            }
        }
        _ => {}
    }
    t
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::widgets::golden::m5_pro;
    use crate::widgets::tests_support::Fixture;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use siltop_core::{FanSnapshot, Surface, Thermal};

    const PANELS: [Panel; 7] = [
        Panel::Cpu,
        Panel::Gpu,
        Panel::Mem,
        Panel::Net,
        Panel::Disk,
        Panel::Fans,
        Panel::Processes,
    ];

    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn rich() -> Fixture {
        let mut fx = m5_pro();
        fx.ready = true;
        fx.snap.thermal = Thermal::Fair;
        fx
    }

    fn screen(fx: &Fixture, w: u16, h: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|frame| crate::widgets::render_app(frame, &fx.view()))
            .unwrap();
        let buf = terminal.backend().buffer();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    /// Every word of every full title token. A clipped number (`24.`,
    /// `1714` without its pair) is not in the set.
    fn allowed_words(fx: &Fixture) -> Vec<String> {
        let theme = Theme::default();
        let view = fx.view();
        let mut words: Vec<String> = PANELS
            .iter()
            .flat_map(|p| {
                let mut t = for_panel(*p, &view, &theme).tokens().to_vec();
                t.extend(context(*p, &view, &theme).tokens().to_vec());
                t
            })
            .flat_map(|t| {
                let mut words: Vec<String> = t.text.split_whitespace().map(str::to_owned).collect();
                words.extend(t.short.iter().cloned());
                words
            })
            .collect();
        words.push(fx.snap.processes.len().to_string());
        words
    }

    /// A border axis label: `10%`, `70–90°`, `2MB/s`, `20Mb/s`.
    fn is_axis_word(word: &str) -> bool {
        let body = word
            .trim_end_matches(['%', '°'])
            .trim_end_matches("/s")
            .trim_end_matches(['B', 'b'])
            .trim_end_matches(['K', 'M', 'G', 'k']);
        !body.is_empty()
            && body
                .split('–')
                .all(|n| n.parse::<f64>().is_ok_and(|v| v >= 0.0))
    }

    #[test]
    fn size_sweep_never_prints_a_partial_number() {
        // Regression (D48): `cpu 100% 7`, `mem 17.1G / 24.`, `1714 rpm 1859`.
        let allowed = allowed_words(&rich());
        let mut sizes = Vec::new();
        for w in (40..=300).step_by(13) {
            for h in (12..=90).step_by(13) {
                sizes.push((w, h));
            }
        }
        sizes.extend([
            (40, 12),
            (300, 90),
            (80, 24),
            (100, 30),
            (120, 40),
            (200, 60),
        ]);
        for surface in [Surface::Work, Surface::Glance] {
            let mut fx = rich();
            fx.surface = surface;
            for &(w, h) in &sizes {
                for row in screen(&fx, w, h) {
                    if !row.contains('╭') {
                        continue;
                    }
                    let cleaned: String = row
                        .chars()
                        .map(|c| {
                            if "╭╮─│↗×…".contains(c) {
                                ' '
                            } else {
                                c
                            }
                        })
                        .collect();
                    for word in cleaned.split_whitespace() {
                        if !word.chars().any(|c| c.is_ascii_digit()) {
                            continue;
                        }
                        assert!(
                            allowed.iter().any(|a| a == word) || is_axis_word(word),
                            "{surface:?} {w}x{h}: clipped token {word:?} in {row:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn fit_never_splits_a_token_at_any_budget() {
        let fx = rich();
        let theme = Theme::default();
        let view = fx.view();
        for panel in PANELS {
            let title = for_panel(panel, &view, &theme);
            let full: Vec<&str> = title
                .tokens()
                .iter()
                .flat_map(|t| std::iter::once(t.text.trim_start()).chain(t.short.as_deref()))
                .collect();
            for budget in 0..160 {
                let line = title.fit(budget, &theme);
                let spans = &line.spans[1..line.spans.len() - 1];
                for pair in spans.chunks(2) {
                    let [gap, token] = pair else {
                        panic!("{panel:?} @{budget}: odd span layout");
                    };
                    if gap.content == " " && token.content == MORE {
                        continue;
                    }
                    assert!(
                        full.contains(&token.content.trim_start()),
                        "{panel:?} @{budget}: {:?}",
                        token.content
                    );
                }
            }
        }
    }

    #[test]
    fn fit_respects_the_budget_whenever_the_headline_fits() {
        let fx = rich();
        let theme = Theme::default();
        let view = fx.view();
        for panel in PANELS {
            let title = for_panel(panel, &view, &theme);
            let floor = title.fit(0, &theme).width();
            for budget in floor..200 {
                let line = title.fit(budget, &theme);
                assert!(
                    line.width() <= budget,
                    "{panel:?} @{budget}: {}",
                    text(&line)
                );
            }
        }
    }

    #[test]
    fn fit_adds_an_ellipsis_only_when_something_dropped() {
        let fx = rich();
        let theme = Theme::default();
        let title = cpu(&fx.view(), &theme);
        let wide = text(&title.fit(500, &theme));
        assert!(!wide.contains(MORE), "{wide}");
        assert_eq!(wide, text(&title.full(&theme)));
        let narrow = text(&title.fit(24, &theme));
        assert!(narrow.ends_with("… "), "{narrow}");
    }

    #[test]
    fn fit_drops_lowest_priority_first() {
        let theme = Theme::default();
        let mut t = Title::new("x");
        t.push(Keep::Always, "AAAA", theme.title());
        t.push(Keep::Low, "llll", theme.dim());
        t.push(Keep::Top, "TTTT", theme.title());
        t.push(Keep::Mid, "mmmm", theme.dim());
        let at = |b: usize| text(&t.fit(b, &theme));
        assert_eq!(at(39), " x  AAAA  llll  TTTT  mmmm ");
        assert!(
            !at(26).contains("llll") && at(26).contains("mmmm"),
            "{}",
            at(26)
        );
        assert!(
            !at(20).contains("mmmm") && at(20).contains("TTTT"),
            "{}",
            at(20)
        );
        assert!(
            at(14).contains("AAAA") && !at(14).contains("TTTT"),
            "{}",
            at(14)
        );
    }

    #[test]
    fn fit_prefers_dropping_later_tokens_of_equal_rank() {
        let theme = Theme::default();
        let mut t = Title::new("x");
        t.push(Keep::Always, "A", theme.title());
        t.push(Keep::Low, "first", theme.dim());
        t.push(Keep::Low, "second", theme.dim());
        let line = text(&t.fit(17, &theme));
        assert!(line.contains("first") && !line.contains("second"), "{line}");
    }

    #[test]
    fn empty_tokens_stay_absent() {
        let theme = Theme::default();
        let mut t = Title::new("x");
        t.push(Keep::Top, "", theme.title())
            .push(Keep::Top, "   ", theme.title());
        assert!(t.tokens().is_empty());
        assert_eq!(text(&t.full(&theme)), " x ");
    }

    #[test]
    fn numbers_are_fixed_width_across_ticks() {
        // Regression (D64): `5%` → `42%` shifted every token to its right.
        let theme = Theme::default();
        let mut fx = rich();
        let mut widths = Vec::new();
        for (scaled, watts, c) in [(0.05, 1.2, 9.0), (0.42, 12.0, 58.0), (0.99, 8.8, 71.0)] {
            fx.snap.cpu.scaled = scaled;
            fx.snap.cpu.active = scaled;
            fx.snap.cpu.watts = Some(watts);
            fx.snap.cpu.temp_c = Some(c);
            widths.push(cpu(&fx.view(), &theme).full(&theme).width());
        }
        assert!(widths.windows(2).all(|w| w[0] == w[1]), "{widths:?}");
    }

    #[test]
    fn one_fact_one_home_on_the_work_screen() {
        // Regression (D53): cpu °C in two titles, the process count 2-3 times.
        let mut fx = rich();
        fx.snap.cpu.temp_c = Some(58.0);
        fx.snap.sensors.cpu_c = Some(58.0);
        fx.proc.filter = String::from("a");
        let n = fx.snap.processes.len();
        for (w, h) in [(120, 40), (200, 60), (300, 90)] {
            let rows = screen(&fx, w, h);
            let words: Vec<String> = rows
                .iter()
                .flat_map(|r| r.split_whitespace().map(str::to_owned).collect::<Vec<_>>())
                .collect();
            let count = |needle: &str| words.iter().filter(|w| *w == needle).count();
            assert!(count("58°") <= 1, "{w}x{h}: cpu °C lives in the cpu title");
            if w >= 200 {
                assert_eq!(count("58°"), 1, "{w}x{h}");
            }
            let suffix = format!("/{n}");
            let proc_counts = words.iter().filter(|w| w.ends_with(&suffix)).count();
            assert_eq!(proc_counts, 1, "{w}x{h}: process count once");
            assert!(!words.iter().any(|w| w == "procs"), "{w}x{h}");
        }
    }

    #[test]
    fn proc_title_shows_filtered_over_total() {
        let theme = Theme::default();
        let mut fx = rich();
        let total = fx.snap.processes.len();
        assert_eq!(
            text(&proc(&fx.view(), total, 0, &theme).full(&theme)),
            format!(" proc  {total} ")
        );
        fx.proc.filter = String::from("zsh");
        assert_eq!(
            text(&proc(&fx.view(), 2, 0, &theme).full(&theme)),
            format!(" proc  2/{total} ")
        );
        assert!(text(&proc(&fx.view(), 2, 7, &theme).full(&theme)).contains("↑ 7"));
    }

    #[test]
    fn fan_text_table() {
        let fan = |rpm| FanMetric {
            name: String::new(),
            rpm,
            max_rpm: 6000,
        };
        assert_eq!(fan_text(&[]), None, "fanless Macs print no fan token");
        assert_eq!(fan_text(&[fan(0)]).as_deref(), Some("fans idle"));
        assert_eq!(fan_text(&[fan(0), fan(0)]).as_deref(), Some("fans idle"));
        assert_eq!(fan_text(&[fan(1714)]).as_deref(), Some("1714rpm"));
        assert_eq!(
            fan_text(&[fan(1714), fan(1859)]).as_deref(),
            Some("1714·1859rpm")
        );
        // Regression: speeds carried a format-string pad, so a mixed pair
        // read `   0· 980rpm` with a space after the separator.
        assert_eq!(fan_text(&[fan(0), fan(980)]).as_deref(), Some("0·980rpm"));
    }

    /// Regression: a tight Glance title read `gpu  2%   466MHz`: the
    /// percent lost its pad while the clock kept a format-string pad, so
    /// padding must be the fitter's and all-or-nothing in every title.
    #[test]
    fn padding_is_all_or_nothing_in_every_panel_title() {
        let theme = Theme::default();
        let mut fx = rich();
        fx.snap.cpu.scaled = 0.03;
        if let Some(g) = fx.snap.gpu.as_mut() {
            g.scaled = 0.02;
            g.freq_mhz = Some(466);
        }
        for panel in PANELS.into_iter().chain([Panel::Power]) {
            let t = for_panel(panel, &fx.view(), &theme);
            for budget in 0..=200 {
                let fit = t.fitted(budget);
                if fit.padded {
                    continue;
                }
                let line = text(&t.render(&fit, &theme));
                assert!(!line.contains("   "), "{panel:?} at {budget}: {line:?}");
                assert!(!line.contains("· ") || panel != Panel::Fans, "{line:?}");
            }
        }
    }

    /// Regression: the power title printed the system draw twice
    /// (`power  6.9W … 79% ▼ 6.9W 8h18m`).
    #[test]
    fn power_title_says_the_draw_once() {
        let theme = Theme::default();
        let mut fx = rich();
        fx.snap.power.system = Some(siltop_core::Reading::measured(6.9));
        fx.snap.battery.present = true;
        fx.snap.battery.source = siltop_core::PowerSource::Battery;
        fx.snap.battery.state = siltop_core::ChargeState::Discharging;
        fx.snap.battery.percent = Some(79);
        fx.snap.battery.watts = Some(-6.9);
        let line = text(&power(&fx.view(), &theme).full(&theme));
        assert_eq!(line.matches("6.9W").count(), 1, "{line:?}");
        assert!(line.contains("79% ▼"), "{line:?}");
        // no system reading: the battery token is the draw's home
        fx.snap.power.system = None;
        let line = text(&power(&fx.view(), &theme).full(&theme));
        assert!(line.contains("79% ▼ 6.9W"), "{line:?}");
    }

    /// Regression: at 80x24 the net (18 wide) and power (17 wide) boxes
    /// showed bare labels; a short form keeps the headline value.
    #[test]
    fn narrow_net_and_power_titles_keep_their_value() {
        let theme = Theme::default();
        let mut fx = rich();
        fx.snap.network.rx_bps = 8 * 1_234_000;
        fx.snap.power.system = Some(siltop_core::Reading::measured(7.6));
        let narrow_net = text(&net(&fx.view(), &theme).fit(budget(18, 0), &theme));
        assert!(narrow_net.contains("↓1.2M"), "{narrow_net:?}");
        let narrow_power = text(&power(&fx.view(), &theme).fit(budget(17, 0), &theme));
        assert!(narrow_power.contains("8W"), "{narrow_power:?}");
        let wide = text(&power(&fx.view(), &theme).fit(budget(60, 0), &theme));
        assert!(wide.contains("7.6W"), "{wide:?}");
    }

    #[test]
    fn gpu_clock_pads_only_with_the_percent() {
        let theme = Theme::default();
        let mut fx = rich();
        if let Some(g) = fx.snap.gpu.as_mut() {
            g.scaled = 0.02;
            g.freq_mhz = Some(466);
        }
        let t = gpu(&fx.view(), &theme);
        let full = text(&t.full(&theme));
        assert!(full.contains("   2%   466MHz"), "padded: {full:?}");
        let tight = (0..=200)
            .map(|b| t.fitted(b))
            .find(|f| !f.padded && f.forms.iter().all(|f| *f != Form::Hidden))
            .map(|f| text(&t.render(&f, &theme)));
        let tight = tight.expect("some budget fits every token unpadded");
        assert!(tight.contains("2%  466MHz"), "unpadded: {tight:?}");
    }

    /// Regression: any swap at all painted in warn ink, so most Macs showed a
    /// permanent amber warning at nominal pressure.
    #[test]
    fn parked_swap_at_nominal_pressure_is_not_a_warning() {
        let theme = Theme::default();
        let mut fx = rich();
        let swap_ink = |fx: &Fixture| {
            let t = mem(&fx.view(), &theme);
            t.tokens()
                .iter()
                .find(|token| token.text.starts_with("swap"))
                .map(|token| token.style)
                .unwrap()
        };
        fx.snap.memory.swap_used_bytes = 820 << 20;
        fx.snap.memory.pressure = Pressure::Nominal;
        fx.snap.memory.swap_in_bps = Some(0);
        fx.snap.memory.swap_out_bps = Some(0);
        assert_eq!(swap_ink(&fx), theme.dim());
        fx.snap.memory.swap_out_bps = Some(4096);
        assert_eq!(swap_ink(&fx), theme.warn_style(), "growing swap warns");
        fx.snap.memory.swap_out_bps = Some(0);
        fx.snap.memory.pressure = Pressure::Warn;
        assert_eq!(swap_ink(&fx), theme.warn_style(), "pressure warns");
    }

    #[test]
    fn mem_pair_pads_like_every_other_number() {
        let theme = Theme::default();
        let mut fx = rich();
        fx.snap.memory.used_bytes = 9_600_000_000;
        let t = mem(&fx.view(), &theme);
        for budget in 0..=120 {
            let fit = t.fitted(budget);
            let line = text(&t.render(&fit, &theme));
            if fit.padded {
                assert!(line.starts_with(" mem   "), "{budget}: {line:?}");
            } else {
                assert!(!line.contains("mem   "), "{budget}: {line:?}");
            }
        }
    }

    #[test]
    fn heat_title_hides_fans_on_fanless_macs() {
        let theme = Theme::default();
        let mut fx = rich();
        fx.snap.fans = FanSnapshot::default();
        let t = text(&heat(&fx.view(), &theme).full(&theme));
        assert!(!t.contains("rpm") && !t.contains("fans"), "{t}");
        assert!(t.contains("peak"), "{t}");
    }

    #[test]
    fn renamed_labels_follow_the_glossary() {
        let theme = Theme::default();
        let fx = rich();
        let t = text(&heat(&fx.view(), &theme).full(&theme));
        assert!(t.starts_with(" heat") && t.contains("peak 64°"), "{t}");
        assert!(!t.contains("sens") && !t.contains("hot"), "{t}");
    }

    #[test]
    fn zero_rates_are_dim() {
        let theme = Theme::default();
        let mut fx = rich();
        fx.snap.network.rx_bps = 0;
        fx.snap.disk.write_bps = 0;
        for title in [net(&fx.view(), &theme), disk(&fx.view(), &theme)] {
            let zero = title
                .tokens()
                .iter()
                .find(|t| t.text.ends_with('0') && !t.text.contains('/'))
                .unwrap();
            assert_eq!(zero.style, theme.dim(), "{:?}", zero.text);
        }
    }

    #[test]
    fn expanded_context_carries_no_live_numbers() {
        let theme = Theme::default();
        let fx = rich();
        let view = fx.view();
        for panel in PANELS {
            let full = for_panel(panel, &view, &theme);
            // A live reading: a digit plus a unit (`en0` and `6S+12P` are names).
            let numeric = |t: &&Token| {
                t.text.chars().any(|c| c.is_ascii_digit()) && t.text.contains(['%', '°', '/', 'W'])
            };
            for token in context(panel, &view, &theme)
                .tokens()
                .iter()
                .filter(numeric)
            {
                assert!(
                    !full.tokens().iter().any(|t| t.text == token.text),
                    "{panel:?} repeats {:?}",
                    token.text
                );
            }
        }
        // The core split lives in the meta column, never the frame title.
        let cpu_ctx = text(&context(Panel::Cpu, &view, &theme).full(&theme));
        assert!(!cpu_ctx.contains("6S+12P"), "{cpu_ctx}");
    }

    /// Regression: the expanded frame read `cpu 6S+12P` right beside a
    /// meta column (formerly a top bar) that already said `6S+12P`.
    #[test]
    fn expanded_context_leaves_identity_to_the_meta_column() {
        let theme = Theme::default();
        let fx = rich();
        let cpu_ctx = text(&context(Panel::Cpu, &fx.view(), &theme).full(&theme));
        assert!(!cpu_ctx.contains("6S+12P"), "{cpu_ctx}");
        let gpu_ctx = text(&context(Panel::Gpu, &fx.view(), &theme).full(&theme));
        assert!(
            gpu_ctx.contains("20-core"),
            "the gpu title keeps its core count: {gpu_ctx}"
        );
    }

    #[test]
    fn bare_title_fits_without_label_padding_or_ellipsis() {
        let theme = Theme::default();
        let mut t = Title::bare();
        t.push(Keep::Mid, "74% ▼ 11W", theme.fg());
        t.push(Keep::Low, "62W", theme.dim());
        t.push(Keep::Always, "● 1 s", theme.dim());
        assert_eq!(text(&t.fit(100, &theme)), "74% ▼ 11W  62W  ● 1 s");
        assert_eq!(text(&t.fit(16, &theme)), "74% ▼ 11W  ● 1 s");
        assert_eq!(text(&t.fit(6, &theme)), "● 1 s");
        assert_eq!(text(&t.fit(2, &theme)), "");
        for budget in 0..40 {
            assert!(t.fit(budget, &theme).width() <= budget, "{budget}");
        }
    }

    #[test]
    fn mem_pair_is_one_unsplittable_token() {
        let theme = Theme::default();
        let fx = rich();
        let t = mem(&fx.view(), &theme);
        let head = &t.tokens()[0];
        assert_eq!(head.keep, Keep::Always);
        assert!(head.text.trim_start().contains("G/"), "{:?}", head.text);
        assert!(head.text.ends_with("24.0G"), "{:?}", head.text);
    }

    #[test]
    fn idle_rails_never_print_zero_watts() {
        // Regression: an idle GPU printed `0.0W` in the live title.
        let theme = Theme::default();
        let mut fx = rich();
        for w in [0.0_f32, 0.01, 0.049] {
            fx.snap.cpu.watts = Some(w);
            if let Some(g) = fx.snap.gpu.as_mut() {
                g.watts = Some(w);
            }
            for t in [cpu(&fx.view(), &theme), gpu(&fx.view(), &theme)] {
                let line = text(&t.full(&theme));
                assert!(!line.contains("0.0W") && !line.contains('W'), "{w}: {line}");
            }
        }
        fx.snap.cpu.watts = Some(0.05);
        assert!(text(&cpu(&fx.view(), &theme).full(&theme)).contains("0.1W"));
    }

    #[test]
    fn net_honours_the_bits_setting() {
        let theme = Theme::default();
        let mut fx = rich();
        let bytes = text(&net(&fx.view(), &theme).full(&theme));
        assert!(bytes.contains("B/s") && !bytes.contains("b/s"), "{bytes}");
        fx.net_unit = NetUnit::Bits;
        let bits = text(&net(&fx.view(), &theme).full(&theme));
        assert!(bits.contains("b/s") && !bits.contains("B/s"), "{bits}");
    }
}
