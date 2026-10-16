#![warn(unreachable_pub)]

mod braille;
mod chips;
mod chrome;
mod layout;
mod theme;
mod titles;
mod widgets;

pub use braille::{BrailleCell, braille_cell, render_cells, render_history};
pub use chips::KeyHints;
pub use chrome::{
    Axis, Graph, GraphInk, cell, panel_title, push_kv, push_token, render_scaled_graph,
    veil_outside,
};
pub use layout::{
    CORNER_CLOSE, CORNER_EXPAND, Hit, LayoutFlags, LayoutPlan, Panel, WORK_TOO_SMALL, card_close,
    hit_test, hop_ring, plan, work_fits,
};
pub use theme::{
    BtopTheme, ColorDepth, Heat, Icons, Opacity, Ramp, Rgb, Surface, TermPalette, Theme,
    install_user_themes,
};
pub use widgets::bench::{BenchView, render_bench};
pub use widgets::onboarding::{
    Screen as OnboardScreen, View as OnboardView, buttons as onboard_buttons, hit as onboard_hit,
    render as render_onboarding,
};
pub use widgets::power::has_power;
pub use widgets::update_dialog::{
    View as UpdateDialogView, close_rect as update_dialog_close, hit as update_dialog_hit,
    render as render_update_dialog,
};
pub use widgets::{
    Anchor, AppView, CardLink, DossierTab, FleetCard, FleetProc, Focus, HelpGroup, KEY_PROFILES,
    MenuEntry, MenuHit, MenuView, ModelRow, Nesting, ProcModel, ProcView, SCROLL_MARGIN, SETTINGS,
    SettingDef, SettingStep, SettingsCategory, SettingsHit, SettingsState, SignalMenuView,
    SignalRow, Tags, TimelineHit, TimelineParts, TimelineView, ToastKind, detail_actions,
    detail_close, detail_rect, detail_tabs, filtered_processes, fleet_grid, fleet_hit, footer_hit,
    help_max_scroll, help_rect, hit_at, hop_hit, hover_rect, inner_process_area, interval_label,
    menu_area, menu_first, menu_first_item, menu_hit, menu_page, menu_step, modal_rect, proc_rows,
    render_app, render_fleet, render_fleet_procs, reveal_swatch, scroll_swatches, settings_hit,
    signal_menu_area, signal_menu_rect, signal_menu_row_at, theme_options, timeline_hit,
    timeline_parts, timeline_plot_width,
};
