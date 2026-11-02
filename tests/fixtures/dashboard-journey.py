#!/usr/bin/env python3
"""Drive the dashboard through a real PTY and inspect its current screen."""
import codecs
import errno
import fcntl
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import sys
import termios
import time


class Screen:
    """Project the cursor and text commands emitted by the dashboard's backend."""
    def __init__(self, width, height):
        self.width, self.height = width, height
        self.cells = [[" "] * width for _ in range(height)]
        self.row = self.col = 0
        self.pending = ""
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")

    def feed(self, data):
        self.pending += self.decoder.decode(data)
        while self.pending:
            if self.pending.startswith("\x1b["):
                match = re.match(r"\x1b\[([0-?]*)([ -/]*)([@-~])", self.pending)
                if not match:
                    return
                args, _, code = match.groups()
                self.pending = self.pending[match.end():]
                if args.startswith("?"):
                    if args == "?1049" and code == "h":
                        self.cells = [[" "] * self.width for _ in range(self.height)]
                    continue
                values = [int(value or 0) for value in args.split(";")]
                amount = values[0] or 1
                if code in "Hf":
                    self.row = min(self.height - 1, amount - 1)
                    self.col = min(self.width - 1, (values[1] if len(values) > 1 else 1) - 1)
                elif code == "A":
                    self.row = max(0, self.row - amount)
                elif code == "B":
                    self.row = min(self.height - 1, self.row + amount)
                elif code == "C":
                    self.col = min(self.width - 1, self.col + amount)
                elif code == "D":
                    self.col = max(0, self.col - amount)
                elif code == "G":
                    self.col = min(self.width - 1, amount - 1)
                elif code == "J" and values[0] in (2, 3):
                    self.cells = [[" "] * self.width for _ in range(self.height)]
                elif code == "K":
                    start, end = (0, self.width) if values[0] == 2 else ((0, self.col + 1) if values[0] == 1 else (self.col, self.width))
                    self.cells[self.row][start:end] = [" "] * (end - start)
                continue
            if self.pending[0] == "\x1b":
                if len(self.pending) < 2:
                    return
                # The dashboard uses CSI for drawing; ignore other two-byte modes.
                self.pending = self.pending[2:]
                continue
            char, self.pending = self.pending[0], self.pending[1:]
            if char == "\r":
                self.col = 0
            elif char == "\n":
                self.row = min(self.height - 1, self.row + 1)
            elif char >= " ":
                self.cells[self.row][self.col] = char
                self.col = min(self.width - 1, self.col + 1)

    def text(self):
        return "\n".join("".join(row) for row in self.cells)


def snapshot(root):
    return {str(path.relative_to(root)): path.read_bytes() for path in root.rglob("*") if path.is_file()}


def journey(binary, project, mode):
    before = snapshot(project / ".fluent/work")
    width, height = (100, 24) if mode == "help" else (80, 15)
    child, master = pty.fork()
    if child == 0:
        fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0))
        os.chdir(project)
        os.environ["TERM"] = "xterm-256color"
        os.environ["HOME"] = str(project.parent)
        os.environ["FLUENT_NO_UPDATE_CHECK"] = "1"
        os.execv(binary, [binary, "dashboard", str(project)])
    screen = Screen(width, height)
    exited = False
    refresh_gate = project / ".fluent/work/items/refresh-gate.json"
    gate_writer = None

    def wait_for(*required, absent=()):
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            if all(word in screen.text() for word in required) and all(word not in screen.text() for word in absent):
                print(screen.text())
                return
            if select.select([master], [], [], 0.05)[0]:
                try:
                    data = os.read(master, 65536)
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
                    break
                if not data:
                    break
                screen.feed(data)
        raise AssertionError(f"Expected {required}, absent {absent}\n{screen.text()}")

    try:
        wait_for("Tasks", "Visible Work")
        if mode == "help":
            os.write(master, b"?")
            wait_for("Help", "↑/↓ select", "→ details", "Enter workers", "Tab/⇧Tab view", "?/Esc close help", absent=("Visible Work", "i Control", "f full-screen"))
            os.write(master, b"\x1b")
            wait_for("Tasks", "Visible Work", absent=("?/Esc close help",))
        elif mode == "refresh":
            wait_for("> ▸ Visible Work", "Second Work")
            # Opening the writer proves that a real snapshot read has reached
            # the FIFO. Retain it without bytes or EOF until navigation ends.
            os.mkfifo(refresh_gate)
            os.write(master, b"r")
            deadline = time.monotonic() + 5
            while gate_writer is None:
                try:
                    gate_writer = os.open(refresh_gate, os.O_WRONLY | os.O_NONBLOCK)
                except OSError as error:
                    if error.errno != errno.ENXIO:
                        raise
                    assert time.monotonic() < deadline, "local snapshot read did not start"
                    time.sleep(0.01)
            os.write(master, b"j")
            wait_for("> ▸ Second Work")
            os.write(master, b"\t")
            wait_for("No Work Items in this view", absent=("Second Work",))
            os.write(master, b"\x1b[Z")
            wait_for("Visible Work", "Second Work", absent=("No Work Items in this view",))
        else:
            assert "broken-29" not in screen.text()
            os.write(master, b"e")
            wait_for("Work Item read errors", "e/Esc back", "broken-00")
            for _ in range(100):
                os.write(master, b"\x1b[6~")
            wait_for("broken-29")
            os.write(master, b"\x1b")
            wait_for("Tasks", "Visible Work", absent=("e/Esc back",))
            os.write(master, b"\x1b[C")
            wait_for("Work Item: work-visible", "←/Esc list")
            os.write(master, b"\x1b")
            wait_for("Tasks", "Visible Work", absent=("←/Esc list",))
        os.write(master, b"q")
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            pid, status = os.waitpid(child, os.WNOHANG)
            if pid:
                exited = True
                assert os.waitstatus_to_exitcode(status) == 0
                break
            if select.select([master], [], [], 0.02)[0]:
                try:
                    data = os.read(master, 65536)
                    if data:
                        screen.feed(data)
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
        assert exited, "dashboard did not quit"
    finally:
        if not exited:
            try:
                os.kill(child, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(child, 0)
        os.close(master)
        if gate_writer is not None:
            os.close(gate_writer)
        if mode == "refresh":
            refresh_gate.unlink(missing_ok=True)
    assert snapshot(project / ".fluent/work") == before, "dashboard changed stored Work bytes"


if __name__ == "__main__":
    journey(str(Path(sys.argv[1]).resolve()), Path(sys.argv[2]).resolve(), sys.argv[3])
