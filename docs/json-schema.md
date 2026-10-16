# JSON output, schema 1

`siltop --json` prints one object; `siltop --json-stream` prints one object per line
(NDJSON), `--count N` lines if given. Both prime the sampler first and print only samples whose
rates were measured over a real interval. Built in `crates/siltop/src/headless.rs` from DTOs
mapped out of `siltop_core::Snapshot`; the core types carry no serde.

## Rules

- `schema_version` is `1`. Schema 1 is frozen at 0.1.0 and only **gains** fields: nothing is
  renamed or removed. `headless::tests::schema_field_set_is_stable` pins the field set; adding a
  field means updating that list and this file in the same commit.
- snake_case everywhere. Names follow macmon where macmon has one.
- `null` means "not measured on this machine / not collected yet". A measured zero is `0`.
  Non-finite floats are never emitted (`NaN` becomes `null`, or `0` for a usage share).
- Units are in the name or listed below: `_bytes`, `_bps` (bytes per second), `_mhz`, `_c` (°C),
  `_power` (watts), `usage` (share 0..=1).
- **One exception:** `network.rx_bps` and `network.tx_bps` are **bits** per second, the way
  network speeds are quoted. Divide by 8 for bytes, and do not add them to the byte rates of
  `disk` or `memory`. Schema 1 is frozen, so the names stay.

## Fields

| Field | Type | Meaning |
| --- | --- | --- |
| `schema_version` | int | `1` |
| `unix_ms` | int | wall-clock time the object was built |
| `interval_ms` | int | sampling interval (`-i`, default 1000 headless) |
| `primed` | bool | rates measured over an interval (always `true` in printed samples today) |
| `soc.name`, `soc.{e,p,s}_cores`, `soc.gpu_cores`, `soc.memory_bytes` | | chip identity |
| `cpu.usage` | 0..=1 | non-idle share across all cores |
| `cpu.scaled` | 0..=1 | usage weighted by frequency: Mach busy × the IOReport DVFS clock ratio per core; equals `usage` only where IOReport residency is unavailable |
| `cpu.freq_mhz`, `cpu.temp_c` | int?/float? | package frequency and temperature |
| `cpu.clusters[]` | `{kind, cores, usage, scaled, freq_mhz}` | per cluster; `kind` is `efficiency`, `performance` or `super`; `freq_mhz` is the residency-weighted average from IOReport DVFS states over the cores' active time; `null` while the cluster is idle (no active residency in the window, common on a quiet Mac), until the first residency window lands, and where IOReport is unavailable — never a made-up idle clock |
| `cpu.cores[]` | `{kind, index, usage}` | per logical core, Mach order |
| `gpu` | object? | `{usage, scaled, freq_mhz, temp_c, mem_in_use_bytes, mem_alloc_bytes}`; `null` without a GPU reading. `freq_mhz` is the residency-weighted active clock, `null` while the GPU was idle for the whole window and where IOReport is unavailable |
| `memory.ram_total`, `memory.ram_usage` | bytes | macmon names |
| `memory.swap_total`, `memory.swap_usage` | bytes | macmon names |
| `memory.wired_bytes`, `memory.compressed_bytes`, `memory.cache_bytes` | bytes | `cache_bytes` is file-backed + purgeable (Activity Monitor's "Cached Files") |
| `memory.pressure` | string | `nominal`, `warn`, `critical` |
| `memory.swap_in_bps`, `memory.swap_out_bps` | bytes/s? | swap activity; `null` until two samples exist |
| `memory.free_percent` | 0..=100? | `kern.memorystatus_level`, the figure `memory_pressure` prints |
| `network.iface`, `network.rx_bps`, `network.tx_bps` | string, **bits**/s | headline interface: the default-route one, else the busiest physical `enN`; bits/s from 64-bit `ifmib` counters |
| `network.primary` | string? | interface carrying the default route (`en0`, a VPN `utunN`…); `null` without one |
| `disk.read_bps`, `disk.write_bps`, `disk.volumes[]` | bytes/s | `volumes[]` is `{name, mount, used_bytes, total_bytes}` |
| `temp.cpu_temp_avg`, `temp.gpu_temp_avg` | °C? | macmon names |
| `temp.hotspot_c`, `temp.{e,p,s}_cluster_c` | °C? | hottest die sensor and per-cluster zones |
| `fans[]` | `{name, rpm, max_rpm}` | empty on fanless Macs |
| `thermal_pressure` | string | `nominal`, `fair`, `serious`, `critical` |
| `sys_power` | W? | whole-system input power (SMC `PSTR`); `null` when the SMC has no `PSTR` |
| `all_power` | W? | sum of the known `cpu/gpu/ane_power` rails; `null` when none is known |
| `cpu_power`, `gpu_power`, `ane_power`, `ram_power` | W? | macmon names. CPU: SMC rails on mapped models, else the IOReport energy counter; GPU/ANE/DRAM: IOReport energy counters, held between PMGR publications and `null` until two have landed or once 10 s stale (docs/research/13) |
| `gpu_freq_mhz` | MHz? | macmon name; same value as `gpu.freq_mhz`, so also `null` while the GPU is idle |
| `battery` | object? | `{percent, charging, on_ac, watts, minutes_remaining}`; `null` on Macs without a battery (desktops). `watts` is the battery's own power: **positive while it charges, negative while it discharges** (powers the Mac), about 0 when full on AC |
| `processes[]` | | top `--procs N` (default 10) by CPU: `{pid, ppid, name, user, cpu_pct, cpu_cores, mem_bytes, threads, energy_w, restricted, status}`. `cpu_pct` is **per-core percent** like Activity Monitor and `top` (100 = one fully busy core, so a process can exceed 100); `cpu_cores = cpu_pct / 100`. `mem_bytes` is the physical footprint. `cpu_pct`, `cpu_cores`, `mem_bytes`, `threads` and `energy_w` are `null` when the kernel withholds them (`restricted: true`, another user's process) — never an invented 0. `status` is `"stopped"` (SIGSTOP, Ctrl-Z), `"zombie"` (exited, not yet reaped), or `""` for every live process: the kernel's `p_stat` reads running for nearly every process, so running and sleeping are not told apart |
| `verdict` | object | `{kind, headline, evidence[]}`; `kind` is `normal`, `busy`, `battery_drain`, `memory_tight` or `throttling` (see `core/src/verdict.rs`) |
| `health` | object | collector → `ok`, `pending`, `stale`, `failed`, `absent` |

## Pending mappings

`sys_power`, `ram_power`, `battery.*` and cluster `freq_mhz` are mapped. One-shot `--json`
waits (at most 2.5 s including the interval) until the slow collectors have a first reading
(`Snapshot::settled`); `--json-stream` does not wait, so its first line may still carry
`null`s. The verdict's battery-drain rule is still pending
(`VerdictEngine::update(.., battery_drain_w)`).
