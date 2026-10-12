#!/usr/bin/env python3
"""Check observed release refusal against protected file facts."""

import hashlib
import json
import re
import sys
from pathlib import Path

RELEASE_SHA = "8bb76717be942e4a0c97c297c318c0a1331e92105180edb386354dddbae12974"


def require(condition, property_name):
    if not condition:
        raise ValueError(property_name)


def project(path: Path, candidate: Path, commit: str):
    trace = json.loads(path.read_text())
    assert set(trace) == {"schema-version", "scenario", "fluent-commit",
                          "fluent-binary-sha256", "model-profile",
                          "collection-complete", "events", "artifacts"}
    assert trace["schema-version"] == 1
    assert trace["scenario"] in ("current-authority", "pending-publication",
                                 "released-shared-service-transition")
    assert trace["model-profile"] == "release-transition"
    assert trace["collection-complete"] is True
    assert trace["fluent-commit"] == commit
    assert trace["fluent-binary-sha256"] == hashlib.sha256(candidate.read_bytes()).hexdigest()
    artifacts = {}
    for entry in trace["artifacts"]:
        assert set(entry) == {"path", "sha256"}
        name = entry["path"]
        assert isinstance(name, str) and name not in artifacts
        assert not Path(name).is_absolute() and ".." not in Path(name).parts
        data = (path.parent / name).read_bytes()
        require(hashlib.sha256(data).hexdigest() == entry["sha256"],
                "artifact digest mismatch")
        artifacts[name] = data
    assert artifacts
    if trace["scenario"] == "released-shared-service-transition":
        return project_service(trace, artifacts)
    before, refused = trace["events"]
    assert before["sequence"] == 1 and refused["sequence"] == 2
    assert before["boundary"] == "before-refusal" and refused["boundary"] == "refused"
    assert before["causal-predecessors"] == []
    assert refused["causal-predecessors"] == [1]
    assert before["operation-id"] == "snapshot"
    require(before["actor"] == "current-reader" and refused["actor"] == "released-reader",
            "refusal actor mismatch")
    require(refused["operation-id"] == "work-item-abandon", "refusal operation mismatch")
    assert before["raw-facts"]["predecessor-sha256"] == RELEASE_SHA
    facts = refused["raw-facts"]
    assert type(facts["exit-code"]) is int and facts["exit-code"] != 0
    assert isinstance(facts["stderr"], str) and facts["stderr"]
    assert isinstance(facts["stdout"], str)
    item_id = ("released-refusal" if trace["scenario"] == "current-authority"
               else "released-pending-refusal")
    if trace["scenario"] == "current-authority":
        item = json.loads(artifacts[next(entry["artifact"] for entry in
                                         before["raw-facts"]["protected"] if
                                         entry["path"] == f"items/{item_id}.json")])
        require(item["model_writer_version"]["format"] == "fluent-work-item-writer"
                and item["model_writer_version"]["schema_version"] == 11,
                "current writer envelope mismatch")
        require("unsupported Work Item writer envelope format/schema" in facts["stderr"]
                and f"items/{item_id}.json" in facts["stderr"],
                "compatibility diagnostic mismatch")
    else:
        transaction = json.loads(artifacts[next(entry["artifact"] for entry in
                                                before["raw-facts"]["protected"] if
                                                entry["path"] == f"transactions/{item_id}.json")])
        require(transaction["schema_version"] == 13 and transaction["id"] == item_id,
                "pending transaction format mismatch")
        require("invalid work model" in facts["stderr"]
                and f"transactions/{item_id}.json" in facts["stderr"]
                and f'work item transaction id "{item_id}" cannot be used as a file name'
                in facts["stderr"], "compatibility diagnostic mismatch")
    protected = before["raw-facts"]["protected"]
    assert isinstance(protected, list) and protected
    require(len(facts["protected"]) == len(protected), "protected snapshot changed")
    require([(entry["path"], entry["kind"], entry["sha256"])
            for entry in facts["protected"]] == [
            (entry["path"], entry["kind"], entry["sha256"])
            for entry in protected], "protected snapshot changed")
    names = set()
    for entry in protected:
        assert set(entry) == {"path", "kind", "sha256", "artifact"}
        name = entry["path"]
        assert isinstance(name, str) and name and name not in names
        assert not Path(name).is_absolute() and ".." not in Path(name).parts
        names.add(name)
        assert entry["kind"] in ("file", "directory", "symlink")
        assert re.fullmatch("[0-9a-f]{64}", entry["sha256"])
        assert hashlib.sha256(artifacts[entry["artifact"]]).hexdigest() == entry["sha256"]
    for entry in facts["protected"]:
        assert set(entry) == {"path", "kind", "sha256", "artifact"}
        assert hashlib.sha256(artifacts[entry["artifact"]]).hexdigest() == entry["sha256"]
    assert {entry["artifact"] for entry in protected + facts["protected"]} == set(artifacts)
    assert any(name.startswith("items/") for name in names)
    if trace["scenario"] == "pending-publication":
        assert any(name.startswith("transactions/") for name in names)
    return {"scenario": trace["scenario"], "protected-entries": len(protected),
            "mapping": {"refused": facts["exit-code"] != 0,
                        "pending-publication": trace["scenario"] == "pending-publication",
                        "before-digest": hashlib.sha256(json.dumps([(e["path"],e["kind"],e["sha256"]) for e in protected]).encode()).hexdigest(),
                        "after-digest": hashlib.sha256(json.dumps([(e["path"],e["kind"],e["sha256"]) for e in facts["protected"]]).encode()).hexdigest()}}


def project_service(trace, artifacts):
    expected = {f"service-ownership/{work}-{stage}-receipt.json"
                for work in ("first", "second") for stage in ("initial", "final")}
    expected |= {"service-ownership/released-service.stdout",
                 "service-ownership/released-service.stderr"}
    assert set(artifacts) == expected
    old, stopped, new = trace["events"]
    for index, (event, boundary, operation) in enumerate(zip(
            (old, stopped, new),
            ("old-owner", "old-owner-settled", "new-owner"),
            ("scheduler-run", "scheduler-stop", "scheduler-run")), 1):
        assert event["sequence"] == index
        assert event["boundary"] == boundary and event["operation-id"] == operation
        assert event["causal-predecessors"] == ([] if index == 1 else [index - 1])
    first = old["raw-facts"]
    settled = stopped["raw-facts"]
    replacement = new["raw-facts"]
    assert first["predecessor-sha256"] == RELEASE_SHA
    assert first["registered-projects"] == replacement["registered-projects"] == 2
    old_health, new_health = first["health"], replacement["health"]
    assert old_health["instance_id"] is None and old_health["state"] is None
    require(isinstance(new_health["instance_id"], str) and new_health["instance_id"],
            "service identity mismatch")
    assert old_health["build"] != new_health["build"]
    assert old_health["generation"] == new_health["generation"]
    incompatible = first["incompatible-start"]
    require(type(incompatible["exit"]) is int and incompatible["exit"] != 0 and
            "lacks verifiable drain control" in incompatible["stderr"] and
            incompatible["health-after"] == old_health, "incompatible start replaced legacy ownership")
    assert settled["stop-exit-codes"] == [0, 0]
    assert len(settled["stop-stdout"]) == 2
    assert all(isinstance(output, str) for output in settled["stop-stdout"])
    assert settled["owned-worker-started"] is True
    owned = settled["owned-workers"]
    require(len(owned) == 2 and {w["work_item_id"] for w in owned} == {"first", "second"},
            "legacy settlement requires both registered workers")
    require(all(any(work["work_item_id"] == worker["work_item_id"]
                    and work["dispatch_id"] == worker["dispatch_id"]
                    for work in settled["active-work"]) for worker in owned),
            "owned dispatch mismatch")
    for worker in owned:
        work = worker["work_item_id"]
        assert work in {"first", "second"}
        require(worker["initial-receipt"]["state"] == "running",
                "worker receipt mismatch")
        require(all(launch["process-group-absent"] is True
                    for launch in worker["launches"]), "worker process group remains")
        for stage in ("initial", "final"):
            name = f"service-ownership/{work}-{stage}-receipt.json"
            assert worker[f"{stage}-receipt-artifact"] == name
            assert json.loads(artifacts[name]) == worker[f"{stage}-receipt"]
        for key in ("project", "workspace", "receipt"):
            value = Path(worker[key])
            assert not value.is_absolute() and ".." not in value.parts
    assert all(worker["work_item_id"] in {"first", "second"}
               and worker["attempt_id"] == "attempt-1"
               and worker["dispatch_id"]
               and isinstance(worker["project"], str) and worker["project"]
               and type(worker["pid"]) is int and worker["pid"] > 0
               and worker["task_id"] and worker["workspace"] and worker["receipt"]
               and worker["execution-state"] in {"settled", "canceled"}
               and worker["initial-receipt"]["state"] == "running"
               and worker["initial-receipt"]["process_group_id"] == worker["pid"]
               and worker["final-receipt"]["state"] == worker["execution-state"]
               and worker["initial-receipt"]["attempt_owner"] == worker["final-receipt"]["attempt_owner"]
               and worker["task-status"] in {"complete", "failed", "needs-user", "superseded"}
               and len(worker["launches"]) > 0
               and all(launch["process-group-absent"] is True for launch in worker["launches"])
               and len({launch["pid"] for launch in worker["launches"]}) == len(worker["launches"])
               and worker["pid"] in {launch["pid"] for launch in worker["launches"]}
               and worker["final-receipt"]["process_group_id"] in {launch["pid"] for launch in worker["launches"]}
               and worker["process-group-absent"] is True
               and worker["status-before-stop"] in {"claimed", "running"}
               and worker["status-after-stop"] in {"candidate-ready", "failed", "needs-user", "canceled", "blocked"}
               for worker in owned)
    assert len({(worker["project"], worker["dispatch_id"]) for worker in owned}) == len(owned)
    active = settled["active-work"]
    terminal = {"candidate-ready", "failed", "needs-user", "canceled", "blocked"}
    assert len(active) == 2
    assert all(work["status"] in terminal | {"queued"} for work in active)
    assert any(work["status"] in terminal for work in active)
    assert all(any(work["work_item_id"] == worker["work_item_id"]
                   and work["dispatch_id"] == worker["dispatch_id"]
                   and work["bound_attempt_id"] == worker["attempt_id"]
                   and work["status"] == worker["status-after-stop"]
                   for work in active) for worker in owned)
    stale = replacement["stale-stop"]
    assert len(first["coordinators"]) == 2
    require(stale["requested-coordinator"] == first["coordinators"][0],
            "stale stop authority mismatch")
    assert stale["request-path"].endswith("/.fluent/work/scheduler/stop-request.json")
    assert stale["request-processed"] is True
    assert stale["coordinator-after"]["instance_id"] != stale["requested-coordinator"]
    assert stale["coordinator-after"]["state"] == "running"
    assert stale["coordinator-after"]["build"] == new_health["build"]
    assert stale["admitted-work"]["work_item_id"] == "first-after-stale"
    assert stale["admitted-work"]["status"] in {"queued", "claimed", "running"}
    assert stale["health-after"] == new_health
    assert len(settled["queues"]) == len(replacement["queues"]) == 2
    assert all(queue["status"] == "queued" for queue in settled["queues"])
    assert all(queue["work_item_id"] == prior["work_item_id"]
               for queue, prior in zip(replacement["queues"], settled["queues"]))
    return {"scenario": trace["scenario"], "service-transfer": "settled",
            "mapping": {"legacy-children": len(owned), "legacy-owned-before": True,
                        "children-held-before": [w["initial-receipt"]["state"] == "running" for w in owned],
                        "children-held-after": [not w["process-group-absent"] for w in owned],
                        "children-completed": [w["task-status"] == "complete" for w in owned],
                        "current-owner": new_health["instance_id"], "stale-stop-refused": stale["health-after"] == new_health}}


if __name__ == "__main__":
    try:
        print(json.dumps(project(Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3])))
    except (AssertionError, OSError, ValueError, IndexError, KeyError) as error:
        print(f"release projection failed: {error}", file=sys.stderr)
        sys.exit(1)
