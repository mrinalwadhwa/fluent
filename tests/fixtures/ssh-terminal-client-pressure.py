"""Check that the tmux substitute can detach while its display is blocked."""
import os
from pathlib import Path
import pty
import select
import subprocess
import sys
import tempfile
import time

with tempfile.TemporaryDirectory(prefix="ssh-client-pressure-") as root:
    log = Path(root, "input")
    master, slave = pty.openpty()
    environment = dict(os.environ, FLUENT_TEST_TERMINAL_CLIENT_LOG=str(log),
                       FLUENT_TEST_TERMINAL_OUTPUT_LINES="16000")
    child = subprocess.Popen(
        [sys.executable, "-I", "-S", "tests/fixtures/ssh-terminal-client.py", "control"],
        stdin=slave, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=environment,
    )
    os.close(slave)
    try:
        assert select.select([child.stdout], [], [], 5)[0], "client did not start"
        # Leave the repaint unread. An ordinary key must not block the input
        # reader in an echo write before it can process a subsequent detach.
        time.sleep(0.2)
        os.write(master, b"x")
        deadline = time.monotonic() + 2
        while not log.exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        assert log.exists() and log.read_bytes() == b"x", "client did not receive input"
        time.sleep(0.1)
        assert child.poll() is None, "client exited before detach"
        os.write(master, b"\x02d")
        try:
            child.wait(timeout=2)
        except subprocess.TimeoutExpired:
            raise AssertionError("client could not detach without draining its display") from None
        assert child.returncode == 0, child.stderr.read().decode(errors="replace")
        assert log.read_bytes() == b"x\x02d"
        # A pipe retains queued display bytes after the writer exits, unlike
        # a macOS PTY that can discard its unread output on slave close.
        output = child.stdout.read()
        assert output.startswith(b"\x1b[2J\x1b[HSSH_FULLSCREEN_PAINT")
        assert b"SSH_FULLSCREEN_PAINT_END" not in output
        print("ok client receives input and detaches before display drain")
    finally:
        if child.poll() is None:
            child.kill()
        child.wait()
        child.stdout.close()
        child.stderr.close()
        os.close(master)
