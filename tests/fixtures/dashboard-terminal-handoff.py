"""Run production terminal input and handoff under a real controlling PTY."""
import errno
import fcntl
import os
import pty
import select
import signal
import shlex
import struct
import subprocess
import sys
import tempfile
import termios
import time

mode = sys.argv[2] if len(sys.argv) > 2 else None
input_mode = mode in ("input", "control-input")
attachment_mode = mode if mode in ("observe", "control") else None
nested = tempfile.TemporaryDirectory(prefix="fluent-cancel-", dir="/private/tmp") if mode == "cancel-nested" else None
input_bytes = {
    "esc": b"\x1b",
    "interrupt": b"\x03",
    "prefix": b"\x02",
    "after_prefix": b"d",
    "paste": "\x1b[200~native-界\x1b[201~".encode(),
    "chord": b"\x1d",
    "enhanced_chord": b"\x1b[93;5u",
    "navigation": b"\x1b[B",
    "literal": b"]",
    "sentinel": b"Z",
    "navigate_chord": b"\x1d",
    "control_second": b"i",
    "second_input": b"\x1b[200~secondZ\x1b[201~",
}
pid, master = pty.fork()
if pid == 0:
    os.environ["FLUENT_HANDOFF_PTY_CHILD"] = "1"
    if attachment_mode:
        os.environ["FLUENT_ATTACHMENT_PTY_MODE"] = attachment_mode
    os.environ["TERM"] = "xterm-256color"
    fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    if nested:
        os.environ["FLUENT_CANCEL_NESTED_CHILD"] = nested.name
        os.environ.pop("TMUX", None)
        command = shlex.join([sys.argv[1], "--exact", "terminal_sessions::setup_cleanup_tests::nested_fullscreen_cancellation_child", "--nocapture"])
        os.execlp("tmux", "tmux", "-S", nested.name + "/outer", "-f", "/dev/null", "new-session", command)
    test = "fullscreen_attachment_pty_child" if attachment_mode else "fullscreen_pty_child"
    if input_mode:
        variable, test = (
            ("FLUENT_INPUT_PTY_CHILD", "terminal_input_pty_child")
            if mode == "input"
            else ("FLUENT_CONTROL_INPUT_PTY_CHILD", "terminal_control_input_pty_child")
        )
        os.environ[variable] = "1"
    os.execv(sys.argv[1], [sys.argv[1], "--exact", test, "--nocapture"])

output = bytearray()
deadline = time.monotonic() + (35 if nested else 15)
status = None
eof = False
resized = False
detached = False
sent = set()
try:
    # PTY EOF and waitpid become observable independently. Drain through EOF
    # and reap through exit, instead of treating the first EOF as a timeout.
    while time.monotonic() < deadline and (status is None or not eof):
        if not eof and select.select([master], [], [], 0.01)[0]:
            try:
                chunk = os.read(master, 65536)
            except OSError as error:
                if error.errno != errno.EIO:
                    raise
                chunk = b""
            if not chunk:
                eof = True
            output.extend(chunk)
            if input_mode:
                for step, data in input_bytes.items():
                    if step not in sent and f"INPUT_READY_{step}".encode() in output:
                        assert os.write(master, data) == len(data)
                        sent.add(step)
            if attachment_mode and not resized and b"RESIZE_ATTACH_READY" in output:
                fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 13, 73, 0, 0))
                resized = True
            if attachment_mode and not detached and b"DETACH_ATTACH_READY" in output:
                assert os.write(master, b"\x02d") == 2
                detached = True
            if len(output) > 1024 * 1024:
                raise AssertionError("unexpected terminal output flood")
        if status is None:
            done, result = os.waitpid(pid, os.WNOHANG)
            if done:
                status = result
            elif eof:
                time.sleep(0.01)
    if status is None:
        os.kill(pid, signal.SIGKILL)
        status = os.waitpid(pid, 0)[1]
        raise AssertionError("terminal handoff child did not settle")
    assert eof, "terminal handoff child left its PTY open"
    assert os.waitstatus_to_exitcode(status) == 0, output.decode(errors="replace")
    if nested:
        assert os.path.isfile(nested.name + "/passed"), output.decode(errors="replace")
        print("ok: nested full-screen cancellation, failed cleanup, and retry")
    elif input_mode:
        required = {"esc", "interrupt", "prefix", "after_prefix", "paste", "chord"}
        required |= {"enhanced_chord"} if mode == "input" else {
            "navigation", "literal", "sentinel", "navigate_chord", "control_second", "second_input"
        }
        assert sent == required, (sent, output)
        print(f"ok: real PTY {mode} decoded and routed")
    elif attachment_mode:
        assert resized, "the full-screen client never attached"
        assert detached, "the full-screen client never requested native detach"
        print(f"ok: full-screen {attachment_mode} resize and detach")
    else:
        assert output.count(b"\x1b[?1049h") == 4, output
        assert output.count(b"\x1b[?1049l") == 4, output
        assert output.count(b"\x1b[?2004h") == 4, output
        assert output.count(b"\x1b[?2004l") == 4, output
        print("ok: raw mode, alternate screen, paste, mouse preference, and dimensions restored")
finally:
    if status is None:
        os.kill(pid, signal.SIGKILL)
        os.waitpid(pid, 0)
    os.close(master)
    if nested:
        try:
            with open(nested.name + "/inner") as inner:
                subprocess.run(["tmux", "-S", inner.read(), "kill-server"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except FileNotFoundError:
            pass
        subprocess.run(["tmux", "-S", nested.name + "/outer", "kill-server"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        nested.cleanup()
