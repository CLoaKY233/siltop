<h1 align="center">siltop</h1>

<p align="center">
  <b>A calm, honest system monitor for Apple Silicon, in the terminal you already live in.</b><br>
  Real watts, real clocks and real temperatures from every M1–M5 chip. No sudo, no telemetry, about 1.3% of one core.
</p>

<p align="center">
  <a href="https://github.com/CLoaKY233/siltop/actions/workflows/ci.yml"><img src="https://github.com/CLoaKY233/siltop/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/CLoaKY233/siltop/releases/latest"><img src="https://img.shields.io/github/v/release/CLoaKY233/siltop?color=4c8" alt="Latest release"></a>
  <img src="https://img.shields.io/badge/Apple%20Silicon-M1%E2%80%93M5-333" alt="Apple Silicon M1 to M5">
  <a href="#license"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue" alt="License: MIT OR Apache-2.0"></a>
</p>

![siltop during a build: CPU, GPU, memory, heat, network and power beside the process table](docs/media/hero-dark.png)

<p align="center">
  <img src="docs/media/glance.png" width="49%" alt="The Glance surface during a GPU inference spike">
  <img src="docs/media/hero-light.png" width="49%" alt="siltop on a light terminal">
</p>

<sub>Screenshots come from `siltop --demo`, a canned 60-second session (build burst, GPU inference, battery drain, rising heat), so they look the same on every Mac.</sub>

## Why another monitor?

`btop` is beautiful, but on a Mac it can't see the chip: no watts, no GPU, no cluster clocks. `asitop` and `powermetrics` see the chip but need `sudo`. `macmon` is lean but stops at the SoC. siltop sits in between: **as clean as you want by default, as deep as you need when something is wrong.**

|                                         | siltop | btop | macmon | asitop |
| --------------------------------------- | :----: | :--: | :----: | :----: |
| CPU / GPU / ANE / DRAM watts             | ✓ | — | ✓ | ✓ |
| Per-cluster clocks (E / P / Super cores) | ✓ | — | ✓ | ✓ |
| Temperatures and fans                    | ✓ | CPU temp | temps | — |
| Per-process energy and GPU               | ✓ | — | — | — |
| Full process manager (tree, signals, filter) | ✓ | ✓ | — | — |
| Runs without `sudo`                      | ✓ | ✓ | ✓ | — |
| Machine-readable output                  | JSON / NDJSON / Prometheus / CSV | — | JSON | — |

<sub>Self-cost was measured back to back with btop and macmon on the same machine; the method and raw numbers are in [docs/research/12-self-cost.md](docs/research/12-self-cost.md).</sub>

## Highlights

**Truth**
- Numbers come straight from macOS: the SMC for fans, temperatures and system power, `IOReport` for CPU and GPU clocks and energy, Mach and `sysctl` for CPU, memory and processes. No `ps`, no `powermetrics`, no scraping.
- A reading the Mac doesn't give is shown as absent, never as a made-up `0`. Fanless Airs show no fans, desktops show no battery, a process the kernel won't describe shows `—`.
- A one-line verdict (`All normal`, `Busy: rustc using 9.1 cores`, `Hot: fans at 80%`) with the evidence behind it.

**Speed**
- About 1.3% of one core at a 1 s interval, half of what btop costs at the same rate (2.5%) and on par with macmon (1.4%), measured back to back on an M5 Pro. It only redraws when something changed, and pausing (`p`) stops sampling entirely.
- One native binary of about 3 MB, written in Rust. Starts instantly.

**Beauty**
- Braille graphs on one shared, time-linear clock, so every panel lines up; `z` zooms time.
- Themes built from your terminal's own palette, readable on translucent and blurred windows. Ten built-in themes plus a theme pack, btop theme import, and 256-colour, 16-colour and `NO_COLOR` fallbacks.
- Two surfaces: **Work** for digging in, **Glance** for a small always-on window. The layout adapts from 80×24 up. Five presets on `1`–`5` and a zen mode (`Z`) that leaves only the graphs.

**Control**
- A process table with live filter, sort by any column, tree, app grouping and a details card (threads, open files, ports) with a signal picker, renice and background QoS. A kill is refused if the pid was reused since you chose it.
- Full mouse support, htop and btop key profiles, a searchable settings page, and a config file that keeps your comments.

**Beyond the dashboard**
- `--json` / `--json-stream` for scripts and status bars, `siltop line` for tmux and SketchyBar, alerts with shell hooks, a timeline with reports, session record/replay, a Prometheus endpoint, a sustained-performance bench and a multi-Mac fleet view.

## Install

Requires macOS on Apple Silicon.

**Homebrew**

```sh
brew install cloaky233/tap/siltop
```

**Cargo** (builds from source; the toolchain is pinned in `rust-toolchain.toml`)

```sh
cargo install --git https://github.com/CLoaKY233/siltop siltop --locked
```

**Prebuilt binary**: every [release](https://github.com/CLoaKY233/siltop/releases) has `siltop-<tag>-macos-arm64.zip` and `siltop-<tag>-macos-arm64.tar.gz` with SHA-256 files. Unpack and put `siltop` on your `PATH`. If macOS says it can't check the file, run `xattr -d com.apple.quarantine siltop` once.

Then run `siltop`. Try `siltop --demo` first if you just want to look around.

## Usage

```sh
siltop                                  # the dashboard
siltop --demo                           # a canned 60 s session, no hardware needed
siltop -i 1s --theme paper              # sample every second, Paper theme
siltop --surface glance --no-mouse      # the compact surface
siltop --json | jq .cpu.clusters        # one JSON sample (schema 1)
siltop --json-stream --count 3 -i 500   # NDJSON, one line per interval
siltop --diagnose                       # "All normal" / "Busy: Safari using 3.1 cores" + evidence
siltop doctor                           # chip ids, SMC keys, HID sensors, readability, self-cost
siltop line --style tmux                # one status line: "cpu 12% gpu 3% 8.4W 51°C"
siltop report week --json               # energy by app, peak temps, time spent hot
siltop agent install                    # alerts in the background, at login
siltop record --duration 10m            # a crash-tolerant session trace; `siltop replay` plays it
siltop serve                            # Prometheus metrics on 127.0.0.1:9101
siltop bench --duration 5m              # sustained CPU test: clocks, heat, throttling
siltop fleet studio mini.local          # this Mac beside two others, over your own ssh
siltop completions zsh                  # also bash and fish; `siltop man` prints the manual
```

The **[user guide](docs/guide.md)** walks through every panel, what the numbers mean, keys, settings, presets, themes, alerts, scripting and troubleshooting. `?` inside the app shows every key; `m` opens the menu; `s` opens settings.

## Configuration

Everything is optional. Settings live in `~/.config/siltop/config.toml` (or under `$XDG_CONFIG_HOME`), are written only when you change something, and keep your comments. The guide lists [every key](docs/guide.md#settings). Useful environment variables:

| Variable | Effect |
| --- | --- |
| `SILTOP_THEME=<name>` | Use that theme. `auto` (the default) reads the terminal. |
| `SILTOP_KEYS=htop\|btop` | htop or btop key layout |
| `SILTOP_COLORS=truecolor\|256\|16\|none` | Override the colour-depth guess |
| `SILTOP_ICONS=nerd` | Nerd Font icons in panel titles |
| `NO_COLOR=1` | Attributes only (bold, dim, reverse) |

## Privacy

- **No network of its own.** siltop does not link an HTTP stack. If you opt in to update checks, it runs `/usr/bin/curl` against the GitHub releases API at most once a day and sends only its version in the `User-Agent`. `siltop serve` listens on 127.0.0.1 unless told otherwise; `siltop fleet` runs your own `ssh`.
- **No telemetry.** Nothing about you or your Mac is collected or sent. Crash files stay on your Mac.
- **No sudo.** Every reading comes from APIs a normal user may call. What the kernel withholds (other users' processes) is shown as `—`, not guessed.

## FAQ

**Why do some processes show `—`?**
macOS only lets you read CPU, memory and energy for your own processes. Rows owned by other users keep their name and pid, but the kernel refuses the rest, so siltop shows `—` rather than a made-up `0`. Their CPU is still counted in the machine total.

**Why does a watt figure sometimes show `≈` and an age?**
Some power rails publish in batches, not every sample. Between publications siltop holds the last measured value and marks it `≈` with its age instead of inventing a number or dropping to zero.

**Why does my CPU read lower than Activity Monitor's?**
The graph is frequency-weighted: a core busy at half its top clock counts as half busy, so it shows how much of the chip's real capacity is in use. Cluster rows show both the busy share and the clock.

**Intel Macs, Linux?**
No. siltop is built around Apple Silicon APIs. Other platforms compile (the collectors are stubbed out) but sample nothing.

## How it works

Four crates with a strict dependency direction (core ← {metrics, ui} ← bin):

| Crate | Role |
| --- | --- |
| [`siltop-core`](crates/siltop-core) | Snapshot types, history store, config, verdicts |
| [`siltop-metrics`](crates/siltop-metrics) | Collectors (SMC, IOReport, IOHID, Mach, sysctl) and the sampler. No UI types |
| [`siltop-ui`](crates/siltop-ui) | Themes, braille graphs, layout, widgets. No metrics imports |
| [`siltop`](crates/siltop) | The binary: event loop, input, CLI, the demo session |

Each interval, `Sampler::tick()` on a worker thread builds one immutable `Snapshot`; the UI renders it read-only. See [crates/ARCHITECTURE.md](crates/ARCHITECTURE.md).

[`docs/research/`](docs/research) is a knowledge base on how macOS exposes this data: SMC keys per chip generation, the `IOReport` energy recipe without sudo, process data sources, graph rendering and colour on translucent terminals. If you're writing your own macOS monitor, start there.

## Contributing

Bug reports with `siltop doctor` output are the most useful thing you can send, especially from a chip or Mac model not covered yet. See [CONTRIBUTING.md](CONTRIBUTING.md) for building, tests and golden screens.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Copyright © 2026 Lay Sheth.
