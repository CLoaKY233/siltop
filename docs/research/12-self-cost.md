# Self-cost: what siltop costs to leave open

| | |
| --- | --- |
| **Status** | Baseline recorded 2026-09-23 (WS-00); re-measured after the wave 1 merge (WS-00, 01, 02, 03, 04, 12), after the wave 3 perf pass the same day, and after the 1.1 adaptive-cadence pass |
| **Tool** | [`scripts/perf-smoke.py`](../../scripts/perf-smoke.py) (pty driver shared with [`scripts/pty-shot.py`](../../scripts/pty-shot.py) via `scripts/ptyterm.py`) |
| **Machine** | MacBook Pro Mac17,8, M5 Pro (6 Super + 12 Performance, 20-core GPU), 24 GB, macOS 27.0, AC power, no sudo |

A system monitor that burns a core is lying about the machine it watches. This page is the method and the numbers: what siltop costs sitting idle in a terminal, what btop and macmon cost on the same machine, and the budget 0.1.0 has to meet. Every workstream that touches the bin or metrics crates re-runs perf-smoke and puts before/after medians in its PR.

---

## Method

`perf-smoke.py BIN` forks the binary under a pty at 160×45 with a fresh temporary `XDG_CONFIG_HOME`, `TERM=xterm-256color`, `COLORTERM=truecolor`, and no `NO_COLOR`. It answers the terminal queries a real emulator would (DSR cursor position, DA1, DECRQM, OSC 10/11 colours), so a query can never stall startup. It waits 5 s, samples every counter, reads the pty for 20 s, samples again, then sends `q` and kills the process if it has not exited about a second later. `--runs 3` repeats that with three fresh processes and reports each run plus the per-metric median. No input is sent unless `--keys` or `--motion` asks for it.

| Metric | Source | Notes |
| --- | --- | --- |
| `draws_per_s` | `ESC[?2026h` markers in the output | Only for programs that wrap frames in synchronized updates. siltop and btop do; macmon does not (reported as 0). |
| `content_draws_per_s` / `idle_draws_per_s` | Frame payload with escape sequences stripped: any printable byte left means the frame painted something | ratatui's empty diff is `ESC[39m ESC[49m ESC[59m ESC[0m ESC[?25l` (25 bytes), so an idle draw is exactly a redraw that changed nothing. |
| `snapshot_period_ms` | Gaps between content frames: n, mean, p50, p90, p99, max | A proxy for the sampler's real period: every snapshot moves a graph or a number, so each one produces exactly one content frame while nothing else is happening. Input or timers also produce content frames, so read it only from runs with no `--keys`/`--motion`. |
| `bytes_per_s`, `bytes_per_draw` | Bytes read from the pty | |
| `read_chunks_per_s` | `read(2)` calls on the pty that returned data | A lower bound on the child's `write(2)` calls: the pty coalesces writes that land between two reads. |
| `cpu_pct` | `proc_pid_rusage(RUSAGE_INFO_V6)` `ri_user_time + ri_system_time` | Mach ticks, converted with `mach_timebase_info` (125/3 on Apple Silicon). % of one core. Falls back to `/proc/PID/stat` or `ps -o time=` off macOS. |
| `threads` | `proc_pidinfo(PROC_PIDLISTTHREADS)` then `PROC_PIDTHREADINFO` per thread | Nanoseconds, with the pthread name (`siltop-sampler`); unnamed threads show as `main` / `thread-N`. More precise than `ps -M`, and keyed by thread handle, so a thread created mid-window still counts. |
| `energy_w` | `ri_energy_nj` delta over wall time | The kernel's per-task energy estimate in nanojoules; readable without sudo for your own processes. |
| `wakeups_per_s` | `ri_interrupt_wkups + ri_pkg_idle_wkups` | `pkg_idle` reads 0 on Apple Silicon; the interrupt count carries the signal. |
| `syscalls_per_s`, `csw_per_s` | `proc_pidinfo(PROC_PIDTASKINFO)` `pti_syscalls_unix + pti_syscalls_mach`, `pti_csw` | |
| `rss_mb`, `footprint_mb` | `ri_resident_size`, `ri_phys_footprint` at the end of the window | Footprint is what Activity Monitor calls Memory. |
| `first_output_ms`, `first_frame_ms` | Spawn to the first byte and to the first `ESC[?2026h` | |

The run also records the host (model, OS, power source), the binary's size and SHA-256 prefix, and the 1-minute load average at the start and after each run. **The test machine was not idle**: other agents were building in parallel worktrees throughout (load average 1.2–3.3 on 18 cores during the session), so treat single digits after the decimal point as noise and compare medians of `--runs 3` taken back to back.

### Reproduce

```sh
cargo build --release -p siltop
python3 scripts/perf-smoke.py --runs 3 --out /tmp/head.json                # default config
python3 scripts/perf-smoke.py --runs 3 --interval-ms 1000                  # writes interval_ms into the temp config
python3 scripts/perf-smoke.py OLD_BIN --runs 3                             # the "before" half of a PR
python3 scripts/perf-smoke.py --runs 1 --motion 50                         # pointer moving at 50 Hz (D02)
python3 scripts/perf-smoke.py /opt/homebrew/bin/btop --runs 3 --xdg-file btop-1s.conf=btop/btop.conf
python3 scripts/perf-smoke.py /opt/homebrew/bin/macmon --runs 3            # macmon's default is -i 1000
```

`btop-1s.conf` is the single line `update_ms = 1000`. `--screen FILE` writes the last measured screen as text, which is how to confirm the tool was actually showing its dashboard and not an error.

---

## Baseline

Binary: `SP/siltop-baseline`, built from `master@7ae9886` (SHA-256 prefix `9940b4831c42c3c6`). The config file holds only `interval_ms`; everything else is the default (Work surface, disk pane off). Medians of three 20 s windows at 160×45, run back to back on 2026-09-23 between 19:07 and 19:12 with the load average at 1.5–2.6. Brackets give the range across the three runs.

| Metric | 250 ms (default today) | 500 ms | 1 s | 250 ms, pointer moving at 50 Hz (1 run) |
| --- | --- | --- | --- | --- |
| `cpu_pct` | **7.06** (6.94–7.38) | **6.06** (5.70–6.24) | **5.09** (5.08–5.34) | **12.49** |
| main thread / `siltop-sampler` | 5.8 / 1.3 | 5.0 / 1.0 | 4.4 / 0.7 | 11.0 / 1.5 |
| `draws_per_s` | 110 (105–116) | 94 (85–95) | 83 (81–83) | 153 |
| `content_draws_per_s` | 3.75 | 1.90 | 0.95 | 3.75 |
| `idle_draws_per_s` | **106** | **92** | **82** | **149** |
| `bytes_per_s` | 21.2 k | 13.4 k | 10.0 k | 21.6 k |
| `read_chunks_per_s` | 299 | 258 | 229 | 416 |
| `energy_w` | **0.276** (0.25–0.30) | 0.211 (0.17–0.24) | **0.181** (0.17–0.19) | 0.331 |
| `wakeups_per_s` | 128 | 109 | 94 | 116 |
| `syscalls_per_s` | 2525 | 2370 | 2273 | 2866 |
| `csw_per_s` | 178 | 158 | 141 | 230 |
| `rss_mb` / `footprint_mb` | 16.6 / 9.9 | 15.9 / 10.1 | 15.6 / 8.8 | 16.3 / 9.5 |
| `first_frame_ms` | 110 | 123 | 150 | 112 |
| `snapshot_period_ms` mean / p50 / p99 / max | 266 / 257 / 348 / 368 | 528 / 509 / 591 / 592 | 1049 / 1049 / 1092 / 1092 | 267 / 257 / 350 / 373 |

The same source built on this branch (`target/release/siltop`, prefix `7d4727efc29c9c90`) measured 9.02% CPU (7.9–9.1), 89 idle draws/s and 0.188 W at 250 ms, and 5.39% CPU (5.2–5.5), 76 idle draws/s and 0.141 W at 1 s, with the load average at 1.5–2.2. The shape is identical; the spread between the two binaries is the machine, which is why a PR compares two binaries measured back to back rather than against this table.

**What the numbers say**

- **Almost every frame is wasted (D01).** 97–99% of draws paint nothing: content frames arrive exactly once per snapshot (3.75/s at 250 ms, 0.95/s at 1 s) and the rest, 82–106 per second, are empty diffs. The main (UI) thread is 82–86% of the process's CPU; the sampler thread is 0.7% at 1 s, 1.0% at 500 ms and 1.3% at 250 ms. Slowing the interval helps little because the idle redraw rate hardly moves with it.
- **Energy and wakeups follow the redraws, not the interval.** 0.18–0.28 W and 94–128 wakeups/s. btop at its default interval spends 0.011 W and 1.7 wakeups/s.
- **Pointer motion costs another 5.4 points of CPU (D02).** 50 motion events per second added 43 empty draws per second.
- **Output reaches the terminal in small pieces (D07).** 229 read chunks per second against 83 draws at 1 s, about 2.8 per draw, and that is a lower bound on the writes behind them.
- **Snapshots arrive late and stretch (D03, D04).** The mean period is 6.5% long at 250 ms, 5.5% at 500 ms and 4.9% at 1 s. p99 sits 90–100 ms above the interval: that is the 2 s sensor sweep blocking the cheap tick.
- **Memory is already fine.** 15.6–16.6 MB RSS and about 10 MB footprint, with about 840 processes on the machine.

Raw JSON for every run: `SP/c-ws00/perf/*.json` (`SP` was a local scratch directory; not in the repo).


### After wave 1

Binary: `target/release/siltop` from `master` after the wave 1 merge (WS-00, WS-01, WS-02, WS-03, WS-04 and WS-12; SHA-256 prefix `6a3b9711aec96a13`). Same method: medians of three 20 s windows at 160×45 with the default config, run back to back at 20:00–20:06 with the load average at 1.2–2.4. The frozen column is one 10 s window after pressing `f`.

| Metric | 500 ms (default) | 1 s | 2 s | frozen |
| --- | --- | --- | --- | --- |
| `cpu_pct` | **0.86** (0.86–0.87) | **0.57** (0.57–0.60) | **0.40** (0.38–0.42) | 0.10 |
| `siltop-sampler` / main / `siltop-sensors` | 0.57 / 0.20 / 0.09 | 0.42 / 0.09 / 0.09 | 0.29 / 0.04 / 0.09 | 0 / 0 / 0.10 |
| `draws_per_s` (all content) | 2.0 | 1.0 | 0.5 | 0 |
| `idle_draws_per_s` | **0** | **0** | **0** | 0 |
| `bytes_per_s` | 6.4 k | 4.0 k | 2.2 k | 0 |
| `read_chunks_per_s` | 7.2 | 4.6 | 2.4 | 0 |
| `energy_w` | 0.004 | 0.004 | 0.003 | 0.000 |
| `wakeups_per_s` | 2.5 | 1.6 | 1.1 | 0.7 |
| `syscalls_per_s` / `csw_per_s` | 1319 / 47 | 1135 / 43 | 875 / 41 | 37 / 38 |
| `rss_mb` / `footprint_mb` | 13.0 / 7.4 | 12.5 / 7.4 | 12.4 / 6.9 | 10.6 / 5.7 |
| `first_frame_ms` | 124 | 123 | 120 | 114 |
| `snapshot_period_ms` mean / p50 / p99 / max | 500 / 500 / 509 / 510 | 1000 / 1001 / 1011 / 1011 | 1999 / 1999 / 2009 / 2009 | n/a |

- Every frame paints something, one per snapshot. The main thread went from 82–86% of the process to about a quarter, and the sampler thread is now the largest cost.
- p99 now sits 9–11 ms over the interval, down from 90–100 ms: the sensor sweep runs on its own thread (WS-04) and the worker keeps a fixed-rate schedule (WS-01).
- Freezing stops the draws and the sampler tick, but not the 2 s sensor thread, which costs 0.10% and 0.7 wakeups/s while frozen. Pausing it with the worker is still to do.
- 2 s is now measurable because WS-01's config accepts 2000 and 5000 ms.
- `read_chunks_per_s` is no longer a write count: a chunk now averages about 880 bytes against 44 in the baseline, which fits one write per frame split by the macOS pty's buffer of about 1 KiB. Frames over 1 KiB therefore read as several chunks, so the write-calls gate needs a syscall trace, not this key.

Raw JSON: `SP/work/merge-wave1/perf-{default-clean,1s,2s,frozen}.json`.

### Wave 3 regression and the perf pass

Waves 2–3 added the `IOReport` thread (frequency and energy) and per-process
rusage, energy and GPU columns; idle self-CPU at 500 ms went from 0.86% to
2.3–2.5%. `sample(1)` on the running binary and per-collector thread-CPU
timers put the cost in two places, both mostly kernel time:

- **`siltop-ioreport`, 1.3–1.6%.** Every `IOReportCreateSamples` is a
  fixed-cost kernel buffer update per provider: PMGR samples cost 5.5–8.4 ms
  however few channels they keep, `GPUPH` about 1 ms. The thread took a CPU
  sample every 1 s, a separate energy sample every 2 s and a GPU sample every
  250 ms. Details and the per-subscription table:
  [`13-ioreport-and-power.md`](13-ioreport-and-power.md) §4.
- **The process table on `siltop-sampler`, ~5–6 ms per 1 s refresh.** For
  about 820 pids: `proc_pid_rusage` v6 plus `PROC_PIDTASKINFO` each (1–2 µs
  per call, 1,650 calls), a `KERN_PROC_ALL` sweep (~1 ms), the GPU-client
  registry walk every 2 s (~2.7 ms), and row building. About 310 of the
  pids belong to other users, so both calls fail with `EPERM` every second.

What changed:

| Change | Where | Saves |
| --- | --- | --- |
| CPU residency and the energy roll-ups share one PMGR subscription, sampled every 2 s (first window 1 s, so the first snapshot settles as fast as before) | `ioreport.rs` (`pmgr_subscription`, `Gate`) | ~0.6% |
| GPU residency follows the snapshot interval (floor 250 ms) instead of a fixed 250 ms | `ioreport.rs` (`gpu_period_for`), `sampler.rs`, `worker.rs` | ~0.2–0.3% |
| `PROC_PIDTASKINFO` is skipped when rusage shows the CPU time did not move: a thread count only changes when a thread runs | `process/mod.rs` (`unchanged_threads`) | ~500 calls/s |
| ~~A pid whose counters did not move between its last two reads is read again every 3 s (`IDLE_REREAD`)~~ Removed in 1.1: an idle pid that started pegging a core read 0% for 3.5–4 s. Every readable pid is read each refresh again; `unchanged_threads` still skips its task-info call | `process/mod.rs` (`held_readings`) | ~250 calls/s given back (~0.4 ms CPU/s) |
| A pid the kernel refused every call for is asked again every 10 s (`DENIED_REREAD`); its row is restricted either way (D23) | `process/mod.rs` | ~600 failing calls/s |

Every hold is dropped on pid reuse (start time) or `exec()` (`p_comm`).
What the UI loses: cluster MHz is a 2 s average instead of 1 s. (The
idle hold also made a long-idle process that woke show 0% for up to 3 s;
that hold is gone.) Nothing becomes absent or zero that was not.

Back to back on 2026-09-23 22:09–22:14, load average 1.7–3.8 (other agents
building), 160×45, medians of `--runs 3`. Before: `dev/v0.1.0@275e0f4`
(SHA-256 prefix `18b90c6e28f3a997`); after: this pass (`c1ed16d6e0f21e8b`).

| Metric | 500 ms before | 500 ms after | 1 s before | 1 s after |
| --- | --- | --- | --- | --- |
| `cpu_pct` | **2.33** (2.33–2.75) | **1.48** (1.41–1.59) | **2.15** (2.11–2.37) | **1.01** (1.01–1.07) |
| `siltop-ioreport` | 1.34–1.52 | 0.55–0.63 | 1.43–1.59 | 0.51–0.54 |
| `siltop-sampler` | 0.70–0.88 | 0.55–0.63 | 0.50–0.61 | 0.32–0.35 |
| main / `siltop-sensors` | 0.19 / 0.10 | 0.21 / 0.11 | 0.08 / 0.10 | 0.08 / 0.10 |
| `energy_w` | 0.013 | 0.007 | 0.011 | 0.006 |
| `wakeups_per_s` | 6.4 | 4.7 | 7.1 | 2.5 |
| `syscalls_per_s` | 1544 | 581 | 1206 | 474 |

Not yet at the brief's ≤1.0% at 500 ms and ≤0.7% at 1 s. What is left, at
1 s: `IOReport` ~0.5% (one PMGR sample per 2 s is ~0.36% on its own), the
process table ~0.25%, the other collectors and the snapshot clone ~0.1%,
UI and sensors ~0.2%. The next levers, each with a visible cost: a 4 s PMGR
cadence (cluster MHz older), skipping the GPU-client walk while the GPU is
idle, or sending processes to the app only when they were refreshed (the
snapshot clone copies every row's argv twice a second; needs an app change).

Measure with `scripts/perf-smoke.py` as in [Reproduce](#reproduce);
per-subscription costs with `cargo test -p siltop-metrics --release --
--ignored --nocapture ioreport_cost`.

### 1.1: adaptive cadences and low-power mode

The 1.0 profile left `IOReport` as the largest thread. Most of its samples
read nothing new on an idle Mac: idle cluster clocks dither, and GPUPH on
an idle desktop sits at 2–5% (the window compositor). Every rule below is a
pure function with its own tests in `ioreport.rs`, `cadence.rs`,
`process/` or `worker.rs`:

| Rule | Where | Effect |
| --- | --- | --- |
| PMGR runs every 4 s (`PMGR_STABLE_PERIOD`) while two windows in a row agree (`clocks_stable`: every cluster either under 15% active on both sides, or within 10% of its table max in MHz and 10 points in load), and 2 s otherwise | `ioreport.rs` | The fixed ~8 ms PMGR cost halves on an idle Mac |
| PMGR samples at the next wake when the cheap tick's CPU load moved by 15 points (`LOAD_JUMP`, `pmgr_urgent`), no sooner than 1 s after the last one | `ioreport.rs`, `cpu.rs` (`note_cpu_load`) | A burst still shows real MHz within one wake |
| GPUPH drops to every 2 s (`GPU_IDLE_PERIOD`) while GPUPH and the `IOAccelerator` utilization, read every snapshot anyway, are both under 8% (`gpu_busy`); unknown counts as busy | `ioreport.rs`, `gpu.rs` (`note_gpu_busy`) | ~0.15% at 500 ms |
| The thread sleeps until the earlier of the two `Gate`s (`Gate::remaining`) instead of riding GPU wakes, so a slow GPU cadence never delays the 1 s first PMGR window | `ioreport.rs` | `settled` timing unchanged |
| The per-process GPU-client registry walk (~2.7 ms) runs every 6 s instead of 2 s while the GPU is idle (`GPU_IDLE_EVERY`); shares from the last walk stand in between | `process/gpu_clients.rs` | ~0.1% |
| On battery (`external_connected == Some(false)`; desktops and unknown never), the worker ticks at 2 s (or the user's slower interval), the process table refreshes every 4 s (every other 2 s tick; a 3 s period landed on 4 s anyway) and the SMC sweep every 10 s; stale-sensor ageing follows the stretched period. Back on the charger everything returns at the next snapshot | `cadence.rs`, `worker.rs` (`PowerFollow`), `sensors.rs`, `sampler.rs` | Low-power mode is automatic |

What the UI gives up: cluster MHz can be up to 4 s old while it is not
moving (a jump is resampled at once); an idle GPU's figure and its
per-process shares are up to 2 s and 6 s old. Nothing becomes absent or
zero that was not.

Back to back on 2026-09-23 23:43–00:03, load average 2–4 (other agents
building in parallel worktrees, so single runs swing by ±0.3 points;
compare medians). Before: `dev/1.1@a5b5d4a`; after: `wave5/perf2`.

| Metric | 500 ms before | 500 ms after | 1 s before | 1 s after |
| --- | --- | --- | --- | --- |
| `cpu_pct` median (runs) | **1.40** (1.12–1.47), again **1.65** (1.14–1.77) | **0.86** (0.79–0.97) | **0.88** (0.79–0.88), again **1.19** (1.02–1.26) | **0.70** (0.65–0.85)¹, again **0.98** (0.88–1.09) |
| `siltop-ioreport` | 0.52–0.72 | 0.34–0.47 | 0.36–0.51 | 0.30–0.40 |
| `siltop-sampler` | 0.37–0.66 | 0.27–0.30 | 0.44–0.48 | 0.36–0.45 |
| main / `siltop-sensors` | 0.14–0.28 / 0.08–0.12 | 0.11–0.14 / 0.07–0.09 | 0.12–0.15 / 0.10–0.11 | 0.11–0.14 / 0.10–0.11 |
| `wakeups_per_s` | 4.6 | 3.8 | 2.5 | 2.5 |
| `snapshot_period_ms.p99` | 509 | 508 | 1014 | 1011 |

¹ The first 1 s pair ran in a quieter minute, with the binary before
the quiet-cluster rule; the second pair (thread ranges above) ran while
three other builds were going, and the before binary rose from 0.88 to
1.19 with it. Per-thread figures are from the second pair.

The 500 ms gate (≤1.0%) is met with room in both pairs. The 1 s gate
(≤0.7%) was met in the quieter pair and missed in the busy one; both show
the same 0.2-point drop. On a busy machine the load-jump rule fires more
often, which is the point, and the sampler thread (process table plus the
snapshot clone) is now as large as `IOReport`: that is the next target.
Next levers, not taken: skip PMGR entirely while no visible view shows MHz
or watts (needs the app to report its visible panes to the worker), and
send processes to the app only on refresh ticks (the snapshot clone copies
every row twice a second; an app-side change).

#### Soak

`scripts/soak.py BIN --minutes 15` runs the binary under the same pty,
records RSS, open descriptors (`lsof`) and threads after a warm-up, then
every 30 s, and fails on RSS growth over 5 MB or any fd or thread growth.
15 minutes at the default 500 ms on this branch (the build before the
quiet-cluster rule), sampled every 30 s after a 30 s warm-up: fds flat at
11; RSS 18.0 → 22.5 MB (+4.4 MB, under the 5 MB gate, most of it in the
first 3 minutes: 18.0 → 21.0, then about 0.12 MB a minute); threads 6 → 7,
one thread that appeared once between 5 and 5.5 minutes and stayed, so
the script failed the run on its zero-growth thread rule. Not an unbounded
leak, but not yet explained either: name it (`perf-smoke` prints thread
names) and check whether the late RSS slope flattens over an hour before
calling the soak gate met.


### 1.1: what the UI needs, and the deep PMGR cadence

The PMGR sample (CPU residency plus the energy rails, a fixed 6–8 ms of
mostly kernel CPU) is still the largest single cost. Two rules, each a
pure function with tests in `ioreport.rs`, `needs.rs`, `pacer.rs`,
`worker.rs` and `app.rs`:

| Rule | Where | Effect |
| --- | --- | --- |
| After `DEEP_AFTER` (3) agreeing windows in a row, PMGR runs every 8 s (`PMGR_DEEP_PERIOD`); one moving window drops it back to 2 s, and a 15-point load swing still samples at the next wake. `MAX_AGE` is now 10 s so a held reading never expires between two deep samples; the energy bank's sleep rule (3 × 4 s) stays above a deep gap | `ioreport.rs` (`pmgr_cadence`, `next_streak`) | Halves PMGR on an idle or pegged Mac. A busy, moving machine gets nothing from it |
| The app sends a `Needs` bit set (`CPU_CLOCKS`, `GPU_CLOCKS`, `POWER`) over the worker's command channel when it changes. Full-screen pages (settings, the timeline) send `GPU_CLOCKS` only. With CPU clocks and watts parked, PMGR samples until the clocks settle and then stops, waking once a second only to check for a load swing (`pmgr_plan`) | `app.rs` (`wanted_needs`), `worker.rs` (`Cmd::Needs`), `ioreport.rs`, `pacer.rs` (`Poke`) | No PMGR sample at all on a steady machine while a page covers the dashboard |

Why parked is "settle, then stop" rather than "never": the CPU history
keeps recording while a page is open, and its headline is Mach busy scaled
by the cluster clocks. Held clocks are right only while the load holds
still, so a swing resamples until the clocks settle again. GPU residency
is never parked by the app: it *is* the GPU figure the history records,
and a held one would draw a flat line for the whole visit (it costs about
1 ms per window). Parked watts are not held: they go absent after
`MAX_AGE`, a gap in the rail history, never a frozen value. Re-enabling a
source restamps its held reading as of now, so the first frame back shows
the last value instead of an absence, and pokes the `IOReport` thread
awake (`Ctl::Poke`, which never unpauses a frozen thread). Measured by
screenshot: closing settings after 14 s shows clocks and rail watts on the
very first frame (`needs-back-immediately.png`).

Found on the way: `Box<dyn Tick>` (`serve`, `record`, headless) did not
forward `set_low_power`, so on battery those modes stretched the tick but
never slowed the collectors. Fixed, with a regression test.

Profile first (`sample` for 26 s at 1 s, a symbolized build): main thread
about 12 ms in 13 s (0.06–0.07% in perf-smoke), mostly allocation and
`memmove` spread across draw code. Caching formatted process rows or
titles cannot win more than a few hundredths of a point, so it was not
done. The sampler thread's time is the process table: `proc_pidinfo`
(~15 of 45 samples), the GPU-client registry walk (~12, every 2 s while
the GPU is busy) and `kinfo` sysctls.

Back to back on 2026-09-24 00:55–01:00, `--runs 3`, 160×45, load average
6–7 (1 min) with other agents building and a stress load starting, and one
`cargo test` build overlapping the 1 s "after" runs. Before:
`dev/1.1@7490a3c`; after: `wave6/perf3` (deep cadence; the dashboard
needs everything, so `Needs` does not change these columns).

| Metric | 1 s before | 1 s after | 500 ms before | 500 ms after |
| --- | --- | --- | --- | --- |
| `cpu_pct` median (runs) | **0.69** (0.64–0.88) | **0.71** (0.57–0.73) | **0.82** (0.77–0.85) | **0.82** (0.75–0.94) |
| `siltop-ioreport` | 0.28–0.31 | 0.24–0.38 | 0.30–0.39 | 0.17–0.29 |
| `siltop-sampler` | 0.22–0.40 | 0.19–0.34 | 0.27–0.30 | 0.28–0.48 |
| main / `siltop-sensors` | 0.06–0.08 / 0.07–0.09 | 0.05–0.07 / 0.06–0.08 | 0.10–0.12 / 0.06–0.08 | 0.12–0.19 / 0.08–0.10 |
| `wakeups_per_s` | 2.75 | 2.70 | 4.8 | 3.75 |
| `snapshot_period_ms.p99` | 1007 | 1008 | 507 | 509 |

The 1 s ≤0.5% and 500 ms ≤0.6% targets were **not met** on this machine
today: with clocks moving (a busy Mac never builds a stable streak) PMGR
runs every 2 s whatever the rule, which alone is 0.3–0.4% of a core. The
500 ms `IOReport` thread dropped by about a third and wakeups by one a
second; everything else is inside run-to-run noise. A measurement with the
settings page open (`--keys s`) did not finish: the load average reached
109 while it ran. Next levers: a cheaper per-client GPU walk (two
registry reads per client today) and sending the process table to the app
only on refresh ticks.

#### Soak (wave 6)

`scripts/soak.py BIN --minutes 15 --warmup 30` on the `wave6/perf3`
binary at the default 500 ms, 00:55–01:10 on 2026-09-24, load average
rising from 6 to over 100 (another agent's stress load) in the last
minutes. fds flat at 11. RSS 19.5 → 22.9 MB (+3.45 MB, under the 5 MB
gate): 19.5 → 22.6 MB in the first 3 minutes, then 22.6 → 22.9 MB over
the remaining 12 (about 0.03 MB a minute, a quarter of the slope the last
soak saw), peak 23.7 MB. Threads 6 → 7 again: one thread appeared between
6 and 6.5 minutes and stayed, so the script failed the run on its
zero-growth thread rule, the same shape as the wave 5 soak (5–5.5
minutes). One late, single, lasting thread looks like a lazily started
system or store thread rather than a leak; naming it (perf-smoke's thread
list after 7 minutes) is still open.

---

## Competitors

Same method, same terminal size, measured right after the baseline (19:12–19:16, load average 1.2–1.9). btop 1.4.7 and macmon 0.8.2 from Homebrew, each with a fresh temporary `XDG_CONFIG_HOME`, so every tool ran with its defaults except where the column says otherwise. mactop, bottom (`btm`) and zenith are not installed on the test machine and were not installed for this, so they are not in the table; the master plan's reference figures for them (for example mactop at 0.5–1 GB RSS) come from the competitor study, not from this method.

| Metric | siltop 1 s (baseline) | btop, 2 s (default) | btop, 1 s | macmon, 1 s (default) |
| --- | --- | --- | --- | --- |
| `cpu_pct` | 5.09 | **1.30** (1.28–1.42) | 2.28 (2.14–2.44) | **1.47** (1.33–1.47) |
| busiest thread | main 4.4 | one worker thread, 1.3 | one worker thread, 2.2 | sampler 1.1–1.2 (main 0.25–0.29) |
| `energy_w` | 0.181 | **0.011** | 0.028 | **0.005** |
| `wakeups_per_s` | 94 | 1.7 | 2.0 | 5.4 |
| `draws_per_s` | 83 | 1.5 | 2.0 | not measurable: no synchronized updates |
| `bytes_per_s` | 10.0 k | 15.5 k | 31.3 k | 1.8 k |
| `read_chunks_per_s` | 229 | 16 | 32 | 6 |
| `syscalls_per_s` | 2273 | 816 | 1630 | 125 |
| `csw_per_s` | 141 | 142 | 281 | 96 |
| `rss_mb` / `footprint_mb` | 15.6 / 8.8 | 14.8 / 9.0 | 14.3 / 8.5 | 18.9 / 13.0 |
| `first_frame_ms` | 150 | 194 | 227 | n/a |

Notes:

- btop repaints once per interval plus a clock tick every second and never draws a frame that changes nothing (`idle_draws_per_s` 0). That is the behaviour WS-01 is aiming for.
- macmon's cost is almost entirely its sampling thread, which reads IOReport every second. That thread alone (1.1–1.2% here) is a useful yardstick for WS-05: IOReport sampling at 1 s is not free, and the §6.1 gate of ≤1.0% at 1 s with power and frequency on has to leave room for it. The competitor study recorded macmon at 0.6–0.9%; this run, on a busier machine and a larger window, measured 1.3–1.5%.
- Syscall counts do not track cost: btop makes 816–1630 syscalls per second at 1.3–2.3% CPU. Budget CPU, energy and wakeups, not syscalls.

---

## 1.2.0, against btop and macmon (2026-10-16)

Release binary of 1.2.0 (SHA-256 prefix `71c76782811cb0d3`, 3.3 MB), btop and macmon from
Homebrew, each with a fresh temporary `XDG_CONFIG_HOME`. Medians of three 20 s windows at
160×45, back to back, MacBook Pro Mac17,8 (M5 Pro), macOS 27.0.1, AC power, load average
2.0–2.5. `ptyterm.child_env` now sets `SILTOP_NO_ONBOARDING=1`: since 1.1 a fresh config
parks the binary on the first-run screens, which measured as 0% and drew nothing.

| Metric | siltop 1 s | siltop 500 ms (default) | btop 1 s | btop 2 s (default) | macmon 1 s (default) |
| --- | --- | --- | --- | --- | --- |
| `cpu_pct` | **1.34** | 1.70 | **2.49** | 1.31 | **1.42** |
| `energy_w` | 0.006 | 0.007 | 0.024 | 0.010 | 0.005 |
| `wakeups_per_s` | 2.75 | 4.75 | 2.05 | 1.5 | 5.05 |
| `draws_per_s` (idle draws) | 1.0 (0) | 2.05 (0) | 2.0 (0) | 1.5 (0) | n/a |
| `footprint_mb` | 10.9 | 12.0 | 9.5 | 9.2 | 13.3 |

At the same 1 s rate siltop costs about half of btop's CPU and a quarter of its energy,
and about the same as macmon, while sampling power rails, cluster clocks, SMC sensors
and every process.

## Budget for 0.1.0

§6.1 of the master plan, expressed as perf-smoke keys. Measure at 160×45 with `--runs 3` and read the median.

| Gate | perf-smoke key | Baseline (median) | After wave 1 | 0.1.0 |
| --- | --- | --- | --- | --- |
| Self CPU at the default interval | `cpu_pct` | 7.06% at 250 ms | 0.86% at 500 ms | ≤1.5% at 500 ms |
| Self CPU at 1 s, power and frequency on | `cpu_pct` with `--interval-ms 1000` | 5.09% | 0.57% (power and frequency not sampled yet) | ≤1.0% |
| Self CPU in low-power mode (2 s) | `cpu_pct` | not measurable yet: today's config maps 2000 ms to 1000 ms | 0.40% | ≤0.6% |
| Idle draws | `idle_draws_per_s` (and `draws_per_s` against `content_draws_per_s`) | 106 / 92 / 82 | 0 / 0 / 0 | about 0: one draw per snapshot, plus input |
| Self energy | `energy_w` at 1 s | 0.181 W | 0.004 W | ≤0.03 W (≤0.02 W low-power) |
| Wakeups | `wakeups_per_s` at 1 s | 94 | 1.6 | ≤10 |
| Terminal write calls | `read_chunks_per_s` against `draws_per_s` | 229 against 83 at 1 s | 4.6 against 1.0 at 1 s | ≤ draws/s |
| RSS with about 900 processes | `rss_mb` | 15.6–16.6 MB | 12.4–13.0 MB | ≤25 MB |
| Snapshot period p99 | `snapshot_period_ms.p99` | 348 / 591 / 1092 ms | 509 / 1011 / 2009 ms | ≤ interval + 15 ms |
| First frame | `first_frame_ms` | 110–150 ms | 120–124 ms | ≤150 ms |

The same gates as a tripwire (exit status 1 when any check fails):

```sh
# default interval (500 ms from WS-01 on)
python3 scripts/perf-smoke.py --runs 3 --interval-ms 500 \
  --assert 'cpu_pct<=1.5' --assert 'idle_draws_per_s<=0.5' --assert 'rss_mb<=25' \
  --assert 'snapshot_period_ms.p99<=515' --assert 'first_frame_ms<=150'
# 1 s with power and frequency on
python3 scripts/perf-smoke.py --runs 3 --interval-ms 1000 \
  --assert 'cpu_pct<=1.0' --assert 'energy_w<=0.03' --assert 'wakeups_per_s<=10' \
  --assert 'snapshot_period_ms.p99<=1015'
```

`--interval-ms` writes `interval_ms = N`, the key `siltop-core/src/config.rs` reads today. If WS-10 renames it, update `write_config` in `perf-smoke.py` in the same commit, or pass a whole file with `--config`.
