//! Every key binding, per mode and per profile. `event.rs` turns a
//! crossterm key into an [`Action`] through these tables, the help overlay
//! and the README key table are generated from them, and nothing else
//! decides what a key does.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use siltop_core::{Preset, ProcSort};
use siltop_ui::HelpGroup;

/// What the app does in response to input. Keys, footer chips and clicks
/// all end up here, and `App::reduce` is the only consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Resize {
        cols: u16,
        rows: u16,
    },
    Help,
    Settings,
    /// Open (or close) the main menu.
    Menu,
    /// Open the update dialog when a newer version is waiting.
    CheckUpdate,
    /// Settings on the appearance page.
    Themes,
    /// Settings on the about page.
    About,
    /// Open (or close) the Timeline page.
    Timeline,
    /// Timeline: move the scrub cursor by this many columns.
    Scrub(i32),
    /// Timeline: step the range shorter (-1) or longer (+1).
    Range(i32),
    /// Close the overlay the key was pressed in (`q` never quits from one).
    Close,
    /// Esc: close the top overlay, else clear the filter, else collapse.
    Cancel,
    CycleSurface,
    Search,
    FilterChar(char),
    FilterBackspace,
    FilterClear,
    /// Leave the search line, keeping the filter on the table.
    FilterDone,
    Move(i32),
    Page(i32),
    Top,
    Bottom,
    /// SIGTERM the selection or the open dossier (macOS "Quit").
    Kill,
    /// SIGKILL (macOS "Force Quit").
    ForceKill,
    Interrupt,
    /// Open the signal picker for the selection or the dossier's pid.
    SignalMenu,
    /// Renice by this step (+ lowers priority, - raises it).
    Renice(i32),
    /// Toggle the Darwin background band.
    ToggleBackground,
    /// List helpers under their `.app` bundle, or not.
    ToggleGroups,
    /// Tag or untag the selected row (every member of a group).
    Tag,
    /// Drop every tag.
    ClearTags,
    /// Show the selected node's members; elsewhere, the next box.
    Unfold,
    /// Hide the selected node's members; elsewhere, the previous box.
    Fold,
    /// Expand (`true`) or collapse every tree node and app group.
    FoldAll(bool),
    /// Open dossier page `n` (1–4).
    DossierTab(u8),
    /// Copy the dossier's pid to the clipboard (OSC 52).
    CopyPid,
    /// Copy the dossier's full command line (OSC 52).
    CopyCommand,
    ConfirmYes,
    ConfirmNo,
    IntervalSlower,
    IntervalFaster,
    Pause,
    ToggleTree,
    CycleSort,
    SortPrev,
    SortNext,
    SortBy(ProcSort),
    ReverseSort,
    Open,
    Expand,
    NextPanel,
    PrevPanel,
    CycleTheme,
    /// Graph zoom: live (2m), 5m, 15m, 1m.
    Zoom,
    /// Apply a named look (`1`–`5`).
    Preset(Preset),
    /// Zen: hide the footer and scale labels.
    ToggleZen,
    /// Settings: move the focused value back one option.
    SettingPrev,
    /// Settings: move the focused value forward one option.
    SettingNext,
    Click {
        col: u16,
        row: u16,
    },
    Drag {
        col: u16,
        row: u16,
    },
    MouseUp,
    /// A right-click: a process row's menu, or the main menu at the pointer.
    ContextMenu {
        col: u16,
        row: u16,
    },
    /// The pointer moved onto `(col, row)`; lights the button there.
    Hover {
        col: u16,
        row: u16,
    },
    Scroll {
        col: u16,
        row: u16,
        delta: i32,
    },
    /// `replay`: jump this many twentieths of the recording (negative is back).
    ReplayScrub(i32),
    /// `replay`: one playback speed step, faster or slower.
    ReplaySpeed(bool),
    /// Ctrl-Z: give the terminal back and stop until `fg`.
    Suspend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    Normal,
    Search,
    Detail,
    Confirm,
    Help,
    Settings,
    /// Typing into the settings screen's `/` filter.
    SettingsFilter,
    /// The main menu card.
    Menu,
    /// The signal picker is open.
    SignalMenu,
    /// The Timeline page.
    Timeline,
    /// The update dialog.
    Update,
}

impl Mode {
    #[cfg(test)]
    pub const ALL: [Self; 11] = [
        Self::Normal,
        Self::Search,
        Self::Detail,
        Self::Confirm,
        Self::Help,
        Self::Settings,
        Self::SettingsFilter,
        Self::Menu,
        Self::SignalMenu,
        Self::Timeline,
        Self::Update,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Profile {
    #[default]
    Siltop,
    Htop,
    Btop,
}

impl Profile {
    #[cfg(test)]
    pub const ALL: [Self; 3] = [Self::Siltop, Self::Htop, Self::Btop];

    /// The canonical name, as the config file and the settings row spell it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Siltop => "siltop",
            Self::Htop => "htop",
            Self::Btop => "btop",
        }
    }

    /// The session's profile: `SILTOP_KEYS` over the file's `keymap`,
    /// written back into `session` under its canonical name so the settings
    /// row shows what the keys do. An alias (`btop++`, `default`) or the
    /// variable shadows the file's value for this run only.
    pub fn apply_session(session: &mut siltop_core::Config, env: Option<&str>) {
        if let Some(profile) = env
            .and_then(Self::by_name)
            .or_else(|| Self::by_name(&session.keymap))
        {
            profile.name().clone_into(&mut session.keymap);
        }
    }

    #[must_use]
    pub fn by_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "siltop" | "default" | "" => Some(Self::Siltop),
            "htop" => Some(Self::Htop),
            "btop" | "btop++" => Some(Self::Btop),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl Key {
    /// Shift is folded into the character (`K`, `?`), so only Ctrl and Alt
    /// take part in matching.
    #[must_use]
    pub fn from_event(key: KeyEvent) -> Self {
        let code = match key.code {
            KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::SHIFT) => {
                KeyCode::Char(c.to_ascii_uppercase())
            }
            code => code,
        };
        Self {
            code,
            mods: key.modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT),
        }
    }

    #[must_use]
    pub fn label(self) -> String {
        let base = match self.code {
            KeyCode::Char(' ') => String::from("space"),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Up => String::from("↑"),
            KeyCode::Down => String::from("↓"),
            KeyCode::Left => String::from("←"),
            KeyCode::Right => String::from("→"),
            KeyCode::Enter => String::from("enter"),
            KeyCode::Esc => String::from("esc"),
            KeyCode::Tab => String::from("tab"),
            KeyCode::BackTab => String::from("shift-tab"),
            KeyCode::Backspace => String::from("backspace"),
            KeyCode::Delete => String::from("delete"),
            KeyCode::Home => String::from("home"),
            KeyCode::End => String::from("end"),
            KeyCode::PageUp => String::from("pgup"),
            KeyCode::PageDown => String::from("pgdn"),
            KeyCode::F(n) => format!("F{n}"),
            other => format!("{other:?}").to_lowercase(),
        };
        if self.mods.contains(KeyModifiers::CONTROL) {
            format!("ctrl-{base}")
        } else {
            base
        }
    }
}

const fn ch(c: char) -> Key {
    Key {
        code: KeyCode::Char(c),
        mods: KeyModifiers::NONE,
    }
}

const fn code(code: KeyCode) -> Key {
    Key {
        code,
        mods: KeyModifiers::NONE,
    }
}

const CTRL_C: Key = Key {
    code: KeyCode::Char('c'),
    mods: KeyModifiers::CONTROL,
};
const CTRL_Z: Key = Key {
    code: KeyCode::Char('z'),
    mods: KeyModifiers::CONTROL,
};
const UP: Key = code(KeyCode::Up);
const DOWN: Key = code(KeyCode::Down);
const LEFT: Key = code(KeyCode::Left);
const RIGHT: Key = code(KeyCode::Right);
const ENTER: Key = code(KeyCode::Enter);
const ESC: Key = code(KeyCode::Esc);
const TAB: Key = code(KeyCode::Tab);
const BACKTAB: Key = code(KeyCode::BackTab);
const HOME: Key = code(KeyCode::Home);
const END: Key = code(KeyCode::End);
const PGUP: Key = code(KeyCode::PageUp);
const PGDN: Key = code(KeyCode::PageDown);
const DELETE: Key = code(KeyCode::Delete);
const BACKSPACE: Key = code(KeyCode::Backspace);

const fn f(n: u8) -> Key {
    code(KeyCode::F(n))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Group {
    General,
    Navigate,
    Processes,
    Details,
    Search,
    Settings,
}

impl Group {
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Navigate => "navigate",
            Self::Processes => "processes",
            Self::Details => "process details",
            Self::Search => "while filtering",
            Self::Settings => "in settings",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub keys: &'static [Key],
    pub action: Action,
    pub group: Group,
    /// Help text; empty hides the binding from help and the README.
    pub help: &'static str,
}

const fn b(keys: &'static [Key], action: Action, group: Group, help: &'static str) -> Binding {
    Binding {
        keys,
        action,
        group,
        help,
    }
}

use Group::{Details, General, Navigate, Processes, Search, Settings};

/// Normal-mode keys every profile shares.
const NORMAL_COMMON: &[Binding] = &[
    b(&[ch('?')], Action::Help, General, "help"),
    b(&[ch('m')], Action::Menu, General, "main menu"),
    b(&[ch('H')], Action::Timeline, General, "timeline (history)"),
    b(&[ch('z')], Action::Zoom, General, "zoom graphs"),
    b(&[ch('q'), CTRL_C], Action::Quit, General, "quit"),
    b(&[CTRL_Z], Action::Suspend, General, "suspend (fg resumes)"),
    b(
        &[ESC],
        Action::Cancel,
        General,
        "close / clear filter / back / menu",
    ),
    b(&[TAB], Action::NextPanel, Navigate, "next box"),
    b(&[BACKTAB], Action::PrevPanel, Navigate, "previous box"),
    b(
        &[ENTER],
        Action::Open,
        Navigate,
        "details / expand focused box",
    ),
    b(&[ch('j'), DOWN], Action::Move(1), Navigate, "move down"),
    b(&[ch('k'), UP], Action::Move(-1), Navigate, "move up"),
    b(&[PGDN], Action::Page(1), Navigate, "page down"),
    b(&[PGUP], Action::Page(-1), Navigate, "page up"),
    b(&[HOME], Action::Top, Navigate, "top of list"),
    b(&[END], Action::Bottom, Navigate, "bottom of list"),
    b(&[ch('/')], Action::Search, Processes, "filter processes"),
    b(&[DELETE], Action::FilterClear, Processes, "clear filter"),
    b(&[BACKSPACE], Action::FilterBackspace, Processes, ""),
    b(
        &[ch('1')],
        Action::Preset(Preset::Silicon),
        General,
        "preset: silicon",
    ),
    b(
        &[ch('2')],
        Action::Preset(Preset::Ml),
        General,
        "preset: ml",
    ),
    b(
        &[ch('3')],
        Action::Preset(Preset::Laptop),
        General,
        "preset: laptop",
    ),
    b(
        &[ch('4')],
        Action::Preset(Preset::Sre),
        General,
        "preset: sre",
    ),
    b(
        &[ch('5')],
        Action::Preset(Preset::Ricer),
        General,
        "preset: ricer",
    ),
];

const NORMAL_SILTOP: &[Binding] = &[
    // `U` is "untag all" on the htop profile, so only the profiles that
    // leave it free open the update dialog from here.
    b(&[ch('U')], Action::CheckUpdate, General, ""),
    b(&[ch('s')], Action::Settings, General, "settings"),
    b(
        &[ch('v')],
        Action::CycleSurface,
        General,
        "cycle work / glance",
    ),
    b(&[ch('T')], Action::CycleTheme, General, "cycle theme"),
    b(&[ch('Z')], Action::ToggleZen, General, "zen mode"),
    b(&[ch('p')], Action::Pause, General, "pause / resume"),
    b(
        &[ch('['), ch('-')],
        Action::IntervalSlower,
        General,
        "sample slower",
    ),
    b(
        &[ch(']'), ch('+'), ch('=')],
        Action::IntervalFaster,
        General,
        "sample faster",
    ),
    b(&[RIGHT], Action::Unfold, Navigate, "open node / next box"),
    b(&[LEFT], Action::Fold, Navigate, "close node / previous box"),
    b(
        &[ch('e')],
        Action::Expand,
        Navigate,
        "expand / collapse box",
    ),
    b(&[ch('g')], Action::Top, Navigate, "top of list"),
    b(&[ch('G')], Action::Bottom, Navigate, "bottom of list"),
    b(&[ch('f')], Action::Search, Processes, "filter processes"),
    b(&[ch('t')], Action::ToggleTree, Processes, "process tree"),
    b(
        &[ch('a')],
        Action::ToggleGroups,
        Processes,
        "group helpers by app",
    ),
    b(
        &[ch('E')],
        Action::FoldAll(true),
        Processes,
        "open all nodes",
    ),
    b(
        &[ch('C')],
        Action::FoldAll(false),
        Processes,
        "close all nodes",
    ),
    b(&[ch(' ')], Action::Tag, Processes, "tag / untag process"),
    b(&[ch('u')], Action::ClearTags, Processes, "clear tags"),
    b(
        &[ch('o'), ch('>')],
        Action::SortNext,
        Processes,
        "next sort column",
    ),
    b(
        &[ch('<')],
        Action::SortPrev,
        Processes,
        "previous sort column",
    ),
    b(&[ch('r')], Action::ReverseSort, Processes, "reverse sort"),
    b(&[ch('x')], Action::Kill, Processes, "quit process (TERM)"),
    b(
        &[ch('K')],
        Action::ForceKill,
        Processes,
        "force quit (KILL)",
    ),
    b(&[ch('X')], Action::SignalMenu, Processes, "send signal…"),
];

const NORMAL_HTOP: &[Binding] = &[
    b(&[f(1)], Action::Help, General, "help"),
    b(&[f(2), ch('S')], Action::Settings, General, "settings"),
    b(&[f(10)], Action::Quit, General, "quit"),
    b(&[ch('Z')], Action::Pause, General, "pause / resume"),
    b(
        &[ch('v')],
        Action::CycleSurface,
        General,
        "cycle work / glance",
    ),
    b(
        &[ch('['), ch('-')],
        Action::IntervalSlower,
        General,
        "sample slower",
    ),
    b(
        &[ch(']'), ch('+'), ch('=')],
        Action::IntervalFaster,
        General,
        "sample faster",
    ),
    b(&[RIGHT], Action::Unfold, Navigate, "open node / next box"),
    b(&[LEFT], Action::Fold, Navigate, "close node / previous box"),
    b(
        &[ch('e')],
        Action::Expand,
        Navigate,
        "expand / collapse box",
    ),
    b(&[f(3), f(4)], Action::Search, Processes, "filter processes"),
    b(
        &[f(5), ch('t')],
        Action::ToggleTree,
        Processes,
        "process tree",
    ),
    b(&[f(6)], Action::CycleSort, Processes, "cycle sort column"),
    b(
        &[ch('P')],
        Action::SortBy(ProcSort::Cpu),
        Processes,
        "sort by cpu",
    ),
    b(
        &[ch('M')],
        Action::SortBy(ProcSort::Mem),
        Processes,
        "sort by memory",
    ),
    b(
        &[ch('N')],
        Action::SortBy(ProcSort::Pid),
        Processes,
        "sort by pid",
    ),
    b(
        &[ch('T')],
        Action::SortBy(ProcSort::Started),
        Processes,
        "sort by start time",
    ),
    b(&[ch('I')], Action::ReverseSort, Processes, "invert sort"),
    b(&[f(7)], Action::Renice(-1), Processes, "raise priority"),
    b(&[f(8)], Action::Renice(1), Processes, "lower priority"),
    b(&[ch('x')], Action::Kill, Processes, "quit process (TERM)"),
    b(
        &[ch('K')],
        Action::ForceKill,
        Processes,
        "force quit (KILL)",
    ),
    b(
        &[f(9), ch('X')],
        Action::SignalMenu,
        Processes,
        "send signal…",
    ),
    b(
        &[ch('a')],
        Action::ToggleGroups,
        Processes,
        "group helpers by app",
    ),
    b(
        &[ch('E')],
        Action::FoldAll(true),
        Processes,
        "open all nodes",
    ),
    b(
        &[ch('C')],
        Action::FoldAll(false),
        Processes,
        "close all nodes",
    ),
    b(&[ch(' ')], Action::Tag, Processes, "tag process"),
    b(&[ch('U')], Action::ClearTags, Processes, "untag all"),
];

const NORMAL_BTOP: &[Binding] = &[
    b(&[ch('U')], Action::CheckUpdate, General, ""),
    b(&[ch('o'), ch('s')], Action::Settings, General, "settings"),
    b(
        &[ch('v')],
        Action::CycleSurface,
        General,
        "cycle work / glance",
    ),
    b(&[ch('T')], Action::CycleTheme, General, "cycle theme"),
    b(&[ch('Z')], Action::ToggleZen, General, "zen mode"),
    b(&[ch('p')], Action::Pause, General, "pause / resume"),
    b(
        &[ch('['), ch('-')],
        Action::IntervalSlower,
        General,
        "sample slower",
    ),
    b(
        &[ch(']'), ch('+'), ch('=')],
        Action::IntervalFaster,
        General,
        "sample faster",
    ),
    b(&[ch('g')], Action::Top, Navigate, "top of list"),
    b(&[ch('G')], Action::Bottom, Navigate, "bottom of list"),
    b(&[ch('f')], Action::Search, Processes, "filter processes"),
    b(
        &[ch('e'), ch('t')],
        Action::ToggleTree,
        Processes,
        "process tree",
    ),
    b(&[LEFT], Action::SortPrev, Processes, "previous sort column"),
    b(&[RIGHT], Action::SortNext, Processes, "next sort column"),
    b(&[ch('r')], Action::ReverseSort, Processes, "reverse sort"),
    b(
        &[ch('l')],
        Action::Unfold,
        Processes,
        "open node / next box",
    ),
    b(
        &[ch('h')],
        Action::Fold,
        Processes,
        "close node / previous box",
    ),
    b(
        &[ch('E')],
        Action::FoldAll(true),
        Processes,
        "open all nodes",
    ),
    b(
        &[ch('C')],
        Action::FoldAll(false),
        Processes,
        "close all nodes",
    ),
    b(
        &[ch('a')],
        Action::ToggleGroups,
        Processes,
        "group helpers by app",
    ),
    b(&[ch(' ')], Action::Tag, Processes, "tag / untag process"),
    b(&[ch('u')], Action::ClearTags, Processes, "clear tags"),
    b(&[ch('x')], Action::Kill, Processes, "quit process (TERM)"),
    b(
        &[ch('K')],
        Action::ForceKill,
        Processes,
        "force quit (KILL)",
    ),
    b(&[ch('X')], Action::SignalMenu, Processes, "send signal…"),
];

const SEARCH: &[Binding] = &[
    b(&[CTRL_C], Action::Quit, Search, ""),
    b(&[ch('?')], Action::Help, Search, ""),
    b(&[ESC], Action::Cancel, Search, "clear, then leave"),
    b(
        &[ENTER, TAB],
        Action::FilterDone,
        Search,
        "keep filter, back to the list",
    ),
    b(&[DOWN], Action::Move(1), Search, "move down, keep typing"),
    b(&[UP], Action::Move(-1), Search, "move up, keep typing"),
    b(&[PGDN], Action::Page(1), Search, ""),
    b(&[PGUP], Action::Page(-1), Search, ""),
    b(&[BACKSPACE], Action::FilterBackspace, Search, ""),
    b(&[DELETE], Action::FilterClear, Search, "clear filter"),
];

const DETAIL: &[Binding] = &[
    b(&[CTRL_C], Action::Quit, Details, ""),
    b(&[ch('?')], Action::Help, Details, ""),
    b(
        &[ESC, ENTER, ch('q')],
        Action::Close,
        Details,
        "close details",
    ),
    b(&[ch('j'), DOWN], Action::Move(1), Details, "next process"),
    b(
        &[ch('k'), UP],
        Action::Move(-1),
        Details,
        "previous process",
    ),
    b(&[ch('x')], Action::Kill, Details, "quit (TERM)"),
    b(&[ch('K')], Action::ForceKill, Details, "force quit (KILL)"),
    b(&[ch('i')], Action::Interrupt, Details, "interrupt (INT)"),
    // `+` and `-` keep their global meaning (sample faster / slower), so
    // renice has its own keys and a stray `+` never demotes a process.
    b(
        &[ch('n'), f(8)],
        Action::Renice(1),
        Details,
        "lower priority (nice +1)",
    ),
    b(
        &[ch('N'), f(7)],
        Action::Renice(-1),
        Details,
        "raise priority (nice -1, admin)",
    ),
    b(&[ch('['), ch('-')], Action::IntervalSlower, Details, ""),
    b(
        &[ch(']'), ch('+'), ch('=')],
        Action::IntervalFaster,
        Details,
        "",
    ),
    b(
        &[ch('b')],
        Action::ToggleBackground,
        Details,
        "background QoS on/off",
    ),
    b(&[ch('y')], Action::CopyPid, Details, "copy pid"),
    b(&[ch('Y')], Action::CopyCommand, Details, "copy command"),
    b(&[ch('X')], Action::SignalMenu, Details, "send signal…"),
    b(&[TAB], Action::NextPanel, Details, "next page"),
    b(&[BACKTAB], Action::PrevPanel, Details, "previous page"),
    b(&[ch('1')], Action::DossierTab(1), Details, "overview"),
    b(&[ch('2')], Action::DossierTab(2), Details, "threads"),
    b(&[ch('3')], Action::DossierTab(3), Details, "open files"),
    b(&[ch('4')], Action::DossierTab(4), Details, "ports"),
];

const SIGNAL_MENU: &[Binding] = &[
    b(&[CTRL_C], Action::Quit, General, ""),
    b(&[ch('j'), DOWN], Action::Move(1), General, ""),
    b(&[ch('k'), UP], Action::Move(-1), General, ""),
    b(&[ENTER], Action::ConfirmYes, General, ""),
    b(
        &[ch('n'), ESC, ch('q'), ch('X')],
        Action::ConfirmNo,
        General,
        "",
    ),
];

const UPDATE: &[Binding] = &[
    b(&[CTRL_C], Action::Quit, General, ""),
    b(&[ch('?')], Action::Help, General, ""),
    b(&[ESC, ch('q')], Action::Cancel, General, ""),
    b(&[ENTER], Action::Open, General, ""),
    b(&[ch('j'), DOWN], Action::Move(1), General, ""),
    b(&[ch('k'), UP], Action::Move(-1), General, ""),
];

const CONFIRM: &[Binding] = &[
    b(&[CTRL_C], Action::Quit, General, ""),
    b(&[ch('y'), ENTER], Action::ConfirmYes, General, ""),
    b(&[ch('n'), ESC, ch('q')], Action::ConfirmNo, General, ""),
];

const HELP: &[Binding] = &[
    b(&[CTRL_C], Action::Quit, General, ""),
    // F1 and F10 close what htop's F1 opened.
    b(
        &[ch('?'), ch('q'), ESC, f(1), f(10)],
        Action::Close,
        General,
        "",
    ),
    b(&[ch('j'), DOWN], Action::Move(1), General, ""),
    b(&[ch('k'), UP], Action::Move(-1), General, ""),
    b(&[PGDN, ch(' ')], Action::Page(1), General, ""),
    b(&[PGUP], Action::Page(-1), General, ""),
    b(&[HOME, ch('g')], Action::Top, General, ""),
    b(&[END, ch('G')], Action::Bottom, General, ""),
];

const SETTINGS: &[Binding] = &[
    b(&[CTRL_C], Action::Quit, Settings, ""),
    b(&[ch('?')], Action::Help, Settings, ""),
    b(&[ESC], Action::Cancel, Settings, "clear search, then close"),
    b(
        &[ch('q'), ch('s'), f(2), f(10)],
        Action::Close,
        Settings,
        "close settings",
    ),
    b(&[ch('j'), DOWN], Action::Move(1), Settings, "next setting"),
    b(
        &[ch('k'), UP],
        Action::Move(-1),
        Settings,
        "previous setting",
    ),
    b(&[HOME, ch('g')], Action::Top, Settings, ""),
    b(&[END, ch('G')], Action::Bottom, Settings, ""),
    b(
        &[ch('l'), RIGHT, ENTER, ch(' ')],
        Action::SettingNext,
        Settings,
        "change value",
    ),
    b(
        &[ch('h'), LEFT],
        Action::SettingPrev,
        Settings,
        "change value back",
    ),
    b(&[TAB], Action::NextPanel, Settings, "next section"),
    b(&[BACKTAB], Action::PrevPanel, Settings, "previous section"),
    b(&[ch('/')], Action::Search, Settings, "search settings"),
    b(&[ch('T')], Action::CycleTheme, Settings, ""),
];

/// The main menu: arrows pick, enter runs, each item's own key runs it.
const MENU: &[Binding] = &[
    b(&[CTRL_C], Action::Quit, General, ""),
    b(&[ch('?')], Action::Help, General, ""),
    b(&[ESC, ch('m'), ch('q')], Action::Close, General, ""),
    b(&[ch('j'), DOWN, TAB], Action::Move(1), General, ""),
    b(&[ch('k'), UP, BACKTAB], Action::Move(-1), General, ""),
    b(&[ENTER, ch(' '), ch('l'), RIGHT], Action::Open, General, ""),
    b(&[ch('s')], Action::Settings, General, ""),
    b(&[ch('t')], Action::Themes, General, ""),
    b(&[ch('a')], Action::About, General, ""),
    b(&[ch('H')], Action::Timeline, General, ""),
];

/// The Timeline page: ←/→ scrub, ↑/↓ change the range.
const TIMELINE: &[Binding] = &[
    b(&[CTRL_C], Action::Quit, General, ""),
    b(&[ch('?')], Action::Help, General, ""),
    b(&[ESC, ch('q'), ch('H')], Action::Close, General, ""),
    b(&[ch('h'), LEFT], Action::Scrub(-1), General, ""),
    b(&[ch('l'), RIGHT], Action::Scrub(1), General, ""),
    b(&[PGUP], Action::Scrub(-10), General, ""),
    b(&[PGDN], Action::Scrub(10), General, ""),
    b(&[HOME, ch('g')], Action::Top, General, ""),
    b(&[END, ch('G')], Action::Bottom, General, ""),
    b(&[ch('k'), UP, BACKTAB], Action::Range(-1), General, ""),
    b(&[ch('j'), DOWN, TAB], Action::Range(1), General, ""),
];

/// Typing into the settings search: arrows still move, the rest is text.
const SETTINGS_FILTER: &[Binding] = &[
    b(&[CTRL_C], Action::Quit, Settings, ""),
    b(&[ESC], Action::Cancel, Settings, ""),
    b(&[ENTER], Action::Open, Settings, ""),
    b(&[DOWN], Action::Move(1), Settings, ""),
    b(&[UP], Action::Move(-1), Settings, ""),
    b(&[RIGHT], Action::SettingNext, Settings, ""),
    b(&[LEFT], Action::SettingPrev, Settings, ""),
    b(&[BACKSPACE], Action::FilterBackspace, Settings, ""),
    b(&[DELETE], Action::FilterClear, Settings, ""),
];

/// Layered over Normal while a trace plays; nothing else changes meaning.
pub const REPLAY: &[Binding] = &[
    b(
        &[RIGHT],
        Action::ReplayScrub(1),
        General,
        "replay: scrub forward",
    ),
    b(
        &[LEFT],
        Action::ReplayScrub(-1),
        General,
        "replay: scrub back",
    ),
    b(
        &[ch(']'), ch('+'), ch('=')],
        Action::ReplaySpeed(true),
        General,
        "replay: faster",
    ),
    b(
        &[ch('['), ch('-')],
        Action::ReplaySpeed(false),
        General,
        "replay: slower",
    ),
];

/// The replay action for `key`, looked up before the Normal tables.
#[must_use]
pub fn resolve_replay(key: KeyEvent) -> Option<Action> {
    let key = Key::from_event(key);
    REPLAY
        .iter()
        .find(|binding| binding.keys.contains(&key))
        .map(|binding| binding.action)
}

/// Job control works from every screen, as in any shell program; Normal
/// mode lists it in help (`NORMAL_COMMON`), the rest bind it quietly.
const EVERYWHERE: &[Binding] = &[b(&[CTRL_Z], Action::Suspend, General, "")];

/// The tables consulted for `mode` under `profile`, in lookup order.
#[must_use]
pub fn tables(mode: Mode, profile: Profile) -> Vec<&'static [Binding]> {
    let mut tables = mode_tables(mode, profile);
    if mode != Mode::Normal {
        tables.push(EVERYWHERE);
    }
    tables
}

fn mode_tables(mode: Mode, profile: Profile) -> Vec<&'static [Binding]> {
    match mode {
        Mode::Normal => vec![
            match profile {
                Profile::Siltop => NORMAL_SILTOP,
                Profile::Htop => NORMAL_HTOP,
                Profile::Btop => NORMAL_BTOP,
            },
            NORMAL_COMMON,
        ],
        Mode::Search => vec![SEARCH],
        Mode::Detail => vec![DETAIL],
        Mode::Confirm => vec![CONFIRM],
        Mode::Help => vec![HELP],
        Mode::Settings => vec![SETTINGS],
        Mode::SettingsFilter => vec![SETTINGS_FILTER],
        Mode::Menu => vec![MENU],
        Mode::SignalMenu => vec![SIGNAL_MENU],
        Mode::Timeline => vec![TIMELINE],
        Mode::Update => vec![UPDATE],
    }
}

pub fn bindings(mode: Mode, profile: Profile) -> impl Iterator<Item = &'static Binding> {
    tables(mode, profile).into_iter().flatten()
}

/// The action `key` runs in `mode`. While filtering, every printable
/// character not bound above is text, so no command can leak into search.
#[must_use]
pub fn resolve(key: KeyEvent, mode: Mode, profile: Profile) -> Option<Action> {
    let key = Key::from_event(key);
    let bound = bindings(mode, profile)
        .find(|binding| binding.keys.contains(&key))
        .map(|binding| binding.action);
    if bound.is_some() {
        return bound;
    }
    match (mode, key.code) {
        (Mode::Search | Mode::SettingsFilter, KeyCode::Char(c))
            if key.mods.is_empty() && !c.is_control() =>
        {
            Some(Action::FilterChar(c))
        }
        _ => None,
    }
}

/// The footer's key hints for `profile`: the first key each hinted action
/// has in Normal mode. Labels are built once per profile and kept for the
/// run, so the view can borrow them as `&'static str`.
#[must_use]
pub fn hints(profile: Profile) -> siltop_ui::KeyHints {
    use std::sync::OnceLock;
    static CACHE: [OnceLock<siltop_ui::KeyHints>; 3] =
        [OnceLock::new(), OnceLock::new(), OnceLock::new()];
    let slot = match profile {
        Profile::Siltop => 0,
        Profile::Htop => 1,
        Profile::Btop => 2,
    };
    *CACHE[slot].get_or_init(|| {
        let key = |action: Action| -> &'static str {
            key_for(Mode::Normal, profile, action)
                .map_or("", |label| Box::leak(label.into_boxed_str()))
        };
        siltop_ui::KeyHints {
            help: key(Action::Help),
            quit: key(Action::Quit),
            search: key(Action::Search),
            kill: key(Action::Kill),
            settings: key(Action::Settings),
            pause: key(Action::Pause),
            work: key(Action::CycleSurface),
        }
    })
}

/// The label of the first key that runs `action` in `mode`, if any.
#[must_use]
pub fn key_for(mode: Mode, profile: Profile, action: Action) -> Option<String> {
    bindings(mode, profile)
        .find(|binding| binding.action == action)
        .and_then(|binding| binding.keys.first())
        .map(|key| key.label())
}

fn keys_label(keys: &[Key]) -> String {
    keys.iter()
        .map(|key| key.label())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Help rows for `profile`, grouped, with every key for one action on one
/// row. Hidden bindings (empty help) are left out.
#[must_use]
pub fn help_groups(profile: Profile) -> Vec<HelpGroup> {
    let mut groups: Vec<HelpGroup> = Vec::new();
    let mut order: Vec<Group> = Vec::new();
    let all = bindings(Mode::Normal, profile)
        .chain(bindings(Mode::Search, profile))
        .chain(bindings(Mode::Detail, profile))
        .chain(bindings(Mode::Settings, profile));
    let mut rows: Vec<(Group, Action, String, &'static str)> = Vec::new();
    for binding in all.filter(|binding| !binding.help.is_empty()) {
        let label = keys_label(binding.keys);
        if let Some(row) = rows
            .iter_mut()
            .find(|(group, action, _, _)| *group == binding.group && *action == binding.action)
        {
            row.2 = format!("{} {label}", row.2);
        } else {
            rows.push((binding.group, binding.action, label, binding.help));
        }
    }
    rows.sort_by_key(|(group, ..)| *group);
    for (group, _, keys, help) in rows {
        if !order.contains(&group) {
            order.push(group);
            groups.push(HelpGroup {
                title: group.title(),
                rows: Vec::new(),
            });
        }
        if let Some(last) = groups.last_mut() {
            last.rows.push((keys, help));
        }
    }
    groups
}

/// The README's key table for the default profile.
#[cfg(test)]
#[must_use]
pub fn readme_table() -> String {
    use std::fmt::Write as _;
    let mut out = String::from("| Key | Where | Action |\n| --- | --- | --- |\n");
    for group in help_groups(Profile::Siltop) {
        for (keys, help) in &group.rows {
            let keys = keys
                .split(' ')
                .map(|key| format!("`{key}`"))
                .collect::<Vec<_>>()
                .join(" ");
            let _ = writeln!(out, "| {keys} | {} | {help} |", group.title);
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn press(c: char) -> KeyEvent {
        KeyEvent::from(KeyCode::Char(c))
    }

    fn all_keys() -> Vec<KeyEvent> {
        let mut out: Vec<KeyEvent> = (' '..='~').map(press).collect();
        for code in [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Delete,
            KeyCode::Backspace,
        ] {
            out.push(KeyEvent::from(code));
        }
        for n in 1..=12 {
            out.push(KeyEvent::from(KeyCode::F(n)));
        }
        out.push(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        out
    }

    #[test]
    fn replay_keys_shadow_only_arrows_and_speed() {
        let right = KeyEvent::from(KeyCode::Right);
        assert_eq!(resolve_replay(right), Some(Action::ReplayScrub(1)));
        assert_eq!(
            resolve_replay(KeyEvent::from(KeyCode::Left)),
            Some(Action::ReplayScrub(-1))
        );
        assert_eq!(resolve_replay(press('+')), Some(Action::ReplaySpeed(true)));
        assert_eq!(resolve_replay(press('-')), Some(Action::ReplaySpeed(false)));
        for c in ['p', 'q', 'm', '?'] {
            assert_eq!(
                resolve_replay(press(c)),
                None,
                "{c} keeps its Normal meaning"
            );
        }
        let mut seen = HashSet::new();
        assert!(REPLAY.iter().flat_map(|b| b.keys).all(|k| seen.insert(*k)));
    }

    #[test]
    fn no_key_is_bound_twice_in_any_mode_or_profile() {
        for profile in Profile::ALL {
            for mode in Mode::ALL {
                let mut seen = HashSet::new();
                for binding in bindings(mode, profile) {
                    for key in binding.keys {
                        assert!(
                            seen.insert(*key),
                            "{profile:?}/{mode:?}: {} bound twice",
                            key.label()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn every_binding_resolves_to_its_own_action() {
        for profile in Profile::ALL {
            for mode in Mode::ALL {
                for binding in bindings(mode, profile) {
                    for key in binding.keys {
                        let event = KeyEvent::new(key.code, key.mods);
                        assert_eq!(
                            resolve(event, mode, profile),
                            Some(binding.action),
                            "{profile:?}/{mode:?} {}",
                            key.label()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn ctrl_c_quits_from_every_mode_and_profile() {
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        for profile in Profile::ALL {
            for mode in Mode::ALL {
                assert_eq!(resolve(ctrl_c, mode, profile), Some(Action::Quit));
            }
        }
    }

    #[test]
    fn q_never_quits_from_inside_an_overlay() {
        for profile in Profile::ALL {
            for mode in [
                Mode::Help,
                Mode::Settings,
                Mode::Detail,
                Mode::Confirm,
                Mode::Menu,
                Mode::Timeline,
            ] {
                let action = resolve(press('q'), mode, profile);
                assert_ne!(action, Some(Action::Quit), "{profile:?}/{mode:?}");
                assert!(
                    matches!(action, Some(Action::Close | Action::ConfirmNo)),
                    "{profile:?}/{mode:?}: {action:?}"
                );
            }
        }
    }

    #[test]
    fn m_opens_the_main_menu_in_every_profile() {
        for profile in Profile::ALL {
            assert_eq!(
                resolve(press('m'), Mode::Normal, profile),
                Some(Action::Menu),
                "{profile:?}"
            );
        }
    }

    #[test]
    fn capital_h_opens_the_timeline_everywhere_it_should() {
        for profile in Profile::ALL {
            for mode in [Mode::Normal, Mode::Menu] {
                assert_eq!(
                    resolve(press('H'), mode, profile),
                    Some(Action::Timeline),
                    "{profile:?}/{mode:?}"
                );
            }
            assert_eq!(
                resolve(press('H'), Mode::Timeline, profile),
                Some(Action::Close),
                "H toggles"
            );
        }
    }

    #[test]
    fn timeline_arrows_scrub_and_change_range() {
        let t = |key: KeyEvent| resolve(key, Mode::Timeline, Profile::Siltop);
        assert_eq!(t(KeyEvent::from(KeyCode::Left)), Some(Action::Scrub(-1)));
        assert_eq!(t(KeyEvent::from(KeyCode::Right)), Some(Action::Scrub(1)));
        assert_eq!(t(KeyEvent::from(KeyCode::Up)), Some(Action::Range(-1)));
        assert_eq!(t(KeyEvent::from(KeyCode::Down)), Some(Action::Range(1)));
        assert_eq!(t(press('x')), None, "no process action leaks in");
    }

    #[test]
    fn menu_keys_pick_move_and_close() {
        let m = |key: KeyEvent| resolve(key, Mode::Menu, Profile::Siltop);
        assert_eq!(m(press('s')), Some(Action::Settings));
        assert_eq!(m(press('t')), Some(Action::Themes));
        assert_eq!(m(press('a')), Some(Action::About));
        assert_eq!(m(press('?')), Some(Action::Help));
        assert_eq!(m(KeyEvent::from(KeyCode::Down)), Some(Action::Move(1)));
        assert_eq!(m(KeyEvent::from(KeyCode::Up)), Some(Action::Move(-1)));
        assert_eq!(m(KeyEvent::from(KeyCode::Enter)), Some(Action::Open));
        assert_eq!(m(KeyEvent::from(KeyCode::Esc)), Some(Action::Close));
        assert_eq!(m(press('m')), Some(Action::Close), "m toggles");
        assert_eq!(m(press('x')), None, "no process action leaks into the menu");
    }

    #[test]
    fn esc_closes_or_cancels_in_every_mode() {
        for profile in Profile::ALL {
            for mode in Mode::ALL {
                let action = resolve(KeyEvent::from(KeyCode::Esc), mode, profile);
                assert!(
                    matches!(
                        action,
                        Some(Action::Close | Action::Cancel | Action::ConfirmNo)
                    ),
                    "{profile:?}/{mode:?}: {action:?}"
                );
            }
        }
    }

    #[test]
    fn question_mark_opens_or_closes_help_everywhere_but_confirm_and_typing() {
        for profile in Profile::ALL {
            for mode in Mode::ALL {
                let action = resolve(press('?'), mode, profile);
                match mode {
                    Mode::Confirm | Mode::SignalMenu => assert_eq!(action, None),
                    Mode::Help => assert_eq!(action, Some(Action::Close)),
                    Mode::SettingsFilter => assert_eq!(action, Some(Action::FilterChar('?'))),
                    _ => assert_eq!(action, Some(Action::Help), "{profile:?}/{mode:?}"),
                }
            }
        }
    }

    #[test]
    fn search_mode_turns_every_letter_into_text() {
        for profile in Profile::ALL {
            for c in ('a'..='z').chain('A'..='Z').chain('0'..='9') {
                assert_eq!(
                    resolve(press(c), Mode::Search, profile),
                    Some(Action::FilterChar(c)),
                    "{profile:?} {c}"
                );
            }
            for c in ['q', 'x', 'K', 'p', 'T', 's', '/', '[', ']', ' ', 'é', '中'] {
                assert_eq!(
                    resolve(press(c), Mode::Search, profile),
                    Some(Action::FilterChar(c)),
                    "{profile:?} {c:?} leaked a command"
                );
            }
        }
    }

    #[test]
    fn search_keeps_focus_while_arrows_move_the_selection() {
        let p = Profile::Siltop;
        assert_eq!(
            resolve(KeyEvent::from(KeyCode::Down), Mode::Search, p),
            Some(Action::Move(1))
        );
        assert_eq!(
            resolve(KeyEvent::from(KeyCode::Up), Mode::Search, p),
            Some(Action::Move(-1))
        );
        assert_eq!(
            resolve(KeyEvent::from(KeyCode::Enter), Mode::Search, p),
            Some(Action::FilterDone)
        );
        assert_eq!(
            resolve(KeyEvent::from(KeyCode::Tab), Mode::Search, p),
            Some(Action::FilterDone)
        );
        assert_eq!(
            resolve(KeyEvent::from(KeyCode::Delete), Mode::Search, p),
            Some(Action::FilterClear)
        );
    }

    /// Regression (review P3): Ctrl+X armed a kill and Ctrl+U typed into
    /// search. A chord runs only what is bound to that exact chord.
    #[test]
    fn rv_chords_never_borrow_the_plain_letter() {
        for profile in Profile::ALL {
            for mode in Mode::ALL {
                for mods in [
                    KeyModifiers::CONTROL,
                    KeyModifiers::ALT,
                    KeyModifiers::CONTROL | KeyModifiers::ALT,
                    KeyModifiers::ALT | KeyModifiers::SHIFT,
                ] {
                    for c in ('a'..='z').chain('0'..='9') {
                        let event = KeyEvent::new(KeyCode::Char(c), mods);
                        let Some(action) = resolve(event, mode, profile) else {
                            continue;
                        };
                        let chord = Key::from_event(event);
                        let bound = bindings(mode, profile)
                            .any(|b| b.action == action && b.keys.contains(&chord));
                        assert!(
                            bound,
                            "{profile:?} {mode:?} {mods:?}+{c} ran {action:?} unbound"
                        );
                        assert!(
                            !matches!(action, Action::FilterChar(_)),
                            "{profile:?} {mode:?}: a chord typed text"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn modifier_chords_are_not_text_in_search() {
        let alt = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT);
        assert_eq!(resolve(alt, Mode::Search, Profile::Siltop), None);
        let ctrl = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert_eq!(resolve(ctrl, Mode::Search, Profile::Siltop), None);
    }

    #[test]
    fn shift_letters_match_their_capital_binding() {
        let shifted = KeyEvent::new(KeyCode::Char('K'), KeyModifiers::SHIFT);
        assert_eq!(
            resolve(shifted, Mode::Normal, Profile::Siltop),
            Some(Action::ForceKill)
        );
        let shifted_lower = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::SHIFT);
        assert_eq!(
            resolve(shifted_lower, Mode::Detail, Profile::Siltop),
            Some(Action::ForceKill)
        );
    }

    #[test]
    fn siltop_owner_calls() {
        let p = Profile::Siltop;
        let n = |c: char| resolve(press(c), Mode::Normal, p);
        assert_eq!(n('v'), Some(Action::CycleSurface));
        assert_eq!(n('g'), Some(Action::Top));
        assert_eq!(n('G'), Some(Action::Bottom));
        assert_eq!(n('w'), None, "w no longer jumps to work");
        assert_eq!(n('['), Some(Action::IntervalSlower));
        assert_eq!(n('-'), Some(Action::IntervalSlower));
        assert_eq!(n(']'), Some(Action::IntervalFaster));
        assert_eq!(n('+'), Some(Action::IntervalFaster));
        assert_eq!(n('p'), Some(Action::Pause));
        assert_eq!(n('f'), Some(Action::Search));
        assert_eq!(n('/'), Some(Action::Search));
        assert_eq!(n('t'), Some(Action::ToggleTree));
        assert_eq!(n('e'), Some(Action::Expand));
        assert_eq!(n('x'), Some(Action::Kill));
        assert_eq!(n('K'), Some(Action::ForceKill));
        assert_eq!(n('X'), Some(Action::SignalMenu));
        assert_eq!(n('T'), Some(Action::CycleTheme));
        assert_eq!(n('>'), Some(Action::SortNext));
        assert_eq!(n('o'), Some(Action::SortNext));
        assert_eq!(n('<'), Some(Action::SortPrev));
        assert_eq!(n('r'), Some(Action::ReverseSort));
        let k = |code: KeyCode| resolve(KeyEvent::from(code), Mode::Normal, p);
        assert_eq!(k(KeyCode::Home), Some(Action::Top));
        assert_eq!(k(KeyCode::End), Some(Action::Bottom));
        assert_eq!(k(KeyCode::PageUp), Some(Action::Page(-1)));
        assert_eq!(k(KeyCode::PageDown), Some(Action::Page(1)));
        assert_eq!(k(KeyCode::Delete), Some(Action::FilterClear));
    }

    #[test]
    fn nesting_and_tag_keys() {
        let n = |c: char| resolve(press(c), Mode::Normal, Profile::Siltop);
        assert_eq!(n('a'), Some(Action::ToggleGroups));
        assert_eq!(n(' '), Some(Action::Tag));
        assert_eq!(n('u'), Some(Action::ClearTags));
        assert_eq!(n('E'), Some(Action::FoldAll(true)));
        assert_eq!(n('C'), Some(Action::FoldAll(false)));
        assert_eq!(n('T'), Some(Action::CycleTheme), "T stays theme");
        let k = |code: KeyCode| resolve(KeyEvent::from(code), Mode::Normal, Profile::Siltop);
        assert_eq!(k(KeyCode::Right), Some(Action::Unfold));
        assert_eq!(k(KeyCode::Left), Some(Action::Fold));
        assert_eq!(
            k(KeyCode::Tab),
            Some(Action::NextPanel),
            "tab still hops boxes"
        );
    }

    #[test]
    fn dossier_tab_keys() {
        let d = |key: KeyEvent| resolve(key, Mode::Detail, Profile::Siltop);
        assert_eq!(d(KeyEvent::from(KeyCode::Tab)), Some(Action::NextPanel));
        assert_eq!(d(KeyEvent::from(KeyCode::BackTab)), Some(Action::PrevPanel));
        for n in 1..=4_u8 {
            let c = char::from(b'0' + n);
            assert_eq!(d(press(c)), Some(Action::DossierTab(n)));
        }
    }

    #[test]
    fn dossier_keys() {
        let d = |c: char| resolve(press(c), Mode::Detail, Profile::Siltop);
        assert_eq!(d('k'), Some(Action::Move(-1)));
        assert_eq!(d('j'), Some(Action::Move(1)));
        assert_eq!(d('K'), Some(Action::ForceKill));
        assert_eq!(d('x'), Some(Action::Kill));
        assert_eq!(d('i'), Some(Action::Interrupt));
        assert_eq!(d('X'), Some(Action::SignalMenu));
        assert_eq!(d('y'), Some(Action::CopyPid));
        assert_eq!(d('Y'), Some(Action::CopyCommand));
        assert_eq!(d('n'), Some(Action::Renice(1)));
        assert_eq!(d('N'), Some(Action::Renice(-1)));
        assert_eq!(d('b'), Some(Action::ToggleBackground));
        assert_eq!(d('t'), None, "t is the tree outside, never a signal");
    }

    #[test]
    fn htop_profile_function_keys() {
        let h = |code: KeyCode| resolve(KeyEvent::from(code), Mode::Normal, Profile::Htop);
        assert_eq!(h(KeyCode::F(1)), Some(Action::Help));
        assert_eq!(h(KeyCode::F(2)), Some(Action::Settings));
        assert_eq!(h(KeyCode::F(3)), Some(Action::Search));
        assert_eq!(h(KeyCode::F(4)), Some(Action::Search));
        assert_eq!(h(KeyCode::F(5)), Some(Action::ToggleTree));
        assert_eq!(h(KeyCode::F(6)), Some(Action::CycleSort));
        assert_eq!(h(KeyCode::F(7)), Some(Action::Renice(-1)));
        assert_eq!(h(KeyCode::F(8)), Some(Action::Renice(1)));
        assert_eq!(h(KeyCode::F(9)), Some(Action::SignalMenu));
        assert_eq!(h(KeyCode::Char('x')), Some(Action::Kill));
        assert_eq!(h(KeyCode::F(10)), Some(Action::Quit));
        assert_eq!(h(KeyCode::Char('P')), Some(Action::SortBy(ProcSort::Cpu)));
        assert_eq!(h(KeyCode::Char('M')), Some(Action::SortBy(ProcSort::Mem)));
        assert_eq!(h(KeyCode::Char('N')), Some(Action::SortBy(ProcSort::Pid)));
        assert_eq!(
            h(KeyCode::Char('T')),
            Some(Action::SortBy(ProcSort::Started))
        );
        assert_eq!(h(KeyCode::Char(' ')), Some(Action::Tag));
    }

    #[test]
    fn every_profile_reaches_every_process_feature() {
        let wanted = [
            Action::Help,
            Action::Settings,
            Action::Search,
            Action::ToggleTree,
            Action::ToggleGroups,
            Action::Fold,
            Action::Unfold,
            Action::FoldAll(true),
            Action::FoldAll(false),
            Action::Tag,
            Action::ClearTags,
            Action::Kill,
            Action::ForceKill,
            Action::SignalMenu,
        ];
        for profile in Profile::ALL {
            for action in wanted {
                assert!(
                    bindings(Mode::Normal, profile)
                        .any(|b| b.action == action && !b.keys.is_empty()),
                    "{profile:?} cannot reach {action:?}"
                );
            }
        }
    }

    #[test]
    fn htop_function_keys_close_what_they_open() {
        let key = |n| KeyEvent::from(KeyCode::F(n));
        assert_eq!(
            resolve(key(1), Mode::Normal, Profile::Htop),
            Some(Action::Help)
        );
        assert_eq!(
            resolve(key(1), Mode::Help, Profile::Htop),
            Some(Action::Close)
        );
        assert_eq!(
            resolve(key(2), Mode::Normal, Profile::Htop),
            Some(Action::Settings)
        );
        assert_eq!(
            resolve(key(2), Mode::Settings, Profile::Htop),
            Some(Action::Close)
        );
        for mode in [Mode::Help, Mode::Settings] {
            assert_eq!(
                resolve(key(10), mode, Profile::Htop),
                Some(Action::Close),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn plus_and_minus_sample_in_details_instead_of_renicing() {
        for profile in Profile::ALL {
            for mode in [Mode::Normal, Mode::Detail] {
                assert_eq!(
                    resolve(press('+'), mode, profile),
                    Some(Action::IntervalFaster),
                    "{profile:?}/{mode:?}"
                );
                assert_eq!(
                    resolve(press('-'), mode, profile),
                    Some(Action::IntervalSlower),
                    "{profile:?}/{mode:?}"
                );
            }
        }
    }

    #[test]
    fn btop_profile_sort_keys() {
        let b = |code: KeyCode| resolve(KeyEvent::from(code), Mode::Normal, Profile::Btop);
        assert_eq!(b(KeyCode::Left), Some(Action::SortPrev));
        assert_eq!(b(KeyCode::Right), Some(Action::SortNext));
        assert_eq!(b(KeyCode::Char('r')), Some(Action::ReverseSort));
        assert_eq!(b(KeyCode::Char('f')), Some(Action::Search));
        assert_eq!(b(KeyCode::Char('e')), Some(Action::ToggleTree));
        assert_eq!(b(KeyCode::Char('t')), Some(Action::ToggleTree));
    }

    #[test]
    fn settings_keys_move_change_and_switch_sections() {
        let r = |key: KeyEvent| resolve(key, Mode::Settings, Profile::Siltop);
        let code = |c: KeyCode| KeyEvent::from(c);
        assert_eq!(r(press('j')), Some(Action::Move(1)));
        assert_eq!(r(code(KeyCode::Up)), Some(Action::Move(-1)));
        for key in [
            press('l'),
            press(' '),
            code(KeyCode::Right),
            code(KeyCode::Enter),
        ] {
            assert_eq!(r(key), Some(Action::SettingNext), "{key:?}");
        }
        assert_eq!(r(press('h')), Some(Action::SettingPrev));
        assert_eq!(r(code(KeyCode::Tab)), Some(Action::NextPanel));
        assert_eq!(r(code(KeyCode::BackTab)), Some(Action::PrevPanel));
        assert_eq!(r(press('/')), Some(Action::Search));
        assert_eq!(r(press('1')), None, "the numbered popup is gone");
    }

    #[test]
    fn settings_filter_takes_every_printable_as_text() {
        for c in (' '..='~').filter(|c| !c.is_control()) {
            assert_eq!(
                resolve(press(c), Mode::SettingsFilter, Profile::Siltop),
                Some(Action::FilterChar(c)),
                "{c}"
            );
        }
        assert_eq!(
            resolve(
                KeyEvent::from(KeyCode::Enter),
                Mode::SettingsFilter,
                Profile::Siltop
            ),
            Some(Action::Open)
        );
    }

    #[test]
    fn confirm_only_answers_yes_or_no() {
        for key in all_keys() {
            let action = resolve(key, Mode::Confirm, Profile::Siltop);
            assert!(
                matches!(
                    action,
                    None | Some(Action::ConfirmYes | Action::ConfirmNo | Action::Quit)
                ),
                "{key:?} -> {action:?}"
            );
        }
    }

    #[test]
    fn help_overlay_only_scrolls_or_closes() {
        for key in all_keys() {
            let action = resolve(key, Mode::Help, Profile::Siltop);
            assert!(
                matches!(
                    action,
                    None | Some(
                        Action::Close
                            | Action::Quit
                            | Action::Move(_)
                            | Action::Page(_)
                            | Action::Top
                            | Action::Bottom
                    )
                ),
                "{key:?} -> {action:?}"
            );
        }
    }

    #[test]
    fn every_visible_action_is_in_help_for_every_profile() {
        for profile in Profile::ALL {
            let groups = help_groups(profile);
            let text: String = groups
                .iter()
                .flat_map(|g| g.rows.iter().map(|(k, h)| format!("{k} {h}\n")))
                .collect();
            for mode in [Mode::Normal, Mode::Search, Mode::Detail, Mode::Settings] {
                for binding in bindings(mode, profile).filter(|b| !b.help.is_empty()) {
                    assert!(text.contains(binding.help), "{profile:?}: {}", binding.help);
                    for key in binding.keys {
                        assert!(text.contains(&key.label()), "{profile:?}: {}", key.label());
                    }
                }
            }
        }
    }

    #[test]
    fn help_rows_fit_a_60_column_popup() {
        for profile in Profile::ALL {
            for group in help_groups(profile) {
                for (keys, help) in &group.rows {
                    let width = keys.chars().count() + help.chars().count();
                    assert!(width + 6 <= 58, "{profile:?}: {keys} {help}");
                }
            }
        }
    }

    #[test]
    fn help_groups_are_unique_and_ordered() {
        let groups = help_groups(Profile::Siltop);
        let titles: Vec<&str> = groups.iter().map(|g| g.title).collect();
        let unique: HashSet<&str> = titles.iter().copied().collect();
        assert_eq!(unique.len(), titles.len(), "{titles:?}");
        assert_eq!(titles.first(), Some(&"general"));
        assert!(groups.iter().all(|g| !g.rows.is_empty()));
    }

    #[test]
    fn env_and_alias_profiles_reach_the_settings_row_by_canonical_name() {
        let mut session = siltop_core::Config::default();
        Profile::apply_session(&mut session, Some("htop"));
        assert_eq!(session.keymap, "htop");
        let mut session = siltop_core::Config {
            keymap: String::from("btop++"),
            ..siltop_core::Config::default()
        };
        Profile::apply_session(&mut session, None);
        assert_eq!(session.keymap, "btop");
        Profile::apply_session(&mut session, Some("nonsense"));
        assert_eq!(
            session.keymap, "btop",
            "an unknown variable changes nothing"
        );
        for profile in Profile::ALL {
            assert_eq!(Profile::by_name(profile.name()), Some(profile));
        }
    }

    #[test]
    fn profiles_parse_by_name() {
        assert_eq!(Profile::by_name("htop"), Some(Profile::Htop));
        assert_eq!(Profile::by_name(" BTOP "), Some(Profile::Btop));
        assert_eq!(Profile::by_name("default"), Some(Profile::Siltop));
        assert_eq!(Profile::by_name("vim"), None);
    }

    #[test]
    fn key_labels_are_human() {
        assert_eq!(ch(' ').label(), "space");
        assert_eq!(CTRL_C.label(), "ctrl-c");
        assert_eq!(f(10).label(), "F10");
        assert_eq!(PGDN.label(), "pgdn");
        assert_eq!(UP.label(), "↑");
    }

    #[test]
    fn guide_key_table_is_generated_from_the_keymap() {
        let readme = include_str!("../../../docs/guide.md");
        let table = readme_table();
        assert!(
            readme.contains(&table),
            "guide key table is stale; replace it with:\n{table}"
        );
    }

    #[test]
    fn digits_pick_presets_in_every_profile() {
        for profile in Profile::ALL {
            for (i, preset) in Preset::ALL.iter().enumerate() {
                let c = char::from(b'1' + u8::try_from(i).unwrap());
                assert_eq!(
                    resolve(press(c), Mode::Normal, profile),
                    Some(Action::Preset(*preset)),
                    "{profile:?} {c}"
                );
            }
        }
    }

    #[test]
    fn zen_key_does_not_steal_htop_pause() {
        assert_eq!(
            resolve(press('Z'), Mode::Normal, Profile::Siltop),
            Some(Action::ToggleZen)
        );
        assert_eq!(
            resolve(press('Z'), Mode::Normal, Profile::Btop),
            Some(Action::ToggleZen)
        );
        assert_eq!(
            resolve(press('Z'), Mode::Normal, Profile::Htop),
            Some(Action::Pause),
            "htop's Z stays pause"
        );
    }

    #[test]
    fn preset_digits_stay_text_while_filtering() {
        for c in '1'..='5' {
            assert_eq!(
                resolve(press(c), Mode::Search, Profile::Siltop),
                Some(Action::FilterChar(c))
            );
        }
    }
}
