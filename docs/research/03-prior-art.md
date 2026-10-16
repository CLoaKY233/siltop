# Prior art — btop, macmon, and the rest

Last updated: 2026-08-24.

We are not forking these. We are stealing **taste**, **metric recipes**, and **efficiency tricks**. Licenses matter if we ever copy more than an idea: btop is Apache-2.0, macmon MIT, bottom MIT, Stats MIT, iSMC GPL-3.0 (don't copy their Go).

## 1. btop++ — the aesthetic bar

Repo: <https://github.com/aristocratos/btop> · C++23 · Linux/macOS/BSD · v1.4.7 when researched.

### 1.1 What it is

The thing people mean when they say a system monitor looks "good." Continuation of bashtop/bpytop. Custom TUI renderer (not Ratatui). Game-like menus, full mouse, themes, presets.

### 1.2 Layout

Boxes: **cpu**, **mem** (RAM + disks), **net**, **proc**, optional **gpu0..gpu5**. Config `shown_boxes` e.g. `"cpu mem net proc"`.

`Draw::calcSizes()` (`btop_draw.cpp` ~2248). Each box has **percent of terminal** + **hard min**:

| Box | `width_p` | `height_p` | min (cells) |
| --- | --- | --- | --- |
| CPU | 100 | 32 | **60 × 8** |
| GPU | 100 | 32 | 41 × 8 |
| MEM | 45 | 40 | 36 × 10 |
| NET | 45 | 28 | 36 × 6 |
| PROC | 55 | 68 | 44 × 16 |

Default tile: CPU full width on top (graph left, cores/temp/battery right); MEM over NET on the left; PROC fills the right column.

- Flags: `cpu_bottom`, `mem_below_net`, `proc_left`.
- GPU boxes get a min height budget first, then leftover goes to cpu/mem/proc.
- On `SIGWINCH`, if the terminal is too small it **hides boxes in a defined order** until the rest fit (or shows “terminal too small”). **Hide, don’t smash.**
- Per-core sidebar inside the CPU box: pick a **column template** by how many cores fit:

  | `b_column_size` | Width budget | What you see |
  | --- | --- | --- |
  | 2 | `(21+12*temp) * cols` | fat core meters + temp |
  | 1 | `(15+6*temp) * cols` | medium |
  | 0 | `(8+6*temp) * cols` | compact; if even that fails, **drop column count** |

That last sentence is the responsive lesson: **degrade the core list before colliding with the graph**.

### 1.3 The graph (this is the "dots")

`Symbols::graph_symbols` in `btop_draw.cpp` ~89. Three families: `braille`, `block`, `tty` (░▒█), each with `_up` and `_down` (invert).

Braille up table — **copy this, it's the look**:

```
 " ", "⢀", "⢠", "⢰", "⢸",
 "⡀", "⣀", "⣠", "⣰", "⣸",
 "⡄", "⣄", "⣤", "⣴", "⣼",
 "⡆", "⣆", "⣦", "⣶", "⣾",
 "⡇", "⣇", "⣧", "⣷", "⣿"
```

`Graph::_create` (~422):

- Values normalized 0–100 (`max_value`, `offset`).
- Two samples per cell (`last`, `data_value`) except TTY mode (1:1).
- Each row of the graph maps a band of 0–100; within the band, map to level 0–4 with a small **bias** (`mod` = 0.3 on 1-row graphs, 0.1 on taller) so tiny values still light a dot.
- Index = `left * 5 + right`.
- **Color:** 1-row graphs colored by `max(last, value)` through a named gradient (`cpu`, `temp`, `used`, …). Tall graphs colored **by row** (top = hottest stop of the same gradient).
- `no_zero` keeps the bottom row from going fully empty (a faint “heartbeat”).
- Two alternating string buffers so adding one sample is O(height).

**Reimplement from Unicode bits, do not copy the 25-string tables.** Braille is `U+2800 + bits` (dot1 = bit0 … dot8 = bit7). Fill `n` dots from the bottom (or top if inverted) independently for the left and right columns. That *is* the look.

Themes build a **101-stop LUT** (`{name}_start` / optional `_mid` / `_end`) in `btop_theme.cpp`. Meters are a row of `■` on the same LUT plus a dim tail.

Also premium: `theme_background` (transparent), **`terminal_sync` (DEC 2026)** to kill flicker, per-box outline colors, `hi_fg` on clickable title keys.

Presets: `p` / `shift+p` cycle. Digit keys **toggle** boxes (`1` CPU … `4` PROC, `5–0` GPU0–5). Format `"box:P:G"` (position bit + graph symbol).

Config (defaults):

| Key | Default | Notes |
| --- | --- | --- |
| `graph_symbol` | `braille` | `block` / `tty` |
| `graph_symbol_{cpu,gpu,mem,net,proc}` | `default` | per-box override |
| `truecolor` | true | else 6×6×6 cube |
| `rounded_corners` | true | ignored in TTY |
| `update_ms` | **2000** | min 100 |
| `shown_boxes` | `cpu mem net proc` |  |
| presets | keys 1–9 |  |

TTY mode if a real tty is detected, or `-t`. Wide chars + braille font required (README prerequisites).

### 1.4 UX that makes it feel "not a simple TUI"

- Every key shown in a title is **clickable**.
- Wheel over the process list.
- Filter, sort (cpu/mem/user/pid/time…), tree, follow, pause list, detailed pane, send signals.
- Options **menu inside the app** — no "edit this toml and restart."
- Battery meter in the CPU box.
- Auto-scaling net graphs.
- Themes as files (same lineage as bpytop).
- Process graphs in the list (tiny per-row sparkline) — pretty, can get costly; they keep a map of `p_graphs` keyed by pid.

### 1.5 macOS collectors (`src/osx/`)

| File | Role |
| --- | --- |
| `btop_collect.cpp` | CPU (Mach), mem, disks (`statvfs`), net (`getifaddrs`), processes (kinfo + `proc_pidinfo`), Apple Silicon GPU (IOReport + HID) when `GPU_SUPPORT && __arm64__` |
| `smc.cpp` / `smc.hpp` | SMC |
| `sensors.cpp` / `.hpp` | temps, gated on recent macOS SDK |

Process CPU: Δ `pti_total_user+system` / Δ host ticks. Cache path/args. Skip re-walk when only filter/sort changed.

**Gaps on Mac (even after #1541):**

- GPU box exists, but AS still has no VRAM, and Intel dGPU is not first-class.
- Temps on AS have regressed before (changelog: "restore Apple Silicon temp reporting on M2 Pro").
- Power (watts) / ANE / frequency-scaled ratios are **not** the product. That's macmon's territory.
- Default 2 s feels sleepy for power graphs; they chose it for *sample quality* and CPU.

### 1.6 What to steal vs leave

Steal: braille table, two-sample packing, gradient-by-value and by-row, rounded boxes, presets, mouse, in-app options, calcSizes degradation, process cache, 2 s-or-so default.

Leave: writing our own terminal renderer, C++, their theme file format (unless we want compatibility), per-row process graphs on day one.

## 2. macmon — the Mac silicon bar

Repo: <https://github.com/vladkens/macmon> · Rust · **Ratatui 0.30.2** · crate `macmon` 0.8.2 · MIT · **Apple Silicon only** · no sudo.

### 2.1 What it is

A small TUI + JSON pipe + Prometheus exporter that reads the **same private APIs as `powermetrics`**. Library-first (`src_lib`). This is the best-documented open IOReport client in Rust.

### 2.2 Metrics it actually exposes

From `Metrics` in `src_lib/metrics.rs` and the README JSON:

- Temp avg CPU / GPU
- RAM total/used, swap total/used
- Fans `[{name, rpm, max_rpm}]`
- Combined + E + P: `*_scaled_ratio`, `*_active_ratio`, `*_freq_mhz`
- Per-core E/P (and M5 M-tier folded into E or P by name): `die_id`, `core_id`, freq, scaled, active
- GPU freq / scaled / active
- Power W: cpu, gpu, ane, all (=sum), sys (`PSTR`), ram, gpu_ram

No processes, no disks, no net, no battery UI, no Intel.

M5 naming is handled; Ultra `DIE_N_` too. Channel filter is tight (`shared.rs`).

### 2.3 How it samples

`Sampler::get_metrics(duration_ms)`:

1. `IOReport::get_sample_interval` — keep previous sample, sleep until `prev + duration`, take next, `CreateSamplesDelta`, **retain next as prev**. First call still waits a full interval.
2. Parse Energy Model + CPU/GPU states.
3. `libc_ram` / `libc_swap`.
4. Temps: SMC if any `Tp`/`Te`/`Ts` keys existed at init, else HID.
5. Fans: cached `F*Ac` keys.
6. `PSTR`.

Init scans SMC keys **once**. IOHID client created per temp read today (we can do better).

**Library warning:** `get_metrics` **blocks the thread**. Their own docs show a worker + `mpsc`. Copy that.

### 2.4 TUI (`src_app/tui.rs`)

Ratatui, rounded-ish blocks, two views (`v`): **Gauge** vs **Sparkline** (built-in Sparkline, not braille history). `d` per-core, `r` scaled/active, `c` color, `+/-` interval.

Layout: vertical `Fill(2)` / `Fill(1)` — top is E-CPU | P-CPU then MEM | GPU; bottom is CPU / GPU / ANE **power** sparklines with temp in the title. Title line: `Apple M3 Pro (6E+6P+18GPU 36GB)`.

**It is not trying to be btop.** It is a silicon dashboard that stays readable in a **small window**. That's a preset we should have ("silicon" / compact).

Sampler thread + input thread + UI thread. Interval default 1000 ms.

### 2.5 What to steal vs leave

Steal: IOReport filter + residency math + M5/Ultra channel parsing + SMC discovery + HID fallback + `PSTR` + worker-thread sampler + compact silicon layout + scaled vs active toggle. Optionally **depend on the crate** for the sampler.

Leave: Sparkline-as-the-only-graph (we want braille), missing processes/disks/net, AS-only if we can help Intel a bit.

## 3. bottom — Ratatui monitor, cross-platform

Repo: <https://github.com/ClementTsang/bottom> · Rust · Ratatui.

Closest **architecture** cousin: widgets + collection split (`src/widgets`, `src/collection`), layout config file, theming.

macOS collection:

- Processes: sysinfo + `ps` **fallback** for CPU (`processes/macos.rs`) — don't copy the `ps` spawn.
- Disks: real IOKit (`collection/disks/unix/macos/io_kit/`).
- CPU/mem/net/temp: mostly sysinfo.

Aesthetic: more "panel of widgets" than btop's cinematic graphs. Useful for process/disk/net structure, not for look.

**Steal from bottom specifically:**

- Collectors never import the UI (`collection/` ⊥ `canvas/`).
- TOML **row/column + ratio** layout (user-editable dashboard).
- **`e` expand** one widget to fullscreen — better than only hide-boxes.
- **`f` freeze** the snapshot (debug / screenshot).
- Time-axis **zoom** on histories.
- Focus via click or Ctrl-arrows.

## 4. Stats.app (exelban)

Repo: <https://github.com/exelban/stats> · Swift · menu bar, not TUI.

Modules: **CPU, GPU, RAM, Disk, Net, Battery, Sensors, Bluetooth, Clock**. The sensor/SMC/HID work in `Modules/Sensors` and CPU/GPU `bridge.h` is a gold mine for **which keys exist on which Macs**.

Steal: the *catalog* of metrics users expect on a Mac (pressure, per-process in popovers, sensor list). Leave: AppKit.

## 5. iSMC

Repo: <https://github.com/dkorunic/iSMC> · Go + CGo · GPL-3.

Two paths: classic SMC (Intel + AS power) and **HID** (AS temp/volt/current). `src/temp.txt`, `fans.txt`, `power.txt` + generated `smc/sensors.go` are the best **named key lists** including M5 / A18 Pro.

Use as a **dictionary**. Do not copy code (GPL).

## 6. Others, briefly

| Tool | Stack | Takeaway |
| --- | --- | --- |
| **asitop** | Python, **sudo powermetrics** | The look people liked before macmon. The method (spawn root powermetrics, parse text) is the **anti-pattern**. |
| **mactop** | Go + CGO, SMC + IOReport + HID | Same backends as macmon. Optional Prometheus. Optional **fan control** (root). Confirms the API set. |
| **MacMonitor** | native | `SENSORS.md` is an M2-validated key map (`TCMz` vs HID lag). |
| **htop** | C, ncurses | Process UX ancestor. Ugly graphs. |
| **glances** | Python | Kitchen sink, heavy. |
| **nvtop** | C | GPU-first; Linux vendors. |
| **Netdata macOS** | C | Production collectors: IOKit NVMe SMART, SMC+HID, thermal levels, IOReport GPU. Good when we want SMART/thermal depth. |
| **mactop** (metaspartan/mactop) | Go + CGO, gotui | Feature-max AS TUI: 20 layouts, DRAM GB/s, thermal, Thunderbolt, experimental per-process GPU, SoC-history preset, Prometheus. No sudo for read. Denser/noisier than btop. Steal layout cycling + a **silicon history** preset. |
| **macpow** (k06a/macpow) | Rust | Broader Energy Model than macmon: ISP, display, media, PCIe, fabric, per-core energy, backlight report. **Measured vs estimated** legend. One thread per source. `ri_billed_energy` per process. App, not a lib — copy parsers. |
| **pumas** | Rust asitop clone | Still **sudo powermetrics**. Memory tab tries to match Activity Monitor. JSON schema useful; privilege story is not. |
| **Mx Power Gadget** (Seense) | closed GUI | Gold standard for **throttling viz** — power and P-core MHz drop while “usage” stays 100%. Why scaled ratio + MHz must sit next to %. |
| **Redline** (apeabody007/redline) | native | Public-API subset + HID. Good Intel-safe core checklist. |
| **macos-smc-exporter** | Rust | Warns: many AS keys are `sp1e`, some `smc` crates miss them. Prefer `flt `. |

## 7. Comparison

|  | btop | macmon | bottom | Stats | asitop |
| --- | --- | --- | --- | --- | --- |
| Look | **best** | clean, small | ok | menu bar | ok |
| Braille graphs | **yes** | no (sparkline) | some | n/a | no |
| AS power / freq | little | **best** | no | yes | yes (sudo) |
| Processes / disks / net | **yes** | no | **yes** | yes | no |
| No sudo | yes (read) | **yes** | yes | yes | **no** |
| Ratatui | no | **yes** | **yes** | no | no |
| Intel | yes | no | yes | yes | no |
| Default interval | 2 s | 1 s | ~1 s | continuous | 1 s |
| Overhead | low if 2 s | very low | low | low | **high** (powermetrics) |

**Siltop sits in the empty cell:** btop's skin and UX + macmon's AS silicon + bottom's process/disk/net structure + Stats' Mac-specific extras (pressure, battery, sensors), all Ratatui, no sudo.

## 8. UX patterns that consistently feel "friendly"

From using / reading all of the above:

1. **Numbers in the title**, graph in the body. `CPU  18%  42°  8.2W` — you don't hunt.
2. **One primary color per box** (cpu cyan, mem green, net blue, gpu magenta) plus a shared gradient.
3. **Hide what does not exist** (fans, battery, second NIC, GPU box on a machine without IOReport).
4. **Degrade, don't wrap into garbage** at 80×24.
5. **Keys are visible.** Footer: `q quit  / filter  t tree  1-4 presets`.
6. **Mouse works** on anything that looks like a control.
7. **Nothing surprising on first launch.** Default boxes = cpu, mem, gpu/power (if AS), proc. Net if width allows.
8. **Interval control in-app.** People will crank it to 200 ms and then complain it's heavy — clamp and warn.
9. **Scaled vs raw CPU** on AS is not optional if we want to be more honest than btop.
10. **Confirm kills.**

## 9. Performance tricks that showed up more than once

- Collector thread ≠ UI thread (macmon, btop runner).
- Cache SMC connection + key list (everyone who isn't bad).
- Filter IOReport channels at subscribe time (macmon).
- Cache process names (btop).
- Don't collect when only the view changed (btop `no_update`).
- History = ring buffer sized to width (everyone).
- Temps slower than CPU % (implicit in Stats, explicit in our plan).
- Never powermetrics (everyone except asitop).
