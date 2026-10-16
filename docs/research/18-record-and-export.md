# 18 — Record, replay and export

Status: landed on `wave5/record` (2026-09-23) for 1.1.

| Command | Where |
|---|---|
| `siltop record [--out FILE] [--duration SPAN]` | `bin/src/record.rs`, format in `core/src/trace.rs` |
| `siltop replay FILE` | `bin/src/replay.rs` (`Player`), `bin/src/source.rs` (`SnapshotSource::Replay`) |
| `siltop serve [--addr HOST:PORT]` | `bin/src/serve.rs` |
| `siltop export --csv [--from WHEN] [--to WHEN]` | `bin/src/export.rs` |

A trace is self-contained: attach it to a bug report and anyone can open it
with `replay`.

## Trace format (`.ptrace`, version 1)

```text
magic "PTRACE\0\0" | version u16 LE | header record | frame records ...
record = payload length u32 LE | CRC-32 (IEEE) of payload u32 LE | payload
header payload = interval (secs varint, nanos varint) | started unix ms varint | note string
frame payload  = ms since previous frame varint | 13 section slots
section slot   = 0 (same bytes as the previous frame) | 1, length varint, bytes
```

- Integers are LEB128 varints (signed ones zigzag), floats raw little-endian,
  `Option` a 0/1 tag, `Vec` and `String` a length prefix. Field order is the
  struct's declaration order, written by the `wire_struct!` macro: adding a
  field to a snapshot type fails to compile until the trace learns it, and
  any layout change bumps `TRACE_VERSION` (readers refuse other versions).
- Sections, in wire order (append only): soc, cpu, memory, gpu, network,
  disk, fans, sensors, processes (+ `process_ncpu`, `unattributed_cpu`),
  thermal + power, battery, flags (status, sampled, primed, settled), health.
  A section byte-identical to the previous frame's is a single `0`, so the
  chip, the battery, the volumes and the fans cost one byte a frame after
  the first.
- Processes are cut to `TRACE_PROCS` = 32 per frame, busiest CPU first
  (unreadable CPU sorts last). `Process::status` is stored as an index into
  `["", "stopped", "zombie"]`.
- Size: a synthetic 240-frame recording with 50 changing processes a frame
  is ~1.5 KB a frame (processes dominate); an idle Mac is far smaller.
- Time: frames carry offsets, never `Instant`s. The reader takes an `epoch`
  and sets `collected_at = epoch + offset`; the player restamps each frame
  with the moment it plays, so the app's shared sample clock stays monotonic.

**Truncation and corruption.** `record` writes every frame as one
`write_all` on an unbuffered file, so Ctrl-C, a crash or a full disk tears
at most the last record. The reader stops at the first record that is short,
fails its CRC, claims more than 16 MiB, or does not decode exactly, and keeps
every frame before it (`Trace::truncated` says so; replay shows a toast).
Deltas only ever refer to the previous frame, so no keyframes are needed for
that guarantee. A bad magic, an unknown version or a damaged header refuses
the file with a specific message.

## Replay

`Player` is pure (wall `Instant`s in, frames out) and lives in the main loop:
`App::next_deadline` includes the player's next frame and `App::update`
applies whatever is due, so replay needs no thread and a paused replay
wakes nobody.

| Key | Action | Notes |
|---|---|---|
| `p` | pause / resume | the app's own freeze; the player holds its position |
| `+` `=` `]` / `-` `[` | speed ¼×, ½×, 1×, 2×, 4×, 8×, 16× | never touches the saved sampling interval |
| `→` / `←` | scrub a twentieth of the recording (at least one interval) | works while paused; the frame at the target shows at once |

Replay keys come from `keymap::REPLAY`, consulted before the Normal tables
only while no overlay is open (help, menus and settings keep their arrows).
The footer toast carries the scrub bar: `replay ▶ 2× ━━━●──────── 01:23 / 10:00`.
A replayed session never writes to this Mac's history store.

Known limit: graphs keep scrolling in play order, so a scrub back appends
older frames after newer ones rather than rewinding the graph window.
Rebuilding the window on seek waits for the series registry refactor.

## Prometheus (`serve`)

`std::net::TcpListener`, one connection at a time, 2 s read/write timeouts,
8 KiB request cap. `GET /metrics` (query string ignored) answers 200 with
`text/plain; version=0.0.4`; any other path is 404, any other method 405.
Loopback `127.0.0.1:9101` unless `--addr` says otherwise. The sampler runs
on the usual worker thread at `-i` (default 1 s); scrapes read the latest
exposition and never trigger a sample.

Every family is a gauge. A reading this Mac lacks, or a rate not yet
measured (`primed == false`), is left out, never exported as 0.

| Metric | Labels | Unit / meaning |
|---|---|---|
| `siltop_cpu_usage_ratio` | | machine CPU busy, frequency-weighted, 0..1 |
| `siltop_cpu_cluster_usage_ratio` | `cluster` = e, p, s | per cluster, 0..1 |
| `siltop_gpu_usage_ratio` | | GPU busy, 0..1 |
| `siltop_memory_used_bytes`, `siltop_memory_total_bytes`, `siltop_swap_used_bytes` | | bytes |
| `siltop_network_receive_bytes_per_second`, `siltop_network_transmit_bytes_per_second` | | all interfaces |
| `siltop_disk_read_bytes_per_second`, `siltop_disk_write_bytes_per_second` | | all disks |
| `siltop_power_watts` | `rail` = system, cpu, gpu, ane, dram | watts; `system` is the whole Mac's input |
| `siltop_temp_celsius` | `zone` = cpu, gpu, hotspot, e, p, s | °C |
| `siltop_fan_rpm` | `fan` | RPM; absent on fanless Macs |
| `siltop_thermal_pressure` | | 0 nominal, 1 fair, 2 serious, 3 critical |
| `siltop_battery_charge_ratio`, `siltop_battery_health_ratio` | | 0..1; laptops only |
| `siltop_battery_watts` | | positive in, negative out |
| `siltop_battery_cycles`, `siltop_battery_external_power` | | count; 1 on an adapter |
| `siltop_process_cpu_ratio` | `name`, `pid` | top 10 by CPU; 1 = one core fully busy |

Label values escape `\`, `"` and newline per the text format. `pid` as a
label is high-cardinality over days; scrape configs that keep long
retention may want `metric_relabel_configs` to drop it.

## CSV (`export --csv`)

Reads the history store (`17-history-store.md`), one row per bucket,
oldest first. `--from` / `--to` take a UTC date (`2026-09-23`; as `--to`
it means the end of that day) or a span ago (`90m`, `24h`, `7d`); the
defaults are everything kept and now. Ranges starting inside the last 24 h
read the Day tier (6 s buckets), older ones the Month tier (144 s).

Columns (48, generated from `Series::ALL` by `export::header`): `unix_s`,
`utc` (ISO 8601, `Z`), `samples`, then `<series>_mean` and `<series>_max`
for each of the 21 series, in this order:

- the 14 of store record version 1: `cpu_ratio`, `gpu_ratio`, `mem_ratio`,
  `swap_bytes`, `net_rx_bytes_per_s`, `net_tx_bytes_per_s`,
  `disk_read_bytes_per_s`, `disk_write_bytes_per_s`, `cpu_temp_c`,
  `gpu_temp_c`, `sys_power_w`, `cpu_power_w`, `gpu_power_w`, `fan_rpm`;
- the 6 version 2 added: `battery_pct` (0..=100), `battery_w` (signed: +
  charging, − draining), `on_ac` (1 on AC, 0 on battery; its mean is the
  share of the bucket on AC), `thermal_state` (0 nominal ..= 3 critical),
  `throttled_ratio` (share of the bucket at serious or worse),
  `battery_health_pct`;
- version 3's `cpu_hot_ratio` (share of the bucket the CPU read at or
  above `HOT_C`).

Then `top_cpu`, `top_energy`, `top_mem` as `name (pid) value; ...`. These
are per-process lists, so a name is the process's display name cut to the
store's 16-byte field (`Brave Browser He`); the report's energy by app
folds helpers into their `.app` first and so keeps whole app names. Battery
columns are empty on desktops, and rows from older records leave the
columns their version lacks empty. An absent reading is an empty cell. Cells are RFC 4180-quoted only when needed. A
closed pipe (`| head`) ends the export quietly.

## Deferred

- InfluxDB line protocol (same families as Prometheus).
- `serve` behind a cargo feature (the plan's §4.4 item 7) — it is ~250 lines
  of std with no dependencies, so it ships in the default build for now.
- Rewinding graph windows on scrub (see above); a scrub bar drawn by the
  footer widget instead of a toast; click-to-seek on that bar.
- `record` while the TUI is open (a key that starts/stops a trace).
