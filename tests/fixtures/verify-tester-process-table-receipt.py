#!/usr/bin/env python3
"""Bind the host fixture's receipt to its captured production profile."""

import hashlib
import json
from pathlib import Path
import shutil
import sys

evidence, authority = map(Path, sys.argv[1:3])
candidate, approval, request = sys.argv[3:]
matches = []
for path in authority.glob("receipt-*.json"):
    receipt = json.loads(path.read_text())
    if receipt.get("candidate_commit") == candidate and receipt.get("approval_id") == approval:
        matches.append((path, receipt))
if len(matches) != 1:
    raise SystemExit(f"expected one protected receipt for fixture, found {len(matches)}")

path, receipt = matches[0]
profile = (evidence / "selected-profile.sb").read_bytes()
profile_digest = "sha256:" + hashlib.sha256(profile).hexdigest()
if (receipt.get("request_digest") != request or
        receipt.get("execution_mode") != "macos-seatbelt" or
        receipt.get("host_platform") != "macos" or
        receipt.get("command_index") != 0 or
        receipt.get("exit_code") != 0 or
        receipt.get("profile_digest") != profile_digest):
    raise SystemExit("protected receipt does not prove the selected production profile")

shutil.copy2(path, evidence / "protected-receipt.json")
observed = {
    "candidate": candidate,
    "approval_id": approval,
    "request_digest": request,
    "receipt_id": path.name,
    "profile_digest": profile_digest,
    "execution_mode": receipt["execution_mode"],
    "admitted_at": receipt["admitted_at"],
    "settled_at": receipt["settled_at"],
}
(evidence / "observed-permission.json").write_text(json.dumps(observed, indent=2) + "\n")
