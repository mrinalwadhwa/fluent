use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;
#[path = "lib/release_trace.rs"]
mod release_trace;

#[test]
fn workflow_assurance_report_isolates_release_collections() {
    let tmp = TempDir::new().unwrap();
    let formal = tmp.path().join("formal");
    let root = release_trace::output_directory(Some(formal.clone()));
    assert_eq!(root, formal);
    fs::write(root.join("receipt.json"), "checked proof").unwrap();
    let threads: Vec<_> = (0..4)
        .map(|_| {
            std::thread::spawn(|| {
                let path = release_trace::output_directory(None);
                fs::write(path.join("receipt.json"), "later test").unwrap();
                path
            })
        })
        .collect();
    let later: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(
        fs::read(root.join("receipt.json")).unwrap(),
        b"checked proof"
    );
    let unique: std::collections::HashSet<_> = later.iter().collect();
    assert_eq!(unique.len(), later.len());
    for path in later {
        assert_ne!(path, formal);
        fs::remove_dir_all(path).unwrap();
    }
}

fn run(dir: &Path, program: &str, args: &[&str]) -> Output {
    Command::new(program)
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

fn fluent(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fluent"))
        .current_dir(dir)
        .env("FLUENT_NO_UPDATE_CHECK", "1")
        .env("FLUENT_TEST_HOST_SANDBOX_PREFLIGHT", "pass")
        .args(args)
        .output()
        .unwrap()
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn event(seq: u64, boundary: &str, operation: &str, predecessors: &[u64], facts: Value) -> Value {
    json!({"sequence":seq,"actor":"operator","operation-id":operation,
        "generation":null,"boundary":boundary,"causal-predecessors":predecessors,
        "raw-facts":facts})
}

fn capture() -> (TempDir, Value) {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    fs::create_dir(&project).unwrap();
    assert!(
        run(&project, "git", &["init", "-b", "main"])
            .status
            .success()
    );
    assert!(
        run(
            &project,
            "git",
            &["config", "user.email", "test@example.com"]
        )
        .status
        .success()
    );
    assert!(
        run(&project, "git", &["config", "user.name", "Test"])
            .status
            .success()
    );
    fs::write(project.join("README.md"), "fixture\n").unwrap();
    assert!(run(&project, "git", &["add", "README.md"]).status.success());
    assert!(
        run(&project, "git", &["commit", "-m", "Initialize fixture"])
            .status
            .success()
    );
    let work = format!(
        "foundation{}",
        tmp.path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .replace(['.', '-'], "")
    );
    let create = fluent(
        &project,
        &["work-item", "create", &work, "--title", "Trace fixture"],
    );
    assert!(
        create.status.success(),
        "{}",
        String::from_utf8_lossy(&create.stderr)
    );
    let show1 = fluent(&project, &["work-item", "show", &work]);
    assert!(show1.status.success());
    let intake: Value = serde_json::from_slice(&show1.stdout).unwrap();
    let attempt = fluent(&project, &["attempt", "create", &work, "attempt-1"]);
    assert!(
        attempt.status.success(),
        "{}",
        String::from_utf8_lossy(&attempt.stderr)
    );
    let show2 = fluent(&project, &["work-item", "show", &work]);
    assert!(show2.status.success());
    let planned: Value = serde_json::from_slice(&show2.stdout).unwrap();
    let cancel = fluent(&project, &["attempt", "cancel", &work, "attempt-1"]);
    assert!(
        cancel.status.success(),
        "{}",
        String::from_utf8_lossy(&cancel.stderr)
    );
    let show3 = fluent(&project, &["work-item", "show", &work]);
    assert!(show3.status.success());
    let canceled: Value = serde_json::from_slice(&show3.stdout).unwrap();
    let run_attempt = fluent(
        &project,
        &["task", "run", &work, "attempt-1", "attempt-1-write-1"],
    );
    let show4 = fluent(&project, &["work-item", "show", &work]);
    assert!(show4.status.success());
    let after_refusal: Value = serde_json::from_slice(&show4.stdout).unwrap();
    let artifacts = [
        (&show1, "intake.json"),
        (&show2, "planned.json"),
        (&show3, "canceled.json"),
        (&show4, "refused.json"),
    ]
    .into_iter()
    .map(|(out, path)| {
        fs::write(tmp.path().join(path), &out.stdout).unwrap();
        json!({"path":path,"sha256":digest(&out.stdout)})
    })
    .collect::<Vec<_>>();
    let binary = fs::read(env!("CARGO_BIN_EXE_fluent")).unwrap();
    let trace = json!({"schema-version":1,"scenario":"foundation-public-cancel",
    "fluent-commit":String::from_utf8_lossy(&run(Path::new(env!("CARGO_MANIFEST_DIR")),"git", &["rev-parse","HEAD"]).stdout).trim(),
    "fluent-binary-sha256":digest(&binary),"model-profile":"foundation",
    "collection-complete":true,"artifacts":artifacts,"events":[
        event(1,"intake","create-work",&[],json!({"exit-code":create.status.code(),"work":intake})),
        event(2,"attempt-create","create-attempt",&[1],json!({"exit-code":attempt.status.code(),"work":planned})),
        event(3,"cancel","cancel-attempt",&[2],json!({"exit-code":cancel.status.code(),"work":canceled})),
        event(4,"run-refused","run-attempt",&[3],json!({"exit-code":run_attempt.status.code(),"stdout":String::from_utf8_lossy(&run_attempt.stdout),"stderr":String::from_utf8_lossy(&run_attempt.stderr),"work":after_refusal}))
    ]});
    (tmp, trace)
}

fn project(trace: &Value, dir: &Path) -> Output {
    let path = dir.join("trace.json");
    fs::write(&path, serde_json::to_vec_pretty(trace).unwrap()).unwrap();
    Command::new("python3")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/formal/workflow/project.py"))
        .arg(path)
        .output()
        .unwrap()
}

#[test]
fn workflow_foundation_public_trace() {
    let (tmp, trace) = capture();
    let result = project(&trace, tmp.path());
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/formal/workflow/foundation");
    fs::create_dir_all(&target).unwrap();
    fs::write(
        target.join("production-trace.json"),
        serde_json::to_vec_pretty(&trace).unwrap(),
    )
    .unwrap();
    for entry in trace["artifacts"].as_array().unwrap() {
        let name = entry["path"].as_str().unwrap();
        fs::copy(tmp.path().join(name), target.join(name)).unwrap();
    }
}

#[test]
fn workflow_foundation_projection_rejects_bad_observations() {
    let (tmp, trace) = capture();
    let original_canceled = fs::read(tmp.path().join("canceled.json")).unwrap();
    let reject = |bad: &Value, expected: &str| {
        let result = project(bad, tmp.path());
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(expected),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    let mut incomplete = trace.clone();
    incomplete["collection-complete"] = json!(false);
    reject(&incomplete, "collection incomplete");
    let mut malformed = trace.clone();
    malformed["events"][1]["boundary"] = json!("unknown");
    reject(&malformed, "event identity or boundary differs");
    let mut ordering = trace.clone();
    ordering["events"][3]["causal-predecessors"] = json!([]);
    reject(&ordering, "missing or ambiguous causal predecessor");
    let mut unrelated_failure = trace.clone();
    unrelated_failure["events"][3]["raw-facts"]["stderr"] =
        json!("Error: task setup failed for another reason\n");
    reject(&unrelated_failure, "cancellation refusal not observed");
    let mut identity = trace.clone();
    identity["events"][2]["raw-facts"]["work"]["id"] = json!("other");
    let canceled_bytes = serde_json::to_vec(&identity["events"][2]["raw-facts"]["work"]).unwrap();
    fs::write(tmp.path().join("canceled.json"), &canceled_bytes).unwrap();
    identity["artifacts"][2]["sha256"] = json!(digest(&canceled_bytes));
    reject(&identity, "work identity changed");
    fs::write(tmp.path().join("canceled.json"), &original_canceled).unwrap();
    let mut invented = trace.clone();
    invented["events"][3]["raw-facts"]["work"]["attempts"][0]["status"] = json!("complete");
    let refused_bytes = serde_json::to_vec(&invented["events"][3]["raw-facts"]["work"]).unwrap();
    fs::write(tmp.path().join("refused.json"), &refused_bytes).unwrap();
    invented["artifacts"][3]["sha256"] = json!(digest(&refused_bytes));
    reject(&invented, "run advanced canceled state");
}

#[test]
fn workflow_release_build_public_workflow() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let built = run(
        root,
        "cargo",
        &["build", "--offline", "--release", "--no-default-features"],
    );
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let binary = root.join("target/release/fluent");
    let version = run(root, binary.to_str().unwrap(), &["version"]);
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).contains("fluent 0.6.0"));
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    assert!(
        run(project, "git", &["init", "-b", "main"])
            .status
            .success()
    );
    assert!(
        run(
            project,
            "git",
            &["config", "user.email", "test@example.com"]
        )
        .status
        .success()
    );
    assert!(
        run(project, "git", &["config", "user.name", "Test"])
            .status
            .success()
    );
    fs::write(project.join("README.md"), "fixture\n").unwrap();
    assert!(run(project, "git", &["add", "README.md"]).status.success());
    assert!(
        run(project, "git", &["commit", "-m", "Initialize fixture"])
            .status
            .success()
    );
    for args in [
        vec!["init"],
        vec![
            "work-item",
            "create",
            "ordinary-release",
            "--title",
            "Ordinary release",
        ],
        vec!["attempt", "create", "ordinary-release", "attempt-1"],
        vec!["attempt", "cancel", "ordinary-release", "attempt-1"],
    ] {
        let result = run(project, binary.to_str().unwrap(), &args);
        assert!(
            result.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let shown = run(
        project,
        binary.to_str().unwrap(),
        &["work-item", "show", "ordinary-release"],
    );
    assert!(
        shown.status.success(),
        "{}",
        String::from_utf8_lossy(&shown.stderr)
    );
    let work: Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(work["attempts"][0]["status"], "needs-user");
    assert!(work["attempts"][0]["cancellation"]["requested_at"].is_string());
    let refused = run(
        project,
        binary.to_str().unwrap(),
        &[
            "task",
            "run",
            "ordinary-release",
            "attempt-1",
            "attempt-1-write-1",
        ],
    );
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("cancel"));
    let after = run(
        project,
        binary.to_str().unwrap(),
        &["work-item", "show", "ordinary-release"],
    );
    assert!(after.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&after.stdout).unwrap(),
        work
    );
    let queued = run(
        project,
        binary.to_str().unwrap(),
        &[
            "work-item",
            "create",
            "queued-release",
            "--title",
            "Queued release",
        ],
    );
    assert!(
        queued.status.success(),
        "{}",
        String::from_utf8_lossy(&queued.stderr)
    );
    let add = run(
        project,
        binary.to_str().unwrap(),
        &["queue", "add", "queued-release"],
    );
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    let list = run(project, binary.to_str().unwrap(), &["queue", "list"]);
    assert!(list.status.success());
    assert!(String::from_utf8_lossy(&list.stdout).contains("queued-release"));
}

#[test]
fn workflow_assurance_report_rejects_missing_and_misbound_evidence() {
    let tmp = TempDir::new().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR"));
    let report = source.join("tests/formal/workflow/report.py");
    let output = tmp.path().join("report.json");
    let args = [
        "--results",
        tmp.path().to_str().unwrap(),
        "--candidate",
        "candidate-a",
        "--binary",
        env!("CARGO_BIN_EXE_fluent"),
        "--output",
        output.to_str().unwrap(),
    ];
    let command = std::iter::once(report.to_str().unwrap())
        .chain(args)
        .collect::<Vec<_>>();
    let missing = run(source, "python3", &command);
    assert!(!missing.status.success());
    let result: Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
    assert_eq!(result["pass"], false);
    let behaviors = result["approved-behaviors"].as_array().unwrap();
    assert_eq!(behaviors.len(), 15);
    assert_eq!(
        behaviors
            .iter()
            .map(|behavior| behavior["tests"].as_array().unwrap().len())
            .sum::<usize>(),
        22
    );
    assert!(behaviors.iter().all(|behavior| {
        behavior["tests"]
            .as_array()
            .unwrap()
            .iter()
            .all(|test| test["state"] == "pending")
    }));
    assert!(result["approved-contracts-sha256"].is_string());
    assert!(result["suite-source-sha256"]["integrated-projector"].is_string());
    assert!(result["suite-source-sha256"]["writer-plan-projector"].is_string());
    assert!(result["model-sha256"]["WriterPlanTrace.tla"].is_string());
    assert!(result["connection-inventory-sha256"].is_string());
    assert_eq!(result["scheduler-suites"]["recovery"]["pass"], false);
    assert!(result["missing"].as_array().unwrap().len() > 0);
    assert!(
        result["missing"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str().unwrap().starts_with("connection: "))
    );
    assert!(result["missing"].as_array().unwrap().iter().any(|item| {
        item.as_str()
            .unwrap()
            .starts_with("integrated-production: ")
    }));
    assert!(
        result["missing"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| { item.as_str().unwrap().starts_with("scheduler recovery: ") })
    );
    let suite = tmp.path().join("foundation");
    fs::create_dir(&suite).unwrap();
    fs::write(suite.join("case.log"), "Model checking completed\n").unwrap();
    fs::write(
        suite.join("result.json"),
        json!({"schema-version":1,"suite":"foundation",
        "candidate-commit":"candidate-b",
        "fluent-binary-sha256":digest(&fs::read(env!("CARGO_BIN_EXE_fluent")).unwrap()),
        "pass":true,
        "cases":[{"case":"foundation.cfg","pass":true,"log":"case.log"}]})
        .to_string(),
    )
    .unwrap();
    let misbound = run(source, "python3", &command);
    assert!(!misbound.status.success());
    let result: Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
    assert_eq!(result["suites"]["foundation"]["pass"], false);
    let partial = json!({"schema-version":1,"suite":"foundation",
        "candidate-commit":"candidate-a",
        "fluent-binary-sha256":digest(&fs::read(env!("CARGO_BIN_EXE_fluent")).unwrap()),
        "pass":true,
        "cases":[{"case":"foundation.cfg","pass":true,"log":"case.log"}]});
    fs::write(suite.join("result.json"), partial.to_string()).unwrap();
    let incomplete = run(source, "python3", &command);
    assert!(!incomplete.status.success());
    let result: Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
    assert_eq!(result["suites"]["foundation"]["pass"], false);
}

#[test]
fn workflow_assurance_report_rejects_unchecked_connection_claims() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = r#"
import sys
from pathlib import Path
sys.path.insert(0, str(Path(sys.argv[1])))
import report
cases = {'foundation': {'foundation.cfg', 'production-projection',
                        'negative-model-path', 'negative-deliverinputs.cfg'},
         'scheduler-recovery': {'queue-proposed.cfg'}}
assert not report.connection_credit('Verified: manually reviewed', cases)
assert report.connection_credit(
    'Check: model=foundation/foundation.cfg; assumption=foundation/foundation.cfg; '
    'production=foundation/production-projection; control=foundation/negative-model-path', cases)
assert not report.connection_credit(
    'Check: model=foundation/foundation.cfg; assumption=foundation/foundation.cfg; '
    'production=release-transition/service-projection; control=foundation/negative-model-path', cases)

assert not report.connection_credit(
    'Verified: model=foundation/foundation.cfg; assumption=foundation/foundation.cfg; '
    'production=foundation/production-projection; control=foundation/missing-control', cases)
assert report.connection_credit(
    'Verified: model=foundation/foundation.cfg; assumption=foundation/foundation.cfg; '
    'production=foundation/production-projection; control=foundation/negative-model-path', cases)
assert report.connection_credit(
    'Verified: model=scheduler-recovery/queue-proposed.cfg; '
    'assumption=foundation/foundation.cfg; production=foundation/production-projection; '
    'control=foundation/negative-model-path', cases)
"#;
    let source = root.join("tests/formal/workflow");
    let result = run(root, "python3", &["-c", script, source.to_str().unwrap()]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn workflow_assurance_report_binds_scheduler_results() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = r#"
import hashlib
import json
import runpy
import sys
import tempfile
from pathlib import Path
source = Path(sys.argv[1])
sys.path.insert(0, str(source))
import report
with tempfile.TemporaryDirectory() as temporary:
    target = Path(temporary)
    (target / 'workflow').mkdir()
    output = target / 'scheduler'
    output.mkdir()
    jar = target / 'tlc.jar'
    jar.write_bytes(b'pinned-tool-fixture')
    definitions = runpy.run_path(str(source.parent / 'scheduler/check.py'))
    for suite, constant in {'recovery':'RECOVERY_CASES', 'lifecycle':'LIFECYCLE_CASES',
                            'launch':'LAUNCH_CASES', 'landing':'LANDING_CASES'}.items():
        cases = []
        for module, config, violation in definitions[constant]:
            log = output / (config + '.log')
            log.write_text('checked\n')
            cases.append({'case':config,'module':module,'expected':violation,
                          'pass':True,'log':log.name,'log-sha256':report.digest(log)})
        (output / (suite + '-result.json')).write_text(json.dumps({
            'schema-version':1,'suite':suite,'candidate-commit':'candidate-a',
            'tlc-jar-sha256':report.digest(jar),'pass':True,'cases':cases}))
    suites, missing = report.scheduler_results(target / 'workflow', 'candidate-a',
                                               jar, source.parent / 'scheduler')
    assert not missing and all(value['pass'] for value in suites.values())
    (output / 'queue-proposed.cfg.log').write_text('changed\n')
    suites, missing = report.scheduler_results(target / 'workflow', 'candidate-a',
                                               jar, source.parent / 'scheduler')
    assert not suites['recovery']['pass'] and missing == [
        'scheduler recovery: malformed result or log']
"#;
    let source = root.join("tests/formal/workflow");
    let result = run(root, "python3", &["-c", script, source.to_str().unwrap()]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn workflow_assurance_report_binds_approved_named_tests() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = r#"
import json
import sys
import tempfile
from pathlib import Path
source = Path(sys.argv[1])
sys.path.insert(0, str(source))
import report
binary = Path(sys.argv[2])
with tempfile.TemporaryDirectory() as temporary:
    results = Path(temporary)
    output = results / 'integrated'
    output.mkdir()
    rows, missing = report.approved_contracts(source, results, 'candidate-a', binary)
    assert len(rows) == 15 and sum(len(row['tests']) for row in rows) == 22
    assert missing and all(test['state'] == 'pending'
                           for row in rows for test in row['tests'])
    cases = []
    for row in rows:
        for test in row['tests']:
            log = test['name'] + '.log'
            (output / log).write_text('PASS ' + test['name'] + '\n')
            cases.append({'binary':test['binary'], 'name':test['name'],
                          'pass':True, 'log':log,
                          'log-sha256':report.digest(output / log)})
    receipt = {'schema-version':1, 'candidate-commit':'candidate-a',
               'fluent-binary-sha256':report.digest(binary), 'cases':cases}
    path = output / 'named-tests-result.json'
    path.write_text(json.dumps(receipt))
    rows, missing = report.approved_contracts(source, results, 'candidate-a', binary)
    assert not missing and all(test['state'] == 'pass'
                               for row in rows for test in row['tests'])
    receipt['cases'][0]['pass'] = False
    path.write_text(json.dumps(receipt))
    rows, missing = report.approved_contracts(source, results, 'candidate-a', binary)
    assert len(missing) == 1 and rows[0]['tests'][0]['state'] == 'fail'
    assert all(test['state'] == 'pass' for row in rows
               for test in row['tests'] if test['name'] != cases[0]['name'])
    receipt['cases'][0]['pass'] = True
    path.write_text(json.dumps(receipt))
    (output / cases[0]['log']).write_text('changed\n')
    rows, missing = report.approved_contracts(source, results, 'candidate-a', binary)
    assert missing and rows[0]['tests'][0]['state'] == 'pending'
"#;
    let result = run(
        root,
        "python3",
        &[
            "-c",
            script,
            root.join("tests/formal/workflow").to_str().unwrap(),
            env!("CARGO_BIN_EXE_fluent"),
        ],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn workflow_integrated_rejects_missing_model_tool() {
    let tmp = TempDir::new().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR"));
    let checker = source.join("tests/formal/workflow/check.py");
    let missing_jar = tmp.path().join("missing.jar");
    let result = run(
        source,
        "python3",
        &[
            checker.to_str().unwrap(),
            "--suite=integrated",
            &format!("--jar={}", missing_jar.display()),
        ],
    );
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("pinned TLC 1.7.4 JAR"));
}

#[test]
fn workflow_integrated_review_only_orders_tester_before_reviews() {
    review_only_trace(false);
}

#[test]
fn workflow_integrated_review_only_resumes_after_interruption() {
    review_only_trace(true);
}

fn review_only_trace(restart: bool) {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("main");
    fs::create_dir(&project).unwrap();
    let project = project.as_path();
    assert!(
        run(project, "git", &["init", "-b", "main"])
            .status
            .success()
    );
    assert!(
        run(
            project,
            "git",
            &["config", "user.email", "test@example.com"]
        )
        .status
        .success()
    );
    assert!(
        run(project, "git", &["config", "user.name", "Test"])
            .status
            .success()
    );
    fs::write(project.join("README.md"), "review fixture\n").unwrap();
    fs::create_dir_all(project.join(".fluent")).unwrap();
    fs::write(
        project.join(".fluent/tester.yaml"),
        "commands:\n  - command: 'true'\n    test_harness: shell-harness\n",
    )
    .unwrap();
    let extractor = project.join(".fluent/extract-tester-results");
    fs::write(&extractor, "#!/bin/sh\necho '[]'\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&extractor, fs::Permissions::from_mode(0o755)).unwrap();
    }
    for role in [
        "documentation",
        "behaviors",
        "architecture",
        "skills",
        "tests",
    ] {
        let dir = project.join(format!("skills/review-{role}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), "review fixture\n").unwrap();
    }
    fs::write(project.join(".gitignore"), ".fluent/work/\n").unwrap();
    assert!(run(project, "git", &["add", "."]).status.success());
    assert!(
        run(project, "git", &["commit", "-m", "Add review fixture"])
            .status
            .success()
    );
    let head = run(project, "git", &["rev-parse", "HEAD"]);
    assert!(head.status.success());

    let created = fluent(
        project,
        &["work-item", "create", "review", "--title", "Review fixture"],
    );
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let planned = fluent(project, &["review", "codebase", "review", "attempt-review"]);
    assert!(
        planned.status.success(),
        "{}",
        String::from_utf8_lossy(&planned.stderr)
    );
    let before = fluent(project, &["work-item", "show", "review"]);
    assert!(before.status.success());

    let mock_dir = tmp.path().join("mock-bin");
    fs::create_dir(&mock_dir).unwrap();
    let mock = mock_dir.join("claude");
    fs::write(&mock, r##"#!/bin/bash
if [ "$1 $2 $3" = 'auth status --json' ]; then echo '{"loggedIn":true}'; exit 0; fi
if [ "$1" = --version ]; then echo '2.1.211 (Claude Code)'; exit 0; fi
prompt=''
for arg in "$@"; do
  case "$arg" in *'"candidate-commit"'*) prompt="$arg" ;; esac
done
if [ -z "$prompt" ]; then exit 0; fi
if [ -n "$FLUENT_REVIEW_ONLY_WAIT" ]; then
  touch "$FLUENT_REVIEW_ONLY_WAIT.started"
  while [ -f "$FLUENT_REVIEW_ONLY_WAIT" ]; do sleep 0.1; done
fi
candidate="$(printf '%s\n' "$prompt" | sed -n 's/^- Workspace: //p' | head -1)"
if [ -z "$candidate" ]; then
  candidate="$(printf '%s\n' "$prompt" | sed -n 's/^Review the codebase at \(.*\)\.$/\1/p' | head -1)"
fi
commit="$(printf '%s\n' "$prompt" | sed -n 's/^[[:space:]]*"candidate-commit": "\([^"]*\)",$/\1/p' | head -1)"
role="$(printf '%s\n' "$prompt" | sed -n 's/^[[:space:]]*"role": "\([^"]*\)",$/\1/p' | head -1)"
inspected="$(git -C "$candidate" ls-tree -r --name-only "$commit" | head -1)"
jq -n --arg commit "$commit" --arg role "$role" --arg path "$inspected" \
  '{"schema-version":1,"candidate-commit":$commit,role:$role,inspections:[{path:$path,summary:"Read tracked candidate file."}]}' > review-inspection.json
printf 'Verdict: pass\n\nReview-only result.\n' > review.md
jq -cn --arg absolute "$candidate/$inspected" \
  '{type:"assistant",message:{content:[{type:"tool_use",id:"review-read",name:"Read",input:{file_path:$absolute}}]}}'
jq -cn '{type:"user",message:{content:[{type:"tool_result",tool_use_id:"review-read",content:"review fixture"}]}}'
"##).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&mock, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let wait = tmp.path().join("wait-review");
    let mut command = Command::new(env!("CARGO_BIN_EXE_fluent"));
    command
        .current_dir(project)
        .env("FLUENT_NO_UPDATE_CHECK", "1")
        .env("FLUENT_TEST_HOST_SANDBOX_PREFLIGHT", "pass")
        .env("FLUENT_TEST_HERMETIC_PROVIDERS", "1")
        .env(
            "PATH",
            format!(
                "{}:{}",
                mock_dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .args(["attempt", "run", "review", "attempt-review", "--no-sandbox"]);
    let mut interrupted = None;
    if restart {
        fs::write(&wait, b"wait").unwrap();
        let mut child = command
            .env("FLUENT_REVIEW_ONLY_WAIT", &wait)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let started = tmp.path().join("wait-review.started");
        while !started.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let reached = started.exists();
        child.kill().unwrap();
        child.wait().unwrap();
        let crashed = fluent(project, &["work-item", "show", "review"]);
        let cancel = fluent(project, &["attempt", "cancel", "review", "attempt-review"]);
        assert!(
            cancel.status.success(),
            "{}",
            String::from_utf8_lossy(&cancel.stderr)
        );
        let paused = fluent(project, &["work-item", "show", "review"]);
        assert!(reached && crashed.status.success() && paused.status.success());
        interrupted = Some((crashed.stdout, paused.stdout));
        fs::remove_file(&wait).unwrap();
        command
            .env_remove("FLUENT_REVIEW_ONLY_WAIT")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
    }
    let completed = command.output().unwrap();
    assert!(
        completed.status.success(),
        "{}",
        String::from_utf8_lossy(&completed.stderr)
    );
    let after = fluent(project, &["work-item", "show", "review"]);
    assert!(after.status.success());
    let merge_candidates = fluent(project, &["merge-candidate", "list", "review"]);
    assert!(merge_candidates.status.success());
    let final_head = run(project, "git", &["rev-parse", "HEAD"]);
    assert!(final_head.status.success());

    let source = Path::new(env!("CARGO_MANIFEST_DIR"));
    let target = source
        .join("target/formal/workflow/integrated/production")
        .join(if restart {
            "review-restart"
        } else {
            "review-only"
        });
    fs::create_dir_all(&target).unwrap();
    let observed = [
        ("planned.json", &before.stdout),
        ("complete.json", &after.stdout),
        ("head-before.txt", &head.stdout),
        ("head-after.txt", &final_head.stdout),
        ("merge-candidates.txt", &merge_candidates.stdout),
    ];
    let artifacts = observed
        .iter()
        .map(|(name, bytes)| {
            fs::write(target.join(name), bytes).unwrap();
            json!({"path":name,"sha256":digest(bytes)})
        })
        .collect::<Vec<_>>();
    let trace = json!({"schema-version":1,"scenario":"review-only-order",
    "fluent-commit":String::from_utf8_lossy(&run(source,"git", &["rev-parse","HEAD"]).stdout).trim(),
    "fluent-binary-sha256":digest(&fs::read(env!("CARGO_BIN_EXE_fluent")).unwrap()),
    "model-profile":"review-only","collection-complete":true,"artifacts":artifacts,
    "events":[
        event(1,"review-only-planned","review-codebase",&[],json!({
            "exit-code":planned.status.code(),"work":serde_json::from_slice::<Value>(&before.stdout).unwrap()})),
        event(2,"review-only-complete","attempt-run",&[1],json!({
            "exit-code":completed.status.code(),"work":serde_json::from_slice::<Value>(&after.stdout).unwrap()}))
    ]});
    let path = target.join("production-trace.json");
    fs::write(&path, serde_json::to_vec_pretty(&trace).unwrap()).unwrap();
    let projected = run(
        source,
        "python3",
        &[
            "tests/formal/workflow/project_review_only.py",
            path.to_str().unwrap(),
        ],
    );
    assert!(
        projected.status.success(),
        "{}",
        String::from_utf8_lossy(&projected.stderr)
    );
    if let Some((crashed, paused)) = interrupted {
        let crashed: Value = serde_json::from_slice(&crashed).unwrap();
        let paused: Value = serde_json::from_slice(&paused).unwrap();
        let finished: Value = serde_json::from_slice(&after.stdout).unwrap();
        assert_eq!(
            crashed["attempts"][0]["tasks"][0], finished["attempts"][0]["tasks"][0],
            "review-only restart replaced completed Tester history"
        );
        assert!(paused["attempts"][0]["cancellation"]["requested_at"].is_string());
        assert!(finished["attempts"][0]["cancellation"]["resumed_at"].is_string());
        fs::write(
            target.join("crashed.json"),
            serde_json::to_vec_pretty(&crashed).unwrap(),
        )
        .unwrap();
        fs::write(
            target.join("paused.json"),
            serde_json::to_vec_pretty(&paused).unwrap(),
        )
        .unwrap();
        let facts = [
            json!({"work":crashed}),
            json!({"work":paused}),
            json!({"continued":trace}),
        ];
        let mut artifacts = Vec::new();
        let mut events = Vec::new();
        for (index, fact) in facts.iter().enumerate() {
            let name = format!("review-restart-{index}.json");
            let bytes = serde_json::to_vec_pretty(fact).unwrap();
            fs::write(target.join(&name), &bytes).unwrap();
            artifacts.push(json!({"path":name,"sha256":digest(&bytes)}));
            events.push(event(
                index as u64 + 1,
                ["crashed", "canceled", "continued"][index],
                ["kill-parent", "attempt-cancel", "attempt-run"][index],
                &if index == 0 {
                    vec![]
                } else {
                    vec![index as u64]
                },
                fact.clone(),
            ));
        }
        let restart_trace = json!({"schema-version":1,"scenario":"review-restart",
            "fluent-commit":trace["fluent-commit"],"fluent-binary-sha256":trace["fluent-binary-sha256"],
            "model-profile":"review-restart","collection-complete":true,"events":events,"artifacts":artifacts});
        let restart_path = target.join("restart-trace.json");
        fs::write(
            &restart_path,
            serde_json::to_vec_pretty(&restart_trace).unwrap(),
        )
        .unwrap();
        let projected = run(
            source,
            "python3",
            &[
                "tests/formal/workflow/project_review_restart.py",
                restart_path.to_str().unwrap(),
            ],
        );
        assert!(
            projected.status.success(),
            "{}",
            String::from_utf8_lossy(&projected.stderr)
        );
    }
    let negative = tmp.path().join("review-order-negative");
    fs::create_dir(&negative).unwrap();
    for (name, _) in observed {
        fs::copy(target.join(name), negative.join(name)).unwrap();
    }
    let mut changed = trace;
    changed["events"][1]["raw-facts"]["work"]["attempts"][0]["tasks"][1]["started_at"] =
        json!("2000-01-01T00:00:00+00:00");
    let bytes = serde_json::to_vec(&changed["events"][1]["raw-facts"]["work"]).unwrap();
    fs::write(negative.join("complete.json"), &bytes).unwrap();
    changed["artifacts"][1]["sha256"] = json!(digest(&bytes));
    let negative_path = negative.join("production-trace.json");
    fs::write(&negative_path, serde_json::to_vec_pretty(&changed).unwrap()).unwrap();
    let rejected = run(
        source,
        "python3",
        &[
            "tests/formal/workflow/project_review_only.py",
            negative_path.to_str().unwrap(),
        ],
    );
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("review started before Tester completed")
    );
}

#[test]
fn workflow_integrated_queue_survives_other_attempt_cancellation() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    assert!(
        run(project, "git", &["init", "-b", "main"])
            .status
            .success()
    );
    assert!(
        run(
            project,
            "git",
            &["config", "user.email", "test@example.com"]
        )
        .status
        .success()
    );
    assert!(
        run(project, "git", &["config", "user.name", "Test"])
            .status
            .success()
    );
    fs::write(project.join("README.md"), "fixture\n").unwrap();
    assert!(run(project, "git", &["add", "README.md"]).status.success());
    assert!(
        run(project, "git", &["commit", "-m", "Initialize fixture"])
            .status
            .success()
    );
    assert!(fluent(project, &["init"]).status.success());
    for work in ["foreground", "queued"] {
        let created = fluent(project, &["work-item", "create", work, "--title", work]);
        assert!(
            created.status.success(),
            "{}",
            String::from_utf8_lossy(&created.stderr)
        );
    }
    let attempt = fluent(project, &["attempt", "create", "foreground", "attempt-1"]);
    assert!(
        attempt.status.success(),
        "{}",
        String::from_utf8_lossy(&attempt.stderr)
    );
    let queued = fluent(project, &["queue", "add", "queued"]);
    assert!(
        queued.status.success(),
        "{}",
        String::from_utf8_lossy(&queued.stderr)
    );
    let before_queue = fluent(project, &["work-item", "show", "queued"]);
    assert!(before_queue.status.success());
    let cancel = fluent(project, &["attempt", "cancel", "foreground", "attempt-1"]);
    assert!(
        cancel.status.success(),
        "{}",
        String::from_utf8_lossy(&cancel.stderr)
    );
    let foreground = fluent(project, &["work-item", "show", "foreground"]);
    assert!(foreground.status.success());
    let work: Value = serde_json::from_slice(&foreground.stdout).unwrap();
    assert_eq!(work["attempts"][0]["status"], "needs-user");
    assert!(work["attempts"][0]["cancellation"]["requested_at"].is_string());
    let queue = fluent(project, &["queue", "list"]);
    assert!(queue.status.success());
    assert!(String::from_utf8_lossy(&queue.stdout).contains("queued"));
    let status = fluent(project, &["status"]);
    assert!(status.status.success());
    let after_queue = fluent(project, &["work-item", "show", "queued"]);
    assert!(after_queue.status.success());
    assert_eq!(before_queue.stdout, after_queue.stdout);
    let refused = fluent(
        project,
        &[
            "task",
            "run",
            "foreground",
            "attempt-1",
            "attempt-1-write-1",
        ],
    );
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("cancel"));
    let after_foreground = fluent(project, &["work-item", "show", "foreground"]);
    assert!(after_foreground.status.success());
    assert_eq!(foreground.stdout, after_foreground.stdout);
    let old_head = run(project, "git", &["rev-parse", "HEAD"]);
    assert!(old_head.status.success());
    fs::write(project.join("README.md"), "independent change\n").unwrap();
    assert!(run(project, "git", &["add", "README.md"]).status.success());
    assert!(
        run(project, "git", &["commit", "-m", "Update fixture"])
            .status
            .success()
    );
    let new_head = run(project, "git", &["rev-parse", "HEAD"]);
    assert!(new_head.status.success());
    assert_ne!(old_head.stdout, new_head.stdout);
    let after_git_queue = fluent(project, &["queue", "list"]);
    let after_git_queued = fluent(project, &["work-item", "show", "queued"]);
    let after_git_foreground = fluent(project, &["work-item", "show", "foreground"]);
    assert!(after_git_queue.status.success());
    assert!(after_git_queued.status.success());
    assert!(after_git_foreground.status.success());
    assert_eq!(after_git_queue.stdout, queue.stdout);
    assert_eq!(after_git_queued.stdout, before_queue.stdout);
    assert_eq!(after_git_foreground.stdout, foreground.stdout);

    let source = Path::new(env!("CARGO_MANIFEST_DIR"));
    let trace_dir = tmp.path().join("integrated-trace");
    fs::create_dir(&trace_dir).unwrap();
    let observed = [
        ("queued-work.json", &before_queue.stdout),
        ("foreground-work.json", &foreground.stdout),
        ("queue.txt", &queue.stdout),
        ("status.txt", &status.stdout),
        ("refused.txt", &refused.stderr),
        ("head-before.txt", &old_head.stdout),
        ("head-after.txt", &new_head.stdout),
        ("queue-after-git.txt", &after_git_queue.stdout),
        ("queued-after-git.json", &after_git_queued.stdout),
        ("foreground-after-git.json", &after_git_foreground.stdout),
    ];
    let artifacts = observed
        .iter()
        .map(|(name, bytes)| {
            fs::write(trace_dir.join(name), bytes).unwrap();
            json!({"path":name,"sha256":digest(bytes)})
        })
        .collect::<Vec<_>>();
    let trace = json!({"schema-version":1,"scenario":"integrated-queue-cancel",
    "fluent-commit":String::from_utf8_lossy(&run(source,"git", &["rev-parse","HEAD"]).stdout).trim(),
    "fluent-binary-sha256":digest(&fs::read(env!("CARGO_BIN_EXE_fluent")).unwrap()),
    "model-profile":"service-competition","collection-complete":true,
    "artifacts":artifacts,"events":[
        event(1,"queue-admitted","queue-add",&[],json!({"exit-code":queued.status.code(),
            "work":serde_json::from_slice::<Value>(&before_queue.stdout).unwrap()})),
        event(2,"other-canceled","attempt-cancel",&[1],json!({"exit-code":cancel.status.code(),
            "work":work})),
        event(3,"observed","status",&[2],json!({"exit-code":status.status.code(),
            "queue":String::from_utf8_lossy(&queue.stdout),
            "status":String::from_utf8_lossy(&status.stdout),
            "work":serde_json::from_slice::<Value>(&after_queue.stdout).unwrap()})),
        event(4,"run-refused","task-run",&[3],json!({"exit-code":refused.status.code(),
            "stderr":String::from_utf8_lossy(&refused.stderr),
            "work":serde_json::from_slice::<Value>(&after_foreground.stdout).unwrap()})),
        event(5,"independent-git-change","git-commit",&[4],json!({
            "exit-code":0,"head-before":String::from_utf8_lossy(&old_head.stdout),
            "head-after":String::from_utf8_lossy(&new_head.stdout),
            "queue":String::from_utf8_lossy(&after_git_queue.stdout),
            "queued-work":serde_json::from_slice::<Value>(&after_git_queued.stdout).unwrap(),
            "foreground-work":serde_json::from_slice::<Value>(&after_git_foreground.stdout).unwrap()}))
    ]});
    let trace_path = trace_dir.join("production-trace.json");
    fs::write(&trace_path, serde_json::to_vec_pretty(&trace).unwrap()).unwrap();
    let projector = source.join("tests/formal/workflow/project_integrated.py");
    let projection = run(
        source,
        "python3",
        &[projector.to_str().unwrap(), trace_path.to_str().unwrap()],
    );
    assert!(
        projection.status.success(),
        "{}",
        String::from_utf8_lossy(&projection.stderr)
    );
    let target = source.join("target/formal/workflow/integrated/production");
    fs::create_dir_all(&target).unwrap();
    fs::copy(&trace_path, target.join("production-trace.json")).unwrap();
    for (name, _) in observed {
        fs::copy(trace_dir.join(name), target.join(name)).unwrap();
    }
    let negative_dir = tmp.path().join("negative-trace");
    fs::create_dir(&negative_dir).unwrap();
    for (name, _) in observed {
        fs::copy(trace_dir.join(name), negative_dir.join(name)).unwrap();
    }
    let mut negative = trace.clone();
    negative["events"][1]["raw-facts"]["work"]["id"] = json!("queued");
    negative["events"][3]["raw-facts"]["work"]["id"] = json!("queued");
    let changed = serde_json::to_vec(&negative["events"][1]["raw-facts"]["work"]).unwrap();
    fs::write(negative_dir.join("foreground-work.json"), &changed).unwrap();
    negative["artifacts"][1]["sha256"] = json!(digest(&changed));
    let negative_path = negative_dir.join("production-trace.json");
    fs::write(
        &negative_path,
        serde_json::to_vec_pretty(&negative).unwrap(),
    )
    .unwrap();
    let rejected = run(
        source,
        "python3",
        &[projector.to_str().unwrap(), negative_path.to_str().unwrap()],
    );
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("Work identity changed"));
    let mut ambiguous = trace;
    ambiguous["events"][2]["causal-predecessors"] = json!([1]);
    let ambiguous_path = trace_dir.join("ambiguous-order-trace.json");
    fs::write(
        &ambiguous_path,
        serde_json::to_vec_pretty(&ambiguous).unwrap(),
    )
    .unwrap();
    let rejected = run(
        source,
        "python3",
        &[
            projector.to_str().unwrap(),
            ambiguous_path.to_str().unwrap(),
        ],
    );
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("missing or ambiguous causal predecessor")
    );
    let mut changed_work =
        json!(serde_json::from_slice::<Value>(&fs::read(&trace_path).unwrap()).unwrap());
    changed_work["events"][4]["raw-facts"]["queued-work"]["title"] = json!("changed");
    let changed_work_dir = tmp.path().join("changed-work-trace");
    fs::create_dir(&changed_work_dir).unwrap();
    for (name, _) in observed {
        fs::copy(trace_dir.join(name), changed_work_dir.join(name)).unwrap();
    }
    let bytes = serde_json::to_vec(&changed_work["events"][4]["raw-facts"]["queued-work"]).unwrap();
    fs::write(changed_work_dir.join("queued-after-git.json"), &bytes).unwrap();
    for artifact in changed_work["artifacts"].as_array_mut().unwrap() {
        if artifact["path"] == "queued-after-git.json" {
            artifact["sha256"] = json!(digest(&bytes));
        }
    }
    let changed_path = changed_work_dir.join("production-trace.json");
    fs::write(
        &changed_path,
        serde_json::to_vec_pretty(&changed_work).unwrap(),
    )
    .unwrap();
    let rejected = run(
        source,
        "python3",
        &[projector.to_str().unwrap(), changed_path.to_str().unwrap()],
    );
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("independent Git change altered retained Work or queue")
    );
    let mut changed_queue: Value = serde_json::from_slice(&fs::read(&trace_path).unwrap()).unwrap();
    let changed_queue_dir = tmp.path().join("changed-queue-trace");
    fs::create_dir(&changed_queue_dir).unwrap();
    for (name, _) in observed {
        fs::copy(trace_dir.join(name), changed_queue_dir.join(name)).unwrap();
    }
    let queue_bytes = b"0 changed queued changed\n";
    fs::write(changed_queue_dir.join("queue-after-git.txt"), queue_bytes).unwrap();
    changed_queue["events"][4]["raw-facts"]["queue"] = json!(String::from_utf8_lossy(queue_bytes));
    for artifact in changed_queue["artifacts"].as_array_mut().unwrap() {
        if artifact["path"] == "queue-after-git.txt" {
            artifact["sha256"] = json!(digest(queue_bytes));
        }
    }
    let changed_queue_path = changed_queue_dir.join("production-trace.json");
    fs::write(
        &changed_queue_path,
        serde_json::to_vec(&changed_queue).unwrap(),
    )
    .unwrap();
    let rejected = run(
        source,
        "python3",
        &[
            projector.to_str().unwrap(),
            changed_queue_path.to_str().unwrap(),
        ],
    );
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("independent Git change altered retained Work or queue")
    );
}

#[test]
fn workflow_integrated_queue_claim_holds_bound_attempt_lease() {
    queue_dispatch_trace(false, false);
}

#[test]
fn workflow_integrated_queue_recovery_fences_old_generation() {
    queue_dispatch_trace(true, false);
}

#[test]
fn workflow_integrated_queue_recovery_waits_for_slow_startup() {
    // Exceed the former ten-second cutoff before starting the real coordinator.
    queue_dispatch_trace(true, true);
}

fn queue_dispatch_trace(recover: bool, delay_start: bool) {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    fs::create_dir(&project).unwrap();
    assert!(
        run(&project, "git", &["init", "-b", "main"])
            .status
            .success()
    );
    assert!(
        run(&project, "git", &["config", "user.name", "Test"])
            .status
            .success()
    );
    assert!(
        run(
            &project,
            "git",
            &["config", "user.email", "test@example.com"]
        )
        .status
        .success()
    );
    fs::create_dir_all(project.join(".fluent/work/items")).unwrap();
    fs::create_dir_all(project.join("mock-bin")).unwrap();
    fs::write(
        project.join(".fluent/config.yaml"),
        "scheduler:\n  local-concurrency: 1\n",
    )
    .unwrap();
    let mock = project.join("mock-bin/claude");
    fs::write(
        &mock,
        r##"#!/bin/bash
if [ "$1 $2 $3" = "auth status --json" ]; then echo '{"loggedIn":true}'; exit 0; fi
if [ "$1" = "--version" ]; then echo '2.1.211 (Claude Code)'; exit 0; fi
touch "$FLUENT_QUEUE_WRITER_STARTED"
while [ ! -f "$FLUENT_QUEUE_WRITER_RELEASE" ]; do sleep 0.1; done
git commit --allow-empty -m 'Add fixture write' >/dev/null 2>&1
exit 0
"##,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&mock, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(project.join("README.md"), "queue connection fixture\n").unwrap();
    assert!(run(&project, "git", &["add", "."]).status.success());
    assert!(
        run(&project, "git", &["commit", "-m", "Add fixture"])
            .status
            .success()
    );
    let token = tmp
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>();
    let first = format!("queue{token}a");
    let second = format!("queue{token}b");
    for id in [&first, &second] {
        fs::write(
            project.join(format!(".fluent/work/items/{id}.json")),
            format!("{{\"id\":\"{id}\",\"title\":\"Queue fixture\"}}\n"),
        )
        .unwrap();
        let admitted = fluent(&project, &["queue", "add", id]);
        assert!(
            admitted.status.success(),
            "{}",
            String::from_utf8_lossy(&admitted.stderr)
        );
    }
    let queued_first = fluent(&project, &["queue", "show", &first]);
    let queued_second = fluent(&project, &["queue", "show", &second]);
    assert!(queued_first.status.success() && queued_second.status.success());
    let started = project.join("writer-started");
    let release = project.join("writer-release");
    let mut recovery_snapshots = Vec::new();
    if recover {
        for round in 0..2 {
            let barrier = tmp.path().join(format!("dispatch-barrier-{round}"));
            let scheduler_log = tmp.path().join(format!("dispatch-scheduler-{round}.log"));
            let mut command = if delay_start && round == 0 {
                // Delay in the same process, then exec the real coordinator so
                // killing a timed-out fixture cannot orphan a sleep process.
                let mut command = Command::new("python3");
                command.args([
                    "-c",
                    "import os, sys, time; time.sleep(11); os.execv(sys.argv[1], sys.argv[1:])",
                    env!("CARGO_BIN_EXE_fluent"),
                ]);
                command
            } else {
                Command::new(env!("CARGO_BIN_EXE_fluent"))
            };
            let mut child = command
                .current_dir(&project)
                .env("FLUENT_NO_UPDATE_CHECK", "1")
                .env("FLUENT_TEST_HOST_SANDBOX_PREFLIGHT", "pass")
                .env("FLUENT_TEST_DISPATCH_RUNNING_BARRIER", &barrier)
                .args(["scheduler", "run", "--poll-seconds", "1"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::from(
                    fs::File::create(&scheduler_log).unwrap(),
                ))
                .spawn()
                .unwrap();
            // Startup hashes the executable and prepares the coordinator before
            // claiming Work. Use the same allowance as the Writer-start wait
            // below so parallel suite load does not cause a false recovery failure.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while !barrier.exists() && std::time::Instant::now() < deadline {
                if child.try_wait().unwrap().is_some() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            // Kill and reap even when the barrier is missing; failure must not
            // leave a coordinator running in the fixture.
            let reached = barrier.exists();
            let snapshot = fluent(&project, &["queue", "show", &first]);
            let work = fluent(&project, &["work-item", "show", &first]);
            let held =
                fluent::lease::is_leased(&fluent::queue::dispatch_lease_path(&project, &first));
            if child.try_wait().unwrap().is_none() {
                child.kill().unwrap();
            }
            let status = child.wait().unwrap();
            assert!(
                reached,
                "scheduler did not reach the running dispatch barrier in round {round} ({status}): {}",
                fs::read_to_string(&scheduler_log).unwrap()
            );
            assert!(snapshot.status.success() && work.status.success() && held);
            assert!(!fluent::lease::is_leased(
                &fluent::queue::dispatch_lease_path(&project, &first)
            ));
            assert!(!fluent::scheduler::coordinator_is_live(&project));
            recovery_snapshots.push(json!({
                "dispatch":serde_json::from_slice::<Value>(&snapshot.stdout).unwrap(),
                "work":serde_json::from_slice::<Value>(&work.stdout).unwrap(),
                "lease-before-crash":held,"lease-after-crash":false
            }));
        }
    }
    let mut scheduler = Command::new(env!("CARGO_BIN_EXE_fluent"))
        .current_dir(&project)
        .env("FLUENT_NO_UPDATE_CHECK", "1")
        .env("FLUENT_TEST_HOST_SANDBOX_PREFLIGHT", "pass")
        .env("FLUENT_TEST_HERMETIC_PROVIDERS", "1")
        .env("FLUENT_QUEUE_WRITER_STARTED", &started)
        .env("FLUENT_QUEUE_WRITER_RELEASE", &release)
        .env(
            "PATH",
            format!(
                "{}:{}",
                project.join("mock-bin").display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .args(["scheduler", "run", "--poll-seconds", "1"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(
            fs::File::create(tmp.path().join("scheduler.log")).unwrap(),
        ))
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut running_first = None;
    while std::time::Instant::now() < deadline {
        let shown = fluent(&project, &["queue", "show", &first]);
        if shown.status.success()
            && serde_json::from_slice::<Value>(&shown.stdout).unwrap()["status"] == "running"
            && started.exists()
        {
            running_first = Some(shown);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let running_second = fluent(&project, &["queue", "show", &second]);
    let running_work = fluent(&project, &["work-item", "show", &first]);
    let competing_work = fluent(&project, &["work-item", "show", &second]);
    let lease_live = fluent::lease::is_leased(
        &project.join(format!(".fluent/work/scheduler/leases/{first}.lock")),
    );
    let coordinator_live = fluent::scheduler::coordinator_is_live(&project);
    let mut stale_results = Vec::new();
    if recover {
        for snapshot in &recovery_snapshots {
            let dispatch: fluent::queue::Dispatch =
                serde_json::from_value(snapshot["dispatch"].clone()).unwrap();
            let token = fluent::queue::DispatchToken {
                work_item_id: dispatch.work_item_id,
                dispatch_id: dispatch.dispatch_id,
                generation: dispatch.generation,
                priority: dispatch.priority,
                bound_attempt_id: dispatch.bound_attempt_id.unwrap(),
                tester_no_sandbox: dispatch.tester_no_sandbox,
            };
            let before = fluent(&project, &["queue", "show", &first]);
            let rejected =
                fluent::queue::reconcile(&project, &token, fluent::queue::DispatchStatus::Failed)
                    .unwrap_err();
            let after = fluent(&project, &["queue", "show", &first]);
            assert_eq!(
                before.stdout, after.stdout,
                "stale settlement changed the queue"
            );
            stale_results.push(json!({"generation": token.generation,
                "error":format!("{rejected:#}"),
                "before":serde_json::from_slice::<Value>(&before.stdout).unwrap(),
                "after":serde_json::from_slice::<Value>(&after.stdout).unwrap()}));
        }
    }
    fs::write(&release, b"release").unwrap();
    let stopped = fluent(&project, &["scheduler", "stop"]);
    let _ = scheduler.wait();
    assert!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    let running_first = running_first.unwrap_or_else(|| {
        panic!(
            "public scheduler did not run queued Work: {}",
            fs::read_to_string(tmp.path().join("scheduler.log")).unwrap()
        )
    });
    assert!(
        running_second.status.success()
            && running_work.status.success()
            && competing_work.status.success()
    );
    assert!(lease_live, "running dispatch had no held Attempt lease");
    assert!(
        coordinator_live,
        "queued claim had no elected project coordinator"
    );

    let target = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/formal/workflow/integrated/production")
        .join(if delay_start {
            "queue-recovery-delayed"
        } else if recover {
            "queue-recovery"
        } else {
            "queue-dispatch"
        });
    fs::create_dir_all(&target).unwrap();
    let observed = [
        ("first-queued.json", &queued_first.stdout),
        ("second-queued.json", &queued_second.stdout),
        ("first-running.json", &running_first.stdout),
        ("second-running.json", &running_second.stdout),
        ("first-work.json", &running_work.stdout),
        ("second-work.json", &competing_work.stdout),
    ];
    let artifacts = observed
        .iter()
        .map(|(name, bytes)| {
            fs::write(target.join(name), bytes).unwrap();
            json!({"path":name,"sha256":digest(bytes)})
        })
        .collect::<Vec<_>>();
    let first_running: Value = serde_json::from_slice(&running_first.stdout).unwrap();
    let second_running: Value = serde_json::from_slice(&running_second.stdout).unwrap();
    let first_work: Value = serde_json::from_slice(&running_work.stdout).unwrap();
    let second_work: Value = serde_json::from_slice(&competing_work.stdout).unwrap();
    let trace = json!({"schema-version":1,"scenario":"queue-dispatch-connection",
    "fluent-commit":String::from_utf8_lossy(&run(Path::new(env!("CARGO_MANIFEST_DIR")),
        "git", &["rev-parse","HEAD"]).stdout).trim(),
    "fluent-binary-sha256":digest(&fs::read(env!("CARGO_BIN_EXE_fluent")).unwrap()),
    "model-profile":"queue-dispatch-connection","collection-complete":true,
    "artifacts":artifacts,"events":[
        json!({"sequence":1,"actor":"operator","operation-id":"queue-add",
            "generation":0,"boundary":"two-queued","causal-predecessors":[],
            "raw-facts":{"first":serde_json::from_slice::<Value>(&queued_first.stdout).unwrap(),
                         "second":serde_json::from_slice::<Value>(&queued_second.stdout).unwrap()}}),
        json!({"sequence":2,"actor":"scheduler","operation-id":"scheduler-run",
            "generation":first_running["generation"],"boundary":"claim-lease-run",
            "causal-predecessors":[1],"raw-facts":{"first":first_running,
                "second":second_running,"first-work":first_work,
                    "second-work":second_work,"lease-live":lease_live,
                    "coordinator-live":coordinator_live}})
    ]});
    let trace_path = target.join("production-trace.json");
    fs::write(&trace_path, serde_json::to_vec_pretty(&trace).unwrap()).unwrap();
    let projection = run(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        "python3",
        &[
            "tests/formal/workflow/project_queue_dispatch.py",
            trace_path.to_str().unwrap(),
        ],
    );
    if !recover {
        assert!(
            projection.status.success(),
            "{}",
            String::from_utf8_lossy(&projection.stderr)
        );
    }
    if recover {
        let mut facts = recovery_snapshots;
        facts.push(json!({"continued":trace,"stale-settlements":stale_results}));
        let mut artifacts = Vec::new();
        let mut events = Vec::new();
        for (index, fact) in facts.iter().enumerate() {
            let name = format!("recovery-{index}.json");
            let bytes = serde_json::to_vec_pretty(fact).unwrap();
            fs::write(target.join(&name), &bytes).unwrap();
            artifacts.push(json!({"path":name,"sha256":digest(&bytes)}));
            events.push(event(
                index as u64 + 1,
                if index < 2 {
                    "process-crash"
                } else {
                    "continued-and-fenced"
                },
                if index < 2 {
                    "kill-scheduler"
                } else {
                    "scheduler-run"
                },
                &if index == 0 {
                    vec![]
                } else {
                    vec![index as u64]
                },
                fact.clone(),
            ));
        }
        let recovery = json!({"schema-version":1,"scenario":"queue-recovery",
            "fluent-commit":trace["fluent-commit"],
            "fluent-binary-sha256":trace["fluent-binary-sha256"],
            "model-profile":"queue-recovery","collection-complete":true,
            "events":events,"artifacts":artifacts});
        let path = target.join("recovery-trace.json");
        fs::write(&path, serde_json::to_vec_pretty(&recovery).unwrap()).unwrap();
        let projected = run(
            Path::new(env!("CARGO_MANIFEST_DIR")),
            "python3",
            &[
                "tests/formal/workflow/project_queue_recovery.py",
                path.to_str().unwrap(),
            ],
        );
        assert!(
            projected.status.success(),
            "{}",
            String::from_utf8_lossy(&projected.stderr)
        );
    }
}

#[test]
fn workflow_assurance_report_preserves_release_evidence() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let result = run(
        root,
        "python3",
        &["tests/formal/workflow/test_release_evidence.py"],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn workflow_assurance_report_accepts_single_nextest_result() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = r#"
import sys
from pathlib import Path
from subprocess import CompletedProcess
sys.path.insert(0, str(Path(sys.argv[1])))
import check
singular = 'Summary [ 0.100s] 1 test run: 1 passed, 204 skipped\n'
plural = 'Summary [ 0.100s] 1 tests run: 1 passed, 204 skipped\n'
for summary in (singular, plural):
    for stdout, stderr in ((summary, ''), ('', summary)):
        assert check.named_test_passed(CompletedProcess([], 0, stdout, stderr)), summary
for stdout, stderr, code in (
    ('', '', 0),
    ('PASS named_test\n', '', 0),
    ('Summary [ 0.100s] 0 tests run: 0 passed\n', '', 0),
    ('Summary [ 0.100s] 2 tests run: 2 passed\n', '', 0),
    ('Summary [ 0.100s] 1 test run: 0 passed, 1 failed\n', '', 1),
    ('Summary [ 0.100s] 1 test run: 0 passed, 1 failed\n', '', 0),
    ('Summary [ 0.100s] 1 test run: 1 passed, 1 failed\n', '', 0),
    (singular, '', 1),
    (singular, singular, 0),
    (singular + 'Summary [ 0.100s] 0 tests run: 0 passed\n', '', 0),
):
    assert not check.named_test_passed(CompletedProcess([], code, stdout, stderr)), (stdout, stderr, code)
assert not check.named_test_passed(None)
"#;
    let result = run(
        root,
        "python3",
        &[
            "-c",
            script,
            root.join("tests/formal/workflow").to_str().unwrap(),
        ],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
