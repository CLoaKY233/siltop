//! The full-screen settings screen. It replaces the dashboard while open:
//! a title bar, categories on the left, rows on the right, the focused
//! row's description underneath and a key hint at the bottom. One
//! [`geometry`] feeds both painting and clicking.

mod registry;

use std::fmt::Write as _;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use siltop_core::Config;
use unicode_width::UnicodeWidthStr;

pub use registry::{Category, KEY_PROFILES, SETTINGS, SettingDef, Step, themes};
pub(crate) use registry::{Kind, visible};

use crate::layout::Panel;
use crate::theme::Theme;
use crate::widgets::AppView;
use crate::widgets::processes::fit;

/// Category column width, border gap included.
const CATEGORY_W: u16 = 14;
/// Below this the category column folds into the title (`settings › panels`).
const CATEGORY_MIN_AREA: u16 = 56;
const LABEL_W: u16 = 18;
/// Description lines under the list.
const DESC_ROWS: u16 = 2;

/// Where the settings screen is: category, row and the `/` filter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsState {
    pub category: Category,
    pub row: usize,
    pub filter: String,
    /// Typing goes to the filter until Enter or Esc.
    pub filtering: bool,
    /// The first theme swatch the appearance preview shows (it scrolls).
    pub swatch_top: usize,
}

impl SettingsState {
    #[must_use]
    pub fn open(category: Category) -> Self {
        Self {
            category,
            ..Self::default()
        }
    }

    pub fn rows(&self) -> impl Iterator<Item = &'static SettingDef> + '_ {
        visible(self.category, &self.filter)
    }

    #[must_use]
    pub fn selected(&self) -> Option<&'static SettingDef> {
        self.rows().nth(self.row)
    }

    /// Moves the row cursor, clamped to the rows on screen.
    pub fn move_row(&mut self, delta: i32) {
        let last = self.rows().count().saturating_sub(1);
        let next = i64::try_from(self.row).unwrap_or(0) + i64::from(delta);
        self.row = usize::try_from(next.max(0)).unwrap_or(0).min(last);
    }

    /// Next or previous category; leaves search.
    pub fn move_category(&mut self, delta: i32) {
        self.category = self.category.step(delta);
        self.select_category(self.category);
    }

    pub fn select_category(&mut self, category: Category) {
        self.category = category;
        self.row = 0;
        self.swatch_top = 0;
        self.filter.clear();
        self.filtering = false;
    }

    pub fn push_filter(&mut self, c: char) {
        self.filtering = true;
        self.filter.push(c);
        self.row = 0;
    }

    pub fn pop_filter(&mut self) {
        self.filter.pop();
        self.row = 0;
    }

    /// Esc: clear a filter first, then report that the screen should close.
    #[must_use]
    pub fn cancel(&mut self) -> bool {
        if self.filtering || !self.filter.is_empty() {
            self.filter.clear();
            self.filtering = false;
            self.row = 0;
            false
        } else {
            true
        }
    }
}

/// What a click on the settings screen lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsHit {
    Category(Category),
    /// A row's name: select it.
    Row(usize),
    /// A row's value: select it and step it forward.
    Value(usize),
    /// A theme swatch in the appearance preview, by option index.
    Theme(usize),
    Close,
}

/// Every rect on the settings screen for one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Geometry {
    pub title: Rect,
    pub close: Rect,
    pub categories: Vec<(Rect, Category)>,
    /// `(name, value, index into the visible rows)`.
    pub rows: Vec<(Rect, Rect, usize)>,
    /// Space under the rows for the category's extra: previews, key list, about.
    pub extra: Rect,
    /// The rule between categories and rows, when the column shows.
    pub divider: Option<Rect>,
    pub description: Rect,
    pub hint: Rect,
}

fn rect(x: u16, y: u16, width: u16, height: u16) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

/// `esc close`, or just `[×]` where the title needs the room.
fn close_label(width: u16) -> &'static str {
    if width >= 48 {
        "esc close"
    } else {
        crate::layout::CORNER_CLOSE
    }
}

/// Lays the screen out for `state`. The rows window scrolls so the
/// selected row is always on screen.
#[must_use]
pub(super) fn geometry(area: Rect, state: &SettingsState) -> Geometry {
    let right = area.x.saturating_add(area.width);
    let close_w = u16::try_from(close_label(area.width).width()).unwrap_or(0);
    let close = rect(
        right.saturating_sub(close_w + 1),
        area.y,
        close_w.min(area.width),
        area.height.min(1),
    );
    // the title stops a gap short of the close button
    let title = rect(
        area.x,
        area.y,
        close.x.saturating_sub(area.x).saturating_sub(1),
        area.height.min(1),
    );
    let top = area.y.saturating_add(2);
    let bottom = area.y.saturating_add(area.height);
    let hint = rect(area.x, bottom.saturating_sub(1), area.width, 1);
    let columns = area.width >= CATEGORY_MIN_AREA;
    let list_x = area
        .x
        .saturating_add(if columns { CATEGORY_W + 1 } else { 1 });
    let list_w = right.saturating_sub(list_x).saturating_sub(1);
    let description = rect(
        list_x,
        hint.y.saturating_sub(DESC_ROWS + 1),
        list_w,
        DESC_ROWS,
    );
    let body_h = description.y.saturating_sub(top).saturating_sub(1);
    let divider = columns.then(|| rect(area.x + CATEGORY_W - 1, top, 1, body_h));
    let categories = if columns {
        Category::ALL
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                let y = top.saturating_add(u16::try_from(i).ok()?);
                (y < top.saturating_add(body_h))
                    .then(|| (rect(area.x + 1, y, CATEGORY_W - 2, 1), *c))
            })
            .collect()
    } else {
        Vec::new()
    };
    let count = state.rows().count();
    let window = usize::from(body_h);
    let first = state.row.saturating_sub(window.saturating_sub(1));
    let label_w = LABEL_W.min(list_w / 2);
    let rows: Vec<(Rect, Rect, usize)> = (first..count)
        .take(window)
        .enumerate()
        .map(|(line, index)| {
            let y = top.saturating_add(u16::try_from(line).unwrap_or(u16::MAX));
            (
                rect(list_x, y, label_w, 1),
                rect(
                    list_x.saturating_add(label_w),
                    y,
                    list_w.saturating_sub(label_w),
                    1,
                ),
                index,
            )
        })
        .collect();
    let used = u16::try_from(rows.len()).unwrap_or(0);
    let extra_y = top.saturating_add(used).saturating_add(1);
    let extra = rect(
        list_x,
        extra_y,
        list_w,
        top.saturating_add(body_h).saturating_sub(extra_y),
    );
    Geometry {
        title,
        close,
        categories,
        rows,
        extra,
        divider,
        description,
        hint,
    }
}

/// Columns from a row's left edge to its label text: the select mark and a
/// space (`▌ theme`). The extras under the rows start their text here too.
const EXTRA_INDENT: &str = "  ";

/// An extra's first column (`version`, a theme name), padded so the second
/// column lines up with the value text of the rows above it.
fn extra_key(text: &str) -> String {
    let w = usize::from(LABEL_W);
    format!("{EXTRA_INDENT}{text:<w$}")
}

/// Swatch rows that fit under the preview's header line.
fn swatch_room(extra: Rect) -> usize {
    usize::from(extra.height.saturating_sub(1))
}

/// The first swatch shown: `top` clamped so the window stays full.
fn swatch_first(extra: Rect, top: usize) -> usize {
    top.min(themes().len().saturating_sub(swatch_room(extra)))
}

/// The theme swatch rows the appearance preview paints, one per option,
/// from `top` down as far as the space goes.
fn swatch_rows(extra: Rect, top: usize) -> impl Iterator<Item = (Rect, usize)> {
    let first = swatch_first(extra, top);
    let y0 = extra.y.saturating_add(1);
    (first..themes().len())
        .take(swatch_room(extra))
        .enumerate()
        .filter_map(move |(line, i)| {
            let y = y0.saturating_add(u16::try_from(line).ok()?);
            Some((rect(extra.x, y, extra.width, 1), i))
        })
}

/// The wheel over the appearance preview scrolls its swatches; true when
/// the pointer was there and the wheel was used.
pub fn scroll_swatches(
    area: Rect,
    state: &mut SettingsState,
    col: u16,
    row: u16,
    delta: i32,
) -> bool {
    if state.category != Category::Appearance || !state.filter.is_empty() {
        return false;
    }
    let extra = geometry(area, state).extra;
    if !crate::layout::contains(extra, col, row) {
        return false;
    }
    let first = swatch_first(extra, state.swatch_top);
    state.swatch_top = swatch_first(
        extra,
        first.saturating_add_signed(isize::try_from(delta).unwrap_or(0)),
    );
    true
}

/// Scrolls the preview just enough to show swatch `index` (a theme picked
/// by key must stay in view).
pub fn reveal_swatch(area: Rect, state: &mut SettingsState, index: usize) {
    let room = swatch_room(geometry(area, state).extra).max(1);
    let first = swatch_first(geometry(area, state).extra, state.swatch_top);
    if index < first {
        state.swatch_top = index;
    } else if index >= first + room {
        state.swatch_top = index + 1 - room;
    }
}

/// What the pointer is on, from the same geometry the frame painted.
#[must_use]
pub fn hit(area: Rect, state: &SettingsState, col: u16, row: u16) -> Option<SettingsHit> {
    target(area, state, col, row).map(|(_, hit)| hit)
}

/// The clickable rect under the pointer and what a click there does: the
/// one geometry both clicks and hover read.
#[must_use]
pub(super) fn target(
    area: Rect,
    state: &SettingsState,
    col: u16,
    row: u16,
) -> Option<(Rect, SettingsHit)> {
    let g = geometry(area, state);
    if crate::layout::contains(g.close, col, row) {
        return Some((g.close, SettingsHit::Close));
    }
    if let Some((r, c)) = g
        .categories
        .iter()
        .find(|(r, _)| crate::layout::contains(*r, col, row))
    {
        return Some((*r, SettingsHit::Category(*c)));
    }
    for (name, value, index) in &g.rows {
        if crate::layout::contains(*name, col, row) {
            return Some((*name, SettingsHit::Row(*index)));
        }
        if crate::layout::contains(*value, col, row) {
            return Some((*value, SettingsHit::Value(*index)));
        }
    }
    if state.category == Category::Appearance && state.filter.is_empty() {
        return swatch_rows(g.extra, state.swatch_top)
            .find(|(r, _)| crate::layout::contains(*r, col, row))
            .map(|(r, i)| (r, SettingsHit::Theme(i)));
    }
    None
}

fn line_at(frame: &mut Frame, r: Rect, line: Line<'_>) {
    if r.width > 0 && r.height > 0 {
        frame.render_widget(Paragraph::new(line), r);
    }
}

/// Paints the whole screen; nothing of the dashboard shows through.
pub(super) fn render(frame: &mut Frame, area: Rect, view: &AppView<'_>, state: &SettingsState) {
    let theme = view.theme;
    let g = geometry(area, state);
    let config = view.config;
    paint_chrome(frame, &g, state, theme);
    paint_rows(frame, &g, state, config, theme);

    if state.filter.is_empty() {
        match state.category {
            Category::Appearance => theme_preview(frame, g.extra, view, config, state.swatch_top),
            Category::Keys => key_list(frame, g.extra, view),
            Category::About => about(frame, g.extra, view),
            _ => {}
        }
    }

    let description = state.selected().map_or_else(
        || match state.category {
            Category::About => "What you are running and where your settings live.",
            _ => "",
        },
        |def| def.description,
    );
    let lines: Vec<Line> = wrap_fit(
        description,
        usize::from(g.description.width),
        usize::from(g.description.height),
    )
    .into_iter()
    .map(|line| Line::from(Span::styled(line, theme.dim())))
    .collect();
    frame.render_widget(Paragraph::new(lines), g.description);
    line_at(
        frame,
        g.hint,
        hint_line(view, state, usize::from(g.hint.width), theme),
    );
}

/// `text` word-wrapped to `width`, at most `rows` lines; the last line
/// ends in `…` when anything was left over.
fn wrap_fit(text: &str, width: usize, rows: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut words = text.split_whitespace().peekable();
    while lines.len() < rows && words.peek().is_some() {
        let mut line = String::new();
        while let Some(word) = words.peek() {
            let need = line.width() + usize::from(!line.is_empty()) + word.width();
            if need > width && !line.is_empty() {
                break;
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
            words.next();
        }
        lines.push(line);
    }
    if words.peek().is_some()
        && let Some(last) = lines.last_mut()
    {
        // the cut always shows, even when the last line was full
        let room = width.min(last.width() + 1);
        *last = fit(
            &format!("{last} …"),
            u16::try_from(room).unwrap_or(u16::MAX),
        );
    }
    lines
        .into_iter()
        .map(|l| fit(&l, u16::try_from(width).unwrap_or(u16::MAX)))
        .collect()
}

/// Title bar, close corner, the category column and its rule.
fn paint_chrome(frame: &mut Frame, g: &Geometry, state: &SettingsState, theme: &Theme) {
    let crumb = if g.categories.is_empty() || !state.filter.is_empty() {
        format!(" › {}", section_title(state))
    } else {
        String::new()
    };
    // Narrow: the wordmark goes first, then the crumb is cut with a `…`.
    let room = usize::from(g.title.width);
    let lead = if " siltop › settings".width() + crumb.width() <= room {
        " siltop › "
    } else {
        " "
    };
    let crumb = crate::widgets::footer::ellipsize(&crumb, room.saturating_sub(lead.width() + 8));
    line_at(
        frame,
        g.title,
        Line::from(vec![
            Span::styled(lead, theme.dim()),
            Span::styled("settings", theme.title()),
            Span::styled(crumb, theme.dim()),
        ]),
    );
    let close = match g.close.width {
        9 => vec![
            Span::styled("esc", theme.title()),
            Span::styled(" close", theme.dim()),
        ],
        _ => vec![Span::styled(crate::layout::CORNER_CLOSE, theme.dim())],
    };
    line_at(frame, g.close, Line::from(close));

    if let Some(rule) = g.divider {
        let lines: Vec<Line> = (0..rule.height)
            .map(|_| Line::from(Span::styled("│", theme.border(false))))
            .collect();
        frame.render_widget(Paragraph::new(lines), rule);
    }
    for (r, category) in &g.categories {
        let active = *category == state.category && state.filter.is_empty();
        let (mark, style) = if active {
            (theme.icons.select_mark(), theme.title())
        } else {
            (" ", theme.dim())
        };
        line_at(
            frame,
            *r,
            Line::from(vec![
                Span::styled(mark, Style::default().fg(theme.hi)),
                Span::styled(format!(" {}", category.label()), style),
            ]),
        );
    }
}

/// The visible rows: marker and label, then the value.
fn paint_rows(
    frame: &mut Frame,
    g: &Geometry,
    state: &SettingsState,
    config: &Config,
    theme: &Theme,
) {
    let first = g.rows.first().map_or(0, |(_, _, index)| *index);
    for ((name, value, index), def) in g.rows.iter().zip(state.rows().skip(first)) {
        let focused = *index == state.row;
        let label_style = if focused { theme.title() } else { theme.fg() };
        line_at(
            frame,
            *name,
            Line::from(vec![
                Span::styled(
                    if focused {
                        theme.icons.select_mark()
                    } else {
                        " "
                    },
                    Style::default().fg(theme.hi),
                ),
                // a gap always parts the label from its value
                Span::styled(
                    fit(&format!(" {}", def.label), name.width.saturating_sub(2)),
                    label_style,
                ),
            ]),
        );
        let mut line = value_line(def, config, focused, value.width, theme);
        if !state.filter.is_empty() {
            // search spans sections: say where each hit lives
            line.spans.push(Span::styled(
                format!("   {}", def.category.label()),
                theme.dim(),
            ));
        }
        line_at(frame, *value, line);
    }
    if g.rows.is_empty() && !state.filter.is_empty() {
        line_at(
            frame,
            g.extra,
            Line::from(Span::styled(
                format!("{EXTRA_INDENT}nothing matches “{}”", state.filter),
                theme.dim(),
            )),
        );
    }
}

fn section_title(state: &SettingsState) -> String {
    if state.filter.is_empty() {
        state.category.label().to_owned()
    } else {
        format!("search “{}”", state.filter)
    }
}

fn value_line(
    def: &SettingDef,
    config: &Config,
    focused: bool,
    width: u16,
    theme: &Theme,
) -> Line<'static> {
    // `‹ ` before and ` ›` after the value
    let shown = fit(&def.display(config), width.saturating_sub(4));
    let value_style = if focused {
        Style::default().fg(theme.hi)
    } else {
        theme.fg()
    };
    let arrows = !matches!(def.kind, Kind::Toggle);
    let mut spans = Vec::with_capacity(3);
    if arrows {
        spans.push(Span::styled(if focused { "‹ " } else { "  " }, theme.dim()));
    } else {
        spans.push(Span::raw("  "));
    }
    if matches!(def.kind, Kind::Toggle) {
        let on = shown == "on";
        spans.push(Span::styled(
            if on { "● on" } else { "○ off" },
            if on {
                Style::default().fg(theme.ok)
            } else {
                theme.dim()
            },
        ));
    } else {
        spans.push(Span::styled(shown, value_style));
        if focused {
            spans.push(Span::styled(" ›", theme.dim()));
        }
    }
    Line::from(spans)
}

/// Every theme as a row: its name, its four resource accents and a braille
/// ribbon in the cpu ramp, so a pick is made by eye, not by name.
fn theme_preview(frame: &mut Frame, extra: Rect, view: &AppView<'_>, config: &Config, top: usize) {
    const RIBBON: [&str; 8] = ["⣀", "⣠", "⣤", "⣴", "⣶", "⣾", "⣿", "⣶"];
    let theme = view.theme;
    if extra.height == 0 {
        return;
    }
    let rows: Vec<(Rect, usize)> = swatch_rows(extra, top).collect();
    let total = themes().len();
    let mut header = format!("{EXTRA_INDENT}preview · click one to use it");
    if let (Some((_, first)), Some((_, last))) = (rows.first(), rows.last())
        && rows.len() < total
    {
        let up = if *first > 0 { "↑" } else { " " };
        let down = if last + 1 < total { "↓" } else { " " };
        let _ = write!(
            header,
            " · {}–{} of {total} {up}{down} scroll",
            first + 1,
            last + 1
        );
    }
    line_at(
        frame,
        Rect { height: 1, ..extra },
        Line::from(Span::styled(header, theme.dim())),
    );
    for (r, i) in rows {
        let Some(name) = themes().get(i) else {
            continue;
        };
        let current = config.theme.eq_ignore_ascii_case(name);
        let sample = if *name == "auto" {
            None
        } else {
            Theme::by_name(name)
        };
        let mut spans = vec![Span::styled(
            extra_key(&if current {
                format!("{name} ✓")
            } else {
                (*name).to_owned()
            }),
            if current { theme.title() } else { theme.fg() },
        )];
        match sample {
            Some(sample) => {
                for panel in [Panel::Cpu, Panel::Gpu, Panel::Mem, Panel::Net] {
                    spans.push(Span::styled(
                        "■ ",
                        Style::default().fg(sample.panel_accent(panel)),
                    ));
                }
                for (k, glyph) in RIBBON.iter().enumerate() {
                    let t = k as f32 / (RIBBON.len() - 1) as f32;
                    spans.push(Span::styled(*glyph, Style::default().fg(sample.cpu.at(t))));
                }
            }
            None => spans.push(Span::styled("follows your terminal", theme.dim())),
        }
        line_at(frame, r, Line::from(spans));
    }
}

/// The keys for the current profile, read-only, as many as fit.
fn key_list(frame: &mut Frame, extra: Rect, view: &AppView<'_>) {
    let theme = view.theme;
    let key_w = view
        .help_groups
        .iter()
        .flat_map(|g| g.rows.iter().map(|(k, _)| k.width()))
        .max()
        .unwrap_or(0)
        .min(20);
    let lines: Vec<Line> = view
        .help_groups
        .iter()
        .flat_map(|group| {
            std::iter::once(Line::from(Span::styled(
                format!("{EXTRA_INDENT}{}", group.title),
                theme.dim(),
            )))
            .chain(group.rows.iter().map(move |(keys, help)| {
                Line::from(vec![
                    Span::styled(format!("{EXTRA_INDENT}  {keys:<key_w$}  "), theme.title()),
                    Span::styled(*help, theme.dim()),
                ])
            }))
        })
        .take(usize::from(extra.height))
        .collect();
    frame.render_widget(Paragraph::new(lines), extra);
}

fn about(frame: &mut Frame, extra: Rect, view: &AppView<'_>) {
    let theme = view.theme;
    let rows = [
        ("version", env!("CARGO_PKG_VERSION").to_owned()),
        (
            "config",
            view.config_path
                .unwrap_or("not saved (no home directory)")
                .to_owned(),
        ),
        ("history", siltop_core::store::RETENTION_LABEL.to_owned()),
    ];
    let (head, tail) = rows.split_at(1);
    let lines: Vec<Line> = head
        .iter()
        .cloned()
        .chain(view.about.iter().cloned())
        .chain(tail.iter().cloned())
        .chain(crate::widgets::identity::facts(view.snapshot))
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| {
            let key = extra_key(k);
            let room = extra
                .width
                .saturating_sub(u16::try_from(key.width()).unwrap_or(u16::MAX));
            Line::from(vec![
                Span::styled(key, theme.dim()),
                Span::styled(fit(&v, room), theme.fg()),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), extra);
}

fn hint_line(
    view: &AppView<'_>,
    state: &SettingsState,
    width: usize,
    theme: &Theme,
) -> Line<'static> {
    if state.filtering {
        return Line::from(vec![
            Span::styled(" / ", theme.title()),
            Span::styled(state.filter.clone(), Style::default().fg(theme.hi)),
            Span::styled("▏", theme.dim()),
            Span::styled("   enter keep · esc clear", theme.dim()),
        ]);
    }
    if let Some(status) = view.status {
        return Line::from(Span::styled(format!(" {status}"), theme.fg()));
    }
    let pairs = [
        ("↑↓", "move"),
        ("←→", "change"),
        ("tab", "section"),
        ("/", "search"),
        ("esc", "close"),
    ];
    // whole pairs only: a narrow screen drops them from the right
    let mut spans = Vec::with_capacity(pairs.len() * 2);
    let mut used = 0;
    for (key, verb) in pairs {
        let w = key.width() + verb.width() + 3;
        if used + w > width {
            break;
        }
        used += w;
        spans.push(Span::styled(format!(" {key}"), theme.title()));
        spans.push(Span::styled(format!(" {verb} "), theme.dim()));
    }
    Line::from(spans)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const SIZES: [(u16, u16); 7] = [
        (160, 45),
        (120, 40),
        (100, 30),
        (80, 24),
        (60, 16),
        (50, 20),
        (40, 12),
    ];

    #[test]
    fn geometry_stays_inside_the_area() {
        for (w, h) in SIZES {
            let area = Rect::new(0, 0, w, h);
            for category in Category::ALL {
                let g = geometry(area, &SettingsState::open(category));
                let all = [g.title, g.close, g.extra, g.description, g.hint]
                    .into_iter()
                    .chain(g.categories.iter().map(|(r, _)| *r))
                    .chain(g.rows.iter().flat_map(|(a, b, _)| [*a, *b]));
                for r in all {
                    assert!(
                        r.x + r.width <= w && r.y + r.height <= h,
                        "{w}x{h} {category:?} {r:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_row_name_and_value_hits_itself() {
        for (w, h) in SIZES {
            let area = Rect::new(0, 0, w, h);
            for category in Category::ALL {
                let state = SettingsState::open(category);
                for (name, value, index) in geometry(area, &state).rows {
                    if name.width > 0 {
                        assert_eq!(
                            hit(area, &state, name.x, name.y),
                            Some(SettingsHit::Row(index))
                        );
                    }
                    if value.width > 0 {
                        assert_eq!(
                            hit(area, &state, value.x + value.width - 1, value.y),
                            Some(SettingsHit::Value(index)),
                            "{w}x{h}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn every_category_hits_itself_when_the_column_shows() {
        let area = Rect::new(0, 0, 100, 30);
        let state = SettingsState::default();
        let g = geometry(area, &state);
        assert_eq!(g.categories.len(), Category::ALL.len());
        for (r, category) in g.categories {
            assert_eq!(
                hit(area, &state, r.x, r.y),
                Some(SettingsHit::Category(category))
            );
        }
    }

    #[test]
    fn narrow_screens_fold_the_category_column() {
        let g = geometry(Rect::new(0, 0, 50, 20), &SettingsState::default());
        assert!(g.categories.is_empty());
        assert!(!g.rows.is_empty());
    }

    #[test]
    fn close_corner_hits_close() {
        let area = Rect::new(0, 0, 80, 24);
        let g = geometry(area, &SettingsState::default());
        assert_eq!(
            hit(area, &SettingsState::default(), g.close.x, 0),
            Some(SettingsHit::Close)
        );
    }

    #[test]
    fn selected_row_stays_on_screen_when_scrolled() {
        let mut state = SettingsState {
            filter: String::from(" "),
            ..SettingsState::default()
        };
        // a blank filter is no filter
        assert_eq!(state.rows().count(), visible(Category::General, "").count());
        state.filter = String::from("e"); // matches almost every row
        let area = Rect::new(0, 0, 80, 14);
        let count = state.rows().count();
        for row in 0..count {
            state.row = row;
            let g = geometry(area, &state);
            assert!(
                g.rows.iter().any(|(_, _, i)| *i == row),
                "row {row} off screen"
            );
        }
    }

    #[test]
    fn move_row_clamps() {
        let mut state = SettingsState::open(Category::Panels);
        state.move_row(-5);
        assert_eq!(state.row, 0);
        state.move_row(100);
        assert_eq!(state.row, visible(Category::Panels, "").count() - 1);
    }

    #[test]
    fn tab_walks_every_category_and_resets_the_row() {
        let mut state = SettingsState {
            row: 2,
            ..SettingsState::default()
        };
        let mut seen = vec![state.category];
        for _ in 1..Category::ALL.len() {
            state.move_category(1);
            assert_eq!(state.row, 0);
            seen.push(state.category);
        }
        assert_eq!(seen, Category::ALL);
        state.move_category(1);
        assert_eq!(state.category, Category::General);
    }

    #[test]
    fn esc_clears_the_filter_before_closing() {
        let mut state = SettingsState::default();
        state.push_filter('t');
        assert!(!state.cancel(), "first esc clears");
        assert!(state.filter.is_empty() && !state.filtering);
        assert!(state.cancel(), "second esc closes");
    }

    /// Regression: the appearance preview, key list and about facts began
    /// one column left of the setting labels above them (`│  preview` under
    /// `│ ▌ theme`), and about values sat in a column of their own.
    #[test]
    fn extras_start_in_the_label_column() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        for (w, h) in [(100, 30), (120, 40), (160, 45)] {
            for category in Category::ALL {
                let mut fx = crate::widgets::golden::m5_pro();
                fx.settings = true;
                fx.settings_state = SettingsState::open(category);
                let area = Rect::new(0, 0, w, h);
                let g = geometry(area, &fx.settings_state);
                let Some((label, _, _)) = g.rows.first() else {
                    continue;
                };
                let label_x = label.x + 2;
                let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
                terminal
                    .draw(|frame| crate::widgets::render_app(frame, &fx.view()))
                    .unwrap();
                let buf = terminal.backend().buffer();
                let mut seen = 0;
                for y in g.extra.y..g.extra.y + g.extra.height {
                    let first = (g.extra.x..g.extra.x + g.extra.width)
                        .find(|&x| buf[(x, y)].symbol() != " ");
                    if let Some(x) = first {
                        seen += 1;
                        assert!(x >= label_x, "{w}x{h} {category:?} row {y}: col {x}");
                    }
                }
                if matches!(category, Category::Appearance | Category::About) {
                    assert!(seen > 0, "{w}x{h} {category:?} painted no extras");
                }
            }
        }
    }

    /// Regression: at 30x8 the label ran into its value
    /// (`sample every‹ 500 ms ›`) and the description was cut mid-word with
    /// no `…`; a long about path was cut the same way.
    #[test]
    fn narrow_settings_keep_a_gap_and_mark_every_cut() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let paint = |fx: &crate::widgets::tests_support::Fixture, w: u16, h: u16| {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal
                .draw(|frame| crate::widgets::render_app(frame, &fx.view()))
                .unwrap();
            let buf = terminal.backend().buffer();
            (0..h)
                .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut fx = crate::widgets::golden::m5_pro();
        fx.settings = true;
        fx.settings_state = SettingsState::open(Category::General);
        let text = paint(&fx, 30, 8);
        assert!(text.contains("sample eve… ‹"), "{text}");
        assert!(text.contains("slower…"), "{text}");
        fx.settings_state = SettingsState::open(Category::About);
        let text = paint(&fx, 40, 12);
        assert!(text.contains("~/.config/siltop/…"), "{text}");
    }

    #[test]
    fn wrap_fit_marks_what_it_leaves_out() {
        assert_eq!(wrap_fit("a bb ccc", 4, 2), ["a bb", "ccc"]);
        assert_eq!(wrap_fit("a bb ccc dd", 4, 2), ["a bb", "ccc…"]);
        assert_eq!(wrap_fit("aaaa bbbb cc", 4, 2), ["aaaa", "bbb…"]);
        assert!(wrap_fit("", 4, 2).is_empty());
    }

    #[test]
    fn extra_key_lines_up_with_the_value_column() {
        let key = extra_key("version");
        assert_eq!(
            key.chars().count(),
            EXTRA_INDENT.len() + usize::from(LABEL_W)
        );
        assert!(key.starts_with("  version"), "{key:?}");
    }

    #[test]
    fn appearance_swatches_hit_their_theme() {
        let area = Rect::new(0, 0, 120, 40);
        let state = SettingsState::open(Category::Appearance);
        let g = geometry(area, &state);
        let rows: Vec<_> = swatch_rows(g.extra, 0).collect();
        assert_eq!(rows.len(), themes().len(), "all themes fit at 120x40");
        for (r, i) in rows {
            assert_eq!(hit(area, &state, r.x + 2, r.y), Some(SettingsHit::Theme(i)));
        }
    }
}
