# 15 — Colour and glass

What we learned building the adaptive theme and glass mode (wave 3, theme lane). The design
rules live in `docs/UI-DESIGN.md` §7; this file is the evidence behind them.

## The owner's terminal

Ghostty, "Kanagawa Dragon" (fg `#c5c9c5`, ANSI green `#8a9a7b` / bright `#87a987`, yellow
`#c4b28a`, red `#c4746e`), background `#141313`, `background-opacity = 0.75`,
`background-blur = 30`, over the macOS Sonoma wallpaper. The harness reproduces it with
`SHOT_PRESET=user` (`scratchpad/shot.py`).

## Ghostty draws explicit cell backgrounds opaque

With `background-opacity < 1`, only cells whose background is the *default* are
translucent. A cell given any explicit background (`bg(Color::Rgb(..))`, and also
`REVERSED`, which swaps in the fg as an explicit bg) is drawn at full opacity. So the old
selection fill (`sel_bg #243039`) became a solid dark bar across a wallpaper-coloured
window: the "heavy slab". The only way to look right on glass is to paint no backgrounds
at all, so glass selection is gutter + BOLD + accent ink.

## What the wallpaper does to contrast

The window composites `bg` at `opacity` over the blurred wallpaper in linear light
(Ghostty's default blending is linear). With blur 30 a busy wallpaper averages out; we
design for a bright mid-grey `#8a8a8a` (luminance ≈ 0.25) as the worst case, plus black.

Effective background luminance for `#141313` over `#8a8a8a`, and what the old Billabong
tokens measured against it:

| opacity | eff. luminance | old border `#40454d` | old dim `#959ca5` | old muted `#6b737c` | Kanagawa fg |
| --- | --- | --- | --- | --- | --- |
| 1.0 | 0.007 | 1.92:1 | 6.69:1 | 3.86:1 | 11.07:1 |
| 0.75 | 0.068 | **1.09:1** (darker than the glass: inverts) | **3.20:1** | **1.84:1** | 5.29:1 |
| 0.70 | 0.081 | 1.20:1 | 2.89:1 | 1.67:1 | 4.79:1 |

So on glass the frame literally vanished, dim labels and axis hints fell below 4.5:1 and
muted meta below 3:1, exactly what the owner reported. The terminal's own fg still holds
4.5:1 at 0.7; that is the budget everything else must fit in, which is why glass dims sit
close to fg in lightness and differ from it mostly in tint.

Physically, nothing light can reach 4.5:1 over a *white* wallpaper at 0.75 (the effective
background is ≈ `#8c8c8c` at best), so the method does not promise that case; the white
wallpaper screenshot is legible but not AA.

## Colour math choices

- **OKLab / OKLCH** (Ottosson 2020) for every mix, ramp and nudge: equal steps look equal,
  and hue stays put when lightness moves (CIELAB drifts blues toward purple).
- **WCAG 2 contrast** for floors, because it is what the tests already used and what
  reviewers know. APCA would rate dark-mode text more kindly; not needed yet.
- **Lightness clamp** (`ensure_contrast`): walk OKLCH L away from the background in 0.01
  steps, keeping hue and chroma (gamut-clipped by shrinking chroma only). An accent never
  fades to grey on the way up, which is why glass graph feet keep their hue.
- **ΔE OK ≥ 0.08** between panel accents and from warn/crit, plus a **35° hue gap** from
  warn/crit: ΔE alone let Gruvbox's yellow-green (`#b8bb26`) through as the cpu colour,
  where it read as a warning next to Gruvbox yellow.
- Near white there is little sRGB chroma: a mid-grey panel (Zenburn `#3f3f3f`) on 70 %
  glass pushes all accents toward white, where eight hues cannot all sit 0.08 apart. The
  measured best was ≈ 0.072; the test floor for that case is 0.065.

## Role hue families (polish wave)

Mapping roles to fixed ANSI slots failed on muted palettes. On Kanagawa Dragon glass 0.75
the cpu green `#87a987` (OKLCH C ≈ 0.07) was lifted to `#a3c6a3` (C 0.062) and the gpu
magenta `#938aa9` (C ≈ 0.05) to `#c2b8d9` (C 0.047): on the Sonoma wallpaper both read as
beige-grey. The old "temp = paler blue" was nudged by separation to teal and collided with
mem. Solarized's bright yellow slot `#657b83` is a grey, so warn was grey too.

Now each role has an OKLCH hue family and picks the closest palette colour with C ≥ 0.06
(lifted to C 0.10), else synthesises the target hue at the palette's lightness. Kanagawa
Dragon glass 0.75 after: cpu `#8fc98f` (C 0.10, 145°), gpu `#d8acec` (synthesised, C 0.10,
315°), mem `#26d3cb` (190°), net `#47cbfa` (226°), heat `#f2ae80` (55°), disk `#e7c55b`
(92°). Heat is placed before disk because orange has the least room (between warn's yellow
and crit's red at glass lightness, orange tops out near C 0.12); disk can slide toward
yellow. Warm roles skip the warn/crit checks: heat runs up to crit's red by design.

## Built-ins meet the floors natively (wave 4)

The built-in hex tables are now frozen outputs of the same `OKLCH` lightness walk
(`ensure_contrast`) that hardens the adaptive theme: hardening any built-in against its
reference backgrounds is a no-op (`every_builtin_meets_floors_without_hardening`). The
walk mostly lifted graph feet, which sat below 3:1 and now reach it (billabong gpu foot
`#4a3c96` → `#675dba`), and Paper's frames (`border` `#cfd5dc` → `#c2c8cf`, 1.5:1 on white).
Kanagawa (Dragon; references `#181616`, the owner's `#141313`, `#0d0c0c`) and Gruvbox Dark
built-ins exist for terminals that do not answer OSC 4. The auto-derived Kanagawa/Gruvbox
themes put disk on the warn colour (gold family), so the built-ins keep disk on muted gold
(Kanagawa `#c4b28a`, ΔE76 37 from warn `#ffa066`) or lavender (Gruvbox `#b8a1dc`); Kanagawa's
aqua and spring blue sit too close to sage and violet (ΔE76 14), so mem `#64c3b9` and net
`#7e9cd8` (crystal blue) were used instead.

`extra_contrast` (setting) raises floors on top: dim 4.5 → 7, muted 3 → 4.5, border 1.5 → 3
(glass 3 → 4.5) and faint 1.3 → 2 (glass 2 → 3). `glass` = auto | on | off is the setting
behind `SILTOP_GLASS`; the env var still wins, and env `auto` defers to the setting.

Thermal Fair (C08) no longer paints every graph amber: only the cpu graph's crest row turns
warn, and the cpu and heat frames tint 60 % toward warn. Serious and Critical still turn
every load graph crit. On glass the selected row keeps each numeric column's value tint;
only neutral text (fg, title, dim, muted, unstyled) takes the highlight ink.

## Querying the palette

One write: `OSC 10 ; ? ST`, `OSC 11 ; ? ST`, sixteen `OSC 4 ; n ; ? ST`, then DA1
(`CSI c`). Terminals answer DA1 after the colour replies, so reading until DA1 costs one
round-trip. Ghostty, kitty, iTerm2, WezTerm and Alacritty are documented to answer all
eighteen (only the harness emulation was exercised this wave, not a live Ghostty). Where
OSC 4 goes unanswered (unverified for Terminal.app) the adaptive builder
declines and the fallback picks by
background luminance. Replies come as `rgb:RRRR/GGGG/BBBB` (1–4 hex digits a channel),
BEL or ST terminated. The tty is restored with `TCSAFLUSH` so unread reply bytes are
dropped; anything later is eaten by `event::OscSwallow` (crossterm tokenises `ESC ]` as
Alt+`]`).

## Config files for opacity

| terminal | key | where |
| --- | --- | --- |
| Ghostty | `background-opacity = 0.75` | `$XDG_CONFIG_HOME/ghostty/config`, `~/Library/Application Support/com.mitchellh.ghostty/config`, `config.ghostty` (later wins) |
| Kitty | `background_opacity 0.8` | `$XDG_CONFIG_HOME/kitty/kitty.conf` |
| Alacritty | `[window] opacity = 0.9` | `~/.alacritty.toml`, `$XDG_CONFIG_HOME/alacritty/alacritty.toml` |
| WezTerm | `window_background_opacity` in Lua | not parsed; use `SILTOP_GLASS` |

Ghostty `config-file` includes and per-theme overrides are not followed yet.
