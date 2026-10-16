# 17 — History store

Status: foundation landed on `wave4/history` (2026-09-23). No UI beyond the
settings → About line yet; scrub/zoom and the blame timeline build on this.

## Shape

| Piece | Where | What |
|---|---|---|
| `Series` | `core/src/store/mod.rs` | 20 stored columns: v1's 14 (cpu, gpu, mem, swap, net rx/tx, disk r/w, cpu/gpu °C, system/cpu/gpu W, fastest fan) then v2's 6 (battery %, battery W signed + charging / − draining, on-AC 1/0, thermal state 0–3, throttled 1/0, battery health %). Order = on-disk column order: **append only** (`Series::V1_COUNT` marks the v1 cut). |
| `Bucket` | `core/src/store/bucket.rs` | `t` (unix s, aligned to tier span), `samples`, per-series `Option<Stat{mean,max}>`, top 5 processes by CPU, energy (W) and memory (footprint bytes), and (v2) the exact per-app energy list: top `APP_N` = 8 apps by mean W plus an `apps_other` row. |
| `Accumulator` | `bucket.rs` | Folds each snapshot into the open 6 s bucket; returns the closed bucket once a snapshot lands in the next span. |
| `Tier`, `merge`, `downsample`, `Rollup` | `core/src/store/tier.rs` | Pure maths between resolutions. |
| `HistoryStore` trait, `MemoryStore` | `store/mod.rs` | `append(bucket)`, `buckets(from, to, tier)`, `range(series, from, to, tier)`. Memory store = the fallback when the disk store cannot open. |
| `DiskStore`, `Recorder` | `bin/src/store_disk.rs` | Segment files; the main-loop writer. |

The core module never reads a clock: every call takes unix seconds, so tests
drive time with plain integers (retention is measured against the newest
appended bucket, not the wall clock).

## Tiers

| Tier | Bucket | Keeps | Held by |
|---|---|---|---|
| Live | 1 s (the cheap cadence) | 15 min | the in-app `History` rings (not the store) |
| Day | 6 s | 24 h | store; what the writer appends |
| Month | 144 s (= 24 × 6 s) | 30 d | store; rolled up from Day |

Queries at `Live` answer from Day. A Month query includes the still-open span,
rolled up on read from the pending Day buckets.

### Downsampling maths

- **Mean**: weighted by each fine bucket's `samples`, and only over buckets
  where the series was present (an absent stretch does not drag it to 0).
- **Max**: max of the fine maxes: downsampling never invents a peak.
- **Top N**: each entry's value × its bucket's samples, summed per
  `(pid, name)`, divided by the coarse bucket's total samples, re-ranked, top
  5. A process present for one sample in six counts a sixth: a brief spike does
  not outrank a steady hog. A reused pid under a new name is a new process.
- Per snapshot only the leading `2 × TOP_N` rows per metric compete, so the
  per-tick cost is one partial sort, and a steady sixth-place process can still
  win the bucket.

### Absent is not zero

Series read `None` for hardware the Mac lacks (fanless Air: no fan; desktop:
no battery rail; no GPU reading) and for rates before the snapshot is primed.
On disk an absent value is NaN; a real 0 stays 0. Both survive the round trip.

## On-disk format (version 2; version 1 still read)

```
$XDG_STATE_HOME/siltop/history/      (else ~/.local/state/siltop/history/)
  day/<segment start>.seg                 6 s records, one file per 4 h
  month/<segment start>.seg               144 s records, one file per 3 days
```

Header, 16 bytes: `PLTYHST\0`, version `u16`, tier id `u8`, series count `u8`,
record length `u32` (all little-endian).

Record, fixed `RECORD_LEN` = 732 bytes (v2; v1 was 488):

```
v1: t u64 | samples u32 | 14 × (mean f32, max f32) | 3 lists × 5 × (pid u32, value f32, name [u8;16]) | crc32 u32
v2: t u64 | samples u32 | 20 × (mean f32, max f32) | 3 lists × 5 × (pid u32, value f32, name [u8;16])
    | 8 × (processes u32, watts f32, name [u8;16]) | other watts f32 | crc32 u32
```

- `Layout { version, series, apps }` in `store_disk.rs` is the one
  description of both; `encode_as`/`decode_as` take it, so v1 and v2 share a
  single encoder/decoder. `Layout::of(header, tier)` picks the reader.
- Upgrade: 1.0 history keeps loading (series v1 lacks read `None`, never 0;
  apps read empty with `apps_other = None`, so reports fall back to v1's top-5
  lists for those buckets). The segment open for append at upgrade time is a
  v1 file: before the first v2 append the writer rewrites it in v2 via a
  `.upgrade` temp file renamed over it (a crash mid-way leaves the v1 file
  whole). Nothing is set aside as `.bad`.
- Fixture: `bin/src/fixtures/history-v1-day.seg`, three v1 records written by
  an independent Python encoder (struct + zlib.crc32); the Rust v1 encoder
  reproduces it byte for byte.

- Names are clipped to 16 bytes on a UTF-8 char boundary, NUL-padded.
- Empty top slots have value NaN.
- CRC-32 is the IEEE/zlib polynomial over every byte before it (check value
  `crc32("123456789") = 0xCBF43926`), table built at compile time: no new dep.

### Crash safety

- Each record is one `write_all` to an `O_APPEND` file. A crash can tear only
  the last record.
- Readers use `chunks_exact`: the torn remainder is ignored; a record whose CRC
  fails is skipped alone and its neighbours stand (every single-byte flip is
  caught, tested exhaustively over a record).
- Before appending, the writer truncates a torn tail back to the last whole
  record so later records stay aligned.
- A file whose header matches no known layout (a future version, another
  tier, junk) is renamed `.bad` and a fresh segment started; nothing foreign
  is appended to.
- On open, the Month span still in progress is rebuilt from the newest Day
  segment, so a restart mid-span loses nothing.
- Dropping the `Recorder` (quit) closes and appends the partial bucket.

No `fsync` per record: the OS flushes; the worst case after a power cut is the
last few seconds, which the CRC/torn-tail rules already tolerate.

### Retention, rotation, size cap

- Rotation: a record whose `t` falls outside the open segment's span opens the
  next segment file.
- On every rotation, `sweep` deletes segments whose end is past the tier's
  retention, then, while the directory exceeds `SIZE_CAP` (64 MiB), the
  oldest non-open segments, Day before Month (Month is what answers "last
  week"). Open segments are never deleted.
- Steady-state size (v2): Day ≈ 14 400 × 732 B ≈ 10.5 MB/day (≤ 30 h kept,
  4 h granularity, ≈ 13 MB); Month ≈ 18 000 × 732 B ≈ 13 MB. Well under the cap.

## Where it lives

`Recorder::new(root)`:

- No home directory: `MemoryStore`, session only. Nothing is created on disk.
- Otherwise: `DiskStore` at `default_root()`. If it cannot open (read-only home, a
  file where the directory should be) the recorder falls back to the session
  store and the app shows one toast; a later write error does the same.

Settings → About shows where history lives and how long it is kept.

## Cost

Per snapshot: 20 adds, three partial sorts of the process rows and one
hash-map fold of every process's watts by name (a few hundred entries, emptied
at each close). Per 6 s: one 732-byte append (plus one more every 144 s). Segment listing only on
rotation (every 4 h). The worker thread is untouched: the writer runs on the
main loop in `App::record`.

## Timeline and reports (wave 5)

- Query helpers live in `core/src/store/query.rs`: `Span` (1 h, today, 24 h,
  7 d, 30 d; today = since local midnight, the UTC offset passed in),
  `column_of`/`column_start` (one shared axis; `column_of(column_start(c)) ==
  c` for every width, swept in tests), `bin` (buckets → one merged bucket per
  plot column, reusing `tier::merge`), `events` (swap +256 MiB, CPU crossing
  95 °C, a gap = an empty column), `Blame::at` (same-named processes folded:
  a build's eight `rustc` are one row ×8), and `Report::build`.
- Spans up to a day read Day buckets; 7 d and 30 d read Month.
- The Timeline page (`ui/widgets/timeline.rs`) is full-screen like settings;
  `parts(area)` is the one geometry for paint, chip clicks, scrub clicks and
  the width the app bins to. Rows whose series never recorded (no GPU
  reading, no input rail) are hidden. The app re-bins on open, range change,
  resize and every 6 s while open (`app/timeline.rs`).
- Energy per bucket = mean W × tier span, summed by app name across pids.
  Since v2 every process's watts are summed per app (display name) in the
  accumulator, not just each tick's leaders: the top 8 are kept per bucket and
  the rest fold into `apps_other`, so `apps + other` is the bucket's whole
  process energy (tested against a brute-force sum; merging buckets preserves
  the total, dropped apps land in other). The report's leaderboard is exact
  for any app that stayed in its buckets' top 8, prints an `other` row, and
  falls back to the top-5 lists for v1 buckets. System energy comes from the
  input rail (`SysPowerW`) when present.
- The app a process counts toward is the dashboard's "group helpers by app"
  rule (`bucket::app_name`): the outermost `.app` in its executable path, so
  `Brave Browser Helper (Renderer)` adds to `Brave Browser`. Folding happens
  before the name meets the record's 16-byte field, so the report shows whole
  app names instead of `Brave Browser He` beside `Brave Browser`. Buckets
  written before this keep their per-helper names; merging is by name, so
  old and new rows simply sit side by side. The top-5 lists stay per pid,
  so their names (and the CSV's `top_*` cells) are still cut at 16 bytes:
  carrying longer names needs a record version bump (a wider name field,
  or a string table per segment), with a decoder for every older layout.

## Battery and thermal state (wave 6, store v2)

- Timeline: a `battery` row (percent, fixed 0–100 scale) whose cursor value
  adds the signed flow with the power panel's arrows (`64% ▼ 8.0W`,
  `90% ▲ 20W`, `idle` under 0.05 W). Hidden on desktops and over v1 history.
- Events (`query::state_events`): `Unplugged` / `PluggedIn` (the column's
  on-AC mean crossing 0.5; columns without a recorded source are silent),
  `Thermal(level)` on every change of the column's worst thermal state
  (fair/serious/critical and back to nominal), `BatteryLow` when the mean
  charge crosses below 20 %.
- Report: `time throttled` = Σ Throttled mean × span, exact even for a
  partly throttled 144 s bucket (a separate 0/1 series because a level's
  mean cannot say how long it sat at ≥ serious); `battery drain` over the
  newest run of on-battery buckets (Wh = Σ max(0, −battery W) × span, average
  W over the buckets with a reading, charge at both ends, "since unplug at"
  when the run began with a seen unplug); `battery health` first → last
  reading in range (Month buckets answer 7 d / 30 d).
- `Series::read`: battery columns only when `battery.present` (a desktop's AC
  is never recorded as a fake "on AC = 1"); thermal state only once primed.

- `siltop report` runs in its own process and opens the disk store read-only in effect
  (`DiskStore::open` creates the directories but writes nothing until append).

## Known gaps / next

- An app that is in a bucket's top 8 some of the time and in "other" the
  rest is undercounted by the leaderboard (the total stays exact). Raising
  `APP_N` costs 24 B per slot per record.
- A column that mixes AC and battery shows the mean flow (e.g. `▲ 4.0W`);
  the unplug event marks it.
- Thermal state on Linux stubs reads nominal once primed (no sensors there).

- The worker stops sampling while the display is paused (`p`), so history has
  a gap for the pause. A "keep recording while paused" needs the worker to
  keep sampling at the cheap cadence.
- History is only written while the TUI runs; `siltop record` (LaunchAgent)
  is plan item 20.
- Reads decode whole segments; the blame timeline over 30 days reads ≤ 10 Month
  files (~9 MB). An index or mmap can come with the UI if it shows up in
  profiles.
- Series are append-only; adding a column means a new `Layout` constant, a
  bump of `CURRENT`, and a fixture for the old one (see v1 → v2 above).
