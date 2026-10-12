"""Regress release controls without starting host services or model checkers."""

import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from subprocess import CompletedProcess
from unittest.mock import patch

import check
import project_release
import report
from release_evidence import verify_release_evidence


def service_fixture():
    old = {"instance_id": None, "state": None, "build": "old", "generation": 1}
    new = {"instance_id": "new-owner", "state": "running", "build": "new", "generation": 1}
    workers, active, artifacts = [], [], {}
    for pid, work in enumerate(("first", "second"), 10):
        worker = {"work_item_id": work, "dispatch_id": work, "attempt_id": "attempt-1",
                  "project": work, "workspace": work, "receipt": work, "pid": pid,
                  "task_id": "writer", "task-status": "complete", "execution-state": "settled",
                  "process-group-absent": True, "status-before-stop": "running",
                  "status-after-stop": "candidate-ready",
                  "launches": [{"pid": pid, "process-group-absent": True}]}
        for stage, state in (("initial", "running"), ("final", "settled")):
            name = f"service-ownership/{work}-{stage}-receipt.json"
            receipt = {"state": state, "process_group_id": pid, "attempt_owner": work}
            worker[f"{stage}-receipt"] = receipt
            worker[f"{stage}-receipt-artifact"] = name
            artifacts[name] = json.dumps(receipt).encode()
        workers.append(worker)
        active.append({"work_item_id": work, "dispatch_id": work,
                       "bound_attempt_id": "attempt-1", "status": "candidate-ready"})
    for stream in ("stdout", "stderr"):
        artifacts[f"service-ownership/released-service.{stream}"] = b""
    queues = [{"work_item_id": work, "status": "queued"} for work in ("first", "second")]
    facts = [
        {"health": old, "predecessor-sha256": project_release.RELEASE_SHA,
         "registered-projects": 2, "coordinators": ["old-first", "old-second"],
         "incompatible-start": {"exit": 1, "stderr": "lacks verifiable drain control",
                                "health-after": old}},
        {"stop-exit-codes": [0, 0], "stop-stdout": ["", ""], "owned-worker-started": True,
         "owned-workers": workers, "active-work": active, "queues": queues},
        {"health": new, "registered-projects": 2, "queues": queues,
         "stale-stop": {"requested-coordinator": "old-first",
                        "request-path": "first/.fluent/work/scheduler/stop-request.json",
                        "request-processed": True, "coordinator-after": new,
                        "admitted-work": {"work_item_id": "first-after-stale", "status": "queued"},
                        "health-after": new}},
    ]
    events = [{"sequence": i, "boundary": boundary, "operation-id": operation,
               "causal-predecessors": [] if i == 1 else [i - 1], "raw-facts": raw}
              for i, (boundary, operation, raw) in enumerate(zip(
                  ("old-owner", "old-owner-settled", "new-owner"),
                  ("scheduler-run", "scheduler-stop", "scheduler-run"), facts), 1)]
    return {"scenario": "released-shared-service-transition", "events": events}, artifacts


def write_collection(directory, reverse):
    """Supply projector fixtures where the collection command requests them."""
    trace, artifacts = service_fixture()
    if reverse:
        trace["events"][1]["raw-facts"]["owned-workers"].reverse()

    def write(name, value, files):
        value.update({"schema-version": 1, "fluent-commit": "candidate",
                      "fluent-binary-sha256": hashlib.sha256(b"candidate").hexdigest(),
                      "model-profile": "release-transition", "collection-complete": True,
                      "artifacts": []})
        for relative, raw in files.items():
            path = directory / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(raw)
            value["artifacts"].append({"path": relative, "sha256": report.digest(path)})
        (directory / f"{name}.json").write_text(json.dumps(value))

    write("service-ownership", trace, artifacts)
    for scenario, item_id in (("current-authority", "released-refusal"),
                              ("pending-publication", "released-pending-refusal")):
        files = {f"{scenario}/item.json": json.dumps({"model_writer_version": {
            "format": "fluent-work-item-writer", "schema_version": 11}}).encode()}
        protected = [{"path": f"items/{item_id}.json", "kind": "file",
                      "artifact": f"{scenario}/item.json",
                      "sha256": hashlib.sha256(next(iter(files.values()))).hexdigest()}]
        diagnostic = f"unsupported Work Item writer envelope format/schema items/{item_id}.json"
        if scenario == "pending-publication":
            name = f"{scenario}/transaction.json"
            raw = json.dumps({"schema_version": 13, "id": item_id}).encode()
            files[name] = raw
            protected.append({"path": f"transactions/{item_id}.json", "kind": "file",
                              "artifact": name, "sha256": hashlib.sha256(raw).hexdigest()})
            diagnostic = (f"invalid work model transactions/{item_id}.json "
                          f'work item transaction id "{item_id}" cannot be used as a file name')
        value = {"scenario": scenario, "events": [
            {"sequence": 1, "boundary": "before-refusal", "causal-predecessors": [],
             "actor": "current-reader", "operation-id": "snapshot",
             "raw-facts": {"protected": protected, "predecessor-sha256": project_release.RELEASE_SHA}},
            {"sequence": 2, "boundary": "refused", "causal-predecessors": [1],
             "actor": "released-reader", "operation-id": "work-item-abandon",
             "raw-facts": {"protected": protected, "exit-code": 1, "stderr": diagnostic, "stdout": ""}}]}
        write(scenario, value, files)


class ReleaseEvidenceTests(unittest.TestCase):
    def test_checker_retains_independent_collections(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            binary = root / "target/debug/fluent"
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b"candidate")
            output = root / "release-transition"
            output.mkdir()
            source = Path(__file__).parent
            run = check.subprocess.run
            collections = []

            def fixture(command, **kwargs):
                if command[0] == "cargo":
                    directory = Path(kwargs["env"]["FLUENT_RELEASE_TRACE_DIR"])
                    if directory not in collections:
                        collections.append(directory)
                    write_collection(directory, reverse=len(collections) == 2)
                    return CompletedProcess(command, 0, "fixture passed\n", "")
                if command[0] == "git":
                    return CompletedProcess(command, 0, "candidate\n", "")
                return run(command, **kwargs)

            # Replace host execution and TLC only; exercise real collection,
            # positive/negative projection, and retained evidence accounting.
            with patch.object(check.subprocess, "run", side_effect=fixture), \
                    patch.object(check, "run_case", return_value={"case": "model", "pass": True}), \
                    patch.object(check, "version_trace_case", return_value={"case": "version", "pass": True}), \
                    patch.object(check, "release_ownership_trace_case", return_value={"case": "ownership", "pass": True}):
                retained = None
                for _ in range(2):
                    self.assertEqual(check.release_transition_suite(source, root, output, None, None), 0)
                    result = json.loads((output / "result.json").read_text())
                    verify_release_evidence(output, result)
                    if retained is not None:
                        for path, raw in retained.items():
                            self.assertEqual(path.read_bytes(), raw)
                    collection = collections[-1]
                    retained = {path: path.read_bytes() for path in collection.rglob("*") if path.is_file()}
                self.assertEqual(len(collections), 2)

    def test_project_control_rejects_both_worker_orders(self):
        trace, artifacts = service_fixture()
        for reverse in (False, True):
            with self.subTest(reverse=reverse):
                valid = copy.deepcopy(trace)
                if reverse:
                    valid["events"][1]["raw-facts"]["owned-workers"].reverse()
                self.assertEqual(project_release.project_service(valid, artifacts)["service-transfer"],
                                 "settled")
                invalid = copy.deepcopy(valid)
                check.corrupt_service_project(invalid)
                self.assertNotEqual(invalid, valid)
                with self.assertRaisesRegex(ValueError, "legacy settlement requires both registered workers"):
                    project_release.project_service(invalid, artifacts)

    def test_report_rejects_changed_retained_release_proof(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            binary = root / "fluent"
            binary.write_bytes(b"candidate")
            directory = root / "release-transition"
            directory.mkdir()
            raw = directory / "collection/receipt.json"
            raw.parent.mkdir()
            raw.write_bytes(b"raw receipt")
            log = directory / "projection.log"
            log.write_text("projection passed\n")
            traces = []
            for scenario in ("current-authority", "pending-publication", "service-ownership"):
                path = raw.parent / f"{scenario}.json"
                path.write_text(json.dumps({"artifacts": [
                    {"path": raw.name, "sha256": report.digest(raw)}]}))
                traces.append(path)
            value = {"schema-version": 1, "suite": "release-transition",
                     "candidate-commit": "candidate", "fluent-binary-sha256": report.digest(binary),
                     "pass": True, "cases": [{"case": name, "pass": True, "log": log.name}
                         for name in report.REQUIRED_CASES["release-transition"]],
                     "retained-artifacts": [{"path": str(path.relative_to(directory)),
                                             "sha256": report.digest(path)}
                                            for path in [raw, log, *traces]]}
            result_path = directory / "result.json"
            result_path.write_text(json.dumps(value))

            def accepted():
                return report.report(root, "candidate", binary, Path(__file__).parent)["suites"][
                    "release-transition"]["pass"]

            self.assertTrue(accepted())
            for path in (raw, log, *traces):
                with self.subTest(changed=path.name):
                    original = path.read_bytes()
                    path.write_bytes(original + b"changed")
                    self.assertFalse(accepted(), f"accepted changed {path.name}")
                    path.unlink()
                    self.assertFalse(accepted(), f"accepted missing {path.name}")
                    path.write_bytes(original)
            for omitted in ("retained-artifacts",):
                incomplete = copy.deepcopy(value)
                del incomplete[omitted]
                result_path.write_text(json.dumps(incomplete))
                self.assertFalse(accepted())
            for path in (raw, log, *traces):
                incomplete = copy.deepcopy(value)
                incomplete["retained-artifacts"] = [entry for entry in incomplete["retained-artifacts"]
                    if entry["path"] != str(path.relative_to(directory))]
                result_path.write_text(json.dumps(incomplete))
                self.assertFalse(accepted(), f"accepted unbound {path.name}")


if __name__ == "__main__":
    unittest.main()
