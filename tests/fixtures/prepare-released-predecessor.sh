#!/usr/bin/env bash
# Prepare the published predecessor without consulting the installed Fluent.
set -euo pipefail

fixture_dir="$(cd "$(dirname "$0")" && pwd)"
command -v python3 >/dev/null || { printf 'error: install python3 to prepare the released predecessor\n' >&2; exit 1; }
python3 - "$fixture_dir" <<'PY'
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

fixtures = Path(sys.argv[1])
identity = json.loads((fixtures / "released-predecessor.json").read_text())
cache = fixtures.parent.parent / ".scratch/released-predecessor/fluent-v0.5.0"


def verify(binary):
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    if digest != identity["sha256"]:
        raise ValueError(f"SHA-256 mismatch for {binary}: expected {identity['sha256']}, got {digest}")
    # Never execute an unverified download or source override.
    result = subprocess.run([str(binary), "version"], capture_output=True, text=True,
                            env=dict(os.environ, FLUENT_NO_UPDATE_CHECK="1"), timeout=15)
    if result.returncode or result.stdout.strip() != identity["version"]:
        raise ValueError(f"version check failed for {binary}: expected {identity['version']!r}, "
                         f"got {result.stdout.strip()!r} (exit {result.returncode}); {result.stderr.strip()}")


try:
    source = os.environ.get("FLUENT_RELEASED_050_SOURCE")
    if source is None and cache.exists():
        verify(cache)
    else:
        cache.parent.mkdir(parents=True, exist_ok=True)
        # A private directory on the same filesystem makes concurrent preparation
        # safe: only a complete, verified file is renamed onto the cache path.
        with tempfile.TemporaryDirectory(prefix=".prepare-", dir=cache.parent) as temporary:
            binary = Path(temporary) / "fluent"
            if source is not None:
                try:
                    binary.write_bytes(Path(source).read_bytes())
                except OSError as error:
                    raise ValueError(f"cannot read FLUENT_RELEASED_050_SOURCE={source!r}: {error}") from error
            else:
                subprocess.run(["curl", "--fail", "--location", "--silent", "--show-error",
                                "--connect-timeout", "15", "--max-time", "120",
                                "--output", str(binary), identity["url"]], check=True)
            binary.chmod(0o755)
            verify(binary)
            os.replace(binary, cache)
    print(cache)
except (OSError, ValueError, subprocess.SubprocessError) as error:
    print(f"error: cannot prepare released predecessor: {error}\n"
          f"Use the published Apple Silicon v0.5.0 asset at {identity['url']}, or set "
          "FLUENT_RELEASED_050_SOURCE to a readable copy of that exact asset. "
          f"For an invalid cache entry, remove {cache} and prepare again. "
          "The executable requires macOS on Apple Silicon.", file=sys.stderr)
    sys.exit(1)
PY
