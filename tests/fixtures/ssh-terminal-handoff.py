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
            if not sent and b"SSH_FULLSCREEN_PAINT" in output:
                os.write(master, b"\x02d")
                sent = True
            assert len(output) < 1024 * 1024, "display flood"
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
