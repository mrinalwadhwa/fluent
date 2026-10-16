"""Run the production terminal guard under a real controlling PTY."""
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

attachment_mode = sys.argv[2] if len(sys.argv) > 2 else None
pid, master = pty.fork()
if pid == 0:
    os.environ["FLUENT_HANDOFF_PTY_CHILD"] = "1"
    if attachment_mode:
        os.environ["FLUENT_ATTACHMENT_PTY_MODE"] = attachment_mode
    os.environ["TERM"] = "xterm-256color"
    fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    test = "fullscreen_attachment_pty_child" if attachment_mode else "fullscreen_pty_child"
    os.execv(sys.argv[1], [sys.argv[1], "--exact", test, "--nocapture"])

output = bytearray()
deadline = time.monotonic() + 15
status = None
eof = False
resized = False
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
            if attachment_mode and not resized and b"RESIZE_ATTACH_READY" in output:
                fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 13, 73, 0, 0))
                resized = True
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
        os.waitpid(pid, 0)
        raise AssertionError("terminal handoff child did not settle")
    assert eof, "terminal handoff child left its PTY open"
    assert os.waitstatus_to_exitcode(status) == 0, output.decode(errors="replace")
    if attachment_mode:
        assert resized, "the full-screen client never attached"
        print(f"ok: full-screen {attachment_mode} resize and detach")
    else:
        assert output.count(b"\x1b[?1049h") == 4, output
        assert output.count(b"\x1b[?1049l") == 4, output
        assert output.count(b"\x1b[?2004h") == 4, output
        assert output.count(b"\x1b[?2004l") == 4, output
        print("ok: raw mode, alternate screen, paste, mouse preference, and dimensions restored")
finally:
    os.close(master)
