"""Drive the real full-screen SSH display through a controlling PTY."""
import errno
import fcntl
import os
import pty
import select
import signal
import struct
import sys
import termios
import time
import tempfile
import json
from pathlib import Path

scenario = sys.argv[4] if len(sys.argv) > 4 else "drain"
unread = len(sys.argv) > 5 and sys.argv[5] == "unread"
signals = tempfile.TemporaryDirectory(prefix="ssh-display-")
os.environ["FLUENT_SSH_FULLSCREEN_SIGNALS"] = signals.name
os.environ["FLUENT_SSH_FULLSCREEN_SCENARIO"] = scenario
pid, master = pty.fork()
if pid == 0:
    Path(signals.name, "initial-modes").write_text(repr(termios.tcgetattr(0)))
    if unread:
        os.environ["FLUENT_SSH_FULLSCREEN_UNREAD"] = "1"
    os.environ["FLUENT_SSH_FULLSCREEN_CHILD"] = sys.argv[2]
    os.environ["TERM"] = "xterm-256color"
    fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    os.execv(sys.argv[1], [sys.argv[1], "--exact", "ssh_fullscreen_pty_child", "--nocapture"])
output = bytearray()
status = None
sent = False
opened = False
input_case = os.environ.get("FLUENT_SSH_PARTIAL_INPUT")
partial = input_case in ("1", "partial")
suffix_sent = False
navigation_sent = False
detach_at = None
eof = False
deadline = time.monotonic() + 35
started = time.monotonic()
phases = [{"phase": "spawned", "elapsed": 0}]
try:
    while time.monotonic() < deadline and (status is None or not eof):
        if not eof and select.select([master], [], [], 0.01)[0]:
            try:
                chunk = os.read(master, 65536)
            except OSError as error:
                if error.errno != errno.EIO:
                    raise
                chunk = b""
            eof = not chunk
            output.extend(chunk)
            if not opened and b"SSH_DASHBOARD_READY" in output:
                os.write(master, b"f\x1b[<0;" if partial else b"f")
                opened = True
                phases.append({"phase": "dashboard-ready", "elapsed": time.monotonic() - started})
            if input_case and not suffix_sent and b"SSH_FULLSCREEN_PAINT" in output:
                payload = {
                    "1": b"10;10M", "partial": b"10;10M",
                    "terminal-report": b"\x1b[?2026;0$y",
                    "fragmented-report": b"\x1b[?2026;",
                    "unfinished": b"\x1b[<0;",
                }.get(input_case, b"")
                os.write(master, payload)
                if input_case == "fragmented-report":
                    time.sleep(0.05)
                    os.write(master, b"0$y")
                suffix_sent = True
            if input_case and not navigation_sent and b"SSH_FULLSCREEN_RETURNED" in output:
                os.write(master, b"ee\x1b[B\x1b[A")
                navigation_sent = True
            if detach_at is None and b"SSH_FULLSCREEN_PAINT" in output:
                phases.append({"phase": "fullscreen-paint", "elapsed": time.monotonic() - started})
                # Pause display consumption long enough to fill the output PTY.
                # Idle input must not make stdout nonblocking and abort repaint.
                stall = float(sys.argv[3]) if len(sys.argv) > 3 else 0.4
                if scenario != "drain":
                    if scenario == "detach" and sys.argv[2] == "control":
                        os.write(master, b"x")
                    time.sleep(0.3)
                    if scenario == "detach":
                        os.write(master, b"\x02d")
                    else:
                        Path(signals.name, "trigger").touch()
                    sent = True
                    # Keep withholding output after the lifecycle event.
                    if unread:
                        settle_deadline = time.monotonic() + 15
                        while (not Path(signals.name, "local-settled").exists()
                               and time.monotonic() < settle_deadline):
                            time.sleep(0.02)
                        assert Path(signals.name, "local-settled").exists(), "local cleanup waited for unread PTY"
                        modes = termios.tcgetattr(master)
                        assert modes[3] & termios.ICANON and modes[3] & termios.ECHO, "raw terminal modes survived cleanup"
                        phases.append({"phase": "settled-before-drain", "elapsed": time.monotonic() - started})
                    else:
                        time.sleep(stall - 0.3)
                    if scenario == "detach" and sys.argv[2] == "control":
                        assert Path(signals.name, "client-detached").exists(), "client detach stalled behind echo"
                    elif scenario != "detach":
                        assert Path(signals.name, "settled").exists(), "lifecycle stalled behind display"
                else:
                    time.sleep(stall)
                detach_at = time.monotonic() + 0.5
                phases.append({"phase": "reader-resumed", "elapsed": time.monotonic() - started})
            if not sent:
                assert b"SSH_FULLSCREEN_RETURNED" not in output, output.decode(errors="replace")
            assert len(output) < 1024 * 1024, "display flood"
        if (not sent and detach_at is not None and time.monotonic() >= detach_at
                and b"SSH_FULLSCREEN_PAINT_END" in output):
            os.write(master, b"\x02d")
            sent = True
        if status is None:
            done, result = os.waitpid(pid, os.WNOHANG)
            if done:
                status = result
    assert status is not None and eof, output.decode(errors="replace")
    assert sent and os.waitstatus_to_exitcode(status) == 0, output.decode(errors="replace")
    assert repr(termios.tcgetattr(master)) == Path(signals.name, "initial-modes").read_text(), "terminal flags or control characters changed"
    if not unread:
        assert b"SSH_FULLSCREEN_RETURNED" in output
    assert b"\x1b[?1049h" in output and output.rfind(b"\x1b[?1049l") > output.rfind(b"\x1b[?1049h")
    assert b"\x1b[?2004h" in output and output.rfind(b"\x1b[?2004l") > output.rfind(b"\x1b[?2004h")
    pressure = "unread until local settlement" if unread else f"reader stall {sys.argv[3] if len(sys.argv)>3 else '0.4'}s"
    print(f"ok controlled PTY: {sys.argv[2]} {scenario}, {pressure}")
finally:
    if status is None:
        # Ask the Rust owner to settle its separate worker session before killing
        # the PTY process group. Drain output so its terminal guard can finish.
        Path(signals.name, "cleanup").touch()
        cleanup_deadline = time.monotonic() + 8
        while status is None and time.monotonic() < cleanup_deadline:
            if select.select([master], [], [], 0.05)[0]:
                try:
                    output.extend(os.read(master, 65536))
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
            done, result = os.waitpid(pid, os.WNOHANG)
            if done:
                status = result
        if status is None:
            os.killpg(pid, signal.SIGKILL)
            _, status = os.waitpid(pid, 0)
    os.close(master)
    evidence = os.environ.get("FLUENT_SSH_PTY_EVIDENCE_DIR")
    if evidence:
        name = f"{sys.argv[2]}-{scenario}-{sys.argv[3] if len(sys.argv)>3 else '0.4'}"
        if unread:
            name += "-unread"
        Path(evidence, name + ".pty").write_bytes(output)
        Path(evidence, name + ".json").write_text(json.dumps({
            "mode": sys.argv[2], "scenario": scenario,
            "stall_seconds": float(sys.argv[3]) if len(sys.argv)>3 else 0.4,
            "unread_until_settlement": unread,
            "wait_status": status, "display_bytes": len(output),
            "phases": phases, "elapsed": time.monotonic() - started,
            "initial_size": [100, 30], "test_binary": sys.argv[1],
            "transport": "hermetic OpenSSH/tmux clients; production helper and full-screen loop"
        }, indent=2) + "\n")
    signals.cleanup()
