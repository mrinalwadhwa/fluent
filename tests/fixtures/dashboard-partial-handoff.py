"""Split a real input sequence across Dashboard and native display entry."""
import errno
import fcntl
import struct
import os
import pty
import select
import signal
import sys
import tempfile
import termios
import time
from pathlib import Path

with tempfile.TemporaryDirectory(prefix="partial-handoff-") as directory:
    client = Path(directory, "client.py")
    client.write_text('''#!/usr/bin/env python3
import os, sys, tty, termios, atexit, select
from pathlib import Path
with open(os.environ["FLUENT_PARTIAL_HANDOFF_CLIENT_PIDS"], "a") as pids:
    pids.write(str(os.getpid()) + "\\n")
original = termios.tcgetattr(0)
atexit.register(termios.tcsetattr, 0, termios.TCSANOW, original)
tty.setraw(0)
os.write(1, b"\\x1b[>1uPARTIAL_FULLSCREEN_READY")
data = bytearray()
while b"\\x02d" not in data and b"\\x1b[98;5ud" not in data:
    if Path(os.environ["FLUENT_PARTIAL_HANDOFF_FAILURE"]).exists():
        sys.exit(7)
    if select.select([0], [], [], 0.01)[0]:
        data.extend(os.read(0, 8192))
sys.exit(0)
''')
    client.chmod(0o700)
    pid, master = pty.fork()
    if pid == 0:
        Path(directory, "initial-modes").write_text(repr(termios.tcgetattr(0)))
        fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
        os.environ.update(TERM="xterm-256color", FLUENT_PARTIAL_HANDOFF_MODE=sys.argv[2],
                          FLUENT_PARTIAL_HANDOFF_OUTCOME=sys.argv[3],
                          FLUENT_PARTIAL_HANDOFF_FAILURE=str(Path(directory, "fail")),
                          FLUENT_PARTIAL_HANDOFF_CLIENT_PIDS=str(Path(directory, "client-pids")),
                          FLUENT_PARTIAL_HANDOFF_WORKER_PID=str(Path(directory, "worker-pid")),
                          FLUENT_TEST_TERMINAL_CLIENT=str(client))
        os.execv(sys.argv[1], [sys.argv[1], "--exact", "partial_handoff_pty_child", "--nocapture"])
    output = bytearray()
    sent = set()
    status = None
    fail_at = None
    deadline = time.monotonic() + 6
    try:
        while time.monotonic() < deadline:
            if select.select([master], [], [], 0.01)[0]:
                try:
                    data = os.read(master, 65536)
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
                    data = b""
                output.extend(data)
                for marker, payload in [
                    (b"PARTIAL_DASHBOARD_READY", b"f\x1b[<0;" if sys.argv[4] == "partial" else b"f"),
                    (b"PARTIAL_FULLSCREEN_READY", (b"10;10M" if sys.argv[4] == "partial" else b"") + (b"\x02d" if sys.argv[3] == "detach" else b"")),
                    (b"PARTIAL_DASHBOARD_RETURNED", b"ee\x1b[B\x1b[A"),
                ]:
                    if marker in output and marker not in sent:
                        os.write(master, payload)
                        sent.add(marker)
                        if marker == b"PARTIAL_FULLSCREEN_READY" and sys.argv[3] == "failure":
                            fail_at = time.monotonic() + 0.1
            if fail_at is not None and time.monotonic() >= fail_at:
                Path(directory, "fail").touch()
                fail_at = None
            done, status_value = os.waitpid(pid, os.WNOHANG)
            if done:
                status = status_value
                break
        assert status is not None, "Dashboard input stalled: " + output.decode(errors="replace")
        assert os.waitstatus_to_exitcode(status) == 0, output.decode(errors="replace")
        assert len(sent) == 3, output.decode(errors="replace")
        assert repr(termios.tcgetattr(master)) == Path(directory, "initial-modes").read_text(), "terminal flags or control characters changed"
        print("ok: ordinary errors overlay and selection after full-screen return")
    finally:
        if status is None:
            # The native clients and worker own separate sessions. Retain their
            # exact fixture-created PIDs so the watchdog cannot orphan them.
            for name in ("client-pids", "worker-pid"):
                path = Path(directory, name)
                for owned_pid in path.read_text().split() if path.exists() else []:
                    try:
                        os.kill(int(owned_pid), signal.SIGKILL)
                    except ProcessLookupError:
                        pass
            os.killpg(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
        os.close(master)
