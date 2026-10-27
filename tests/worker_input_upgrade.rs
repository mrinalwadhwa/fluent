#![cfg(all(unix, feature = "test-support"))]

use fluent::git;
use fluent::work_model::{
    ArtifactRef, PlanningContext, TaskKind, TaskStatus, WRITER_COMPLETION_CONTRACT_VERSION,
    WorkItem, WorkModelStore, WriterCompletionContract,
};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

const WORK: &str = "input-upgrade";
const ASSIGNMENT: &str = "Create result.txt containing the saved instruction marker.";

fn executable(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

struct Fixture {
    root: tempfile::TempDir,
    project: PathBuf,
    task: String,
    handoff_executable: OnceLock<PathBuf>,
}

impl Fixture {
    fn new(review: bool, planning: bool) -> Self {
        // A real host run must not inherit broad /private/tmp read/write grants.
        let root =
            tempfile::tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("target")).unwrap();
        let project = root.path().join("source");
        fs::create_dir_all(project.join(".fluent/expertise")).unwrap();
        fs::write(project.join(".gitignore"), ".fluent/work/\n").unwrap();
        fs::write(project.join("README.md"), "source product\n").unwrap();
        fs::write(
            project.join(".fluent/expertise/INDEX.md"),
            "project expertise\n",
        )
        .unwrap();
        git::run(&project, &["init", "-b", "main"], "init").unwrap();
        git::run(&project, &["config", "user.name", "Fixture"], "config").unwrap();
        git::run(
            &project,
            &["config", "user.email", "fixture@example.test"],
            "config",
        )
        .unwrap();
        git::run(&project, &["config", "commit.gpgsign", "false"], "config").unwrap();
        git::run(&project, &["add", "."], "stage").unwrap();
        git::run(&project, &["commit", "-m", "Add fixture"], "commit").unwrap();
        let head = git::run_stdout(&project, &["rev-parse", "HEAD"], "head").unwrap();
        let mut item = WorkItem::planned(WORK, "A title without the assignment");
        item.instructions = Some(ASSIGNMENT.into());
        if planning {
            item.planning_context = Some(PlanningContext {
                brief: Some("# Brief\nRead declared inputs.\n".into()),
                behaviors: Some("# Behaviors\nRead declared inputs.\n".into()),
                approach: Some("# Approach\nProbe exact files.\n".into()),
                plan: Some("# Plan\nProbe exact files.\n".into()),
                ..Default::default()
            });
        }
        item.add_initial_attempt("attempt-1").unwrap();
        if review {
            let writer = &mut item.attempts[0].tasks[0];
            let workspace = writer.workspace_access.writes[0].clone();
            let candidate = project.join(&workspace.path);
            git::run(
                &project,
                &["worktree", "add", "--detach", candidate.to_str().unwrap()],
                "create review candidate",
            )
            .unwrap();
            writer.status = TaskStatus::Complete;
            writer.output = Some(fluent::work_model::TaskOutput {
                workspace_id: workspace.id,
                workspace_path: workspace.path,
                source_branch: "main".into(),
                base_commit: Some(head.clone()),
                commit: head,
                no_change: None,
                learner_canonicalization: None,
            });
            item.add_review_tasks("attempt-1", &["tests"]).unwrap();
        } else {
            item.attempts[0].writer_completion_contract = Some(WriterCompletionContract {
                version: WRITER_COMPLETION_CONTRACT_VERSION,
                requirements: vec![],
            });
        }
        let task = item.attempts[0]
            .tasks
            .iter_mut()
            .find(|t| {
                t.kind
                    == if review {
                        TaskKind::Review
                    } else {
                        TaskKind::Write
                    }
            })
            .unwrap();
        let correction = ".fluent/work/artifacts/input-upgrade/attempt-1/prior/review.md";
        fs::create_dir_all(project.join(correction).parent().unwrap()).unwrap();
        fs::write(
            project.join(correction),
            "Verdict: fail\n\n- [ ] Preserve the current finding marker (blocking)\n",
        )
        .unwrap();
        task.input_artifacts.push(ArtifactRef {
            producer_id: "prior-review".into(),
            path: correction.into(),
        });
        let task_id = task.id.clone();
        for artifact in &task.input_artifacts {
            let path = project.join(&artifact.path);
            if !path.exists() {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, "{}\n").unwrap();
            }
        }
        WorkModelStore::new(&project)
            .create_work_item(&item)
            .unwrap();
        let progress = project.join(format!(
            ".fluent/work/progress/{WORK}/attempt-1/progress.md"
        ));
        fs::create_dir_all(progress.parent().unwrap()).unwrap();
        fs::write(progress, "- [ ] Read declared inputs\n").unwrap();
        let bin = root.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(root.path().join("home")).unwrap();
        executable(
            &bin.join("claude"),
            r#"#!/bin/bash
set -eu
if [ "${1:-}" = --version ]; then echo '2.1.220 (Claude Code)'; exit 0; fi
if [ "${1:-} ${2:-} ${3:-}" = 'auth status --json' ]; then echo '{"loggedIn":true}'; exit 0; fi
case "$*" in *FLUENT_CLAUDE_READY*) echo '[{"type":"result","subtype":"success","is_error":false,"result":"FLUENT_CLAUDE_READY"}]'; exit 0;; esac
printf '%s\n' "$@" > "$WORKER_CAPTURE"
assignment="$(printf '%s\n' "$@" | sed -n 's/^Read the saved Task assignment at \(.*\)\.$/\1/p')"
if [ -n "$assignment" ]; then cat "$assignment" > "$WORKER_CAPTURE.assignment"; fi
exit 73
"#,
        );
        executable(
            &bin.join("sandbox-exec"),
            "#!/bin/bash\nset -eu\n[ \"$1\" = -f ]\ncp \"$2\" \"$WORKER_PROFILE\"\nshift 2\nexec \"$@\"\n",
        );
        executable(
            &bin.join("read-probe"),
            r#"#!/bin/bash
set -eu
[ "$1" = -f ]
cp "$2" "$WORKER_PROFILE"
shift 2
program="$1"
shift
for path in "$@"; do
  [ "$path" = -1 ] && continue
  printf '%s\n' "$path" >> "$WORKER_READS"
  if [ "$path" = "${WORKER_DENIED:-}" ]; then
    printf 'fixture denied exact input: %s\n' "$path" >&2
    exit 1
  fi
done
exec "$program" "$@"
"#,
        );
        Self {
            root,
            project,
            task: task_id,
            handoff_executable: OnceLock::new(),
        }
    }

    fn handoff_executable(&self, candidate: &Path) -> &Path {
        self.handoff_executable.get_or_init(|| {
            let tools = candidate.join(".fluent/work/tools");
            fs::create_dir_all(&tools).unwrap();
            let binary = std::env::var_os("FLUENT_BIN_OVERRIDE")
                .map(PathBuf::from)
                .unwrap_or_else(|| assert_cmd::cargo::cargo_bin!("fluent").into());
            let mut source = fs::File::open(&binary).unwrap();
            let metadata = source.metadata().unwrap();
            assert!(metadata.is_file());
            let path = tools.join("fluent");
            // Never overwrite a previously executed signed image. Each fixture
            // stages one fresh file and all of its later handoffs reuse it.
            let mut staged = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .unwrap();
            std::io::copy(&mut source, &mut staged).unwrap();
            staged.set_permissions(metadata.permissions()).unwrap();
            staged.sync_all().unwrap();
            let actual = fs::symlink_metadata(&path).unwrap();
            let opened = staged.metadata().unwrap();
            assert!(actual.is_file());
            assert_eq!((actual.dev(), actual.ino()), (opened.dev(), opened.ino()));
            assert_eq!(actual.nlink(), 1);
            assert_eq!(actual.len(), metadata.len());
            let expected_digest = Sha256::digest(fs::read(&binary).unwrap());
            assert_eq!(Sha256::digest(fs::read(&path).unwrap()), expected_digest);
            eprintln!(
                "staged handoff executable {}: device={}, inode={}, sha256={expected_digest:x}",
                path.display(),
                actual.dev(),
                actual.ino()
            );
            path
        })
    }

    fn command(&self) -> Command {
        let binary = std::env::var_os("FLUENT_BIN_OVERRIDE")
            .map(PathBuf::from)
            .unwrap_or_else(|| assert_cmd::cargo::cargo_bin!("fluent").into());
        let mut cmd = Command::new(binary);
        cmd.current_dir(&self.project)
            .env("HOME", self.root.path().join("home"))
            .env("FLUENT_NO_UPDATE_CHECK", "1")
            .env("FLUENT_TEST_HERMETIC_PROVIDERS", "1")
            .env("FLUENT_TEST_HOST_SANDBOX_PREFLIGHT", "pass")
            .env(
                "FLUENT_TEST_WORKER_READ_PROBE",
                self.root.path().join("bin/read-probe"),
            )
            .env("FLUENT_MAX_TASK_RETRIES", "0")
            .env("WORKER_CAPTURE", self.root.path().join("prompt"))
            .env("WORKER_PROFILE", self.root.path().join("profile.sb"))
            .env("WORKER_READS", self.root.path().join("reads"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.root.path().join("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            );
        for key in [
            "FLUENT_TASK_KIND",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "OPENAI_API_KEY",
        ] {
            cmd.env_remove(key);
        }
        cmd
    }

    fn import_released(&self) {
        let released: serde_json::Value =
            serde_json::from_str(include_str!("fixtures/worker-input-060.json")).unwrap();
        assert_eq!(
            released["release"]["commit"],
            "be19732c42ac645cb9fac5701c8e93171b1aa480"
        );
        for (relative, record) in released["records"].as_object().unwrap() {
            let path = self.project.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, serde_json::to_vec_pretty(record).unwrap()).unwrap();
        }
    }

    fn run(&self, denied: Option<&Path>) -> Output {
        let mut cmd = self.command();
        cmd.args(["task", "run", WORK, "attempt-1", &self.task]);
        if let Some(path) = denied {
            cmd.env("WORKER_DENIED", path);
        }
        let output = cmd.output().unwrap();
        fs::create_dir_all("tests/output").unwrap();
        fs::write(
            format!(
                "tests/output/worker-input-{}.log",
                self.root.path().file_name().unwrap().to_string_lossy()
            ),
            [&output.stdout[..], &output.stderr[..]].concat(),
        )
        .unwrap();
        output
    }
}

fn real_profile_available(fixture: &Fixture) -> bool {
    let output = Command::new("/usr/bin/sandbox-exec")
        .arg("-f")
        .arg(fixture.root.path().join("profile.sb"))
        .arg("/usr/bin/true")
        .output()
        .unwrap();
    if output.status.success() {
        return true;
    }
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        std::env::var_os("FLUENT_WORKER_INPUT_REQUIRE_SEATBELT").is_none(),
        "host acceptance requires real profile enforcement: {error}"
    );
    assert!(
        error.contains("Operation not permitted"),
        "unexpected sandbox error: {error}"
    );
    eprintln!("Real worker confinement unavailable: {error}; host acceptance remains pending.");
    false
}

fn active_handoff_fixture() -> (Fixture, PathBuf) {
    let fixture = Fixture::new(false, false);
    let output = fixture.run(None);
    assert!(
        fixture.root.path().join("profile.sb").exists(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let store = WorkModelStore::new(&fixture.project);
    let mut item = store.read_work_item_snapshot(WORK).unwrap();
    // Construct an active host binding in this isolated fixture, retaining the
    // baseline and final profile generated by the public launch above.
    item.attempts[0].status = fluent::work_model::AttemptStatus::Executing;
    item.attempts[0].pause_kind = None;
    item.attempts[0].writer_completion_contract = None;
    item.attempts[0].tasks[0].status = TaskStatus::Executing;
    let candidate = fs::canonicalize(
        fixture
            .project
            .join(&item.attempts[0].tasks[0].workspace_access.writes[0].path),
    )
    .unwrap();
    store.write_work_item(&item).unwrap();
    fs::write(candidate.join("result.txt"), "saved instruction marker\n").unwrap();
    git::run(&candidate, &["add", "result.txt"], "stage candidate").unwrap();
    git::run(
        &candidate,
        &[
            "commit",
            "-m",
            "Add result",
            "-m",
            "- Preserve saved instructions",
        ],
        "commit candidate",
    )
    .unwrap();
    (fixture, candidate)
}

fn handoff_command(
    fixture: &Fixture,
    candidate: &Path,
    sandboxed: bool,
    task: &str,
    phase: &str,
) -> Output {
    let mut cmd = if sandboxed {
        let executable = fixture.handoff_executable(candidate);
        let mut cmd = Command::new("/usr/bin/sandbox-exec");
        cmd.arg("-f")
            .arg(fixture.root.path().join("profile.sb"))
            .arg(executable);
        cmd.env("HOME", fixture.root.path().join("home"))
            .env("FLUENT_NO_UPDATE_CHECK", "1")
            .env_remove("FLUENT_TASK_KIND");
        cmd
    } else {
        fixture.command()
    };
    let executable = if sandboxed {
        candidate.join(".fluent/work/tools/fluent")
    } else {
        PathBuf::from(cmd.get_program())
    };
    cmd.current_dir(candidate)
        .args(["task", "check-handoff", WORK, "attempt-1", task]);
    capture_handoff_command(&mut cmd, &executable, sandboxed, phase).0
}

fn capture_handoff_command(
    cmd: &mut Command,
    executable: &Path,
    sandboxed: bool,
    phase: &str,
) -> (Output, PathBuf) {
    fs::create_dir_all("tests/output").unwrap();
    // Keep each invocation outside the disposable fixture, including expected
    // negative controls. Unique files preserve concurrent test execution.
    let (_, path) = tempfile::Builder::new()
        .prefix("worker-handoff-")
        .suffix(".json")
        .tempfile_in("tests/output")
        .unwrap()
        .keep()
        .unwrap();
    let candidate = cmd.get_current_dir().unwrap();
    // Capture only explicit noncredential context, never Command's Debug
    // representation or its inherited/configured environment.
    let mut log = serde_json::json!({
        "phase": phase,
        "sandboxed": sandboxed,
        "executable": executable,
        "program": cmd.get_program().to_string_lossy(),
        "args": cmd.get_args().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>(),
        "candidate": candidate,
        "candidate_commit": git::run_stdout(candidate, &["rev-parse", "HEAD"], "diagnostic candidate").ok(),
        "started_unix_ms": SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis(),
    });
    fs::write(&path, serde_json::to_vec_pretty(&log).unwrap()).unwrap();
    let result = (|| -> std::io::Result<Output> {
        let child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        log["pid"] = child.id().into();
        let saved = fs::write(&path, serde_json::to_vec_pretty(&log).unwrap());
        let output = child.wait_with_output();
        saved?;
        output
    })();
    match &result {
        Ok(output) => {
            log["status"] = serde_json::json!({
                "display": output.status.to_string(),
                "debug": format!("{:?}", output.status),
                "raw": output.status.into_raw(),
                "success": output.status.success(),
                "exit_code": output.status.code(),
                "signal": output.status.signal(),
                "core_dumped": output.status.core_dumped(),
            });
            log["stdout"] = String::from_utf8_lossy(&output.stdout).into_owned().into();
            log["stderr"] = String::from_utf8_lossy(&output.stderr).into_owned().into();
            log["stdout_bytes"] = serde_json::json!(output.stdout);
            log["stderr_bytes"] = serde_json::json!(output.stderr);
        }
        Err(error) => log["error"] = error.to_string().into(),
    }
    fs::write(&path, serde_json::to_vec_pretty(&log).unwrap()).unwrap();
    // The host driver retains --nocapture output. Include the entire receipt so
    // diagnostics survive even when it archives only the command's results.log.
    eprintln!("handoff-child {}: {log}", path.display());
    (
        result.unwrap_or_else(|error| panic!("handoff child failed: {error}; {log}")),
        path,
    )
}

#[test]
fn handoff_child_diagnostics_retain_exit_and_signal() {
    fs::create_dir_all("tests/output").unwrap();
    let root = tempfile::tempdir_in("target").unwrap();
    for (phase, script, exit_code, signal) in [
        (
            "negative-control",
            "printf 'out\\377'; printf 'err\\376' >&2; exit 29",
            Some(29),
            None,
        ),
        (
            "signal",
            "printf 'out\\377'; printf 'err\\376' >&2; kill -KILL $$",
            None,
            Some(9),
        ),
        ("silent-signal", "kill -KILL $$", None, Some(9)),
    ] {
        let mut cmd = Command::new("/bin/sh");
        cmd.current_dir(root.path()).args(["-c", script]);
        // Never include inherited or explicitly supplied environment values.
        cmd.env("HANDOFF_DIAGNOSTIC_SECRET", "private-marker");
        let (output, path) = capture_handoff_command(&mut cmd, Path::new("/bin/sh"), false, phase);
        assert!(!output.status.success());
        let bytes = fs::read(&path).unwrap();
        let log: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(log["pid"].as_u64().unwrap() > 0);
        assert_eq!(log["phase"], phase);
        assert_eq!(log["sandboxed"], false);
        assert_eq!(log["executable"], "/bin/sh");
        assert_eq!(log["program"], "/bin/sh");
        assert_eq!(log["args"], serde_json::json!(["-c", script]));
        assert_eq!(log["candidate"], root.path().to_string_lossy().as_ref());
        assert_eq!(log["status"]["exit_code"], serde_json::json!(exit_code));
        assert_eq!(log["status"]["signal"], serde_json::json!(signal));
        assert_eq!(log["status"]["display"], output.status.to_string());
        assert_eq!(log["status"]["raw"], output.status.into_raw());
        assert_eq!(log["status"]["core_dumped"], output.status.core_dumped());
        assert_eq!(log["status"]["success"], false);
        assert_eq!(log["stdout_bytes"], serde_json::json!(output.stdout));
        assert_eq!(log["stderr_bytes"], serde_json::json!(output.stderr));
        if phase == "silent-signal" {
            assert!(output.stdout.is_empty());
            assert!(output.stderr.is_empty());
        } else {
            assert_eq!(output.stdout, b"out\xff");
            assert_eq!(output.stderr, b"err\xfe");
        }
        assert!(!String::from_utf8_lossy(&bytes).contains("private-marker"));
    }
}

#[test]
fn repeated_handoffs_preserve_staged_executable() {
    let (fixture, candidate) = active_handoff_fixture();
    let real = real_profile_available(&fixture);
    let first = handoff_command(&fixture, &candidate, true, &fixture.task, "staging-first");
    if real {
        assert!(first.status.success(), "{first:?}");
        let report: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
        assert_eq!(
            report["candidate_commit"],
            git::run_stdout(&candidate, &["rev-parse", "HEAD"], "head").unwrap()
        );
    }
    // Even when this Writer cannot apply nested Seatbelt, staging happens before
    // launch. Keep a descriptor to detect replacement as well as in-place writes.
    let path = candidate.join(".fluent/work/tools/fluent");
    let descriptor = fs::File::open(&path).unwrap();
    let bytes = fs::read(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o555)).unwrap();
    let identity = |metadata: fs::Metadata| {
        (
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mode(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        )
    };
    let before = identity(descriptor.metadata().unwrap());
    for (task, phase) in [
        (&fixture.task[..], "staging-repeat"),
        ("foreign-task", "staging-foreign"),
    ] {
        let output = handoff_command(&fixture, &candidate, true, task, phase);
        assert_eq!(identity(fs::symlink_metadata(&path).unwrap()), before);
        assert_eq!(identity(descriptor.metadata().unwrap()), before);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        if real && task == fixture.task {
            assert!(output.status.success(), "{output:?}");
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(
                report["candidate_commit"],
                git::run_stdout(&candidate, &["rev-parse", "HEAD"], "head").unwrap()
            );
        } else if real && output.status.success() {
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["overall"], "not-verifiable");
        }
    }
}

#[test]
fn writer_check_handoff_runs_under_production_profile() {
    let (fixture, candidate) = active_handoff_fixture();
    // A registered but unavailable sibling must not abort discovery of the
    // assigned host binding. It supplies no authority for this Writer.
    let sibling = fixture.root.path().join("unavailable-worktree");
    git::run(
        &fixture.project,
        &["worktree", "add", "--detach", sibling.to_str().unwrap()],
        "add sibling",
    )
    .unwrap();
    fs::remove_dir_all(&sibling).unwrap();
    let profile = fs::read_to_string(fixture.root.path().join("profile.sb")).unwrap();
    assert!(
        profile.contains(&format!(
            "(allow file-read* (literal \"{}\"))",
            fixture.project.display()
        )),
        "Writer must be able to enter the source directory without reading its tree"
    );
    let before = WorkModelStore::new(&fixture.project)
        .read_work_item_snapshot(WORK)
        .unwrap();
    let head = git::run_stdout(&candidate, &["rev-parse", "HEAD"], "candidate").unwrap();
    let real = real_profile_available(&fixture);
    for sandboxed in [false, true]
        .into_iter()
        .filter(|sandboxed| !sandboxed || real)
    {
        let output = handoff_command(
            &fixture,
            &candidate,
            sandboxed,
            &fixture.task,
            "valid-binding",
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["candidate_commit"], head);
        assert!(
            report["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|check| check["name"] == "candidate-identity" && check["status"] == "passed"),
            "{report}"
        );
        let foreign = handoff_command(
            &fixture,
            &candidate,
            sandboxed,
            "foreign-task",
            "foreign-task",
        );
        if foreign.status.success() {
            let report: serde_json::Value = serde_json::from_slice(&foreign.stdout).unwrap();
            assert_eq!(report["overall"], "not-verifiable");
        }
        assert_eq!(
            WorkModelStore::new(&fixture.project)
                .read_work_item_snapshot(WORK)
                .unwrap(),
            before
        );
    }
    // An older running worker may still carry the pre-upgrade profile. Git's
    // inability to enter the source must produce data, never an apparent pass.
    let git_double = fixture.root.path().join("bin/git");
    executable(
        &git_double,
        r#"#!/bin/sh
if [ "$PWD" = "${WORKER_GIT_DENIED_SOURCE:-}" ]; then
  echo 'fixture denied source working directory' >&2
  exit 128
fi
exec /usr/bin/git "$@"
"#,
    );
    let mut cmd = fixture.command();
    let selected_executable = PathBuf::from(cmd.get_program());
    cmd.current_dir(&candidate)
        .env("WORKER_GIT_DENIED_SOURCE", &fixture.project)
        .args(["task", "check-handoff", WORK, "attempt-1", &fixture.task]);
    let output = capture_handoff_command(
        &mut cmd,
        &selected_executable,
        false,
        "source-directory-denied",
    )
    .0;
    fs::remove_file(git_double).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["overall"], "not-verifiable");
    assert!(
        report
            .to_string()
            .contains("fixture denied source working directory")
    );

    let hooks = fixture.project.join(".fluent/hooks");
    fs::create_dir_all(&hooks).unwrap();
    executable(
        &hooks.join("check-pre-merge"),
        "#!/bin/sh\necho 'fixture policy rejects candidate'\nexit 29\n",
    );
    assert!(
        profile.contains(&format!(
            "(subpath \"{}\")",
            hooks.join("check-pre-merge").display()
        )),
        "the source policy must not become invisible"
    );
    let output = handoff_command(
        &fixture,
        &candidate,
        real,
        &fixture.task,
        "source-policy-rejection",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_ne!(
        report["overall"], "passed",
        "source policy rejection was skipped: {report}"
    );

    // A genuinely inaccessible host record must yield an explicit unverified
    // report and must never be repaired or rewritten by this command.
    let record = WorkModelStore::new(&fixture.project)
        .work_item_path(WORK)
        .unwrap();
    let bytes = fs::read(&record).unwrap();
    fs::set_permissions(&record, fs::Permissions::from_mode(0o000)).unwrap();
    let output = handoff_command(
        &fixture,
        &candidate,
        real,
        &fixture.task,
        "unreadable-host-record",
    );
    fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["overall"], "not-verifiable");
    assert_eq!(fs::read(&record).unwrap(), bytes);
    let mut stale = before;
    stale.attempts[0].tasks[0].status = TaskStatus::Failed;
    WorkModelStore::new(&fixture.project)
        .write_work_item(&stale)
        .unwrap();
    let output = handoff_command(&fixture, &candidate, real, &fixture.task, "stale-task");
    if output.status.success() {
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["overall"], "not-verifiable");
    }
}

#[test]
fn handoff_grants_preserve_source_and_other_work_boundaries() {
    let (fixture, candidate) = active_handoff_fixture();
    let profile = fs::read_to_string(fixture.root.path().join("profile.sb")).unwrap();
    assert!(!profile.contains(&format!(
        "(allow file-read*  (subpath \"{}\"))",
        fixture.project.display()
    )));
    assert!(
        !profile.contains(&format!(
            "(allow file-write* (subpath \"{}\"))",
            fixture.project.join(".fluent/work/progress").display()
        )),
        "unrelated progress must stay host-owned"
    );
    let private = fixture.root.path().join("home/private-state");
    fs::write(&private, "operator private\n").unwrap();
    let unrelated = fixture
        .project
        .join(".fluent/work/progress/other/attempt-1/progress.md");
    fs::create_dir_all(unrelated.parent().unwrap()).unwrap();
    fs::write(&unrelated, "unrelated Work\n").unwrap();
    let record = WorkModelStore::new(&fixture.project)
        .work_item_path(WORK)
        .unwrap();
    let product = fixture.project.join("README.md");
    let paths = [&product, &record, &unrelated, &private];
    let before: Vec<_> = paths.iter().map(|path| fs::read(path).unwrap()).collect();
    if real_profile_available(&fixture) {
        for path in &paths {
            let output = Command::new("/usr/bin/sandbox-exec")
                .arg("-f")
                .arg(fixture.root.path().join("profile.sb"))
                .args(["/bin/sh", "-c", "printf forbidden >> \"$1\"", "probe"])
                .arg(path)
                .output()
                .unwrap();
            assert!(
                !output.status.success(),
                "profile allowed write to {}",
                path.display()
            );
        }
        for path in [&unrelated, &private] {
            let output = Command::new("/usr/bin/sandbox-exec")
                .arg("-f")
                .arg(fixture.root.path().join("profile.sb"))
                .arg("/bin/cat")
                .arg(path)
                .output()
                .unwrap();
            assert!(
                !output.status.success(),
                "profile exposed {}",
                path.display()
            );
        }
        let output = Command::new("/usr/bin/sandbox-exec")
            .arg("-f")
            .arg(fixture.root.path().join("profile.sb"))
            .args(["/bin/sh", "-c", "printf allowed > \"$1\"", "probe"])
            .arg(candidate.join("allowed.txt"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(
        paths
            .iter()
            .map(|path| fs::read(path).unwrap())
            .collect::<Vec<_>>(),
        before
    );
}

#[test]
fn released_instruction_only_work_receives_saved_assignment() {
    let fixture = Fixture::new(false, false);
    fixture.import_released();
    let before = WorkModelStore::new(&fixture.project)
        .read_work_item_snapshot(WORK)
        .unwrap();
    assert!(before.planning_context.is_none());
    assert_eq!(
        before.attempts[0].tasks[0].instructions.as_deref(),
        Some(ASSIGNMENT)
    );
    let output = fixture.run(None);
    let prompt = fs::read_to_string(fixture.root.path().join("prompt"))
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)));
    assert!(prompt.contains(ASSIGNMENT));
    for absent in [
        "Read Brief at",
        "Read Behaviors at",
        "Read Approach at",
        "Read Plan at",
    ] {
        assert!(
            !prompt.contains(absent),
            "instruction-only prompt requires absent input: {absent}"
        );
    }
    assert!(
        !fixture
            .project
            .join(".fluent/work/items/input-upgrade")
            .exists()
    );

    // Current correction inputs remain visible on the same instruction-only
    // contract. They must not be dropped when no planning documents exist.
    let correction = Fixture::new(false, false);
    let output = correction.run(None);
    let prompt = fs::read_to_string(correction.root.path().join("prompt"))
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)));
    assert_eq!(
        fs::read_to_string(correction.root.path().join("prompt.assignment")).unwrap(),
        ASSIGNMENT
    );
    assert!(prompt.contains("prior/review.md"));
    for absent in [
        "- Brief: \n",
        "- Behaviors: \n",
        "- Approach: \n",
        "- Plan: \n",
    ] {
        assert!(!prompt.contains(absent));
    }
    let context = correction.project.join(format!(
        ".fluent/work/artifacts/{WORK}/attempt-1/{}/execution-context.json",
        correction.task
    ));
    assert!(
        fs::read_to_string(context)
            .unwrap()
            .contains("current finding marker")
    );
    let reviewer = Fixture::new(true, false);
    let output = reviewer.run(None);
    let prompt = fs::read_to_string(reviewer.root.path().join("prompt"))
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)));
    assert_eq!(
        fs::read_to_string(reviewer.root.path().join("prompt.assignment")).unwrap(),
        ASSIGNMENT
    );
    for absent in [
        "Read Brief at  —",
        "Read Behaviors at  —",
        "Read Approach at  —",
        "Read Plan at  —",
    ] {
        assert!(
            !prompt.contains(absent),
            "Reviewer requires absent input: {absent}"
        );
    }
}

#[test]
fn existing_060_inputs_and_bootstrap_survive_upgrade() {
    for configured in [false, true] {
        let fixture = Fixture::new(false, false);
        fixture.import_released();
        let config = fixture.project.join(".fluent/config.yaml");
        let config_bytes = "# Retain the user's existing configuration\ncoders:\n  writer:\n    coder: claude\n    model: saved-model\n";
        fs::write(&config, config_bytes).unwrap();
        let user_config = fixture.root.path().join("home/.config/fluent/config.yaml");
        fs::create_dir_all(user_config.parent().unwrap()).unwrap();
        fs::write(&user_config, "# Existing user preferences\n").unwrap();
        let tester = fixture.project.join(".fluent/tester.yaml");
        let extractor = fixture.project.join(".fluent/extract-tester-results");
        let tester_bytes =
            "commands:\n  - command: printf 'ok fixture\\n'\n    test_harness: shell-harness\n";
        if configured {
            fs::write(&tester, tester_bytes).unwrap();
            executable(
                &extractor,
                "#!/bin/sh\ncat >/dev/null\nprintf '{\"tests\":[{\"id\":\"fixture\",\"status\":\"pass\"}]}'\n",
            );
        }
        git::run(
            &fixture.project,
            &["add", ".fluent"],
            "stage saved configuration",
        )
        .unwrap();
        git::run(
            &fixture.project,
            &["commit", "-m", "Preserve configuration"],
            "commit saved configuration",
        )
        .unwrap();
        let head =
            git::run_stdout(&fixture.project, &["rev-parse", "HEAD"], "source history").unwrap();
        let store = WorkModelStore::new(&fixture.project);
        let before = store.read_work_item_snapshot(WORK).unwrap();
        let record_paths = [
            store.work_item_path(WORK).unwrap(),
            store.work_attempt_path(WORK, "attempt-1").unwrap(),
            store
                .work_task_path(WORK, "attempt-1", &fixture.task)
                .unwrap(),
        ];
        let bytes: Vec<_> = record_paths
            .iter()
            .map(|path| fs::read(path).unwrap())
            .collect();
        let inspection = fixture
            .command()
            .args(["work-item", "show", WORK])
            .output()
            .unwrap();
        assert!(
            inspection.status.success(),
            "{}",
            String::from_utf8_lossy(&inspection.stderr)
        );
        assert_eq!(
            record_paths
                .iter()
                .map(|path| fs::read(path).unwrap())
                .collect::<Vec<_>>(),
            bytes,
            "inspection must not migrate released state"
        );
        let output = fixture.run(None);
        let prompt = fs::read_to_string(fixture.root.path().join("prompt"))
            .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)));
        assert!(prompt.contains(ASSIGNMENT));
        assert_eq!(
            prompt.contains("`.fluent/tester.yaml` is missing"),
            !configured
        );
        assert_eq!(
            prompt.contains("`.fluent/extract-tester-results` is missing"),
            !configured
        );
        assert_eq!(fs::read_to_string(&config).unwrap(), config_bytes);
        assert_eq!(
            fs::read_to_string(user_config).unwrap(),
            "# Existing user preferences\n"
        );
        if configured {
            assert_eq!(fs::read_to_string(tester).unwrap(), tester_bytes);
        } else {
            assert!(!tester.exists());
            assert!(!extractor.exists());
        }
        let after = store.read_work_item_snapshot(WORK).unwrap();
        assert_eq!(after.instructions, before.instructions);
        assert!(after.transitions.starts_with(&before.transitions));
        assert_eq!(
            after.attempts[0].coder_mapping,
            before.attempts[0].coder_mapping
        );
        assert_eq!(
            git::run_stdout(&fixture.project, &["rev-parse", "HEAD"], "source history").unwrap(),
            head
        );
    }
}

#[test]
fn declared_worker_inputs_are_readable_before_provider_launch() {
    for review in [false, true] {
        let fixture = Fixture::new(review, true);
        let output = fixture.run(None);
        assert!(
            fixture.root.path().join("prompt").is_file(),
            "provider not reached: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let reads = fs::read_to_string(fixture.root.path().join("reads"))
            .expect("probe must open each input before provider launch");
        if real_profile_available(&fixture) {
            for path in reads.lines().collect::<std::collections::BTreeSet<_>>() {
                if !Path::new(path).is_file() {
                    continue;
                }
                let output = Command::new("/usr/bin/sandbox-exec")
                    .arg("-f")
                    .arg(fixture.root.path().join("profile.sb"))
                    .arg("/bin/cat")
                    .arg(path)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "production profile cannot open {path}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        let mut required = vec![
            ".fluent/work/items/input-upgrade/brief.md".into(),
            ".fluent/work/items/input-upgrade/behaviors.md".into(),
            ".fluent/work/items/input-upgrade/approach.md".into(),
            ".fluent/work/items/input-upgrade/plan.md".into(),
            ".fluent/work/expertise/INDEX.md".into(),
            ".fluent/work/expertise/tests.md".into(),
            ".fluent/work/artifacts/input-upgrade/attempt-1/prior/review.md".into(),
            ".fluent/work/progress/input-upgrade/attempt-1/progress.md".into(),
            format!(
                ".fluent/work/artifacts/{WORK}/attempt-1/{}/execution-context.json",
                fixture.task
            ),
        ];
        required.push(format!(
            ".fluent/work/artifacts/{WORK}/attempt-1/{}/task-assignment.md",
            fixture.task
        ));
        if !review {
            required.extend(
                ["writer-completion.json", "task-assignment.md"].map(|name| {
                    format!(
                        ".fluent/work/artifacts/{WORK}/attempt-1/{}/{name}",
                        fixture.task
                    )
                }),
            );
        }
        for relative in required {
            let path = fixture.project.join(&relative);
            assert!(
                reads.lines().any(|line| line == path.to_str().unwrap()),
                "missing direct open of {} in {reads}",
                path.display()
            );
            let denied = Fixture::new(review, true);
            let path = denied.project.join(&relative);
            let output = denied.run(Some(&path));
            assert!(
                !denied.root.path().join("prompt").exists(),
                "provider launched despite unreadable {}",
                path.display()
            );
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(path.to_str().unwrap()),
                "missing exact path: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let item = WorkModelStore::new(&denied.project)
                .read_work_item_snapshot(WORK)
                .unwrap();
            let task = item.attempts[0]
                .tasks
                .iter()
                .find(|t| t.id == denied.task)
                .unwrap();
            assert_ne!(task.status, TaskStatus::Executing);
        }
    }
}
