"""Shared pty plumbing for pty-shot.py and perf-smoke.py. Standard library only.

A fake terminal: fork a TUI under a pty at a given size, answer the handful of
terminal queries a real emulator would (cursor position, primary device
attributes, DECRQM, OSC 10/11 colours), and split the output stream into
synchronized-update frames (ESC[?2026h ... ESC[?2026l).
"""

import fcntl
import os
import pty
import re
import signal
import struct
import termios
import time

SYNC_BEGIN = b"\x1b[?2026h"
SYNC_END = b"\x1b[?2026l"

DEFAULT_BG = "161616"
DEFAULT_FG = "d8d8d8"

_ESCAPES = re.compile(
    rb"\x1b\[[0-?]*[ -/]*[@-~]"  # CSI
    rb"|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)"  # OSC
    rb"|\x1bP[^\x1b]*\x1b\\"  # DCS
    rb"|\x1b[ -/]*[0-~]"  # two-byte and nF escapes
)
_QUERIES = re.compile(
    rb"\x1b\[6n"  # DSR cursor position
    rb"|\x1b\[0?c"  # DA1
    rb"|\x1b\[\?(\d+)\$p"  # DECRQM private mode
    rb"|\x1b\]1([01]);\?(\x07|\x1b\\)"  # OSC 10/11 colour query
)
_QUERY_TAIL = 32


def unescape(text):
    """Key and raw-byte steps accept \\e (Esc), \\r (Enter), \\t (Tab) and \\n."""
    return (
        text.replace("\\e", "\x1b")
        .replace("\\r", "\r")
        .replace("\\t", "\t")
        .replace("\\n", "\n")
    )


def child_env(xdg=None):
    env = dict(os.environ)
    env["TERM"] = "xterm-256color"
    env["COLORTERM"] = "truecolor"
    env.pop("NO_COLOR", None)
    # A fresh config would otherwise park siltop on the first-run screens.
    env.setdefault("SILTOP_NO_ONBOARDING", "1")
    if "UTF-8" not in env.get("LANG", "").upper().replace("UTF8", "UTF-8"):
        env["LANG"] = "en_US.UTF-8"
    if xdg is not None:
        env["XDG_CONFIG_HOME"] = xdg
    return env


def set_size(fd, pid, cols, rows):
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    try:
        os.kill(pid, signal.SIGWINCH)
    except ProcessLookupError:
        pass


def spawn(argv, cols, rows, env):
    """Fork argv under a pty sized cols x rows. Returns (pid, master fd)."""
    pid, fd = pty.fork()
    if pid == 0:
        try:
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
            os.execvpe(argv[0], argv, env)
        finally:
            os._exit(127)
    set_size(fd, pid, cols, rows)
    return pid, fd


def stop(pid, fd, pump, quit_keys=b"q", grace=1.0):
    """Ask politely with quit_keys, drain for `grace` seconds, then SIGKILL.
    Returns the exit status if the child exited on its own, else None."""
    try:
        os.write(fd, quit_keys)
    except OSError:
        pass
    pump(grace)
    status = None
    deadline = time.monotonic() + 0.5
    try:
        while status is None and time.monotonic() < deadline:
            done, raw = os.waitpid(pid, os.WNOHANG)
            if done:
                status = os.waitstatus_to_exitcode(raw)
            else:
                time.sleep(0.01)
    except ChildProcessError:
        pass
    if status is None:
        try:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
        except (ProcessLookupError, ChildProcessError):
            pass
    try:
        os.close(fd)
    except OSError:
        pass
    return status


def has_content(payload):
    """True when a frame payload paints something: any printable byte left
    after stripping escape sequences. ratatui's empty diff is only SGR resets
    and cursor hide/show, so it counts as no content."""
    rest = _ESCAPES.sub(b"", bytes(payload))
    return any(b >= 0x20 and b != 0x7F for b in rest)


def _osc_rgb(hex6):
    r, g, b = (int(hex6[i : i + 2], 16) for i in (0, 2, 4))
    return f"rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}"


class Responder:
    """Answers terminal queries found in the child's output, across read
    boundaries, exactly once each."""

    def __init__(self, fg=DEFAULT_FG, bg=DEFAULT_BG, cursor=None):
        self.fg = fg
        self.bg = bg
        self.cursor = cursor or (lambda: (1, 1))
        self._tail = b""
        self._done_until = 0
        self._offset = 0
        self.answered = 0

    def feed(self, data):
        window = self._tail + data
        base = self._offset - len(self._tail)
        out = bytearray()
        for m in _QUERIES.finditer(window):
            if base + m.end() <= self._done_until:
                continue
            self._done_until = base + m.end()
            out += self._answer(m)
            self.answered += 1
        self._offset += len(data)
        self._tail = window[-_QUERY_TAIL:]
        return bytes(out)

    def _answer(self, m):
        text = m.group(0)
        if text == b"\x1b[6n":
            row, col = self.cursor()
            return f"\x1b[{row};{col}R".encode()
        if text.endswith(b"c"):
            return b"\x1b[?62;22c"
        if m.group(1) is not None:
            mode = int(m.group(1))
            state = 2 if mode == 2026 else 0
            return f"\x1b[?{mode};{state}$y".encode()
        which = m.group(2).decode()
        colour = self.fg if which == "0" else self.bg
        return f"\x1b]1{which};{_osc_rgb(colour)}".encode() + m.group(3)


class Frame:
    __slots__ = ("begin", "end", "size", "content")

    def __init__(self, begin, end, size, content):
        self.begin = begin
        self.end = end
        self.size = size
        self.content = content


class FrameSplitter:
    """Splits the stream on synchronized-update markers. Each closed frame is
    appended to `frames` with its begin/end read time and payload size."""

    def __init__(self):
        self.frames = []
        self._carry = b""
        self._open = False
        self._begin = 0.0
        self._payload = bytearray()

    def feed(self, data, now):
        buf = self._carry + data
        while True:
            if not self._open:
                i = buf.find(SYNC_BEGIN)
                if i < 0:
                    self._carry = buf[-(len(SYNC_BEGIN) - 1) :]
                    return
                self._open = True
                self._begin = now
                self._payload = bytearray()
                buf = buf[i + len(SYNC_BEGIN) :]
            else:
                j = buf.find(SYNC_END)
                if j < 0:
                    keep = len(SYNC_END) - 1
                    if len(buf) > keep:
                        self._payload += buf[:-keep]
                        buf = buf[-keep:]
                    self._carry = buf
                    return
                self._payload += buf[:j]
                self.frames.append(
                    Frame(self._begin, now, len(self._payload), has_content(self._payload))
                )
                self._open = False
                buf = buf[j + len(SYNC_END) :]
