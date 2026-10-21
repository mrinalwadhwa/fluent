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

pid, master = pty.fork()
if pid == 0:
    os.environ["FLUENT_SSH_FULLSCREEN_CHILD"] = sys.argv[2]
    os.environ["TERM"] = "xterm-256color"
    fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    os.execv(sys.argv[1], [sys.argv[1], "--exact", "ssh_fullscreen_pty_child", "--nocapture"])
output = bytearray()
status = None
sent = False
opened = False
detach_at = None
eof = False
deadline = time.monotonic() + 25
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
                os.write(master, b"f")
                opened = True
            if detach_at is None and b"SSH_FULLSCREEN_PAINT" in output:
                # Pause display consumption long enough to fill the output PTY.
                # Idle input must not make stdout nonblocking and abort repaint.
                time.sleep(0.4)
                detach_at = time.monotonic() + 0.5
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
finally:
    if status is None:
        os.kill(pid, signal.SIGKILL)
        os.waitpid(pid, 0)
    os.close(master)
