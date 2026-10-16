#!/usr/bin/env python3
"""Measure what a TUI costs while it sits idle in a terminal. Prints JSON.

usage: perf-smoke.py [BIN] [--size 160x45] [--warmup 5] [--seconds 20] [--runs 1]
                     [--interval-ms MS | --config FILE] [--args "ARGS"] [--keys KEYS]
                     [--motion HZ] [--assert EXPR ...] [--out FILE] [--screen FILE]

BIN defaults to target/release/siltop. The binary runs in a pty with a fresh
temporary XDG_CONFIG_HOME, warms up, then is measured for --seconds:

  draws_per_s            synchronized-update frames (ESC[?2026h) per second
  content_draws_per_s    frames that painted something (roughly snapshots/s)
  idle_draws_per_s       frames whose diff was empty: pure waste
  bytes_per_s            terminal output
  read_chunks_per_s      pty reads that returned data (a lower bound on write(2) calls)
  cpu_pct                user+system time over wall time, % of one core
  threads                per-thread % of one core, busiest first
  energy_w               ri_energy_nj delta (proc_pid_rusage v6), macOS only
  wakeups_per_s          idle-package + interrupt wakeups, macOS only
  syscalls_per_s         unix + mach syscalls (proc_taskinfo), macOS only
  csw_per_s              context switches
  rss_mb, footprint_mb   at the end of the window
  first_frame_ms         spawn to the first frame
  snapshot_period_ms     gaps between content frames: n, mean, p50, p90, p99, max

--runs N measures N fresh processes and reports each run plus the per-metric
median. --assert takes KEY OP NUMBER against the median, e.g.
  --assert 'cpu_pct<=1.5' --assert 'snapshot_period_ms.p99<=515'
and exits 1 when any check fails (the JSON is printed either way).

--interval-ms writes `interval_ms = MS` into the temporary siltop config;
--config copies a whole file there instead. Other tools read their own
config under the same temporary XDG_CONFIG_HOME; seed it with
--xdg-file SRC=DEST, e.g. --xdg-file btop-1s.conf=btop/btop.conf.
"""

import argparse
import ctypes
import hashlib
import json
import os
import platform
import re
import select
import shlex
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import ptyterm  # noqa: E402

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEFAULT_BIN = os.path.join(REPO, "target", "release", "siltop")
MACOS = sys.platform == "darwin"

PROC_PIDTASKINFO = 4
PROC_PIDTHREADINFO = 5
PROC_PIDLISTTHREADS = 6
RUSAGE_INFO_V6 = 6

_RUSAGE_V6 = (
    "user_time system_time pkg_idle_wkups interrupt_wkups pageins wired_size "
    "resident_size phys_footprint proc_start_abstime proc_exit_abstime "
    "child_user_time child_system_time child_pkg_idle_wkups child_interrupt_wkups "
    "child_pageins child_elapsed_abstime diskio_bytesread diskio_byteswritten "
    "cpu_time_qos_default cpu_time_qos_maintenance cpu_time_qos_background "
    "cpu_time_qos_utility cpu_time_qos_legacy cpu_time_qos_user_initiated "
    "cpu_time_qos_user_interactive billed_system_time serviced_system_time "
    "logical_writes lifetime_max_phys_footprint instructions cycles billed_energy "
    "serviced_energy interval_max_phys_footprint runnable_time flags user_ptime "
    "system_ptime pinstructions pcycles energy_nj penergy_nj secure_time_in_use "
    "secure_ptime_in_use neural_footprint lifetime_max_neural_footprint "
    "interval_max_neural_footprint"
).split()


class RusageV6(ctypes.Structure):
    _fields_ = (
        [("uuid", ctypes.c_uint8 * 16)]
        + [(name, ctypes.c_uint64) for name in _RUSAGE_V6]
        + [("reserved", ctypes.c_uint64 * 9)]
    )


class TaskInfo(ctypes.Structure):
    _fields_ = [
        (name, ctypes.c_uint64)
        for name in "virtual_size resident_size total_user total_system threads_user threads_system".split()
    ] + [
        (name, ctypes.c_int32)
        for name in (
            "policy faults pageins cow_faults messages_sent messages_received "
            "syscalls_mach syscalls_unix csw threadnum numrunning priority"
        ).split()
    ]


class ThreadInfo(ctypes.Structure):
    _fields_ = [
        ("user_time", ctypes.c_uint64),
        ("system_time", ctypes.c_uint64),
        ("cpu_usage", ctypes.c_int32),
        ("policy", ctypes.c_int32),
        ("run_state", ctypes.c_int32),
        ("flags", ctypes.c_int32),
        ("sleep_time", ctypes.c_int32),
        ("curpri", ctypes.c_int32),
        ("priority", ctypes.c_int32),
        ("maxpriority", ctypes.c_int32),
        ("name", ctypes.c_char * 64),
    ]


class Timebase(ctypes.Structure):
    _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]


class Probe:
    """Cumulative counters for one pid. Every field is None when the platform
    cannot provide it."""

    def __init__(self):
        self.libproc = None
        self.ticks_to_ns = 1.0
        if MACOS:
            try:
                self.libproc = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
                libc = ctypes.CDLL("/usr/lib/libSystem.B.dylib")
                tb = Timebase()
                libc.mach_timebase_info(ctypes.byref(tb))
                self.ticks_to_ns = tb.numer / tb.denom
            except OSError:
                self.libproc = None

    def sample(self, pid):
        out = {
            "t": time.monotonic(),
            "cpu_ns": None,
            "energy_nj": None,
            "wakeups": None,
            "syscalls": None,
            "csw": None,
            "rss": None,
            "footprint": None,
            "threads": {},
        }
        if self.libproc is not None:
            self._sample_macos(pid, out)
        elif os.path.exists(f"/proc/{pid}/stat"):
            self._sample_linux(pid, out)
        if out["cpu_ns"] is None:
            out["cpu_ns"] = ps_cpu_ns(pid)
        return out

    def _sample_macos(self, pid, out):
        ru = RusageV6()
        if self.libproc.proc_pid_rusage(pid, RUSAGE_INFO_V6, ctypes.byref(ru)) == 0:
            out["cpu_ns"] = (ru.user_time + ru.system_time) * self.ticks_to_ns
            out["energy_nj"] = ru.energy_nj
            out["wakeups"] = ru.pkg_idle_wkups + ru.interrupt_wkups
            out["rss"] = ru.resident_size
            out["footprint"] = ru.phys_footprint
        ti = TaskInfo()
        size = ctypes.sizeof(ti)
        if self.libproc.proc_pidinfo(pid, PROC_PIDTASKINFO, ctypes.c_uint64(0), ctypes.byref(ti), size) == size:
            out["syscalls"] = ti.syscalls_unix + ti.syscalls_mach
            out["csw"] = ti.csw
        handles = (ctypes.c_uint64 * 512)()
        got = self.libproc.proc_pidinfo(
            pid, PROC_PIDLISTTHREADS, ctypes.c_uint64(0), handles, ctypes.sizeof(handles)
        )
        for index in range(max(got, 0) // 8):
            info = ThreadInfo()
            size = ctypes.sizeof(info)
            handle = handles[index]
            if self.libproc.proc_pidinfo(pid, PROC_PIDTHREADINFO, ctypes.c_uint64(handle), ctypes.byref(info), size) != size:
                continue
            name = info.name.decode("utf-8", "replace") or ("main" if index == 0 else f"thread-{index}")
            out["threads"][handle] = (name, info.user_time + info.system_time)

    def _sample_linux(self, pid, out):
        hz = os.sysconf("SC_CLK_TCK")
        per_tick = 1e9 / hz

        def stat_ticks(path):
            with open(path, encoding="utf-8") as f:
                fields = f.read().rsplit(")", 1)[1].split()
            return (int(fields[11]) + int(fields[12])) * per_tick

        try:
            out["cpu_ns"] = stat_ticks(f"/proc/{pid}/stat")
            with open(f"/proc/{pid}/status", encoding="utf-8") as f:
                status = dict(line.split(":", 1) for line in f if ":" in line)
            out["rss"] = int(status["VmRSS"].split()[0]) * 1024
            out["csw"] = int(status["voluntary_ctxt_switches"]) + int(status["nonvoluntary_ctxt_switches"])
            for tid in sorted(os.listdir(f"/proc/{pid}/task"), key=int):
                with open(f"/proc/{pid}/task/{tid}/comm", encoding="utf-8") as f:
                    name = f.read().strip()
                out["threads"][int(tid)] = (name, stat_ticks(f"/proc/{pid}/task/{tid}/stat"))
        except (OSError, KeyError, ValueError, IndexError):
            pass


def ps_cpu_ns(pid):
    text = subprocess.run(["ps", "-o", "time=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    if not text:
        return None
    days, _, clock = text.rpartition("-")
    secs = 0.0
    for part in clock.split(":"):
        secs = secs * 60 + float(part)
    return (secs + 86400 * int(days or 0)) * 1e9


def rate(a, b, key, wall, scale=1.0):
    if a[key] is None or b[key] is None or wall <= 0:
        return None
    return (b[key] - a[key]) * scale / wall


def percentile(sorted_values, pct):
    if not sorted_values:
        return None
    k = (len(sorted_values) - 1) * pct / 100.0
    lo = int(k)
    hi = min(lo + 1, len(sorted_values) - 1)
    return sorted_values[lo] + (sorted_values[hi] - sorted_values[lo]) * (k - lo)


def period_stats(times):
    gaps = sorted((b - a) * 1000.0 for a, b in zip(times, times[1:]))
    if not gaps:
        return {"n": 0, "mean": None, "p50": None, "p90": None, "p99": None, "max": None}
    return {
        "n": len(gaps),
        "mean": statistics.fmean(gaps),
        "p50": percentile(gaps, 50),
        "p90": percentile(gaps, 90),
        "p99": percentile(gaps, 99),
        "max": gaps[-1],
    }


def thread_table(a, b, wall):
    rows = []
    for handle, (name, after) in b["threads"].items():
        before = a["threads"].get(handle, (name, 0))[1]
        rows.append({"name": name, "cpu_pct": round(100.0 * (after - before) / 1e9 / wall, 3)})
    rows.sort(key=lambda row: -row["cpu_pct"])
    return rows


def write_config(xdg, opts):
    target = os.path.join(xdg, "siltop", "config.toml")
    if opts.config:
        os.makedirs(os.path.dirname(target), exist_ok=True)
        shutil.copyfile(opts.config, target)
    elif opts.interval_ms is not None:
        os.makedirs(os.path.dirname(target), exist_ok=True)
        with open(target, "w", encoding="utf-8") as f:
            f.write(f"interval_ms = {opts.interval_ms}\n")
    for spec in opts.xdg_file:
        src, _, dest = spec.partition("=")
        path = os.path.join(xdg, dest)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        shutil.copyfile(src, path)


def measure_once(opts, probe, run_index):
    xdg = tempfile.mkdtemp(prefix="perf-smoke-")
    write_config(xdg, opts)
    argv = [opts.bin] + shlex.split(opts.args)
    cols, rows = opts.size
    spawned = time.monotonic()
    pid, fd = ptyterm.spawn(argv, cols, rows, ptyterm.child_env(xdg, onboarding=False))
    responder = None if opts.no_respond else ptyterm.Responder()
    frames = ptyterm.FrameSplitter()
    counts = {"bytes": [], "first_output": None}
    raw = bytearray() if opts.screen else None
    motion = {"next": None, "i": 0, "capture": True}
    alive = [True]

    def pump(secs):
        end = time.monotonic() + secs
        while alive[0]:
            now = time.monotonic()
            if now >= end:
                return
            wait = min(end - now, 0.01)
            if motion["next"] is not None:
                if now >= motion["next"]:
                    x = 5 + motion["i"] % 60
                    motion["i"] += 1
                    motion["next"] += 1.0 / opts.motion
                    try:
                        os.write(fd, f"\x1b[<35;{x};10M".encode())
                    except OSError:
                        pass
                wait = max(0.0, min(wait, motion["next"] - now))
            ready, _, _ = select.select([fd], [], [], wait)
            if not ready:
                continue
            try:
                data = os.read(fd, 1 << 16)
            except OSError:
                data = b""
            if not data:
                alive[0] = False
                return
            now = time.monotonic()
            if counts["first_output"] is None:
                counts["first_output"] = now
            counts["bytes"].append((now, len(data)))
            frames.feed(data, now)
            if raw is not None and motion["capture"]:
                raw.extend(data)
            if responder is not None:
                reply = responder.feed(data)
                if reply:
                    os.write(fd, reply)

    if opts.keys:
        pump(min(1.0, opts.warmup))
        os.write(fd, ptyterm.unescape(opts.keys).encode())
        pump(max(0.0, opts.warmup - 1.0))
    else:
        pump(opts.warmup)
    a = probe.sample(pid)
    if opts.motion > 0:
        motion["next"] = time.monotonic()
    pump(opts.seconds)
    b = probe.sample(pid)
    motion["next"] = None
    motion["capture"] = False
    exited_early = not alive[0]
    status = ptyterm.stop(pid, fd, pump)
    if not opts.keep_xdg:
        shutil.rmtree(xdg, ignore_errors=True)

    t0, t1 = a["t"], b["t"]
    wall = t1 - t0
    window = [f for f in frames.frames if t0 <= f.begin < t1]
    content = [f for f in window if f.content]
    chunks = [n for t, n in counts["bytes"] if t0 <= t < t1]
    first_frame = frames.frames[0].begin if frames.frames else None
    result = {
        "run": run_index,
        "wall_s": round(wall, 3),
        "draws_per_s": len(window) / wall,
        "content_draws_per_s": len(content) / wall,
        "idle_draws_per_s": (len(window) - len(content)) / wall,
        "bytes_per_s": sum(chunks) / wall,
        "bytes_per_draw": (sum(f.size for f in window) / len(window)) if window else None,
        "read_chunks_per_s": len(chunks) / wall,
        "cpu_pct": rate(a, b, "cpu_ns", wall, 100.0 / 1e9),
        "energy_w": rate(a, b, "energy_nj", wall, 1e-9),
        "wakeups_per_s": rate(a, b, "wakeups", wall),
        "syscalls_per_s": rate(a, b, "syscalls", wall),
        "csw_per_s": rate(a, b, "csw", wall),
        "rss_mb": b["rss"] / 2**20 if b["rss"] is not None else None,
        "footprint_mb": b["footprint"] / 2**20 if b["footprint"] is not None else None,
        "first_output_ms": (counts["first_output"] - spawned) * 1000.0 if counts["first_output"] else None,
        "first_frame_ms": (first_frame - spawned) * 1000.0 if first_frame is not None else None,
        "snapshot_period_ms": period_stats([f.begin for f in content]),
        "threads": thread_table(a, b, wall),
        "queries_answered": responder.answered if responder else 0,
        "exited_early": exited_early,
        "exit_status": status,
        "loadavg_1m": os.getloadavg()[0],
    }
    if raw is not None:
        result["screen"] = dump_screen(raw, cols, rows, opts.screen, run_index)
    return result


def dump_screen(raw, cols, rows, path, run_index):
    try:
        import pyte
    except ImportError:
        return None
    screen = pyte.Screen(cols, rows)
    pyte.ByteStream(screen).feed(bytes(raw))
    target = path if run_index == 0 else f"{path}.{run_index}"
    with open(target, "w", encoding="utf-8") as f:
        f.write("".join(f"|{line}|\n" for line in screen.display))
    return target


SCALARS = [
    "draws_per_s", "content_draws_per_s", "idle_draws_per_s", "bytes_per_s",
    "bytes_per_draw", "read_chunks_per_s", "cpu_pct", "energy_w", "wakeups_per_s",
    "syscalls_per_s", "csw_per_s", "rss_mb", "footprint_mb",
    "first_output_ms", "first_frame_ms",
]


def median_of(runs):
    def med(values):
        values = [v for v in values if v is not None]
        return statistics.median(values) if values else None

    out = {key: med(run[key] for run in runs) for key in SCALARS}
    out["snapshot_period_ms"] = {
        key: med(run["snapshot_period_ms"][key] for run in runs)
        for key in ("n", "mean", "p50", "p90", "p99", "max")
    }
    return out


_ASSERT = re.compile(r"^\s*([\w.]+)\s*(<=|>=|==|<|>)\s*(-?[\d.]+)\s*$")


def check(expr, summary):
    m = _ASSERT.match(expr)
    if not m:
        return {"expr": expr, "ok": False, "error": "expected KEY OP NUMBER"}
    key, op, want = m.group(1), m.group(2), float(m.group(3))
    value = summary
    for part in key.split("."):
        value = value.get(part) if isinstance(value, dict) else None
    if not isinstance(value, (int, float)):
        return {"expr": expr, "ok": False, "value": value, "error": "no numeric value"}
    ok = {
        "<=": value <= want,
        ">=": value >= want,
        "==": value == want,
        "<": value < want,
        ">": value > want,
    }[op]
    return {"expr": expr, "ok": ok, "value": value}


def sysctl(name):
    if not MACOS:
        return None
    return subprocess.run(["sysctl", "-n", name], capture_output=True, text=True).stdout.strip() or None


def host_info():
    info = {
        "platform": platform.platform(),
        "model": sysctl("hw.model"),
        "cpu": sysctl("machdep.cpu.brand_string"),
        "logical_cpus": os.cpu_count(),
        "loadavg_1m": os.getloadavg()[0],
    }
    if MACOS:
        info["os"] = subprocess.run(["sw_vers", "-productVersion"], capture_output=True, text=True).stdout.strip()
        batt = subprocess.run(["pmset", "-g", "batt"], capture_output=True, text=True).stdout
        m = re.search(r"'([^']+)'", batt)
        info["power_source"] = m.group(1) if m else None
    return info


def binary_info(path):
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return {"path": path, "sha256": digest.hexdigest()[:16], "bytes": os.path.getsize(path)}


def parse_size(text):
    cols, _, rows = text.lower().partition("x")
    return int(cols), int(rows)


def round_floats(value, places=3):
    if isinstance(value, float):
        return round(value, places)
    if isinstance(value, dict):
        return {k: round_floats(v, places) for k, v in value.items()}
    if isinstance(value, list):
        return [round_floats(v, places) for v in value]
    return value


def main():
    parser = argparse.ArgumentParser(
        description=__doc__.splitlines()[0],
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="\n".join(__doc__.splitlines()[2:]),
    )
    parser.add_argument("bin", nargs="?", default=DEFAULT_BIN)
    parser.add_argument("--size", type=parse_size, default=(160, 45), help="COLSxROWS (default 160x45)")
    parser.add_argument("--warmup", type=float, default=5.0, help="seconds before measuring (default 5)")
    parser.add_argument("--seconds", type=float, default=20.0, help="measured seconds (default 20)")
    parser.add_argument("--runs", type=int, default=1, help="fresh processes to measure (default 1)")
    parser.add_argument("--interval-ms", type=int, help="write interval_ms into the temp siltop config")
    parser.add_argument("--config", help="copy this file to the temp siltop config.toml")
    parser.add_argument(
        "--xdg-file",
        action="append",
        default=[],
        metavar="SRC=DEST",
        help="copy SRC to DEST under the temp XDG_CONFIG_HOME (other tools' configs)",
    )
    parser.add_argument("--args", default="", help="extra arguments for the binary")
    parser.add_argument("--keys", help="keys typed 1 s after launch (escapes \\e \\r \\t)")
    parser.add_argument("--motion", type=float, default=0.0, help="mouse-motion events per second while measuring")
    parser.add_argument("--assert", dest="asserts", action="append", default=[], metavar="EXPR")
    parser.add_argument("--label", help="free-form label copied into the JSON")
    parser.add_argument("--out", help="also write the JSON here")
    parser.add_argument("--screen", help="write the final screen text here (needs pyte)")
    parser.add_argument("--no-respond", action="store_true", help="do not answer terminal queries")
    parser.add_argument("--keep-xdg", action="store_true", help="keep the temporary config dir")
    opts = parser.parse_args()

    opts.bin = shutil.which(opts.bin) or opts.bin
    if not os.path.isfile(opts.bin):
        sys.exit(f"perf-smoke: {opts.bin} not found (cargo build --release -p siltop)")
    if opts.config and opts.interval_ms is not None:
        sys.exit("perf-smoke: use --interval-ms or --config, not both")

    probe = Probe()
    runs = []
    for index in range(opts.runs):
        print(f"perf-smoke: run {index + 1}/{opts.runs} {opts.bin}", file=sys.stderr)
        runs.append(measure_once(opts, probe, index))

    summary = median_of(runs)
    checks = [check(expr, summary) for expr in opts.asserts]
    report = {
        "tool": "perf-smoke",
        "schema": 1,
        "label": opts.label,
        "binary": binary_info(opts.bin),
        "args": opts.args,
        "size": f"{opts.size[0]}x{opts.size[1]}",
        "interval_ms": opts.interval_ms,
        "warmup_s": opts.warmup,
        "seconds": opts.seconds,
        "motion_hz": opts.motion,
        "host": host_info(),
        "median": summary,
        "runs": runs,
        "asserts": checks,
    }
    text = json.dumps(round_floats(report), indent=2)
    print(text)
    if opts.out:
        with open(opts.out, "w", encoding="utf-8") as f:
            f.write(text + "\n")
    if any(not c["ok"] for c in checks):
        for c in checks:
            if not c["ok"]:
                print(f"perf-smoke: FAIL {c['expr']} (got {c.get('value')})", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
