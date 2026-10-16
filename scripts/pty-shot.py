#!/usr/bin/env python3
"""Drive a TUI in a pty and save screens as text (and PNG when Pillow is installed).

usage: pty-shot.py [--no-png] [--args "ARGS"] [--xdg DIR] BIN COLS ROWS OUTPREFIX STEP [STEP ...]

steps:
  wait:SECONDS           pump output for SECONDS
  until:TEXT[@SECONDS]   pump until the screen contains TEXT (default 5 s)
  key:TEXT               type keys; escapes \\e (Esc) \\r (Enter) \\t (Tab) \\n
  click:X,Y              left click at 0-based cell X,Y (SGR mouse)
  dclick:X,Y             double click
  wheel:up|down,X,Y      one wheel notch
  move:X,Y               pointer motion with no button held
  resize:CxR             resize the pty and the virtual screen
  shot:NAME              write OUTPREFIX-NAME.txt (+ .png)

Every .txt line is framed as |row| so trailing blanks stay visible.
Requires pyte (pip install pyte). PNG needs Pillow and a monospace font;
without them the script writes .txt only.
Env: SHOT_ARGS (extra binary args), SHOT_BG / SHOT_FG (hex), SHOT_FONT, SHOT_SIZE.
"""

import argparse
import os
import select
import shlex
import sys
import time

sys.dont_write_bytecode = True
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import ptyterm  # noqa: E402

try:
    import pyte
except ImportError:
    sys.exit("pty-shot: pyte is required (python3 -m pip install pyte)")

try:
    from PIL import Image, ImageDraw, ImageFont
except ImportError:
    Image = None

BG = os.environ.get("SHOT_BG", ptyterm.DEFAULT_BG)
FG = os.environ.get("SHOT_FG", ptyterm.DEFAULT_FG)
SIZE = int(os.environ.get("SHOT_SIZE", "15"))
FONTS = [
    os.environ.get("SHOT_FONT", ""),
    os.path.expanduser("~/Library/Fonts/JetBrainsMonoNerdFont-Regular.ttf"),
    os.path.expanduser("~/Library/Fonts/JetBrainsMono-Regular.ttf"),
    "/System/Library/Fonts/SFNSMono.ttf",
    "/System/Library/Fonts/Menlo.ttc",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
]

NAMED = {
    "black": "000000", "red": "cd3131", "green": "0dbc79", "brown": "e5e510",
    "yellow": "e5e510", "blue": "2472c8", "magenta": "bc3fbc", "cyan": "11a8cd",
    "white": "e5e5e5", "brightblack": "666666", "brightred": "f14c4c",
    "brightgreen": "23d18b", "brightbrown": "f5f543", "brightyellow": "f5f543",
    "brightblue": "3b8eea", "brightmagenta": "d670d6", "brightcyan": "29b8db",
    "brightwhite": "ffffff",
}


def pick_font():
    for path in FONTS:
        if path and os.path.exists(path):
            return path
    return None


def colour(value, default):
    if value == "default":
        return default
    if value in NAMED:
        return NAMED[value]
    if len(value) == 6:
        return value
    return default


def render_png(screen, path, font_path):
    font = ImageFont.truetype(font_path, SIZE)
    bold_path = font_path.replace("Regular", "Bold")
    bold = ImageFont.truetype(bold_path, SIZE) if os.path.exists(bold_path) else font
    asc, desc = font.getmetrics()
    cw = int(round(font.getlength("M")))
    ch = asc + desc
    img = Image.new("RGB", (screen.columns * cw + 16, screen.lines * ch + 16), "#" + BG)
    draw = ImageDraw.Draw(img)
    for y in range(screen.lines):
        row = screen.buffer[y]
        for x in range(screen.columns):
            cell = row[x]
            fg = colour(cell.fg, FG)
            bg = colour(cell.bg, BG)
            if cell.reverse:
                fg, bg = bg, fg
            px, py = 8 + x * cw, 8 + y * ch
            if bg != BG:
                draw.rectangle([px, py, px + cw - 1, py + ch - 1], fill="#" + bg)
            glyph = cell.data
            if not glyph or glyph == " ":
                continue
            if glyph in "─━│┃":
                # Box lines as geometry so neighbouring cells join.
                mid_y, mid_x = py + ch // 2, px + cw // 2
                w = 2 if glyph in "━┃" else 1
                if glyph in "─━":
                    draw.rectangle([px, mid_y, px + cw, mid_y + w - 1], fill="#" + fg)
                else:
                    draw.rectangle([mid_x, py, mid_x + w - 1, py + ch], fill="#" + fg)
                continue
            draw.text((px, py + 1), glyph, font=bold if cell.bold else font, fill="#" + fg)
    img.save(path)


def screen_text(screen):
    return "".join(f"|{line}|\n" for line in screen.display)


class Session:
    def __init__(self, argv, cols, rows, env):
        self.screen = pyte.Screen(cols, rows)
        self.stream = pyte.ByteStream(self.screen)
        self.responder = ptyterm.Responder(
            fg=FG,
            bg=BG,
            cursor=lambda: (self.screen.cursor.y + 1, self.screen.cursor.x + 1),
        )
        self.pid, self.fd = ptyterm.spawn(argv, cols, rows, env)
        self.alive = True

    def pump(self, secs, until=None):
        end = time.monotonic() + secs
        while self.alive:
            if until is not None and until in "\n".join(self.screen.display):
                return True
            left = end - time.monotonic()
            if left <= 0:
                break
            ready, _, _ = select.select([self.fd], [], [], min(left, 0.05))
            if not ready:
                continue
            try:
                data = os.read(self.fd, 1 << 16)
            except OSError:
                data = b""
            if not data:
                self.alive = False
                break
            self.stream.feed(data)
            reply = self.responder.feed(data)
            if reply:
                os.write(self.fd, reply)
        return until is None

    def send(self, data, settle):
        os.write(self.fd, data)
        self.pump(settle)

    def resize(self, cols, rows):
        self.screen.resize(rows, cols)
        ptyterm.set_size(self.fd, self.pid, cols, rows)
        self.pump(0.5)


def cell(arg):
    x, y = (int(v) for v in arg.split(","))
    return x + 1, y + 1


def run_step(sess, step, prefix, font_path):
    kind, _, arg = step.partition(":")
    if kind == "wait":
        sess.pump(float(arg))
    elif kind == "until":
        text, _, secs = arg.rpartition("@") if "@" in arg else (arg, "", "5")
        if not sess.pump(float(secs), until=text):
            print(f"pty-shot: timed out waiting for {text!r}", file=sys.stderr)
    elif kind == "key":
        sess.send(ptyterm.unescape(arg).encode(), 0.25)
    elif kind == "click":
        x, y = cell(arg)
        sess.send(f"\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m".encode(), 0.3)
    elif kind == "dclick":
        x, y = cell(arg)
        press = f"\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m".encode()
        sess.send(press + press, 0.3)
    elif kind == "wheel":
        direction, rest = arg.split(",", 1)
        x, y = cell(rest)
        button = 64 if direction == "up" else 65
        sess.send(f"\x1b[<{button};{x};{y}M".encode(), 0.2)
    elif kind == "move":
        x, y = cell(arg)
        sess.send(f"\x1b[<35;{x};{y}M".encode(), 0.1)
    elif kind == "resize":
        cols, rows = (int(v) for v in arg.split("x"))
        sess.resize(cols, rows)
    elif kind == "shot":
        with open(f"{prefix}-{arg}.txt", "w", encoding="utf-8") as out:
            out.write(screen_text(sess.screen))
        if font_path:
            render_png(sess.screen, f"{prefix}-{arg}.png", font_path)
    else:
        print(f"pty-shot: unknown step {step!r}", file=sys.stderr)


def main():
    parser = argparse.ArgumentParser(
        description=__doc__.splitlines()[0],
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="\n".join(__doc__.splitlines()[4:]),
    )
    parser.add_argument("--no-png", action="store_true", help="write .txt only")
    parser.add_argument("--args", default=os.environ.get("SHOT_ARGS", ""), help="extra binary args")
    parser.add_argument("--xdg", help="XDG_CONFIG_HOME for the child (default: inherit)")
    parser.add_argument("bin")
    parser.add_argument("cols", type=int)
    parser.add_argument("rows", type=int)
    parser.add_argument("prefix")
    parser.add_argument("steps", nargs="*")
    opts = parser.parse_args()

    font_path = None
    if not opts.no_png:
        if Image is None:
            print("pty-shot: Pillow not installed, writing .txt only", file=sys.stderr)
        else:
            font_path = pick_font()
            if font_path is None:
                print("pty-shot: no monospace font found, writing .txt only", file=sys.stderr)

    prefix_dir = os.path.dirname(opts.prefix)
    if prefix_dir:
        os.makedirs(prefix_dir, exist_ok=True)
    argv = [opts.bin] + shlex.split(opts.args)
    sess = Session(argv, opts.cols, opts.rows, ptyterm.child_env(opts.xdg))
    sess.pump(0.3)
    for step in opts.steps:
        run_step(sess, step, opts.prefix, font_path)
    status = ptyterm.stop(sess.pid, sess.fd, sess.pump, grace=0.3)
    if status not in (None, 0):
        print(f"pty-shot: child exited with {status}", file=sys.stderr)


if __name__ == "__main__":
    main()
