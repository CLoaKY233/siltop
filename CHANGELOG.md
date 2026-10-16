# Changelog

All notable changes to siltop. Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [SemVer](https://semver.org/).

## [Unreleased]

## [1.2.1] - 2026-10-16

### Fixed
- `siltop update` no longer replaces a copy installed by `cargo install`; it prints the `cargo install --tag` line instead.
- A release download unpacked into any writable `bin` directory, `/usr/local/bin` included, can now update itself. Before, only copies under your home folder could.
- `SILTOP_NO_ONBOARDING` no longer turns off an update check you opted in to, and `SILTOP_NO_UPDATE_CHECK=0` no longer counts as set.
- An update found by the daily check is offered again on later launches until you install or skip it, instead of disappearing for a day.
- "Always update" in the update dialog now installs the offered version right away.
- Update errors are one line, a GitHub rate limit says so, and the manual `siltop update` waits up to 20 s instead of 3 s on a slow link.
- `updates.toml` is valid TOML.
- `--help` shows that bare `siltop update` works.

### Changed
- The Homebrew formula installs shell completions and the man page, and passes `brew audit --strict`.
- The README's install lines are pinned to the release, with a curl one-liner whose download is not quarantined.

## [1.2.0] - 2026-10-16

First public release, under MIT OR Apache-2.0. siltop grew up as a private project
(1.0 to 1.1.x); this is the same code base with everything unlocked and the history
squashed.

### Live readings
- **Power and clocks** without sudo: system and CPU watts from the SMC, CPU and GPU clocks from `IOReport` residency, and `IOReport` energy rails (CPU, GPU, ANE, DRAM). Batched readings are held and marked `≈` with their age instead of dropping to zero.
- **Every Apple Silicon chip, M1 to M5**, including Pro, Max and Ultra: per-cluster load for Efficiency, Performance and Super cores, with the clock each cluster ran at.
- **Heat and fans** from the full SMC key list, mapped per chip into CPU and GPU zones; fanless Macs show no fans, desktops show no battery.
- **Battery**: charge, state, health, cycles, time remaining, adapter, low-power mode and what is keeping the Mac awake.
- **Memory composition** (app, wired, compressed, cached), swap and the kernel's free level.
- **A verdict** (`All normal`, `Busy: rustc using 9.1 cores`, `Hot: fans at 80%`) with the evidence behind it.

### Processes
- Footprint memory, per-process energy, GPU, disk and wakeups; `—` instead of an invented zero when the kernel will not share a value.
- Live filter, sort by any column, tree with subtree totals, group helpers by app, tags, and a details card with threads, open files and ports.
- Safe signals (a reused pid is refused), a signal picker, renice, background QoS and copy over OSC 52.

### Interface
- Braille graphs on one shared, time-linear clock; `z` zooms time.
- Two surfaces, **Work** and **Glance**, adapting from 80×24 up; five presets and a zen mode.
- Themes built from your terminal's own palette, glass mode for translucent windows, ten built-in themes plus a theme pack, btop theme import, and 256-colour, 16-colour and `NO_COLOR` fallbacks.
- Full mouse support, a full-screen settings page with search, and htop and btop key profiles.

### Beyond the dashboard
- `--json` / `--json-stream` (macmon-compatible field names), `--diagnose`, `doctor` and `--demo`.
- **Timeline** (`H`) and `siltop report`: history on disk with energy by app, peak temperatures and time spent hot.
- **Alerts** in the dashboard and from a background LaunchAgent (`siltop agent install`), with shell hooks.
- `siltop line` for tmux, SketchyBar and Starship.
- `record` / `replay` crash-tolerant session traces, a Prometheus endpoint (`serve`) and CSV `export`.
- `siltop bench`: a sustained-performance test with a throttling report card.
- `siltop fleet`: several Macs in one terminal over your own ssh.
- Housekeeping: `siltop theme list|import`, `siltop update` (opt-in, verified self-update from GitHub releases), `siltop feedback`, `siltop reset`, `siltop onboarding`, `siltop completions` and `siltop man`.

### Cost
- About 1.3% of one core at a 1 s interval (btop: 2.5%, macmon: 1.4%, same machine); it only draws when something changed, and pausing stops sampling entirely. Method and numbers: [docs/research/12-self-cost.md](docs/research/12-self-cost.md).

[Unreleased]: https://github.com/CLoaKY233/siltop/compare/v1.2.1...HEAD
[1.2.1]: https://github.com/CLoaKY233/siltop/compare/v1.2.0...v1.2.1
[1.2.0]: https://github.com/CLoaKY233/siltop/releases/tag/v1.2.0
