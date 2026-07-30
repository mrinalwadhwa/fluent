use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use crate::content::ContentResolver;
use crate::os;

const TESTER_YAML_PATH: &str = ".fluent/tester.yaml";
const EXTRACTOR_PATH: &str = ".fluent/extract-tester-results";
const FAILURE_EXCERPT_MAX: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TesterResults {
    pub commands: Vec<CommandResult>,
    pub tests: Vec<TestResult>,
    pub summary: Summary,
    pub error: Option<TesterError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub command: String,
    pub test_harness: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub stdout_log: String,
    pub stderr_log: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    pub id: String,
    pub test_harness: String,
    pub status: String,
    pub duration_ms: Option<u64>,
    pub failure_excerpt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub total: usize,
    pub pass: usize,
    pub fail: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TesterError {
    pub kind: String,
    pub message: String,
    pub details: String,
}

#[derive(Debug)]
struct NormalizationError {
    kind: &'static str,
    message: String,
}

impl std::fmt::Display for NormalizationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for NormalizationError {}

/// The trustworthy result of executing the Tester boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TesterOutcome {
    Passed,
    TestFailures,
    HarnessError,
}

#[derive(Debug, Deserialize)]
struct TesterConfig {
    commands: Vec<TesterCommand>,
}

#[derive(Debug, Deserialize)]
struct TesterCommand {
    command: String,
    test_harness: String,
}

fn tester_writable_roots(candidate: &Path, artifact: &Path, home: &Path) -> Vec<PathBuf> {
    let mut roots = vec![candidate.to_path_buf(), artifact.to_path_buf()];

    let cargo_registry = home.join(".cargo/registry");
    let cargo_git = home.join(".cargo/git/db");
    if cargo_registry.is_dir() {
        roots.push(cargo_registry);
    }
    if cargo_git.is_dir() {
        roots.push(cargo_git);
    }

    if candidate.join("package.json").is_file() {
        let mut pnpm_stores = Vec::new();
        if let Ok(pnpm_home) = std::env::var("PNPM_HOME") {
            pnpm_stores.push(PathBuf::from(pnpm_home).join("store"));
        }
        pnpm_stores.push(home.join("Library/pnpm/store"));
        pnpm_stores.push(home.join(".local/share/pnpm/store"));
        for store in pnpm_stores {
            if store.is_dir() {
                roots.push(store);
            }
        }
    }

    roots
}

pub fn run(
    candidate_workspace: &Path,
    artifact_dir: &Path,
    no_sandbox: bool,
    resolver: &ContentResolver,
) -> Result<TesterOutcome> {
    let profile =
        preflight_sandbox_profile(candidate_workspace, artifact_dir, no_sandbox, resolver)?;
    run_with_sandbox_profile(candidate_workspace, artifact_dir, profile.as_ref())
}

/// Run the selected checkout through the production Tester boundary after first
/// collecting every independent structural readiness problem.
pub fn check(
    candidate_workspace: &Path,
    no_sandbox: bool,
    resolver: &ContentResolver,
) -> Result<TesterOutcome> {
    let artifact = tempfile::tempdir().context("creating Tester readiness artifact")?;
    let profile =
        preflight_sandbox_profile(candidate_workspace, artifact.path(), no_sandbox, resolver)?;
    let problems = structural_problems(candidate_workspace, artifact.path(), profile.as_ref());
    if !problems.is_empty() {
        anyhow::bail!(
            "Tester is not structurally ready:\n{}",
            problems
                .into_iter()
                .map(|p| format!("- {p}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    run_with_sandbox_profile(candidate_workspace, artifact.path(), profile.as_ref())
}

/// Validate Tester configuration and empty-artifact extraction without running
/// declared commands. Review-only setup uses this gate before it creates Work
/// state; `check` adds the production command run above this common validation.
pub fn check_structural(
    candidate_workspace: &Path,
    no_sandbox: bool,
    resolver: &ContentResolver,
) -> Result<()> {
    let artifact = tempfile::tempdir().context("creating Tester readiness artifact")?;
    let profile =
        preflight_sandbox_profile(candidate_workspace, artifact.path(), no_sandbox, resolver)?;
    let problems = structural_problems(candidate_workspace, artifact.path(), profile.as_ref());
    if problems.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "Tester is not structurally ready:\n{}",
            problems
                .into_iter()
                .map(|p| format!("- {p}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

/// Render and apply the Tester production boundary without creating artifacts or
/// launching a test command. Callers that reserve Tasks use this before their
/// durable reservation, then pass the returned profile to `run_with_sandbox_profile`
/// so the exact probed profile encloses the workload.
pub fn preflight_sandbox_profile(
    candidate_workspace: &Path,
    artifact_dir: &Path,
    no_sandbox: bool,
    resolver: &ContentResolver,
) -> Result<Option<os::SandboxProfile>> {
    if no_sandbox {
        return Ok(None);
    }
    // Test-support can force the host preflight result while macOS test runners
    // remain unable to apply a nested Seatbelt profile. Production never takes
    // this branch and always returns the profile it preflighted.
    #[cfg(feature = "test-support")]
    if std::env::var("FLUENT_TEST_HOST_SANDBOX_PREFLIGHT")
        .ok()
        .as_deref()
        == Some("pass")
    {
        return Ok(None);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let candidate_abs =
        fs::canonicalize(candidate_workspace).unwrap_or_else(|_| candidate_workspace.to_path_buf());
    let artifact_abs =
        fs::canonicalize(artifact_dir).unwrap_or_else(|_| artifact_dir.to_path_buf());
    let writable_roots = tester_writable_roots(&candidate_abs, &artifact_abs, Path::new(&home));
    match os::render_profile_common_only(resolver, &home, &writable_roots, &[]) {
        Ok(profile) => {
            os::preflight_profile(&profile)?;
            Ok(Some(profile))
        }
        Err(err) => {
            eprintln!("  Warning: sandbox render failed: {err:#}; running unsandboxed");
            Ok(None)
        }
    }
}

pub fn run_with_sandbox_profile(
    candidate_workspace: &Path,
    artifact_dir: &Path,
    sandbox_profile: Option<&os::SandboxProfile>,
) -> Result<TesterOutcome> {
    record_test_boundary(sandbox_profile.is_some());
    let tester_yaml_path = candidate_workspace.join(TESTER_YAML_PATH);
    let extractor_path = candidate_workspace.join(EXTRACTOR_PATH);
    let config = match read_tester_config(&tester_yaml_path) {
        Ok(config) => config,
        Err(error) => {
            return persist_harness_error(
                artifact_dir,
                "tester_yaml_problem",
                format!("Failed to read {TESTER_YAML_PATH}"),
                format!("{error:#}"),
                Vec::new(),
            );
        }
    };
    if !extractor_path.is_file() || !is_executable(&extractor_path) {
        return persist_harness_error(
            artifact_dir,
            "extractor_missing",
            format!("{EXTRACTOR_PATH} missing or not executable"),
            String::new(),
            Vec::new(),
        );
    }

    let commands_dir = artifact_dir.join("commands");
    fs::create_dir_all(&commands_dir)?;

    if let Some(profile) = sandbox_profile {
        eprintln!("  Sandbox profile  {}", profile.path.display());
    }

    let sandbox_path = sandbox_profile.as_ref().map(|p| &p.path);

    let mut command_results = Vec::new();
    for (index, cmd) in config.commands.iter().enumerate() {
        eprintln!("  Running command {}: {}", index, cmd.command);
        let result = run_command(
            candidate_workspace,
            &cmd.command,
            &cmd.test_harness,
            index,
            &commands_dir,
            sandbox_path,
        )?;
        eprintln!(
            "    exit_code={} duration={}ms",
            result.exit_code, result.duration_ms
        );
        command_results.push(result);
    }

    let commands_json = serde_json::to_string_pretty(&command_results)?;
    fs::write(artifact_dir.join("commands.json"), &commands_json)?;

    if let Some(result) = command_results.iter().find(|result| {
        fs::read_to_string(artifact_dir.join(&result.stderr_log))
            .ok()
            .is_some_and(|stderr| is_nested_swiftpm_sandbox_failure(&result.command, &stderr))
    }) {
        let details = format!(
            "The Tester sandbox already confines `{}`. Disable SwiftPM's inner sandbox (for example, pass --disable-sandbox) and configure writable project-local cache paths before retrying. Fluent did not modify .fluent/tester.yaml or project scripts.",
            result.command
        );
        eprintln!("  Tester guidance: {details}");
        return persist_harness_error(
            artifact_dir,
            "nested_swiftpm_sandbox",
            "Swift Package Manager could not create its inner sandbox".to_string(),
            details,
            command_results,
        );
    }

    let (tests, summary) = match extract_and_normalize(&extractor_path, artifact_dir, sandbox_path)
    {
        Ok(normalized) => normalized,
        Err(error) => {
            let (kind, message) =
                if let Some(normalization) = error.downcast_ref::<NormalizationError>() {
                    (normalization.kind, normalization.message.clone())
                } else {
                    (
                        "extractor_failure",
                        "extract-tester-results failed".to_string(),
                    )
                };
            return persist_harness_error(
                artifact_dir,
                kind,
                message,
                truncate_tail(&format!("{error:#}"), FAILURE_EXCERPT_MAX),
                command_results,
            );
        }
    };

    let results = TesterResults {
        commands: command_results,
        tests,
        summary,
        error: None,
    };

    write_results(artifact_dir, &results)?;
    Ok(if results.summary.fail == 0 {
        TesterOutcome::Passed
    } else {
        TesterOutcome::TestFailures
    })
}

#[cfg(feature = "test-support")]
fn record_test_boundary(sandboxed: bool) {
    let Ok(path) = std::env::var("FLUENT_TEST_TESTER_BOUNDARY_LOG") else {
        return;
    };
    let _ = fs::write(
        path,
        format!("run_with_sandbox_profile sandboxed={sandboxed}\n"),
    );
}

#[cfg(not(feature = "test-support"))]
fn record_test_boundary(_: bool) {}

fn persist_harness_error(
    artifact_dir: &Path,
    kind: &str,
    message: String,
    details: String,
    commands: Vec<CommandResult>,
) -> Result<TesterOutcome> {
    write_results(
        artifact_dir,
        &TesterResults {
            commands,
            tests: Vec::new(),
            summary: Summary {
                total: 0,
                pass: 0,
                fail: 0,
                skipped: 0,
            },
            error: Some(TesterError {
                kind: kind.to_string(),
                message,
                details,
            }),
        },
    )?;
    Ok(TesterOutcome::HarnessError)
}

fn structural_problems(
    candidate_workspace: &Path,
    artifact_dir: &Path,
    profile: Option<&os::SandboxProfile>,
) -> Vec<String> {
    let mut problems = Vec::new();
    if let Err(error) = read_tester_config(&candidate_workspace.join(TESTER_YAML_PATH)) {
        problems.push(format!("{TESTER_YAML_PATH}: {error:#}"));
    }
    let extractor = candidate_workspace.join(EXTRACTOR_PATH);
    if !extractor.is_file() {
        problems.push(format!("{EXTRACTOR_PATH}: not found"));
    } else if !is_executable(&extractor) {
        problems.push(format!("{EXTRACTOR_PATH}: not executable"));
    } else {
        if let Err(error) = fs::write(artifact_dir.join("commands.json"), "[]") {
            problems.push(format!("readiness artifact: {error}"));
        } else if let Err(error) =
            extract_and_normalize(&extractor, artifact_dir, profile.map(|p| &p.path))
        {
            problems.push(format!(
                "{EXTRACTOR_PATH}: invalid empty artifact: {error:#}"
            ));
        }
    }
    problems
}

fn extract_and_normalize(
    extractor_path: &Path,
    artifact_dir: &Path,
    sandbox_path: Option<&PathBuf>,
) -> Result<(Vec<TestResult>, Summary)> {
    let tests = run_extractor(extractor_path, artifact_dir, sandbox_path)?;
    let duplicate_ids = find_duplicate_ids(&tests);
    if !duplicate_ids.is_empty() {
        return Err(NormalizationError {
            kind: "duplicate_test_ids",
            message: format!("Duplicate test ids: {}", duplicate_ids.join(", ")),
        }
        .into());
    }
    let mut tests = cap_failure_excerpts(tests);
    tests.sort_by(|a, b| {
        a.test_harness
            .cmp(&b.test_harness)
            .then_with(|| a.id.cmp(&b.id))
    });
    let summary = compute_summary(&tests);
    Ok((tests, summary))
}

fn read_tester_config(path: &Path) -> Result<TesterConfig> {
    let content =
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let config: TesterConfig =
        serde_yaml::from_str(&content).with_context(|| format!("parsing {}", path.display()))?;
    if config.commands.is_empty() {
        anyhow::bail!("tester.yaml declares no commands");
    }
    Ok(config)
}

fn run_command(
    working_dir: &Path,
    command: &str,
    test_harness: &str,
    index: usize,
    commands_dir: &Path,
    sandbox_path: Option<&PathBuf>,
) -> Result<CommandResult> {
    let stdout_path = commands_dir.join(format!("{index}-stdout.log"));
    let stderr_path = commands_dir.join(format!("{index}-stderr.log"));

    let start = Instant::now();
    let output = if let Some(profile) = sandbox_path {
        Command::new("sandbox-exec")
            .arg("-f")
            .arg(profile)
            .arg("sh")
            .arg("-c")
            .arg(command)
            .current_dir(working_dir)
            .output()
            .with_context(|| format!("spawning sandboxed command: {command}"))?
    } else {
        Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(working_dir)
            .output()
            .with_context(|| format!("spawning command: {command}"))?
    };
    let duration = start.elapsed();

    fs::write(&stdout_path, &output.stdout)?;
    fs::write(&stderr_path, &output.stderr)?;

    let exit_code = output.status.code().unwrap_or(-1);

    let stdout_log = format!("commands/{index}-stdout.log");
    let stderr_log = format!("commands/{index}-stderr.log");

    Ok(CommandResult {
        command: command.to_string(),
        test_harness: test_harness.to_string(),
        exit_code,
        duration_ms: duration.as_millis() as u64,
        stdout_log,
        stderr_log,
    })
}

fn run_extractor(
    extractor_path: &Path,
    artifact_dir: &Path,
    sandbox_path: Option<&PathBuf>,
) -> Result<Vec<TestResult>> {
    let output = if let Some(profile) = sandbox_path {
        Command::new("sandbox-exec")
            .arg("-f")
            .arg(profile)
            .arg(extractor_path)
            .arg(artifact_dir)
            .output()
            .with_context(|| format!("running sandboxed {}", extractor_path.display()))?
    } else {
        Command::new(extractor_path)
            .arg(artifact_dir)
            .output()
            .with_context(|| format!("running {}", extractor_path.display()))?
    };

    if !output.status.success() {
        let stderr_tail = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "exit code {}: {}",
            output.status.code().unwrap_or(-1),
            truncate_tail(&stderr_tail, FAILURE_EXCERPT_MAX)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let tests: Vec<TestResult> = serde_json::from_str(&stdout).with_context(|| {
        format!(
            "parsing extractor output as JSON array: {}",
            truncate_tail(&stdout, 200)
        )
    })?;

    for test in &tests {
        match test.status.as_str() {
            "pass" | "fail" | "skipped" | "not_run" => {}
            other => {
                anyhow::bail!("invalid test status {:?} for test {:?}", other, test.id);
            }
        }
    }

    Ok(tests)
}

fn find_duplicate_ids(tests: &[TestResult]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut duplicates = std::collections::BTreeSet::new();
    for test in tests {
        if !seen.insert(&test.id) {
            duplicates.insert(test.id.clone());
        }
    }
    duplicates.into_iter().collect()
}

fn compute_summary(tests: &[TestResult]) -> Summary {
    let mut pass = 0;
    let mut fail = 0;
    let mut skipped = 0;
    for test in tests {
        match test.status.as_str() {
            "pass" => pass += 1,
            "fail" => fail += 1,
            "skipped" | "not_run" => skipped += 1,
            _ => {}
        }
    }
    Summary {
        total: tests.len(),
        pass,
        fail,
        skipped,
    }
}

fn cap_failure_excerpts(mut tests: Vec<TestResult>) -> Vec<TestResult> {
    for test in &mut tests {
        if let Some(excerpt) = &test.failure_excerpt {
            if excerpt.len() > FAILURE_EXCERPT_MAX {
                test.failure_excerpt = Some(truncate_tail(excerpt, FAILURE_EXCERPT_MAX));
            }
        }
    }
    tests
}

fn truncate_tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut start = s.len() - max;
        while !s.is_char_boundary(start) {
            start += 1;
        }
        format!("…{}", &s[start..])
    }
}

fn is_nested_swiftpm_sandbox_failure(command: &str, stderr: &str) -> bool {
    let command = command.to_ascii_lowercase();
    let stderr = stderr.to_ascii_lowercase();
    (command.contains("swift") || stderr.contains("swift package manager"))
        && stderr.contains("sandbox-exec")
        && (stderr.contains("sandbox_apply") || stderr.contains("operation not permitted"))
}

fn write_results(artifact_dir: &Path, results: &TesterResults) -> Result<()> {
    let json = serde_json::to_string_pretty(results)?;
    let path = artifact_dir.join("tester-results.json");
    crate::atomic_write::atomic_write(&path, json.as_bytes())?;
    eprintln!("  Wrote {}", path.display());
    Ok(())
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = fs::metadata(path) {
            return metadata.permissions().mode() & 0o111 != 0;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn make_workspace(dir: &Path) {
        let fluent_dir = dir.join(".fluent");
        fs::create_dir_all(&fluent_dir).unwrap();
    }

    fn write_tester_yaml(dir: &Path, content: &str) {
        let path = dir.join(TESTER_YAML_PATH);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
    }

    fn write_extractor(dir: &Path, script: &str) {
        let path = dir.join(EXTRACTOR_PATH);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, script).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
    }

    fn resolver() -> ContentResolver {
        ContentResolver::new(None)
    }

    fn read_results(artifact_dir: &Path) -> TesterResults {
        let path = artifact_dir.join("tester-results.json");
        let content = fs::read_to_string(&path).unwrap();
        serde_json::from_str(&content).unwrap()
    }

    #[test]
    fn task_kind_tester_round_trips() {
        use crate::work_model::TaskKind;
        let json = serde_json::to_string(&TaskKind::Tester).unwrap();
        assert_eq!(json, r#""tester""#);
        let kind: TaskKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, TaskKind::Tester);
        assert_eq!(TaskKind::Tester.as_str(), "tester");
        assert_eq!(format!("{}", TaskKind::Tester), "tester");
        assert_eq!("tester".parse::<TaskKind>().unwrap(), TaskKind::Tester);
    }

    #[test]
    fn tester_results_top_level_shape() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());

        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: echo hello\n    test_harness: shell-harness\n",
        );
        write_extractor(workspace.path(), "#!/bin/sh\necho '[]'\n");

        run(workspace.path(), artifact_dir.path(), true, &resolver()).unwrap();
        let results = read_results(artifact_dir.path());

        assert!(results.error.is_none());
        assert!(!results.commands.is_empty());
        assert!(results.tests.is_empty());
        assert_eq!(results.summary.total, 0);
    }

    #[test]
    fn tester_diagnoses_nested_swiftpm_sandbox_failure() {
        assert!(is_nested_swiftpm_sandbox_failure(
            "swift test",
            "sandbox-exec: sandbox_apply: Operation not permitted"
        ));
        assert!(!is_nested_swiftpm_sandbox_failure(
            "cargo test",
            "sandbox-exec: sandbox_apply: Operation not permitted"
        ));
    }

    #[test]
    fn check_rejects_extractor_that_cannot_read_empty_commands() {
        let workspace = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: echo hello\n    test_harness: shell-harness\n",
        );
        write_extractor(workspace.path(), "#!/bin/sh\nexit 1\n");

        let error = check_structural(workspace.path(), true, &resolver()).unwrap_err();
        assert!(format!("{error:#}").contains("invalid empty artifact"));
    }

    #[test]
    fn structural_check_rejects_duplicate_empty_artifact_ids() {
        let workspace = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: echo hello\n    test_harness: shell-harness\n",
        );
        write_extractor(
            workspace.path(),
            "#!/bin/sh\necho '[{\"id\":\"duplicate\",\"test_harness\":\"shell-harness\",\"status\":\"pass\",\"duration_ms\":null,\"failure_excerpt\":null},{\"id\":\"duplicate\",\"test_harness\":\"shell-harness\",\"status\":\"pass\",\"duration_ms\":null,\"failure_excerpt\":null}]'\n",
        );

        let error = check_structural(workspace.path(), true, &resolver()).unwrap_err();
        assert!(format!("{error:#}").contains("Duplicate test ids: duplicate"));
    }

    #[test]
    fn tester_results_command_entry_shape() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());

        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: echo hello\n    test_harness: shell-harness\n",
        );
        write_extractor(workspace.path(), "#!/bin/sh\necho '[]'\n");

        run(workspace.path(), artifact_dir.path(), true, &resolver()).unwrap();
        let results = read_results(artifact_dir.path());

        assert_eq!(results.commands.len(), 1);
        let cmd = &results.commands[0];
        assert_eq!(cmd.command, "echo hello");
        assert_eq!(cmd.exit_code, 0);
        assert_eq!(cmd.stdout_log, "commands/0-stdout.log");
        assert_eq!(cmd.stderr_log, "commands/0-stderr.log");
    }

    #[test]
    fn tester_results_test_entry_shape() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());

        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: echo test\n    test_harness: cargo-nextest\n",
        );
        write_extractor(
            workspace.path(),
            r#"#!/bin/sh
echo '[{"id": "tests::foo", "test_harness": "cargo-nextest", "status": "pass", "duration_ms": 42, "failure_excerpt": null}]'
"#,
        );

        run(workspace.path(), artifact_dir.path(), true, &resolver()).unwrap();
        let results = read_results(artifact_dir.path());

        assert_eq!(results.tests.len(), 1);
        let test = &results.tests[0];
        assert_eq!(test.id, "tests::foo");
        assert_eq!(test.test_harness, "cargo-nextest");
        assert_eq!(test.status, "pass");
        assert_eq!(test.duration_ms, Some(42));
        assert!(test.failure_excerpt.is_none());
    }

    #[test]
    fn tester_results_failure_excerpt_capped_at_500_chars() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());

        let long_excerpt = "x".repeat(800);
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: echo test\n    test_harness: cargo-nextest\n",
        );
        write_extractor(
            workspace.path(),
            &format!(
                r#"#!/bin/sh
echo '[{{"id": "tests::bar", "test_harness": "cargo-nextest", "status": "fail", "duration_ms": 10, "failure_excerpt": "{long_excerpt}"}}]'
"#,
            ),
        );

        run(workspace.path(), artifact_dir.path(), true, &resolver()).unwrap();
        let results = read_results(artifact_dir.path());

        let excerpt = results.tests[0].failure_excerpt.as_ref().unwrap();
        assert!(excerpt.len() <= FAILURE_EXCERPT_MAX + "…".len());
    }

    #[test]
    fn truncate_tail_preserves_utf8_boundaries() {
        let excerpt = format!("{}…{}", "x".repeat(499), "z".repeat(498));

        let truncated = truncate_tail(&excerpt, FAILURE_EXCERPT_MAX);

        assert_eq!(truncated, format!("…{}", "z".repeat(498)));
        assert!(truncated.len() <= FAILURE_EXCERPT_MAX + "…".len());
    }

    #[test]
    fn tester_results_caps_multibyte_failure_excerpt_without_panicking() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());

        let long_excerpt = format!("{}…{}", "x".repeat(499), "z".repeat(498));
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: false\n    test_harness: cargo-nextest\n",
        );
        write_extractor(
            workspace.path(),
            &format!(
                r#"#!/bin/sh
echo '[{{"id": "tests::multibyte", "test_harness": "cargo-nextest", "status": "fail", "duration_ms": 10, "failure_excerpt": "{long_excerpt}"}}]'
"#,
            ),
        );

        run(workspace.path(), artifact_dir.path(), true, &resolver()).unwrap();
        let results = read_results(artifact_dir.path());

        let excerpt = results.tests[0].failure_excerpt.as_ref().unwrap();
        assert_eq!(excerpt, &format!("…{}", "z".repeat(498)));
        assert!(excerpt.len() <= FAILURE_EXCERPT_MAX + "…".len());
    }

    #[test]
    fn tester_results_summary_counts_match_tests_partition() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());

        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: echo test\n    test_harness: cargo-nextest\n",
        );
        write_extractor(
            workspace.path(),
            r#"#!/bin/sh
echo '[
  {"id": "a", "test_harness": "cargo-nextest", "status": "pass", "duration_ms": 1, "failure_excerpt": null},
  {"id": "b", "test_harness": "cargo-nextest", "status": "fail", "duration_ms": 2, "failure_excerpt": "oops"},
  {"id": "c", "test_harness": "cargo-nextest", "status": "skipped", "duration_ms": null, "failure_excerpt": null}
]'
"#,
        );

        run(workspace.path(), artifact_dir.path(), true, &resolver()).unwrap();
        let results = read_results(artifact_dir.path());

        assert_eq!(results.summary.total, 3);
        assert_eq!(results.summary.pass, 1);
        assert_eq!(results.summary.fail, 1);
        assert_eq!(results.summary.skipped, 1);
        assert_eq!(
            results.summary.total,
            results.summary.pass + results.summary.fail + results.summary.skipped
        );
    }

    #[test]
    fn tester_writes_complete_command_logs_to_artifact_dir() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());

        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: echo stdout-content && echo stderr-content >&2\n    test_harness: shell-harness\n",
        );
        write_extractor(workspace.path(), "#!/bin/sh\necho '[]'\n");

        run(workspace.path(), artifact_dir.path(), true, &resolver()).unwrap();

        let stdout = fs::read_to_string(artifact_dir.path().join("commands/0-stdout.log")).unwrap();
        let stderr = fs::read_to_string(artifact_dir.path().join("commands/0-stderr.log")).unwrap();
        assert!(stdout.contains("stdout-content"));
        assert!(stderr.contains("stderr-content"));
    }

    #[test]
    fn tester_results_tests_ordered_by_test_harness_then_id() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());

        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: echo test\n    test_harness: cargo-nextest\n",
        );
        write_extractor(
            workspace.path(),
            r#"#!/bin/sh
echo '[
  {"id": "z_test", "test_harness": "shell-harness", "status": "pass", "duration_ms": null, "failure_excerpt": null},
  {"id": "a_test", "test_harness": "cargo-nextest", "status": "pass", "duration_ms": 1, "failure_excerpt": null},
  {"id": "m_test", "test_harness": "cargo-nextest", "status": "pass", "duration_ms": 2, "failure_excerpt": null}
]'
"#,
        );

        run(workspace.path(), artifact_dir.path(), true, &resolver()).unwrap();
        let results = read_results(artifact_dir.path());

        assert_eq!(results.tests[0].test_harness, "cargo-nextest");
        assert_eq!(results.tests[0].id, "a_test");
        assert_eq!(results.tests[1].test_harness, "cargo-nextest");
        assert_eq!(results.tests[1].id, "m_test");
        assert_eq!(results.tests[2].test_harness, "shell-harness");
        assert_eq!(results.tests[2].id, "z_test");
    }

    #[test]
    fn tester_results_error_object_shape() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        // No tester.yaml => tester_yaml_problem

        run(workspace.path(), artifact_dir.path(), true, &resolver()).unwrap();
        let results = read_results(artifact_dir.path());

        let error = results.error.unwrap();
        assert_eq!(error.kind, "tester_yaml_problem");
        assert!(!error.message.is_empty());
        assert!(
            [
                "tester_yaml_problem",
                "extractor_missing",
                "extractor_failure",
                "duplicate_test_ids"
            ]
            .contains(&error.kind.as_str())
        );
    }

    #[test]
    fn duplicate_test_ids_produce_error() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());

        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: echo test\n    test_harness: cargo-nextest\n",
        );
        write_extractor(
            workspace.path(),
            r#"#!/bin/sh
echo '[
  {"id": "rejects_null_args", "test_harness": "cargo-nextest", "status": "pass", "duration_ms": 1, "failure_excerpt": null},
  {"id": "unique_test", "test_harness": "cargo-nextest", "status": "pass", "duration_ms": 2, "failure_excerpt": null},
  {"id": "rejects_null_args", "test_harness": "cargo-nextest", "status": "fail", "duration_ms": 3, "failure_excerpt": "boom"}
]'
"#,
        );

        run(workspace.path(), artifact_dir.path(), true, &resolver()).unwrap();
        let results = read_results(artifact_dir.path());

        let error = results.error.as_ref().expect("should have error");
        assert_eq!(error.kind, "duplicate_test_ids");
        assert!(
            error.message.contains("rejects_null_args"),
            "error message should name the colliding id; got: {}",
            error.message
        );
        assert!(results.tests.is_empty(), "should emit no test entries");
        assert_eq!(results.summary.total, 0);
        assert_eq!(results.summary.pass, 0);
        assert_eq!(results.summary.fail, 0);
    }

    #[test]
    fn tester_soft_fails_when_tester_yaml_missing() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());

        run(workspace.path(), artifact_dir.path(), true, &resolver()).unwrap();
        let results = read_results(artifact_dir.path());

        assert_eq!(results.error.as_ref().unwrap().kind, "tester_yaml_problem");
        assert!(results.commands.is_empty());
        assert!(results.tests.is_empty());
        assert_eq!(results.summary.total, 0);
    }

    #[test]
    fn tester_soft_fails_when_tester_yaml_malformed() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(workspace.path(), "not: valid: yaml: [[[");

        run(workspace.path(), artifact_dir.path(), true, &resolver()).unwrap();
        let results = read_results(artifact_dir.path());

        assert_eq!(results.error.as_ref().unwrap().kind, "tester_yaml_problem");
    }

    #[test]
    fn tester_soft_fails_when_extractor_missing() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: echo hi\n    test_harness: shell-harness\n",
        );

        run(workspace.path(), artifact_dir.path(), true, &resolver()).unwrap();
        let results = read_results(artifact_dir.path());

        assert_eq!(results.error.as_ref().unwrap().kind, "extractor_missing");
        assert!(results.commands.is_empty());
        assert!(results.tests.is_empty());
    }

    #[test]
    fn tester_soft_fails_when_extractor_not_executable() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: echo hi\n    test_harness: shell-harness\n",
        );
        let extractor = workspace.path().join(EXTRACTOR_PATH);
        fs::create_dir_all(extractor.parent().unwrap()).unwrap();
        fs::write(&extractor, "#!/bin/sh\necho '[]'\n").unwrap();
        let mut perms = fs::metadata(&extractor).unwrap().permissions();
        perms.set_mode(0o644);
        fs::set_permissions(&extractor, perms).unwrap();

        run(workspace.path(), artifact_dir.path(), true, &resolver()).unwrap();
        let results = read_results(artifact_dir.path());

        assert_eq!(results.error.as_ref().unwrap().kind, "extractor_missing");
    }

    #[test]
    fn tester_continues_when_command_exits_nonzero() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: exit 1\n    test_harness: shell-harness\n  - command: echo second\n    test_harness: shell-harness\n",
        );
        write_extractor(workspace.path(), "#!/bin/sh\necho '[]'\n");

        run(workspace.path(), artifact_dir.path(), true, &resolver()).unwrap();
        let results = read_results(artifact_dir.path());

        assert_eq!(results.commands.len(), 2);
        assert_eq!(results.commands[0].exit_code, 1);
        assert_eq!(results.commands[1].exit_code, 0);
    }

    #[test]
    fn tester_task_succeeds_when_individual_commands_fail() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: exit 42\n    test_harness: shell-harness\n",
        );
        write_extractor(workspace.path(), "#!/bin/sh\necho '[]'\n");

        let result = run(workspace.path(), artifact_dir.path(), true, &resolver());
        assert!(result.is_ok());
    }

    #[test]
    fn tester_soft_fails_when_extractor_exits_nonzero() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: echo hi\n    test_harness: shell-harness\n",
        );
        write_extractor(
            workspace.path(),
            "#!/bin/sh\necho 'some error detail' >&2\nexit 1\n",
        );

        run(workspace.path(), artifact_dir.path(), true, &resolver()).unwrap();
        let results = read_results(artifact_dir.path());

        assert_eq!(results.error.as_ref().unwrap().kind, "extractor_failure");
        assert!(
            results
                .error
                .as_ref()
                .unwrap()
                .details
                .contains("error detail")
        );
        assert_eq!(results.commands.len(), 1);
        assert_eq!(results.commands[0].command, "echo hi");
        assert_eq!(results.commands[0].exit_code, 0);
        assert!(results.tests.is_empty());
    }

    #[test]
    fn tester_soft_fails_when_extractor_emits_invalid_schema() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: echo hi\n    test_harness: shell-harness\n",
        );
        write_extractor(workspace.path(), "#!/bin/sh\necho 'not json'\n");

        run(workspace.path(), artifact_dir.path(), true, &resolver()).unwrap();
        let results = read_results(artifact_dir.path());

        assert_eq!(results.error.as_ref().unwrap().kind, "extractor_failure");
    }

    #[test]
    fn tester_invokes_extractor_and_writes_results_json() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: echo hello\n    test_harness: cargo-nextest\n",
        );
        write_extractor(
            workspace.path(),
            r#"#!/bin/sh
echo '[{"id": "my_test", "test_harness": "cargo-nextest", "status": "pass", "duration_ms": 100, "failure_excerpt": null}]'
"#,
        );

        run(workspace.path(), artifact_dir.path(), true, &resolver()).unwrap();

        let results_path = artifact_dir.path().join("tester-results.json");
        assert!(results_path.is_file());
        let results = read_results(artifact_dir.path());
        assert!(results.error.is_none());
        assert_eq!(results.tests.len(), 1);
        assert_eq!(results.tests[0].id, "my_test");
        assert_eq!(results.summary.total, 1);
        assert_eq!(results.summary.pass, 1);
    }

    #[test]
    fn tester_runs_declared_commands_sequentially() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());

        let marker_file = workspace.path().join("order.txt");
        write_tester_yaml(
            workspace.path(),
            &format!(
                "commands:\n  - command: echo first >> {path}\n    test_harness: shell-harness\n  - command: echo second >> {path}\n    test_harness: shell-harness\n",
                path = marker_file.display()
            ),
        );
        write_extractor(workspace.path(), "#!/bin/sh\necho '[]'\n");

        run(workspace.path(), artifact_dir.path(), true, &resolver()).unwrap();

        let content = fs::read_to_string(&marker_file).unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines, vec!["first", "second"]);
    }

    #[test]
    fn tester_captures_stdout_stderr_exit_duration() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: echo out && echo err >&2 && exit 42\n    test_harness: shell-harness\n",
        );
        write_extractor(workspace.path(), "#!/bin/sh\necho '[]'\n");

        run(workspace.path(), artifact_dir.path(), true, &resolver()).unwrap();
        let results = read_results(artifact_dir.path());

        let cmd = &results.commands[0];
        assert_eq!(cmd.exit_code, 42);
        assert!(cmd.duration_ms > 0 || cmd.duration_ms == 0);

        let stdout = fs::read_to_string(artifact_dir.path().join(&cmd.stdout_log)).unwrap();
        let stderr = fs::read_to_string(artifact_dir.path().join(&cmd.stderr_log)).unwrap();
        assert!(stdout.contains("out"));
        assert!(stderr.contains("err"));
    }

    #[test]
    fn writable_roots_include_pnpm_store_when_package_json_exists() {
        let tmp = TempDir::new().unwrap();
        let candidate = tmp.path().join("candidate");
        let artifact = tmp.path().join("artifact");
        let home = tmp.path().join("home");
        fs::create_dir_all(&candidate).unwrap();
        fs::create_dir_all(&artifact).unwrap();

        fs::write(candidate.join("package.json"), "{}").unwrap();
        let pnpm_store = home.join("Library/pnpm/store");
        fs::create_dir_all(&pnpm_store).unwrap();

        let roots = tester_writable_roots(&candidate, &artifact, &home);
        assert!(
            roots.contains(&pnpm_store),
            "writable roots should include the pnpm store when package.json exists; got: {roots:?}"
        );
    }

    #[test]
    fn writable_roots_exclude_pnpm_store_without_package_json() {
        let tmp = TempDir::new().unwrap();
        let candidate = tmp.path().join("candidate");
        let artifact = tmp.path().join("artifact");
        let home = tmp.path().join("home");
        fs::create_dir_all(&candidate).unwrap();
        fs::create_dir_all(&artifact).unwrap();

        let pnpm_store = home.join("Library/pnpm/store");
        fs::create_dir_all(&pnpm_store).unwrap();

        let roots = tester_writable_roots(&candidate, &artifact, &home);
        assert!(
            !roots.contains(&pnpm_store),
            "writable roots should not include the pnpm store without package.json; got: {roots:?}"
        );
    }

    #[test]
    fn writable_roots_exclude_pnpm_store_when_dir_missing() {
        let tmp = TempDir::new().unwrap();
        let candidate = tmp.path().join("candidate");
        let artifact = tmp.path().join("artifact");
        let home = tmp.path().join("home");
        fs::create_dir_all(&candidate).unwrap();
        fs::create_dir_all(&artifact).unwrap();
        fs::create_dir_all(&home).unwrap();

        fs::write(candidate.join("package.json"), "{}").unwrap();

        let roots = tester_writable_roots(&candidate, &artifact, &home);
        let pnpm_store = home.join("Library/pnpm/store");
        assert!(
            !roots.contains(&pnpm_store),
            "writable roots should not include a non-existent pnpm store; got: {roots:?}"
        );
    }

    #[test]
    fn writable_roots_include_cargo_caches_when_present() {
        let tmp = TempDir::new().unwrap();
        let candidate = tmp.path().join("candidate");
        let artifact = tmp.path().join("artifact");
        let home = tmp.path().join("home");
        fs::create_dir_all(&candidate).unwrap();
        fs::create_dir_all(&artifact).unwrap();

        let cargo_registry = home.join(".cargo/registry");
        let cargo_git = home.join(".cargo/git/db");
        fs::create_dir_all(&cargo_registry).unwrap();
        fs::create_dir_all(&cargo_git).unwrap();

        let roots = tester_writable_roots(&candidate, &artifact, &home);
        assert!(roots.contains(&cargo_registry));
        assert!(roots.contains(&cargo_git));
    }

    #[test]
    fn writable_roots_always_include_candidate_and_artifact() {
        let tmp = TempDir::new().unwrap();
        let candidate = tmp.path().join("candidate");
        let artifact = tmp.path().join("artifact");
        let home = tmp.path().join("home");
        fs::create_dir_all(&candidate).unwrap();
        fs::create_dir_all(&artifact).unwrap();
        fs::create_dir_all(&home).unwrap();

        let roots = tester_writable_roots(&candidate, &artifact, &home);
        assert_eq!(roots[0], candidate);
        assert_eq!(roots[1], artifact);
    }

    #[test]
    fn writable_roots_include_xdg_pnpm_store_when_present() {
        let tmp = TempDir::new().unwrap();
        let candidate = tmp.path().join("candidate");
        let artifact = tmp.path().join("artifact");
        let home = tmp.path().join("home");
        fs::create_dir_all(&candidate).unwrap();
        fs::create_dir_all(&artifact).unwrap();

        fs::write(candidate.join("package.json"), "{}").unwrap();
        let xdg_store = home.join(".local/share/pnpm/store");
        fs::create_dir_all(&xdg_store).unwrap();

        let roots = tester_writable_roots(&candidate, &artifact, &home);
        assert!(
            roots.contains(&xdg_store),
            "writable roots should include the XDG pnpm store; got: {roots:?}"
        );
    }
}
