#![cfg(feature = "test-support")]

use fluent::git;
use fluent::work_model::{ArtifactRef, WorkModelStore, WriterOutcome, WriterRun, WriterRunKind};
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "lib/released_060.rs"]
mod released_060;

const SESSION: &str = "11111111-1111-4111-8111-111111111111";

#[test]
fn public_help_explains_missing_claude_session_recovery() {
    let output = Command::new(assert_cmd::cargo::cargo_bin!("fluent"))
        .args(["attempt", "run", "--help"])
        .env("FLUENT_NO_UPDATE_CHECK", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("missing Claude conversation"), "{help}");
    assert!(help.contains("candidate and retained evidence"), "{help}");
}

struct Fixture {
    root: tempfile::TempDir,
    project: PathBuf,
    work: String,
    binary: PathBuf,
}

impl Fixture {
    fn legacy_host() -> Self {
        Self::with_brief(
            true,
            "Acceptance requires at least two lines 'Claude legacy upgrade check' in README.md. Run sh check-acceptance.sh and commit README.md. The host runs the initial Writer as a single Task, then tags its settled commit claude-legacy-initial before continuing the Attempt. Once that tag exists, acceptance also requires the exact line 'Recovered initial candidate: <full tagged commit hash>' in README.md. The corrective Writer must add that line and any missing acceptance lines, preserving the original commit. Change only README.md; preserve the acceptance check, Tester configuration, and host-owned tag. The host stops after observing the recovered Writer; downstream acceptance remains separate.",
            Some(
                "# Verify the legacy Claude upgrade\n\nAcceptance requires at least two complete lines 'Claude legacy upgrade check' in README.md. Run `sh check-acceptance.sh` to verify this requirement.\n\nThe host runs the initial Writer as a single Task, then creates the host-owned Git tag `claude-legacy-initial` at its settled candidate before continuing the Attempt. Once the tag exists, README.md must also contain the exact line `Recovered initial candidate: <full tagged commit hash>`. Read the hash with `git rev-parse refs/tags/claude-legacy-initial`. This requirement creates an ordinary review correction even when the initial Writer completed both acceptance lines: a commit cannot already contain its own hash. Reviewers must report the missing acknowledgment as a blocking finding and must not pass until the acceptance check passes. The corrective Writer must add the acknowledgment and any missing acceptance lines while preserving the original commit. Change only README.md; keep the host-owned tag, acceptance check, and Tester configuration unchanged.\n",
            ),
        )
    }

    fn stranded() -> Self {
        Self::with_release(true).strand()
    }

    fn strand(mut self) -> Self {
        let fixture = &mut self;
        let output = fixture
            .command()
            .args(["attempt", "run", &fixture.work, "attempt-1", "--no-sandbox"])
            .output()
            .unwrap();
        let item = WorkModelStore::new(&fixture.project)
            .read_work_item_snapshot(&fixture.work)
            .unwrap();
        assert_eq!(
            item.attempts[0].tasks.last().unwrap().status,
            fluent::work_model::TaskStatus::Failed,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        fixture.binary = assert_cmd::cargo::cargo_bin!("fluent").into();
        self
    }

    fn resume(&self) -> Command {
        let mut command = self.command();
        command.env("SESSION_PROBE_STOP_AFTER_RECOVERY", "1").args([
            "attempt",
            "run",
            &self.work,
            "attempt-1",
            "--no-sandbox",
        ]);
        command
    }

    fn new() -> Self {
        Self::with_release(false)
    }

    fn with_release(released: bool) -> Self {
        Self::with_instructions(
            released,
            "Preserve the first candidate and apply the current correction.",
        )
    }

    fn with_instructions(released: bool, instructions: &str) -> Self {
        Self::with_brief(released, instructions, None)
    }

    fn with_brief(released: bool, instructions: &str, brief: Option<&str>) -> Self {
        Self::with_planning(released, instructions, brief, None)
    }

    fn with_pending_completion() -> Self {
        Self::with_planning(
            true,
            "Preserve the first candidate and apply the current correction.",
            None,
            Some(
                "| # | State reached | Verification | Responsibility | Req? |\n|---|---|---|---|---|\n| 1 | Apply the correction | `git diff --check` | writer | Yes |\n",
            ),
        )
    }

    fn with_planning(
        released: bool,
        instructions: &str,
        brief: Option<&str>,
        plan: Option<&str>,
    ) -> Self {
        let root = tempfile::tempdir().unwrap();
        let binary = if released {
            released_060::copy_to(root.path()).unwrap()
        } else {
            assert_cmd::cargo::cargo_bin!("fluent").into()
        };
        let project = root.path().join("main");
        fs::create_dir_all(project.join(".fluent/expertise")).unwrap();
        fs::create_dir_all(root.path().join("bin")).unwrap();
        fs::create_dir_all(root.path().join("home")).unwrap();
        fs::write(
            project.join(".gitignore"),
            ".fluent/*\n!.fluent/expertise/\n",
        )
        .unwrap();
        fs::write(project.join("README.md"), "First candidate\n").unwrap();
        fs::write(project.join(".fluent/expertise/INDEX.md"), "# Expertise\n").unwrap();
        if let Some(brief) = brief {
            // Commit a complete checker in the baseline. Ordinary reviews read
            // the Work Item brief; rich Writer instructions alone do not reach them.
            fs::write(root.path().join("brief.md"), brief).unwrap();
            fs::write(
                project.join(".gitignore"),
                ".fluent/*\n!.fluent/expertise/\n!.fluent/tester.yaml\n!.fluent/extract-tester-results\n",
            )
            .unwrap();
            fs::write(
                project.join(".fluent/tester.yaml"),
                "commands:\n  - command: sh check-acceptance.sh\n    test_harness: shell-harness\n",
            )
            .unwrap();
            let extractor = project.join(".fluent/extract-tester-results");
            fs::write(
                &extractor,
                include_str!("../.fluent/extract-tester-results"),
            )
            .unwrap();
            fs::set_permissions(&extractor, fs::Permissions::from_mode(0o755)).unwrap();
            fs::write(
                project.join("check-acceptance.sh"),
                r#"#!/bin/sh
set -eu
if ! awk '/^Claude legacy upgrade check$/ { count++ } END { exit (count < 2) }' README.md; then
  echo 'not ok legacy-acceptance - README.md needs at least two acceptance lines'
  exit 1
fi
if initial=$(git rev-parse --verify refs/tags/claude-legacy-initial 2>/dev/null); then
  if ! grep -Fxq "Recovered initial candidate: $initial" README.md; then
    echo "not ok legacy-acceptance - README.md must contain: Recovered initial candidate: $initial"
    exit 1
  fi
fi
echo 'ok legacy-acceptance'
"#,
            )
            .unwrap();
        }
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Session fixture"],
            vec!["config", "user.email", "session@example.test"],
            vec!["config", "commit.gpgsign", "false"],
            vec!["add", "."],
            vec!["commit", "-m", "Add initial candidate"],
        ] {
            git::run(&project, &args, "prepare fixture").unwrap();
        }
        let work = format!(
            "claude-{}",
            root.path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .replace('.', "")
        );
        let fixture = Self {
            root,
            project: project.canonicalize().unwrap(),
            work,
            binary,
        };
        fixture.provider();
        let mut create = vec![
            "work-item",
            "create",
            &fixture.work,
            "--title",
            "Continue Claude",
            "--instructions",
            instructions,
        ];
        let brief_path = fixture.root.path().join("brief.md");
        if brief.is_some() {
            create.extend(["--brief-file", brief_path.to_str().unwrap()]);
        }
        let plan_path = fixture.root.path().join("plan.md");
        if let Some(plan) = plan {
            fs::write(&plan_path, plan).unwrap();
            create.extend(["--plan-file", plan_path.to_str().unwrap()]);
        }
        fixture.ok(&create);
        fixture.ok(&[
            "attempt",
            "create",
            &fixture.work,
            "attempt-1",
            "--write-coder",
            "claude",
            "--review-coder",
            "claude",
        ]);
        fixture
    }

    fn command(&self) -> Command {
        self.command_for(&self.binary)
    }

    fn command_for(&self, binary: &Path) -> Command {
        let mut command = Command::new(binary);
        command
            .current_dir(&self.project)
            .env("HOME", self.root.path().join("home"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("XDG_DATA_HOME", self.root.path().join("data"))
            .env(
                "CLAUDE_CONFIG_DIR",
                self.root.path().join("unrelated-claude-config"),
            )
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.root.path().join("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("FLUENT_TEST_HERMETIC_PROVIDERS", "1")
            .env("FLUENT_TEST_HOST_SANDBOX_PREFLIGHT", "pass")
            .env("FLUENT_NO_UPDATE_CHECK", "1")
            .env("FLUENT_RATE_LIMIT_RETRY_AFTER_SECS", "0")
            .env("FLUENT_RATE_LIMIT_JITTER_MAX_SECONDS", "0")
            .env("SESSION_PROBE_LOG", self.root.path().join("launches.jsonl"))
            .env_remove("FLUENT_TASK_KIND");
        if binary
            .file_name()
            .is_some_and(|name| name == "fluent-v0.6.0")
        {
            command.env("SESSION_PROBE_LEGACY", "1");
        }
        for name in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "OPENAI_API_KEY",
            "PI_API_KEY",
        ] {
            command.env_remove(name);
        }
        command
    }

    fn ok(&self, args: &[&str]) -> Output {
        let output = self.command().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn run_writer(&self, task: &str) {
        self.ok(&["task", "run", &self.work, "attempt-1", task, "--no-sandbox"]);
    }

    fn run_mode(&self, task: &str, mode: &str) -> Output {
        self.command()
            .env("SESSION_PROBE_MODE", mode)
            .args(["task", "run", &self.work, "attempt-1", task, "--no-sandbox"])
            .output()
            .unwrap()
    }

    fn area(&self, task: &str) -> PathBuf {
        let item = WorkModelStore::new(&self.project)
            .read_work_item_snapshot(&self.work)
            .unwrap();
        self.project.join(
            &item.attempts[0]
                .tasks
                .iter()
                .find(|t| t.id == task)
                .unwrap()
                .artifact_area
                .as_ref()
                .unwrap()
                .path,
        )
    }

    fn provider(&self) {
        let script = r##"#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys
args = sys.argv[1:]
if args == ['auth', 'status', '--json']:
    print('{"loggedIn":true}')
    sys.exit(0)
if args == ['--version']:
    print('2.1.310 (Claude Code)')
    sys.exit(0)
if '-p' not in args:
    sys.exit('unexpected provider invocation')
resume = args[args.index('--resume') + 1] if '--resume' in args else None
captured = pathlib.Path(os.environ['SESSION_PROBE_RESULT']).read_text() if os.environ.get('SESSION_PROBE_RESULT') else None
home = pathlib.Path(os.environ['HOME'])
config = pathlib.Path(os.environ.get('CLAUDE_CONFIG_DIR', str(home / '.claude')))
prompt = args[args.index('-p') + 1]
# The integrated handoff keeps corrective instructions in a declared immutable
# file. Resolve it as a provider would, retaining the actual prompt separately.
assignment = prompt
for line in prompt.splitlines():
    if 'task-assignment.md' in line:
        import re
        for path in re.findall(r'/[^\s`]+task-assignment\.md', line):
            assignment += '\n' + pathlib.Path(path).read_text()
if prompt.startswith('Review changes'):
    verdict = 'uncertain' if os.environ.get('SESSION_PROBE_STOP_AFTER_RECOVERY') else 'fail'
    finding = 'The committed candidate needs the assigned correction.'
    workspace = pathlib.Path(next(line.removeprefix('- Workspace: ') for line in prompt.splitlines() if line.startswith('- Workspace: ')))
    if (workspace / 'check-acceptance.sh').exists():
        brief_path = next(line.split('Read Brief at ', 1)[1].split(' —', 1)[0] for line in prompt.splitlines() if 'Read Brief at ' in line)
        brief = pathlib.Path(brief_path).read_text()
        assert 'at least two' in brief and 'sh check-acceptance.sh' in brief
        checked = subprocess.run(['sh', 'check-acceptance.sh'], cwd=workspace, capture_output=True)
        verdict = 'fail' if checked.returncode else ('uncertain' if os.environ.get('SESSION_PROBE_STOP_AFTER_RECOVERY') else 'pass')
        finding = 'sh check-acceptance.sh fails: ' + checked.stdout.decode().strip() if checked.returncode else ''
    pathlib.Path('review.md').write_text('Verdict: ' + verdict + '\n\n## Findings\n\n' + ('### Apply the current correction\n\n' + finding + '\n' if finding else ''))
    print(json.dumps(dict(type='result', subtype='success', is_error=False, num_turns=1)))
    sys.exit(0)
with open(os.environ['SESSION_PROBE_LOG'], 'a') as log:
    log.write(json.dumps(dict(home=str(home), config=str(config), resume=resume, prompt=prompt, assignment=assignment, args=args)) + '\n')
legacy = os.environ.get('SESSION_PROBE_LEGACY') == '1'
assert legacy or config == home, 'Writer must bind CLAUDE_CONFIG_DIR to its private home'
mode = 'missing' if legacy and resume else os.environ.get('SESSION_PROBE_MODE', '')
if resume and mode in ('progress-then-missing', 'tool-then-missing'):
    resumed = [json.loads(line) for line in pathlib.Path(os.environ['SESSION_PROBE_LOG']).read_text().splitlines() if json.loads(line)['resume']]
    mode = mode.split('-')[0] if len(resumed) == 1 else 'missing'
if resume and mode and captured:
    assert resume == json.loads(captured)['session_id']
    sys.stdout.write(captured)
    print('No conversation found with session ID: ' + resume, file=sys.stderr)
    sys.exit(1)
if resume and mode:
    error = 'No conversation found with session ID: ' + resume
    event = dict(type='result', subtype='error_during_execution', is_error=True, num_turns=0, session_id=resume, errors=[error])
    if mode == 'progress':
        print(json.dumps(dict(type='assistant', message={'content':[{'type':'text','text':'model progress'}]})))
    if mode == 'tool':
        print(json.dumps(dict(type='assistant', message={'content':[{'type':'tool_use','name':'Bash','input':{'command':'true'}}]})))
    if mode == 'identity':
        event['session_id'] = '22222222-2222-4222-8222-222222222222'
    if mode == 'turn':
        event['num_turns'] = 1
    if mode == 'tokens':
        event['usage'] = {'output_tokens': 1}
    if mode == 'provider':
        event['errors'].append('Invalid API key')
    if mode == 'malformed':
        print('incomplete provider output')
    print(json.dumps(event))
    print(error, file=sys.stderr)
    sys.exit(1)
if mode == 'fresh-failure':
    print('Fresh conversation failed', file=sys.stderr)
    sys.exit(1)
if mode == 'fresh-rate-limit':
    print(json.dumps(dict(type='result', subtype='error_during_execution', is_error=True, num_turns=0, errors=['rate limit'])))
    sys.exit(1)
session = resume or ('22222222-2222-4222-8222-222222222222' if mode else '11111111-1111-4111-8111-111111111111')
if captured and not resume:
    session = json.loads(captured)['session_id']
directory = config / 'projects' / ''.join(c if c.isalnum() else '-' for c in os.getcwd())
saved = directory / (session + '.jsonl')
if resume:
    assert saved.is_file(), 'resume target was not persisted'
if not legacy:
    directory.mkdir(parents=True, exist_ok=True)
    with saved.open('a') as stream:
        stream.write(json.dumps(dict(type='user', sessionId=session, message={'role':'user', 'content':prompt})) + '\n')
with open('README.md', 'a') as output:
    output.write('Claude legacy upgrade check\n' if pathlib.Path('check-acceptance.sh').exists() else 'Correction applied\n')
    if legacy and os.environ.get('SESSION_PROBE_EARLY_COMPLETION'):
        output.write('Claude legacy upgrade check\n')
    if not legacy and pathlib.Path('check-acceptance.sh').exists():
        initial = subprocess.check_output(['git', 'rev-parse', 'refs/tags/claude-legacy-initial'], text=True).strip()
        output.write('Recovered initial candidate: ' + initial + '\n')
subprocess.run(['git', 'add', 'README.md'], check=True)
subprocess.run(['git', 'commit', '-m', 'Apply current correction', '-m', '- Preserve the existing candidate and apply the assigned correction'], check=True, stdout=subprocess.DEVNULL)
# Leave the initial matrix pending so the released coordinator records Continue.
# The fresh corrective Writer completes that same requirement through its prompt.
if not legacy:
    for line in prompt.splitlines():
        if line.startswith('- Writer completion matrix: '):
            matrix_path = pathlib.Path(line.removeprefix('- Writer completion matrix: '))
            matrix = json.loads(matrix_path.read_text())
            subprocess.run(['git', 'diff', '--check'], check=True)
            for row in matrix['rows']:
                if row.get('responsibility', 'writer') == 'writer':
                    row['state'] = 'complete'
                    row['implementation-evidence'] = ['README.md: applied the correction']
                    row['verification'] = [{'command':'git diff --check', 'result':'pass'}]
            matrix_path.write_text(json.dumps(matrix))
print(json.dumps(dict(type='system', subtype='init', session_id=session)))
print(json.dumps(dict(type='result', subtype='success', is_error=False, session_id=session, num_turns=1)))
"##;
        let path = self.root.path().join("bin/claude");
        fs::write(&path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn correction(&self) -> (String, String, PathBuf) {
        let store = WorkModelStore::new(&self.project);
        let mut item = store.read_work_item_snapshot(&self.work).unwrap();
        let task = &item.attempts[0].tasks[0];
        let candidate = task.output.as_ref().unwrap().commit.clone();
        let area = self
            .project
            .join(&task.artifact_area.as_ref().unwrap().path);
        let transcript = fs::read_to_string(area.join("transcript.jsonl")).unwrap();
        let session = transcript
            .lines()
            .find_map(fluent::transcript::conversation_id_from_line)
            .unwrap();
        item.attempts[0].writer_runs.push(WriterRun {
            task_id: "attempt-1-write-1".into(),
            outcome: WriterOutcome::Complete,
            kind: WriterRunKind::Initial,
            provider: "claude".into(),
            session_id: Some(session),
            continuation: 0,
            checked_required: 0,
            completed_matrix: 0,
            candidate_commit: candidate.clone(),
        });
        let review = area.join("review.md");
        fs::write(
            &review,
            "Verdict: fail\n\n## Findings\n\nApply the current correction.\n",
        )
        .unwrap();
        let next = item
            .add_next_write_round(
                "attempt-1",
                vec![ArtifactRef {
                    producer_id: "attempt-1-write-1".into(),
                    path: review
                        .strip_prefix(&self.project)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                }],
            )
            .unwrap();
        store.write_work_item(&item).unwrap();
        (next, candidate, review)
    }

    fn launches(&self) -> Vec<Value> {
        fs::read_to_string(self.root.path().join("launches.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

#[test]
fn claude_writer_persists_and_resumes_in_private_home() {
    let fixture = Fixture::new();
    fixture.run_writer("attempt-1-write-1");
    let (task, _, _) = fixture.correction();
    fixture.run_writer(&task);
    let launches = fixture.launches();
    assert_eq!(launches.len(), 2);
    assert_eq!(launches[0]["config"], launches[1]["config"]);
    assert_eq!(launches[1]["resume"], SESSION);
    assert!(
        Path::new(launches[0]["home"].as_str().unwrap()).starts_with(
            fixture
                .project
                .join(".fluent/work/artifacts")
                .join(&fixture.work)
                .join("attempt-1")
        )
    );

    // Native construction must use the same effective directory and persistence.
    let home = Path::new(launches[0]["home"].as_str().unwrap());
    let settings = home.join("owned-settings.json");
    fs::write(&settings, "{}").unwrap();
    let native = fluent::coder::SandboxedClaudeCode {
        sandbox_profile: Some("assigned.sb".into()),
        trusted_sandbox: true,
        model_override: Some("sonnet".into()),
        effort: Some("high".into()),
    }
    .native_task_command(
        &fixture.project,
        home,
        &settings,
        fluent::coder::CoderSession::Resume(SESSION),
        "system",
        "current assignment",
    )
    .unwrap();
    let env: std::collections::HashMap<_, _> = native.get_envs().collect();
    assert_eq!(
        env[std::ffi::OsStr::new("CLAUDE_CONFIG_DIR")],
        Some(home.as_os_str())
    );
    assert!(
        !native
            .get_args()
            .any(|arg| arg == "--no-session-persistence")
    );
}

#[test]
fn available_claude_session_resumes_with_current_assignment() {
    let fixture = Fixture::new();
    fixture.run_writer("attempt-1-write-1");
    let (task, candidate, review) = fixture.correction();
    fixture.run_writer(&task);
    let launches = fixture.launches();
    assert_eq!(launches[1]["resume"], SESSION);
    let prompt = launches[1]["assignment"].as_str().unwrap();
    assert!(prompt.contains("Preserve the first candidate and apply the current correction."));
    assert!(prompt.contains(review.to_str().unwrap()));
    let item = WorkModelStore::new(&fixture.project)
        .read_work_item_snapshot(&fixture.work)
        .unwrap();
    let area = fixture.project.join(
        &item.attempts[0]
            .tasks
            .last()
            .unwrap()
            .artifact_area
            .as_ref()
            .unwrap()
            .path,
    );
    let context: Value =
        serde_json::from_slice(&fs::read(area.join("execution-context.json")).unwrap()).unwrap();
    assert_eq!(context["candidate-commit"], json!(candidate));
}

#[test]
fn available_legacy_claude_home_keeps_its_conversation() {
    let fixture = Fixture::new();
    fixture.run_writer("attempt-1-write-1");
    let (task, _, _) = fixture.correction();
    let home = PathBuf::from(fixture.launches()[0]["home"].as_str().unwrap());
    let legacy = home.join(".claude");
    fs::create_dir(&legacy).unwrap();
    fs::rename(home.join("projects"), legacy.join("projects")).unwrap();
    fixture.run_writer(&task);
    let launches = fixture.launches();
    assert_eq!(launches[1]["resume"], SESSION);
    assert_eq!(launches[1]["config"], legacy.to_string_lossy().as_ref());
}

#[test]
fn missing_claude_session_falls_back_once_without_losing_work() {
    for local_missing in [true, false] {
        let fixture = Fixture::new();
        fixture.run_writer("attempt-1-write-1");
        let (task, candidate, review) = fixture.correction();
        let first_transcript =
            fs::read(fixture.area("attempt-1-write-1").join("transcript.jsonl")).unwrap();
        let findings = fs::read(&review).unwrap();
        if local_missing {
            let home = PathBuf::from(fixture.launches()[0]["home"].as_str().unwrap());
            fs::remove_dir_all(home.join("projects")).unwrap();
        }
        let output = fixture.run_mode(&task, "missing");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let launches = fixture.launches();
        assert_eq!(launches.len(), if local_missing { 2 } else { 3 });
        assert!(launches.last().unwrap()["resume"].is_null());
        assert!(
            launches.last().unwrap()["assignment"]
                .as_str()
                .unwrap()
                .contains("Preserve the first candidate")
        );
        assert_eq!(fs::read(review).unwrap(), findings);
        assert_eq!(
            fs::read(fixture.area("attempt-1-write-1").join("transcript.jsonl")).unwrap(),
            first_transcript
        );
        let item = WorkModelStore::new(&fixture.project)
            .read_work_item_snapshot(&fixture.work)
            .unwrap();
        let output = item.attempts[0]
            .tasks
            .last()
            .unwrap()
            .output
            .as_ref()
            .unwrap();
        git::run(
            &fixture.project.join(&output.workspace_path),
            &["merge-base", "--is-ancestor", &candidate, &output.commit],
            "retain first candidate",
        )
        .unwrap();
        let area = fixture.area(&task);
        if !local_missing {
            assert!(
                fs::read_to_string(area.join("transcript.0.jsonl"))
                    .unwrap()
                    .contains("No conversation found")
            );
            assert!(
                fs::read_to_string(area.join("provider-stderr.0.log"))
                    .unwrap()
                    .contains(SESSION)
            );
        }
        assert!(
            fs::read_to_string(area.join("transcript.jsonl"))
                .unwrap()
                .contains("22222222-2222-4222-8222-222222222222")
        );
    }

    let fixture = Fixture::new();
    fixture.run_writer("attempt-1-write-1");
    let (task, _, _) = fixture.correction();
    assert!(!fixture.run_mode(&task, "fresh-failure").status.success());
    assert_eq!(
        fixture.launches().len(),
        3,
        "a failed fresh launch must not restart through generic retries"
    );
    assert!(!fixture.run_mode(&task, "fresh-failure").status.success());
    assert_eq!(
        fixture.launches().len(),
        3,
        "repeating a failed Task must not spend another fallback"
    );

    let limited = Fixture::new();
    limited.run_writer("attempt-1-write-1");
    let (task, _, _) = limited.correction();
    assert!(!limited.run_mode(&task, "fresh-rate-limit").status.success());
    assert_eq!(
        limited.launches().len(),
        3,
        "provider retries must not start more fresh conversations"
    );
}

#[test]
fn ambiguous_or_progressing_failure_does_not_reset_session() {
    for mode in [
        "progress",
        "tool",
        "identity",
        "turn",
        "tokens",
        "provider",
        "malformed",
        "progress-then-missing",
        "tool-then-missing",
    ] {
        let fixture = Fixture::new();
        fixture.run_writer("attempt-1-write-1");
        let (task, _, _) = fixture.correction();
        assert!(!fixture.run_mode(&task, mode).status.success(), "{mode}");
        let launches = fixture.launches();
        assert!(
            launches[1..]
                .iter()
                .all(|launch| launch["resume"] == SESSION),
            "{mode}: {launches:?}"
        );
        if mode.ends_with("-then-missing") {
            assert!(
                launches.len() >= 3,
                "exercise the outer retry: {launches:?}"
            );
            let area = fixture.area(&task);
            let first = fs::read_to_string(area.join("transcript.0.jsonl")).unwrap();
            assert!(first.contains(if mode.starts_with("tool") {
                "tool_use"
            } else {
                "model progress"
            }));
            assert!(
                fs::read_to_string(area.join("provider-stderr.0.log"))
                    .unwrap()
                    .contains(SESSION)
            );
            assert!(area.join("coder-supervision.0.json").is_file());
            assert!(
                fs::read_to_string(area.join("transcript.jsonl"))
                    .unwrap()
                    .contains("No conversation found")
            );
        }
    }
}

#[test]
fn released_060_missing_session_recovers_through_public_resume() {
    assert_public_missing_session_recovery(Fixture::with_release(true), WriterOutcome::Complete);
}

#[test]
fn released_060_committed_continue_recovers_through_public_resume() {
    assert_public_missing_session_recovery(
        Fixture::with_pending_completion(),
        WriterOutcome::Continue,
    );
}

fn assert_public_missing_session_recovery(mut fixture: Fixture, expected: WriterOutcome) {
    let released = fixture
        .command()
        .args(["attempt", "run", &fixture.work, "attempt-1", "--no-sandbox"])
        .output()
        .unwrap();
    let store = WorkModelStore::new(&fixture.project);
    let before = store.read_work_item_snapshot(&fixture.work).unwrap();
    let attempt = &before.attempts[0];
    assert_eq!(attempt.writer_runs.len(), 1);
    let run = &attempt.writer_runs[0];
    assert_eq!(run.outcome, expected);
    assert_eq!(run.task_id, attempt.tasks[0].id);
    assert_eq!(
        attempt.tasks[0].status,
        fluent::work_model::TaskStatus::Complete
    );
    assert_eq!(
        run.candidate_commit,
        attempt.tasks[0].output.as_ref().unwrap().commit
    );
    assert_eq!(
        attempt.pause_kind,
        Some(fluent::work_model::PauseKind::RoundCap),
        "{}\n{}\n{attempt:#?}",
        String::from_utf8_lossy(&released.stdout),
        String::from_utf8_lossy(&released.stderr)
    );
    let failed = attempt
        .tasks
        .iter()
        .rev()
        .find(|t| t.kind == fluent::work_model::TaskKind::Write)
        .unwrap();
    assert_eq!(
        failed.status,
        fluent::work_model::TaskStatus::Failed,
        "{attempt:#?}"
    );
    let failed_id = failed.id.clone();
    let original_area = fixture
        .project
        .join(&failed.artifact_area.as_ref().unwrap().path);
    let transcript = fs::read(original_area.join("transcript.jsonl")).unwrap();
    let stderr = fs::read(original_area.join("provider-stderr.log")).unwrap();
    assert!(String::from_utf8_lossy(&transcript).contains("No conversation found"));
    let candidate = attempt.current_candidate_commit().unwrap().to_owned();
    let original_evidence = snapshot(&original_area);
    let first_evidence = snapshot(&fixture.area(&attempt.tasks[0].id));
    let launches = fixture.launches().len();
    fixture.binary = assert_cmd::cargo::cargo_bin!("fluent").into();
    let resumed = fixture
        .command()
        .env("SESSION_PROBE_STOP_AFTER_RECOVERY", "1")
        .args(["attempt", "run", &fixture.work, "attempt-1", "--no-sandbox"])
        .output()
        .unwrap();
    assert!(
        resumed.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&resumed.stdout),
        String::from_utf8_lossy(&resumed.stderr)
    );
    let after = store.read_work_item_snapshot(&fixture.work).unwrap();
    let recovered = after.attempts[0]
        .tasks
        .iter()
        .find(|t| t.id == failed_id)
        .unwrap();
    assert_eq!(recovered.status, fluent::work_model::TaskStatus::Complete);
    assert_ne!(recovered.output.as_ref().unwrap().commit, candidate);
    assert_eq!(after.attempts[0].writer_runs[0], *run);
    assert_eq!(fixture.launches().len(), launches + 1);
    let fresh = fixture.launches().pop().unwrap();
    assert!(fresh["resume"].is_null());
    assert_eq!(fresh["config"], fresh["home"]);
    assert_eq!(fresh["home"], fixture.launches()[0]["home"]);
    assert_eq!(recovered.instructions, failed.instructions);
    assert_eq!(recovered.workspace_access, failed.workspace_access);
    assert_eq!(snapshot(&original_area), original_evidence);
    assert_eq!(
        snapshot(&fixture.area(&attempt.tasks[0].id)),
        first_evidence
    );
    let output = recovered.output.as_ref().unwrap();
    git::run(
        &fixture.project.join(&output.workspace_path),
        &["merge-base", "--is-ancestor", &candidate, &output.commit],
        "retain the original candidate",
    )
    .unwrap();
    // Check the package verifier against the actual public recovery audit and
    // assignment, including Rust's serialized operation identity.
    let assignment = fixture.root.path().join("recovered-assignment.json");
    fs::write(&assignment, serde_json::to_vec(&after.attempts[0]).unwrap()).unwrap();
    let verified = Command::new("python3")
        .args([
            "-I",
            "-S",
            "-c",
            r#"import importlib.util, json, sys
from pathlib import Path
spec = importlib.util.spec_from_file_location('upgrade', sys.argv[1])
upgrade = importlib.util.module_from_spec(spec)
spec.loader.exec_module(upgrade)
attempt = json.loads(Path(sys.argv[3]).read_text())
task = next(t for t in attempt['tasks'] if t['id'] == sys.argv[4])
upgrade.require_package_artifact_assignment(Path(sys.argv[2]), attempt, task)
"#,
        ])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integrated-upgrade.py"))
        .arg(&fixture.project)
        .arg(&assignment)
        .arg(&failed_id)
        .output()
        .unwrap();
    assert!(
        verified.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&verified.stdout),
        String::from_utf8_lossy(&verified.stderr)
    );
    assert_eq!(after.attempts[0].tasks[0], before.attempts[0].tasks[0]);
    assert_eq!(
        fs::read(original_area.join("transcript.jsonl")).unwrap(),
        transcript
    );
    assert_eq!(
        fs::read(original_area.join("provider-stderr.log")).unwrap(),
        stderr
    );
    assert!(
        after.merge_candidates.is_empty(),
        "ordinary reviews, Tester, and Learning remain required"
    );
    let _ = fixture.resume().output().unwrap();
    assert_eq!(
        fixture.launches().len(),
        launches + 1,
        "recovery must not relaunch the Writer"
    );
    let old = fixture.root.path().join("fluent-v0.6.0");
    let before_refusal = snapshot(&fixture.project.join(".fluent/work"));
    let refused = Command::new(old)
        .current_dir(&fixture.project)
        .env("FLUENT_NO_UPDATE_CHECK", "1")
        .args([
            "work-item",
            "abandon",
            &fixture.work,
            "--reason",
            "old writer must refuse",
        ])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("schema"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert_eq!(
        snapshot(&fixture.project.join(".fluent/work")),
        before_refusal
    );
}

#[test]
fn committed_continue_recovery_keeps_admission_guards() {
    let fixture = Fixture::with_pending_completion().strand();
    let store = WorkModelStore::new(&fixture.project);
    let original = store.read_work_item_snapshot(&fixture.work).unwrap();
    assert_eq!(
        original.attempts[0].writer_runs[0].outcome,
        WriterOutcome::Continue
    );
    let task = original.attempts[0].tasks.last().unwrap();
    let area = fixture.area(&task.id);
    let workspace = fixture.project.join(&task.workspace_access.writes[0].path);
    let baseline = store
        .work_task_path(&fixture.work, "attempt-1", &task.id)
        .unwrap()
        .with_extension("write-baseline");
    let launches = fixture.launches().len();

    // Alter only disposable negative controls; the positive uses the released
    // coordinator's committed Continue run without rewriting its outcome.
    for case in [
        "blocked",
        "source-task",
        "source-candidate",
        "session",
        "write-path",
        "write-id",
        "assignment",
        "dirty",
        "host-baseline",
        "artifact-baseline",
        "fallback",
        "live-owner",
        "progress",
    ] {
        let mut changed = store.read_work_item_snapshot(&fixture.work).unwrap();
        let attempt = &mut changed.attempts[0];
        let run = &mut attempt.writer_runs[0];
        let failed = attempt.tasks.last_mut().unwrap();
        let altered_file = match case {
            "blocked" => {
                run.outcome = WriterOutcome::Blocked;
                None
            }
            "source-task" => {
                run.task_id = failed.id.clone();
                None
            }
            "source-candidate" => {
                run.candidate_commit = "0".repeat(40);
                None
            }
            "session" => {
                run.session_id = Some("22222222-2222-4222-8222-222222222222".into());
                None
            }
            "write-path" => {
                failed.workspace_access.writes[0].path = ".".into();
                None
            }
            "write-id" => {
                failed.workspace_access.writes[0].id = "foreign".into();
                None
            }
            "assignment" => {
                failed.instructions = Some("Changed assignment".into());
                None
            }
            "dirty" => Some((workspace.join("uncommitted.txt"), b"dirty".to_vec())),
            "host-baseline" => Some((baseline.clone(), b"changed".to_vec())),
            "artifact-baseline" => Some((area.join("write-baseline-commit"), b"changed".to_vec())),
            "fallback" => Some((area.join("claude-session-fallback.json"), b"{}".to_vec())),
            "progress" => {
                let path = area.join("transcript.jsonl");
                let mut event: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                event["num_turns"] = json!(1);
                Some((path, serde_json::to_vec(&event).unwrap()))
            }
            "live-owner" => None,
            _ => unreachable!(),
        };
        store.write_work_item(&changed).unwrap();
        let prior_bytes = altered_file.as_ref().map(|(path, bytes)| {
            let prior = fs::read(path).ok();
            fs::write(path, bytes).unwrap();
            prior
        });
        let lease = (case == "live-owner").then(|| {
            fluent::lease::acquire(&fluent::lease::task_lock_path(
                &fixture.project,
                &fixture.work,
                &task.id,
            ))
            .unwrap()
        });
        let before = snapshot(&fixture.project.join(".fluent/work"));
        let rejected = fixture.resume().output().unwrap();
        assert!(!rejected.status.success(), "accepted {case}");
        if case == "blocked" {
            assert!(
                String::from_utf8_lossy(&rejected.stderr)
                    .contains("missing-session recovery candidate or workspace changed")
            );
        }
        assert_eq!(fixture.launches().len(), launches, "{case}");
        if case == "progress" {
            // A nonzero-turn failure follows normal pause assessment instead of
            // missing-session recovery. It may update pause bookkeeping, but
            // must preserve the failed Task and grant no recovery authority.
            let refused = store.read_work_item_snapshot(&fixture.work).unwrap();
            assert_eq!(refused.attempts[0].tasks, changed.attempts[0].tasks);
            assert_eq!(
                refused.attempts[0].current_candidate_commit(),
                original.attempts[0].current_candidate_commit()
            );
            assert!(refused.attempts[0].recovery_operations.is_empty());
        }
        let after = snapshot(&fixture.project.join(".fluent/work"));
        let changed_paths = before
            .keys()
            .chain(after.keys())
            .filter(|path| before.get(*path) != after.get(*path))
            .filter(|path| {
                case != "progress"
                    || ![
                        PathBuf::from(format!("items/{}.json", fixture.work)),
                        PathBuf::from(format!("attempts/{}/attempt-1.json", fixture.work)),
                    ]
                    .contains(path)
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert!(
            changed_paths.is_empty(),
            "{case}: changed {changed_paths:?}"
        );
        drop(lease);
        if let Some((path, _)) = altered_file {
            match prior_bytes.unwrap() {
                Some(bytes) => fs::write(path, bytes).unwrap(),
                None => fs::remove_file(path).unwrap(),
            }
        }
        let mut restored = store.read_work_item_snapshot(&fixture.work).unwrap();
        restored.attempts = original.attempts.clone();
        store.write_work_item(&restored).unwrap();
    }
}

#[test]
fn committed_continue_recovery_spends_fallback_once() {
    let fixture = Fixture::with_pending_completion().strand();
    let launches = fixture.launches().len();
    let first = fixture
        .resume()
        .env("SESSION_PROBE_MODE", "fresh-failure")
        .output()
        .unwrap();
    assert!(!first.status.success());
    assert_eq!(fixture.launches().len(), launches + 1);
    assert!(fixture.launches().last().unwrap()["resume"].is_null());
    let second = fixture
        .resume()
        .env("SESSION_PROBE_MODE", "fresh-failure")
        .output()
        .unwrap();
    assert!(!second.status.success());
    assert_eq!(fixture.launches().len(), launches + 1);
}

#[test]
fn real_claude_metadata_allows_bounded_legacy_recovery() {
    let mut fixture = Fixture::with_release(true);
    let captured = include_bytes!("fixtures/claude-2.1.307-missing-conversation.json");
    let payload = fixture.root.path().join("captured.json");
    fs::write(&payload, captured).unwrap();
    let released = fixture
        .command()
        .env("SESSION_PROBE_RESULT", &payload)
        .args(["attempt", "run", &fixture.work, "attempt-1", "--no-sandbox"])
        .output()
        .unwrap();
    let store = WorkModelStore::new(&fixture.project);
    let before = store.read_work_item_snapshot(&fixture.work).unwrap();
    let attempt = &before.attempts[0];
    assert_eq!(
        attempt.pause_kind,
        Some(fluent::work_model::PauseKind::RoundCap),
        "{}",
        String::from_utf8_lossy(&released.stderr)
    );
    let failed = attempt
        .tasks
        .iter()
        .rev()
        .find(|t| t.kind == fluent::work_model::TaskKind::Write)
        .unwrap();
    assert_eq!(failed.status, fluent::work_model::TaskStatus::Failed);
    let area = fixture.area(&failed.id);
    let transcript = area.join("transcript.jsonl");
    assert_eq!(fs::read(&transcript).unwrap(), captured);
    let stderr = fs::read(area.join("provider-stderr.log")).unwrap();
    let candidate = attempt.current_candidate_commit().unwrap();
    let launches = fixture.launches().len();
    fixture.binary = assert_cmd::cargo::cargo_bin!("fluent").into();

    // Each altered artifact is a negative control only. Restore the exact
    // released output before exercising the successful public recovery.
    let real: Value = serde_json::from_slice(captured).unwrap();
    for (pointer, value) in [
        ("/stop_reason", json!("end_turn")),
        ("/result_index", json!(1)),
        ("/result_index", Value::Null),
        ("/usage/fallback_credit", json!(0.01)),
        ("/usage/fallback_credit", json!(0)),
        ("/usage/input_tokens", json!(1)),
        ("/usage/server_tool_use/web_fetch_requests", json!(1)),
        ("/usage/output_tokens_details/thinking_tokens", json!(1)),
        ("/num_turns", json!(1)),
        ("/modelUsage", json!({"claude": {"inputTokens": 1}})),
        ("/errors", json!(["Invalid API key"])),
    ] {
        let mut variant = real.clone();
        *variant.pointer_mut(pointer).unwrap() = value;
        fs::write(&transcript, variant.to_string()).unwrap();
        let snapshot_before = snapshot(&fixture.project.join(".fluent/work"));
        let rejected = fixture.resume().output().unwrap();
        assert!(!rejected.status.success(), "accepted {pointer}: {variant}");
        assert_eq!(fixture.launches().len(), launches, "{pointer}");
        assert_eq!(
            snapshot(&fixture.project.join(".fluent/work")),
            snapshot_before,
            "{pointer}"
        );
    }
    for pointer in [
        "",
        "/usage",
        "/usage/cache_creation",
        "/usage/server_tool_use",
    ] {
        for value in [Value::Null, json!(0), json!({}), json!([])] {
            let mut variant = real.clone();
            variant
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("unknown_metadata".into(), value);
            fs::write(&transcript, variant.to_string()).unwrap();
            let snapshot_before = snapshot(&fixture.project.join(".fluent/work"));
            assert!(
                !fixture.resume().output().unwrap().status.success(),
                "accepted {variant}"
            );
            assert_eq!(fixture.launches().len(), launches);
            assert_eq!(
                snapshot(&fixture.project.join(".fluent/work")),
                snapshot_before
            );
        }
    }
    for prefix in [
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"progress"}]}}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}]}}"#,
        std::str::from_utf8(captured).unwrap(),
    ] {
        fs::write(
            &transcript,
            format!("{prefix}\n{}", std::str::from_utf8(captured).unwrap()),
        )
        .unwrap();
        let snapshot_before = snapshot(&fixture.project.join(".fluent/work"));
        assert!(!fixture.resume().output().unwrap().status.success());
        assert_eq!(fixture.launches().len(), launches);
        assert_eq!(
            snapshot(&fixture.project.join(".fluent/work")),
            snapshot_before
        );
    }
    fs::write(&transcript, captured).unwrap();
    let before_inspection = snapshot(&fixture.project.join(".fluent/work"));
    let shown = fixture.ok(&["work-item", "show", &fixture.work]);
    assert!(String::from_utf8_lossy(&shown.stdout).contains("fluent attempt run"));
    assert_eq!(
        snapshot(&fixture.project.join(".fluent/work")),
        before_inspection
    );
    let resumed = fixture.resume().output().unwrap();
    assert!(
        resumed.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&resumed.stdout),
        String::from_utf8_lossy(&resumed.stderr)
    );
    let after = store.read_work_item_snapshot(&fixture.work).unwrap();
    let recovered = after.attempts[0]
        .tasks
        .iter()
        .find(|t| t.id == failed.id)
        .unwrap();
    assert_eq!(recovered.status, fluent::work_model::TaskStatus::Complete);
    assert_ne!(recovered.output.as_ref().unwrap().commit, candidate);
    assert_eq!(after.attempts[0].tasks[0], attempt.tasks[0]);
    for review in attempt
        .tasks
        .iter()
        .filter(|t| t.kind == fluent::work_model::TaskKind::Review)
    {
        assert_eq!(
            after.attempts[0]
                .tasks
                .iter()
                .find(|t| t.id == review.id)
                .unwrap(),
            review
        );
    }
    assert_eq!(fs::read(&transcript).unwrap(), captured);
    assert_eq!(fs::read(area.join("provider-stderr.log")).unwrap(), stderr);
    assert_eq!(fixture.launches().len(), launches + 1);
    assert!(
        after.merge_candidates.is_empty(),
        "later gates remain required"
    );
    assert!(fixture.resume().output().unwrap().status.success());
    assert_eq!(fixture.launches().len(), launches + 1);
}

#[test]
fn released_060_retained_session_rejection_recovers_once() {
    let fixture = Fixture::stranded();
    let launches = fixture.launches();
    let home = Path::new(launches[0]["home"].as_str().unwrap());
    let saved = home
        .join("projects/retained")
        .join(format!("{SESSION}.jsonl"));
    fs::create_dir_all(saved.parent().unwrap()).unwrap();
    fs::write(&saved, "unfamiliar provider history\n").unwrap();
    let output = fixture
        .resume()
        .env("SESSION_PROBE_MODE", "missing")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let after = fixture.launches();
    assert_eq!(
        after.len(),
        launches.len() + 2,
        "one rejected resume and one fresh launch"
    );
    assert_eq!(after[launches.len()]["resume"], SESSION);
    assert!(after.last().unwrap()["resume"].is_null());
    assert_eq!(
        fs::read_to_string(saved).unwrap(),
        "unfamiliar provider history\n"
    );
    let item = WorkModelStore::new(&fixture.project)
        .read_work_item_snapshot(&fixture.work)
        .unwrap();
    let recovered = item.attempts[0]
        .tasks
        .iter()
        .rev()
        .find(|t| t.kind == fluent::work_model::TaskKind::Write)
        .unwrap();
    assert_eq!(recovered.status, fluent::work_model::TaskStatus::Complete);
    let area = fixture.area(&recovered.id);
    assert!(
        fs::read_to_string(area.join("transcript.0.jsonl"))
            .unwrap()
            .contains(SESSION)
    );
    assert!(area.join("coder-supervision.0.json").is_file());
    assert!(fixture.resume().output().unwrap().status.success());
    assert_eq!(
        fixture.launches().len(),
        after.len(),
        "repeated resume cannot launch again"
    );
}

fn prechange_reader(directory: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    const SOURCE: &str = "6d8c9bc6de9b93f57d7aab1c9c6640a86b4bf036";
    let binary = PathBuf::from(std::env::var_os("FLUENT_CLAUDE_SESSION_OLD_BIN")
        .expect("prepare FLUENT_CLAUDE_SESSION_OLD_BIN with tests/fixtures/build-claude-session-old-reader.sh"));
    let root = binary.parent().unwrap().parent().unwrap().parent().unwrap();
    let identity: Value =
        serde_json::from_slice(&fs::read(root.join("identity.json")).unwrap()).unwrap();
    assert_eq!(identity["source_commit"], SOURCE);
    let archive =
        git::run_raw(Path::new(env!("CARGO_MANIFEST_DIR")), &["archive", SOURCE]).unwrap();
    assert!(archive.status.success());
    assert_eq!(
        identity["source_archive_sha256"],
        format!("{:x}", Sha256::digest(&archive.stdout))
    );
    let bytes = fs::read(binary).unwrap();
    assert_eq!(
        identity["binary_sha256"],
        format!("{:x}", Sha256::digest(&bytes))
    );
    let copy = directory.join("fluent-pre-claude-recovery");
    fs::write(&copy, bytes).unwrap();
    fs::set_permissions(&copy, fs::Permissions::from_mode(0o755)).unwrap();
    copy
}

#[test]
fn claude_session_recovery_fences_prechange_writer() {
    let control = Fixture::new();
    let binary = prechange_reader(control.root.path());
    let mutate = |fixture: &Fixture| {
        Command::new(&binary)
            .current_dir(&fixture.project)
            .env("FLUENT_NO_UPDATE_CHECK", "1")
            .args([
                "work-item",
                "abandon",
                &fixture.work,
                "--reason",
                "check reader compatibility",
            ])
            .output()
            .unwrap()
    };
    // The package-version fence remains independent of the session schema.
    let before = snapshot(&control.project.join(".fluent/work"));
    let refused = mutate(&control);
    assert!(
        !refused.status.success(),
        "v0.6 accepted a current-version Work Item"
    );
    assert!(String::from_utf8_lossy(&refused.stderr).contains("older"));
    assert_eq!(snapshot(&control.project.join(".fluent/work")), before);

    // Exercise the historical session boundary with a historical model version.
    let path = control
        .project
        .join(format!(".fluent/work/items/{}.json", control.work));
    let mut record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record["model_writer_version"]["version"] = json!("0.6.0");
    fs::write(path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
    let supported = mutate(&control);
    assert!(
        supported.status.success(),
        "base-format control: {}",
        String::from_utf8_lossy(&supported.stderr)
    );

    for fence in ["record", "envelope", "attempt", "journal"] {
        let fixture = Fixture::stranded();
        let interrupted = fixture
            .resume()
            .env(
                "FLUENT_TEST_CONTEXT_RETRY_INTERRUPT",
                if fence == "journal" {
                    "journal"
                } else {
                    "after"
                },
            )
            .output()
            .unwrap();
        if fence == "journal" {
            assert_eq!(
                interrupted.status.code(),
                Some(86),
                "{}",
                String::from_utf8_lossy(&interrupted.stderr)
            );
            let journal: Value = serde_json::from_slice(
                &fs::read(
                    fixture
                        .project
                        .join(format!(".fluent/work/transactions/{}.json", fixture.work)),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(journal["schema_version"], 16);
        } else {
            assert!(
                interrupted.status.success(),
                "{}",
                String::from_utf8_lossy(&interrupted.stderr)
            );
            let path = fixture
                .project
                .join(format!(".fluent/work/items/{}.json", fixture.work));
            let mut record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(record["schema_version"], 16);
            assert_eq!(record["model_writer_version"]["schema_version"], 14);
            let attempt: Value = serde_json::from_slice(
                &fs::read(fixture.project.join(format!(
                    ".fluent/work/attempts/{}/attempt-1.json",
                    fixture.work
                )))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(attempt["schema_version"], 16);
            // Expose each inner fence while retaining the real published authority.
            if fence != "record" {
                record["schema_version"] = json!(15);
            }
            if fence != "envelope" {
                record["model_writer_version"]["schema_version"] = json!(13);
            }
            fs::write(path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
        }
        let before = snapshot(&fixture.project.join(".fluent/work"));
        let launches = fixture.launches().len();
        let refused = mutate(&fixture);
        assert!(
            !refused.status.success(),
            "{fence}: old writer accepted new authority"
        );
        let error = String::from_utf8_lossy(&refused.stderr);
        assert!(
            error.contains("schema") || (fence == "journal" && error.contains("transaction")),
            "{fence}: {error}"
        );
        assert_eq!(
            snapshot(&fixture.project.join(".fluent/work")),
            before,
            "{fence}"
        );
        assert_eq!(fixture.launches().len(), launches);
    }
}

fn snapshot(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    fn visit(
        root: &Path,
        directory: &Path,
        files: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
    ) {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                visit(root, &entry.path(), files);
            } else if entry.file_type().unwrap().is_file() {
                files.insert(
                    entry.path().strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(entry.path()).unwrap(),
                );
            }
        }
    }
    let mut files = std::collections::BTreeMap::new();
    visit(root, root, &mut files);
    files
}

#[test]
fn missing_session_recovery_preserves_cancellation_and_idempotency() {
    let fixture = Fixture::stranded();
    let store = WorkModelStore::new(&fixture.project);
    let original = store.read_work_item_snapshot(&fixture.work).unwrap();
    let task = original.attempts[0].tasks.last().unwrap();
    let launches = fixture.launches().len();
    let area = fixture.area(&task.id);
    let transcript = fs::read(area.join("transcript.jsonl")).unwrap();
    let workspace = fixture.project.join(&task.workspace_access.writes[0].path);

    // A status query is an observation, even when it discovers a recovery.
    let before = snapshot(&fixture.project.join(".fluent/work"));
    let shown = fixture.ok(&["status"]);
    assert!(
        String::from_utf8_lossy(&shown.stdout)
            .contains(&format!("fluent attempt run {} attempt-1", fixture.work))
    );
    assert_eq!(snapshot(&fixture.project.join(".fluent/work")), before);

    fs::write(workspace.join("uncommitted.txt"), "changed candidate").unwrap();
    assert!(!fixture.resume().output().unwrap().status.success());
    fs::remove_file(workspace.join("uncommitted.txt")).unwrap();
    let mut progress: Value = serde_json::from_slice(&transcript).unwrap();
    progress["num_turns"] = json!(1);
    fs::write(
        area.join("transcript.jsonl"),
        serde_json::to_vec(&progress).unwrap(),
    )
    .unwrap();
    assert!(!fixture.resume().output().unwrap().status.success());
    fs::write(area.join("transcript.jsonl"), &transcript).unwrap();
    let lease = fluent::lease::acquire(&fluent::lease::task_lock_path(
        &fixture.project,
        &fixture.work,
        &task.id,
    ))
    .unwrap();
    assert!(!fixture.resume().output().unwrap().status.success());
    drop(lease);
    assert_eq!(fixture.launches().len(), launches);

    // An interrupted audit publication cannot authorize changed evidence.
    let interrupted = fixture
        .resume()
        .env("FLUENT_TEST_CONTEXT_RETRY_INTERRUPT", "audit")
        .output()
        .unwrap();
    assert_eq!(interrupted.status.code(), Some(86));
    let review = fixture.project.join(
        &task
            .input_artifacts
            .iter()
            .find(|a| a.path.ends_with("review.md"))
            .unwrap()
            .path,
    );
    let findings = fs::read(&review).unwrap();
    fs::write(&review, "Verdict: pass\n").unwrap();
    assert!(!fixture.resume().output().unwrap().status.success());
    assert_eq!(fixture.launches().len(), launches);
    fs::write(review, findings).unwrap();

    // Cancellation survives an interrupted journal and rejects automatic reuse.
    let interrupted = fixture
        .resume()
        .env("FLUENT_TEST_CONTEXT_RETRY_INTERRUPT", "journal")
        .output()
        .unwrap();
    assert_eq!(
        interrupted.status.code(),
        Some(86),
        "{}",
        String::from_utf8_lossy(&interrupted.stderr)
    );
    let pending = snapshot(&fixture.project.join(".fluent/work"));
    let old_writer = Command::new(fixture.root.path().join("fluent-v0.6.0"))
        .current_dir(&fixture.project)
        .env("HOME", fixture.root.path().join("home"))
        .env("FLUENT_NO_UPDATE_CHECK", "1")
        .args([
            "work-item",
            "abandon",
            &fixture.work,
            "--reason",
            "reject pending future authority",
        ])
        .output()
        .unwrap();
    assert!(!old_writer.status.success());
    // The released reader reports an unsupported transaction schema through
    // InvalidId, even though the journal's identity is valid.
    assert!(
        String::from_utf8_lossy(&old_writer.stderr).contains("work item transaction"),
        "{}",
        String::from_utf8_lossy(&old_writer.stderr)
    );
    assert_eq!(snapshot(&fixture.project.join(".fluent/work")), pending);
    fs::write(area.join("coder-supervision.json"), "{}").unwrap();
    assert!(
        !fixture.resume().output().unwrap().status.success(),
        "new settlement evidence must invalidate an unpublished recovery"
    );
    assert_eq!(fixture.launches().len(), launches);
    fs::remove_file(area.join("coder-supervision.json")).unwrap();
    fixture.ok(&["attempt", "cancel", &fixture.work, "attempt-1"]);
    assert!(!fixture.resume().output().unwrap().status.success());
    assert_eq!(fixture.launches().len(), launches);
    assert!(
        store
            .read_work_item_snapshot(&fixture.work)
            .unwrap()
            .attempts[0]
            .cancellation_requested()
    );

    let concurrent = Fixture::stranded();
    let count = concurrent.launches().len();
    let first = concurrent
        .resume()
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let second = concurrent
        .resume()
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let one = first.wait_with_output().unwrap();
    let two = second.wait_with_output().unwrap();
    assert!(
        one.status.success() || two.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&one.stderr),
        String::from_utf8_lossy(&two.stderr)
    );
    assert_eq!(concurrent.launches().len(), count + 1);
    let _ = concurrent.resume().output().unwrap();
    assert_eq!(
        concurrent.launches().len(),
        count + 1,
        "a completed recovery must not launch another Writer"
    );
}

#[test]
fn upgrade_preserves_existing_projects_and_unrelated_work() {
    use fluent::work_model::{AttemptReviewState, AttemptStatus, TaskStatus};
    let mut fixture = Fixture::with_release(true);
    let config = fixture.project.join(".fluent/config.yaml");
    fs::write(
        &config,
        "# Existing operator configuration\nmax-total-write-rounds: 4\n",
    )
    .unwrap();
    fixture.run_writer("attempt-1-write-1");
    let store = WorkModelStore::new(&fixture.project);
    let completed = store.read_work_item_snapshot(&fixture.work).unwrap();
    // Terminal controls are synthetic; the released committed Writer, rich
    // instructions, configuration, and artifact bytes above are genuine.
    for state in [AttemptStatus::Complete, AttemptStatus::NeedsUser] {
        let mut control = store.read_work_item_snapshot(&fixture.work).unwrap();
        control.attempts[0].status = state.clone();
        control.attempts[0].review_state = Some(AttemptReviewState::Passed);
        control.attempts[0].completed_at = Some(fluent::work_model::now_iso8601());
        if state == AttemptStatus::NeedsUser {
            control.add_next_write_round("attempt-1", vec![]).unwrap();
            control.attempts[0].status = AttemptStatus::NeedsUser;
            control.attempts[0].pause_kind = Some(fluent::work_model::PauseKind::RoundCap);
            control.attempts[0].tasks.last_mut().unwrap().status = TaskStatus::Failed;
        }
        store.write_work_item(&control).unwrap();
        fixture.binary = assert_cmd::cargo::cargo_bin!("fluent").into();
        let before = snapshot(&fixture.project.join(".fluent"));
        fixture.ok(&["work-item", "show", &fixture.work]);
        fixture.ok(&["work-item", "list"]);
        fixture.ok(&["status"]);
        assert_eq!(snapshot(&fixture.project.join(".fluent")), before);
        assert_eq!(
            store
                .read_work_item_snapshot(&fixture.work)
                .unwrap()
                .instructions,
            completed.instructions
        );
        assert_eq!(
            fs::read_to_string(&config).unwrap(),
            "# Existing operator configuration\nmax-total-write-rounds: 4\n"
        );
        if state == AttemptStatus::NeedsUser {
            assert!(!fixture.resume().output().unwrap().status.success());
            assert_eq!(
                store
                    .read_work_item_snapshot(&fixture.work)
                    .unwrap()
                    .attempts[0]
                    .tasks
                    .last()
                    .unwrap()
                    .status,
                TaskStatus::Failed
            );
        }
    }
}

/// The host operator runs this only through the candidate-bound shell entry point.
/// Fixture setup uses deterministic commands; all provider launches use a separate
/// production build with the real sandbox, installed CLI, and host credentials.
#[test]
#[ignore = "requires authorized real Claude turns and the production host sandbox"]
fn host_claude_writer_session_upgrade() {
    assert_eq!(std::env::var("FLUENT_CLAUDE_SESSION_HOST").unwrap(), "1");
    let binary = PathBuf::from(std::env::var_os("FLUENT_CLAUDE_SESSION_PRODUCTION_BIN").unwrap());
    let output_root = PathBuf::from(std::env::var_os("FLUENT_CLAUDE_SESSION_OUTPUT_DIR").unwrap());
    let model = std::env::var("FLUENT_CLAUDE_SESSION_MODEL").unwrap();
    let effort = std::env::var("FLUENT_CLAUDE_SESSION_EFFORT").unwrap();
    for mode in ["headless", "native"] {
        let mut fixture = Fixture::new();
        fixture.work = format!("session-host-{mode}");
        fixture.ok(&["work-item", "create", &fixture.work, "--title", "Verify Claude persistence",
            "--instructions", "On EACH invocation append a new line 'Claude session check' to README.md and commit it, even if previous identical lines exist. Make no other product changes. This disposable fixture has no test suite; verify the new line and clean commit with git. Follow this saved instruction on the correction invocation too."]);
        fixture.ok(&[
            "attempt",
            "create",
            &fixture.work,
            "attempt-1",
            "--terminal-mode",
            mode,
            "--write-coder",
            "claude",
            "--write-model",
            &model,
            "--write-effort",
            &effort,
            "--review-coder",
            "claude",
        ]);
        let destination = output_root.join(mode);
        fs::create_dir(&destination).unwrap();
        let run = |task: &str| {
            let logs = destination.join(task);
            fs::create_dir(&logs).unwrap();
            let mut command = Command::new(&binary);
            command
                .current_dir(&fixture.project)
                .env("FLUENT_NO_UPDATE_CHECK", "1")
                .args(["task", "run", &fixture.work, "attempt-1", task]);
            // Never inherit deterministic provider seams from a test runner.
            for key in [
                "FLUENT_TEST_HERMETIC_PROVIDERS",
                "FLUENT_TEST_HOST_SANDBOX_PREFLIGHT",
                "FLUENT_TEST_FOCUSED_EVIDENCE_SANDBOX_EXEC",
                "FLUENT_BIN_OVERRIDE",
                "FLUENT_TASK_KIND",
            ] {
                command.env_remove(key);
            }
            command.stdout(fs::File::create(logs.join("command.stdout")).unwrap());
            command.stderr(fs::File::create(logs.join("command.stderr")).unwrap());
            let status = command.status().unwrap();
            let area = fixture.area(task);
            // Preserve task evidence, never copy the credential-bearing private home.
            if area.is_dir() {
                for (relative, bytes) in snapshot(&area) {
                    let target = logs.join("artifacts").join(relative);
                    fs::create_dir_all(target.parent().unwrap()).unwrap();
                    fs::write(target, bytes).unwrap();
                }
            }
            assert!(
                status.success(),
                "{mode} {task} failed; see {}",
                logs.display()
            );
            assert!(
                !area.join("claude-session-fallback.json").exists(),
                "available host session must resume"
            );
            let transcript = fs::read_to_string(area.join("transcript.jsonl")).unwrap();
            let session = transcript
                .lines()
                .find_map(fluent::transcript::conversation_id_from_line)
                .unwrap();
            let home = fixture
                .project
                .join(".fluent/work/artifacts")
                .join(&fixture.work)
                .join("attempt-1/claude-home");
            let saved = fs::read_dir(home.join("projects"))
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.path().join(format!("{session}.jsonl")))
                .find(|path| path.is_file())
                .expect("real CLI must persist in the assigned home");
            assert!(fs::metadata(&saved).unwrap().len() > 0);
            use sha2::{Digest, Sha256};
            let saved_sha256 = format!("{:x}", Sha256::digest(fs::read(&saved).unwrap()));
            fs::write(
                logs.join("session.json"),
                serde_json::to_vec_pretty(&json!({
                    "session": session, "private_home": home, "saved_path": saved,
                    "saved_sha256": saved_sha256
                }))
                .unwrap(),
            )
            .unwrap();
            session
        };
        let first = run("attempt-1-write-1");
        let (task, prior_commit, _) = fixture.correction();
        assert_eq!(
            run(&task),
            first,
            "real CLI must continue the same conversation"
        );
        let item = WorkModelStore::new(&fixture.project)
            .read_work_item_snapshot(&fixture.work)
            .unwrap();
        let output = item.attempts[0]
            .tasks
            .last()
            .unwrap()
            .output
            .as_ref()
            .unwrap();
        assert_ne!(output.commit, prior_commit);
        let workspace = fixture.project.join(&output.workspace_path);
        let status = Command::new("git")
            .current_dir(&workspace)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        assert!(status.status.success() && status.stdout.is_empty());
        fs::write(
            destination.join("candidate.json"),
            serde_json::to_vec_pretty(&json!({
                "initial": prior_commit, "corrected": output.commit, "session": first,
                "model": model, "effort": effort, "mode": mode, "sandbox": "production"
            }))
            .unwrap(),
        )
        .unwrap();
    }
}
#[test]
fn legacy_host_fixture_exposes_committed_acceptance() {
    let fixture = Fixture::legacy_host();
    for path in [
        ".fluent/tester.yaml",
        ".fluent/extract-tester-results",
        "check-acceptance.sh",
    ] {
        git::run(
            &fixture.project,
            &["cat-file", "-e", &format!("HEAD:{path}")],
            "require committed acceptance checks in the baseline",
        )
        .unwrap();
    }
    let brief = WorkModelStore::new(&fixture.project)
        .read_work_item_snapshot(&fixture.work)
        .unwrap()
        .planning_context
        .unwrap()
        .brief
        .unwrap();
    assert!(brief.contains("at least two"), "{brief}");
    assert!(brief.contains("Claude legacy upgrade check"), "{brief}");
    assert!(brief.contains("sh check-acceptance.sh"), "{brief}");

    // The released Tester must recognize an ordinary failing check, then a
    // passing check, without relying on a provider's review verdict.
    for lines in 0..=2 {
        fs::write(
            fixture.project.join("README.md"),
            format!(
                "First candidate\n{}",
                "Claude legacy upgrade check\n".repeat(lines)
            ),
        )
        .unwrap();
        git::run(
            &fixture.project,
            &["add", "README.md"],
            "stage acceptance sample",
        )
        .unwrap();
        git::run(
            &fixture.project,
            &["commit", "--allow-empty", "-m", "Update acceptance sample"],
            "commit acceptance sample",
        )
        .unwrap();
        let checked = fixture
            .command()
            .args(["tester", "check", "--no-sandbox"])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&checked.stdout);
        let stderr = String::from_utf8_lossy(&checked.stderr);
        assert_eq!(checked.status.success(), lines == 2, "{stdout}\n{stderr}");
        assert!(!stderr.contains("structurally ready"), "{stderr}");
        if lines == 2 {
            assert!(stdout.contains("Tester passed"), "{stdout}\n{stderr}");
        } else {
            assert!(stderr.contains("test-suite failures"), "{stdout}\n{stderr}");
        }
    }
}

// Exercise the same release-created history and assertions as the host check.
// Only this deterministic case substitutes the external Claude process.
#[test]
fn legacy_host_scenario_preserves_history_and_later_gates() {
    exercise_legacy_host_scenario(false, false);
}

#[test]
fn legacy_host_early_completion_still_requires_correction() {
    exercise_legacy_host_scenario(false, true);
}

#[test]
#[ignore = "requires authorized real Claude turns and the production host sandbox"]
fn host_claude_real_legacy_upgrade() {
    assert_eq!(std::env::var("FLUENT_CLAUDE_LEGACY_HOST").unwrap(), "1");
    exercise_legacy_host_scenario(true, true);
}

fn exercise_legacy_host_scenario(real_provider: bool, early_completion: bool) {
    let mut fixture = Fixture::legacy_host();
    let evidence = if real_provider {
        // Retain this newly allocated private fixture even on failure. Its home
        // contains credentials; attach only the separate evidence directory.
        fixture.root.disable_cleanup(true);
        let path = PathBuf::from(std::env::var_os("FLUENT_CLAUDE_LEGACY_OUTPUT_DIR").unwrap());
        fs::write(
            path.join("private-fixture-path.txt"),
            fixture.root.path().as_os_str().as_encoded_bytes(),
        )
        .unwrap();
        path
    } else {
        let path = fixture.root.path().join("evidence");
        fs::create_dir(&path).unwrap();
        path
    };
    let candidate_binary = if real_provider {
        PathBuf::from(std::env::var_os("FLUENT_CLAUDE_SESSION_PRODUCTION_BIN").unwrap())
    } else {
        assert_cmd::cargo::cargo_bin!("fluent").into()
    };
    let command = |binary: &Path| {
        if !real_provider {
            let mut command = fixture.command_for(binary);
            if early_completion {
                command.env("SESSION_PROBE_EARLY_COMPLETION", "1");
            }
            if binary == candidate_binary {
                command.env("SESSION_PROBE_STOP_AFTER_RECOVERY", "1");
            }
            return command;
        }
        let mut command = Command::new(binary);
        command
            .current_dir(&fixture.project)
            .env("XDG_CONFIG_HOME", fixture.root.path().join("config"))
            .env("XDG_DATA_HOME", fixture.root.path().join("data"))
            .env("FLUENT_NO_UPDATE_CHECK", "1");
        // HOME remains available to the host's normal credential preparation.
        // Fluent gives the actual provider its confined private home. Never
        // inherit a test double, Task identity, or fault-injection switch.
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if name.starts_with("FLUENT_TEST_") || name.starts_with("SESSION_PROBE_") {
                command.env_remove(key);
            }
        }
        for name in [
            "FLUENT_BIN_OVERRIDE",
            "FLUENT_TASK_KIND",
            "FLUENT_MAX_TOTAL_WRITE_ROUNDS",
            "CLAUDE_CONFIG_DIR",
        ] {
            command.env_remove(name);
        }
        command
    };
    let run = |binary: &Path, label: &str, args: &[&str]| {
        let result = command(binary).args(args).output().unwrap();
        fs::write(evidence.join(format!("{label}.stdout")), &result.stdout).unwrap();
        fs::write(evidence.join(format!("{label}.stderr")), &result.stderr).unwrap();
        fs::write(
            evidence.join(format!("{label}.status")),
            result.status.to_string(),
        )
        .unwrap();
        result
    };
    let mut args = vec!["attempt", "run", &fixture.work, "attempt-1"];
    let model;
    let effort;
    if real_provider {
        model = std::env::var("FLUENT_CLAUDE_SESSION_MODEL").unwrap();
        effort = std::env::var("FLUENT_CLAUDE_SESSION_EFFORT").unwrap();
        args.extend(["--model", &model, "--effort", &effort]);
    } else {
        args.push("--no-sandbox");
    }
    // A public single-Task run is the phase boundary: no reviewer or Tester can
    // finish the Attempt before the host binds the second-phase requirement.
    // The tag changes no Work records or candidate bytes. Its hash cannot be
    // acknowledged inside the very commit it names, even by an eager Writer.
    let mut initial_args = vec![
        "task",
        "run",
        &fixture.work,
        "attempt-1",
        "attempt-1-write-1",
    ];
    initial_args.extend_from_slice(&args[4..]);
    let initial_run = run(&fixture.binary, "released-initial-writer", &initial_args);
    assert!(initial_run.status.success(), "see {}", evidence.display());
    let store = WorkModelStore::new(&fixture.project);
    let initial_state = store.read_work_item_snapshot(&fixture.work).unwrap();
    fs::write(
        evidence.join("initial-state.json"),
        serde_json::to_vec_pretty(&initial_state).unwrap(),
    )
    .unwrap();
    assert_eq!(initial_state.attempts[0].tasks.len(), 1);
    let initial_task = &initial_state.attempts[0].tasks[0];
    assert_eq!(
        initial_task.status,
        fluent::work_model::TaskStatus::Complete
    );
    let initial = initial_task.output.as_ref().unwrap();
    let workspace = fixture.project.join(&initial.workspace_path);
    let check = |label: &str| {
        let checked = Command::new("sh")
            .current_dir(&workspace)
            .arg("check-acceptance.sh")
            .output()
            .unwrap();
        fs::write(evidence.join(format!("{label}.stdout")), &checked.stdout).unwrap();
        fs::write(evidence.join(format!("{label}.stderr")), &checked.stderr).unwrap();
        fs::write(
            evidence.join(format!("{label}.status")),
            checked.status.to_string(),
        )
        .unwrap();
        checked
    };
    let visible = check("initial-acceptance");
    if !real_provider {
        assert_eq!(visible.status.success(), early_completion);
    }
    let before_tag = snapshot(&fixture.project.join(".fluent/work"));
    git::run(
        &workspace,
        &["tag", "claude-legacy-initial", &initial.commit],
        "bind the settled initial candidate before ordinary review",
    )
    .unwrap();
    assert!(snapshot(&fixture.project.join(".fluent/work")) == before_tag);
    assert!(!check("correction-acceptance").status.success());
    assert_eq!(
        git::run_stdout(
            &workspace,
            &["rev-parse", "HEAD"],
            "retain initial candidate"
        )
        .unwrap(),
        initial.commit
    );
    assert!(
        git::run_stdout(
            &workspace,
            &["status", "--porcelain"],
            "retain clean candidate"
        )
        .unwrap()
        .is_empty()
    );
    let _released = run(&fixture.binary, "released-run", &args);
    let before = store.read_work_item_snapshot(&fixture.work).unwrap();
    fs::write(
        evidence.join("released-state.json"),
        serde_json::to_vec_pretty(&before).unwrap(),
    )
    .unwrap();
    let attempt = &before.attempts[0];
    assert_eq!(
        attempt.pause_kind,
        Some(fluent::work_model::PauseKind::RoundCap),
        "see {}",
        evidence.display()
    );
    let first = &attempt.tasks[0];
    assert_eq!(first, initial_task, "retain the initial public Task result");
    assert_eq!(first.status, fluent::work_model::TaskStatus::Complete);
    let original = first.output.as_ref().unwrap();
    let failed = attempt.tasks.last().unwrap();
    assert_eq!(failed.kind, fluent::work_model::TaskKind::Write);
    assert_eq!(failed.status, fluent::work_model::TaskStatus::Failed);
    let area = fixture.area(&failed.id);
    let retained = snapshot(&area);
    for (relative, bytes) in &retained {
        let target = evidence.join("original-failure").join(relative);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, bytes).unwrap();
    }
    let transcript: Value =
        serde_json::from_slice(&retained[Path::new("transcript.jsonl")]).unwrap();
    let session = attempt
        .writer_runs
        .last()
        .unwrap()
        .session_id
        .as_deref()
        .unwrap();
    assert_eq!(transcript["num_turns"], 0);
    assert_eq!(transcript["session_id"], session);
    assert_eq!(
        transcript["errors"],
        json!([format!("No conversation found with session ID: {session}")])
    );
    if real_provider {
        assert_eq!(transcript.get("stop_reason"), Some(&Value::Null));
        assert_eq!(transcript.get("result_index"), Some(&json!(0)));
        assert_eq!(
            transcript.pointer("/usage/fallback_credit"),
            Some(&Value::Null)
        );
    }
    let workspace = fixture.project.join(&original.workspace_path);
    let first_readme = git::run_stdout(
        &workspace,
        &["show", &format!("{}:README.md", original.commit)],
        "inspect first Writer acceptance state",
    )
    .unwrap();
    assert!(
        !first_readme
            .lines()
            .any(|line| { line == format!("Recovered initial candidate: {}", original.commit) }),
        "the first candidate must leave the host-bound acknowledgment for correction"
    );
    git::run(
        &workspace,
        &[
            "bundle",
            "create",
            evidence.join("original-candidate.bundle").to_str().unwrap(),
            "HEAD",
            "refs/tags/claude-legacy-initial",
        ],
        "retain original commit",
    )
    .unwrap();
    let before_inspection = snapshot(&fixture.project.join(".fluent/work"));
    let shown = run(&candidate_binary, "upgraded-status", &["status"]);
    assert!(shown.status.success());
    assert!(
        String::from_utf8_lossy(&shown.stdout)
            .contains(&format!("fluent attempt run {} attempt-1", fixture.work)),
        "{}\n{before:#?}",
        String::from_utf8_lossy(&shown.stdout)
    );
    // Compare private home bytes without ever rendering them into host logs.
    assert!(
        snapshot(&fixture.project.join(".fluent/work")) == before_inspection,
        "read-only status changed the disposable fixture"
    );
    let mut resume = command(&candidate_binary);
    resume
        .args(&args)
        .stdout(fs::File::create(evidence.join("upgraded-resume.stdout")).unwrap())
        .stderr(fs::File::create(evidence.join("upgraded-resume.stderr")).unwrap());
    let mut child = resume.spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    loop {
        let completed = store
            .read_work_item_snapshot(&fixture.work)
            .ok()
            .is_some_and(|item| {
                item.attempts[0].tasks.iter().any(|task| {
                    task.id == failed.id
                        && task.status == fluent::work_model::TaskStatus::Complete
                        && item.attempts[0].writer_runs.iter().any(|run| {
                            run.task_id == failed.id && run.outcome == WriterOutcome::Complete
                        })
                        && task.artifact_area.as_ref().is_some_and(|area| {
                            let record = fluent::execution::record_path_for_artifact(
                                &fixture.project,
                                &area.path,
                            );
                            fluent::execution::execution_is_live(&record).is_ok_and(|live| !live)
                        })
                })
            });
        if completed || child.try_wait().unwrap().is_some() || std::time::Instant::now() >= deadline
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // Wait for the coordinator to record the Writer run and for its execution
    // owner to settle before canceling. A terminal Task field alone can appear
    // while the worker is still returning its result. Review may already have
    // started; no downstream gate gets credit.
    let canceled = run(
        &candidate_binary,
        "stop-after-writer",
        &["attempt", "cancel", &fixture.work, "attempt-1"],
    );
    let status = child.wait().unwrap();
    fs::write(evidence.join("upgraded-resume.status"), status.to_string()).unwrap();
    assert!(canceled.status.success(), "see {}", evidence.display());
    let after = store.read_work_item_snapshot(&fixture.work).unwrap();
    fs::write(
        evidence.join("recovered-state.json"),
        serde_json::to_vec_pretty(&after).unwrap(),
    )
    .unwrap();
    let recovered = after.attempts[0]
        .tasks
        .iter()
        .find(|t| t.id == failed.id)
        .unwrap();
    assert_eq!(
        recovered.status,
        fluent::work_model::TaskStatus::Complete,
        "resume: {}\ncancel: {}\n{after:#?}",
        fs::read_to_string(evidence.join("upgraded-resume.stderr")).unwrap(),
        String::from_utf8_lossy(&canceled.stderr)
    );
    let output = recovered.output.as_ref().unwrap();
    assert_ne!(output.commit, original.commit);
    assert_eq!(
        git::run_stdout(
            &workspace,
            &["rev-parse", "refs/tags/claude-legacy-initial"],
            "preserve the host-owned initial candidate tag",
        )
        .unwrap(),
        original.commit
    );
    assert_eq!(
        git::run_stdout(
            &workspace,
            &["diff", "--name-only", "main", &output.commit],
            "preserve the baseline acceptance check and Tester configuration",
        )
        .unwrap()
        .trim(),
        "README.md"
    );
    let accepted = Command::new("sh")
        .current_dir(&workspace)
        .arg("check-acceptance.sh")
        .output()
        .unwrap();
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stdout)
    );
    assert_eq!(after.attempts[0].tasks[0], *first);
    assert_eq!(
        snapshot(&area),
        retained,
        "retain the entire original failure"
    );
    for input in &failed.input_artifacts {
        assert!(
            recovered.input_artifacts.contains(input),
            "retain original finding {}",
            input.path
        );
    }
    git::run(
        &workspace,
        &[
            "merge-base",
            "--is-ancestor",
            &original.commit,
            &output.commit,
        ],
        "preserve first Writer commit",
    )
    .unwrap();
    assert!(
        git::run_stdout(
            &workspace,
            &["status", "--porcelain"],
            "check recovered candidate"
        )
        .unwrap()
        .is_empty()
    );
    assert!(after.merge_candidates.is_empty());
    let recovered_area = fixture.area(&recovered.id);
    for (relative, bytes) in snapshot(&recovered_area) {
        let target = evidence.join("recovered-writer").join(relative);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, bytes).unwrap();
    }
    git::run(
        &workspace,
        &[
            "bundle",
            "create",
            evidence
                .join("recovered-candidate.bundle")
                .to_str()
                .unwrap(),
            "HEAD",
            "refs/tags/claude-legacy-initial",
        ],
        "retain recovered commit",
    )
    .unwrap();
    fs::write(
        evidence.join("result.json"),
        serde_json::to_vec_pretty(&json!({
            "released_commit": "be19732c42ac645cb9fac5701c8e93171b1aa480",
            "first_candidate": original.commit, "recovered_candidate": output.commit,
            "failed_task": failed.id, "recovered_task": recovered.id,
            "real_provider": real_provider, "writer_progress": "pass",
            "downstream_gates": "not credited; public cancellation stops after recovered Writer"
        }))
        .unwrap(),
    )
    .unwrap();
}
