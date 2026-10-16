# Ratatui — complete field notes

Last updated: 2026-08-24. Researched against **ratatui 0.30.2** (cloned `main`, crate version in workspace `Cargo.toml`).

Official: <https://ratatui.rs> · API: <https://docs.rs/ratatui/latest/ratatui/> · Repo: <https://github.com/ratatui/ratatui>

## 1. What Ratatui is

Immediate-mode TUI toolkit. Every frame you rebuild widgets from app state and call `Terminal::draw`. Widgets are **consumed** when rendered (builder objects, not retained scene-graph nodes). Diffing happens at the **cell buffer** layer (`ratatui-core/src/buffer/diff.rs`), so unchanged cells are not rewritten to the terminal.

This is the opposite of a retained GUI. It is also why "don't draw unless something changed" is our job, not the library's.

Backends: **crossterm** (default, what we want on macOS), termion (Unix), termwiz, termina. macmon 0.8.2 already uses `ratatui = 0.30.2` + crossterm. bottom started on tui-rs and tracks Ratatui.

### 1.1 Crate split (0.30+)

From `ARCHITECTURE.md`:

```
ratatui                    ← apps depend on this
├── ratatui-core           ← Widget/StatefulWidget, Buffer, Layout, Style, Symbols, Terminal
├── ratatui-widgets        ← Block, Chart, Sparkline, … (no_std capable)
├── ratatui-crossterm
├── ratatui-termion / termwiz / termina
└── ratatui-macros
```

Apps: `use ratatui::{...}`. Widget libraries: depend on `ratatui-core` for stability.

Current workspace rust-version: **1.88**. Edition 2024.

**0.30 line we actually care about:**

| Release | Relevant bits |
| --- | --- |
| 0.30.0 | workspace split; `ratatui::run()`; `Marker::{Quadrant,Sextant,Octant}`; `BarChart::{vertical,horizontal,grouped}`; `LineGauge` custom glyphs; **`Block::merge_borders`**; `Block::title` is now `Into<Line>` (old `Title` struct gone) |
| 0.30.1 | `Block::shadow`; `GraphType::Area` + `Dataset::fill_to_y`; Canvas `FilledLine`; `Fill` widget; `Marker::Custom`; `Cell::column_span`; cheaper buffer diffs; `Terminal::apply_buffer` |
| 0.30.2 | current patch |

Do not start on 0.29 unless a crate is stuck there.

**0.30 layout default:** `Layout::new` packs with `Flex::Start`, **not** “last child eats leftover.” If you want the old tui-rs feel, set `Flex::Legacy` or give the last pane `Constraint::Fill(1)`.

### 1.2 Widget traits

| Trait | Signature | When |
| --- | --- | --- |
| `Widget` | `fn render(self, area: Rect, buf: &mut Buffer)` | Ephemeral widgets built each frame |
| `StatefulWidget` | `fn render(self, area, buf, state: &mut Self::State)` | Selection, scroll, cursor live *outside* the widget |
| `WidgetRef` / `StatefulWidgetRef` | `render_ref(&self, ...)` | Store widgets. Unstable (`unstable-widget-ref`). All built-ins implement it. |

Recommended app shape from the book (and what macmon does): one root `impl Widget for &App` (or `&mut App`), nested `Layout::split`, then `frame.render_widget` / `render_stateful_widget`.

**State rule of thumb:** if recreating the widget should *not* reset selection/scroll/history, keep that state in the model.

## 2. Built-in widgets — inventory

Source: `ratatui-widgets/src/lib.rs` plus each widget file. All re-exported at `ratatui::widgets`.

### 2.1 Block — chrome for every panel

Draws a framed region with optional titles, padding, borders.

**Options (fluent):** `borders`, `border_type`, `border_style`, `border_set` (custom chars), `title` / `title_top` / `title_bottom` (multiple, with `Alignment`), `title_style`, `padding`, `style`.

`Borders` bitflags: `TOP | RIGHT | BOTTOM | LEFT | ALL | NONE`.

`BorderType` (`ratatui-widgets/src/borders.rs`):

| Variant | Look | Use |
| --- | --- | --- |
| `Plain` (default) | `┌─┐│└─┘` | Dense dashboards |
| **`Rounded`** | `╭─╮│╰─╯` | **btop-like. Default for us.** |
| `Double` | `╔═╗║╚═╝` | Modal / focus |
| `Thick` | `┏━┓┃┗━┛` | Emphasis |
| `LightDoubleDashed` / `HeavyDoubleDashed` | dashed | Subtle separators |
| `LightTripleDashed` / `HeavyTripleDashed` |  |  |
| `LightQuadrupleDashed` / `HeavyQuadrupleDashed` |  |  |
| `QuadrantInside` / `QuadrantOutside` | half-block frames | Heavy aesthetic, eats a cell of contrast |

`Padding { left, right, top, bottom }`. `Block::bordered()` is the usual starter.

**0.30 extras:** `Block::shadow(Shadow::dark_shade().offset(...))` — optional, easy to overuse. **`Block::merge_borders(MergeStrategy::Exact)`** — adjacent panes share one line instead of `┘┏` seams. That is how you get a btop-like grid without wasting a column per internal edge. See upstream `collapsed-borders` example.

**Monitor use:** wrap every box. Put metric name + live value in the title (`CPU  12%  3.2 GHz`), help in `title_bottom`. Prefer merged rounded borders for the dashboard grid.

### 2.2 Gauge

Fat percentage bar. `percent(0..=100)` or `ratio(0.0..=1.0)`. `label`, `gauge_style` (fg = fill, bg = track), `use_unicode(true)` for eighth-blocks (`▏▎▍▌▋▊▉█`) instead of full cells, `block`.

**Monitor use:** RAM / disk / battery fill when we have height ≥ 3. Label as `"12.4 / 36 GB"`.

### 2.3 LineGauge

One-row gauge. Same ratio API, uses line/block symbols. **This is the compact meter** — per-core bars, fan, power.

`filled_style`, `unfilled_style`, `label`, `filled_symbol` / `unfilled_symbol` (0.30; old `.line_set` / `.gauge_style` are deprecated).

### 2.4 Sparkline

Compact history. One or more rows of vertical bars.

| Setter | Notes |
| --- | --- |
| `data(&[u64] \| &[Option<u64>] \| &[SparklineBar])` | `None` = absent sample |
| `max(u64)` | Else auto from data |
| `bar_set` | `symbols::bar::NINE_LEVELS` (default, `▁▂▃▄▅▆▇█`) or `THREE_LEVELS` |
| `direction` | `LeftToRight` (default) or `RightToLeft` |
| `style` | fg = bars |
| `absent_value_style` / `absent_value_symbol` | Gaps |
| `block` |  |

**Monitor use:** 1-row CPU/net/power history when a panel is short. macmon's "sparkline" view is exactly this. **Not** the btop braille look — resolution is 1 sample × 8 (or 3) vertical levels per cell, vs braille's 2 samples × 4 levels.

Per-bar color via `SparklineBar::from(v).style(...)` — enough for a cheap usage gradient.

### 2.5 Chart + Dataset + Axis

Cartesian plot. `GraphType`: `Scatter` | `Line` | `Bar` | `Area`.

`Dataset`: `name`, `data(&[(f64,f64)])`, `marker(Marker)`, `graph_type`, `style`, `fg`, plus `fill_to_y` for `Area`.

`Axis`: `title`, `bounds([min,max])`, `labels`, `style`, `labels_alignment`.

`Chart`: `datasets`, `block`, `x_axis`, `y_axis`, `hidden_legend_constraints`, `legend_position` (`TopRight` default, also Top/TopLeft/Bottom/…).

**Monitor use:** pretty, but **heavy and axis-hungry**. Axes steal rows/cols. Good for a "detail" zoom of one metric, bad as the default history widget. Prefer Sparkline / custom braille / `tui-bar-graph` for the main dashboard.

`Marker` on a Chart is the same enum as Canvas (see §4). `Marker::Braille` on `GraphType::Line` is a **polyline of dots**, not btop's filled history columns. `GraphType::Area` + `fill_to_y(0.0)` is closer (filled under the curve) but still not two-sample braille columns. Fine for a detail zoom.

Hide the legend on small panes: `.legend_position(None)` or `hidden_legend_constraints` (default hides if legend > ~25% of the chart).

### 2.6 BarChart

Categorical vertical (or horizontal) bars. Grouped datasets. `Bar` / `BarGroup`.

`BarChart::new` / `::vertical` / `::horizontal` / `::grouped`. `data(&[(&str,u64)])` or `BarGroup`. `bar_width`, `bar_gap`, `group_gap`, `bar_style`, `value_style`, `label_style`, `bar_set` (`NINE_LEVELS`), `max`, `direction`.

**Does not impl `Widget` for `&T`** — it mutates itself while rendering. Build it each frame.

**Monitor use:** per-core snapshot (not history). Horizontal for process CPU%. Grouped for user/sys. For *history* of one series, Sparkline / tui-bar-graph.

### 2.7 Canvas

A painter with world coordinates. Shapes: `Circle`, `Line`, `FilledLine`, `Points`, `Rectangle`, `Map`. Custom `Shape` trait: `fn draw(&self, painter: &mut Painter)`.

`Canvas::default().block(...).x_bounds([a,b]).y_bounds([c,d]).marker(Marker::Braille).paint(|ctx| { ctx.draw(&Points { coords, color }); ctx.layer(); ctx.print(x,y, "label"); })`.

Resolution by marker (see §4). Layers let you composite (e.g. block fill + braille overlay).

**Monitor use:** custom widgets that need sub-cell pixels — braille history we write ourselves, heatmaps, per-core "dot matrix". This is how you beat Sparkline's 8-level cap.

Caveat: one color per *cell*, not per braille dot. Same limitation as btop.

### 2.8 List — StatefulWidget

Items + `ListState` (selected, offset).

`highlight_style`, `highlight_symbol`, `repeat_highlight_symbol`, `highlight_spacing`, `direction` (`TopToBottom` / `BottomToTop`), `scroll_padding`, `block`.

**Monitor use:** process list if we do **not** need columns. We do need columns → Table.

### 2.9 Table — StatefulWidget

Rows of `Cell`s, `header`, `footer`, `widths(&[Constraint])` (**required** or columns are 0-wide), `column_spacing`, `row_highlight_style`, `column_highlight_style`, `cell_highlight_style`, `highlight_symbol`, `flex`.

`TableState`: selected row / column / cell, offset.

0.30.1: `Cell::column_span(n)` for header merges. `Row::height`, `.bottom_margin`. Highlight cascade is row → column → cell.

**Monitor use:** **the process table.** Also disks, NICs, sensors. Pair with `Scrollbar`.

### 2.10 Tabs — StatefulWidget

`Tabs::new(["CPU","MEM","NET"]).select(i).highlight_style(...).divider(...)`.

**Monitor use:** presets, or a "detail" pane that pages CPU / GPU / Sensors. Not a replacement for a tiled dashboard.

### 2.11 Paragraph

Styled, wrapped text. `wrap(Wrap { trim })`, `scroll((y,x))`, `alignment`, `block`. `Line` + `Span` + `Modifier`.

**Monitor use:** help overlay, process detail, sensor dump, footer hints.

### 2.12 Scrollbar — StatefulWidget

`Scrollbar::new(orientation).begin_symbol(...).end_symbol(...).track_symbol(...).thumb_symbol(...)`. Needs `ScrollbarState`.

Orientations: vertical right/left, horizontal top/bottom.

**Must set `ScrollbarState::content_length` or it draws blank.** Render *after* the table on an inner margin.

**Monitor use:** process table, long sensor lists.

### 2.13 Clear

Paints empty cells over whatever was there. **Required** for popups/menus so the previous frame does not bleed through (immediate mode has no z-order).

Pattern: render dashboard, then `Clear` in the modal rect, then the modal.

### 2.14 Fill (0.30)

Paints every cell in the area with one symbol + style. Cheap background wash.

### 2.15 Calendar (`feature = "widget-calendar"`)

`calendar::Monthly`. Not useful for a monitor.

### 2.16 RatatuiLogo / RatatuiMascot

Easter eggs. Ignore.

### 2.17 Text primitives as widgets

`&str`, `String`, `Span`, `Line`, `Text` implement `Widget`. Fine for a one-line title; use Paragraph for wrapping.

## 3. Layout — this is how we stay responsive

Docs: <https://ratatui.rs/concepts/layout/>

Coordinate origin: top-left `(0,0)`, `u16` cells.

```rust
let chunks = Layout::default()
    .direction(Direction::Vertical) // or Horizontal
    .constraints([
        Constraint::Length(3),       // exact rows/cols
        Constraint::Percentage(40),  // of *parent*
        Constraint::Ratio(1, 3),
        Constraint::Min(8),          // never smaller
        Constraint::Max(20),         // never larger
        Constraint::Fill(1),         // share leftover; weight 1
        Constraint::Fill(2),         // twice the leftover of Fill(1)
    ])
    .spacing(0)
    .margin(0)
    .flex(Flex::Legacy)
    .split(area);
```

Solver: **kasuari** (Cassowary-family). If constraints conflict, result is "close" and **non-deterministic**. Always give every graph pane a `Min` so it cannot collapse to 0.

**Priority if the solver cannot satisfy everything** (highest first): `Min` → `Max` → `Length` → `Percentage` → `Ratio` → `Fill`.

**Percentage / Ratio are of the *whole* split area, not leftover after Length.** `[Min(20), Percentage(50), Percentage(50)]` is **not** reliably `[20, 40, 40]`. Dashboards should be `Length`/`Min`/`Max` + `Fill(weight)`.

Layout results are **LRU-cached** (`layout-cache` feature, default 500). `Layout::init_cache` if we have many unique (constraints, area) pairs.

Preferred 0.30 API: `Layout::vertical([...]).areas(rect)` with array destructure. `.split` still returns `Rc<[Rect]>` for a runtime count. `.spacing(n)` is the gap between children (better than fake `Length(1)` spacers unless we render in the gap). `.spacers` / `.split_with_spacers` if we need the gutter rects.

`u16` implements `Into<Constraint>` as **`Length`**, so `Layout::vertical([1, 2, 3])` is three fixed rows.

**Leftover space:** `Flex::Legacy` dumps it into the last track (old tui-rs behavior). Other flex:

| Flex | Behavior |
| --- | --- |
| `Legacy` | Last child eats leftover |
| `Start` / `End` / `Center` | Pack, leftover as spacer |
| `SpaceBetween` / `SpaceAround` | CSS-like |

macmon uses `Constraint::Fill(2)` + `Fill(1)` for a 2:1 vertical split. That is the simplest responsive pattern.

### 3.1 Nesting

Split vertically, then split a row horizontally. This is the whole dashboard. There is no CSS grid built in. Taffy (flexbox/grid) has unofficial Ratatui experiments; not shipped.

Helpers on `Rect`: `rows()`, `columns()`, `inner(Margin)`, `offset`, `intersection`, `saturating` shrink. **Never index a cell outside `area`** — Buffer panics.

### 3.2 Responsive recipes that actually work

Ratatui will not hide widgets for you. **You** branch on `area.width` / `area.height` (and maybe `Term` size from `frame.area()`).

| Terminal | Strategy |
| --- | --- |
| ≥ 160×48 | Full tile: CPU graph + per-core, MEM, GPU, NET, PROC |
| 120×32 | CPU graph (no per-core), MEM+GPU stacked, PROC |
| 80×24 | CPU sparkline + MEM gauge + PROC (btop still tries; we should drop NET) |
| < 60×18 | Single focused panel + tabs, or refuse with a "resize" message |

Patterns:

1. **Min sizes, then hide.** `if area.height < 8 { skip GPU box }`.
2. **Downgrade, don't clip.** Graph → Sparkline → LineGauge → a number.
3. **Presets** (btop 1–9): named `shown_boxes` lists, not just auto-hide.
4. **`Constraint::Min(n)` + `Fill`** so a box never becomes 1 row of garbage.
5. **Per-core columns** computed like btop: `b_columns = ceil(cores / (height - chrome))`, then pick a column template (wide / medium / spark-only) by remaining width.
6. **Don't use Percentage alone** for chrome (titles, help). Use `Length` for those, `Fill` for graphs.

`Layout::horizontal([...])` / `Layout::vertical([...])` constructors exist and are nicer than `default().direction()`.

## 4. Symbols, markers, the "dot" look

`ratatui-core/src/symbols/`

### 4.1 `Marker` (`symbols/marker.rs`)

Used by Canvas and Chart.

| Variant | Resolution / cell | Glyphs | Font risk | Notes |
| --- | --- | --- | --- | --- |
| `Dot` (default) | 1×1 | `•` | none | Sparse |
| `Block` | 1×1 | `█` | none | Chunky |
| `Bar` | 1×1 | `▄` | none |  |
| **`Braille`** | **2×4** | U+2800–U+28FF | common (iTerm, Ghostty, Kitty, Wez, Apple Terminal OK) | **btop default** |
| `HalfBlock` | 1×2 | `█▄▀` | none | Square-ish pixels |
| `Quadrant` | 2×2 | `▖▗▘▝▄▌▐▛▜▙▟█` | good | Denser than block, no "holes" |
| `Sextant` | 2×3 | Legacy Computing | **spotty** |  |
| `Octant` | 2×4 | Legacy Computing Supplement | **spotty** | Same res as braille, solid pixels |
| `Custom(char)` | 1×1 |  |  |  |

Braille has visible "holes" between dots — that *is* the btop look. Octant is smoother but many fonts show `�`. Default = braille, option = block / octant.

**`HalfBlock` is the only marker with independent fg + bg per cell.** Better for heatmaps and solid fills. Braille / octant / quadrant are one fg color per cell.

Full 256-entry table: `symbols/braille.rs` `BRAILLE`.

### 4.2 Bar / shade / line / border / scrollbar / pixel

- `symbols::bar::{NINE_LEVELS, THREE_LEVELS}` — Sparkline, Gauge unicode
- `symbols::shade::{EMPTY, LIGHT, MEDIUM, DARK, FULL}` — `░▒▓█`
- `symbols::line::{NORMAL, ROUNDED, DOUBLE, THICK, ...}`
- `symbols::border::{PLAIN, ROUNDED, DOUBLE, THICK, QUADRANT_INSIDE, QUADRANT_OUTSIDE}`
- `symbols::scrollbar`
- `symbols::pixel::{QUADRANTS, SEXTANTS, OCTANTS}` — Canvas grids

### 4.3 How btop / tui-bar-graph pack a column

Not Canvas. They treat each **cell column** as two horizontal samples (left + right braille columns) and four vertical levels.

```
level 4  ⢸ ⣿
level 3  ⢰ ⣾
level 2  ⢠ ⣼
level 1  ⢀ ⣸
level 0  (space)
         left right
```

`tui-bar-graph` `BRAILLE_PATTERNS[left][right]` **is the same table as btop**. That crate is the shortest path to the aesthetic.

`BarStyle`: `Braille` (default) | `Solid` | `Quadrant` | `Octant`.
`ColorMode`: `Solid` (color by value) | `VerticalGradient` (color by height — closer to btop's multi-row graphs).

For 1-row graphs btop colors **by the max of the two samples** (`Theme::g(color_gradient).at(clamp(max(last, data_value)))`). `ColorMode::Solid` is that. For tall graphs btop colors **by row** (top of the graph = hot color). `VerticalGradient` is that.

## 5. Style and color

`Style { fg, bg, underline_color, add_modifier, sub_modifier }`.

`Color`: named ANSI 16, `Indexed(u8)` (256), `Rgb(r,g,b)`, `Reset`.

`Modifier`: `BOLD`, `DIM`, `ITALIC`, `UNDERLINED`, `SLOW_BLINK`, `RAPID_BLINK`, `REVERSED`, `HIDDEN`, `CROSSED_OUT`.

`Stylize` trait: `"hi".red().on_black().bold()`.

Palettes shipped in core: `style::palette::tailwind`, `style::palette::material`.

**Truecolor:** target **Ghostty / iTerm2 / Kitty / WezTerm / Alacritty**. **Apple Terminal.app did not do 24-bit before macOS 26** — Ratatui docs warn Crossterm `Color::Rgb` can look glitched there ([issue 475](https://github.com/ratatui/ratatui/issues/475)). Terminal.app gained 24-bit colour with macOS 26; it reports its build in `TERM_PROGRAM_VERSION` (macOS 15 ships 455, macOS 27 ships 488), so siltop treats Apple_Terminal ≥ 460 as truecolor. Detect with `termprofile` or `COLORTERM=truecolor` and fall back to `Indexed` / named 16. btop's `truecolor = false` path quantizes 24-bit → 6×6×6.

**What siltop does (WS-12, `crates/siltop/src/term_caps.rs` + `siltop-ui/src/theme/depth.rs`):**

- Depth order: `SILTOP_COLORS` override → `COLORTERM` truecolor/24bit → known `TERM_PROGRAM` (iTerm.app, WezTerm, ghostty, vscode, Hyper, WarpTerminal, Tabby, rio, zed; Apple_Terminal by build) → `TERM` (`*-direct`, kitty, ghostty, alacritty, foot → 24-bit; `*256*` → 256; `xterm`, `linux`, `screen`, `vt100`… → 16) → 256.
- Themes stay authored in RGB and every blend happens in RGB. Lowering is one pass over the finished `Buffer` (`Theme::finish`, last call in `render_app`), skipped for truecolor. 256: tmux's `colour_find_rgb` (nearest cube cell vs nearest grey, squared RGB distance). 16: hue family + brightness, not RGB distance — xterm's pure primaries make RGB-nearest pick `DarkGray` for pastel green. Selection fills become `REVERSED` at 16 colours and under `NO_COLOR`.
- `NO_COLOR`: no colour at all; `dim`-family ink becomes `Modifier::DIM`, alarms `BOLD`, selection `REVERSED`.
- Background: OSC 11 (`ESC ]11;? ESC \`) then DA1 (`ESC [ c`) on `/dev/tty` in raw mode with `VMIN=0, VTIME=1`, before crossterm reads input. DA1 is answered by virtually every terminal and after the OSC 11 reply, so its arrival ends the wait early; the 100 ms budget only covers terminals that answer nothing. Replies come as `rgb:R/G/B` with 1–4 hex digits per channel (scale by `2^(4n)-1`), `rgba:`, or rarely `#rrggbb`; BEL or ST terminated. A late OSC reply read by crossterm would decode as Alt+`]` followed by literal keys, which is why the probe is skipped over ssh.

Feature `palette` adds `Color::from_hsl` / `from_hsluv`. Feature `underline-color` is Crossterm-only.

**Do not blink.** It looks cheap and costs redraws.

Theme shape we should copy from btop (`btop_theme`): named roles (`cpu_box`, `mem_box`, `proc_box`, `main_bg`, `main_fg`, `inactive_fg`, `hi_fg`, `title`, `gradient_cpu[0..=100]`, …) not raw hex in widgets.

Useful crates: `colorgrad` (ramps), `coolor`, `color-to-tui`, `opaline` (token themes), `ggsci-ratatui`.

## 6. Terminal, backends, event loop

### 6.1 Lifecycle

```rust
let mut terminal = ratatui::init(); // raw mode + alt screen + panic hook
let result = run(&mut terminal);
ratatui::restore();
```

Or manual: `enable_raw_mode`, `EnterAlternateScreen`, `CrosstermBackend::new(stdout)`, `Terminal::new`, on exit `LeaveAlternateScreen` + `disable_raw_mode`. **Always restore on panic.**

`Viewport`: `Fullscreen` (default), `Inline(n)` (draw n rows in-place, keep scrollback), `Fixed(rect)`. Fullscreen for a monitor.

Double-buffer: `Terminal` keeps previous `Buffer`, diffs, writes only changes.

### 6.2 Events (crossterm)

`event::poll(timeout)` + `event::read()`, or `EventStream` under tokio.

We care about: `Key`, `Mouse` (need `EnableMouseCapture`), `Resize`.

`KeyEventKind::Press` only — ignore `Repeat`/`Release` on macOS or we double-fire.

btop: every highlighted key is clickable; wheel scrolls the process list. That is the UX bar.

### 6.3 Loop shapes

**Bad:** `loop { collect(); draw(); sleep(16ms); }` — burns CPU, redraws identical frames.

**Good (macmon):**

- Input thread: `poll` ~250 ms, send keys.
- Sampler thread: IOReport sleep *is* the interval; send `Metrics`.
- UI thread: `recv` → update model → `draw` once.

**Also good (sync, simpler):**

```
poll(min(next_sample, 250ms))
if key → handle
if resize → layout dirty
if sample due → collect cheap metrics; maybe skip processes this tick
if dirty → draw
```

Adaptive interval: if our own CPU (via `mach_task_basic_info` or sampling ourselves) is high, back off. btop default `update_ms = 2000`, min 100. macmon default 1000, TUI clamps.

**Skip identical frames:** hash or `PartialEq` the last drawn snapshot vs current. Terminal diff already helps; skipping `draw()` entirely saves widget construction.

### 6.4 Mouse

Enable, map clicks to `Rect`s. Keep a `Vec<(Rect, Action)>` rebuilt during render (btop `Input::mouse_mappings`). Ratatui does not hit-test for you.

### 6.5 Async

Not required. IOReport wants a dedicated thread that is allowed to `sleep`. Tokio is extra surface. Start sync + 2 threads.

## 7. Performance (Ratatui-side)

- Constructing widgets is cheap compared to syscalls. The cost is **collection**, then **wide tables**.
- `Buffer` is `width * height` cells. 200×60 is nothing. Don't allocate a new `String` per cell if you can write `&str` / `Span`.
- Braille graphs: precompute the 5×5 table, push into a ring buffer of `u8` levels, emit chars. Don't go through Canvas (float world-coords + painter) for a simple history.
- `tachyonfx` / shimmer / rain: fun, not free. Default **off**.
- `ratatui-image` / sixel / kitty graphics: GPU-ish and terminal-specific. Not for a meter UI.
- `ratatui-wgpu`: the opposite of "least GPU usage".

## 8. Third-party widgets that matter

From <https://ratatui.rs/showcase/third-party-widgets/> and awesome-ratatui.

### Use

| Crate | Why |
| --- | --- |
| **`tui-bar-graph`** | Braille/octant/quadrant history + colorgrad. Closest crate to btop graphs. |
| `tui-equalizer` | Multi-band meters (another per-core look). |
| `tui-tree-widget` | Process tree. |
| `tui-popup` | Help / options / confirm-kill overlay (with `Clear`). |
| `tui-scrollview` | Sensor pages that overflow. |
| `tui-prompts` / `tui-input` | Process filter, jump-to-pid. |
| `tui-big-text` | Optional splash / huge % on a "focus" preset. Easy to overuse. |
| `colorgrad` | Gradients. |
| `throbber-widgets-tui` | Only if a sample is in-flight at startup. |

### Maybe

| Crate | Why / why not |
| --- | --- |
| `tui-logger` | Debug overlay, not a user feature. |
| `tachyonfx` | Panel fade on preset switch. Off by default. |
| `tui-menu` | Options menu. We can also do a custom list like btop. |
| `tui-piechart` | RAM composition. Cute, low information density. |
| `malevich` | Serious plotting. Overkill. |
| `rat-widget` / `rat-salsa` | Full framework. We don't need it yet. |
| `opaline` | If we want user theme files early. |

### Avoid for this app

`ratatui-image`, `ratatui-wgpu`, `bevy_ratatui`, `tui-term`, `tui-nodes`, `tui-globe`, `tui-rain`.

## 9. Application patterns

Book: <https://ratatui.rs/concepts/application-patterns/>

**TEA (Elm):** `Model` + `Message` + `update` + `view`. Clean for keybindings. `update` may return another `Message` (state machine).

**Component:** each panel is a struct with `handle(msg)` + `render(area, buf)`. Better once CPU/MEM/PROC diverge.

macmon is a single `render` with helpers (`render_freq_block`, `render_cores`). Fine at their size. We will outgrow it when processes + disks + net land.

Recommended for us: TEA messages at the app edge, component structs for panels, a `LayoutPlan` computed from term size + config (which boxes, which density).

## 10. Examples worth reading (upstream)

Under `ratatui/examples` and `ratatui-widgets` examples:

| Example | Steal |
| --- | --- |
| `sparkline` | History widget API |
| `gauge` | Gauge + LineGauge |
| `chart` | Axes — then decide not to use them everywhere |
| `barchart` / `weather` | Categorical bars |
| `table` | Process-table patterns |
| `list` | Selection |
| `scrollbar` | Pairing with table |
| `canvas` | Custom painter |
| `flex` / `constraints` / `constraint-explorer` | Layout intuition |
| `collapsed-borders` / `shadow` | Grid chrome |
| `demo2` | Closest official “pretty dashboard” |
| `popup` / `clear` | Overlays |
| `user-input` | Filter box |
| `mouse-drawing` / custom widget | Hit testing |
| `async` | EventStream if we ever tokio |
| `colors-rgb` / `colors-256` | Theme preview |

## 11. What Ratatui will *not* do for us

- Braille history with btop's exact two-sample packing — use `tui-bar-graph` or a 40-line custom widget.
- Responsive hide/show — our `LayoutPlan`.
- Mouse hit-testing — our map.
- Themes as files — our code.
- 60 fps — we don't want it.

## 12. Quick mapping: monitor panel → widget

| Panel | Primary | Fallback when small |
| --- | --- | --- |
| CPU history | `tui-bar-graph` Braille + turbo/magma | Sparkline → one `%` |
| Per-core | LineGauge or 1-row Sparkline per core | compact `##` hashes, then hide |
| GPU / ANE / power | same as CPU | number + temp |
| RAM composition | stacked custom bars or Gauge + legend | one Gauge |
| Disks / NICs | Table + tiny Sparkline in a cell | Table only |
| Processes | Table + Scrollbar (+ tree widget) | Table, no graphs-in-rows |
| Fans / temps | LineGauge + number | number |
| Help / options | Clear + Block + Paragraph / List |  |
| Focus / "hero" % | tui-big-text **or** a huge Gauge |  |
