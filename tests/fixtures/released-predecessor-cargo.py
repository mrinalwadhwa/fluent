#!/usr/bin/env python3
"""Double Cargo to test declared predecessor setup in an isolated project."""

import os
from pathlib import Path
import sys

if sys.argv[1] == "build":
    # Satisfy the declared command's production-binary copy without building or
    # invoking Fluent. The behavior script runs this inside its private root.
    binary = Path("target/debug/fluent")
    binary.parent.mkdir(parents=True, exist_ok=True)
    binary.touch()
elif sys.argv[1:3] == ["nextest", "run"]:
    prepared = Path(os.environ["FLUENT_RELEASED_050_BIN"])
    assert prepared.read_bytes() == Path(os.environ["DOWNLOAD_SOURCE"]).read_bytes()
    Path("scenarios-started").touch()
else:
    sys.exit("unexpected Cargo invocation")
