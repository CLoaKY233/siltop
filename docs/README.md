# siltop docs

| File | What it is |
| --- | --- |
| [guide.md](guide.md) | **The user guide**: every panel, keys, settings, presets, themes, alerts, scripting, troubleshooting |
| [json-schema.md](json-schema.md) | `--json` / `--json-stream` schema 1: every field, units, null rules, macmon name mapping |
| [UI-DESIGN.md](UI-DESIGN.md) | Design system: information architecture, layout algebra, spacing, colour tokens, rendering strategy, house style |
| [release.md](release.md) | Release runbook: GitHub releases, Homebrew tap, codesign and notarization, Gatekeeper |

## Research notes

A knowledge base on how macOS exposes this data, kept current as we learn things about an
API, a widget or a chip generation. If you are writing your own macOS monitor, start here.

| File | What it is |
| --- | --- |
| [research/01-ratatui.md](research/01-ratatui.md) | Ratatui's widgets, layout, styles, canvas and markers, backends, third-party crates, performance |
| [research/02-macos-metrics.md](research/02-macos-metrics.md) | How macOS exposes CPU, GPU, ANE, RAM, storage, network, fans, temperatures, power, battery and processes: APIs, privilege, cost, per-chip SMC keys |
| [research/03-prior-art.md](research/03-prior-art.md) | btop, macmon, bottom, Stats.app, iSMC, asitop: what each does well |
| [research/12-self-cost.md](research/12-self-cost.md) | What siltop costs to leave open: `scripts/perf-smoke.py` method, numbers, and btop and macmon on the same machine |
| [research/13-ioreport-and-power.md](research/13-ioreport-and-power.md) | Package power without sudo: SMC `PSTR` and rails, the IOReport "Energy Model" recipe, macOS 27 PMGR batching, whole-family coverage |
| [research/14-process-data.md](research/14-process-data.md) | Process data without sudo: kinfo offsets, rusage v6, footprint vs RSS, exec renames, argv display names, per-pid GPU clients |
| [research/15-colour-and-glass.md](research/15-colour-and-glass.md) | Colour on translucent and blurred terminals, palette detection, contrast |
| [research/17-history-store.md](research/17-history-store.md) | Long history: buckets, tiers and downsampling, on-disk segment format (CRC, torn tails), retention |
| [research/18-record-and-export.md](research/18-record-and-export.md) | The `.ptrace` format, replay, Prometheus metric names, CSV columns |
