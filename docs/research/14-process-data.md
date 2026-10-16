# 14 · Process data without sudo

What the process collector (`crates/siltop-metrics/src/process/`) reads, from which API, and what it can't read. Measured on a Mac17,8 (M5 Pro, 18 logical CPUs) running macOS 27, as an ordinary user.

## Sources, per sample (1 s)

| Data | API | Readable for other users' pids? |
|---|---|---|
| pid, ppid, **uid**, start time (µs), `p_stat`, `comm` | one `sysctl {CTL_KERN, KERN_PROC, KERN_PROC_ALL}` → `struct kinfo_proc` (648 B) | yes, all of them, including pid 0 |
| threads, CPU ticks, RSS | `proc_pidinfo(PROC_PIDTASKINFO)` | no: `EPERM` |
| CPU time, **footprint**, RSS, energy (nJ), disk read and written bytes, wakeups, children's CPU | `proc_pid_rusage(RUSAGE_INFO_V6)` | no: `EPERM` |
| argv (capped at 4 KiB), exec path | `sysctl KERN_PROCARGS2`, `proc_pidpath` | no; fetched only when the identity changes |
| per-process GPU | `IOAccelerator` children, `IOUserClientCreator` + `AppUsage[].accumulatedGPUTime`, every 2 s | yes (the registry is world-readable) |

`kinfo_proc` offsets used (LP64): `p_starttime` 0 (sec) and 8 (µs), `p_stat` 36, `p_pid` 40, `p_comm` 243 (17 B), `e_ucred.cr_uid` 420, `e_ppid` 560. libc 0.2.189 doesn't declare `kinfo_proc`, so a test reads `KERN_PROC_PID` for the test process and checks the record is 648 bytes and matches `getpid`, `geteuid` and `getppid`. libc also stops at `rusage_info_v4`; `RusageInfoV6` is declared locally (464 B, which is checked by a compile-time assert and by offset tests against the v4 prefix). On the macOS 27 SDK, v6 renames part of the reserved tail (`ri_neural_footprint`, `ri_conclave_footprint`, …) but keeps the size and every field we read.

## Truths learned

- **EPERM is common.** In a live sweep 295 of 837 pids (35%) refuse both task info and rusage. That includes WindowServer, launchd and kernel_task. These rows now carry `restricted: true` and `None` for CPU, memory and threads. The UI shows a dim `—` and sorts them last in both directions. They used to show a made-up `0.0 / 0B`.
- **Footprint matches `top`.** `ri_phys_footprint` matched `top -l 1 -stats pid,mem` within 5% for 91 of 91 of the user's processes over 20 MiB, most within ±1%. RSS (`pti_resident_size`) disagrees in both directions, so it is kept only as `rss_bytes` for the dossier.
- **`p_stat` values are 1 SIDL, 2 SRUN, 3 SSLEEP, 4 SSTOP, 5 SZOMB.** The old code mapped 5 to "stopped" and 6 to "zombie", which was wrong. Almost every process reads SRUN, so only `stopped` and `zombie` are shown. Running and sleeping would need a per-thread query.
- **`exec()` keeps pid and start time but changes `p_comm`.** Caching `comm` per pid and resolving the identity again when it changes renames `bash -c 'sleep 2; exec yes'` within one sample. CPU-time counters carry across an exec, so the CPU delta stays continuous.
- **First-seen processes** get `cpu_ns / age` (start time from kinfo, at µs precision), capped at `100 × ncpu`. A 300 ms-old `rustc` that has used 290 ms reads about 97 instead of a baseline 0. On the collector's first (baseline) pass, CPU is `None`.
- **CPU is per-core percent** (C04): 100 means one busy core, and `Snapshot::process_ncpu` carries the ceiling. `Snapshot::unattributed_cpu` is `active × ncpu × 100 − Σ readable rows`, floored at 0. It covers restricted pids, the kernel, and processes that came and went between samples.
- **GPU clients.** `IOUserClientCreator` is the string `"pid N, name"`. `accumulatedGPUTime` is cumulative per `AppUsage` entry, so the share is Δ(Σ)/wall time, capped at 100. The walk takes well under 50 ms. The Metal-load accuracy check (within 10 points of device utilisation) has not been run yet.

## Display names from argv

`argv::display_name` is used before the executable basename:
python, node/bun/deno, ruby, perl, and the shells show the script (skipping options, including those that take a value) or `-m module`. `-c`/`-e` inline code keeps the tool name. rustc and clippy-driver show `rustc <crate-name>`. swift-frontend shows `swift-frontend <primary-file>`. `java -jar x.jar` shows `x.jar`, and a plain main class shows its last segment. A table test covers 32 argv fixtures, some captured on this machine.
