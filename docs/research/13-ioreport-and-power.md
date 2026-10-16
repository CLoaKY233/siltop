# 13 — IOReport energy and package power

How siltop gets CPU, GPU, ANE, DRAM and system watts without sudo on
every Apple Silicon Mac, what it costs, and where the edges are. Code:
`crates/siltop-metrics/src/{power,ioreport,sampler,pacer}.rs`.

## 1. Sources, in order of preference

| Figure | Primary | Fallback | Absent when |
|---|---|---|---|
| System (`sys_power`) | SMC `PSTR` (flt W), every sensor sweep | none | the SMC has no `PSTR` |
| CPU | SMC CPU rails, exact-model table (`smc_keys::cpu_rails`; Mac17,8 today), 3-sweep mean | IOReport `CPU Energy` held rate | neither exists or both fail the ceiling |
| GPU | IOReport `GPU Energy` 1 s delta (`GpuFreq.watts`) | IOReport held rate (`power.gpu`) | no `GPU Energy` channel |
| ANE | IOReport `ANE<n>` held rate (summed) | none | no ANE channel or < 2 publications |
| DRAM | IOReport `DRAM<n>` held rate (summed) | none | as ANE |

Nothing is ever reported as 0 W for missing data. `RailReadings::apply`
lays the IOReport rails over the SMC figures on every tick, starting from
the last SMC frame each time, so a rail that disappears from the feed does
not stick (regression test `energy_overlay_does_not_stick_after_the_feed_goes_quiet`).
Any rail above `PSTR + 5 W` is dropped as a glitch.

## 2. The recipe

1. `IOReportCopyChannelsInGroup("Energy Model", NULL)`; keep only the
   roll-up totals (`power::rollup_scoped`): `CPU Energy`, `GPU Energy`,
   `ANE`/`ANE<n>`, `DRAM`/`DRAM<n>`, and the same names behind a `DIE_<n>_`
   prefix. Never the cluster/core/`PACC`/`DTL`/`SRAM` channels: they
   overlap the totals and summing them over-counts about 3×.
2. `IOReportCreateSubscription` once, on the IOReport thread (a
   subscription must stay on the thread that made it), merged with "CPU
   Stats / CPU Core Performance States" into one PMGR subscription: a
   merged sample costs about what either half costs alone (§4).
3. Every `PMGR_PERIOD` (2 s; the first window after 1 s so the first
   snapshot settles): `IOReportCreateSamples`. The delta gives per-core
   residency and `GPU Energy` watts over the window; for each kept energy
   channel read, from the **raw** sample (not the delta):
   - the running total, `IOReportSimpleGetIntegerValue(ch, 0)`;
   - the unit label (`mJ` for PMGR channels, `nJ` for `GPU Energy`, `uJ`/`µJ`
     for a few others);
   - the driver timestamp: `RawElements` is a `CFData` holding an
     `IOReportElement` — `u64 provider_id`, `u64 channel_id`, 8-byte channel
     type, then `u64 timestamp` at **byte 24**, in mach absolute ticks.
4. Convert ticks with `mach_timebase_info` (125/3 on Apple Silicon; the
   conversion goes through `u128`, see `power::ticks_to_ns`), and stamp the
   read with `mach_absolute_time` (same clock) and `mach_continuous_time`
   (keeps running through sleep; only its gaps matter).
5. Feed each channel's `EnergyHold` (one per channel name, made on first
   sight): a rate is computed only between two *publications* (timestamp
   changed, ≥ 10 ms apart), held until the next one, dropped once the
   publication is older than `POWER_STALE_AFTER` (10 s), and reset on a
   counter reset, a timestamp going backwards, or a wall-clock gap longer
   than 3 cadences (sleep).
6. Roll up per rail: package totals win; per-die totals are summed only
   when no package total exists (so a chip exposing both is never counted
   twice).
7. The reader ages the frame to its own clock (`RailReadings::aged`):
   `Measured` stays measured for one CPU period, then counts as `Held`;
   held ages grow; anything past 10 s is dropped. A frame older than
   `MAX_AGE` (5 s, a stalled or parked thread) is ignored entirely.

## 3. macOS 27 batching

On macOS 27 the PMGR-owned channels (`CPU Energy`, `ANE*`, `DRAM*`, the
cluster channels) publish in **batches 2–108 s apart**; between batches the
total and its timestamp do not move at all (20-minute capture:
`src/fixtures/energy_rawts_20min.txt`). The AGX-owned `GPU Energy` channel
publishes on every read. Consequences:

- A delta sample over 1 s reads 0 J for PMGR channels most of the time.
  Dividing by the sample interval would show 0 W, then a spike. Dividing
  by the *driver* timestamp span gives the true average over the batch.
- The first PMGR rate needs two publications, so ANE/DRAM (and the CPU
  fallback) can take up to ~2 minutes to appear after launch. The UI shows
  them as absent until then, and as held (with age) between batches.
- With a 108 s batch the held value is older than 10 s for most of the
  window, so it goes absent again until the next batch. That is honest:
  we do not know the power in between.
- The PMGR can also go **silent for much longer**. Live trace on
  2026-09-23 21:15 (`ioreport::tests::energy_live_trace`, 16 s): `CPU
  Energy`, `ANE0` and `DRAM0` all carried a driver timestamp ~1,380 s old
  and never moved, with either the filtered roll-up subscription or the
  whole "Energy Model" group, while `GPU Energy` advanced every read. A
  120 s `--json-stream` run in the same window never produced `ram_power`
  or `ane_power`. So on the Mac17,8 the IOReport CPU/ANE/DRAM rails are
  often absent in practice; the SMC rails stay the primary CPU source and
  `PSTR` the system figure. Whatever re-triggers the PMGR batches (earlier
  captures saw 2–108 s) is not yet known — a follow-up.

## 4. Cost

Measured with `cargo test -p siltop-metrics --release -- --ignored
--nocapture ioreport_cost` on the Mac17,8 (thread CPU time per sample;
the host was busy with parallel builds, so absolute numbers are high):

| Subscription | Channels | Cadence | CPU per sample | Share |
|---|---|---|---|---|
| GPU residency (`GPUPH`) | 1 | the snapshot interval, floor 250 ms | 0.8–1.0 ms (macOS 27) | 0.2 % at 500 ms, 0.1 % at 1 s |
| PMGR: CPU residency + energy roll-ups | per-core + 4 (7 on Ultra) | 2 s | 7.2 ms (6.3–8.4) | ~0.36 % |

Until wave 3's perf pass (2026-09-23) there were three subscriptions: GPU
at 250 ms, CPU residency + `GPU Energy` at 1 s and the energy roll-ups at
2 s, 1.2–1.5 % of one core together on the IOReport thread alone. The
per-sample cost is almost all kernel time under `IOReportCreateSamples`
→ `_updateKernelBuffer` → `IOConnectCallMethod` (`sample(1)` stacks), and
it is fixed per provider, not per channel. One run of the scratch
experiment on the Mac17,8 (thread CPU per sample, 100 ms apart):

| Subscription | ms/sample |
|---|---|
| CPU core states only | 7.2 |
| `GPU Energy` only | 1.0 |
| energy roll-ups only | 5.5 |
| CPU states + `GPU Energy` (old slow sub) | 6.3 |
| CPU states + roll-ups (the PMGR sub now) | 8.4 |
| `GPUPH` only | 1.0 |
| `GPUPH` + CPU states + roll-ups | 7.7 |

So the roll-ups ride the CPU sample for free, and the PMGR sample moved
from 1 s to 2 s: cluster MHz (the only thing the CPU window feeds, as the
MHz ratio of the hybrid headline in `cpu::apply_freq`) is a 2 s average,
the busy share still moves every tick, and energy is unchanged because
the PMGR publishes every 2–108 s anyway (held age accurate to ±2 s
against a 10 s stale limit). GPU residency was sampled at 250 ms while
snapshots came every 500 ms, so half the windows were thrown away; it now
follows the snapshot interval (`Sampler::set_interval`, forwarded by the
worker) with a 250 ms floor. Merging `GPUPH` into the PMGR sub as well
would cost ~7.7 ms per sample, so the GPU keeps its own cheap sub.

The floor this leaves is about 0.5 % of one core at 1 s and 0.6 % at
500 ms. Going lower means a slower PMGR cadence (cluster MHz older than
2 s) or no per-core frequency at all.

## 5. Pausing

The sensor thread and the IOReport thread both sleep through a
`pacer::Pacer`. `Sampler::set_paused(true)` (sent by the worker when the
app is frozen) parks both on a blocking receive: no SMC, HID or IOReport
calls while frozen. On resume the schedules restart from now; the energy
holds see the wall-clock gap and rebase instead of averaging across it.

## 6. First reading for headless `--json`

`Snapshot::settled` is true once every slow source this Mac has delivered
its first reading or reported itself absent: the sensor thread's first
frame (`PSTR`, CPU rails, fans, temps) and the IOReport thread's first CPU
window (cluster MHz, GPU watts). The battery collector runs inline from
the start. One-shot `--json` and `--diagnose` re-tick in 100 ms steps until
settled, bounded at 2.5 s including the interval; `--json-stream` never
waits. Energy-counter rails are not waited for (they need two
publications). On the Mac17,8 a one-shot now reports cluster MHz,
`sys_power`, GPU watts and the battery; the CPU rails can still be `null`
on the very first sensor frame because the SMC reports every rail as 0.0
until it populates them (treated as "not yet", never 0 W).

## 7. SMC rail calibration (Mac17,8)

`src/fixtures/rails_calib.txt` records IOReport `CPU Energy` beside the SMC
rails and `PSTR` every ~0.27 s through idle and `load.sh 18`. Over every
PMGR publication span the raw rail sum is within 0.85–1.10× of the IOReport
rate (`rails_track_ioreport_cpu_energy_within_tolerance`), and so is the
displayed 3-read mean over spans ≥ 60 s. The rails lead `PSTR` by up to
30 W for a sweep or two at a load step, so the ceiling is the max `PSTR`
over the same window, not the current one.

## 8. Whole-family coverage

Only the M5 Pro channel list (`src/fixtures/energy_model_channels.tsv`, 364
channels) is captured from hardware. The other rows encode the naming
macmon and asitop document and are pinned as fixture tests
(`power::tests::family_*`, `dvfs::tests::*family*`, `fan::tests::*`); treat
them as the expected naming, to be replaced with captures when we get the
hardware.

| Shape | CPU clusters (DVFS) | Energy roll-ups | Fans | Battery | CPU watts source |
|---|---|---|---|---|---|
| M1/M2/M3 base, Pro, Max | `ECPU`/`PCPU` from pmgr `voltage-states1`/`5` (Hz); Pro/Max `PCPU1` 2nd P cluster | `CPU Energy`, `GPU Energy`, `ANE`/`ANE0`, `DRAM`/`DRAM0` | `FNum` (MBP: 2) | MacBooks | IOReport held |
| M4 base, Pro, Max | same layout, tables in kHz (a >4.29 GHz top state does not fit `u32` Hz) | as above | as above | MacBooks | IOReport held |
| M5 Pro/Max (Mac17,8 captured) | `perf-domains` + `acc-clusters`: `MCPU0` (Super), `PCPU` | captured list | 2 | yes | SMC rails, IOReport fallback |
| Ultra (Studio) | per-die channels (`DIE_<n>_…`) do **not** map to cluster tables: residency and MHz degrade to absent rather than misfile into die 0 | `DIE_<n>_CPU Energy`/`ANE0`/`DRAM0` summed per die; package `GPU Energy` | `FNum` | none → `battery: null` | IOReport held (per-die sum) |
| Fanless MacBook Air | as its chip | as its chip | no `FNum`, no `F<n>Ac` → fans absent, never 0 rpm | yes | IOReport held |
| Mac mini / Studio / iMac | as its chip | as its chip | 1 | no `AppleSmartBattery` → battery absent | IOReport held; `PSTR` still present |
| Unknown future chip | no plausible table → no clusters, no MHz | only exact roll-up names; anything else ignored | as keys say | as registry says | IOReport if names match, else absent |

Known gaps (follow-ups): Ultra per-die DVFS tables; SMC CPU-rail tables for
models other than Mac17,8; confirming M1–M4 and Ultra channel names on
real hardware.
