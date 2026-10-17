# The siltop user guide

siltop is a terminal system monitor for Apple Silicon Macs. It shows current CPU, GPU,
memory, power, temperature, network and disk activity, plus history from earlier. This
guide covers everything from the first launch to scripting.

If you only remember three keys: **`m`** opens the menu, **`?`** lists every key, and
**`T`** cycles themes. The first launch shows exactly that hint in the footer when the
window is wide enough for it (about 140 columns); a narrower footer always keeps `? help`.

## Getting started

```sh
siltop         # the dashboard
siltop --demo  # a canned 60 s session: try it without load, or on another Mac
siltop --help  # every option, grouped, with examples
siltop help bench  # one command's usage and options; same as siltop bench --help
```

On the very first launch in a terminal, siltop shows three short screens: what it is,
privacy, and whether to check for updates. Esc skips them and leaves update
checks off. They are not shown for `--json`, `line`, `doctor`, `--demo`, `replay`, or when
`CI` or `SILTOP_NO_ONBOARDING` is set. `siltop onboarding` shows them again.

Quit with `q`. Everything you change in settings is saved for next time.

```sh
siltop --interval 1s         # or -i 1s: sample every second (100 ms to 60 s)
siltop --config ~/work.toml  # read preferences from another file
```

### Shell completion and the manual

```sh
siltop completions zsh  > ~/.zfunc/_siltop  # zsh (put ~/.zfunc on $fpath)
siltop completions bash > ~/.local/share/bash-completion/completions/siltop
siltop completions fish > ~/.config/fish/completions/siltop.fish
siltop man | mandoc -a                      # read the manual page
```

Completion and the man page are generated from the same table the argument parser uses, so
they always match the binary you have.

## Reading the dashboard

There are two surfaces. **Work** (the default on a big window) is a grid of graphs beside a
process table. **Glance** is graphs only, for a small corner window. `v` switches between
them; `--surface glance` starts there.

Every box can be focused (`tab`, arrows or a click), expanded to the whole window (`e`, or
`enter`, or the `[↗]` corner) and closed again (`esc` or `[×]`). An expanded box shows the
longer story: per-core rows, clocks, more history.

**Hardware your Mac does not have is hidden, not drawn as zero.** A fanless MacBook Air has
no fan box, a Mac mini or Studio has no battery, and a reading the kernel refuses is shown as
`—`. If you see a number, it was measured.

### CPU

The big graph is how much of the chip's real capacity is in use. It is
*frequency-weighted*: a core that is busy but running at half its top clock counts as half
busy. That is why it can read lower than Activity Monitor, which counts busy time only.

Below it are the clusters: efficiency (E), performance (P) and, on M5-class chips, super (S)
cores, each with its busy share and the clock it ran at. Expand the box (`e`) for per-core
rows; each core has its own small graph, so you can see one runaway thread pin one core.

### GPU

GPU busy share and its clock in MHz. Expanded, it adds GPU watts and temperature when this
Mac exposes them.

### Memory

Used memory against the total, swap, and memory pressure (nominal, warning, critical).
Pressure is the number that matters: macOS happily uses all free RAM as cache, so "used"
being high is normal. Warning or critical pressure is when things slow down.

### Power and battery

Whole-system watts, CPU and GPU watts, and on a laptop the battery level, charge state and
time left. Some power sources publish in batches every second or two. Between batches
siltop holds the last measured value and marks it **`≈`** with its age (for example
`≈ 12.4 W 2s`) rather than inventing a new number or dropping to zero. After 10 s a held
value counts as stale.

A laptop plugged in but not charging at its charge limit shows **held**: macOS is holding
the battery at a level (often 80%) to protect it. That is not a fault.

### Heat and fans

Chip temperatures by cluster, GPU temperature, and fan speeds with their range. A Mac that
exposes no temperature sensors to user space simply has no heat box.

### Network and disk

Receive and send rates, and disk read and write rates. The disk box is off by default:
turn it on in settings → panels (`disk = true` under `[panels]`). `units.net_bits` in
settings switches network rates to bits per second.

### Processes

The table on the right of Work. Columns include CPU, memory, GPU, energy, threads and more;
click a header or press `<` `>` to sort, `r` to reverse.

- **CPU** is per core by default: `200%` means two full cores. Settings → processes →
  `cpu_unit = "total"` makes 100% the whole machine instead.
- **Memory** is the *footprint*, the same number Activity Monitor and `top` show. It is what
  the process really costs you, including compressed memory, not the older "resident size".
- Rows owned by other users (`root`, `_windowserver`) show `—` for what macOS will not tell a
  normal user. Their CPU is still counted in the machine total.

`/` filters by name, `t` shows the tree, `enter` opens a process's details card, `x` asks it
to quit, `K` force-quits, and `X` offers every signal.

`a` groups helper processes under their app (all of Chrome's renderers under Chrome.app) with
their CPU, memory, energy and GPU summed; `enter` opens a group. In the tree and in groups,
`←` `→` fold and unfold a node and `E` `C` open or close them all; a folded node shows its
subtree's totals. `space` tags a row (a mark in the gutter) and `u` clears the tags; with
rows tagged, `x` and the signal picker act on the tagged set, and the confirmation names
every process first. The details card has pages for an overview, threads, open files and
ports: `tab` or `1`–`4` switch between them.

### The footer

The bottom line starts with the `siltop ≡` menu button, then the keys for what you are
looking at, then the **verdict**: a one-line judgement such as "All normal" or "Busy: Safari
using 3.1 cores" (click a `Busy:` verdict to select that process). On the right sit your
power source, system watts and the sampling rate. `siltop --diagnose` prints the same
verdict with its evidence.

## Keys

Press `?` for the full, searchable list. The essentials:

| Key | What it does |
| --- | --- |
| `m` | Main menu (settings, themes, keys, about, quit) |
| `?` | Every key |
| `s` | Settings |
| `T` | Next theme |
| `v` | Work / glance |
| `tab` `←` `→` | Move between boxes |
| `e` | Expand or collapse the focused box |
| `z` | Zoom the graphs' time window |
| `p` | Pause (sampling stops too, so it costs nothing) |
| `[` `]` | Sample slower / faster |
| `1`–`5` | Presets |
| `H` | Timeline: long history on one time axis |
| `Z` | Zen mode: graphs only |
| `q` | Quit |

Used to htop or btop? Pick their key profile in settings → keys, or start with
`SILTOP_KEYS=htop siltop`.

The mouse works everywhere: click to focus or select, double-click for details, scroll the
process table, drag the gutter between graphs and processes, and every footer hint is a
button.

### Every key

The table below is generated from `crates/siltop/src/keymap.rs` (a test fails if it drifts). `q` never quits from inside an overlay; it closes it.

| Key | Where | Action |
| --- | --- | --- |
| `s` | general | settings |
| `v` | general | cycle work / glance |
| `T` | general | cycle theme |
| `Z` | general | zen mode |
| `p` | general | pause / resume |
| `[` `-` | general | sample slower |
| `]` `+` `=` | general | sample faster |
| `U` | general | check for updates |
| `?` | general | help |
| `m` | general | main menu |
| `H` | general | timeline (history) |
| `z` | general | zoom graphs |
| `q` `ctrl-c` | general | quit |
| `ctrl-z` | general | suspend (fg resumes) |
| `esc` | general | close / clear filter / back / menu |
| `1` | general | preset: silicon |
| `2` | general | preset: ml |
| `3` | general | preset: laptop |
| `4` | general | preset: sre |
| `5` | general | preset: ricer |
| `→` | navigate | open node / next box |
| `←` | navigate | close node / previous box |
| `e` | navigate | expand / collapse box |
| `g` `home` | navigate | top of list |
| `G` `end` | navigate | bottom of list |
| `tab` | navigate | next box |
| `shift-tab` | navigate | previous box |
| `enter` | navigate | details / expand focused box |
| `j` `↓` | navigate | move down |
| `k` `↑` | navigate | move up |
| `pgdn` | navigate | page down |
| `pgup` | navigate | page up |
| `f` `/` | processes | filter processes |
| `t` | processes | process tree |
| `a` | processes | group helpers by app |
| `E` | processes | open all nodes |
| `C` | processes | close all nodes |
| `space` | processes | tag / untag process |
| `u` | processes | clear tags |
| `o` `>` | processes | next sort column |
| `<` | processes | previous sort column |
| `r` | processes | reverse sort |
| `x` | processes | quit process (TERM) |
| `K` | processes | force quit (KILL) |
| `X` | processes | send signal… |
| `delete` | processes | clear filter |
| `esc` `enter` `q` | process details | close details |
| `j` `↓` | process details | next process |
| `k` `↑` | process details | previous process |
| `x` | process details | quit (TERM) |
| `K` | process details | force quit (KILL) |
| `i` | process details | interrupt (INT) |
| `n` `F8` | process details | lower priority (nice +1) |
| `N` `F7` | process details | raise priority (nice -1, admin) |
| `b` | process details | background QoS on/off |
| `y` | process details | copy pid |
| `Y` | process details | copy command |
| `X` | process details | send signal… |
| `tab` | process details | next page |
| `shift-tab` | process details | previous page |
| `1` | process details | overview |
| `2` | process details | threads |
| `3` | process details | open files |
| `4` | process details | ports |
| `esc` | while filtering | clear, then leave |
| `enter` `tab` | while filtering | keep filter, back to the list |
| `↓` | while filtering | move down, keep typing |
| `↑` | while filtering | move up, keep typing |
| `delete` | while filtering | clear filter |
| `esc` | in settings | clear search, then close |
| `q` `s` `F2` `F10` | in settings | close settings |
| `j` `↓` | in settings | next setting |
| `k` `↑` | in settings | previous setting |
| `l` `→` `enter` `space` | in settings | change value |
| `h` `←` | in settings | change value back |
| `tab` | in settings | next section |
| `shift-tab` | in settings | previous section |
| `/` | in settings | search settings |

## Settings

`s` opens a full-screen settings page: general, appearance, panels, processes, alerts,
keys, privacy and about. `↑` `↓` move, `←` `→` change, `/` searches every section. Changes apply at once and
are written to `~/.config/siltop/config.toml` a second later. siltop only writes the
lines you changed, so your comments survive. `siltop --print-config` prints every key
with its effective value (after `-i`, `--surface` and `--theme`), in the same layout the file
uses, so you can paste it into `config.toml`.

Settings → **privacy** turns the daily update check and automatic install on or off.
Settings → **about** shows the version, commit and build date, and where your config and
history live. `siltop --version --verbose` prints the same.

## Presets

Presets rearrange the dashboard for a job in one key. They only change which boxes show and
how the process table sorts; your theme and everything else stay.

| Key | Preset | For |
| --- | --- | --- |
| `1` | silicon | The chip itself: CPU, GPU, power and heat |
| `2` | ml | Local models: GPU, memory and power, processes by memory |
| `3` | laptop | On battery: power, CPU and heat |
| `4` | sre | A wide process table beside network and disk |
| `5` | ricer | Every graph on the glance surface, chrome hidden, for screenshots |

## Themes

The default theme, `auto`, asks your terminal for its colours and builds a matching theme,
so siltop looks like it belongs in whatever you already use. `T` cycles the built-in
themes (`billabong`, `paper`, `okabe` for colour blindness, `monotreme`, `terminal`,
`catppuccin-mocha`, `tokyo-night`, `nord`, `kanagawa`, `gruvbox-dark`); settings →
appearance previews each one.

**Theme pack.** On top of the built-ins there are `dracula`, `rose-pine`, `solarized-dark`,
`solarized-light`, `everforest`, `one-dark` and `catppuccin-latte`, plus your own themes:
every `*.toml` in `themes/` next to the config file joins the picker (every colour is
optional, and unreadable contrast is corrected). `siltop theme list` lists every theme;
`siltop theme import <FILE>` converts a btop `.theme`
file into that folder.

**Glass.** On a translucent or blurred terminal window, glass mode never paints a cell
background, so your wallpaper shows through, and it brightens the frames to stay readable.
`auto` reads your terminal's opacity setting; you can force it `on` or `off`. **Extra
contrast** brightens dim text over a busy wallpaper.

## Timeline and reports

`H` (or **Timeline** at the top of the main menu) opens a full-screen page with CPU, GPU,
memory, power and CPU temperature on one shared time axis: the last hour, today, 24 h, 7 d or
30 d. Scrub with `←` `→` or the mouse; the blame card under the cursor names the top apps by
CPU, energy and memory in that slice, and flags swap growth, running hot and gaps. History is saved
to disk while the dashboard (or the agent) runs and kept for 30 days.

```sh
siltop report today        # energy by app, peak power and temperatures, time at 95 °C+
siltop report week --json  # the last 7 days as one JSON object (month: 30 days)
```

`siltop report` reads the saved history, so it works with the dashboard closed.

## Alerts, the agent and the status line

Alerts watch for sustained trouble: running hot, memory pressure, a runaway process, fast
battery drain, a nearly full disk (and, if you turn them on, a busy CPU, swap growth and loud
fans). A rule fires once its condition has held for a while, then stays quiet until the
reading recovers. In the dashboard they ring the terminal and post a macOS notification;
settings → alerts turns each rule on or off, and the README shows how to write your own
`[[alerts]]` rules with a shell hook.

```sh
siltop agent install  # alerts in the background at login, no dashboard needed
siltop agent status   # is it installed and running?
siltop agent uninstall
```

The agent is a normal LaunchAgent: no sudo, nothing outside your home folder.

`siltop line` prints one status line and exits, for tmux, SketchyBar or Starship.
`--format` takes a template of `{cpu} {gpu} {mem} {power} {temp} {swap} {thermal} {verdict}
{alerts}` (a reading this Mac cannot measure disappears rather than reading 0), and
`--style tmux` or `--style ansi` adds colour. Each reading takes its resource's colour from
your theme (cpu green, memory blue and so on in Billabong), and turns the theme's warning or
critical colour when it runs hot or busy; `SILTOP_THEME` picks the theme here as it does
for the dashboard. With `NO_COLOR` set (to anything but empty) or `TERM=dumb`, `--style
ansi` prints plain text; `--style tmux` keeps its markup but leaves colour out under
`NO_COLOR`. With the agent running it costs nothing.

```sh
set -g status-right '#(siltop line --style tmux)'   # in ~/.tmux.conf
```

## Record, replay and export

```sh
siltop record --duration 10m --out build.ptrace  # headless, one frame per interval
siltop replay build.ptrace                       # the full dashboard, from the file
siltop serve                                     # Prometheus metrics on 127.0.0.1:9101
siltop export --csv --from 24h > day.csv         # saved history as CSV
```

`record` writes a compact `.ptrace` until `--duration` runs out or you press Ctrl-C; a
recording cut short still plays up to its last whole frame. Attach a trace to a bug
report and anyone can open it with `replay`: `p` pauses, `+` `-` change the speed
and `←` `→` scrub. `serve` answers `GET /metrics` only and listens on loopback unless
`--addr` says otherwise. `export --csv` takes `--from` and `--to` as a UTC date
(`2026-09-23`) or a span ago (`24h`, `7d`).

## Sustained-performance bench

```sh
siltop bench                # every core busy for 60 s, then a report card
siltop bench --duration 5m  # a longer soak: does this Mac hold its clocks?
siltop bench --light        # half the cores: a gentler test
siltop bench --json         # the report card as one JSON object
```

`bench` spins one busy thread per logical core (half with `--light`) and samples every
500 ms through the same sampler as the dashboard. On a terminal it shows a small live page
with the clock, temperature and CPU power graphs and a countdown; Esc, `q` or Ctrl-C stops
early and still reports. The report card lists each cluster's early clock (the median of the
first 5 s under load), its sustained clock (the median of the last third of the run), how
far sustained dropped under early, and its single fastest reading as `peak`. The drop is
measured from the early clock, not the peak, so one boost reading never counts as a drop.
Then the peak temperature (`84°C`), average CPU power and efficiency in MHz per watt, how
the fans responded, and a verdict:

- **PASS: no throttling**, or
- **THROTTLED at mm:ss**, the first moment macOS reported a serious thermal state or a
  cluster held more than 15% under its early clock for two samples in a row (one slow
  reading is noise). A cluster whose sustained clock ended more than 15% under its early
  clock is THROTTLED too, at its first low reading, even if its dips never ran two in a
  row: a drop over 15% never sits beside a PASS.
- **incomplete: stopped after mm:ss, no verdict** when Esc, Ctrl-C or the 105 °C safety stop
  ended the run before any throttle (`"verdict": "incomplete"` in `--json`): a cut-short
  run is too short to pass. A throttle seen before the stop still reads THROTTLED.

A cluster the load never reached, busy less than half the time (with `--light` the
threads land on the fastest cores and most of the others idle), is shown as `only 31% busy,
drop not judged`: an idle cluster picks its own clock, so its drop says nothing about heat.
In `--json` each cluster carries `peak_mhz`, `baseline_mhz` (the early clock),
`sustained_mhz`, `drop_pct` (sustained under baseline, never negative), `busy_pct` and
`loaded`.

Readings a Mac does not have are left out: a fanless Air has no fans row, and a Mac without
a CPU power rail has no power or efficiency row. The run stops at once if any temperature
reaches 105 °C, and the load threads are always stopped before the report prints.

Each run is saved next to your history (`~/.local/state/siltop/bench.ndjson`) and
compared with your last full run of the same kind on the same chip: "clocks 3% lower than
your last run on 2026-09-20". Close heavy apps first for a fair comparison.
`siltop bench --demo` plays a canned loaded run (clocks near the top that settle a few
percent as the chip warms, then a pass) without loading the CPU or saving anything.

## Fleet: several Macs at once

```sh
siltop fleet studio mini.local  # this Mac beside two others
siltop fleet --demo a b c       # the grid with canned data, no ssh
```

`fleet` shows one card per Mac in a grid that reflows with the terminal: chip, CPU load
and cluster clocks, GPU, memory and pressure, power, the hottest sensor, battery when
there is one, the verdict and a small CPU history. This Mac is always the first card.
Arrow keys or `h` `j` `k` `l` move between cards, Enter (or a click) shows that Mac's top
processes, Esc goes back and `q` quits.

Each remote runs `ssh -o BatchMode=yes -- <host> siltop --json-stream --procs 3` with
your own `~/.ssh/config`, keys and agent, so every remote Mac needs siltop on its `PATH`
and key-based login (BatchMode never prompts for a password). siltop itself still opens
no sockets: the only network traffic is the ssh you already trust. A card with no reading
for three intervals dims and says `reconnecting…`; a dropped link is retried after 1s, 2s,
4s and so on up to 30s. Quitting kills every ssh child.

## Scripts and JSON

```sh
siltop --json | jq .cpu.usage                       # one sample
siltop --json-stream -i 1s --count 60 > run.ndjson  # one line per second for a minute
siltop --diagnose                                   # the verdict and its evidence
```

JSON uses snake_case names, and a value that was not measured is `null`, never `0`. The
fields are listed in [json-schema.md](json-schema.md); schema 1 only ever gains fields.
`--procs N` sets how many processes are included (default 10). Add `--demo` to test a script
against the canned session.

## Doctor

`siltop doctor` prints what this Mac exposes: model, chip, SMC keys, temperature
sensors, which readings work without root, and what siltop itself costs. Paste it into
a bug report, especially on a chip we have not seen yet.

## Troubleshooting

**The colours are wrong or the theme looks flat.** siltop asks the terminal for its
palette with OSC colour queries. Over ssh this is skipped; set `SILTOP_THEME=<name>`.
In **tmux**, detection works when tmux passes the colour queries through to your terminal
(`set -g allow-passthrough on` in `~/.tmux.conf` on tmux 3.3 or later); if it still guesses
wrong, pick a theme by name.

**Terminal.app.** It has 24-bit colour from macOS 26; on older macOS siltop uses the
nearest 256 colours automatically. `SILTOP_COLORS=256` forces that if detection guesses
wrong.

**The graphs are boxes or question marks.** The graphs are drawn with braille characters.
Use a font that has them: SF Mono, Menlo, JetBrains Mono, Iosevka and every Nerd Font do.
`SILTOP_ICONS=nerd` turns on Nerd Font icons in titles; `none` keeps plain marks.

**A box is missing.** Your Mac does not have that hardware (fans on an Air, a battery on a
desktop), or you hid it in settings → panels.

**Some processes show `—`.** macOS does not let a normal user read other users' processes.
Run with `sudo` if you really need those numbers; nothing else changes.

**It says "the dashboard needs a terminal".** stdin or stdout is not a terminal (a pipe, a
cron job). Use `--json`, `--json-stream` or `--diagnose` there.

**My settings are not saved.** A config file that does not parse is copied to
`config.toml.bak` and left alone, and a read-only file is never written; the status line
says which. `siltop --version --verbose` shows the path in use.

## Updates, feedback and reset

```sh
siltop update                 # check GitHub for a newer release, and say how to install it
siltop update --check         # exit 10 when a newer version is available
siltop update --yes           # install a downloaded (non-Homebrew) update without asking
siltop update --rollback      # restore <name>.old beside this copy
siltop feedback --bug --copy  # a prefilled bug-report link; doctor output on the clipboard
siltop reset --yes            # delete config, state and history
```

`siltop feedback` prints a link to a new GitHub issue, prefilled with only the siltop
version, the chip, the macOS build and how siltop was installed, and opens it in your
browser (not over ssh, where it only prints it). `--bug` picks the bug-report form instead
of the feature request. `--copy` puts `siltop doctor` output on the clipboard and only
prints the link, without opening a browser, so you can open it and paste.

Who can update itself depends on how siltop was installed:

- **A release download** in a `bin` directory you can write (`~/.local/bin`, `~/bin`,
  `/usr/local/bin`, ...) replaces itself. The archive is verified against its SHA-256, the
  new binary must report the expected version before it is swapped in, and the previous copy
  is kept as `siltop.old` for `--rollback`.
- **Homebrew** is never replaced in place: `update` tells you to run `brew upgrade siltop`.
- **`cargo install`** (`~/.cargo/bin`) is left to cargo: `update` prints the `cargo install`
  line for the new tag.
- **A local build** anywhere else, such as `target/release`, is never touched.

The opt-in daily check (onboarding, or settings → privacy) runs only in the dashboard, at
most once a day, and offers a found update again on later launches until you install or skip
it. `SILTOP_NO_UPDATE_CHECK=1` turns it off regardless of the setting.

## Uninstalling

`brew uninstall siltop`, or delete the binary you installed (and `siltop.old` beside it, if
`siltop update` left one). Run `siltop reset` first if you also want the config, state and
history gone, and `siltop agent uninstall` if you installed the background agent.

## Privacy

- **No network of its own.** siltop does not link an HTTP stack. It opens one local
  route socket to read the default route, and it reads network counters. It does not
  connect to a server unless you opt in to update checks: then it runs `/usr/bin/curl`
  at most once a day against the GitHub releases API and sends only the
  User-Agent `siltop/<version>`. `siltop update` does the same when you ask.
  `siltop serve` listens on 127.0.0.1 unless you pass another `--addr`.
  `fleet` reaches other Macs only through the `ssh` you run yourself.
- **No telemetry.** Nothing about you or this Mac is collected or sent. A crash file stays on disk; nothing sends it. You can attach it to a GitHub issue yourself.
- **No sudo.** Every reading comes from APIs a normal user may call.
- **It writes only its own files**, and only these:
  - in `~/.config/siltop/`: `config.toml` on first launch and when you change a setting
    (`config.toml.bak` keeps a copy of a file it could not read), and `themes/*.toml` when
    you run `siltop theme import`;
  - in `~/.local/state/siltop/`: history under `history/`, `bench` runs in
    `bench.ndjson`, `updates.toml` (when the last update check ran, and a skipped
    version), `crash-<time>.txt` if the process panicked, and, with the agent, its last
    sample in `agent.json` and its log in `agent.log`;
  - `~/Library/LaunchAgents/com.siltop.agent.plist`, only while `siltop agent` is
    installed (`agent uninstall` removes it);
  - `siltop record` writes its `.ptrace` in the current directory, or where `--out` says.

  `XDG_CONFIG_HOME` and `XDG_STATE_HOME` move the first two folders.
