//! rc.2 regressions: the footer at small sizes, search that keeps its
//! filter, the session timeline, the mouse path to every action.

use super::*;

fn on_battery(app: &mut App) {
    app.snapshot.battery.present = true;
    app.snapshot.battery.percent = Some(79);
    app.snapshot.battery.source = siltop_core::PowerSource::Battery;
}

fn footer(app: &mut App) -> String {
    app.screen().last().cloned().unwrap_or_default()
}

/// The toast either shows whole or ends in `…`, and never touches the
/// status block: `no79% ▼` and `…part o79%` were the rc.1 bugs.
fn assert_clean_footer(line: &str, toast: &str) {
    let status = ["79% ▼", "● "]
        .iter()
        .filter_map(|s| line.find(s))
        .min()
        .unwrap_or(line.len());
    let before = &line[..status];
    assert!(before.ends_with("  "), "no gap before the status: {line:?}");
    if let Some(at) = line.find(&toast[..toast.len().min(6)]) {
        let shown = line[at..status].trim_end();
        assert!(
            toast == shown
                || (shown.ends_with('…') && toast.starts_with(shown.trim_end_matches('…'))),
            "{shown:?} is neither whole nor cut cleanly: {line:?}"
        );
    }
}

#[test]
fn footer_toasts_never_run_into_the_status_block() {
    for (w, h) in [(80, 24), (40, 12), (120, 40), (60, 20)] {
        for toast in [
            String::from("no process selected"),
            String::from("theme nord-btop · imported from btop"),
            String::from("owned by root: needs admin to signal this process at all"),
        ] {
            let mut app = app();
            on_battery(&mut app);
            app.at(w, h);
            app.set_processes(procs(30));
            app.set_toast(toast.clone(), ToastKind::Warn, STATUS_TTL);
            let line = footer(&mut app);
            assert_clean_footer(&line, &toast);
            assert!(line.contains("● 500 ms"), "{w}x{h}: {line:?}");
            assert!(
                line.contains("? help") && line.contains("q quit"),
                "{line:?}"
            );
        }
    }
}

fn filtered_app() -> App {
    let mut app = app();
    app.at(80, 24);
    app.set_processes(procs(30));
    for c in "/proc1".chars() {
        press(&mut app, c);
    }
    app
}

#[test]
fn tab_and_enter_leave_search_keeping_the_filter() {
    for code in [KeyCode::Tab, KeyCode::Enter] {
        let mut app = filtered_app();
        let n = app.proc.model.len();
        press_code(&mut app, code);
        assert!(!app.searching && app.mode() == Mode::Normal, "{code:?}");
        assert_eq!(app.proc.filter, "proc1");
        assert_eq!(app.proc.model.len(), n, "the table stays filtered");
        let search = &app.screen()[1];
        assert!(search.contains("filter: proc1 ✕"), "{search}");
    }
}

#[test]
fn space_tags_and_x_arms_on_the_filtered_list() {
    let mut app = filtered_app();
    press_code(&mut app, KeyCode::Tab);
    let first = app.proc.selected_pid().unwrap();
    press(&mut app, ' ');
    assert_eq!(app.proc.tags.pids().collect::<Vec<_>>(), vec![first]);
    assert!(app.proc.filter == "proc1", "space tags instead of typing");
    press(&mut app, 'x');
    assert!(app.confirm_kill, "x arms on the filtered list");
    assert_eq!(app.confirm.map(|t| t.pid), Some(first));
}

#[test]
fn esc_still_clears_a_kept_filter() {
    let mut app = filtered_app();
    press_code(&mut app, KeyCode::Enter);
    press_code(&mut app, KeyCode::Esc);
    assert!(app.proc.filter.is_empty());
}

#[test]
fn the_filter_chip_clears_on_click_and_edits_elsewhere() {
    let mut app = filtered_app();
    press_code(&mut app, KeyCode::Tab);
    let line = app.screen()[1].clone();
    app.click(column_of(&line, "✕"), 1);
    assert!(app.proc.filter.is_empty(), "{line}");
    assert!(
        app.status
            .as_ref()
            .is_some_and(|(s, _)| s == "filter cleared")
    );
    let mut app = filtered_app();
    press_code(&mut app, KeyCode::Tab);
    let line = app.screen()[1].clone();
    app.click(column_of(&line, "proc1"), 1);
    assert!(app.searching, "a click on the text edits it");
    assert_eq!(app.proc.filter, "proc1");
}

#[test]
fn a_long_filter_shows_its_end_while_typing() {
    let mut app = app();
    app.at(80, 24);
    for c in "/abcdefghijklmnopqrstuvwxyz0123456789-end".chars() {
        press(&mut app, c);
    }
    let line = app.screen()[1].clone();
    assert!(line.contains("-end▌"), "{line}");
}

#[test]
fn footer_hints_follow_the_key_profile() {
    let mut app = app();
    app.set_profile(Profile::Htop);
    app.at(120, 40);
    app.set_processes(procs(10));
    let line = footer(&mut app);
    for chip in ["F1 help", "F3 filter", "x kill", "F2 settings", "F10 quit"] {
        assert!(line.contains(chip), "{chip}: {line}");
    }
    app.set_profile(Profile::Siltop);
    let line = footer(&mut app);
    for chip in ["? help", "f filter", "s settings", "q quit"] {
        assert!(line.contains(chip), "{chip}: {line}");
    }
    // every hint is the key that really runs it
    for profile in Profile::ALL {
        let keys = keymap::hints(profile);
        for (label, action) in [
            (keys.help, Action::Help),
            (keys.quit, Action::Quit),
            (keys.search, Action::Search),
            (keys.settings, Action::Settings),
            (keys.pause, Action::Pause),
        ] {
            assert_eq!(
                keymap::key_for(Mode::Normal, profile, action).as_deref(),
                Some(label),
                "{profile:?}"
            );
        }
    }
}

fn appearance(w: u16, h: u16) -> App {
    let mut app = app();
    app.at(w, h);
    app.open_settings(SettingsCategory::Appearance);
    app
}

#[test]
fn appearance_steps_through_every_theme() {
    let mut app = appearance(80, 24);
    let mut seen = std::collections::HashSet::new();
    for _ in 0..theme_options().len() {
        app.send(Action::SettingNext);
        seen.insert(app.config.theme.clone());
    }
    assert_eq!(seen.len(), theme_options().len(), "{seen:?}");
    assert!(seen.contains("dracula"), "{seen:?}");
}

#[test]
fn every_theme_swatch_is_reachable_by_wheel_at_80x24() {
    let mut app = appearance(80, 24);
    let screen = app.screen().join("\n");
    assert!(screen.contains("of 18"), "a scroll hint: {screen}");
    let (col, row) = {
        let lines = app.screen();
        let y = lines.iter().position(|l| l.contains("billabong")).unwrap();
        (column_of(&lines[y], "billabong"), u16::try_from(y).unwrap())
    };
    let mut seen = std::collections::HashSet::new();
    for _ in 0..30 {
        for line in app.screen() {
            for name in theme_options() {
                if line.contains(&format!("   {name} ")) {
                    seen.insert(*name);
                }
            }
        }
        app.send(Action::Scroll { col, row, delta: 1 });
    }
    assert_eq!(seen.len(), theme_options().len(), "{seen:?}");
    assert_eq!(
        app.settings.as_ref().unwrap().row,
        0,
        "the wheel scrolled the preview"
    );
}

#[test]
fn pack_swatches_line_up_with_core_ones() {
    let mut app = appearance(120, 40);
    let lines = app.screen();
    let col = |name: &str| {
        let line = lines
            .iter()
            .find(|l| l.contains(&format!("   {name} ")))
            .unwrap();
        line.chars().position(|c| c == '■').unwrap()
    };
    assert_eq!(col("billabong"), col("dracula"), "{}", lines.join("\n"));
}

#[test]
fn a_theme_picked_by_key_scrolls_into_view() {
    let mut app = appearance(80, 24);
    for _ in 0..16 {
        app.send(Action::SettingNext);
    }
    let name = app.config.theme.clone();
    let screen = app.screen().join("\n");
    assert!(screen.contains(&format!("{name} ✓")), "{name}\n{screen}");
}

#[test]
fn the_settings_header_never_overlaps_its_close_button() {
    for (w, h) in [(20, 5), (40, 12), (60, 16), (80, 24)] {
        let mut app = app();
        app.at(w, h);
        app.open_settings(SettingsCategory::General);
        let top = app.screen()[0].clone();
        let close = if w >= 48 { "esc close" } else { "[×]" };
        assert!(top.trim_end().ends_with(close), "{w}x{h}: {top:?}");
        let at = top.find(close).unwrap();
        assert!(top[..at].ends_with(' '), "{w}x{h}: {top:?}");
        app.click(column_of(&top, close) + 1, 0);
        assert!(app.settings.is_none(), "{w}x{h}: the close button works");
    }
}

#[test]
fn ctrl_z_suspends_from_every_screen_and_sigcont_repaints() {
    for profile in Profile::ALL {
        for mode in Mode::ALL {
            let key = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL);
            assert_eq!(
                keymap::resolve(key, mode, profile),
                Some(Action::Suspend),
                "{profile:?}/{mode:?}"
            );
        }
    }
    let mut app = app();
    app.update(
        AppMsg::Input(TermEvent::Key(KeyEvent::new(
            KeyCode::Char('z'),
            KeyModifiers::CONTROL,
        ))),
        app.now,
    );
    assert!(app.suspend && !app.quit);
    let mut app = self::app();
    app.update(AppMsg::Suspend, app.now);
    assert!(app.suspend, "SIGTSTP from outside suspends too");
    app.update(AppMsg::Continued, app.now);
    assert!(app.repaint, "SIGCONT repaints the whole screen");
}

#[test]
fn a_sort_the_header_hides_is_named_in_the_title() {
    let mut app = app();
    app.at(80, 24);
    app.set_processes(procs(10));
    let top = app.screen()[0].clone();
    assert!(!top.contains(" by "), "cpu% is in the header: {top}");
    app.send(Action::SortBy(ProcSort::Started));
    let label = ProcSort::Started.label();
    let top = app.screen()[0].clone();
    let header = app.screen()[2].clone();
    assert!(
        !header.contains("started"),
        "hidden at 80 columns: {header}"
    );
    assert!(top.contains(&format!("by {label}")), "{top}");
    app.send(Action::ReverseSort);
    let top = app.screen()[0].clone();
    assert!(top.contains('↑') || top.contains('↓'), "{top}");
}

#[test]
fn the_first_live_sweep_shows_no_cpu_until_a_second_one() {
    let mut first = snapshot();
    first.processes[0].cpu = Some(140.0);
    let mut app = scripted(vec![first, snapshot()]);
    app.blank_first_cpu = true;
    assert!(app.pump());
    app.at(120, 40);
    assert!(app.snapshot.processes.iter().all(|p| p.cpu.is_none()));
    let screen = app.screen().join("\n");
    assert!(!screen.contains("140.0"), "{screen}");
    assert!(screen.contains("—"), "{screen}");
    assert!(app.pump());
    assert!(app.snapshot.processes.iter().all(|p| p.cpu.is_some()));
}

/// How the mouse runs `action` in `mode` when no menu lists it: the
/// pointer itself. `None` means there is no mouse path.
fn pointer_path(mode: Mode, action: Action) -> Option<&'static str> {
    use Action as A;
    Some(match (mode, action) {
        (_, A::Menu) => "the footer's siltop ≡, or a right-click",
        (_, A::Help) => "the footer's ? help chip",
        (_, A::Quit) => "the footer's q quit chip",
        (_, A::Cancel | A::Close | A::ConfirmNo) => {
            "a [×], esc close or n cancel, or a click outside"
        }
        (_, A::ConfirmYes) => "the y yes chip, or a click on a signal row",
        (_, A::Move(_) | A::Page(_) | A::Top | A::Bottom) => "the wheel, or a click on the row",
        (_, A::Open) => "a double-click, a button or a menu row",
        (_, A::FilterChar(_) | A::FilterBackspace) => "text entry; the filter's ✕ clears it",
        (_, A::FilterClear) => "the filter's ✕",
        (_, A::SortBy(_) | A::CycleSort) => "a click on a column header (again reverses)",
        (_, A::FilterDone) => "a click on a row leaves search, filter kept",
        (Mode::SettingsFilter | Mode::Settings, A::Search) => "text entry; categories are clicks",
        (Mode::Detail, A::NextPanel | A::PrevPanel | A::DossierTab(_)) => "the page tabs",
        (Mode::Settings, A::NextPanel | A::PrevPanel) => "the category column",
        (Mode::Settings | Mode::SettingsFilter, A::SettingNext | A::SettingPrev) => {
            "a click on the value"
        }
        (Mode::Settings, A::CycleTheme) => "a click on a theme swatch",
        (Mode::Timeline, A::Scrub(_)) => "a click or drag on the plot",
        (Mode::Timeline, A::Range(_)) => "the range chips",
        (Mode::Timeline, A::Timeline) => "the page's [×]",
        (Mode::SignalMenu, A::SignalMenu) => "the picker's [×]",
        _ => return None,
    })
}

/// The owner's rule: everything the keys do, the mouse can do too. Every
/// action in every mode of every profile is in a menu or has a pointer
/// path; a new key without one fails here.
#[test]
fn every_keymap_action_has_a_mouse_path() {
    let menus: Vec<Action> = super::menus::main_actions()
        .into_iter()
        .chain(super::menus::row_actions())
        .collect();
    let mut missing = Vec::new();
    for profile in Profile::ALL {
        for mode in Mode::ALL {
            for binding in keymap::bindings(mode, profile) {
                let action = binding.action;
                if !menus.contains(&action) && pointer_path(mode, action).is_none() {
                    missing.push(format!("{profile:?}/{mode:?}: {action:?}"));
                }
            }
        }
    }
    missing.sort();
    missing.dedup();
    assert!(missing.is_empty(), "no mouse path:\n{}", missing.join("\n"));
}

#[test]
fn the_main_menu_lists_every_command_with_its_key() {
    let mut app = app();
    app.at(120, 40);
    app.send(Action::Menu);
    let menu = app.menu.as_ref().unwrap();
    for action in super::menus::main_actions() {
        let i = menu
            .actions
            .iter()
            .position(|a| *a == Some(action))
            .unwrap();
        let want = keymap::key_for(Mode::Normal, Profile::Siltop, action)
            .or_else(|| keymap::key_for(Mode::Detail, Profile::Siltop, action));
        if let Some(want) = want {
            assert_eq!(menu.entries[i].key, want, "{action:?}");
        }
    }
    // every entry can be reached with the wheel
    let entries = menu.entries.len();
    let (mut seen, mut last) = (std::collections::HashSet::new(), usize::MAX);
    for _ in 0..entries * 2 {
        let selected = app.menu.as_ref().unwrap().selected;
        seen.insert(selected);
        if selected == last {
            break;
        }
        last = selected;
        app.send(Action::Scroll {
            col: 60,
            row: 20,
            delta: 1,
        });
    }
    let items = app.menu.as_ref().unwrap().actions.iter().flatten().count();
    assert_eq!(seen.len(), items, "the wheel walks every item");
}

#[test]
fn menu_keys_follow_the_htop_profile() {
    let mut app = app();
    app.set_profile(Profile::Htop);
    app.at(120, 40);
    app.send(Action::Menu);
    let menu = app.menu.as_ref().unwrap();
    let key = |label: &str| {
        let i = menu.entries.iter().position(|e| e.label == label).unwrap();
        menu.entries[i].key.clone()
    };
    assert_eq!(key("Settings"), "F2");
    assert_eq!(key("Keys & help"), "F1");
    assert_eq!(key("Pause / resume"), "Z");
}

fn right_click_row(app: &mut App, name: &str) -> (u16, u16) {
    let lines = app.screen();
    let y = lines
        .iter()
        .position(|l| l.contains(&format!(" {name} ")))
        .unwrap();
    let at = (column_of(&lines[y], name), u16::try_from(y).unwrap());
    app.send(Action::ContextMenu {
        col: at.0,
        row: at.1,
    });
    at
}

fn pick(app: &mut App, label: &str) {
    let lines = app.screen();
    let y = lines
        .iter()
        .position(|l| l.contains(label))
        .unwrap_or_else(|| panic!("{label}"));
    app.click(column_of(&lines[y], label), u16::try_from(y).unwrap());
}

#[test]
fn right_click_on_a_row_selects_it_and_its_menu_acts_on_it() {
    let mut app = app();
    app.at(120, 40);
    app.set_processes(procs(10));
    right_click_row(&mut app, "proc3");
    let pid = app.proc.selected_pid().unwrap();
    assert_eq!(app.proc_name(pid), "proc3", "the row is selected");
    let menu = app.menu.as_ref().unwrap();
    assert!(menu.title.contains("proc3"), "{}", menu.title);
    for label in [
        "Details",
        "Quit (TERM)",
        "Send signal…",
        "Copy pid",
        "Lower priority",
    ] {
        assert!(menu.entries.iter().any(|e| e.label == label), "{label}");
    }
    pick(&mut app, "Quit (TERM)");
    assert!(app.menu.is_none());
    assert!(app.confirm_kill, "a destructive pick still asks first");
    assert_eq!(app.confirm.map(|t| t.pid), Some(pid));
    let footer = footer(&mut app);
    app.click(column_of(&footer, "n cancel"), 39);
    assert!(!app.confirm_kill);
    right_click_row(&mut app, "proc3");
    pick(&mut app, "Copy pid");
    assert_eq!(app.clipboard, crate::osc::sequence(&pid.to_string()));
    assert!(
        app.status.as_ref().is_some_and(|(s, _)| s == "copied pid"),
        "feedback"
    );
    right_click_row(&mut app, "proc3");
    pick(&mut app, "Details");
    assert_eq!(app.detail_pid, Some(pid));
}

#[test]
fn right_click_off_a_row_opens_the_main_menu_at_the_pointer() {
    let mut app = app();
    app.at(120, 40);
    app.send(Action::ContextMenu { col: 5, row: 5 });
    let menu = app.menu.as_ref().unwrap();
    assert_eq!(menu.title, "siltop");
    assert_eq!(menu.anchor, Some((5, 5)));
}

#[test]
fn a_replay_row_menu_offers_no_signals() {
    let now = Instant::now();
    let mut app = super::replaying_table(now);
    app.at(120, 40);
    let lines = app.screen();
    let y = (3..38).find(|&y| lines[y].contains("proc")).unwrap();
    app.send(Action::ContextMenu {
        col: 70,
        row: u16::try_from(y).unwrap(),
    });
    let menu = app.menu.as_ref().unwrap();
    for e in &menu.entries {
        assert!(
            !e.label.contains("TERM") && !e.label.contains("signal"),
            "{}",
            e.label
        );
    }
}

#[test]
fn details_tabs_and_close_are_clickable() {
    let mut app = app();
    app.at(120, 40);
    app.set_processes(procs(10));
    press_code(&mut app, KeyCode::Enter);
    assert!(app.detail_pid.is_some());
    let lines = app.screen();
    let y = lines.iter().position(|l| l.contains("2 threads")).unwrap();
    app.click(column_of(&lines[y], "threads"), u16::try_from(y).unwrap());
    assert_eq!(app.proc.tab, DossierTab::Threads);
    let lines = app.screen();
    let title = lines.iter().position(|l| l.contains("╭ process ")).unwrap();
    assert!(lines[title].contains("[×]"), "{}", lines[title]);
    app.click(
        column_of(&lines[title], "[×]") + 1,
        u16::try_from(title).unwrap(),
    );
    assert!(app.detail_pid.is_none());
}

#[test]
fn every_overlay_has_a_working_close_button() {
    let close_at = |app: &mut App| {
        let lines = app.screen();
        let y = lines
            .iter()
            .rposition(|l| l.contains("[×]"))
            .unwrap_or_else(|| panic!("no [×]:\n{}", lines.join("\n")));
        (column_of(&lines[y], "[×]") + 1, u16::try_from(y).unwrap())
    };
    let mut app = app();
    app.at(120, 40);
    app.set_processes(procs(10));
    app.send(Action::Help);
    let (c, r) = close_at(&mut app);
    app.click(c, r);
    assert!(!app.help, "help");
    app.send(Action::Menu);
    let (c, r) = close_at(&mut app);
    app.click(c, r);
    assert!(app.menu.is_none(), "menu");
    app.send(Action::SignalMenu);
    assert!(app.signal_menu.is_some());
    let (c, r) = close_at(&mut app);
    app.click(c, r);
    assert!(app.signal_menu.is_none(), "signal picker");
    app.update_offer = Some(super::super::PendingUpdate {
        version: String::from("9.9.9"),
        url: String::new(),
        sha256: String::new(),
        notes: String::new(),
        brew: false,
    });
    app.send(Action::CheckUpdate);
    assert!(app.update_dialog);
    let (c, r) = close_at(&mut app);
    app.click(c, r);
    assert!(!app.update_dialog, "update dialog");
}

#[test]
fn the_signal_picker_centres_on_the_details_card() {
    let mut app = app();
    app.at(120, 40);
    app.set_processes(procs(10));
    press_code(&mut app, KeyCode::Enter);
    press(&mut app, 'X');
    let view = app.view();
    let area = siltop_ui::signal_menu_area(&view, app.last_area);
    let card = siltop_ui::detail_rect(app.proc_pane().unwrap());
    assert_eq!(area, card, "centred on the card it was raised from");
    let menu = siltop_ui::signal_menu_rect(area, &view.signal_menu.unwrap());
    let mid = |r: Rect| (r.x + r.width / 2, r.y + r.height / 2);
    let (a, b) = (mid(menu), mid(card));
    assert!(
        a.0.abs_diff(b.0) <= 1 && a.1.abs_diff(b.1) <= 1,
        "{menu:?} in {card:?}"
    );
}

#[test]
fn a_live_sleeping_process_reads_sleeping_not_a_dash() {
    let mut app = app();
    app.at(120, 40);
    let mut list = procs(3);
    for p in &mut list {
        p.status = "";
        p.cpu = Some(0.0);
    }
    app.set_processes(list);
    press_code(&mut app, KeyCode::Enter);
    let screen = app.screen().join("\n");
    assert!(screen.contains("state  sleeping"), "{screen}");
}

#[test]
fn zen_gives_the_footer_row_to_the_graphs() {
    let mut app = app();
    app.at(80, 24);
    app.send(Action::ToggleZen);
    app.status = None;
    let last = app.screen()[23].clone();
    assert!(last.contains('╰'), "{last:?}");
}

#[test]
fn a_threads_page_note_wraps_whole_at_80_columns() {
    let mut app = app();
    app.demo = true;
    app.at(80, 24);
    app.set_processes(procs(5));
    press_code(&mut app, KeyCode::Enter);
    press(&mut app, '2');
    let screen = app.screen().join("\n");
    assert!(screen.contains("threads, files"), "{screen}");
    assert!(screen.contains("ports"), "{screen}");
    assert!(!screen.contains("demo has no…"), "{screen}");
}
