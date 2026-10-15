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

pid, master = pty.fork()
if pid == 0:
    os.environ["FLUENT_HANDOFF_PTY_CHILD"] = "1"
    os.environ["TERM"] = "xterm-256color"
    fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    os.execv(sys.argv[1], [sys.argv[1], "--exact", "fullscreen_pty_child", "--nocapture"])

output = bytearray()
deadline = time.monotonic() + 15
status = None
try:
    while time.monotonic() < deadline:
        if select.select([master], [], [], 0.05)[0]:
            try:
                chunk = os.read(master, 65536)
            except OSError as error:
                if error.errno == errno.EIO:
                    break
                raise
            if not chunk:
                break
            output.extend(chunk)
            if len(output) > 1024 * 1024:
                raise AssertionError("unexpected terminal output flood")
        done, result = os.waitpid(pid, os.WNOHANG)
        if done:
            status = result
            break
    if status is None:
        done, status = os.waitpid(pid, os.WNOHANG)
        if not done:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
            raise AssertionError("terminal handoff child did not settle")
    assert os.waitstatus_to_exitcode(status) == 0, output.decode(errors="replace")
    assert output.count(b"\x1b[?1049h") == 4, output
    assert output.count(b"\x1b[?1049l") == 4, output
    assert output.count(b"\x1b[?2004h") == 4, output
    assert output.count(b"\x1b[?2004l") == 4, output
    print("ok: raw mode, alternate screen, paste, mouse preference, and dimensions restored")
finally:
    os.close(master)
