#!/usr/bin/env python3
"""Soak a TUI for leaks: RSS growth, open file descriptors, thread count. Prints JSON.

usage: soak.py [BIN] [--minutes 15] [--every 30] [--warmup 60] [--size 160x45]
               [--interval-ms MS] [--args "ARGS"] [--max-rss-growth-mb 5]
               [--max-fd-growth 0] [--max-thread-growth 0] [--out FILE]

BIN defaults to target/release/siltop. It runs under a pty (the same fake
terminal as perf-smoke.py, via ptyterm.py) with a fresh XDG_CONFIG_HOME. After
--warmup seconds it records a baseline, then samples every --every seconds
until --minutes have passed:

  rss_mb       resident size (`ps -o rss=`)
  fds          open descriptors (`lsof -p PID`, or /proc/PID/fd on Linux)
  threads      thread count (`ps -M`, or /proc/PID/task on Linux)

The run fails (exit 1) when the last sample exceeds the baseline by more than
the allowed growth on any of the three, or when the process dies. Growth is
measured from the post-warmup baseline so first-use caches (the uid table,
the SMC key list, font and colour state) do not count as a leak.

The 1.1 gate: 15 minutes at the default interval, RSS growth < 5 MB, fds and
threads flat.
"""

import argparse
import json
import os
import select
import shlex
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import ptyterm  # noqa: E402


def run_text(argv):
    try:
        return subprocess.run(argv, capture_output=True, text=True, timeout=20).stdout
    except (OSError, subprocess.SubprocessError):
        return ""


def rss_mb(pid):
    text = run_text(["ps", "-o", "rss=", "-p", str(pid)]).strip()
    return int(text) / 1024.0 if text.isdigit() else None


def fd_count(pid):
    proc = f"/proc/{pid}/fd"
    if os.path.isdir(proc):
        return len(os.listdir(proc))
    lines = run_text(["lsof", "-n", "-P", "-p", str(pid)]).splitlines()
    # Header plus one line per descriptor; cwd/txt/mapped files are stable.
    return max(0, len(lines) - 1) if lines else None


def thread_count(pid):
    proc = f"/proc/{pid}/task"
    if os.path.isdir(proc):
        return len(os.listdir(proc))
    lines = run_text(["ps", "-M", "-p", str(pid)]).splitlines()
    return max(0, len(lines) - 1) if lines else None


def sample(pid, t0):
    return {
        "t_s": round(time.monotonic() - t0, 1),
        "rss_mb": rss_mb(pid),
        "fds": fd_count(pid),
        "threads": thread_count(pid),
    }


def verdict(baseline, last, opts):
    """Growth per metric and the list of failures (empty means pass)."""
    limits = {
        "rss_mb": opts.max_rss_growth_mb,
        "fds": opts.max_fd_growth,
        "threads": opts.max_thread_growth,
    }
    growth, failures = {}, []
    for key, limit in limits.items():
        a, b = baseline.get(key), last.get(key)
        if a is None or b is None:
            failures.append(f"{key}: unreadable")
            continue
        growth[key] = round(b - a, 3)
        if b - a > limit:
            failures.append(f"{key} grew {b - a:.2f} (limit {limit})")
    return growth, failures


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("bin", nargs="?", default="target/release/siltop")
    ap.add_argument("--minutes", type=float, default=15.0)
    ap.add_argument("--every", type=float, default=30.0, help="seconds between samples")
    ap.add_argument("--warmup", type=float, default=60.0, help="seconds before the baseline")
    ap.add_argument("--size", default="160x45")
    ap.add_argument("--interval-ms", type=int)
    ap.add_argument("--args", default="")
    ap.add_argument("--max-rss-growth-mb", type=float, default=5.0)
    ap.add_argument("--max-fd-growth", type=int, default=0)
    ap.add_argument("--max-thread-growth", type=int, default=0)
    ap.add_argument("--out")
    opts = ap.parse_args()

    cols, rows = (int(v) for v in opts.size.lower().split("x"))
    xdg = tempfile.mkdtemp(prefix="soak-")
    if opts.interval_ms is not None:
        os.makedirs(os.path.join(xdg, "siltop"), exist_ok=True)
        with open(os.path.join(xdg, "siltop", "config.toml"), "w", encoding="utf-8") as f:
            f.write(f"interval_ms = {opts.interval_ms}\n")
    argv = [opts.bin] + shlex.split(opts.args)
    pid, fd = ptyterm.spawn(argv, cols, rows, ptyterm.child_env(xdg, onboarding=False))
    responder = ptyterm.Responder()
    alive = [True]

    def pump(secs):
        end = time.monotonic() + secs
        while alive[0] and time.monotonic() < end:
            ready, _, _ = select.select([fd], [], [], min(0.2, max(0.0, end - time.monotonic())))
            if not ready:
                continue
            try:
                data = os.read(fd, 1 << 16)
            except OSError:
                data = b""
            if not data:
                alive[0] = False
                return
            reply = responder.feed(data)
            if reply:
                os.write(fd, reply)

    t0 = time.monotonic()
    pump(opts.warmup)
    samples = [sample(pid, t0)] if alive[0] else []
    deadline = t0 + opts.warmup + opts.minutes * 60.0
    while alive[0] and time.monotonic() < deadline:
        pump(min(opts.every, max(0.0, deadline - time.monotonic())))
        if alive[0]:
            samples.append(sample(pid, t0))
    died = not alive[0]
    ptyterm.stop(pid, fd, pump)

    report = {"bin": opts.bin, "minutes": opts.minutes, "samples": samples}
    failures = ["process exited during the soak"] if died else []
    if len(samples) >= 2:
        growth, more = verdict(samples[0], samples[-1], opts)
        report["growth"] = growth
        report["peak_rss_mb"] = max(s["rss_mb"] or 0 for s in samples)
        failures += more
    else:
        failures.append("fewer than two samples")
    report["failures"] = failures
    report["pass"] = not failures
    text = json.dumps(report, indent=2)
    print(text)
    if opts.out:
        with open(opts.out, "w", encoding="utf-8") as f:
            f.write(text + "\n")
    return 0 if not failures else 1


if __name__ == "__main__":
    sys.exit(main())
