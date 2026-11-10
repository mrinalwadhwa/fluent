#!/usr/bin/env python3
"""Check that dashboard captures close menus and reap timed-out PTY children."""
import os
from pathlib import Path
import runpy
import sys
import tempfile
import unittest


class CaptureTests(unittest.TestCase):
    def setUp(self):
        self.capture = runpy.run_path(str(Path(__file__).with_name("dashboard-journey.py")))["capture"]
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.project = self.root / "project"
        self.project.mkdir()
        self.binary = self.root / "fluent"

    def fixture(self, mode):
        self.binary.write_text(f'''#!{sys.executable}
import os, time, tty
from pathlib import Path
Path("pid").write_text(str(os.getpid()))
tty.setraw(0)
if {mode!r} != "initial-stall":
    os.write(1, b"Work Items\\r\\nSource reads: 1 healthy\\r\\nq quit\\r\\n")
if {mode!r} in ("initial-stall", "exit-stall"):
    while True:
        time.sleep(60)
menu = False
while True:
    key = os.read(0, 1)
    if key == b"a":
        menu = True
        os.write(1, b"Fluent actions\\r\\n")
    elif key == b"\\x1b":
        menu = False
    elif key == b"q" and not menu:
        break
''')
        self.binary.chmod(0o755)

    def assert_reaped(self):
        pid = int((self.project / "pid").read_text())
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)
        with self.assertRaises(ChildProcessError):
            os.waitpid(pid, os.WNOHANG)

    def test_closes_action_menu_before_quitting(self):
        self.fixture("menu")
        output = self.capture(str(self.binary), self.project, b"a", initial_timeout=2, exit_timeout=0.3)
        self.assertIn(b"Fluent actions", output)
        self.assert_reaped()

    def test_reaps_child_when_initial_read_stalls(self):
        self.fixture("initial-stall")
        with self.assertRaisesRegex(TimeoutError, "initial read"):
            self.capture(str(self.binary), self.project, b"", initial_timeout=2, exit_timeout=0.3)
        self.assert_reaped()

    def test_reaps_child_when_quit_stalls(self):
        self.fixture("exit-stall")
        with self.assertRaisesRegex(TimeoutError, "quit"):
            self.capture(str(self.binary), self.project, b"", initial_timeout=2, exit_timeout=0.3)
        self.assert_reaped()


if __name__ == "__main__":
    unittest.main()
