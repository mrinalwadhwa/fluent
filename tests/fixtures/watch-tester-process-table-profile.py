#!/usr/bin/env python3
"""Expose the live production profile path to the host acceptance fixture."""

import os
from pathlib import Path
import sys
import time

store, path_file, evidence, ready = map(Path, sys.argv[1:])
rule = b'(allow sysctl-read (sysctl-name "kern.proc.all"))'
existing = {path.name for path in store.iterdir()}
ready_temporary = ready.with_name(f"{ready.name}.{os.getpid()}.tmp")
ready_temporary.write_text("ready\n")
os.replace(ready_temporary, ready)
deadline = time.monotonic() + 30

while time.monotonic() < deadline:
    for path in store.iterdir():
        if (path.name in existing or path.suffix == ".json"
                or path.is_symlink() or not path.is_file()):
            continue
        try:
            content = path.read_bytes()
        except FileNotFoundError:
            continue
        if rule not in content:
            continue
        evidence.write_bytes(content)
        temporary = path_file.with_name(f"{path_file.name}.{os.getpid()}.tmp")
        temporary.write_text(f"{path}\n")
        os.replace(temporary, path_file)
        sys.exit(0)
    time.sleep(0.05)

raise SystemExit("no live process-table Tester profile appeared")
