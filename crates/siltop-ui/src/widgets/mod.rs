pub(crate) mod bench;
mod cpu;
mod disk;
mod expanded;
mod fans;
pub(crate) mod fleet;
mod footer;
#[cfg(test)]
pub(crate) mod golden;
mod gpu;
mod grid;
mod help;
mod hover;
pub(crate) mod identity;
mod mem;
mod menu;
mod mirror;
mod net;
pub(crate) mod onboarding;
pub(crate) mod power;
mod processes;
mod settings;
pub(crate) mod timeline;
pub(crate) mod update_dialog;

use ratatui::Frame;
use ratatui::layout::Rect;
use siltop_core::{
    Config, Histories, History, NetUnit, ProcSort, SeriesId, Snapshot, Stamp, Surface, TimeAxis,
    Verdict, Zoom,
};

use crate::layout::{Hit, LayoutFlags, LayoutPlan, Panel, hit_test, plan};
use crate::theme::Theme;

pub use processes::{
    Anchor, ModelRow, Nesting, ProcModel, ProcView, SCROLL_MARGIN, Tags, proc_rows,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    Cpu,
    Gpu,
    Mem,
    Net,
    Disk,
    Fans,
    Power,
    Search,
    #[default]
    Processes,
}

impl Focus {
    #[must_use]
    pub const fn panel(self) -> Panel {
        match self {
            Self::Cpu => Panel::Cpu,
            Self::Gpu => Panel::Gpu,
            Self::Mem => Panel::Mem,
            Self::Net => Panel::Net,
            Self::Disk => Panel::Disk,
            Self::Fans => Panel::Fans,
            Self::Power => Panel::Power,
            Self::Search | Self::Processes => Panel::Processes,
        }
    }

    #[must_use]
    pub const fn from_panel(panel: Panel) -> Self {
        match panel {
            Panel::Cpu => Self::Cpu,
            Panel::Gpu => Self::Gpu,
            Panel::Mem => Self::Mem,
            Panel::Net => Self::Net,
            Panel::Disk => Self::Disk,
            Panel::Fans => Self::Fans,
            Panel::Power => Self::Power,
            Panel::Processes => Self::Processes,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppView<'a> {
    pub theme: &'a Theme,
    pub snapshot: &'a Snapshot,
    /// Every recorded series; read through [`AppView::history`].
    pub histories: &'a Histories,
    pub surface: Surface,
    pub focus: Focus,
    pub proc: &'a ProcView,
    pub help: bool,
    /// The full-screen settings page, when open; the dashboard is not drawn.
    pub settings: Option<&'a SettingsState>,
    /// The live config the settings page reads its values from.
    pub config: &'a Config,
    /// Where settings are saved, for the about page.
    pub config_path: Option<&'a str>,
    pub confirm_kill: bool,
    pub confirm_pid: Option<u32>,
    pub confirm_signal: &'static str,
    /// How many armed targets are critical (loginwindow, WindowServer…):
    /// the prompt says so, since ending one can log the user out.
    pub confirm_critical: usize,
    /// The open signal picker, drawn above everything but help.
    pub signal_menu: Option<SignalMenuView<'a>>,
    pub searching: bool,
    pub ready: bool,
    pub frozen: bool,
    /// A replay: its pids are history, so no chip offers to signal them.
    pub read_only: bool,
    pub show_gpu: bool,
    pub show_net: bool,
    pub show_disk: bool,
    pub show_fans: bool,
    pub show_power: bool,
    pub show_cores: bool,
    pub show_threads: bool,
    pub show_tree: bool,
    pub sort: ProcSort,
    /// Flips the sort column's natural direction (cpu/mem high first, pid low first).
    pub sort_reversed: bool,
    pub detail_pid: Option<u32>,
    pub expanded: Option<Panel>,
    pub proc_ratio: u16,
    /// The interval snapshots really arrive at.
    pub interval_ms: u64,
    /// The battery saver stretched `interval_ms` past the chosen interval.
    pub battery_saver: bool,
    /// The sampler has stopped for good: the numbers are its last.
    pub stopped: bool,
    pub status: Option<&'a str>,
    /// Colours the status toast.
    pub status_kind: ToastKind,
    /// Wall-clock `hh:mm:ss` the display was paused at.
    pub paused_at: Option<&'a str>,
    /// Help overlay content, generated from the binary's keymap.
    pub help_groups: &'a [HelpGroup],
    pub help_scroll: u16,
    /// Bytes/s by default; `units.net_bits` switches network rates to bits.
    pub net_unit: NetUnit,
    /// Graph zoom the user picked with `z`; `None` = each view's default.
    pub zoom: Option<Zoom>,
    /// Stamp of the newest snapshot on the shared sample clock.
    pub clock: Option<Stamp>,
    /// The open popup menu: the main menu or a process row's.
    pub menu: Option<menu::MenuView<'a>>,
    /// The verdict engine's current call, for the footer's verdict slot.
    pub verdict: Option<&'a Verdict>,
    /// Build and path rows (`commit`, `built`, `state`) for the about page,
    /// supplied by the binary; absent facts are simply left out.
    pub about: &'a [(&'static str, String)],
    /// The button under the pointer ([`hover_rect`]), drawn lit.
    pub hover: Option<Rect>,
    /// The full-screen Timeline page, when open; the dashboard is not drawn.
    pub timeline: Option<&'a timeline::TimelineView>,
    /// `update 1.1.1 available · u to update`, when a newer version is waiting.
    pub update_note: Option<&'a str>,
    /// The keys the footer hints name, from the session's key profile.
    pub keys: crate::chips::KeyHints,
}

impl<'a> AppView<'a> {
    /// Zen hides the footer, except while it carries something the user must
    /// see: a toast, a kill confirmation, the filter being typed, a pause, or
    /// the main menu open over its `siltop ≡` button.
    #[must_use]
    pub fn zen_quiet(&self) -> bool {
        self.config.zen
            && self.status.is_none()
            && !self.confirm_kill
            && !self.searching
            && !self.frozen
            && self.menu.is_none()
    }

    #[must_use]
    pub fn flags(&self) -> LayoutFlags {
        LayoutFlags {
            show_gpu: self.show_gpu,
            show_net: self.show_net,
            show_disk: self.show_disk,
            show_fans: self.show_fans,
            show_power: self.show_power,
            has_gpu: self.snapshot.gpu.is_some(),
            has_fans: self.snapshot.fans.is_present() || self.snapshot.sensors.is_present(),
            has_disk: !self.snapshot.disk.volumes.is_empty(),
            has_power: power::has_power(self.snapshot),
            expanded: self.expanded,
            proc_ratio: self.proc_ratio,
            no_footer: self.zen_quiet(),
        }
    }

    #[must_use]
    pub fn is_focused(&self, panel: Panel) -> bool {
        self.focus.panel() == panel
    }

    #[must_use]
    pub fn is_expanded(&self, panel: Panel) -> bool {
        self.expanded == Some(panel)
    }

    /// The zoom graphs use: the user's pick, else live for compact views and
    /// five minutes once a panel is expanded.
    #[must_use]
    pub fn zoom(&self) -> Zoom {
        self.zoom.unwrap_or(if self.expanded.is_some() {
            Zoom::FiveMin
        } else {
            Zoom::Live
        })
    }

    /// Time axis for a graph of `history`: every graph shares the snapshot
    /// clock and one window per view (the zoom never depends on a plot's
    /// width), so a moment lands at the same fraction of every plot.
    #[must_use]
    pub fn time_for(&self, history: &History) -> TimeAxis {
        TimeAxis {
            zoom: self.zoom(),
            now: self.clock.or_else(|| history.newest()).unwrap_or(0),
            cadence: std::time::Duration::from_millis(self.interval_ms.max(1)),
        }
    }

    /// The recorded ring for `id`.
    #[must_use]
    pub fn history(&self, id: SeriesId) -> &'a History {
        self.histories.get(id)
    }

    /// Per-fan RPM rings, index-aligned with the snapshot's fans.
    #[must_use]
    pub fn fan_histories(&self) -> &'a [History] {
        self.histories.fans()
    }

    #[must_use]
    pub fn zone_temp_history(&self, kind: siltop_core::ClusterKind) -> &'a History {
        self.history(match kind {
            siltop_core::ClusterKind::Efficiency => SeriesId::ETemp,
            siltop_core::ClusterKind::Performance => SeriesId::PTemp,
            siltop_core::ClusterKind::Super => SeriesId::STemp,
        })
    }

    #[must_use]
    pub fn cluster_load_history(&self, kind: siltop_core::ClusterKind) -> &'a History {
        self.history(match kind {
            siltop_core::ClusterKind::Efficiency => SeriesId::ELoad,
            siltop_core::ClusterKind::Performance => SeriesId::PLoad,
            siltop_core::ClusterKind::Super => SeriesId::SLoad,
        })
    }
}

pub use expanded::hop_hit;

/// Frames a compact panel with its shared title fitted to the width and the
/// plot's scale in the top border. Returns the inner area.
pub(crate) fn frame_panel<P: crate::chrome::BorderScale>(
    frame: &mut Frame,
    area: Rect,
    panel: Panel,
    view: &AppView<'_>,
    theme: &Theme,
    plot: Option<&P>,
) -> Rect {
    use crate::titles::{Keep, for_panel, panel_budget};
    let title = for_panel(panel, view, theme);
    // The plot fills the frame's inside, so its scale is the one for that width.
    let plot_width = area.width.saturating_sub(2);
    // The scale label yields to the title's headline facts on narrow panels.
    // Zen drops the scale labels: the borders carry only the titles.
    let axis = plot
        .filter(|_| !view.config.zen)
        .and_then(|p| p.border_label(plot_width))
        .filter(|a| {
            title.keeps(
                panel_budget(panel, area.width, crate::chrome::axis_reserve(a), theme),
                Keep::Top,
            )
        });
    let reserve = axis.as_deref().map_or(0, crate::chrome::axis_reserve);
    let title = title.fit(panel_budget(panel, area.width, reserve, theme), theme);
    let (focused, expanded) = (view.is_focused(panel), view.is_expanded(panel));
    let mut block = crate::chrome::panel_block_axis(panel, title, focused, expanded, axis, theme);
    // C08: cpu and heat tint their resting frame with the thermal state.
    if !(focused || expanded)
        && let Some(tint) = theme.thermal_border(panel, view.snapshot.thermal)
    {
        block = block.border_style(tint);
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}
pub use fleet::{
    CardLink, FleetCard, FleetProc, fleet_grid, fleet_hit, render_fleet, render_fleet_procs,
};
pub use footer::{ToastKind, footer_hit, interval_label};
pub use help::{HelpGroup, help_max_scroll, help_rect};
pub use hover::hover_rect;
pub use menu::{
    MenuEntry, MenuHit, MenuView, first as menu_first, first_item as menu_first_item,
    hit as menu_hit, page as menu_page, step as menu_step,
};
pub use processes::filtered as filtered_processes;
pub use processes::signal_menu::{
    SignalMenuView, SignalRow, menu_rect as signal_menu_rect, row_at as signal_menu_row_at,
};
pub use processes::{DossierTab, detail_actions, detail_close, detail_rect, detail_tabs};
pub use settings::{
    Category as SettingsCategory, KEY_PROFILES, SETTINGS, SettingDef, SettingsHit, SettingsState,
    Step as SettingStep, hit as settings_hit, reveal_swatch, scroll_swatches,
    themes as theme_options,
};
pub use timeline::{
    Parts as TimelineParts, TimelineHit, TimelineView, hit as timeline_hit,
    parts as timeline_parts, plot_width as timeline_plot_width,
};

/// Hit test with the view: panels and corners from the layout, then the
/// process pane's search line, header columns and scrolled rows.
#[must_use]
pub fn hit_at(view: &AppView<'_>, area: Rect, col: u16, row: u16) -> Option<Hit> {
    let hit = hit_test(area, view.surface, view.flags(), col, row);
    if hit != Some(Hit::Panel(Panel::Processes)) {
        return hit;
    }
    plan(area, view.surface, view.flags())
        .processes
        .and_then(|rect| processes::hit(view, rect, col, row))
        .or(hit)
}

pub fn render_app(frame: &mut Frame, view: &AppView<'_>) {
    let theme = view.theme;
    let area = frame.area();
    if !render_page(frame, area, view) {
        render_dashboard(frame, area, view);
    }
    if let Some(target) = view.hover {
        crate::chrome::light(frame.buffer_mut(), target, theme);
    }
    crate::chrome::scrub_controls(frame.buffer_mut());
    theme.finish(frame.buffer_mut());
}

/// A full-screen page (settings, else the timeline) with help over it;
/// nothing of the dashboard shows through. False when no page is open.
fn render_page(frame: &mut Frame, area: Rect, view: &AppView<'_>) -> bool {
    let theme = view.theme;
    if let Some(state) = view.settings {
        settings::render(frame, area, view, state);
    } else if let Some(timeline) = view.timeline {
        timeline::render(frame, area, timeline, view.status, theme);
    } else {
        return false;
    }
    if view.help {
        help::render(frame, area, view, theme);
        if let Some(modal) = modal_rect(view, area) {
            crate::chrome::veil_outside(frame.buffer_mut(), modal, theme);
        }
    }
    true
}

/// The panels, the footer and every overlay, the topmost one lit and the
/// rest veiled.
fn render_dashboard(frame: &mut Frame, area: Rect, view: &AppView<'_>) {
    let theme = view.theme;
    let layout = plan(area, view.surface, view.flags());
    let view = AppView {
        surface: layout.surface,
        ..view.clone()
    };
    let expanded = layout.expanded;

    if let Some(area) = layout.cpu {
        render_panel(frame, area, &view, theme, Panel::Cpu, expanded, cpu::render);
    }
    if let Some(area) = layout.gpu {
        render_panel(frame, area, &view, theme, Panel::Gpu, expanded, gpu::render);
    }
    if let Some(area) = layout.mem {
        render_panel(frame, area, &view, theme, Panel::Mem, expanded, mem::render);
    }
    if let Some(area) = layout.net {
        render_panel(frame, area, &view, theme, Panel::Net, expanded, net::render);
    }
    if let Some(area) = layout.disk {
        render_panel(
            frame,
            area,
            &view,
            theme,
            Panel::Disk,
            expanded,
            disk::render,
        );
    }
    if let Some(area) = layout.fans {
        render_panel(
            frame,
            area,
            &view,
            theme,
            Panel::Fans,
            expanded,
            fans::render,
        );
    }
    if let Some(area) = layout.power {
        render_panel(
            frame,
            area,
            &view,
            theme,
            Panel::Power,
            expanded,
            power::render,
        );
    }
    if let Some(area) = layout.processes {
        render_panel(
            frame,
            area,
            &view,
            theme,
            Panel::Processes,
            expanded,
            processes::render,
        );
    }
    if !view.zen_quiet() {
        footer::render(frame, layout.footer, &view, theme);
    }
    if let Some(menu) = &view.signal_menu {
        processes::signal_menu::render(frame, signal_menu_area(&view, area), menu, theme);
    }

    if let Some(menu) = &view.menu {
        menu::render(frame, menu_area(area), menu, theme);
    }
    if view.help {
        help::render(frame, area, &view, theme);
    }
    if let Some(modal) = modal_in(&view, area, &layout) {
        let mut lit = vec![modal];
        if view.confirm_kill && modal != layout.footer {
            // a kill armed from the dossier asks its y/n in the footer
            lit.push(layout.footer);
        } else if modal == layout.footer
            && let Some(pane) = layout.processes
        {
            lit.extend(processes::armed_rows(&view, pane));
        }
        crate::chrome::veil_except(frame.buffer_mut(), &lit, theme);
    }
}

/// The topmost open overlay, which stays lit while [`crate::chrome::veil_outside`]
/// dims the rest; `None` when nothing modal is open. Over a full page
/// (settings, the timeline) only help is modal.
#[must_use]
pub fn modal_rect(view: &AppView<'_>, area: Rect) -> Option<Rect> {
    if view.settings.is_some() || view.timeline.is_some() {
        return view.help.then(|| help_rect(area, view.help_groups));
    }
    modal_in(view, area, &plan(area, view.surface, view.flags()))
}

/// Draw order reversed: help, the main menu, the kill confirmation (which
/// lives in the footer and is the question being asked, even when the
/// signal picker or the dossier raised it), the signal picker, then the
/// process dossier.
fn modal_in(view: &AppView<'_>, area: Rect, layout: &LayoutPlan) -> Option<Rect> {
    if view.help {
        return Some(help_rect(area, view.help_groups));
    }
    if let Some(menu) = &view.menu {
        return Some(menu::rect(menu_area(area), menu));
    }
    if view.confirm_kill {
        return Some(layout.footer);
    }
    if let Some(menu) = &view.signal_menu {
        return Some(signal_menu_rect(signal_menu_area(view, area), menu));
    }
    if let (Some(pid), Some(pane)) = (view.detail_pid, layout.processes)
        && view.snapshot.processes.iter().any(|p| p.pid == pid)
    {
        let card = detail_rect(pane);
        return (!card.is_empty()).then_some(card);
    }
    None
}

fn render_panel(
    frame: &mut Frame,
    area: Rect,
    view: &AppView<'_>,
    theme: &Theme,
    panel: Panel,
    expanded: Option<Panel>,
    compact: fn(&mut Frame, Rect, &AppView<'_>, &Theme),
) {
    if expanded == Some(panel) {
        expanded::render(frame, area, view, theme, panel);
    } else {
        compact(frame, area, view, theme);
    }
}

/// The area the signal picker centres in: the dossier card it was raised
/// from, else the process pane, else the screen, whichever first fits it.
#[must_use]
pub fn signal_menu_area(view: &AppView<'_>, screen: Rect) -> Rect {
    let Some(menu) = &view.signal_menu else {
        return screen;
    };
    let want = signal_menu_rect(screen, menu);
    let layout = plan(screen, view.surface, view.flags());
    let pane = layout.processes;
    let card = pane
        .filter(|_| view.detail_pid.is_some())
        .map(detail_rect)
        .filter(|r| !r.is_empty());
    card.into_iter()
        .chain(pane)
        .find(|r| r.width >= want.width && r.height >= want.height)
        .unwrap_or(screen)
}

/// The area the main menu centres in: everything above the footer.
#[must_use]
pub fn menu_area(screen: Rect) -> Rect {
    Rect {
        height: screen.height.saturating_sub(1),
        ..screen
    }
}

#[must_use]
pub fn inner_process_area(area: Rect, surface: Surface, flags: LayoutFlags) -> Option<Rect> {
    plan(area, surface, flags).processes
}

#[cfg(test)]
pub(crate) mod tests_support {
    use siltop_core::{Histories, Process, Snapshot, Surface};

    use super::{AppView, Focus, ProcView};

    pub(crate) struct Fixture {
        pub net_unit: siltop_core::NetUnit,
        pub snap: Snapshot,
        pub histories: Histories,
        pub proc: ProcView,
        pub surface: Surface,
        pub focus: Focus,
        pub help: bool,
        pub settings: bool,
        pub confirm_kill: bool,
        pub confirm_pid: Option<u32>,
        pub confirm_signal: &'static str,
        pub detail_pid: Option<u32>,
        pub searching: bool,
        pub ready: bool,
        pub frozen: bool,
        pub show_tree: bool,
        pub expanded: Option<crate::layout::Panel>,
        pub theme: crate::theme::Theme,
        pub status_kind: super::ToastKind,
        pub help_groups: Vec<super::HelpGroup>,
        pub help_scroll: u16,
        pub menu: Option<usize>,
        pub menu_entries: Vec<super::MenuEntry>,
        pub verdict: Option<siltop_core::Verdict>,
        pub settings_state: super::SettingsState,
        pub config: siltop_core::Config,
    }

    pub(crate) fn process(pid: u32, name: &str, cpu: f32) -> Process {
        Process {
            pid,
            ppid: 1,
            name: name.to_owned(),
            cpu: Some(cpu),
            mem_bytes: Some(0),
            threads: Some(1),
            user: String::from("cloaky"),
            status: "sleeping",
            ..Process::default()
        }
    }

    pub(crate) fn fixture(filter: &str) -> Fixture {
        let mut snap = Snapshot::empty();
        snap.soc.name = String::from("M4 Pro");
        snap.soc.e_cores = 4;
        snap.soc.p_cores = 8;
        snap.soc.gpu_cores = 16;
        snap.soc.memory_bytes = 36 * 1024 * 1024 * 1024;
        snap.processes = vec![process(904, "Xcode", 48.1)];
        Fixture {
            snap,
            histories: Histories::default(),
            proc: ProcView {
                filter: filter.to_owned(),
                ..ProcView::default()
            },
            surface: Surface::Work,
            focus: Focus::Processes,
            help: false,
            settings: false,
            confirm_kill: false,
            confirm_pid: None,
            confirm_signal: "TERM",
            detail_pid: None,
            searching: false,
            ready: true,
            frozen: false,
            show_tree: false,
            expanded: None,
            theme: crate::theme::Theme::default(),
            status_kind: super::ToastKind::Info,
            help_groups: vec![super::HelpGroup {
                title: "general",
                rows: vec![
                    (String::from("?"), "help"),
                    (String::from("q ctrl-c"), "quit"),
                ],
            }],
            help_scroll: 0,
            menu: None,
            menu_entries: vec![
                super::MenuEntry::header("pages"),
                super::MenuEntry::item("Timeline", "H"),
                super::MenuEntry::item("Settings", "s"),
                super::MenuEntry::item("Themes", "t"),
                super::MenuEntry::item("Keys & help", "?"),
                super::MenuEntry::item("About", "a"),
                super::MenuEntry::item("Quit", "q"),
            ],
            verdict: None,
            net_unit: siltop_core::NetUnit::Bytes,
            settings_state: super::SettingsState::default(),
            config: siltop_core::Config::default(),
        }
    }

    impl Fixture {
        pub(crate) fn view(&self) -> AppView<'_> {
            AppView {
                theme: &self.theme,
                snapshot: &self.snap,
                histories: &self.histories,
                surface: self.surface,
                focus: self.focus,
                proc: &self.proc,
                help: self.help,
                settings: self.settings.then_some(&self.settings_state),
                config: &self.config,
                config_path: Some("~/.config/siltop/config.toml"),
                confirm_kill: self.confirm_kill,
                confirm_pid: self.confirm_pid,
                confirm_signal: self.confirm_signal,
                confirm_critical: 0,
                signal_menu: None,
                searching: self.searching,
                ready: self.ready,
                frozen: self.frozen,
                read_only: false,
                show_gpu: true,
                show_net: true,
                show_disk: true,
                show_fans: true,
                show_power: true,
                show_cores: true,
                show_threads: false,
                show_tree: self.show_tree,
                sort: siltop_core::ProcSort::Cpu,
                sort_reversed: false,
                detail_pid: self.detail_pid,
                expanded: self.expanded,
                proc_ratio: 55,
                interval_ms: 1000,
                battery_saver: false,
                stopped: false,
                status: None,
                status_kind: self.status_kind,
                paused_at: None,
                help_groups: &self.help_groups,
                help_scroll: self.help_scroll,
                net_unit: self.net_unit,
                zoom: None,
                clock: None,
                menu: self.menu.map(|selected| super::MenuView {
                    title: "siltop",
                    entries: &self.menu_entries,
                    selected,
                    top: 0,
                    anchor: None,
                }),
                verdict: self.verdict.as_ref(),
                about: &[],
                hover: None,
                timeline: None,
                update_note: None,
                keys: crate::chips::KeyHints::default(),
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod render_tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use siltop_core::SeriesId;
    use siltop_core::{
        ClusterKind, CoreSample, DiskSnapshot, DiskVolume, FanMetric, FanSnapshot, GpuSnapshot,
        History, Surface,
    };

    use super::render_app;
    use super::tests_support::fixture;
    use crate::layout::Panel;

    #[test]
    fn zone_and_cluster_accessors_read_their_own_series() {
        let mut fx = fixture("");
        let kinds = [
            (ClusterKind::Efficiency, SeriesId::ETemp, SeriesId::ELoad),
            (ClusterKind::Performance, SeriesId::PTemp, SeriesId::PLoad),
            (ClusterKind::Super, SeriesId::STemp, SeriesId::SLoad),
        ];
        for (i, (_, temp, load)) in kinds.iter().enumerate() {
            let v = i as f32;
            fx.histories.get_mut(*temp).push(40.0 + v);
            fx.histories.get_mut(*load).push(0.1 + v);
        }
        let view = fx.view();
        for (i, (kind, _, _)) in kinds.into_iter().enumerate() {
            let v = i as f32;
            assert_eq!(view.zone_temp_history(kind).last(), Some(40.0 + v));
            assert_eq!(view.cluster_load_history(kind).last(), Some(0.1 + v));
        }
    }

    #[test]
    fn view_history_is_the_app_ring_not_a_copy() {
        let mut fx = fixture("");
        fx.histories.get_mut(SeriesId::PowerSystem).push(12.0);
        let view = fx.view();
        assert!(std::ptr::eq(
            view.history(SeriesId::PowerSystem),
            fx.histories.get(SeriesId::PowerSystem)
        ));
        assert!(view.fan_histories().is_empty(), "fanless: no rings");
    }

    /// A process name carrying an OSC escape (as a replayed trace or a fleet
    /// host could deliver it, bypassing the collector's own cleaning) must
    /// never reach the terminal as control characters.
    #[test]
    fn escape_sequences_in_process_names_never_reach_the_terminal() {
        let mut fx = fixture("");
        fx.snap.processes = vec![
            crate::widgets::tests_support::process(87890, "x\u{1b}]2;RVBC\u{7}y", 50.0),
            crate::widgets::tests_support::process(87891, "a\u{9b}31mb\u{202e}txt", 40.0),
        ];
        for (w, h) in [(80, 24), (120, 40), (160, 45)] {
            let text = paint(&fx.view(), w, h);
            assert!(
                text.lines()
                    .all(|row| !row.chars().any(siltop_core::is_unsafe)),
                "{w}x{h}: control character painted"
            );
            assert!(
                text.contains("]2;RVBC"),
                "{w}x{h}: the rest of the name still shows"
            );
        }
    }

    fn paint(view: &super::AppView<'_>, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render_app(frame, view)).unwrap();
        let buf = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn work_paints_separate_boxes() {
        let mut fx = fixture("");
        fx.snap.cpu.scaled = 0.184;
        fx.snap.cpu.active = 0.184;
        fx.snap.cpu.watts = Some(8.24);
        fx.snap.cpu.temp_c = Some(42.0);
        fx.snap.memory.used_bytes = 18 * 1024 * 1024 * 1024;
        fx.snap.memory.total_bytes = 36 * 1024 * 1024 * 1024;
        fx.snap.gpu = Some(GpuSnapshot {
            scaled: 0.12,
            watts: Some(1.1),
            freq_mhz: Some(461),
            temp_c: Some(51.0),
            ..GpuSnapshot::default()
        });
        fx.snap.disk = DiskSnapshot {
            volumes: vec![DiskVolume {
                name: String::from("Macintosh HD"),
                mount: String::from("/"),
                used_bytes: 400 * 1024 * 1024 * 1024,
                total_bytes: 926 * 1024 * 1024 * 1024,
            }],
            read_bps: 0,
            write_bps: 0,
            devices: Vec::new(),
        };
        fx.snap.fans = FanSnapshot {
            fans: vec![FanMetric {
                name: String::from("Fan"),
                rpm: 1850,
                max_rpm: 6000,
            }],
        };
        fx.ready = true;
        let text = paint(&fx.view(), 120, 36);
        assert!(text.contains("cpu"), "{text}");
        assert!(text.contains("18%"), "{text}");
        assert!(text.contains("gpu"), "{text}");
        assert!(text.contains("51°"), "{text}");
        assert!(text.contains('↗'), "{text}");
        assert!(text.contains("mem"), "{text}");
        assert!(text.contains("net"), "{text}");
        assert!(text.contains("disk"), "{text}");
        assert!(text.contains("heat"), "{text}");
        assert!(text.contains("Xcode"), "{text}");
        assert!(text.contains("help"), "{text}");
        assert!(!text.contains("nominal"), "{text}");
    }

    #[test]
    fn flags_has_fans_when_sensors_present() {
        let mut fx = fixture("");
        assert!(!fx.view().flags().has_fans);
        fx.snap.sensors.cpu_c = Some(42.0);
        assert!(fx.view().flags().has_fans);
        assert!(fx.view().flags().visible(Panel::Fans));
    }

    #[test]
    fn sensors_box_shows_without_fans() {
        let mut fx = fixture("");
        fx.snap.sensors.cpu_c = Some(42.0);
        let text = paint(&fx.view(), 120, 36);
        assert!(text.contains("heat"), "{text}");
        assert!(text.contains("42°"), "{text}");
    }

    #[test]
    fn glance_is_compact_cluster() {
        let mut fx = fixture("");
        fx.surface = Surface::Glance;
        fx.snap.cpu.scaled = 0.18;
        fx.snap.cpu.active = 0.18;
        fx.snap.cpu.watts = Some(8.2);
        fx.snap.gpu = Some(GpuSnapshot {
            scaled: 0.12,
            watts: Some(1.1),
            freq_mhz: Some(461),
            ..GpuSnapshot::default()
        });
        let text = paint(&fx.view(), 80, 20);
        assert!(text.contains("cpu"), "{text}");
        assert!(text.contains("gpu"), "{text}");
        assert!(text.contains("mem"), "{text}");
        assert!(!text.contains("nominal"), "{text}");
        assert!(!text.contains("Xcode"), "{text}");
    }

    #[test]
    fn work_request_rendered_inside_the_panic_window_is_glance() {
        // Regression for the 40..59 column clamp panic: asking for Work here
        // must fall back and still paint a complete frame.
        let mut fx = fixture("");
        fx.ready = true;
        fx.snap.cpu.active = 0.3;
        for (width, height) in [(45_u16, 20_u16), (59, 16), (41, 24)] {
            let text = paint(&fx.view(), width, height);
            assert!(text.contains("cpu"), "{width}x{height}: {text}");
            assert!(text.contains("mem"), "{width}x{height}: {text}");
        }
    }

    #[test]
    fn tight_work_drops_fan_graph_but_keeps_headline() {
        let mut fx = fixture("");
        fx.ready = true;
        fx.snap.cpu.scaled = 0.3;
        fx.snap.sensors.cpu_c = Some(42.0);
        fx.snap.fans = FanSnapshot {
            fans: vec![FanMetric {
                name: String::from("Fan 1"),
                rpm: 1200,
                max_rpm: 6000,
            }],
        };
        let view = fx.view();
        let text = paint(&view, 120, 30);
        assert!(text.contains("heat  1200rpm"), "{text}");
        assert!(
            text.contains("42°"),
            "cpu °C stays in the cpu title: {text}"
        );
        assert!(
            !text.contains("related"),
            "compact never shows related: {text}"
        );
    }

    /// Regression (owner feedback): the top settings bar is gone; the
    /// graphs own row 0 on every surface and size.
    #[test]
    fn no_top_bar_row_zero_is_a_panel_border() {
        let fx = fixture("");
        for (w, h) in [(60, 16), (80, 24), (120, 40), (160, 45), (220, 50)] {
            for surface in [Surface::Work, Surface::Glance] {
                let view = super::AppView {
                    surface,
                    ..fx.view()
                };
                let screen = paint(&view, w, h);
                let row0 = screen.lines().next().unwrap();
                assert!(row0.starts_with('╭'), "{w}x{h} {surface:?}: {row0}");
                assert!(!row0.contains("settings"), "{w}x{h}: {row0}");
                let last = screen.lines().last().unwrap();
                assert!(
                    last.contains("≡"),
                    "{w}x{h}: menu button in the footer: {last}"
                );
            }
        }
    }

    #[test]
    fn open_menu_paints_over_the_dashboard() {
        let mut fx = fixture("");
        fx.menu = Some(0);
        for (w, h) in [(60, 16), (80, 24), (160, 45)] {
            let text = paint(&fx.view(), w, h);
            for item in &fx.menu_entries {
                assert!(
                    text.contains(&item.label),
                    "{w}x{h}: {}\n{text}",
                    item.label
                );
            }
        }
    }

    #[test]
    fn settings_screen_paints_no_dashboard() {
        let mut fx = fixture("");
        fx.settings = true;
        for (w, h) in [(160, 45), (100, 30), (80, 24), (60, 16)] {
            let text = paint(&fx.view(), w, h);
            assert!(text.contains("settings"), "{text}");
            assert!(text.contains("sample every"), "{w}x{h}\n{text}");
            assert!(!text.contains('╭'), "a panel box shows through:\n{text}");
            assert!(!text.contains("Xcode"), "a process shows through:\n{text}");
        }
    }

    #[test]
    fn settings_appearance_previews_every_theme() {
        let mut fx = fixture("");
        fx.settings = true;
        fx.settings_state = super::SettingsState::open(super::SettingsCategory::Appearance);
        let text = paint(&fx.view(), 120, 40);
        for name in crate::theme::Theme::builtins() {
            assert!(text.contains(name), "{name} missing:\n{text}");
        }
        assert!(text.contains("follows your terminal"), "{text}");
    }

    #[test]
    fn settings_about_shows_version_path_and_history() {
        let mut fx = fixture("");
        fx.settings = true;
        fx.settings_state = super::SettingsState::open(super::SettingsCategory::About);
        let text = paint(&fx.view(), 100, 30);
        assert!(text.contains(env!("CARGO_PKG_VERSION")), "{text}");
        assert!(text.contains("config.toml"), "{text}");
        assert!(text.contains("30 days"), "{text}");
    }

    #[test]
    fn minimal_work_strips_specs_and_graphs() {
        let mut fx = fixture("");
        fx.ready = true;
        fx.snap.cpu.scaled = 0.3;
        fx.snap.memory.wired_bytes = 8 * 1024 * 1024 * 1024;
        fx.snap.cpu.temp_c = Some(42.0);
        let view = fx.view();
        let screen = paint(&view, 100, 26);
        // the top bar names the chip; the cpu box must not
        let text: String = screen.lines().skip(1).collect::<Vec<_>>().join("\n");
        assert!(text.contains("mem"), "{text}");
        assert!(
            !text.contains("wired"),
            "specs must collapse at Minimal: {text}"
        );
        assert!(
            !text.contains("M4 Pro"),
            "cpu spec subline must collapse at Minimal: {text}"
        );
    }

    #[test]
    fn expand_cpu_is_a_grid_of_cells() {
        let mut fx = fixture("");
        fx.expanded = Some(Panel::Cpu);
        fx.snap.cpu.scaled = 0.5;
        fx.snap.cpu.active = 0.7;
        fx.snap.cpu.cores = vec![
            CoreSample {
                kind: ClusterKind::Efficiency,
                index: 0,
                scaled: 0.2,
                active: 0.2,
                freq_mhz: None,
            },
            CoreSample {
                kind: ClusterKind::Performance,
                index: 0,
                scaled: 0.8,
                active: 0.8,
                freq_mhz: None,
            },
        ];
        fx.snap.sensors.e_c = Some(36.0);
        fx.snap.sensors.p_c = Some(51.0);
        fx.histories.get_mut(SeriesId::ETemp).push(36.0);
        fx.histories.get_mut(SeriesId::PTemp).push(51.0);
        fx.histories.get_mut(SeriesId::ELoad).push(0.2);
        fx.histories.get_mut(SeriesId::PLoad).push(0.8);
        let text = paint(&fx.view(), 80, 24);
        for want in [
            "cpu",
            "efficiency",
            "performance",
            "eff zone",
            "perf zone",
            "36°",
            "51°",
        ] {
            assert!(text.contains(want), "missing {want}: {text}");
        }
        let outer = text.lines().next().unwrap_or_default();
        assert!(
            !outer.contains('%'),
            "outer title is context, not numbers: {outer}"
        );
        assert!(
            text.contains("E0"),
            "per-core % belongs on CPU expand: {text}"
        );
        assert!(text.contains("20%"), "{text}");
        assert!(!text.contains("power"), "empty power cell: {text}");
        assert!(!text.contains("clock"), "empty clock cell: {text}");
        assert!(
            !text.contains("Macintosh"),
            "other panels must hide: {text}"
        );
        assert!(!text.contains("Xcode"), "procs stay in their pane: {text}");
    }

    #[test]
    fn expand_cpu_tall_without_zones_fills() {
        let mut fx = fixture("");
        fx.expanded = Some(Panel::Cpu);
        fx.snap.gpu = Some(GpuSnapshot {
            scaled: 0.16,
            ..GpuSnapshot::default()
        });
        fx.snap.cpu.cores = vec![CoreSample {
            kind: ClusterKind::Performance,
            index: 0,
            scaled: 0.4,
            active: 0.4,
            freq_mhz: None,
        }];
        fx.snap.sensors.e_c = None;
        fx.snap.sensors.p_c = None;
        fx.snap.sensors.s_c = None;
        fx.snap.cpu.temp_c = None;
        for v in [0.1, 0.2, 0.3, 0.4, 0.5] {
            fx.histories.get_mut(SeriesId::PLoad).push(v);
            fx.histories.get_mut(SeriesId::Cpu).push(v);
        }
        let text = paint(&fx.view(), 160, 44);
        assert!(text.contains("performance"), "{text}");
        assert!(!text.contains("zone"), "{text}");
        let has_spark = text.chars().any(|c| ('\u{2801}'..='\u{28FF}').contains(&c));
        assert!(has_spark, "usage graph must paint: {text}");
        let last = text.lines().nth(41).unwrap_or("");
        assert!(
            last.chars()
                .any(|c| ('\u{2801}'..='\u{28FF}').contains(&c) || c == '│'),
            "bottom of a 44-row expand must still be graph or border, not empty: {last}"
        );
    }

    #[test]
    fn expand_cpu_wide_lists_per_core_percent() {
        let mut fx = fixture("");
        fx.expanded = Some(Panel::Cpu);
        fx.snap.gpu = Some(GpuSnapshot {
            scaled: 0.16,
            ..GpuSnapshot::default()
        });
        fx.snap.cpu.cores = vec![
            CoreSample {
                kind: ClusterKind::Super,
                index: 0,
                scaled: 0.81,
                active: 0.81,
                freq_mhz: None,
            },
            CoreSample {
                kind: ClusterKind::Performance,
                index: 0,
                scaled: 0.44,
                active: 0.44,
                freq_mhz: None,
            },
        ];
        fx.snap.sensors.s_c = Some(71.0);
        fx.histories.get_mut(SeriesId::STemp).push(71.0);
        fx.histories.get_mut(SeriesId::SLoad).push(0.81);
        fx.histories.get_mut(SeriesId::PLoad).push(0.44);
        let text = paint(&fx.view(), 160, 40);
        assert!(text.contains("S0"), "super core label: {text}");
        assert!(text.contains("81%"), "{text}");
        assert!(text.contains("P0"), "perf core label: {text}");
        assert!(text.contains("44%"), "{text}");
        assert!(text.contains("super"), "{text}");
        assert!(text.contains("performance"), "{text}");
    }

    #[test]
    fn expand_gpu_is_stats_over_two_graphs() {
        let mut fx = fixture("");
        fx.expanded = Some(Panel::Gpu);
        fx.ready = true;
        fx.snap.gpu = Some(GpuSnapshot {
            scaled: 0.12,
            watts: Some(1.1),
            freq_mhz: Some(461),
            temp_c: Some(51.0),
            ..GpuSnapshot::default()
        });
        fx.histories.get_mut(SeriesId::GpuTemp).push(51.0);
        let text = paint(&fx.view(), 80, 24);
        for want in ["gpu", "gpu temp", "12%", "51°"] {
            assert!(text.contains(want), "missing {want}: {text}");
        }
        assert!(text.contains("1.1W") || text.contains("1.1 W"), "{text}");
        assert!(!text.contains("no readings on this machine"), "{text}");
    }

    #[test]
    fn expand_cpu_hides_empty_power_and_clock() {
        let mut fx = fixture("");
        fx.expanded = Some(Panel::Cpu);
        fx.snap.cpu.scaled = 0.4;
        fx.snap.cpu.active = 0.4;
        fx.snap.cpu.watts = None;
        fx.snap.cpu.freq_mhz = None;
        fx.snap.cpu.cores = vec![CoreSample {
            kind: ClusterKind::Performance,
            index: 0,
            scaled: 0.4,
            active: 0.4,
            freq_mhz: None,
        }];
        let text = paint(&fx.view(), 80, 24);
        assert!(text.contains("cpu"), "{text}");
        assert!(text.contains("performance"), "{text}");
        assert!(!text.contains("power"), "{text}");
        assert!(!text.contains("clock"), "{text}");
    }

    #[test]
    fn expand_gpu_hides_empty_optional_cells() {
        let mut fx = fixture("");
        fx.expanded = Some(Panel::Gpu);
        fx.ready = true;
        fx.snap.gpu = Some(GpuSnapshot {
            scaled: 0.22,
            ..GpuSnapshot::default()
        });
        fx.snap.soc.gpu_cores = 0;
        let text = paint(&fx.view(), 80, 24);
        assert!(text.contains("gpu"), "{text}");
        assert!(text.contains("22%"), "{text}");
        assert!(!text.contains("power"), "{text}");
        assert!(!text.contains("clock"), "{text}");
        assert!(!text.contains("gpu temp"), "{text}");
    }

    #[test]
    fn expand_disk_graphs_io_not_capacity() {
        let mut fx = fixture("");
        fx.expanded = Some(Panel::Disk);
        fx.snap.disk = DiskSnapshot {
            volumes: vec![DiskVolume {
                name: String::from("Macintosh HD"),
                mount: String::from("/"),
                used_bytes: 400 * 1024 * 1024 * 1024,
                total_bytes: 926 * 1024 * 1024 * 1024,
            }],
            read_bps: 2 * 1024 * 1024,
            write_bps: 512 * 1024,
            devices: Vec::new(),
        };
        let text = paint(&fx.view(), 80, 24);
        assert!(text.contains("volumes"), "{text}");
        assert!(text.contains("Macintosh HD"), "{text}");
        assert!(text.contains("read"), "{text}");
        assert!(text.contains("write"), "{text}");
        // Decimal units, like Activity Monitor: 2 MiB/s reads 2.1MB/s.
        assert!(text.contains("2.1MB/s"), "{text}");
        assert!(!text.contains("primary volume"), "{text}");
    }

    #[test]
    fn expand_sensors_graphs_each_fan() {
        let mut fx = fixture("");
        fx.expanded = Some(Panel::Fans);
        fx.snap.fans = FanSnapshot {
            fans: vec![
                FanMetric {
                    name: String::from("Fan 1"),
                    rpm: 1200,
                    max_rpm: 6000,
                },
                FanMetric {
                    name: String::from("Fan 2"),
                    rpm: 1850,
                    max_rpm: 6000,
                },
            ],
        };
        fx.snap.sensors.cpu_c = Some(42.0);
        fx.snap.sensors.p_c = Some(51.0);
        fx.snap.sensors.s_c = Some(62.0);
        fx.histories.get_mut(SeriesId::PTemp).push(51.0);
        fx.histories.get_mut(SeriesId::STemp).push(62.0);
        *fx.histories.fans_mut() = vec![{
            let mut h = History::default();
            for rpm in [800.0, 1000.0, 1200.0] {
                h.push(rpm);
            }
            h
        }];
        fx.histories.get_mut(SeriesId::CpuTemp).push(40.0);
        fx.histories.get_mut(SeriesId::CpuTemp).push(42.0);
        let text = paint(&fx.view(), 100, 30);
        assert!(text.contains("fan 1"), "{text}");
        assert!(text.contains("fan 2"), "{text}");
        assert!(text.contains("1200rpm"), "{text}");
        assert!(text.contains("1850rpm"), "{text}");
        assert!(text.contains("perf zone"), "{text}");
        assert!(text.contains("super zone"), "{text}");
        assert!(text.contains("51°"), "{text}");
        assert!(text.contains("62°"), "{text}");
        assert!(!text.contains("E0 36°"), "{text}");
    }

    fn last_braille_row(buf: &ratatui::buffer::Buffer, rect: ratatui::layout::Rect) -> Option<u16> {
        let end = rect.y.saturating_add(rect.height);
        (rect.y..end).rev().find(|&y| {
            (rect.x..rect.x.saturating_add(rect.width)).any(|x| {
                buf[(x, y)]
                    .symbol()
                    .chars()
                    .next()
                    .is_some_and(|c| ('\u{2801}'..='\u{28FF}').contains(&c))
            })
        })
    }

    fn row_text(buf: &ratatui::buffer::Buffer, rect: ratatui::layout::Rect, y: u16) -> String {
        let mut out = String::new();
        for x in rect.x..rect.x.saturating_add(rect.width) {
            out.push_str(buf[(x, y)].symbol());
        }
        out
    }

    #[test]
    fn compact_sibling_graphs_share_a_baseline() {
        let mut fx = fixture("");
        fx.ready = true;
        fx.snap.gpu = Some(GpuSnapshot {
            scaled: 0.16,
            ..GpuSnapshot::default()
        });
        fx.snap.sensors.e_c = Some(48.0);
        fx.snap.sensors.p_c = Some(62.0);
        fx.snap.memory.used_bytes = 18 * 1024 * 1024 * 1024;
        fx.snap.memory.total_bytes = 36 * 1024 * 1024 * 1024;
        fx.snap.disk = DiskSnapshot {
            volumes: vec![DiskVolume {
                name: String::from("Macintosh HD"),
                mount: String::from("/"),
                used_bytes: 400 * 1024 * 1024 * 1024,
                total_bytes: 926 * 1024 * 1024 * 1024,
            }],
            read_bps: 0,
            write_bps: 0,
            devices: Vec::new(),
        };
        for v in [0.12, 0.24, 0.36, 0.48] {
            fx.histories.get_mut(SeriesId::Cpu).push(v);
            fx.histories.get_mut(SeriesId::Gpu).push(v * 0.5);
            fx.histories.get_mut(SeriesId::Mem).push(v);
            fx.histories
                .get_mut(SeriesId::CpuTemp)
                .push(40.0 + v * 30.0);
            fx.histories.get_mut(SeriesId::NetRx).push(v * 8_000.0);
            fx.histories.get_mut(SeriesId::NetTx).push(v * 8_000.0);
            fx.histories.get_mut(SeriesId::Disk).push(v * 1_024.0);
            fx.histories.get_mut(SeriesId::DiskRead).push(v * 1_024.0);
            fx.histories.get_mut(SeriesId::DiskWrite).push(v * 1_024.0);
        }
        let view = fx.view();
        let area = ratatui::layout::Rect::new(0, 0, 140, 36);
        let layout = crate::layout::plan(area, Surface::Work, view.flags());
        let backend = TestBackend::new(140, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render_app(frame, &view)).unwrap();
        let buf = terminal.backend().buffer();

        let last_inner =
            |rect: ratatui::layout::Rect| rect.y.saturating_add(rect.height).saturating_sub(2);
        let cpu = layout.cpu.expect("cpu");
        let gpu = layout.gpu.expect("gpu");
        assert_eq!(cpu.y, gpu.y);
        assert_eq!(
            last_braille_row(buf, cpu),
            Some(last_inner(cpu)),
            "cpu graph must fill to the bottom border"
        );
        assert_eq!(
            last_braille_row(buf, gpu),
            Some(last_inner(gpu)),
            "gpu graph must fill to the bottom border"
        );

        let mem = layout.mem.expect("mem");
        let fans = layout.fans.expect("sens");
        assert_eq!(mem.y, fans.y);
        assert_eq!(last_braille_row(buf, mem), Some(last_inner(mem)));
        assert_eq!(last_braille_row(buf, fans), Some(last_inner(fans)));
        let fans_inner = row_text(buf, fans, fans.y.saturating_add(1));
        assert!(
            !fans_inner.contains("e 48") && !fans_inner.contains("48°"),
            "sens numbers stay in the title, not a row above the graph: {fans_inner}"
        );

        let net = layout.net.expect("net");
        let disk = layout.disk.expect("disk");
        assert_eq!(net.y, disk.y);
        // mirrored: download/read rise from the midline, upload/write hang below it
        for rect in [net, disk] {
            let inner_h = rect.height.saturating_sub(2);
            let (up, _) = crate::widgets::mirror::split(inner_h);
            let midline = rect.y + up;
            let lowest = last_braille_row(buf, rect).expect("mirror paints");
            assert!(
                lowest > midline,
                "lower half must paint: {lowest} vs {midline}"
            );
        }
    }

    #[test]
    fn compact_sens_sparks_at_80x24_minimal() {
        let mut fx = fixture("");
        fx.snap.sensors.e_c = Some(48.0);
        fx.snap.sensors.p_c = Some(62.0);
        fx.snap.sensors.s_c = Some(71.0);
        fx.histories.get_mut(SeriesId::CpuTemp).push(40.0);
        fx.histories.get_mut(SeriesId::CpuTemp).push(71.0);
        let view = fx.view();
        let layout = crate::layout::plan(
            ratatui::layout::Rect::new(0, 0, 80, 24),
            Surface::Work,
            view.flags(),
        );
        assert!(layout.fans.is_some());
        let text = paint(&view, 80, 24);
        assert!(
            text.contains("heat  71°"),
            "hottest zone rides in the title: {text}"
        );
        let has_spark = text.chars().any(|c| ('\u{2801}'..='\u{28FF}').contains(&c));
        assert!(has_spark, "SENS at 80x24 Minimal must keep a spark: {text}");
    }

    #[test]
    fn glance_sens_is_numbers_not_spark() {
        let mut fx = fixture("");
        fx.surface = Surface::Glance;
        fx.snap.sensors.e_c = Some(48.0);
        fx.snap.sensors.p_c = Some(62.0);
        fx.histories.get_mut(SeriesId::CpuTemp).push(48.0);
        fx.histories.get_mut(SeriesId::CpuTemp).push(62.0);
        let text = paint(&fx.view(), 80, 20);
        assert!(text.contains("peak 62°"), "{text}");
    }

    #[test]
    fn expand_sens_shows_cpu_gpu_hops() {
        let mut fx = fixture("");
        fx.expanded = Some(Panel::Fans);
        fx.snap.gpu = Some(GpuSnapshot {
            scaled: 0.31,
            ..GpuSnapshot::default()
        });
        fx.snap.cpu.scaled = 0.44;
        fx.snap.sensors.p_c = Some(51.0);
        fx.histories.get_mut(SeriesId::PTemp).push(51.0);
        let text = paint(&fx.view(), 100, 30);
        assert!(text.contains("cpu"), "{text}");
        assert!(text.contains("gpu"), "{text}");
        assert!(text.contains('→'), "{text}");
        assert!(text.contains("44%") || text.contains("31%"), "{text}");
    }

    #[test]
    fn expand_cpu_fan_hop_shows_rpm() {
        let mut fx = fixture("");
        fx.expanded = Some(Panel::Cpu);
        fx.snap.cpu.cores = vec![CoreSample {
            kind: ClusterKind::Performance,
            index: 0,
            scaled: 0.4,
            active: 0.4,
            freq_mhz: None,
        }];
        fx.snap.fans = FanSnapshot {
            fans: vec![
                FanMetric {
                    name: String::from("Fan 1"),
                    rpm: 1200,
                    max_rpm: 6000,
                },
                FanMetric {
                    name: String::from("Fan 2"),
                    rpm: 2140,
                    max_rpm: 6000,
                },
            ],
        };
        let text = paint(&fx.view(), 80, 24);
        assert!(text.contains("rpm"), "fan hop must show rpm: {text}");
        assert!(
            text.contains("max 2140rpm"),
            "two fans use the max title: {text}"
        );
        assert!(text.contains("fan"), "{text}");
    }

    #[test]
    fn expand_cpu_idle_fan_shows_zero_rpm() {
        let mut fx = fixture("");
        fx.expanded = Some(Panel::Cpu);
        fx.snap.cpu.cores = vec![CoreSample {
            kind: ClusterKind::Performance,
            index: 0,
            scaled: 0.2,
            active: 0.2,
            freq_mhz: None,
        }];
        fx.snap.fans = FanSnapshot {
            fans: vec![FanMetric {
                name: String::from("Fan 1"),
                rpm: 0,
                max_rpm: 6000,
            }],
        };
        let text = paint(&fx.view(), 80, 24);
        assert!(text.contains("0rpm"), "idle fan is a real number: {text}");
    }

    #[test]
    fn hop_hit_on_related_cell() {
        let mut fx = fixture("");
        fx.expanded = Some(Panel::Cpu);
        fx.snap.gpu = Some(GpuSnapshot {
            scaled: 0.2,
            ..GpuSnapshot::default()
        });
        fx.snap.fans = FanSnapshot {
            fans: vec![FanMetric {
                name: String::from("Fan 1"),
                rpm: 1000,
                max_rpm: 6000,
            }],
        };
        let view = fx.view();
        let area = ratatui::layout::Rect::new(0, 0, 80, 23);
        let hit = (0..area.height).find_map(|y| {
            (0..area.width).find_map(|x| {
                crate::widgets::hop_hit(area, &view, x, y).filter(|p| *p == Panel::Gpu)
            })
        });
        assert_eq!(hit, Some(Panel::Gpu), "a gpu hop cell must exist");

        let wide = ratatui::layout::Rect::new(0, 0, 160, 40);
        let wide_hit = (0..wide.height).find_map(|y| {
            (0..26).find_map(|x| {
                crate::widgets::hop_hit(wide, &view, x, y).filter(|p| *p == Panel::Gpu)
            })
        });
        assert_eq!(
            wide_hit,
            Some(Panel::Gpu),
            "wide expand parks hops in the left column"
        );
    }

    #[test]
    fn expand_cpu_honors_show_cores_off() {
        let mut fx = fixture("");
        fx.expanded = Some(Panel::Cpu);
        fx.snap.cpu.cores = vec![CoreSample {
            kind: ClusterKind::Efficiency,
            index: 0,
            scaled: 0.3,
            active: 0.3,
            freq_mhz: None,
        }];
        let mut view = fx.view();
        view.show_cores = false;
        let text = paint(&view, 80, 24);
        assert!(text.contains("efficiency"), "{text}");
        assert!(!text.contains("E0"), "{text}");
    }

    #[test]
    fn expand_cpu_no_cluster_keeps_overall_graph() {
        let mut fx = fixture("");
        fx.expanded = Some(Panel::Cpu);
        fx.snap.cpu.scaled = 0.33;
        fx.snap.cpu.cores.clear();
        let text = paint(&fx.view(), 80, 24);
        assert!(text.contains("33%") || text.contains("cpu"), "{text}");
    }

    #[test]
    fn hop_hit_mem_labelonly_is_one_line() {
        let mut fx = fixture("");
        fx.expanded = Some(Panel::Mem);
        let view = fx.view();
        let area = ratatui::layout::Rect::new(0, 0, 80, 23);
        let hit = (0..area.width).find_map(|x| crate::widgets::hop_hit(area, &view, x, 21));
        assert_eq!(hit, Some(Panel::Processes));
        assert_eq!(
            crate::widgets::hop_hit(area, &view, 2, 6),
            None,
            "clicks above the one-line hop must not jump"
        );
    }

    #[test]
    fn expand_cpu_no_double_cluster() {
        let mut fx = fixture("");
        fx.expanded = Some(Panel::Cpu);
        fx.snap.cpu.cores = vec![CoreSample {
            kind: ClusterKind::Performance,
            index: 0,
            scaled: 0.5,
            active: 0.5,
            freq_mhz: None,
        }];
        fx.snap.sensors.p_c = Some(51.0);
        fx.histories.get_mut(SeriesId::PTemp).push(51.0);
        let text = paint(&fx.view(), 80, 24);
        assert!(text.contains("perf zone"), "{text}");
        assert!(
            text.contains("P0"),
            "live per-core % sits in the metadata region: {text}"
        );
    }

    #[test]
    fn expand_sens_keeps_extra_readings_on_wide() {
        let mut fx = fixture("");
        fx.expanded = Some(Panel::Fans);
        fx.snap.sensors.readings = vec![siltop_core::TempReading {
            name: String::from("nand"),
            celsius: 38.0,
        }];
        let text = paint(&fx.view(), 160, 40);
        assert!(
            text.contains("nand"),
            "extras belong in the readings list: {text}"
        );
        assert!(text.contains("38°"), "{text}");
    }

    #[test]
    fn detail_popup_shows_identity_and_actions() {
        let mut fx = fixture("");
        fx.proc.anchor = super::Anchor::Pinned(904);
        fx.snap.processes = vec![crate::widgets::tests_support::process(904, "claude", 12.5)];
        fx.snap.processes[0].command = Some(String::from("/Users/u/.local/bin/claude"));
        fx.snap.processes[0].start_unix = 1_700_000_000;
        fx.expanded = None;
        let mut view = fx.view();
        view.detail_pid = Some(904);
        let text = paint(&view, 100, 30);
        for want in [
            "process",
            "@cloaky",
            "/Users/u/.local/bin/claude",
            "sleeping",
            "quit",
            "force quit",
            "interrupt",
            "esc close",
        ] {
            assert!(text.contains(want), "missing {want}: {text}");
        }
    }

    #[test]
    fn help_overlay_lists_keys() {
        let mut fx = fixture("");
        fx.help = true;
        let text = paint(&fx.view(), 80, 24);
        assert!(text.contains("help"), "{text}");
        assert!(text.contains("general"), "{text}");
        assert!(text.contains("q ctrl-c"), "{text}");
        assert!(text.contains("quit"), "{text}");
    }
}

#[cfg(test)]
mod visual_dump {
    use crate::layout::Panel;
    use crate::widgets::render_app;
    use crate::widgets::tests_support::fixture;
    use ratatui::{Terminal, backend::TestBackend};
    use siltop_core::*;

    fn paint(view: &crate::widgets::AppView<'_>, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| render_app(f, view)).unwrap();
        let b = t.backend().buffer();
        let mut out = String::new();
        for y in 0..b.area.height {
            out.push('|');
            for x in 0..b.area.width {
                out.push_str(b[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    #[ignore = "visual tool: run with --ignored --nocapture"]
    fn dump_detail_popup() {
        use crate::widgets::tests_support::process;
        let mut fx = fixture("");
        fx.ready = true;
        fx.snap.processes = vec![process(904, "claude", 12.5)];
        fx.snap.processes[0].user = String::from("cloaky");
        fx.snap.processes[0].command = Some(String::from(
            "/Users/cloaky/.local/share/claude/versions/2.1.241/bin/claude",
        ));
        fx.snap.processes[0].start_unix = 1_755_000_000;
        fx.snap.processes[0].mem_bytes = Some(544 * 1024 * 1024);
        let mut view = fx.view();
        view.detail_pid = Some(904);
        println!("{}", paint(&view, 100, 30));
    }

    #[test]
    #[ignore = "visual tool: run with --ignored --nocapture"]
    fn dump_all_expanded() {
        for (panel, name) in [
            (Panel::Cpu, "CPU"),
            (Panel::Gpu, "GPU"),
            (Panel::Mem, "MEM"),
            (Panel::Net, "NET"),
            (Panel::Disk, "DISK"),
            (Panel::Fans, "SENS"),
        ] {
            let mut fx = fixture("");
            fx.ready = true;
            fx.expanded = Some(panel);
            fx.snap.cpu.scaled = 0.42;
            fx.snap.cpu.active = 0.61;
            fx.snap.cpu.watts = Some(9.3);
            fx.snap.cpu.cores = (0..10)
                .map(|i| CoreSample {
                    kind: if i < 4 {
                        ClusterKind::Efficiency
                    } else {
                        ClusterKind::Performance
                    },
                    index: i % 6,
                    scaled: f32::from(i) * 0.07 + 0.15,
                    active: f32::from(i) * 0.06 + 0.2,
                    freq_mhz: None,
                })
                .collect();
            fx.snap.sensors.e_c = Some(38.0);
            fx.snap.sensors.p_c = Some(58.0);
            fx.snap.gpu = Some(GpuSnapshot {
                scaled: 0.31,
                watts: Some(2.2),
                ane_watts: Some(0.4),
                freq_mhz: Some(1278),
                temp_c: Some(49.0),
                ..Default::default()
            });
            fx.snap.memory.used_bytes = 22 * 1024 * 1024 * 1024;
            fx.snap.memory.total_bytes = 36 * 1024 * 1024 * 1024;
            fx.snap.memory.wired_bytes = 7 * 1024 * 1024 * 1024;
            fx.snap.memory.compressed_bytes = 2 * 1024 * 1024 * 1024;
            fx.snap.memory.cache_bytes = 3 * 1024 * 1024 * 1024;
            fx.snap.memory.swap_used_bytes = 512 * 1024 * 1024;
            fx.snap.memory.swap_total_bytes = 4 * 1024 * 1024 * 1024;
            fx.snap.network.iface = "en0".into();
            fx.snap.network.rx_bps = 12_400_000;
            fx.snap.network.tx_bps = 890_000;
            fx.snap.disk.volumes.push(DiskVolume {
                name: "Macintosh HD".into(),
                mount: "/".into(),
                used_bytes: 400 * 1024 * 1024 * 1024,
                total_bytes: 926 * 1024 * 1024 * 1024,
            });
            fx.snap.fans.fans.push(FanMetric {
                name: "Fan 1".into(),
                rpm: 1720,
                max_rpm: 6000,
            });
            fx.snap.fans.fans.push(FanMetric {
                name: "Fan 2".into(),
                rpm: 2140,
                max_rpm: 5800,
            });
            let text = paint(&fx.view(), 100, 30);
            println!("\n===== {name} =====\n{text}");
        }
    }

    fn zen_fixture(zen: bool) -> super::tests_support::Fixture {
        let mut fx = fixture("");
        fx.snap.memory.used_bytes = 18 * 1024 * 1024 * 1024;
        fx.snap.memory.total_bytes = 36 * 1024 * 1024 * 1024;
        fx.snap.network.rx_bps = 4_000_000;
        for v in [1_000_000.0, 3_000_000.0, 2_000_000.0] {
            fx.histories.get_mut(SeriesId::NetRx).push(v);
            fx.histories.get_mut(SeriesId::NetTx).push(v);
        }
        fx.config.zen = zen;
        fx
    }

    /// The footer row's ink, with this module's `|`/`·`/`␊` markers removed.
    fn last_row(text: &str) -> String {
        text.lines()
            .rfind(|l| l.starts_with('|'))
            .unwrap_or_default()
            .chars()
            .filter(|c| !matches!(c, '|' | '·' | '␊' | ' '))
            .collect()
    }

    #[test]
    fn zen_hides_the_footer_and_the_graphs_take_its_row() {
        for (w, h) in [(80, 24), (120, 40), (160, 45)] {
            let calm = paint(&zen_fixture(false).view(), w, h);
            assert!(last_row(&calm).contains("help"), "footer at {w}x{h}");
            let zen = paint(&zen_fixture(true).view(), w, h);
            let last = last_row(&zen);
            assert!(!last.contains("help"), "{w}x{h}:\n{zen}");
            assert!(last.contains('╰'), "no empty last row at {w}x{h}:\n{zen}");
        }
    }

    #[test]
    fn zen_keeps_the_footer_while_paused() {
        let mut fx = zen_fixture(true);
        fx.frozen = true;
        let text = paint(&fx.view(), 120, 40);
        assert!(!last_row(&text).is_empty(), "{text}");
    }

    #[test]
    fn zen_footer_is_not_clickable() {
        let fx = zen_fixture(true);
        let view = fx.view();
        let area = ratatui::layout::Rect::new(0, 39, 120, 1);
        for col in 0..120 {
            assert!(super::footer_hit(&view, area, col, 39).is_none(), "{col}");
        }
    }

    #[test]
    fn zen_drops_axis_labels_from_borders() {
        let calm = paint(&zen_fixture(false).view(), 120, 40);
        let zen = paint(&zen_fixture(true).view(), 120, 40);
        let has_scale = |t: &str| t.contains("Mb/s") || t.contains("MB/s") || t.contains("kb/s");
        if has_scale(&calm) {
            assert!(!has_scale(&zen), "{zen}");
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod veil {
    use crate::widgets::tests_support::{Fixture, fixture};
    use crate::widgets::{AppView, SettingsState, modal_rect, render_app};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Modifier};
    use ratatui::{Terminal, backend::TestBackend};

    const SCREEN: Rect = Rect::new(0, 0, 120, 36);
    const OVERLAYS: [&str; 4] = ["help", "menu", "dossier", "confirm"];

    fn paint(view: &AppView<'_>) -> Buffer {
        let mut t = Terminal::new(TestBackend::new(SCREEN.width, SCREEN.height)).unwrap();
        t.draw(|f| render_app(f, view)).unwrap();
        t.backend().buffer().clone()
    }

    fn menu_of(fx: &Fixture, selected: usize) -> super::MenuView<'_> {
        super::MenuView {
            title: "siltop",
            entries: &fx.menu_entries,
            selected,
            top: 0,
            anchor: None,
        }
    }

    fn with<'a>(fx: &'a Fixture, which: &str) -> AppView<'a> {
        let mut view = fx.view();
        match which {
            "help" => view.help = true,
            "menu" => view.menu = Some(menu_of(fx, 1)),
            "dossier" => view.detail_pid = Some(904),
            "confirm" => view.confirm_kill = true,
            other => panic!("unknown overlay {other}"),
        }
        view
    }

    #[test]
    fn nothing_open_means_no_modal() {
        let fx = fixture("");
        assert_eq!(modal_rect(&fx.view(), SCREEN), None);
    }

    #[test]
    fn every_overlay_has_a_modal_rect_on_screen() {
        let fx = fixture("");
        for which in OVERLAYS {
            let modal = modal_rect(&with(&fx, which), SCREEN).unwrap();
            assert!(!modal.is_empty(), "{which}");
            assert_eq!(modal.intersection(SCREEN), modal, "{which}");
        }
    }

    #[test]
    fn help_outranks_menu_outranks_dossier() {
        let fx = fixture("");
        let mut view = with(&fx, "dossier");
        view.menu = Some(menu_of(&fx, 2));
        assert_eq!(
            modal_rect(&view, SCREEN),
            modal_rect(&with(&fx, "menu"), SCREEN)
        );
        view.help = true;
        assert_eq!(
            modal_rect(&view, SCREEN),
            modal_rect(&with(&fx, "help"), SCREEN)
        );
    }

    #[test]
    fn kill_confirmation_from_the_dossier_is_lit_not_veiled() {
        // Seen live: K in the dossier greyed out the Force-quit question
        // and left the dossier bright.
        let fx = fixture("");
        let mut view = with(&fx, "dossier");
        view.confirm_kill = true;
        let footer = modal_rect(&with(&fx, "confirm"), SCREEN);
        assert_eq!(modal_rect(&view, SCREEN), footer);
        let footer = footer.unwrap();
        let buf = paint(&view);
        assert!(
            footer
                .positions()
                .any(|p| buf[p].modifier.contains(Modifier::BOLD)),
            "the prompt keeps its bold keys"
        );
    }

    #[test]
    fn dossier_for_a_gone_process_is_not_modal() {
        let fx = fixture("");
        let mut view = fx.view();
        view.detail_pid = Some(1);
        assert_eq!(modal_rect(&view, SCREEN), None);
    }

    #[test]
    fn veil_dims_the_fg_of_every_cell_outside_the_modal() {
        let fx = fixture("");
        let plain = paint(&fx.view());
        for which in OVERLAYS {
            let view = with(&fx, which);
            let modal = modal_rect(&view, SCREEN).unwrap();
            let veiled = paint(&view);
            for pos in SCREEN.positions().filter(|p| !modal.contains(*p)) {
                let cell = &veiled[pos];
                assert!(!cell.modifier.contains(Modifier::BOLD), "{which} {pos:?}");
                // Where the overlay did not redraw the cell, the ink is
                // exactly the veiled form of the plain frame's ink.
                if cell.symbol() == plain[pos].symbol() && which != "confirm" {
                    assert_eq!(cell.fg, view.theme.veiled(plain[pos].fg), "{which} {pos:?}");
                }
            }
        }
    }

    #[test]
    fn kill_confirm_regression_keeps_the_armed_row_lit() {
        let fx = fixture("");
        let mut view = with(&fx, "confirm");
        view.confirm_pid = Some(904);
        let buf = paint(&view);
        let theme = view.theme;
        let armed: Vec<_> = SCREEN
            .positions()
            .filter(|p| buf[*p].bg == theme.kill_bg)
            .collect();
        assert!(!armed.is_empty(), "the armed row is on screen");
        for pos in armed {
            assert_eq!(buf[pos].fg, theme.title, "{pos:?} kept its alarm ink");
            assert!(buf[pos].modifier.contains(Modifier::BOLD), "{pos:?}");
        }
    }

    #[test]
    fn dossier_kill_regression_keeps_the_prompt_lit() {
        let fx = fixture("");
        let mut view = with(&fx, "dossier");
        view.confirm_kill = true;
        view.confirm_pid = Some(904);
        let from_card = paint(&view);
        view.detail_pid = None;
        let prompt = paint(&view);
        let footer = SCREEN.height - 1;
        for x in 0..SCREEN.width {
            let (card, alone) = (&from_card[(x, footer)], &prompt[(x, footer)]);
            assert_eq!(card.symbol(), alone.symbol(), "{x}");
            assert_eq!((card.fg, card.modifier), (alone.fg, alone.modifier), "{x}");
        }
    }

    #[test]
    fn no_color_regression_overlays_dim_what_is_behind_them() {
        let fx = fixture("");
        let theme = crate::theme::Theme::auto(None, crate::theme::ColorDepth::NoColor);
        for which in OVERLAYS {
            let mut view = with(&fx, which);
            view.theme = &theme;
            let modal = modal_rect(&view, SCREEN).unwrap();
            let buf = paint(&view);
            for pos in SCREEN.positions().filter(|p| !modal.contains(*p)) {
                let cell = &buf[pos];
                if !cell.symbol().trim().is_empty() && cell.bg == Color::Reset {
                    assert!(cell.modifier.contains(Modifier::DIM), "{which} {pos:?}");
                }
            }
        }
    }

    #[test]
    fn nerd_icon_regression_titles_leave_room_for_the_icon() {
        use crate::layout::Panel;
        type Paint = fn(&mut ratatui::Frame, Rect, &AppView<'_>, &crate::theme::Theme);
        let fx = crate::widgets::golden::m5_pro();
        let plain = crate::theme::Theme::default();
        let nerd = plain.clone().with_icons(crate::theme::Icons::Nerd);
        let top = |paint: Paint, theme: &crate::theme::Theme, w: u16| -> String {
            let view = AppView { theme, ..fx.view() };
            let mut t = Terminal::new(TestBackend::new(w, 8)).unwrap();
            t.draw(|f| paint(f, f.area(), &view, theme)).unwrap();
            let buf = t.backend().buffer().clone();
            (0..w).map(|x| buf[(x, 0)].symbol()).collect()
        };
        let panels: [(Panel, Paint); 5] = [
            (Panel::Cpu, super::cpu::render),
            (Panel::Gpu, super::gpu::render),
            (Panel::Mem, super::mem::render),
            (Panel::Net, super::net::render),
            (Panel::Fans, super::fans::render),
        ];
        for (panel, paint) in panels {
            let icon = format!(" {}", nerd.icons.panel(panel).unwrap());
            for w in 24..=120_u16 {
                // the icon costs the title exactly its own columns
                let with_icon = top(paint, &nerd, w).replacen(&icon, "", 1);
                assert_eq!(with_icon, top(paint, &plain, w - 2), "{panel:?} {w}");
            }
        }
    }

    #[test]
    fn veil_never_paints_a_background() {
        let fx = fixture("");
        let plain = paint(&fx.view());
        for which in OVERLAYS {
            let view = with(&fx, which);
            let modal = modal_rect(&view, SCREEN).unwrap();
            let veiled = paint(&view);
            for pos in SCREEN.positions().filter(|p| !modal.contains(*p)) {
                assert_eq!(veiled[pos].bg, plain[pos].bg, "{which} {pos:?}");
            }
        }
    }

    #[test]
    fn the_modal_itself_keeps_its_ink() {
        let fx = fixture("");
        let view = with(&fx, "help");
        let buf = paint(&view);
        let modal = modal_rect(&view, SCREEN).unwrap();
        assert_eq!(buf[(modal.x, modal.y)].fg, view.theme.hi);
    }

    #[test]
    fn help_over_settings_veils_the_settings_page() {
        let fx = fixture("");
        let state = SettingsState::default();
        let mut view = fx.view();
        view.settings = Some(&state);
        assert_eq!(modal_rect(&view, SCREEN), None, "settings alone is a page");
        view.help = true;
        let modal = modal_rect(&view, SCREEN).unwrap();
        let buf = paint(&view);
        for pos in SCREEN.positions().filter(|p| !modal.contains(*p)) {
            assert_ne!(buf[pos].fg, Color::Reset, "{pos:?}");
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod border_scale_tests {
    use super::frame_panel;
    use crate::chrome::BorderScale;
    use crate::layout::Panel;
    use crate::theme::Theme;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    /// A plot whose scale label is fixed, so only the width varies.
    struct Fixed(&'static str);

    impl BorderScale for Fixed {
        fn border_label(&self, _width: u16) -> Option<String> {
            Some(self.0.to_owned())
        }
    }

    fn top_row(panel: Panel, axis: &'static str, width: u16) -> String {
        let fx = crate::widgets::golden::m5_pro();
        let theme = Theme::default();
        let mut terminal = Terminal::new(TestBackend::new(width, 4)).unwrap();
        terminal
            .draw(|frame| {
                frame_panel(
                    frame,
                    Rect::new(0, 0, width, 4),
                    panel,
                    &fx.view(),
                    &theme,
                    Some(&Fixed(axis)),
                );
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        (0..width).map(|x| buf[(x, 0)].symbol()).collect()
    }

    /// Regression: a full title ran straight into the scale label
    /// (`cpu … 1.4W  43°  10% [↗]`), so the ceiling read as a title token.
    /// At every width the scale keeps at least one cell of rule on its left.
    #[test]
    fn scale_label_never_touches_the_title() {
        let panels = [
            (Panel::Cpu, "10%"),
            (Panel::Gpu, "20%"),
            (Panel::Net, "500KB/s"),
            (Panel::Disk, "5MB/s"),
            (Panel::Fans, "40–65°"),
            (Panel::Power, "80–95%"),
        ];
        for (panel, axis) in panels {
            let mut shown = 0;
            for width in 12..=240 {
                let top = top_row(panel, axis, width);
                let needle = format!(" {axis} ");
                let Some(at) = top.find(&needle) else {
                    continue;
                };
                shown += 1;
                let before = top[..at].chars().last();
                assert_eq!(before, Some('─'), "{panel:?} at {width}: {top:?}");
            }
            assert!(shown > 0, "{panel:?} never showed its scale");
        }
    }
}
