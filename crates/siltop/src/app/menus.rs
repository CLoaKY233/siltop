//! The popup menus' contents and what picking an entry does: the main
//! menu lists every command the keymap has, grouped, each with the key that
//! also runs it; a right-click on a process row lists what can be done to
//! that process. Painting and geometry live in `siltop_ui`'s
//! `widgets/menu.rs`; this only decides the entries and runs them.

use siltop_core::Preset;
use siltop_ui::{
    MenuEntry, MenuHit, MenuView, menu_area, menu_first, menu_first_item, menu_hit, menu_step,
};

use super::App;
use crate::keymap::{self, Action, Mode};

/// The main menu, in groups. Every action a Normal-mode or dossier key
/// runs is here, so the mouse reaches everything the keyboard does.
const MAIN: &[(&str, &[(&str, Action)])] = &[
    (
        "pages",
        &[
            ("Timeline", Action::Timeline),
            ("Settings", Action::Settings),
            ("Themes", Action::Themes),
            ("Keys & help", Action::Help),
            ("About", Action::About),
            ("Check for updates", Action::CheckUpdate),
        ],
    ),
    (
        "view",
        &[
            ("Work / glance", Action::CycleSurface),
            ("Expand or collapse box", Action::Expand),
            ("Next box", Action::NextPanel),
            ("Previous box", Action::PrevPanel),
            ("Zoom graphs", Action::Zoom),
            ("Zen mode", Action::ToggleZen),
            ("Next theme", Action::CycleTheme),
            ("Pause / resume", Action::Pause),
            ("Sample faster", Action::IntervalFaster),
            ("Sample slower", Action::IntervalSlower),
        ],
    ),
    (
        "looks",
        &[
            ("Silicon", Action::Preset(Preset::Silicon)),
            ("ML", Action::Preset(Preset::Ml)),
            ("Laptop", Action::Preset(Preset::Laptop)),
            ("SRE", Action::Preset(Preset::Sre)),
            ("Ricer", Action::Preset(Preset::Ricer)),
        ],
    ),
    (
        "process list",
        &[
            ("Filter…", Action::Search),
            ("Clear filter", Action::FilterClear),
            ("Process tree", Action::ToggleTree),
            ("Group helpers by app", Action::ToggleGroups),
            ("Open node", Action::Unfold),
            ("Close node", Action::Fold),
            ("Open all nodes", Action::FoldAll(true)),
            ("Close all nodes", Action::FoldAll(false)),
            ("Next sort column", Action::SortNext),
            ("Previous sort column", Action::SortPrev),
            ("Reverse sort", Action::ReverseSort),
            ("Tag / untag", Action::Tag),
            ("Clear tags", Action::ClearTags),
            ("Top of list", Action::Top),
            ("Bottom of list", Action::Bottom),
        ],
    ),
    ("process · keys in details", ROW),
    (
        "app",
        &[
            ("Suspend (fg resumes)", Action::Suspend),
            ("Quit siltop", Action::Quit),
        ],
    ),
];

/// A process row's right-click menu, and the main menu's process group.
const ROW: &[(&str, Action)] = &[
    ("Details", Action::Open),
    ("Quit (TERM)", Action::Kill),
    ("Force quit (KILL)", Action::ForceKill),
    ("Interrupt (INT)", Action::Interrupt),
    ("Send signal…", Action::SignalMenu),
    ("Lower priority", Action::Renice(1)),
    ("Raise priority (admin)", Action::Renice(-1)),
    ("Background QoS on/off", Action::ToggleBackground),
    ("Copy pid", Action::CopyPid),
    ("Copy command", Action::CopyCommand),
    ("Tag / untag", Action::Tag),
];

/// What a replay's row menu keeps: its pids are history, nothing is signalled.
fn read_only_safe(action: Action) -> bool {
    matches!(
        action,
        Action::Open | Action::CopyPid | Action::CopyCommand | Action::Tag
    )
}

/// An open popup menu.
#[derive(Debug, Clone)]
pub(super) struct MenuState {
    pub title: String,
    pub entries: Vec<MenuEntry>,
    /// Per entry, what picking it runs (`None` for a group header).
    pub actions: Vec<Option<Action>>,
    pub selected: usize,
    pub top: usize,
    pub anchor: Option<(u16, u16)>,
}

impl MenuState {
    pub(super) fn view(&self) -> MenuView<'_> {
        MenuView {
            title: &self.title,
            entries: &self.entries,
            selected: self.selected,
            top: self.top,
            anchor: self.anchor,
        }
    }
}

/// Every action the main menu runs, for the coverage test.
#[cfg(test)]
pub(super) fn main_actions() -> Vec<Action> {
    MAIN.iter()
        .flat_map(|(_, items)| items.iter().map(|(_, a)| *a))
        .collect()
}

/// Every action a process row's menu runs, for the coverage test.
#[cfg(test)]
pub(super) fn row_actions() -> Vec<Action> {
    ROW.iter().map(|(_, a)| *a).collect()
}

impl App {
    /// The key that runs `action` outside the menu, for its right column.
    /// Pages only the menu opens show its own letter after `m` (`m t`).
    fn menu_key(&self, action: Action) -> String {
        keymap::key_for(Mode::Normal, self.profile, action)
            .or_else(|| keymap::key_for(Mode::Detail, self.profile, action))
            .or_else(|| {
                keymap::key_for(Mode::Menu, self.profile, action)
                    .zip(keymap::key_for(Mode::Normal, self.profile, Action::Menu))
                    .map(|(key, menu)| format!("{menu} {key}"))
            })
            .unwrap_or_default()
    }

    fn build_menu(
        &self,
        title: String,
        groups: &[(&str, &[(&str, Action)])],
        anchor: Option<(u16, u16)>,
    ) -> MenuState {
        let mut entries = Vec::new();
        let mut actions = Vec::new();
        for (group, items) in groups {
            if groups.len() > 1 {
                entries.push(MenuEntry::header(*group));
                actions.push(None);
            }
            for (label, action) in *items {
                entries.push(MenuEntry::item(*label, self.menu_key(*action)));
                actions.push(Some(*action));
            }
        }
        let selected = menu_first_item(&entries);
        MenuState {
            title,
            entries,
            actions,
            selected,
            top: 0,
            anchor,
        }
    }

    /// A row menu's items: a replay keeps only the harmless ones, and the
    /// dossier's own menu has no `Details`.
    fn row_items(&self, details: bool) -> Vec<(&'static str, Action)> {
        ROW.iter()
            .copied()
            .filter(|(_, a)| !self.read_only() || read_only_safe(*a))
            .filter(|(_, a)| details || *a != Action::Open)
            .collect()
    }

    /// Opens the main menu on its first item, or closes it.
    pub(super) fn toggle_menu(&mut self) {
        self.menu = match self.menu {
            Some(_) => None,
            None => Some(self.build_menu(String::from("siltop"), MAIN, None)),
        };
    }

    /// A right-click: a process row's menu (the row selected first), or
    /// the main menu at the pointer anywhere else on the dashboard.
    pub(super) fn open_context_menu(&mut self, col: u16, row: u16) {
        if self.overlay_open() {
            return;
        }
        if let Some(pid) = self.detail_pid {
            // inside the dossier: its process's menu, less `Details`
            let on_card = self
                .proc_pane()
                .is_some_and(|pane| Self::point_in(siltop_ui::detail_rect(pane), col, row));
            if on_card {
                let title = format!("{} ({pid})", self.proc_name(pid));
                let items = self.row_items(false);
                self.menu = Some(self.build_menu(title, &[("", &items)], Some((col, row))));
            }
            return;
        }
        self.sync_model();
        if let Some(siltop_ui::Hit::ProcRow(index)) =
            siltop_ui::hit_at(&self.view(), self.last_area, col, row)
            && let Some(row_info) = self.proc.model.rows().get(index)
        {
            let pid = row_info.pid;
            self.searching = false;
            self.focus = siltop_ui::Focus::Processes;
            self.proc.select(index, self.table_rows());
            let title = format!("{} ({pid})", self.proc_name(pid));
            let items = self.row_items(true);
            self.menu = Some(self.build_menu(title, &[("", &items)], Some((col, row))));
            return;
        }
        self.menu = Some(self.build_menu(String::from("siltop"), MAIN, Some((col, row))));
    }

    pub(super) fn handle_menu(&mut self, action: Action) {
        let Some(menu) = self.menu.as_mut() else {
            return;
        };
        match action {
            Action::Move(delta) => {
                menu.selected = menu_step(&menu.entries, menu.selected, delta);
                let area = menu_area(self.last_area);
                let top = menu_first(area, &menu.view());
                if let Some(menu) = self.menu.as_mut() {
                    menu.top = top;
                }
            }
            Action::Open => {
                let selected = menu.selected;
                self.pick_menu(selected);
            }
            // the main menu's own letters (`s`, `t`, `a`, `H`)
            Action::Settings | Action::Themes | Action::About | Action::Timeline => {
                self.menu = None;
                self.reduce(action);
            }
            _ => {}
        }
    }

    /// Runs entry `index`; every entry leaves the menu first, so what it
    /// runs lands on the screen underneath.
    pub(super) fn pick_menu(&mut self, index: usize) {
        let action = self
            .menu
            .take()
            .and_then(|menu| menu.actions.get(index).copied().flatten());
        if let Some(action) = action {
            self.reduce(action);
        }
    }

    pub(super) fn click_menu(&mut self, col: u16, row: u16) {
        let Some(menu) = self.menu.as_ref() else {
            return;
        };
        match menu_hit(menu_area(self.last_area), &menu.view(), col, row) {
            MenuHit::Item(index) => self.pick_menu(index),
            MenuHit::Card => {}
            MenuHit::Close | MenuHit::Outside => self.menu = None,
        }
    }
}
