#!/usr/bin/env python3
"""Drive `oracle tui` in a real pty and check what it draws.

The interface is covered three ways. Its state machine is a plain Rust type
tested without a terminal, its frames are rendered to ratatui's test backend and
asserted on as text, and this script runs the actual binary against an actual
pty. Only the third catches the things that are wrong *between* the program and
the terminal: a screen that never paints, raw mode that is never left, a cursor
that never comes back.

    python3 scripts/tui-smoke.py [path-to-oracle]

Exits 0 when everything below passes. Needs nothing but CPython.
"""

import fcntl
import os
import pty
import re
import select
import struct
import subprocess
import sys
import termios
import time

ROWS, COLS = 40, 120

# Cursor positioning, colour, and mode switches. Stripping these leaves the
# glyphs but not the spacing between them, so comparisons ignore whitespace.
ANSI = re.compile(rb"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[()][A-Z0-9]|\x1b[=>]|\x1b\][^\x07]*\x07")

ALT_SCREEN_OFF = b"\x1b[?1049l"
CURSOR_ON = b"\x1b[?25h"


class Session:
    """A running `oracle tui` on the far end of a pseudo-terminal."""

    def __init__(self, binary):
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.environ["TERM"] = "xterm-256color"
            # Point at a config that cannot exist, so the run is not shaped by
            # whatever the person running this happens to have configured.
            os.environ["XDG_CONFIG_HOME"] = "/nonexistent-oracle-smoke"
            os.execv(binary, [binary, "tui"])
        self.resize(ROWS, COLS)
        os.set_blocking(self.fd, False)
        self.buf = bytearray()

    def resize(self, rows, cols):
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    def pump(self, seconds):
        end = time.time() + seconds
        while time.time() < end:
            ready, _, _ = select.select([self.fd], [], [], 0.05)
            if not ready:
                continue
            try:
                data = os.read(self.fd, 1 << 20)
            except OSError:
                return
            if not data:
                return
            self.buf.extend(data)

    def send(self, keys, wait=0.4):
        os.write(self.fd, keys)
        self.pump(wait)

    def echo_is_off(self):
        """Whether the program has put the terminal in raw mode."""
        return not (termios.tcgetattr(self.fd)[3] & termios.ECHO)

    def screen(self):
        """A full repaint, as text.

        A terminal normally receives only the cells that changed, which cannot
        be reassembled into a screen. Resizing makes ratatui redraw everything.
        """
        self.buf.clear()
        self.resize(ROWS, COLS - 1)
        self.pump(0.35)
        self.buf.clear()
        self.resize(ROWS, COLS)
        self.pump(0.6)
        text = ANSI.sub(b"", bytes(self.buf)).decode("utf-8", "replace")
        return text.replace("\r", "\n")

    def finish(self, keys=b"q", wait=1.5):
        self.buf.clear()
        os.write(self.fd, keys)
        self.pump(wait)
        tail = bytes(self.buf)
        _, status = os.waitpid(self.pid, 0)
        return os.waitstatus_to_exitcode(status), tail


def shows(screen, phrase):
    flat = "".join(screen.split())
    return "".join(phrase.split()) in flat


class Report:
    def __init__(self):
        self.failures = 0

    def check(self, name, condition):
        if not condition:
            self.failures += 1
        print(f"  {'ok  ' if condition else 'FAIL'}  {name}")

    def section(self, title):
        print(f"\n{title}")


def main():
    binary = sys.argv[1] if len(sys.argv) > 1 else "target/release/oracle"
    if not os.path.exists(binary):
        print(f"no binary at {binary}; build it first", file=sys.stderr)
        return 2
    if "tui" not in subprocess.run(
        [binary, "help"], capture_output=True, text=True
    ).stdout:
        print("this build has no terminal interface; nothing to smoke", file=sys.stderr)
        return 0

    r = Report()

    r.section("The report screen")
    s = Session(binary)
    s.pump(2.5)
    r.check("raw mode is on while it runs", s.echo_is_off())
    report = s.screen()
    for phrase in ["oracle", "read-only", "findings", "detail", "quit"]:
        r.check(f"shows {phrase!r}", shows(report, phrase))

    r.section("The help overlay")
    s.send(b"?")
    help_screen = s.screen()
    for phrase in [
        "never runs a command",
        "copy this finding",
        "check the machine again",
        "Nothing here changes the system",
    ]:
        r.check(f"shows {phrase!r}", shows(help_screen, phrase))

    r.section("The ask screen")
    s.send(b"\x1b")
    s.send(b"a")
    s.send(b"why is the disk full")
    ask = s.screen()
    r.check("echoes what was typed", shows(ask, "why is the disk full"))
    r.check("prompts for a question", shows(ask, "press Enter"))

    r.section("Leaving with q")
    code, tail = s.finish(b"\x1b\x1bq")
    r.check("exits cleanly", code == 0)
    r.check("leaves the alternate screen", ALT_SCREEN_OFF in tail)
    r.check("shows the cursor again", CURSOR_ON in tail)

    r.section("Leaving with ctrl-c")
    s = Session(binary)
    s.pump(2.0)
    code, tail = s.finish(b"\x03")
    r.check("exits cleanly", code == 0)
    r.check("leaves the alternate screen", ALT_SCREEN_OFF in tail)
    r.check("shows the cursor again", CURSOR_ON in tail)

    print()
    if r.failures:
        print(f"{r.failures} check(s) failed")
        return 1
    print("all checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
