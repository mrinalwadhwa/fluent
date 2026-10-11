#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "$0")/../../.." && pwd)"
python3 - "$project_dir" <<'PY'
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest

PROJECT = Path(sys.argv.pop())


class ReleasedPredecessorFixture(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="released fixture ")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        fixtures = self.root / "tests/fixtures"
        fixtures.mkdir(parents=True)
        self.helper = fixtures / "prepare-released-predecessor.sh"
        shutil.copyfile(PROJECT / "tests/fixtures" / self.helper.name, self.helper)
        identity = json.loads((PROJECT / "tests/fixtures/released-predecessor.json").read_text())
        self.source = self.root / "source with spaces"
        self.source.write_text('''#!/bin/sh
printf 'executed\\n' >> "$VERSION_CALLS"
printf '%s\\n' "${FIXTURE_VERSION:-fluent 0.5.0 6875b409}"
exit "${FIXTURE_VERSION_EXIT:-0}"
''')
        # Only the isolated helper copy trusts this small executable. Release
        # integration tests use the unchanged committed identity and real asset.
        identity["sha256"] = hashlib.sha256(self.source.read_bytes()).hexdigest()
        (fixtures / "released-predecessor.json").write_text(json.dumps(identity))
        self.cache = self.root / ".scratch/released-predecessor/fluent-v0.5.0"
        self.calls = self.root / "version-calls"
        self.downloads = self.root / "download-calls"
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        curl = bin_dir / "curl"
        curl.write_text('''#!/usr/bin/env python3
import os, pathlib, sys, time
args = sys.argv[1:]
assert args[-1] == os.environ["EXPECTED_URL"], args
assert "--fail" in args and "--location" in args, args
target = pathlib.Path(args[args.index("--output") + 1])
with open(os.environ["DOWNLOAD_CALLS"], "a") as log:
    log.write("download\\n")
target.write_bytes(b"partial")
if os.environ.get("DOWNLOAD_FAIL"):
    sys.exit(7)
if os.environ.get("DOWNLOAD_WAIT"):
    pathlib.Path(os.environ["DOWNLOAD_STARTED"]).touch()
    deadline = time.monotonic() + 10
    while not pathlib.Path(os.environ["DOWNLOAD_RELEASE"]).exists():
        assert time.monotonic() < deadline, "download was never released"
        time.sleep(0.01)
target.write_bytes(pathlib.Path(os.environ["DOWNLOAD_SOURCE"]).read_bytes())
''')
        curl.chmod(0o755)
        self.env = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}",
                        VERSION_CALLS=str(self.calls), DOWNLOAD_CALLS=str(self.downloads),
                        DOWNLOAD_SOURCE=str(self.source), EXPECTED_URL=identity["url"])
        self.env.pop("FLUENT_RELEASED_050_SOURCE", None)

    def prepare(self, **env):
        return subprocess.run(["bash", str(self.helper)], env=dict(self.env, **env),
                              capture_output=True, text=True, timeout=25)

    def assert_prepared(self, result):
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, f"{self.cache}\n")
        self.assertEqual(self.cache.read_bytes(), self.source.read_bytes())
        self.assertTrue(os.access(self.cache, os.X_OK))

    def assert_rejected(self, result, diagnostic):
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn(diagnostic, result.stderr)

    def test_source_with_spaces_and_cache_reuse(self):
        self.assert_prepared(self.prepare(FLUENT_RELEASED_050_SOURCE=str(self.source)))
        self.assert_prepared(self.prepare())
        self.assertEqual(self.calls.read_text().splitlines(), ["executed", "executed"])
        self.assertFalse(self.downloads.exists())

    def test_download_and_cache_reuse(self):
        self.assert_prepared(self.prepare())
        self.assert_prepared(self.prepare())
        self.assertEqual(self.downloads.read_text().splitlines(), ["download"])

    def test_changed_cache_is_rejected_before_execution(self):
        self.assert_prepared(self.prepare())
        self.cache.write_bytes(b"changed release")
        self.assert_rejected(self.prepare(), "SHA-256")
        self.assertEqual(self.calls.read_text().splitlines(), ["executed"])
        self.assertEqual(self.downloads.read_text().splitlines(), ["download"])

    def test_invalid_source_cannot_hide_behind_valid_cache(self):
        self.assert_prepared(self.prepare())
        invalid = self.root / "current installed release double"
        invalid.write_text("#!/bin/sh\nexit 99\n")
        original = self.cache.read_bytes()
        self.assert_rejected(self.prepare(FLUENT_RELEASED_050_SOURCE=str(invalid)), "SHA-256")
        self.assertEqual(self.cache.read_bytes(), original)
        self.assertEqual(self.calls.read_text().splitlines(), ["executed"])

    def test_missing_and_unreadable_sources(self):
        for source in [self.root / "missing", self.root]:
            with self.subTest(source=source):
                self.assert_rejected(self.prepare(FLUENT_RELEASED_050_SOURCE=str(source)), "read")
        self.source.chmod(0)
        self.addCleanup(self.source.chmod, 0o600)
        self.assert_rejected(self.prepare(FLUENT_RELEASED_050_SOURCE=str(self.source)), "read")
        self.assertFalse(self.cache.exists())
        self.assertFalse(self.downloads.exists())
        self.assertFalse(self.calls.exists())

    def test_wrong_version_and_failed_version_command(self):
        self.assert_rejected(self.prepare(FIXTURE_VERSION="fluent 0.6.0 wrong"), "version")
        self.assertFalse(self.cache.exists())
        self.assert_rejected(self.prepare(FIXTURE_VERSION_EXIT="1"), "version")
        self.assertFalse(self.cache.exists())
        self.assert_prepared(self.prepare())
        self.assert_rejected(self.prepare(FIXTURE_VERSION="fluent 0.6.0 wrong"), "version")

    def test_partial_download_is_never_published(self):
        self.assert_rejected(self.prepare(DOWNLOAD_FAIL="1"), "FLUENT_RELEASED_050_SOURCE")
        self.assertEqual(list(self.cache.parent.iterdir()), [])
        self.assertFalse(self.calls.exists())
        self.assert_prepared(self.prepare())

    def test_concurrent_downloads_publish_only_complete_bytes(self):
        started = self.root / "download-started"
        release = self.root / "download-release"
        env = dict(self.env, DOWNLOAD_WAIT="1", DOWNLOAD_STARTED=str(started),
                   DOWNLOAD_RELEASE=str(release))
        processes = []
        try:
            for _ in range(2):
                processes.append(subprocess.Popen(["bash", str(self.helper)], env=env,
                                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                                  text=True))
            deadline = time.monotonic() + 10
            while not (started.exists() and self.downloads.read_text().count("download") == 2):
                self.assertLess(time.monotonic(), deadline, "downloads did not start")
                time.sleep(0.01)
            self.assertFalse(self.cache.exists())
            release.touch()
            for process in processes:
                stdout, stderr = process.communicate(timeout=15)
                self.assert_prepared(subprocess.CompletedProcess([], process.returncode, stdout, stderr))
            self.assertEqual(list(self.cache.parent.iterdir()), [self.cache])
        finally:
            release.touch()
            for process in processes:
                if process.poll() is None:
                    process.kill()
                process.communicate()

    def test_declared_preparation_gates_scenario_execution(self):
        # Execute the real declared command; double only unrelated prerequisites
        # and Cargo so this focused check cannot launch the complete Rust suite.
        config = (PROJECT / ".fluent/tester.yaml").read_text()
        command = textwrap.dedent(config.split("  - command: |\n", 1)[1]
                                 .split("\n    test_harness:", 1)[0])
        for helper in re.findall(r"bash (tests/fixtures/[a-z0-9-]+\.sh)", command):
            if Path(helper).name != self.helper.name:
                (self.root / helper).write_text('#!/bin/sh\nprintf "/unused-old-reader\\n"\n')
        cargo = self.root / "bin/cargo"
        shutil.copyfile(PROJECT / "tests/fixtures/released-predecessor-cargo.py", cargo)
        cargo.chmod(0o755)
        env = dict(self.env, FLUENT_RELEASED_050_SOURCE=str(self.root / "missing"))
        env.pop("FLUENT_RELEASED_050_BIN", None)
        result = subprocess.run(["sh", "-c", command], cwd=self.root, env=env,
                                capture_output=True, text=True, timeout=25)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot read FLUENT_RELEASED_050_SOURCE", result.stderr)
        self.assertFalse((self.root / "scenarios-started").exists())
        self.assertFalse((self.root / "target").exists())
        env["FLUENT_RELEASED_050_SOURCE"] = str(self.source)
        result = subprocess.run(["sh", "-c", command], cwd=self.root, env=env,
                                capture_output=True, text=True, timeout=25)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((self.root / "scenarios-started").exists())


unittest.main()
PY
