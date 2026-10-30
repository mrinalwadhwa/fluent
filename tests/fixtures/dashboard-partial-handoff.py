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

input_case = sys.argv[4]
pressure = len(sys.argv) > 5 and sys.argv[5] == "pressure"

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
pressure = os.environ["FLUENT_PARTIAL_HANDOFF_PRESSURE"] == "1"
if pressure:
    os.set_blocking(1, False)
data = bytearray()
while b"\\x02d" not in data and b"\\x1b[98;5ud" not in data:
    if Path(os.environ["FLUENT_PARTIAL_HANDOFF_FAILURE"]).exists():
        sys.exit(7)
    if pressure:
        try:
            os.write(1, b"\\x1b[H" + b"display-pressure" * 512)
        except BlockingIOError:
            Path(os.environ["FLUENT_PARTIAL_HANDOFF_FAILURE"]).with_name(
                "backpressure-" + str(os.getpid())
            ).touch()
    if select.select([0], [], [], 0.001 if pressure else 0.01)[0]:
        chunk = os.read(0, 8192)
        with open(os.environ["FLUENT_PARTIAL_HANDOFF_CLIENT_INPUT"], "ab") as receipt:
            receipt.write(chunk)
        data.extend(chunk)
sys.exit(0)
''')
    client.chmod(0o700)
    pid, master = pty.fork()
    if pid == 0:
        Path(directory, "initial-modes").write_text(repr(termios.tcgetattr(0)))
        fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
        os.environ.update(TERM="xterm-256color", FLUENT_PARTIAL_HANDOFF_MODE=sys.argv[2],
                          FLUENT_PARTIAL_HANDOFF_OUTCOME=sys.argv[3],
                          FLUENT_PARTIAL_HANDOFF_PRESSURE="1" if pressure else "0",
                          FLUENT_PARTIAL_HANDOFF_FAILURE=str(Path(directory, "fail")),
                          FLUENT_PARTIAL_HANDOFF_CLIENT_PIDS=str(Path(directory, "client-pids")),
                          FLUENT_PARTIAL_HANDOFF_CLIENT_INPUT=str(Path(directory, "client-input")),
                          FLUENT_PARTIAL_HANDOFF_WORKER_PID=str(Path(directory, "worker-pid")),
                          FLUENT_TEST_TERMINAL_CLIENT=str(client))
        os.execv(sys.argv[1], [sys.argv[1], "--exact", "partial_handoff_pty_child", "--nocapture"])
    output = bytearray()
    sent = set()
    status = None
    fail_at = None
    detach_at = None
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
                    (b"PARTIAL_FULLSCREEN_READY", {
                        "partial": b"10;10M",
                        "terminal-report": b"\x1b[?2026;0$y",
                        "separated-report": b"\x1b[?2026;0$y",
                        "literal-paste": b"\x1b[200~\x02x\x1b[?2026;0$y\x1b[201~",
                        "fragmented-report": b"\x1b[?2026;",
                        "unfinished": b"\x1b[<0;",
                    }.get(input_case, b"")),
                    (b"PARTIAL_DASHBOARD_RETURNED", b"ee\x1b[B\x1b[A"),
                ]:
                    if marker in output and marker not in sent:
                        if pressure and marker == b"PARTIAL_FULLSCREEN_READY":
                            # Observe actual producer backpressure before
                            # detaching. Keep this native client's exact PID;
                            # returning may create another embedded client.
                            client_pid = int(Path(directory, "client-pids").read_text().split()[-1])
                            blocked = Path(directory, "backpressure-" + str(client_pid))
                            deadline_pressure = time.monotonic() + 1
                            while not blocked.exists() and time.monotonic() < deadline_pressure:
                                time.sleep(0.01)
                            assert blocked.exists(), "native display never reached backpressure"
                        os.write(master, payload)
                        if marker == b"PARTIAL_FULLSCREEN_READY":
                            # Let the reader consume the report or incomplete
                            # sequence before ordinary native detach arrives.
                            detach_at = time.monotonic() + 0.1
                        if pressure and marker == b"PARTIAL_FULLSCREEN_READY":
                            os.write(master, b"\x02d")
                            detach_at = None
                            deadline_detach = time.monotonic() + 1
                            while time.monotonic() < deadline_detach:
                                try:
                                    os.kill(client_pid, 0)
                                except ProcessLookupError:
                                    break
                                time.sleep(0.01)
                            else:
                                raise AssertionError("native detach waited for display drain")
                        sent.add(marker)
                        if marker == b"PARTIAL_FULLSCREEN_READY" and sys.argv[3] == "failure":
                            fail_at = time.monotonic() + 0.1
            if detach_at is not None and time.monotonic() >= detach_at:
                if input_case == "fragmented-report":
                    os.write(master, b"0$y")
                if sys.argv[3] == "detach":
                    if input_case == "separated-report":
                        os.write(master, b"\x02")
                        time.sleep(0.03)
                        os.write(master, b"d")
                    else:
                        os.write(master, b"\x02d")
                detach_at = None
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
        receipt = Path(directory, "client-input")
        received = receipt.read_bytes() if receipt.exists() else b""
        if sys.argv[2] == "observe":
            assert not received, f"Observe forwarded native input: {received!r}"
        elif input_case == "literal-paste":
            # The native paste encoder can sanitize ESC. Ctrl-B must remain
            # paste content, rather than become an enhanced-key event.
            assert b"\x02x" in received, f"paste control byte changed: {received!r}"
        print("ok: ordinary errors overlay and selection after full-screen return")
        if pressure:
            print("ok: detached before display drain")
    finally:
        if status is None:
            # Native clients and the worker lead separate process groups.
            # Kill each owned group before reaping the Dashboard, then verify
            # settlement so a timeout cannot silently leave descendants alive.
            groups = {pid}
            for name in ("client-pids", "worker-pid"):
                path = Path(directory, name)
                for owned_pid in path.read_text().split() if path.exists() else []:
                    groups.add(int(owned_pid))
            for group in groups:
                try:
                    os.killpg(group, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            os.waitpid(pid, 0)
            cleanup_deadline = time.monotonic() + 2
            while groups and time.monotonic() < cleanup_deadline:
                for group in list(groups):
                    try:
                        os.killpg(group, 0)
                    except ProcessLookupError:
                        groups.remove(group)
                if groups:
                    time.sleep(0.01)
            assert not groups, f"owned process groups survived timeout cleanup: {groups}"
        os.close(master)
