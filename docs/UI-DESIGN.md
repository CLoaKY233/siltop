# siltop UI design

The design contract for this app. If code and this file disagree, one of them is wrong — fix it
here first, then the code.

## 1. Stance

A minimalist Apple Silicon monitor that stays quiet until the machine isn't. One hero story per
surface, one accent family per panel, no decoration without information.

| | btop | macmon | siltop |
| --- | --- | --- | --- |
| First 3 seconds | cinematic density | small silicon readout | **one labeled hero + quiet chrome** |
| CPU truth on AS | Mach ticks only ("100% busy" at 600 MHz looks like 4 GHz) | scaled + active, labeled | **scaled headline, `busy` rides along dim** |
| Act on processes | excellent | none | **filter → detail popup → kill without leaving it** |
| Small pane | ugly squeeze | native | **Glance surface, auto-selected** |
| Background | paints its own | paints its own | **terminal decides** (transparency-safe) |
| Renderer | custom C++ | ratatui sparklines | **ratatui braille history** |

We win by refusing btop's density and macmon's passivity at the same time.

## 2. Information architecture

Two surfaces, nothing else:

- **Work** (wide default): health rail on the left (`cpu | gpu` heroes over `mem | sens`, net
  opt-in), process table owns ~55% of width on the right. Enter expands any card into a full-body
  cell grid.
- **Glance** (small terminal, or `v` to switch): cpu hero absorbs all slack, compact strips pinned to the
  bottom border.

Panel order is fixed everywhere: `cpu · gpu · mem · net · disk · sens · proc`. Tab cycles it,
Enter expands it, Esc returns home.

**No top bar.** Graphs own every row but the last; row 0 belongs to the panels. The chrome is
one **footer** row at the bottom of both surfaces: on the left the quiet `siltop ≡` menu
button (just `≡` below 50 columns) and the contextual key chips (§5), in the middle the verdict
(`All normal`, `Busy: Safari using 3.1 cores`; a toast takes the slot while it lasts), and on
the right one status segment: power, system watts and sampling state
(`79% ▼ 8h05m  6.8W  ● 500 ms`, or `❚❚ paused` while frozen). The power token reads `AC 80%` /
`AC 62% ▲` on a charger and `74% ▼ 3h12m` on battery (warn ink at ≤ 20%); a desktop has no
battery, so its power token is absent, never `0%`. Beside a system-watts token the battery
token drops its drain (§5). Status tokens drop whole through `Title::fit`, least important
first, and yield to the essential chips, never the other way round; the sampling state goes
last. `widgets/footer.rs::layout` places everything once; painting, clicks and hover all read it.

**Main menu** (`m`, or a click on `siltop ≡`): a small centred card,
`Timeline H · Settings s · Themes t · Keys & help ? · About a · Quit ctrl-c`
(`widgets/menu.rs`). Chip identity (chip name, core counts, GPU cores, RAM) has one home per
screen, the expanded CPU view's meta column (and settings → about), never the chrome.

**Settings** is a screen, not an overlay. While it is open the dashboard is not painted at
all (sampling continues): title bar `siltop › settings … esc close`, categories on the left
(`general · appearance · panels · processes · alerts · keys · about`), rows on the right as
`label  value`, the focused row's plain-English description above a key-hint line. Rows are
data — `widgets/settings/registry.rs` holds one `SettingDef { id, category, label,
description, kind: Toggle | Cycle(options) | Number{min,max,step}, get, set }` per option, and
paint, clicks, keys and the `/` search all walk that one table. The binary applies a change by
diffing the old and new `Config` (`App::apply_config`), so `T`, the settings page and the
config file cannot disagree. Appearance previews every theme (four resource accents plus a
braille ribbon in its cpu ramp); keys lists the live keymap; about shows version, config path
and history location. Below 56 columns the category column folds into the title crumb.

A cell or graph is drawn only when the snapshot has a real value. Missing Option metrics
(`watts`, live MHz, ANE, GPU die temp) stay off the board. Zone °C (`e`/`p`/`s`) are first-class
on compact SENS and on expanded CPU/SENS, labeled as zones, never as per-core die temps.
History rings never receive `0.0` for a missing sample. Per-process GPU (`gpu%`) is the busy
share from each AGX user client's `AppUsage` → `accumulatedGPUTime` in the `IOAccelerator`
registry (no sudo; see research/14), refreshed every 2 s (6 s while the GPU idles), and shown
as the process table's `gpu%` column when it has data. Disk is off by default
(`[panels] disk = false`); its compact/expanded graphs are I/O bytes, not the near-static
used-ratio. Tab / ← → while expanded hops the related family (`cpu ↔ gpu ↔ sens`,
`net ↔ disk`, `mem ↔ proc`).

| Panel | Compact | Expanded | Graph |
| --- | --- | --- | --- |
| cpu | title `%` + busy (if scaled≠active) + °C + thermal; body is the load graph | cluster load graphs (or overall if no clusters), zone °C if live else package, per-core `%` in the meta column | load auto (10% floor); zone/package °C band |
| gpu | title `%` + °C + watts if present; body is the util graph | util + die °C if live; related CPU/SENS hops | util auto (10% floor); temp band |
| mem | title used/total + pressure; body is the used graph when inner ≥ 2 | used graph, composition bars, hop to proc | used % fixed 0–100 |
| net | title iface ↓ ↑; body is the rx graph | down / up graphs; hop to disk | rx + tx bits, auto scale |
| disk | title volume used/total; body is the I/O graph when inner ≥ 2 (bar if shorter) | read/write I/O graphs + volumes; hop to net | I/O bytes, auto scale |
| sens | title zone °C + fan RPM; spark when inner ≥ 2 (Glance stays numbers) | zone + package + GPU °C graphs, per-fan RPM, related CPU/GPU hops | fans RPM auto; temps band |
| proc | pid name cpu% mem (+ threads) | same table + dossier (identity, live, command, cpu spark, family) | per-pid cpu spark in dossier |

## 3. Component hierarchy

```
Surface (Work | Glance)
└── Panel (bordered, titled, [↗] / [×] corner mark, focus ring in the panel accent)
    ├── compact view   — title tokens + graph (no spec subline)
    └── expanded view  — `grid::pack` of graph-first cells
        ├── usage / zone graphs : title carries the live number
        ├── related hops        : hoppable (`→`), left column when wide
        └── cluster / list      : per-core `%` bars, volumes
Footer (≡ menu button · contextual chips · verdict or toast · power/status)   Menu card (from ≡)
Overlays (help, kill confirm, signal picker, process detail)
Settings screen and Timeline (replace the dashboard)
```

Primitives live in `crates/siltop-ui/src/chrome.rs`: `panel_block`, `panel_title`,
`push_token`, `push_kv`, `cell_titled`, `Graph { .. }` + `render_scaled_graph`, `render_fill_bar`.
Expanded geometry lives in `widgets/grid.rs`. Nothing draws raw borders outside chrome.

## 4. Layout algebra

- `plan(area, surface, flags) -> LayoutPlan` is pure geometry; widgets never self-measure.
- **Work gate:** 60 cols × 16 rows (24 proc + 36 metrics); below it, Glance — never squeeze.
- **Process share:** default 55%, clamp 35..72 (`PROC_RATIO_*` in core), drag-resizable.
- **Degrade ladder:** `Full → Tight → Minimal` is still computed on the Work left
  rail and Glance body, but compact widgets no longer hide graphs or drop spec
  lines from it — numbers live in titles, bodies are graphs whenever inner
  height ≥ 2. Expanded views reflow via `grid::pack` plus `split_meta`. Wide
  expand (`inner.width ≥ 100`) parks hops, identity, and per-core `%` in a
  **26-col left column** (hops eat leftover column height); graphs keep the
  remaining full height. Narrow expand uses a bottom strip. Leftover is shared
  across `take_leftover` bands so usage and heat both grow; when heat is
  absent the usage band fills the pane.
  Expanded cells are not required to be odd height.
- **Slack policy:** leftover vertical space goes to the current hero (Work: cpu/gpu row;
  Glance: the cpu panel unions the fill row directly beneath it).
- Rows use odd heights (3/5/7) so rounded borders stay symmetric top and bottom.

## 5. Spacing law

Positive space — measured in terminal columns/rows:

1. Border → content: exactly **1 space**. Title prefix is `" label"` inside its own span.
2. Between tokens: exactly **2 spaces**, emitted only by `push_token`/`push_kv` (the helper skips
   the separator when the previous span already ends in whitespace — separators never stack).
3. Key/value pairs render as `key␣␣value`; keys are `theme.dim()`, values carry the data style.
4. Numbers are right-aligned in fixed-width spans; names truncate to width with `…`. In a
   title the pad belongs to the fitter (`push_fixed`), never to a `format!("{:>4}")`: padding
   is all-or-nothing, so a tight title drops every pad at once (`gpu  2%  466MHz`, not
   `gpu  2%   466MHz`). Units are glued like the digits they follow: `12W`, `589MHz`,
   `1714rpm`, `58°` — one spelling in titles, cells and the footer.
   Cell titles (`chrome::cell_titled`) end in one space before the rule resumes, like panel
   titles: `╭ memory  12.8G/24.0G ──`, never `12.8G/24.0G──`. Labels are lowercase everywhere,
   including hardware names (`fan 1`, not `Fan 1`).
   One fact, one home, for power too: beside a system-watts token the battery token drops its
   drain (`79% ▼ 8h18m  6.9W`, not `79% ▼ 6.9W 8h18m  6.9W`); a charging flow is a different
   fact and stays. `power::Flow` says which home the flow has.
5. Bars anchor to the **bottom row** of their region; text anchors top-left.
6. Corner mark is bracketed so it reads as a button and sits one cell in from the right
   corner of the title row (3 columns, `LayoutPlan::corner_hit` is its hit rect): `[↗]` home,
   `[×]` expanded. Frames narrower than 5 columns get no mark.

Negative space — what we deliberately leave empty:

- Idle graphs stay empty (no heartbeat dots, no fake axis ink).
- No axis gutter and no hint over plot cells: the plot owns every inner cell. Auto scales
  print their ceiling (or band) in the **top border**, left of the corner mark (`10%`,
  `40–65°`, `2MB/s`), rounded to 1-2-5 in the printed unit and computed over the time on
  screen, so it matches what is drawn. Fixed percent has no label (the top is 100%). The
  **bottom border** carries time: `−5m ┴ … ┴ now`. Mirrored net/disk graphs share one
  ceiling for both halves.
- Thermal `nominal` prints nothing. Pressure prints a dot, colored only when it leaves nominal.
- Footer: after the `siltop ≡` menu button, `? help` leads and `q quit` ends every
  context except the kill confirmation (`y yes  n cancel`); contextual verbs appear only in
  context (`f filter  x kill` over the process table, `v work` on Glance, `s settings` on home
  screens and first to drop, `tab related  esc home` expanded, the dossier's
  `x quit  K force quit  i interrupt  esc close`, `p resume` while frozen). One
  `Chip { key, verb, action, style }` list (`chips.rs`) paints the footer and the dossier
  action row **and** answers their clicks, so a chip is clickable exactly where it is drawn.
  When a row is too narrow, non-essential chips drop from the right; help and quit never drop.
- Status messages are toasts: they take the verdict's slot in the middle of the footer and
  expire after 4 s (an interval change after 1.5 s); they never hide the chips. The verdict
  shows only when it fits whole; it is never truncated.
- Footer spacing: 1 space before the menu button, 2 columns between neighbouring groups
  (menu button, chips, verdict, status), 1 after the status. No background is ever painted,
  so transparent terminals show through exactly as they do on the dashboard.
- Settings screen: categories in a 13-col column ruled off by a `border` `│`; labels in an
  18-col column; values after `‹ ›` when focused (cycles) or `● on` / `○ off` (toggles);
  every text cell is `fg`, `title`, `hi` or `dim` so it meets the 4.5:1 contrast gate.
  Extras under the rows (theme preview, key list, about) start in the label column, and their
  second column lines up with the value text above (`extra_key`).
- Process table: numeric headers are right-aligned over their numbers; the sort column carries
  an arrow that points the way the rows run (`cpu%↓`, `pid↑`); an idle `0.0` is dim. When the
  pane narrows, pid drops below 44 inner columns and mem below 32, so the name keeps ≥12 cells
  down to the 60-column Work minimum; names that do not fit end in `…`. A scrolled table says
  so in its title (`proc  834  ↑ 40`).

## 6. Typography

A TUI cannot choose fonts — the terminal emulator does. What we own:

**Glyph inventory** (all must render in the user's font):

| Glyphs | Use |
| --- | --- |
| `U+2800..U+28FF` braille | history graphs, two samples per cell, bottom-fill |
| `━ ─` | fill bars (filled/track), same row height as text |
| `╭ ╮ ╰ ╯ ─ │` | rounded panel/cell borders |
| `↓ ↑ ● ° … — ▌` | rates, pressure, degrees, pending state, empty value, cursor |

**Recommended fonts** (README-worthy): JetBrains Mono, Berkeley Mono, SF Mono, Iosevka — any
mono with full braille coverage and slab-ish dots. Fonts whose braille glyphs are thin gaps
(e.g. some default Windows consoles) degrade graph readability; the `tty` fallback idea remains
a non-goal until someone ships us a screenshot that needs it.

**Type ramp** (weight, not size — terminals give us one size):

- Hero numbers: `title` color + BOLD modifier, one per view max.
- Titles: `dim` label + `title` values.
- Identity in the expand meta column: all `dim`.
- Data accents: one cool hue per resource (cpu green, gpu violet, mem sky, net periwinkle,
  disk lavender, fan steel, temp blue); amber and coral mean state only (§7).

## 7. Color system

**Warm = alarm.** Every resource owns a cool hue; amber and coral only ever mean a state
(thermal, pressure, kill). A calm machine is a cool-toned screen, so any heat on it is news.

**The terminal owns the background.** Nothing paints a background except the selected row
and the row armed for kill, and on a glass surface (below) nothing paints one at all. `fg` and `title` default to `Color::Reset`, so body text and
hero numbers follow the terminal's own foreground on any profile; hero numbers add BOLD.

Code lives in `crates/siltop-ui/src/theme/`: `tokens.rs` (the `Theme`, `Ramp`, `Heat`
and `Icons` types), `palettes.rs` (the built-ins), `color.rs` (sRGB ↔ linear ↔ OKLab/OKLCH,
perceptual mix, WCAG contrast, the lightness clamp), `adaptive.rs` (the adaptive theme,
`Surface`/glass and token hardening), `depth.rs` (colour-depth lowering) and `mod.rs` (style
helpers including `graph_ink`, lookup, auto choice and the contrast tests). Widgets ask for roles
(`theme.dim()`, `theme.cpu`, `theme.temp(c)`), never hex. The bin detects the terminal once
(`crates/siltop/src/term_caps.rs`), keeps the `Theme` in `App` and hands `&Theme` to
`AppView`; `render_app` never builds a theme.

### Tokens

| token | Billabong (dark default) | Paper (light) | used for |
| --- | --- | --- | --- |
| `fg` | `Reset` (ref `#e6e8eb`) | `Reset` (ref `#1f2328`) | body text |
| `title` | `Reset` + BOLD | `#0b0d10` + BOLD | hero numbers, keys |
| `dim` | `#959ca5` | `#56616c` | labels, axis hints, secondary text |
| `muted` | `#6b737c` | `#7a848e` | meta, the idle selection mark (3:1, not body text) |
| `faint` | `#3a3f46` | `#d2d7dd` | bar tracks, the `sampling…` placeholder, the veil behind overlays |
| `border` | `#40454d` | `#c2c8cf` | resting frames |
| `hi` | `#5fd497` | `#107542` | search hits (+ underline), selection mark, proc focus ring |
| `sel_bg` / `sel_idle` | `#243039` / `#1c232a` | `#dbe7f5` / `#e9eef5` | selected row, focused / not |
| `kill_bg` | `#5c2e2e` | `#ffd8d3` | row armed for kill (text forced to `title`) |
| `ok` / `warn` / `crit` | `#5fd497` / `#ffb347` / `#ff5f6d` | `#107542` / `#a14f00` / `#cf222e` | states only |

Series are three-stop ramps `{foot, body, crest}`; `body` is the ink for text and fills:

| ramp | Billabong foot · body · crest | Paper foot · body · crest |
| --- | --- | --- |
| `cpu` | `#23794f` · `#5fd497` · `#c4f7d9` | `#609a7d` · `#107542` · `#07472a` |
| `gpu` | `#675dba` · `#ae8cf5` · `#e2d9ff` | `#9783c0` · `#6d3fd9` · `#3f1f8f` |
| `mem` | `#33708e` · `#5cbfe8` · `#c8eeff` | `#6692a5` · `#0a6f9c` · `#064a69` |
| `net` (down) / `net_up` | `#78a2f7` / `#5a80d8` bodies | `#2f55d4` / `#6079d9` bodies |
| `disk` (read) / `disk_write` | `#bba9ea` / `#9585c9` bodies | `#7a5cb8` / `#8a70c2` bodies |
| `fan` | `#626d7d` · `#a3b1c2` · `#e1e7ee` | `#878d92` · `#57606a` · `#30363d` |
| `temp` | `#486c9f` · `#7fb2ff` · `#d2e4ff` | `#798eb0` · `#2560c8` · `#1a4591` |

`heat` has five stops anchored to degrees (45°, 70°, 80°, 88°, 95°+), blended in RGB between
them. Billabong: `#7fb2ff → #9fd0c8 → #e9d27c → #ffb347 → #ff5f6d`. Every °C on screen goes
through `theme.temp(c)`, so one reading has one colour in every title.

### Built-in themes

`Theme::names()` lists them and `Theme::by_name` finds them (case, spaces and underscores
forgiven; `catppuccin`/`mocha`, `tokyonight`/`tokyo`, `mono`, `ansi`, `kanagawa-dragon`/`dragon`
and `gruvbox` are aliases). The source of truth is `palettes.rs`; the hex tables above are
checked against it by hand, so update both in one commit.

| name | for | notes |
| --- | --- | --- |
| `billabong` | dark terminals (default) | the palette above |
| `paper` | light terminals | darker bodies so series colours pass as text |
| `okabe` | colour-blind users | Okabe-Ito: sky cpu, reddish-purple gpu, bluish-green mem, orange net; yellow warn, vermillion crit |
| `monotreme` | minimalists, `NO_COLOR` | one grey ink `#5a5a5a · #d0d0d0 · #ffffff`; only warn and crit have colour |
| `terminal` | any scheme, 16 colours | ANSI names and `Reset` only, REVERSED selection, no gradients |
| `catppuccin-mocha` | Catppuccin users | canonical Mocha hues; foot/crest derived against base |
| `tokyo-night` | Tokyo Night users | disk borrows teal so it never twins the purples; `muted` lifted to `#737aa2` |
| `nord` | Nord users | lifted for 4.5:1 on polar night: crit `#f08c8c`, gpu `#c09bb8`, net `#9ab0e0`, disk `#7fc4b4`, dim `#aab3c3` |
| `kanagawa` | Kanagawa Dragon users | Dragon's own inks: sage cpu, violet gpu, aqua mem, spring-blue net, a muted gold disk kept clear of the surimi-orange warning |
| `gruvbox-dark` | Gruvbox users | aqua-green cpu, purple gpu, blue mem; yellow, orange and red stay the states |

### Rules

1. **Focus ring in the panel's accent.** A focused (or expanded) panel's border, its label
   (BOLD) and its `↗`/`×` mark take the panel accent (`cpu`, `gpu`, `mem`, `net`, `disk`
   bodies; `temp` for sens; `hi` for proc). At rest the border is `border` and the mark is
   `dim`, so chrome is quieter than data.
2. **Neutral selection.** The selected row is `sel_bg` + BOLD with an accent `▌` in front;
   cells keep their own ink. When the table is not focused the fill drops to `sel_idle` and
   the mark to `muted`. Search hits are `hi` + underline + BOLD. Red is reserved for the row
   armed for kill and for critical states.
3. **Graph ink.** Braille mountains shade by row from `body` (top row) to `foot` (bottom row),
   so an idle graph keeps its hue instead of fading to grey. `crest` is reserved for the
   per-column ridge (WS-15b). Thermal Fair / Serious still repaint load graphs `warn` / `crit`
   until WS-15b moves thermal state to the border, badge and crest.
4. **One pass for depth.** Themes are authored in RGB and every blend is done in RGB; the
   finished frame is lowered once in `Theme::finish` (last call in `render_app`). Truecolor
   skips the pass entirely.

### Adaptive theme (the `auto` default)

At startup, before raw input, siltop sends one batched query to `/dev/tty`: OSC 10 (fg),
OSC 11 (bg), OSC 4;0…15 (the ANSI palette), then DA1. It reads replies until DA1 arrives or
120 ms pass, and restores the tty with `TCSAFLUSH` so unread bytes are dropped. A reply that
straggles in later is eaten by the input thread (`event::OscSwallow`: Alt+`]` … Alt+`\` or
Ctrl+G, armed for the first 3 s only, capped at 64 keys).

When fg, bg and the six chromatic hues answered, `Theme::from_terminal` builds the theme
from the user's palette:

- **Role → hue family.** Each panel role aims at an OKLCH hue: cpu green 145° (±35),
  gpu violet/magenta 315° (±45), mem teal 190° (±25), net blue 250° (±30), disk gold 92°
  (±22), heat orange 55° (±18); warn yellow 88° (±25) and crit red 25° (±25) are picked the
  same way. The role takes the palette colour (normal or bright, the variant that reads
  better on this background preferred by 8°) closest to its target among those with chroma
  ≥ 0.06 inside the window, lifted to chroma 0.10 if it is below; with none, it is
  synthesised at the target hue from the palette's mean lightness and chroma (0.10–0.15).
  fan = fg tinted 15 % toward blue, ok = hi = the cpu accent. The adaptive theme is the one
  place a resource is warm: its heat panel and disk take the warm families by request.
- **Greys from fg → bg in OKLab.** dim 30 %, muted 50 %, faint 78 %, border 72 % (45 % on
  glass). Selection fill = bg 20 % toward hi.
- **Ramps in OKLCH.** body = the accent (the graph's top row); foot = 0.19 L darker (toward
  bg), 6° cooler, 90 % chroma; crest = 0.06 L lighter at full chroma. Heat's ramp runs
  yellow (foot, +18°) → orange (body) → red (crest, −18°); its five °C stops run gold → amber
  → orange → red-orange → crit. Upload / write = a deeper, 10° turned shade.
- **Separation.** Accents are placed cpu, gpu, mem, net, heat, disk; one within ΔE OK 0.08
  of an earlier accent, below chroma 0.05, outside its window, or (cool roles only) within
  ΔE 0.08 or 35° of warn/crit's hue tries hue turns inside its window (±10° … ±60°),
  lightness steps and a chroma boost until it clears them. This is what stops Gruvbox's
  yellow-green from reading as a warning.
- **Hardening.** Every RGB token is walked in OKLCH lightness (hue kept) until it meets its
  floor against every effective background: text 4.5:1, graph/muted 3:1.

When the query fails (Terminal.app without OSC 4, tmux without passthrough, ssh, 16-colour
terminals) `auto` falls back to Billabong / Paper by background luminance, as before.

### Glass surfaces (translucent terminals)

Ghostty draws explicit cell backgrounds at full opacity, so on a translucent window every
painted background is a solid slab over the wallpaper. `Surface::Glass(opacity)` is detected
from the running terminal's own config, keyed by `TERM_PROGRAM` / `TERM` / window-id env:
Ghostty `background-opacity` (`~/.config/ghostty/config`, then Application Support
`config` / `config.ghostty`, last wins), Kitty `background_opacity`, Alacritty
`[window] opacity`. `SILTOP_GLASS=auto|on|off|<0..1>` overrides. On glass:

- **No backgrounds, ever.** Selection = the `▌` accent gutter + BOLD + `hi` ink; unfocused =
  BOLD; kill-armed = `crit` + BOLD + UNDERLINED. A buffer test renders every scene under
  every built-in on glass and asserts zero cell backgrounds.
- **Effective backgrounds.** Every token must read against the panel bg itself, the bg
  composited in linear light at `opacity` over a bright blurred wallpaper (`#8a8a8a`, the
  worst case the method designs for) and over black.
- **Frames brighten:** border ≥ 3:1, faint ≥ 2:1 against all three. Dim text keeps 4.5:1.
- **Graph feet keep hue and chroma**: the foot is lifted in lightness to 3:1, never faded
  to grey.

Named built-ins go through the same hardening via `Theme::on_surface`; on an opaque surface
they are untouched.

### Choosing a theme

`theme = "<name|auto>"` in the config file picks the theme; the environment overrides it:

- `SILTOP_THEME=<name|auto>` — wins over the config file; a name wins over everything,
  including `NO_COLOR`.
- `NO_COLOR` (non-empty) with no name — Monotreme, attributes only.
- `auto` (default) — the batched OSC 10/11/4 + DA1 query above (120 ms budget, ends early
  on DA1) builds the adaptive theme; failing that, `COLORFGBG` (7 or 9–15 light, 0–6 or 8
  dark); then dark. A background lighter than CIE L* 50 picks Paper, otherwise Billabong. The query is skipped over ssh (`SSH_TTY` /
  `SSH_CONNECTION`), where a late reply could arrive as keystrokes. Inside tmux the query
  goes to tmux itself, not the outer terminal: siltop does not wrap it in tmux's
  `ESC Ptmux;` passthrough, so the answer depends on whether your tmux replies to OSC 11
  (recent releases do; DA1 still ends the wait early when it does not). If tmux guesses
  wrong, set `SILTOP_THEME`.
- Colour depth: `COLORTERM=truecolor|24bit`, or a known truecolor `TERM_PROGRAM` (iTerm2,
  WezTerm, Ghostty, VS Code, Hyper, Warp, Tabby, Rio, Zed; Apple Terminal from build 460,
  i.e. macOS 26), or a truecolor `TERM` (`*-direct`, kitty, ghostty, alacritty, foot) →
  truecolor. `TERM` with `256` → xterm-256. `xterm`, `linux`, `screen`, `vt100`… → 16 colours,
  and `auto` then picks the Terminal theme. Anything else → 256. `SILTOP_COLORS=
  truecolor|256|16|none` overrides the guess.
- `SILTOP_ICONS=none|unicode|nerd` — `nerd` prefixes panel labels with Nerd Font icons
  (in `dim`, width measured with `unicode-width`, always followed by a space); `none` swaps
  the `▌` selection mark for `>`.

### Colour depth

| depth | what `Theme::finish` does |
| --- | --- |
| truecolor | nothing |
| 256 | every `Rgb` → nearest xterm-256 index (6×6×6 cube vs grey ramp, tmux's method) |
| 16 | every `Rgb` → the named colour of the same hue family (bright when the max channel ≥ `0xc0`; greys by lightness); `sel_bg` / `kill_bg` fills become REVERSED |
| none | all colour → `Reset`; `dim`/`muted`/`faint`/`border` ink → DIM, `warn`/`crit`/`hi` → BOLD, selection → REVERSED |

### Contrast gates (tests in `theme/mod.rs`)

Each palette names its reference backgrounds and the default foreground it assumes for
`Reset`: Billabong, Okabe and Monotreme against `#000`, `#161616`, `#1e1e2e`; Paper against
`#fff`, `#f5f5f5`; Catppuccin against `#1e1e2e`, `#181825`; Tokyo Night against `#1a1b26`,
`#16161e`; Nord against `#2e3440`; Kanagawa against `#181616`, `#141313`, `#0d0c0c`;
Gruvbox Dark against `#282828`, `#1d2021` (`palettes::REFERENCES`).

- Text tokens (`fg`, `title`, `dim`, `hi`, `ok`, `warn`, `crit`, series bodies, heat stops)
  reach 4.5:1; graph bodies (including `net_up`, `disk_write`) and `muted` reach 3:1.
- `fg`, `title` and `dim` reach 4.5:1 on `sel_bg`; bodies and `hi` reach 3:1 on it.
- Warm = alarm: every series body is ΔE76 ≥ 30 from `warn` and `crit`; `warn`/`crit` ≥ 20
  apart; cpu/gpu/mem/net pairwise ≥ 20 (the old mints were 15.5) and no two series twin (< 10).
- Frame test: Work, Glance, every expanded panel, help, settings, the dossier and a
  search + kill-confirm screen are rendered under every palette, and every text cell must
  reach 4.5:1 against every reference background (Paper on `#f5f5f5` included).
- The Terminal theme holds no `Color::Rgb`, and no palette emits one at 256 or 16 colours;
  `NO_COLOR` frames hold no colour at all.
- Adaptive (`theme/adaptive.rs`): 30 real Ghostty palettes (`theme/fixtures/`, light and
  dark) × {opaque, glass 0.75, glass 0.7} meet 4.5:1 text / 3:1 graph against every
  effective background; every panel accent has chroma ≥ 0.05 and a hue inside its role's
  family, heat and its °C stops stay warm (never teal or blue), panel accents stay ΔE OK
  ≥ 0.08 apart and cool ones clear of warn/crit (0.065 for mid-grey panels on 70 % glass,
  where sRGB runs out of chroma near white) and never share a hue with warn/crit. Every built-in on glass 0.7 meets the same floors.

## 8. Rendering strategy

- **Sampler thread** owns the collectors (confinement — no locks, no shared mutable state);
  commands flow down `mpsc` (`Interval/Paused/Quit`). It runs on a fixed-rate schedule
  (`next += interval`; more than a period behind resyncs and counts the skip), so tick cost never
  stretches the period. Paused, it blocks on its command channel and wakes nobody.
- **Sensor thread** (inside the sampler) owns SMC, HID, fans and the thermal state and
  publishes a frame every 2 s; the cheap tick merges the newest frame without waiting, so the
  sensor pass never delays a snapshot (p99 period at 250 ms: 260 ms, was 347 ms). A collector
  that fails keeps its last good value and reports `Health::Stale(age)` in `Snapshot::health`;
  `Snapshot::primed` is false while rates lack a real interval. History takes only fresh,
  primed values; until then rates show `…`.
- **One message channel:** an input thread forwards `crossterm::event::read()` and the sampler
  sends `Result<Snapshot>` into the same `AppMsg` channel. The main loop blocks in
  `recv_timeout(next deadline)`, drains everything queued, and draws **at most once** — and only
  if something changed (a snapshot, a mapped input, a resize, or a deadline such as toast
  expiry). Pointer motion and key releases are dropped in the input thread; mouse capture asks
  for `?1000/?1002/?1006` only, never any-motion `?1003`. Drag and key-repeat redraws are capped
  at 60/s; the first frame waits for the first snapshot (≤500 ms) so the layout does not jump.
- **Draw cadence:** idle, draws per second equal samples per second. The cheap tick defaults to
  **500 ms** (`[` slower, `]` faster through 250 ms "smooth", 500 ms, 1 s, 2 s, 5 s);
  processes 1 s; HID/SMC 2 s. No tweening. Frozen means zero draws and zero sampler ticks:
  `Sampler::set_paused(true)` parks the sensor thread too, so no SMC, HID or IOReport call
  runs while frozen (research/13 §5).
- **Terminal output:** one 64 KiB `BufWriter`; ratatui's mid-draw flushes are held, so each
  frame — DEC 2026 begin/end markers included — reaches the terminal as one write.
- **History:** every series covers the same **15-minute** window (`HISTORY_WINDOW`).
  Ring length is `capacity_for(period)` so a 500 ms cheap clock is 1800 samples and
  the 2 s sensor clock is 450; `[` `]` resize the cheap rings and the
  window does not move. Draw path uses `downsample_shaped`: right half last-value,
  left half peak. Bottom-fill braille with a small-value bias so 2% still lights a dot.
- **Verification:** every layout change gets eyeballed through the ignored
  `visual_dump::dump_all_expanded` test (ASCII frames of every expanded panel), plus pty runs at
  boundary sizes (61×16, 60×24) checking boot/render/quit and a size-sweep test that replans and
  hit-tests hundreds of geometries for panics.
- **Persistence (config v1, WS-10):** `$XDG_CONFIG_HOME/siltop/config.toml` (else
  `~/.config/...`). A change marks the config dirty; the loop saves one second after the last
  change (`SAVE_DEBOUNCE`, a loop deadline, never a timer thread) and on quit, HUP, TERM or INT.
  A save re-reads the file, writes only the settings that changed through `toml_edit` (comments,
  formatting, unknown keys and hand edits made while running all survive), then writes
  `config.toml.tmp` and renames it over the file (through a symlink to its target). A file that
  does not parse is copied to `config.toml.bak`, the session runs on defaults, the status line
  says "config error, using defaults (not saved)" for 4 s, and the file is never written. A
  read-only file or directory is never written ("config is read-only (not saved)", once). The
  dragged split ratio persists. `v` switches the surface for the session only; `surface` in the
  file stays `auto` unless the user sets it.

## 9. Code patterns (the house style)

Workspace lints deny `unwrap`, `expect`, `panic!`, `dbg!`, `process::exit`; production code
returns `Result`. Tests opt back in per module. The UI crate contains **zero `unsafe`** — FFI is
confined to `siltop-metrics` plus the bin's one-shot termios probe (`term_caps.rs`), each
block carrying a SAFETY comment.

Helpers over literals:

```rust
pub fn panel_title(label: &str, theme: &Theme) -> Line<'static> {
    Line::from(vec![Span::styled(format!(" {label}"), theme.dim())])
}

pub fn push_token(spans: &mut Vec<Span<'static>>, text: String, style: Style) {
    if !spans.is_empty()
        && !spans.last().is_some_and(|span| span.content.ends_with(' '))
    {
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(text, style));
}
```

One graph type, eight fields behind a struct — call sites stay flat:

```rust
render_scaled_graph(
    frame,
    inner,
    Graph {
        history: view.cpu_history,
        ramp: theme.cpu,
        theme,
        scale: Scale::Fixed(1.0),
        axis: Axis::Percent,
        ink: GraphInk::Load(view.snapshot.thermal),
    },
);
```

Cells are the only way to draw a boxed section in an expanded view:

```rust
fn cell(frame: &mut Frame, area: Rect, title: &str, theme: &Theme) -> Rect {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Line::from(Span::styled(format!(" {title}"), theme.dim())))
        .border_style(theme.border(false));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}
```

Geometry degrades before content does: `layout::plan` still computes `Degrade`
from rail size, but compact paint no longer consults it. Inner height decides
graph vs spark vs headline.

## 10. Checklist status

- [x] Minimalist, aligned layouts (spacing law enforced by helpers + contract test)
- [x] Positive/negative spacing defined and applied
- [x] Glyph/typography system documented; font recommendations for users
- [x] Clean, comment-light samples; zero unsafe in the UI crate
- [x] btop/macmon deltas stated above
- [x] Empty Option cells (power / clock / zone °C / per-pid GPU) stay off the board
- [x] Battery (IOPS), IOReport residency (real `scaled`, cluster frequencies) and PSTR
      system watts shipped in 1.0
