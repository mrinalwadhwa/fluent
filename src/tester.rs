use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use crate::content::ContentResolver;
use crate::os;
use crate::process_settlement::{
    HostProcessInventoryCollector, ProcessInventoryCollector, ProcessSettlement,
};

const TESTER_YAML_PATH: &str = ".fluent/tester.yaml";
const EXTRACTOR_PATH: &str = ".fluent/extract-tester-results";
const FAILURE_EXCERPT_MAX: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TesterResults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_commit: Option<String>,
    pub commands: Vec<CommandResult>,
    pub tests: Vec<TestResult>,
    pub summary: Summary,
    pub error: Option<TesterError>,
}

impl TesterResults {
    pub(crate) fn needs_failure_diagnosis(&self) -> bool {
        self.error.is_none()
            && !self.tests.iter().any(|test| test.status == "fail")
            && self.commands.iter().any(|command| command.exit_code != 0)
    }
}

pub const FAILED_COMMAND_RECHECK_SCHEMA_VERSION: u32 = 1;

/// Host-owned identity that maps one subset run back to the failed commands in
/// its source final-Tester result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedCommandRecheckManifest {
    pub schema_version: u32,
    pub candidate_commit: String,
    pub source_results: String,
    pub failed_command_indexes: Vec<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_cause: Option<CommandFailureCause>,
    pub command: String,
    pub test_harness: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub stdout_log: String,
    pub stderr_log: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_receipt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_receipt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_settlement: Option<ProcessSettlementEvidence>,
}

/// Preserve diagnosed cause separately from the command's failed result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CommandFailureCause {
    Unclassified,
    /// The executor observed a platform mismatch and did not launch the command.
    UnmetPrerequisite {
        fact: String,
        required: String,
        observed: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessSettlementEvidence {
    pub settlement_directory: String,
    pub leaks: Vec<ProcessLeak>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessLeak {
    pub pid: u32,
    pub kind: String,
    pub settlement_directory: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<crate::verification_reference::NativeTestIdentity>,
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

/// Return whether persisted harness evidence requires a tracked project Tester
/// repair rather than an environment-only retry of the unchanged candidate.
pub fn harness_error_requires_project_repair(artifact_dir: &Path) -> bool {
    let Ok(bytes) = fs::read(artifact_dir.join("tester-results.json")) else {
        return false;
    };
    let Ok(results) = serde_json::from_slice::<TesterResults>(&bytes) else {
        return false;
    };
    results.error.as_ref().is_some_and(|error| {
        matches!(
            error.kind.as_str(),
            "extractor_missing" | "duplicate_test_ids"
        )
    })
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TesterConfig {
    pub(crate) commands: Vec<TesterCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TesterCommand {
    #[serde(
        default,
        skip_serializing_if = "crate::work_model::CheckApplicability::is_default"
    )]
    pub(crate) applicability: crate::work_model::CheckApplicability,
    pub(crate) command: String,
    pub(crate) test_harness: String,
    #[serde(default)]
    pub(crate) reject_process_leaks: bool,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_tester_permissions"
    )]
    pub(crate) permissions: Vec<TesterPermission>,
}

/// A request in project configuration never grants host authority by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum TesterPermission {
    ProcessTableRead,
}

fn deserialize_tester_permissions<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<TesterPermission>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let permissions = Vec::<TesterPermission>::deserialize(deserializer)?;
    let mut seen = std::collections::HashSet::new();
    for permission in &permissions {
        if !seen.insert(*permission) {
            return Err(serde::de::Error::custom("duplicate Tester permission"));
        }
    }
    Ok(permissions)
}

/// Capture the complete configured command list before a canonical execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CanonicalRunManifest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environment: Vec<std::collections::BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applicability: Vec<crate::work_model::ResolvedCheckApplicability>,
    pub schema_version: u32,
    pub config_digest: String,
    pub extractor_digest: String,
    pub commands: Vec<TesterCommand>,
    pub full_run: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub original_command_indexes: Vec<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permission_receipts: Vec<Option<String>>,
}

/// Read-only Tester configuration used to defer filesystem setup until a Task
/// has durably reserved permission to start.
#[derive(Debug, Clone)]
pub struct TesterPlan {
    commands: Vec<TesterCommand>,
    original_indexes: Vec<usize>,
}

/// Read Tester configuration without creating an artifact or settlement path.
pub fn plan(candidate_workspace: &Path) -> Result<TesterPlan> {
    let config = read_tester_config(&candidate_workspace.join(TESTER_YAML_PATH))?;
    let original_indexes = (0..config.commands.len()).collect();
    Ok(TesterPlan {
        commands: config.commands,
        original_indexes,
    })
}

/// The sandbox profile and host-owned settlement paths prepared for one Tester run.
pub struct TesterSandbox {
    profile: Option<os::SandboxProfile>,
    command_profiles: Vec<Option<os::SandboxProfile>>,
    candidate_commit: Option<String>,
    original_indexes: Vec<usize>,
    settlement_directories: Vec<Option<PathBuf>>,
    commands: Vec<TesterCommand>,
    launch_env: Vec<(String, String)>,
}

/// Canonical settlement paths created for an authorized Tester Task.
pub struct TesterSettlement {
    settlement_directories: Vec<Option<PathBuf>>,
    commands: Vec<TesterCommand>,
    original_indexes: Vec<usize>,
}

impl TesterSandbox {
    fn profile(&self) -> Option<&os::SandboxProfile> {
        self.profile.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn settlement_directory(&self, index: usize) -> Option<PathBuf> {
        self.settlement_directories.get(index).cloned().flatten()
    }
}

/// Build the immutable Tester plan for only the commands that failed in a
/// prior final-Tester result. The current candidate configuration remains the
/// execution authority: every selected command must still occupy the same
/// index with the same command and harness before Fluent can recheck it.
pub fn plan_failed_commands(
    candidate_workspace: &Path,
    source_results: &TesterResults,
) -> Result<(TesterPlan, Vec<usize>)> {
    let current = read_tester_config(&candidate_workspace.join(TESTER_YAML_PATH))?;
    let failed_indexes = source_results
        .commands
        .iter()
        .enumerate()
        .filter_map(|(index, command)| (command.exit_code != 0).then_some(index))
        .collect::<Vec<_>>();
    if failed_indexes.is_empty() {
        anyhow::bail!("failed-command recheck source has no nonzero command exits");
    }

    let mut commands = Vec::with_capacity(failed_indexes.len());
    for index in &failed_indexes {
        let source = &source_results.commands[*index];
        let configured = current.commands.get(*index).ok_or_else(|| {
            anyhow::anyhow!(
                "failed-command recheck source index {index} is absent from current {TESTER_YAML_PATH}"
            )
        })?;
        if configured.command != source.command || configured.test_harness != source.test_harness {
            anyhow::bail!(
                "failed-command recheck source index {index} no longer matches current {TESTER_YAML_PATH}"
            );
        }
        commands.push(configured.clone());
    }

    Ok((
        TesterPlan {
            commands,
            original_indexes: failed_indexes.clone(),
        },
        failed_indexes,
    ))
}

/// Build a recheck plan from a prior failed-command recheck result.
///
/// A recheck result contains only the commands selected from its source
/// Tester, so its local indexes no longer refer to the full current Tester
/// configuration. Match each failed command by its exact command text and
/// harness, using each configured entry at most once. The ordinary
/// `plan_failed_commands` path remains index-strict for the first recheck.
pub(crate) fn plan_chained_failed_commands(
    candidate_workspace: &Path,
    source_results: &TesterResults,
) -> Result<(TesterPlan, Vec<usize>)> {
    let current = read_tester_config(&candidate_workspace.join(TESTER_YAML_PATH))?;
    let failed_indexes = source_results
        .commands
        .iter()
        .enumerate()
        .filter_map(|(index, command)| (command.exit_code != 0).then_some(index))
        .collect::<Vec<_>>();
    if failed_indexes.is_empty() {
        anyhow::bail!("failed-command recheck source has no nonzero command exits");
    }

    let mut used = std::collections::BTreeSet::new();
    let mut commands = Vec::with_capacity(failed_indexes.len());
    let mut original_indexes = Vec::with_capacity(failed_indexes.len());
    for index in &failed_indexes {
        let source = &source_results.commands[*index];
        let matches = current
            .commands
            .iter()
            .enumerate()
            .filter(|(configured_index, configured)| {
                !used.contains(configured_index)
                    && configured.command == source.command
                    && configured.test_harness == source.test_harness
            })
            .collect::<Vec<_>>();
        let [(configured_index, configured)] = matches.as_slice() else {
            anyhow::bail!(
                "failed-command recheck source command {index} does not uniquely match current {TESTER_YAML_PATH}"
            );
        };
        used.insert(*configured_index);
        original_indexes.push(*configured_index);
        commands.push((*configured).clone());
    }

    Ok((
        TesterPlan {
            commands,
            original_indexes,
        },
        failed_indexes,
    ))
}

/// Return whether a path uses the host-owned Tester command-log spelling.
pub(crate) fn is_command_log_path(path: &Path) -> bool {
    if !path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == "commands")
    {
        return false;
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    ["-stdout.log", "-stderr.log"].iter().any(|suffix| {
        name.strip_suffix(suffix).is_some_and(|index| {
            !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
        })
    })
}

fn tester_writable_roots(candidate: &Path, artifact: &Path, home: &Path) -> Vec<PathBuf> {
    tester_writable_roots_with_cargo_access(candidate, artifact, home, true)
}

fn tester_writable_roots_with_cargo_access(
    candidate: &Path,
    artifact: &Path,
    home: &Path,
    include_cargo_stores: bool,
) -> Vec<PathBuf> {
    let mut roots = vec![candidate.to_path_buf(), artifact.to_path_buf()];

    if include_cargo_stores {
        let cargo_registry = home.join(".cargo/registry");
        let cargo_git = home.join(".cargo/git/db");
        if cargo_registry.is_dir() {
            roots.push(cargo_registry);
        }
        if cargo_git.is_dir() {
            roots.push(cargo_git);
        }
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

/// Resolve the Git metadata a candidate's test commands may inspect but never
/// mutate. Linked worktrees keep this directory outside the candidate root, so
/// granting only the checkout would make ordinary `git` discovery fail inside
/// the Tester sandbox.
fn tester_git_metadata_root(candidate: &Path) -> Result<PathBuf> {
    let common = crate::worktree::git_common_dir(candidate)?;
    fs::canonicalize(&common)
        .with_context(|| format!("canonicalize Tester Git metadata {}", common.display()))
}

pub fn run(
    candidate_workspace: &Path,
    artifact_dir: &Path,
    no_sandbox: bool,
    resolver: &ContentResolver,
) -> Result<TesterOutcome> {
    let plan = match plan(candidate_workspace) {
        Ok(plan) => plan,
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
    let sandbox = prepare_sandbox_profile(
        candidate_workspace,
        artifact_dir,
        no_sandbox,
        resolver,
        &plan,
    )?;
    run_with_sandbox_profile(candidate_workspace, artifact_dir, &sandbox)
}

/// Run the selected checkout through the production Tester boundary after first
/// collecting every independent structural readiness problem.
pub fn check(
    candidate_workspace: &Path,
    no_sandbox: bool,
    resolver: &ContentResolver,
) -> Result<TesterOutcome> {
    let artifact = tempfile::tempdir().context("creating Tester readiness artifact")?;
    // Collect independent configuration and extractor diagnostics, but never
    // execute candidate-owned readiness code with the host's authority.
    let readiness = prepare_readiness_profile(candidate_workspace, artifact.path(), no_sandbox)?;
    let problems = structural_problems(candidate_workspace, artifact.path(), readiness.as_ref());
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
    let plan = plan(candidate_workspace)?;
    let sandbox = prepare_sandbox_profile(
        candidate_workspace,
        artifact.path(),
        no_sandbox,
        resolver,
        &plan,
    )?;
    run_with_sandbox_profile(candidate_workspace, artifact.path(), &sandbox)
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
    let readiness = prepare_readiness_profile(candidate_workspace, artifact.path(), no_sandbox)?;
    let problems = structural_problems(candidate_workspace, artifact.path(), readiness.as_ref());
    if !problems.is_empty() {
        anyhow::bail!(
            "Tester is not structurally ready:\n{}",
            problems
                .into_iter()
                .map(|p| format!("- {p}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
    let plan = plan(candidate_workspace)?;
    let _sandbox = prepare_sandbox_profile(
        candidate_workspace,
        artifact.path(),
        no_sandbox,
        resolver,
        &plan,
    )?;
    Ok(())
}

/// Prepare the host-owned Tester paths, then render and preflight the production
/// sandbox boundary without launching a test command. Callers pass the returned
/// boundary to `run_with_sandbox_profile` so execution uses the exact prepared paths.
pub fn preflight_sandbox_profile(
    candidate_workspace: &Path,
    artifact_dir: &Path,
    no_sandbox: bool,
    resolver: &ContentResolver,
) -> Result<TesterSandbox> {
    let plan = plan(candidate_workspace)?;
    prepare_sandbox_profile(
        candidate_workspace,
        artifact_dir,
        no_sandbox,
        resolver,
        &plan,
    )
}

/// Create and canonicalize guarded settlement paths after start authorization,
/// then render and preflight the sandbox that grants access to those paths.
pub fn prepare_sandbox_profile(
    candidate_workspace: &Path,
    artifact_dir: &Path,
    no_sandbox: bool,
    resolver: &ContentResolver,
    plan: &TesterPlan,
) -> Result<TesterSandbox> {
    let settlement = prepare_settlement_directories(artifact_dir, plan)?;
    render_sandbox_profile(
        candidate_workspace,
        artifact_dir,
        no_sandbox,
        resolver,
        settlement,
    )
}

/// Create and canonicalize guarded settlement directories for an authorized
/// Tester Task. This performs no sandbox rendering or host preflight.
pub fn prepare_settlement_directories(
    artifact_dir: &Path,
    plan: &TesterPlan,
) -> Result<TesterSettlement> {
    prepare_settlement_directories_with(artifact_dir, plan, ProcessSettlement::prepare_directory)
}

fn prepare_settlement_directories_with(
    artifact_dir: &Path,
    plan: &TesterPlan,
    mut prepare_directory: impl FnMut(PathBuf) -> Result<PathBuf>,
) -> Result<TesterSettlement> {
    let root = artifact_dir.join("commands/settlement");
    let created_parents = [artifact_dir.join("commands"), root.clone()]
        .into_iter()
        .filter(|path| !path.exists())
        .collect::<Vec<_>>();
    let mut settlement_directories = Vec::with_capacity(plan.commands.len());
    let mut created = Vec::new();
    for (index, command) in plan.commands.iter().enumerate() {
        if !command.reject_process_leaks {
            settlement_directories.push(None);
            continue;
        }
        let directory = root.join(index.to_string());
        let existed = directory.exists();
        match prepare_directory(directory.clone()) {
            Ok(directory) => {
                if !existed {
                    created.push(directory.clone());
                }
                settlement_directories.push(Some(directory));
            }
            Err(error) => {
                for created_directory in created.into_iter().rev() {
                    let _ = fs::remove_dir_all(created_directory);
                }
                for created_parent in created_parents.into_iter().rev() {
                    let _ = fs::remove_dir(created_parent);
                }
                return Err(error);
            }
        }
    }
    Ok(TesterSettlement {
        settlement_directories,
        commands: plan.commands.clone(),
        original_indexes: plan.original_indexes.clone(),
    })
}

/// Render and preflight the sandbox that grants a prepared settlement.
pub fn render_sandbox_profile(
    candidate_workspace: &Path,
    artifact_dir: &Path,
    no_sandbox: bool,
    resolver: &ContentResolver,
    settlement: TesterSettlement,
) -> Result<TesterSandbox> {
    render_sandbox_profile_with_cache(
        candidate_workspace,
        artifact_dir,
        no_sandbox,
        resolver,
        settlement,
        None,
    )
}

/// Render the Tester boundary with the Attempt-scoped dependency cache shared
/// by the Writer rounds and final Tester for this Attempt.
pub(crate) fn render_sandbox_profile_with_cache(
    candidate_workspace: &Path,
    artifact_dir: &Path,
    no_sandbox: bool,
    resolver: &ContentResolver,
    settlement: TesterSettlement,
    dependency_cache: Option<&crate::writer_cache::WriterDependencyCache>,
) -> Result<TesterSandbox> {
    let TesterSettlement {
        settlement_directories,
        commands,
        original_indexes,
    } = settlement;
    let has_permission_request = commands
        .iter()
        .any(|command| !command.permissions.is_empty());
    check_requested_permissions(
        candidate_workspace,
        no_sandbox,
        &commands,
        &original_indexes,
    )?;
    let candidate_commit = if has_permission_request {
        Some(crate::git::run_stdout(
            candidate_workspace,
            &["rev-parse", "HEAD"],
            "resolve Tester candidate",
        )?)
    } else {
        None
    };
    let launch_env = dependency_cache
        .map(|cache| cache.launch_env())
        .unwrap_or_default();
    if no_sandbox {
        return Ok(TesterSandbox {
            profile: None,
            command_profiles: Vec::new(),
            candidate_commit,
            original_indexes,
            settlement_directories,
            commands,
            launch_env,
        });
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
        return Ok(TesterSandbox {
            profile: None,
            command_profiles: Vec::new(),
            candidate_commit,
            original_indexes,
            settlement_directories,
            commands,
            launch_env,
        });
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let candidate_abs =
        fs::canonicalize(candidate_workspace).unwrap_or_else(|_| candidate_workspace.to_path_buf());
    let artifact_abs =
        fs::canonicalize(artifact_dir).unwrap_or_else(|_| artifact_dir.to_path_buf());
    let mut writable_roots = match dependency_cache {
        Some(_) => tester_writable_roots_with_cargo_access(
            &candidate_abs,
            &artifact_abs,
            Path::new(&home),
            false,
        ),
        None => tester_writable_roots(&candidate_abs, &artifact_abs, Path::new(&home)),
    };
    if let Some(cache) = dependency_cache {
        writable_roots.push(cache.root().to_path_buf());
    }
    writable_roots.extend(settlement_directories.iter().flatten().cloned());
    let common_git_dir = tester_git_metadata_root(&candidate_abs)?;
    let rendered = if has_permission_request {
        let mut forbidden_roots = writable_roots.clone();
        forbidden_roots.push(common_git_dir.clone());
        let protected = crate::tester_permissions::protected_profile_directory(&forbidden_roots)?;
        os::render_protected_tester_profile(
            &home,
            &writable_roots,
            std::slice::from_ref(&common_git_dir),
            std::slice::from_ref(&common_git_dir),
            &protected,
        )
        .and_then(|baseline| {
            let selected = render_command_profiles(&commands, &baseline, &protected)?;
            Ok((baseline, selected))
        })
    } else {
        os::render_profile_common_only_with_denied_access(
            resolver,
            &home,
            &writable_roots,
            std::slice::from_ref(&common_git_dir),
            &[],
            std::slice::from_ref(&common_git_dir),
        )
        .map(|profile| (profile, Vec::new()))
    };
    match rendered {
        Ok((profile, command_profiles)) => {
            os::preflight_profile(&profile)?;
            for selected in command_profiles.iter().flatten() {
                os::preflight_profile(selected)?;
            }
            Ok(TesterSandbox {
                profile: Some(profile),
                command_profiles,
                candidate_commit,
                original_indexes,
                settlement_directories,
                commands,
                launch_env,
            })
        }
        Err(err) => Err(err),
    }
}

fn render_command_profiles(
    commands: &[TesterCommand],
    baseline: &os::SandboxProfile,
    protected_directory: &Path,
) -> Result<Vec<Option<os::SandboxProfile>>> {
    commands
        .iter()
        .map(|command| {
            if command
                .permissions
                .contains(&TesterPermission::ProcessTableRead)
            {
                os::protect_process_table_profile(baseline, protected_directory).map(Some)
            } else {
                Ok(None)
            }
        })
        .collect()
}

fn check_requested_permissions(
    candidate_workspace: &Path,
    no_sandbox: bool,
    commands: &[TesterCommand],
    original_indexes: &[usize],
) -> Result<()> {
    anyhow::ensure!(
        commands.len() == original_indexes.len(),
        "Tester command identity mapping is incomplete"
    );
    if commands
        .iter()
        .all(|command| command.permissions.is_empty())
    {
        return Ok(());
    }
    anyhow::ensure!(!no_sandbox, "process-table-read requires the macOS sandbox");
    anyhow::ensure!(
        cfg!(target_os = "macos"),
        "process-table-read requires macOS"
    );
    let candidate = crate::git::run_stdout(
        candidate_workspace,
        &["rev-parse", "HEAD"],
        "resolve Tester candidate",
    )?;
    for (command, original_index) in commands.iter().zip(original_indexes) {
        if command
            .permissions
            .contains(&TesterPermission::ProcessTableRead)
        {
            crate::tester_permissions::check_current_approval(
                candidate_workspace,
                &candidate,
                *original_index,
            )?;
        }
    }
    Ok(())
}

pub fn run_with_sandbox_profile(
    candidate_workspace: &Path,
    artifact_dir: &Path,
    sandbox: &TesterSandbox,
) -> Result<TesterOutcome> {
    run_with_prepared_sandbox_profile_and_collector(
        candidate_workspace,
        artifact_dir,
        sandbox.profile(),
        Some(&sandbox.command_profiles),
        sandbox
            .candidate_commit
            .as_deref()
            .map(|candidate| (candidate, sandbox.original_indexes.as_slice())),
        &sandbox.commands,
        Some(&sandbox.settlement_directories),
        &sandbox.launch_env,
        &HostProcessInventoryCollector,
    )
}

fn run_with_prepared_sandbox_profile_and_collector(
    candidate_workspace: &Path,
    artifact_dir: &Path,
    sandbox_profile: Option<&os::SandboxProfile>,
    command_profiles: Option<&[Option<os::SandboxProfile>]>,
    admission: Option<(&str, &[usize])>,
    commands: &[TesterCommand],
    settlement_directories: Option<&[Option<PathBuf>]>,
    launch_env: &[(String, String)],
    collector: &dyn ProcessInventoryCollector,
) -> Result<TesterOutcome> {
    record_test_boundary(sandbox_profile.is_some());
    if commands
        .iter()
        .any(|command| !command.permissions.is_empty())
    {
        anyhow::ensure!(
            command_profiles.is_some_and(|selected| {
                selected.len() == commands.len()
                    && commands.iter().zip(selected).all(|(command, profile)| {
                        command.permissions.is_empty() || profile.is_some()
                    })
            }),
            "permission-bearing Tester commands lack per-command profiles"
        );
        anyhow::ensure!(
            admission.is_some_and(|(_, indexes)| indexes.len() == commands.len()),
            "permission-bearing Tester commands lack pinned admission identity"
        );
    }
    let extractor_path = candidate_workspace.join(EXTRACTOR_PATH);
    if !extractor_path.is_file() || !is_executable(&extractor_path) {
        return persist_harness_error(
            artifact_dir,
            "extractor_missing",
            format!("{EXTRACTOR_PATH} missing or not executable"),
            String::new(),
            Vec::new(),
        );
    }

    use sha2::{Digest, Sha256};
    let configured = read_tester_config(&candidate_workspace.join(TESTER_YAML_PATH))?;
    let has_permission_request = commands
        .iter()
        .any(|command| !command.permissions.is_empty());
    let manifest = CanonicalRunManifest {
        run_at: Some(crate::work_model::now_iso8601()),
        environment: commands
            .iter()
            .map(|command| {
                command
                    .applicability
                    .environment
                    .keys()
                    .filter_map(|key| {
                        let value = match key.as_str() {
                            "os" => std::env::consts::OS,
                            "architecture" => std::env::consts::ARCH,
                            _ => return None,
                        };
                        Some((key.clone(), value.to_string()))
                    })
                    .collect()
            })
            .collect(),
        applicability: commands
            .iter()
            .map(|command| {
                crate::check_applicability::resolve_check_applicability(
                    candidate_workspace,
                    &command.applicability,
                )
            })
            .collect::<Result<Vec<_>>>()?,
        schema_version: if has_permission_request { 2 } else { 1 },
        config_digest: format!(
            "sha256:{:x}",
            Sha256::digest(fs::read(candidate_workspace.join(TESTER_YAML_PATH))?)
        ),
        extractor_digest: format!("sha256:{:x}", Sha256::digest(fs::read(&extractor_path)?)),
        full_run: configured.commands == commands,
        commands: commands.to_vec(),
        original_command_indexes: if has_permission_request {
            admission
                .context("permission-bearing run lacks pinned command indexes")?
                .1
                .to_vec()
        } else {
            Vec::new()
        },
        permission_receipts: if has_permission_request {
            vec![None; commands.len()]
        } else {
            Vec::new()
        },
    };
    crate::tester_permissions::write_command_artifact(
        &artifact_dir.join("canonical-run.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;

    let commands_dir = artifact_dir.join("commands");
    fs::create_dir_all(&commands_dir)?;

    if let Some(profile) = sandbox_profile {
        eprintln!("  Sandbox profile  {}", profile.path.display());
    }

    let mut command_results = Vec::new();
    for (index, cmd) in commands.iter().enumerate() {
        if let Some((fact, required, observed)) =
            cmd.applicability
                .environment
                .iter()
                .find_map(|(fact, required)| {
                    let observed = manifest.environment.get(index)?.get(fact)?;
                    (observed != required)
                        .then(|| (fact.clone(), required.clone(), observed.clone()))
                })
        {
            let stdout_log = format!("commands/{index}-stdout.log");
            let stderr_log = format!("commands/{index}-stderr.log");
            crate::tester_permissions::write_command_artifact(
                &artifact_dir.join(&stdout_log),
                b"",
            )?;
            crate::tester_permissions::write_command_artifact(
                &artifact_dir.join(&stderr_log),
                format!(
                    "Required {fact}={required}, observed {observed}; prerequisite prevented execution.\n"
                )
                .as_bytes(),
            )?;
            command_results.push(CommandResult {
                failure_cause: Some(CommandFailureCause::UnmetPrerequisite {
                    fact,
                    required,
                    observed,
                }),
                command: cmd.command.clone(),
                test_harness: cmd.test_harness.clone(),
                exit_code: 125,
                duration_ms: 0,
                stdout_log,
                stderr_log,
                permission_receipt: None,
                permission_receipt_id: None,
                process_settlement: None,
            });
            continue;
        }
        eprintln!("  Running command {}: {}", index, cmd.command);
        let settlement = if cmd.reject_process_leaks {
            let prepared_directory = settlement_directories
                .and_then(|directories| directories.get(index))
                .cloned()
                .flatten();
            let settlement_result = match prepared_directory {
                Some(directory) => ProcessSettlement::begin_prepared(directory, collector),
                None => anyhow::bail!(
                    "guarded Tester command {index} has no prepared settlement directory"
                ),
            };
            match settlement_result {
                Ok(settlement) => Some(settlement),
                Err(error) => {
                    publish_command_records(artifact_dir, &command_results, Some(&manifest))?;
                    return persist_harness_error(
                        artifact_dir,
                        "process_settlement_infrastructure",
                        "Unable to establish process-settlement baseline".to_string(),
                        format!("{error:#}"),
                        command_results,
                    );
                }
            }
        } else {
            None
        };
        let execution = run_command(
            candidate_workspace,
            &cmd.command,
            &cmd.test_harness,
            index,
            &commands_dir,
            command_profiles
                .and_then(|selected| selected.get(index))
                .and_then(Option::as_ref)
                .or(sandbox_profile),
            if cmd.permissions.is_empty() {
                None
            } else {
                admission.map(|(candidate, indexes)| (candidate, indexes[index]))
            },
            launch_env,
            settlement
                .as_ref()
                .map(|settlement| settlement.directory.as_path()),
        );
        let mut result = match execution {
            Ok(result) => result,
            Err(error) => {
                publish_command_records(artifact_dir, &command_results, Some(&manifest))?;
                return persist_harness_error(
                    artifact_dir,
                    "command_launch_admission",
                    format!("Unable to launch Tester command {index}"),
                    format!("{error:#}"),
                    command_results,
                );
            }
        };
        if let Some(settlement) = settlement {
            match settlement.new_processes(collector) {
                Ok(leaks) => {
                    for leak in &leaks {
                        eprintln!(
                            "  Process settlement leak: pid={} kind={} directory={}",
                            leak.pid,
                            leak.kind,
                            leak.settlement_directory.display()
                        );
                    }
                    result.process_settlement = Some(ProcessSettlementEvidence {
                        settlement_directory: settlement.directory.display().to_string(),
                        leaks: leaks
                            .into_iter()
                            .map(|leak| ProcessLeak {
                                pid: leak.pid,
                                kind: leak.kind,
                                settlement_directory: leak
                                    .settlement_directory
                                    .display()
                                    .to_string(),
                            })
                            .collect(),
                    });
                }
                Err(error) => {
                    command_results.push(result);
                    publish_command_records(artifact_dir, &command_results, Some(&manifest))?;
                    return persist_harness_error(
                        artifact_dir,
                        "process_settlement_infrastructure",
                        "Unable to establish post-command process-settlement inventory".to_string(),
                        format!("{error:#}"),
                        command_results,
                    );
                }
            }
        }
        eprintln!(
            "    exit_code={} duration={}ms",
            result.exit_code, result.duration_ms
        );
        command_results.push(result);
    }

    publish_command_records(artifact_dir, &command_results, Some(&manifest))?;

    if let Some(result) = command_results.iter().find(|result| {
        fs::read_to_string(artifact_dir.join(&result.stderr_log))
            .ok()
            .is_some_and(|stderr| is_nested_swiftpm_sandbox_failure(&result.command, &stderr))
    }) {
        let details = format!(
            "Xcode's embedded SwiftPM could not create its manifest sandbox inside Fluent's Tester sandbox while running `{}`. Fluent kept its sandbox enabled and did not modify .fluent/tester.yaml or project scripts. For a trusted direct check from a host shell that is not itself sandboxed, rerun `fluent tester check --no-sandbox`; a queued Work Attempt provides an exact one-run recovery command in its needs-user handoff.",
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

    let (tests, summary) = match extract_and_normalize(
        candidate_workspace,
        &extractor_path,
        artifact_dir,
        sandbox_profile,
        launch_env,
    ) {
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
        candidate_commit: None,
        commands: command_results,
        tests,
        summary,
        error: None,
    };

    write_results(artifact_dir, &results)?;
    Ok(
        if results.summary.fail == 0
            && !results.commands.iter().any(|command| {
                command
                    .process_settlement
                    .as_ref()
                    .is_some_and(|evidence| !evidence.leaks.is_empty())
            })
        {
            TesterOutcome::Passed
        } else {
            TesterOutcome::TestFailures
        },
    )
}

#[cfg(test)]
fn run_with_sandbox_profile_and_collector(
    candidate_workspace: &Path,
    artifact_dir: &Path,
    sandbox_profile: Option<&os::SandboxProfile>,
    settlement_directories: Option<&[Option<PathBuf>]>,
    collector: &dyn ProcessInventoryCollector,
) -> Result<TesterOutcome> {
    let plan = plan(candidate_workspace)?;
    let prepared = if settlement_directories.is_none() {
        Some(prepare_settlement_directories(artifact_dir, &plan)?)
    } else {
        None
    };
    run_with_prepared_sandbox_profile_and_collector(
        candidate_workspace,
        artifact_dir,
        sandbox_profile,
        None,
        None,
        &plan.commands,
        settlement_directories.or_else(|| {
            prepared
                .as_ref()
                .map(|settlement| settlement.settlement_directories.as_slice())
        }),
        &[],
        collector,
    )
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

/// Publish completed outcomes even if a later command cannot be admitted.
/// Leave receipt slots for commands that did not launch empty.
fn publish_command_records(
    artifact_dir: &Path,
    commands: &[CommandResult],
    manifest: Option<&CanonicalRunManifest>,
) -> Result<()> {
    if let Some(manifest) = manifest {
        let mut manifest = manifest.clone();
        if manifest.schema_version == 2 {
            manifest.permission_receipts = (0..manifest.commands.len())
                .map(|index| {
                    commands
                        .get(index)
                        .and_then(|result| result.permission_receipt_id.clone())
                })
                .collect();
            crate::tester_permissions::write_command_artifact(
                &artifact_dir.join("canonical-run.json"),
                &serde_json::to_vec_pretty(&manifest)?,
            )?;
        }
    }
    crate::tester_permissions::write_command_artifact(
        &artifact_dir.join("commands.json"),
        &serde_json::to_vec_pretty(commands)?,
    )
}

pub(crate) fn persist_harness_error(
    artifact_dir: &Path,
    kind: &str,
    message: String,
    details: String,
    commands: Vec<CommandResult>,
) -> Result<TesterOutcome> {
    publish_command_records(artifact_dir, &commands, None)?;
    write_results(
        artifact_dir,
        &TesterResults {
            candidate_commit: None,
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

/// Confine candidate-owned empty-artifact extraction before reading approvals
/// or preparing capability profiles. This baseline always uses bundled policy.
fn prepare_readiness_profile(
    candidate_workspace: &Path,
    artifact_dir: &Path,
    no_sandbox: bool,
) -> Result<Option<os::SandboxProfile>> {
    if no_sandbox {
        if let Ok(config) = read_tester_config(&candidate_workspace.join(TESTER_YAML_PATH)) {
            anyhow::ensure!(
                config
                    .commands
                    .iter()
                    .all(|command| command.permissions.is_empty()),
                "process-table-read requires the macOS sandbox"
            );
        }
        return Ok(None);
    }
    #[cfg(feature = "test-support")]
    if std::env::var("FLUENT_TEST_HOST_SANDBOX_PREFLIGHT").as_deref() == Ok("pass") {
        return Ok(None);
    }
    let candidate = fs::canonicalize(candidate_workspace)?;
    let artifact = fs::canonicalize(artifact_dir)?;
    let home = os::operator_home()?;
    let roots = tester_writable_roots(&candidate, &artifact, &home);
    let common = tester_git_metadata_root(&candidate)?;
    let mut forbidden = roots.clone();
    forbidden.push(common.clone());
    let protected =
        crate::tester_permissions::prepare_readiness_profile_directory(&candidate, &forbidden)?;
    let profile = os::render_protected_tester_profile(
        &home.to_string_lossy(),
        &roots,
        std::slice::from_ref(&common),
        std::slice::from_ref(&common),
        &protected,
    )?;
    os::preflight_profile(&profile)?;
    Ok(Some(profile))
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
        if let Err(error) = crate::tester_permissions::write_command_artifact(
            &artifact_dir.join("commands.json"),
            b"[]",
        ) {
            problems.push(format!("readiness artifact: {error}"));
        } else if let Err(error) =
            extract_and_normalize(candidate_workspace, &extractor, artifact_dir, profile, &[])
        {
            problems.push(format!(
                "{EXTRACTOR_PATH}: invalid empty artifact: {error:#}"
            ));
        }
    }
    problems
}

fn extract_and_normalize(
    candidate_workspace: &Path,
    extractor_path: &Path,
    artifact_dir: &Path,
    sandbox_profile: Option<&os::SandboxProfile>,
    launch_env: &[(String, String)],
) -> Result<(Vec<TestResult>, Summary)> {
    let tests = run_extractor(
        candidate_workspace,
        extractor_path,
        artifact_dir,
        sandbox_profile,
        launch_env,
    )?;
    let duplicate_ids = find_duplicate_ids(&tests);
    if !duplicate_ids.is_empty() {
        return Err(NormalizationError {
            kind: "duplicate_test_ids",
            message: format!("Duplicate test ids: {}", duplicate_ids.join(", ")),
        }
        .into());
    }
    let mut tests = cap_failure_excerpts(tests);
    for test in &mut tests {
        let native = crate::verification_reference::normalize_native_identity(&test.id);
        if test.identity.is_none() {
            test.identity = native;
        }
        crate::verification_reference::normalized_result(test).map_err(|message| {
            NormalizationError {
                kind: "invalid_test_identity",
                message,
            }
        })?;
    }
    tests.sort_by(|a, b| {
        a.test_harness
            .cmp(&b.test_harness)
            .then_with(|| a.id.cmp(&b.id))
    });
    let summary = compute_summary(&tests);
    Ok((tests, summary))
}

pub(crate) fn read_tester_config(path: &Path) -> Result<TesterConfig> {
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
    sandbox_profile: Option<&os::SandboxProfile>,
    admission: Option<(&str, usize)>,
    launch_env: &[(String, String)],
    settlement_directory: Option<&Path>,
) -> Result<CommandResult> {
    let stdout_path = commands_dir.join(format!("{index}-stdout.log"));
    let stderr_path = commands_dir.join(format!("{index}-stderr.log"));

    let start = Instant::now();
    let mut process = if let Some(profile) = sandbox_profile {
        let mut process = os::command_under_profile(profile, Path::new("sh"));
        process.arg("-c").arg(command);
        process
    } else {
        let mut process = Command::new("sh");
        process.arg("-c").arg(command);
        process
    };
    bind_tester_child_to_workspace(&mut process, working_dir);
    for (key, value) in launch_env {
        process.env(key, value);
    }
    if let Some(directory) = settlement_directory {
        process
            .env("TMPDIR", directory)
            .env("FLUENT_RELEASE_TEST_ROOTS", directory);
    }
    let transcript = commands_dir
        .parent()
        .context("Tester command has no artifact area")?
        .join("transcript.jsonl");
    let mut applied_authority = None;
    let output = crate::execution::output_with_launch_guard(&mut process, &transcript, || {
        admission
            .map(|(candidate, original_index)| {
                let admitted = crate::tester_permissions::begin_launch_admission(
                    working_dir,
                    candidate,
                    original_index,
                )?;
                applied_authority = Some(admitted.authority.clone());
                Ok(admitted)
            })
            .transpose()
    })
    .with_context(|| format!("spawning Tester command: {command}"))?;
    let duration = start.elapsed();

    crate::tester_permissions::write_command_artifact(&stdout_path, &output.stdout)?;
    crate::tester_permissions::write_command_artifact(&stderr_path, &output.stderr)?;

    let exit_code = output.status.code().unwrap_or(-1);

    let (permission_receipt, permission_receipt_id) = if let Some(authority) = applied_authority {
        let profile =
            sandbox_profile.context("permission-bearing command lacks a sandbox profile")?;
        let name = format!("commands/{index}-permission-receipt.json");
        let receipt_id = crate::tester_permissions::record_applied_authority(
            &authority,
            &profile.path,
            exit_code,
            &output.stdout,
            &output.stderr,
            &commands_dir.join(format!("{index}-permission-receipt.json")),
        )?;
        (Some(name), Some(receipt_id))
    } else {
        (None, None)
    };

    let stdout_log = format!("commands/{index}-stdout.log");
    let stderr_log = format!("commands/{index}-stderr.log");

    Ok(CommandResult {
        failure_cause: (exit_code != 0).then_some(CommandFailureCause::Unclassified),
        command: command.to_string(),
        test_harness: test_harness.to_string(),
        exit_code,
        duration_ms: duration.as_millis() as u64,
        stdout_log,
        stderr_log,
        permission_receipt,
        permission_receipt_id,
        process_settlement: None,
    })
}

/// Keep host-owned scheduler coordination outside project test processes. The
/// outer Fluent process consumes this token when it elects or reuses a baseline;
/// a nested Fluent invocation is an ordinary test subject, not a cache peer.
fn isolate_tester_child_environment(command: &mut Command) {
    command.env_remove(crate::baseline_cache::BASELINE_SESSION_ENV);
}

/// Bind the child process's filesystem and shell view to the checkout under
/// test. `Command::current_dir` does not rewrite inherited `PWD`, and Cargo
/// trusts that variable when locating a workspace. A detached landed-baseline
/// checkout must therefore replace both views atomically.
fn bind_tester_child_to_workspace(command: &mut Command, working_dir: &Path) {
    let working_dir = fs::canonicalize(working_dir).unwrap_or_else(|_| working_dir.to_path_buf());
    isolate_tester_child_environment(command);
    command
        .current_dir(&working_dir)
        .env("PWD", &working_dir)
        .env_remove("OLDPWD");
}

fn run_extractor(
    candidate_workspace: &Path,
    extractor_path: &Path,
    artifact_dir: &Path,
    sandbox_profile: Option<&os::SandboxProfile>,
    launch_env: &[(String, String)],
) -> Result<Vec<TestResult>> {
    let mut command = if let Some(profile) = sandbox_profile {
        let mut command = os::command_under_profile(profile, extractor_path);
        command.arg(artifact_dir);
        command
    } else {
        let mut command = Command::new(extractor_path);
        command.arg(artifact_dir);
        command
    };
    bind_tester_child_to_workspace(&mut command, candidate_workspace);
    for (key, value) in launch_env {
        command.env(key, value);
    }
    let output = crate::execution::output(&mut command, &artifact_dir.join("transcript.jsonl"))
        .with_context(|| format!("running {}", extractor_path.display()))?;

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

/// Whether persisted Tester evidence identifies Xcode/SwiftPM's unsupported
/// nested sandbox boundary. This is advisory classification for the recovery
/// handoff; unreadable evidence remains a generic Tester-harness failure.
pub fn is_nested_swiftpm_harness_error(artifact_dir: &Path) -> bool {
    fs::read(artifact_dir.join("tester-results.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<TesterResults>(&bytes).ok())
        .and_then(|results| results.error)
        .is_some_and(|error| error.kind == "nested_swiftpm_sandbox")
}

fn write_results(artifact_dir: &Path, results: &TesterResults) -> Result<()> {
    let json = serde_json::to_string_pretty(results)?;
    let path = artifact_dir.join("tester-results.json");
    crate::tester_permissions::write_command_artifact(&path, json.as_bytes())?;
    eprintln!("  Wrote {}", path.display());
    Ok(())
}

/// Bind host-produced Tester evidence to the exact candidate commit once.
/// Repeating the same binding is idempotent; a conflicting binding fails closed.
pub fn bind_candidate_commit(artifact_dir: &Path, candidate_commit: &str) -> Result<()> {
    anyhow::ensure!(
        !candidate_commit.trim().is_empty(),
        "candidate commit must not be empty"
    );
    let path = artifact_dir.join("tester-results.json");
    let source = fs::read(&path)
        .with_context(|| format!("failed to read Tester evidence at {}", path.display()))?;
    let mut results: TesterResults = serde_json::from_slice(&source)
        .with_context(|| format!("failed to parse Tester evidence at {}", path.display()))?;
    match results.candidate_commit.as_deref() {
        Some(existing) if existing != candidate_commit => anyhow::bail!(
            "Tester evidence is already bound to candidate {existing}, not {candidate_commit}"
        ),
        Some(_) => return Ok(()),
        None => {}
    }
    results.candidate_commit = Some(candidate_commit.to_string());
    write_results(artifact_dir, &results)
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

    #[test]
    fn command_logs_do_not_follow_worker_created_aliases() {
        let workspace = TempDir::new().unwrap();
        let commands = workspace.path().join("commands");
        fs::create_dir(&commands).unwrap();
        let target = workspace.path().join("host-target");
        fs::write(&target, b"original").unwrap();
        std::os::unix::fs::symlink(&target, commands.join("0-stdout.log")).unwrap();
        let result = run_command(
            workspace.path(),
            "printf output",
            "shell-harness",
            0,
            &commands,
            None,
            None,
            &[],
            None,
        );
        assert!(result.is_err());
        assert_eq!(fs::read(&target).unwrap(), b"original");
    }

    #[test]
    fn requested_profile_is_selected_only_for_its_command() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let private = root.join("profiles");
        fs::create_dir(&private).unwrap();
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
        let baseline = os::render_profile_common_only(
            &ContentResolver::new(None),
            "/staged/worker-home",
            &[root.join("workspace")],
            &[],
        )
        .unwrap();
        let commands: Vec<TesterCommand> = serde_yaml::from_str(
            "- command: 'true'\n  test_harness: shell-harness\n- command: 'true'\n  test_harness: shell-harness\n  permissions: [process-table-read]\n- command: 'true'\n  test_harness: shell-harness\n",
        )
        .unwrap();
        let profiles = render_command_profiles(&commands, &baseline, &private).unwrap();
        assert_eq!(profiles.len(), 3);
        assert!(profiles[0].is_none());
        assert!(profiles[2].is_none());
        let requested = fs::read_to_string(&profiles[1].as_ref().unwrap().path).unwrap();
        assert!(requested.contains(crate::tester_permissions::PROCESS_TABLE_RULE));
        assert!(
            !fs::read_to_string(&baseline.path)
                .unwrap()
                .contains("kern.proc.all")
        );
    }

    #[test]
    fn requested_command_cannot_fall_back_to_baseline_profile() {
        let tmp = tempfile::TempDir::new().unwrap();
        let commands: Vec<TesterCommand> = serde_yaml::from_str(
            "- command: 'true'\n  test_harness: shell-harness\n  permissions: [process-table-read]\n",
        )
        .unwrap();
        let error = run_with_prepared_sandbox_profile_and_collector(
            tmp.path(),
            tmp.path(),
            None,
            None,
            None,
            &commands,
            None,
            &[],
            &HostProcessInventoryCollector,
        )
        .expect_err("a requested command needs its own profile");
        assert!(error.to_string().contains("per-command profiles"));
    }

    #[test]
    fn requested_command_requires_pinned_admission_identity() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let profile = os::render_profile_common_only(
            &ContentResolver::new(None),
            "/staged/worker-home",
            &[root.clone()],
            &[],
        )
        .unwrap();
        let commands: Vec<TesterCommand> = serde_yaml::from_str(
            "- command: 'true'\n  test_harness: shell-harness\n  permissions: [process-table-read]\n",
        )
        .unwrap();
        let selected = [Some(profile)];
        let error = run_with_prepared_sandbox_profile_and_collector(
            &root,
            &root,
            None,
            Some(&selected),
            None,
            &commands,
            None,
            &[],
            &HostProcessInventoryCollector,
        )
        .expect_err("a requested command needs its pinned request identity");
        assert!(error.to_string().contains("admission identity"));
    }

    #[test]
    fn tester_results_bind_once_to_the_exact_candidate_commit() {
        let artifact = TempDir::new().unwrap();
        write_results(
            artifact.path(),
            &TesterResults {
                candidate_commit: None,
                commands: Vec::new(),
                tests: Vec::new(),
                summary: Summary {
                    total: 0,
                    pass: 0,
                    fail: 0,
                    skipped: 0,
                },
                error: None,
            },
        )
        .unwrap();

        bind_candidate_commit(artifact.path(), "candidate-abc").unwrap();
        assert_eq!(
            read_results(artifact.path()).candidate_commit.as_deref(),
            Some("candidate-abc")
        );
        let error = bind_candidate_commit(artifact.path(), "candidate-def").unwrap_err();
        assert!(error.to_string().contains("candidate-abc"));
        assert_eq!(
            read_results(artifact.path()).candidate_commit.as_deref(),
            Some("candidate-abc")
        );
    }
    use crate::process_settlement::ProcessEntry;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;
    use tempfile::TempDir;

    #[test]
    fn persisted_harness_error_distinguishes_project_repair_from_environment_retry() {
        let project_owned = TempDir::new().unwrap();
        persist_harness_error(
            project_owned.path(),
            "duplicate_test_ids",
            "Duplicate test ids".to_string(),
            String::new(),
            Vec::new(),
        )
        .unwrap();
        assert!(harness_error_requires_project_repair(project_owned.path()));

        let environmental = TempDir::new().unwrap();
        persist_harness_error(
            environmental.path(),
            "process_settlement_infrastructure",
            "Unable to inspect processes".to_string(),
            String::new(),
            Vec::new(),
        )
        .unwrap();
        assert!(!harness_error_requires_project_repair(environmental.path()));
    }

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

    struct RecordingCollector {
        calls: Mutex<Vec<PathBuf>>,
    }

    impl RecordingCollector {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl ProcessInventoryCollector for RecordingCollector {
        fn collect(&self, directory: &Path) -> Result<Vec<ProcessEntry>> {
            self.calls.lock().unwrap().push(directory.to_path_buf());
            Ok(Vec::new())
        }
    }

    struct NoInventoryCollector;

    impl ProcessInventoryCollector for NoInventoryCollector {
        fn collect(&self, _: &Path) -> Result<Vec<ProcessEntry>> {
            panic!("unguarded commands must not collect process inventory")
        }
    }

    #[test]
    fn tester_children_do_not_inherit_scheduler_baseline_session() {
        let mut command = Command::new("sh");
        command.env(
            crate::baseline_cache::BASELINE_SESSION_ENV,
            "outer-scheduler-session",
        );

        isolate_tester_child_environment(&mut command);

        let inherited = command
            .get_envs()
            .find(|(name, _)| *name == crate::baseline_cache::BASELINE_SESSION_ENV)
            .map(|(_, value)| value);
        assert_eq!(inherited, Some(None));
    }

    #[test]
    fn tester_process_table_requests_reject_unknown_and_duplicate_capabilities() {
        for permissions in ["[process-table-read, process-table-read]", "[file-write]"] {
            let yaml = format!(
                "commands:\n  - command: 'true'\n    test_harness: shell-harness\n    permissions: {permissions}\n"
            );
            assert!(serde_yaml::from_str::<TesterConfig>(&yaml).is_err());
        }
        let yaml = "commands:\n  - command: 'true'\n    test_harness: shell-harness\n    permissions: [process-table-read]\n";
        let config: TesterConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.commands[0].permissions.len(), 1);
    }

    #[test]
    fn tester_legacy_command_serialization_omits_permissions() {
        let yaml = "commands:\n  - command: 'true'\n    test_harness: shell-harness\n";
        let config: TesterConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.commands[0].permissions.is_empty());
        let serialized = serde_json::to_string(&config.commands[0]).unwrap();
        assert!(!serialized.contains("permissions"));
    }

    #[test]
    fn tester_process_table_request_fails_closed_before_launch() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: 'touch launched'\n    test_harness: shell-harness\n    permissions: [process-table-read]\n",
        );
        let result = run(workspace.path(), artifact_dir.path(), true, &resolver());
        assert!(result.is_err(), "an unapproved command must not launch");
        assert!(!workspace.path().join("launched").exists());
    }

    #[test]
    fn tester_requested_permission_cannot_bypass_render_boundary() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: true\n    test_harness: shell-harness\n    permissions: [process-table-read]\n",
        );
        let plan = plan(workspace.path()).unwrap();
        let settlement = prepare_settlement_directories(artifact_dir.path(), &plan).unwrap();

        let error = render_sandbox_profile(
            workspace.path(),
            artifact_dir.path(),
            true,
            &resolver(),
            settlement,
        )
        .err()
        .expect("direct profile rendering must reject a permission request");

        assert!(
            error.to_string().contains("requires the macOS sandbox"),
            "{error:#}"
        );
    }

    #[test]
    fn tester_commands_and_extractor_bind_pwd_to_tested_checkout() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: 'test \"$PWD\" = \"$(pwd -P)\" && ! env | grep -q \"^OLDPWD=\"'\n    test_harness: shell-harness\n",
        );
        write_extractor(
            workspace.path(),
            "#!/bin/sh\ntest \"$PWD\" = \"$(pwd -P)\"\n! env | grep -q '^OLDPWD='\necho '[]'\n",
        );

        let outcome = run(workspace.path(), artifact_dir.path(), true, &resolver()).unwrap();

        assert_eq!(outcome, TesterOutcome::Passed);
        let results = read_results(artifact_dir.path());
        assert_eq!(results.commands[0].exit_code, 0);
        assert!(results.error.is_none());
    }

    #[test]
    fn attempt_scoped_tester_commands_receive_private_cargo_home() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        let cache = crate::writer_cache::preflight_writer_dependency_cache(
            workspace.path(),
            "work-1",
            "attempt-1",
        )
        .unwrap();
        crate::writer_cache::prepare_writer_dependency_cache(&cache, None).unwrap();
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: 'test -d \"$CARGO_HOME\" && touch \"$CARGO_HOME/command-marker\"'\n    test_harness: shell-harness\n",
        );
        write_extractor(workspace.path(), "#!/bin/sh\necho '[]'\n");
        let plan = plan(workspace.path()).unwrap();
        let settlement = prepare_settlement_directories(artifact_dir.path(), &plan).unwrap();
        let sandbox = render_sandbox_profile_with_cache(
            workspace.path(),
            artifact_dir.path(),
            true,
            &resolver(),
            settlement,
            Some(&cache),
        )
        .unwrap();

        assert_eq!(sandbox.launch_env, cache.launch_env());
        assert_eq!(
            run_with_sandbox_profile(workspace.path(), artifact_dir.path(), &sandbox).unwrap(),
            TesterOutcome::Passed
        );
        assert!(cache.cargo_home().join("command-marker").is_file());
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn launch_handoff_route_nested_commands_run_through_rendered_sandbox() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        crate::git::run(
            workspace.path(),
            &["init", "-q"],
            "initialize rendered Tester sandbox fixture",
        )
        .unwrap();
        let cache = crate::writer_cache::preflight_writer_dependency_cache(
            workspace.path(),
            "work-1",
            "attempt-1",
        )
        .unwrap();
        crate::writer_cache::prepare_writer_dependency_cache(&cache, None).unwrap();
        write_tester_yaml(
            workspace.path(),
            &format!(
                "commands:\n  - command: 'test -d \"$CARGO_HOME\" && touch \"$CARGO_HOME/rendered-sandbox-marker\" && cp \"{0}/execution-receipt\" \"{0}/first-receipt\"'\n    test_harness: shell-harness\n  - command: 'cp \"{0}/execution-receipt\" \"{0}/second-receipt\"'\n    test_harness: shell-harness\n",
                artifact_dir.path().display()
            ),
        );
        write_extractor(workspace.path(), "#!/bin/sh\necho '[]'\n");
        let plan = plan(workspace.path()).unwrap();
        let settlement = prepare_settlement_directories(artifact_dir.path(), &plan).unwrap();
        let invocation_log = artifact_dir.path().join("sandbox-invocations.log");
        let sandbox_launcher = workspace.path().join("test-sandbox-exec");
        fs::write(
            &sandbox_launcher,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$2\" >> \"{}\"\nshift 2\nexec \"$@\"\n",
                invocation_log.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&sandbox_launcher).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&sandbox_launcher, permissions).unwrap();

        unsafe {
            std::env::set_var("FLUENT_TEST_HOST_SANDBOX_PREFLIGHT", "render-pass");
            std::env::set_var(
                "FLUENT_TEST_FOCUSED_EVIDENCE_SANDBOX_EXEC",
                &sandbox_launcher,
            );
        }
        let result = (|| -> Result<(TesterOutcome, String)> {
            let sandbox = render_sandbox_profile_with_cache(
                workspace.path(),
                artifact_dir.path(),
                false,
                &resolver(),
                settlement,
                Some(&cache),
            )?;
            let profile = sandbox
                .profile()
                .expect("the rendered Tester sandbox must retain its profile");
            let profile_path = profile.path.display().to_string();
            let profile_content = fs::read_to_string(&profile.path)?;
            let outcome =
                run_with_sandbox_profile(workspace.path(), artifact_dir.path(), &sandbox)?;
            Ok((outcome, format!("{profile_path}\n{profile_content}")))
        })();
        unsafe {
            std::env::remove_var("FLUENT_TEST_HOST_SANDBOX_PREFLIGHT");
            std::env::remove_var("FLUENT_TEST_FOCUSED_EVIDENCE_SANDBOX_EXEC");
        }
        let (outcome, profile) = result.unwrap();
        let (profile_path, profile_content) = profile.split_once('\n').unwrap();

        assert_eq!(outcome, TesterOutcome::Passed);
        assert!(cache.cargo_home().join("rendered-sandbox-marker").is_file());
        let first = fs::read_to_string(artifact_dir.path().join("first-receipt")).unwrap();
        let second = fs::read_to_string(artifact_dir.path().join("second-receipt")).unwrap();
        assert_ne!(
            first, second,
            "nested Tester commands need fresh generations"
        );
        let record: crate::execution::ExecutionRecord =
            serde_json::from_slice(&fs::read(artifact_dir.path().join("execution.json")).unwrap())
                .unwrap();
        assert_eq!(record.schema_version, 2);
        assert!(second.starts_with("2 "));
        assert!(record.launch_generation.is_some());
        let cache_write = format!(
            "(allow file-write* (subpath \"{}\"))",
            cache.root().display()
        );
        assert!(
            profile_content.contains(&cache_write),
            "the rendered Tester sandbox must grant the exact Attempt cache: {profile_content}"
        );
        if let Some(home) = std::env::var_os("HOME") {
            let user_cargo = PathBuf::from(home).join(".cargo");
            let user_cargo_write =
                format!("(allow file-write* (subpath \"{}\"))", user_cargo.display());
            assert!(
                !profile_content.contains(&user_cargo_write),
                "the rendered Tester sandbox must not grant the user's Cargo store: {profile_content}"
            );
        }
        let invocations = fs::read_to_string(invocation_log).unwrap();
        let invocation_paths = invocations.lines().collect::<Vec<_>>();
        assert_eq!(invocation_paths.len(), 3);
        assert!(invocation_paths.iter().all(|path| *path == profile_path));
    }

    enum ScriptedInventory {
        Empty,
        Leak { pid: u32, kind: &'static str },
        Error(&'static str),
    }

    struct ScriptedCollector {
        inventories: Mutex<Vec<ScriptedInventory>>,
    }

    impl ScriptedCollector {
        fn new(inventories: Vec<ScriptedInventory>) -> Self {
            Self {
                inventories: Mutex::new(inventories),
            }
        }
    }

    impl ProcessInventoryCollector for ScriptedCollector {
        fn collect(&self, directory: &Path) -> Result<Vec<ProcessEntry>> {
            match self.inventories.lock().unwrap().remove(0) {
                ScriptedInventory::Empty => Ok(Vec::new()),
                ScriptedInventory::Leak { pid, kind } => Ok(vec![ProcessEntry {
                    pid,
                    kind: kind.to_string(),
                    settlement_directory: directory.to_path_buf(),
                }]),
                ScriptedInventory::Error(message) => anyhow::bail!(message),
            }
        }
    }

    #[test]
    fn guarded_command_receives_host_owned_settlement_directory() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        let fixture = workspace.path().join("fixture.txt");
        write_tester_yaml(
            workspace.path(),
            &format!(
                "commands:\n  - command: 'printf %s \"$TMPDIR:$FLUENT_RELEASE_TEST_ROOTS\" > {}'\n    test_harness: shell-harness\n    reject_process_leaks: true\n",
                fixture.display()
            ),
        );
        write_extractor(workspace.path(), "#!/bin/sh\necho '[]'\n");
        let collector = RecordingCollector::new();

        run_with_sandbox_profile_and_collector(
            workspace.path(),
            artifact_dir.path(),
            None,
            None,
            &collector,
        )
        .unwrap();

        let calls = collector.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        let directory = &calls[0];
        assert!(directory.is_dir());
        assert_eq!(calls[1], *directory);
        assert_eq!(
            fs::read_to_string(fixture).unwrap(),
            format!("{0}:{0}", directory.display())
        );
    }

    #[test]
    fn guarded_tester_prepares_settlement_before_sandbox_after_reservation() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: true\n    test_harness: shell-harness\n    reject_process_leaks: true\n",
        );

        let sandbox =
            preflight_sandbox_profile(workspace.path(), artifact_dir.path(), true, &resolver())
                .unwrap();

        let directory = sandbox.settlement_directory(0).unwrap();
        assert!(directory.is_dir());
        assert_eq!(
            directory,
            fs::canonicalize(artifact_dir.path().join("commands/settlement/0")).unwrap()
        );
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn guarded_tester_grants_canonical_settlement_to_rendered_sandbox() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        crate::git::run(
            workspace.path(),
            &["init", "-q"],
            "initialize rendered sandbox fixture",
        )
        .unwrap();
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: true\n    test_harness: shell-harness\n    reject_process_leaks: true\n",
        );

        unsafe { std::env::set_var("FLUENT_TEST_HOST_SANDBOX_PREFLIGHT", "render-pass") };
        let sandbox =
            preflight_sandbox_profile(workspace.path(), artifact_dir.path(), false, &resolver())
                .unwrap();
        unsafe { std::env::remove_var("FLUENT_TEST_HOST_SANDBOX_PREFLIGHT") };

        let settlement = sandbox.settlement_directory(0).unwrap();
        let profile = sandbox.profile().expect("sandbox rendering must complete");
        let profile_content = fs::read_to_string(&profile.path).unwrap();
        assert_eq!(
            settlement,
            fs::canonicalize(artifact_dir.path().join("commands/settlement/0")).unwrap()
        );
        assert!(
            profile_content.contains(&format!(
                "(allow file-write* (subpath \"{}\"))",
                settlement.display()
            )),
            "the rendered sandbox must grant the canonical guarded settlement path: {profile_content}"
        );
    }

    #[test]
    fn prepared_tester_plan_remains_execution_authority() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        let planned = workspace.path().join("planned-command");
        let changed = workspace.path().join("changed-command");
        write_tester_yaml(
            workspace.path(),
            &format!(
                "commands:\n  - command: 'touch {}'\n    test_harness: shell-harness\n    reject_process_leaks: true\n",
                planned.display()
            ),
        );
        write_extractor(workspace.path(), "#!/bin/sh\necho '[]'\n");
        let plan = plan(workspace.path()).unwrap();
        let settlement = prepare_settlement_directories(artifact_dir.path(), &plan).unwrap();
        fs::write(
            workspace.path().join(TESTER_YAML_PATH),
            format!(
                "commands:\n  - command: 'touch {}'\n    test_harness: shell-harness\n",
                changed.display()
            ),
        )
        .unwrap();
        let sandbox = render_sandbox_profile(
            workspace.path(),
            artifact_dir.path(),
            true,
            &resolver(),
            settlement,
        )
        .unwrap();

        let collector = RecordingCollector::new();
        run_with_prepared_sandbox_profile_and_collector(
            workspace.path(),
            artifact_dir.path(),
            sandbox.profile(),
            Some(&sandbox.command_profiles),
            sandbox
                .candidate_commit
                .as_deref()
                .map(|candidate| (candidate, sandbox.original_indexes.as_slice())),
            &sandbox.commands,
            Some(&sandbox.settlement_directories),
            &[],
            &collector,
        )
        .unwrap();

        assert!(planned.is_file());
        assert!(!changed.exists());
        assert!(sandbox.settlement_directory(0).unwrap().is_dir());
    }

    #[test]
    fn failed_command_plan_selects_only_matching_nonzero_commands() {
        let workspace = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: first\n    test_harness: shell\n  - command: second\n    test_harness: shell\n  - command: third\n    test_harness: cargo\n",
        );
        let source = TesterResults {
            candidate_commit: Some("candidate".to_string()),
            commands: vec![
                command_result("first", "shell", 0),
                command_result("second", "shell", 65),
                command_result("third", "cargo", 1),
            ],
            tests: Vec::new(),
            summary: Summary {
                total: 0,
                pass: 0,
                fail: 0,
                skipped: 0,
            },
            error: None,
        };

        let (plan, indexes) = plan_failed_commands(workspace.path(), &source).unwrap();

        assert_eq!(indexes, vec![1, 2]);
        assert_eq!(plan.commands.len(), 2);
        assert_eq!(plan.original_indexes, vec![1, 2]);
        assert_eq!(plan.commands[0].command, "second");
        assert_eq!(plan.commands[1].command, "third");
    }

    #[test]
    fn failed_command_plan_rejects_current_config_drift() {
        let workspace = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: changed\n    test_harness: shell\n",
        );
        let source = TesterResults {
            candidate_commit: Some("candidate".to_string()),
            commands: vec![command_result("original", "shell", 1)],
            tests: Vec::new(),
            summary: Summary {
                total: 0,
                pass: 0,
                fail: 0,
                skipped: 0,
            },
            error: None,
        };

        let error = plan_failed_commands(workspace.path(), &source)
            .expect_err("changed command must invalidate the targeted recheck");

        assert!(error.to_string().contains("no longer matches"));
    }

    #[test]
    fn chained_failed_command_plan_keeps_original_configuration_index() {
        let workspace = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: first\n    test_harness: shell\n  - command: second\n    test_harness: shell\n",
        );
        let source = TesterResults {
            candidate_commit: Some("candidate".to_string()),
            commands: vec![command_result("second", "shell", 1)],
            tests: Vec::new(),
            summary: Summary {
                total: 0,
                pass: 0,
                fail: 0,
                skipped: 0,
            },
            error: None,
        };

        let (plan, local_indexes) =
            plan_chained_failed_commands(workspace.path(), &source).unwrap();

        assert_eq!(local_indexes, vec![0]);
        assert_eq!(plan.original_indexes, vec![1]);
    }

    fn command_result(command: &str, harness: &str, exit_code: i32) -> CommandResult {
        CommandResult {
            failure_cause: None,
            command: command.to_string(),
            test_harness: harness.to_string(),
            exit_code,
            duration_ms: 1,
            stdout_log: "stdout.log".to_string(),
            stderr_log: "stderr.log".to_string(),
            permission_receipt: None,
            permission_receipt_id: None,
            process_settlement: None,
        }
    }

    #[test]
    fn settlement_preparation_removes_created_parent_directories_on_failure() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: true\n    test_harness: shell-harness\n    reject_process_leaks: true\n  - command: true\n    test_harness: shell-harness\n    reject_process_leaks: true\n",
        );
        let plan = plan(workspace.path()).unwrap();
        let mut calls = 0;

        let error =
            match prepare_settlement_directories_with(artifact_dir.path(), &plan, |directory| {
                calls += 1;
                if calls == 2 {
                    anyhow::bail!("injected settlement preparation failure");
                }
                ProcessSettlement::prepare_directory(directory)
            }) {
                Ok(_) => panic!("the second guarded settlement setup must fail"),
                Err(error) => error,
            };

        assert!(error.to_string().contains("injected settlement"));
        assert!(
            !artifact_dir.path().join("commands").exists(),
            "cleanup removes every parent directory created by this invocation"
        );
    }

    #[test]
    fn guarded_command_cannot_replace_host_process_inventory() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: 'export FLUENT_TEST_PROCESS_INVENTORY=forged; true'\n    test_harness: shell-harness\n    reject_process_leaks: true\n",
        );
        write_extractor(workspace.path(), "#!/bin/sh\necho '[]'\n");
        let collector = RecordingCollector::new();

        run_with_sandbox_profile_and_collector(
            workspace.path(),
            artifact_dir.path(),
            None,
            None,
            &collector,
        )
        .unwrap();

        assert_eq!(collector.calls.lock().unwrap().len(), 2);
    }

    #[test]
    fn unguarded_command_retains_existing_tester_boundary() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: echo ordinary\n    test_harness: shell-harness\n",
        );
        write_extractor(workspace.path(), "#!/bin/sh\necho '[]'\n");

        run_with_sandbox_profile_and_collector(
            workspace.path(),
            artifact_dir.path(),
            None,
            None,
            &NoInventoryCollector,
        )
        .unwrap();

        let results = read_results(artifact_dir.path());
        assert_eq!(results.commands[0].exit_code, 0);
        assert_eq!(
            fs::read_to_string(artifact_dir.path().join(&results.commands[0].stdout_log)).unwrap(),
            "ordinary\n"
        );
    }

    #[test]
    fn transient_process_inventory_does_not_discard_baseline_results() {
        use crate::tester_evidence::{
            introduced_tester_failures, unexplained_failed_tester_commands,
        };
        use std::os::unix::process::ExitStatusExt;

        struct TransientCollector {
            snapshots: Mutex<usize>,
            after_command_only: bool,
        }

        impl ProcessInventoryCollector for TransientCollector {
            fn collect(&self, directory: &Path) -> Result<Vec<ProcessEntry>> {
                let mut snapshots = self.snapshots.lock().unwrap();
                let transient = !self.after_command_only || *snapshots > 0;
                *snapshots += 1;
                HostProcessInventoryCollector::collect_with_probe(
                    directory,
                    &mut |command, args| {
                        let (code, stdout) = match (command, args) {
                            ("ps", ["-axo", "pid=,command="]) if transient => {
                                (0, "101 codex exec\n102 fluent scheduler\n")
                            }
                            ("ps", ["-axo", "pid=,command="]) => (0, "999 unrelated\n"),
                            ("lsof", ["-a", "-p", "101" | "102", "-d", "cwd", "-Fn"])
                            | ("ps", ["-p", "101", "-o", "stat="]) => (1, ""),
                            ("ps", ["-p", "102", "-o", "stat="]) => (0, "Z+\n"),
                            _ => panic!("unexpected probe: {command} {args:?}"),
                        };
                        Ok(std::process::Output {
                            status: std::process::ExitStatus::from_raw(code << 8),
                            stdout: stdout.as_bytes().to_vec(),
                            stderr: vec![],
                        })
                    },
                )
            }
        }

        // Exercise both baseline admission and the inventory after a completed
        // command, where the original race discarded otherwise usable results.
        for after_command_only in [true, false] {
            let workspace = TempDir::new().unwrap();
            let baseline_dir = TempDir::new().unwrap();
            let candidate_dir = TempDir::new().unwrap();
            let regression_dir = TempDir::new().unwrap();
            make_workspace(workspace.path());
            write_tester_yaml(
                workspace.path(),
                "commands:\n  - command: 'echo ambient-failure; exit 1'\n    test_harness: shell-harness\n    reject_process_leaks: true\n",
            );
            let ambient = r#"{"id":"ambient","test_harness":"shell-harness","status":"fail","duration_ms":null,"failure_excerpt":"ambient-failure"}"#;
            write_extractor(
                workspace.path(),
                &format!("#!/bin/sh\necho '[{ambient}]'\n"),
            );
            let collector = TransientCollector {
                snapshots: Mutex::new(0),
                after_command_only,
            };

            let outcome = run_with_sandbox_profile_and_collector(
                workspace.path(),
                baseline_dir.path(),
                None,
                None,
                &collector,
            )
            .unwrap();

            assert_eq!(outcome, TesterOutcome::TestFailures);
            assert_eq!(*collector.snapshots.lock().unwrap(), 2);
            let baseline = read_results(baseline_dir.path());
            assert!(baseline.error.is_none());
            assert_eq!(baseline.commands.len(), 1);
            assert_eq!(baseline.commands[0].exit_code, 1);
            assert_eq!(
                fs::read_to_string(baseline_dir.path().join(&baseline.commands[0].stdout_log))
                    .unwrap(),
                "ambient-failure\n"
            );
            assert!(
                baseline.commands[0]
                    .process_settlement
                    .as_ref()
                    .unwrap()
                    .leaks
                    .is_empty()
            );
            assert_eq!(baseline.summary.fail, 1);
            assert_eq!(baseline.tests[0].id, "ambient");
            assert_eq!(
                baseline.tests[0].failure_excerpt.as_deref(),
                Some("ambient-failure")
            );
            let baseline_path = baseline_dir.path().join("tester-results.json");
            let baseline_bytes = fs::read(&baseline_path).unwrap();

            for (artifact, introduces_failure) in
                [(candidate_dir.path(), false), (regression_dir.path(), true)]
            {
                if introduces_failure {
                    let regression = r#"{"id":"regression","test_harness":"shell-harness","status":"fail","duration_ms":null,"failure_excerpt":"new failure"}"#;
                    write_extractor(
                        workspace.path(),
                        &format!("#!/bin/sh\necho '[{ambient},{regression}]'\n"),
                    );
                }
                let outcome = run_with_sandbox_profile_and_collector(
                    workspace.path(),
                    artifact,
                    None,
                    None,
                    &ScriptedCollector::new(vec![
                        ScriptedInventory::Empty,
                        ScriptedInventory::Empty,
                    ]),
                )
                .unwrap();
                assert_eq!(outcome, TesterOutcome::TestFailures);
                let candidate_path = artifact.join("tester-results.json");
                assert_eq!(
                    introduced_tester_failures(&candidate_path, Some(&baseline_path)),
                    usize::from(introduces_failure),
                );
                assert_eq!(
                    unexplained_failed_tester_commands(&candidate_path, Some(&baseline_path)),
                    usize::from(introduces_failure),
                );
            }
            assert_eq!(fs::read(baseline_path).unwrap(), baseline_bytes);
        }
    }

    #[test]
    fn unavailable_settlement_baseline_prevents_command_launch() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        let launched = workspace.path().join("launched");
        write_tester_yaml(
            workspace.path(),
            &format!(
                "commands:\n  - command: 'touch {}'\n    test_harness: shell-harness\n    reject_process_leaks: true\n",
                launched.display()
            ),
        );
        write_extractor(workspace.path(), "#!/bin/sh\necho '[]'\n");
        let collector = ScriptedCollector::new(vec![ScriptedInventory::Error("ps denied")]);

        let outcome = run_with_sandbox_profile_and_collector(
            workspace.path(),
            artifact_dir.path(),
            None,
            None,
            &collector,
        )
        .unwrap();

        assert_eq!(outcome, TesterOutcome::HarnessError);
        assert!(!launched.exists());
        let results = read_results(artifact_dir.path());
        assert_eq!(
            results.error.unwrap().kind,
            "process_settlement_infrastructure"
        );
        assert!(results.commands.is_empty());
    }

    #[test]
    fn unavailable_post_command_inventory_preserves_command_evidence() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: 'echo completed; exit 42'\n    test_harness: shell-harness\n    reject_process_leaks: true\n",
        );
        write_extractor(workspace.path(), "#!/bin/sh\necho '[]'\n");
        let collector = ScriptedCollector::new(vec![
            ScriptedInventory::Empty,
            ScriptedInventory::Error("lsof denied"),
        ]);

        let outcome = run_with_sandbox_profile_and_collector(
            workspace.path(),
            artifact_dir.path(),
            None,
            None,
            &collector,
        )
        .unwrap();

        assert_eq!(outcome, TesterOutcome::HarnessError);
        let results = read_results(artifact_dir.path());
        assert_eq!(
            results.error.unwrap().kind,
            "process_settlement_infrastructure"
        );
        assert_eq!(results.commands[0].exit_code, 42);
        assert!(
            fs::read_to_string(artifact_dir.path().join(&results.commands[0].stdout_log))
                .unwrap()
                .contains("completed")
        );
    }

    #[test]
    fn tester_host_process_settlement_reports_scoped_leak() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: true\n    test_harness: shell-harness\n    reject_process_leaks: true\n",
        );
        write_extractor(workspace.path(), "#!/bin/sh\necho '[]'\n");
        let collector = ScriptedCollector::new(vec![
            ScriptedInventory::Empty,
            ScriptedInventory::Leak {
                pid: 4242,
                kind: "codex",
            },
            ScriptedInventory::Leak {
                pid: 4242,
                kind: "codex",
            },
            ScriptedInventory::Leak {
                pid: 4242,
                kind: "codex",
            },
            ScriptedInventory::Leak {
                pid: 4242,
                kind: "codex",
            },
            ScriptedInventory::Leak {
                pid: 4242,
                kind: "codex",
            },
        ]);

        let outcome = run_with_sandbox_profile_and_collector(
            workspace.path(),
            artifact_dir.path(),
            None,
            None,
            &collector,
        )
        .unwrap();

        assert_eq!(outcome, TesterOutcome::TestFailures);
        let results = read_results(artifact_dir.path());
        let evidence = results.commands[0].process_settlement.as_ref().unwrap();
        assert_eq!(evidence.leaks[0].pid, 4242);
        assert_eq!(evidence.leaks[0].kind, "codex");
        assert_eq!(
            evidence.leaks[0].settlement_directory,
            evidence.settlement_directory
        );
    }

    #[test]
    fn tester_host_process_settlement_preserves_clean_command_result() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        write_tester_yaml(
            workspace.path(),
            "commands:\n  - command: 'echo clean-output; exit 42'\n    test_harness: shell-harness\n    reject_process_leaks: true\n",
        );
        write_extractor(
            workspace.path(),
            "#!/bin/sh\necho '[{\"id\":\"clean\",\"test_harness\":\"shell-harness\",\"status\":\"pass\",\"duration_ms\":null,\"failure_excerpt\":null}]'\n",
        );
        let collector =
            ScriptedCollector::new(vec![ScriptedInventory::Empty, ScriptedInventory::Empty]);

        let outcome = run_with_sandbox_profile_and_collector(
            workspace.path(),
            artifact_dir.path(),
            None,
            None,
            &collector,
        )
        .unwrap();

        assert_eq!(outcome, TesterOutcome::Passed);
        let results = read_results(artifact_dir.path());
        assert_eq!(results.commands[0].exit_code, 42);
        assert!(
            fs::read_to_string(artifact_dir.path().join(&results.commands[0].stdout_log))
                .unwrap()
                .contains("clean-output")
        );
        assert_eq!(results.tests[0].id, "clean");
        assert!(
            results.commands[0]
                .process_settlement
                .as_ref()
                .unwrap()
                .leaks
                .is_empty()
        );
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

        let artifact = TempDir::new().unwrap();
        persist_harness_error(
            artifact.path(),
            "nested_swiftpm_sandbox",
            "nested sandbox".to_string(),
            "details".to_string(),
            Vec::new(),
        )
        .unwrap();
        assert!(is_nested_swiftpm_harness_error(artifact.path()));
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
    fn producer_identity_cannot_weaken_native_qualification() {
        for (raw, complete, partials) in [
            (
                "src/native.rs (pkg::lib$module::works)",
                serde_json::json!({"kind":"rust","path":"src/native.rs","binary":"pkg::lib","name":"module::works"}),
                vec![
                    serde_json::json!({"kind":"python","name":"works"}),
                    serde_json::json!({"kind":"python","path":"tests/native.py","class":"Native","name":"works"}),
                    serde_json::json!({"kind":"rust","binary":"pkg::lib","name":"module::works"}),
                    serde_json::json!({"kind":"rust","path":"src/native.rs","name":"module::works"}),
                    serde_json::json!({"kind":"rust","path":"src/native.rs","binary":"pkg::lib","name":"works"}),
                    serde_json::json!({"kind":"rust","path":"src/native.rs","binary":"other","name":"module::works"}),
                ],
            ),
            (
                "tests/native.py::module.Native.works",
                serde_json::json!({"kind":"python","path":"tests/native.py","module":"module","class":"Native","name":"works"}),
                vec![
                    serde_json::json!({"kind":"rust","name":"works"}),
                    serde_json::json!({"kind":"rust","path":"src/native.rs","binary":"pkg::lib","name":"module::works"}),
                    serde_json::json!({"kind":"python","module":"module","class":"Native","name":"works"}),
                    serde_json::json!({"kind":"python","path":"tests/native.py","class":"Native","name":"works"}),
                    serde_json::json!({"kind":"python","path":"tests/native.py","name":"works"}),
                    serde_json::json!({"kind":"python","path":"tests/native.py","module":"other","class":"Native","name":"works"}),
                ],
            ),
            (
                "tests/other.sh::works",
                serde_json::json!({"kind":"named-test","path":"tests/other.sh","name":"works"}),
                vec![
                    serde_json::json!({"kind":"rust","name":"works"}),
                    serde_json::json!({"kind":"python","name":"works"}),
                ],
            ),
            (
                "tests/other.sh",
                serde_json::json!({"kind":"whole-script","path":"tests/other.sh"}),
                vec![
                    serde_json::json!({"kind":"rust","name":"works"}),
                    serde_json::json!({"kind":"python","name":"works"}),
                ],
            ),
            (
                "works",
                serde_json::json!({"kind":"rust","name":"works"}),
                vec![],
            ),
            (
                "works",
                serde_json::json!({"kind":"python","name":"works"}),
                vec![],
            ),
            (
                "producer display label",
                serde_json::json!({"kind":"rust","name":"works"}),
                vec![],
            ),
            (
                "producer display label",
                serde_json::json!({"kind":"python","name":"works"}),
                vec![],
            ),
        ] {
            for (identity, valid) in std::iter::once((complete, true))
                .chain(partials.into_iter().map(|identity| (identity, false)))
            {
                let workspace = TempDir::new().unwrap();
                let artifacts = TempDir::new().unwrap();
                make_workspace(workspace.path());
                write_tester_yaml(
                    workspace.path(),
                    "commands:\n  - command: true\n    test_harness: shell-harness\n",
                );
                let rows = serde_json::json!([{"id":raw,"identity":identity,"status":"pass","test_harness":"native"}]);
                write_extractor(
                    workspace.path(),
                    &format!("#!/bin/sh\ncat <<'RESULTS'\n{rows}\nRESULTS\n"),
                );
                run(workspace.path(), artifacts.path(), true, &resolver()).unwrap();
                let result = read_results(artifacts.path());
                if valid {
                    assert!(result.error.is_none());
                    assert_eq!(result.tests[0].id, raw);
                    assert_eq!(
                        result.tests[0].identity.as_ref().unwrap(),
                        &serde_json::from_value::<crate::verification_reference::NativeTestIdentity>(identity).unwrap()
                    );
                } else {
                    assert_eq!(
                        result.error.as_ref().map(|error| error.kind.as_str()),
                        Some("invalid_test_identity"),
                        "{raw}: {identity}"
                    );
                }
            }
        }
    }

    #[test]
    fn diagnosed_test_and_harness_defects_are_not_ambiguous() {
        let mut results = TesterResults {
            candidate_commit: None,
            commands: vec![command_result("false", "shell-harness", 1)],
            tests: vec![],
            summary: Summary {
                total: 0,
                pass: 0,
                fail: 0,
                skipped: 0,
            },
            error: None,
        };
        assert!(results.needs_failure_diagnosis());
        results.tests.push(TestResult {
            identity: None,
            id: "assertion".into(),
            test_harness: "shell-harness".into(),
            status: "fail".into(),
            duration_ms: None,
            failure_excerpt: Some("actual != expected".into()),
        });
        assert!(!results.needs_failure_diagnosis());
        results.tests.clear();
        results.error = Some(TesterError {
            kind: "extractor_missing".into(),
            message: "missing executable".into(),
            details: String::new(),
        });
        assert!(!results.needs_failure_diagnosis());
    }

    #[test]
    fn tester_records_prerequisite_that_prevented_execution() {
        let workspace = TempDir::new().unwrap();
        let artifact_dir = TempDir::new().unwrap();
        make_workspace(workspace.path());
        let required = if std::env::consts::OS == "linux" {
            "macos"
        } else {
            "linux"
        };
        write_tester_yaml(
            workspace.path(),
            &format!(
                "commands:\n  - command: touch not-run\n    test_harness: shell-harness\n    applicability:\n      environment: {{os: {required}}}\n  - command: touch ran\n    test_harness: shell-harness\n"
            ),
        );
        write_extractor(workspace.path(), "#!/bin/sh\necho '[]'\n");
        run(workspace.path(), artifact_dir.path(), true, &resolver()).unwrap();
        assert!(!workspace.path().join("not-run").exists());
        assert!(workspace.path().join("ran").exists());
        let results = read_results(artifact_dir.path());
        assert_ne!(results.commands[0].exit_code, 0);
        assert_eq!(results.commands[1].exit_code, 0);
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

    #[test]
    fn tester_sandbox_reads_but_cannot_write_linked_worktree_git_metadata() {
        let tmp = TempDir::new().unwrap();
        let main = tmp.path().join("main");
        fs::create_dir_all(&main).unwrap();
        crate::git::run(&main, &["init", "-q"], "initialize Tester fixture").unwrap();
        crate::git::run(
            &main,
            &["config", "user.name", "Test Author"],
            "configure Tester fixture author",
        )
        .unwrap();
        crate::git::run(
            &main,
            &["config", "user.email", "test@example.com"],
            "configure Tester fixture email",
        )
        .unwrap();
        fs::write(main.join("tracked.txt"), "base\n").unwrap();
        crate::git::run(&main, &["add", "tracked.txt"], "stage Tester fixture").unwrap();
        crate::git::run(&main, &["commit", "-qm", "Base"], "commit Tester fixture").unwrap();

        let candidate = tmp.path().join("candidate");
        crate::git::run(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "tester-candidate",
                candidate.to_str().unwrap(),
                "HEAD",
            ],
            "create linked Tester candidate",
        )
        .unwrap();
        let common = tester_git_metadata_root(&candidate).unwrap();
        assert_eq!(common, main.join(".git").canonicalize().unwrap());

        let artifact = tmp.path().join("artifact");
        let home = tmp.path().join("home");
        fs::create_dir_all(&artifact).unwrap();
        fs::create_dir_all(&home).unwrap();
        let writable = tester_writable_roots(&candidate, &artifact, &home);
        let profile = os::render_profile_common_only_with_denied_access(
            &ContentResolver::new(None),
            &home.to_string_lossy(),
            &writable,
            std::slice::from_ref(&common),
            &[],
            std::slice::from_ref(&common),
        )
        .unwrap();
        let content = fs::read_to_string(profile.path).unwrap();
        assert!(content.contains(&format!(
            "(allow file-read*  (subpath \"{}\"))",
            common.display()
        )));
        assert!(content.contains(&format!(
            "(deny file-write* (subpath \"{}\"))",
            common.display()
        )));
        assert!(!content.contains(&format!(
            "(allow file-write* (subpath \"{}\"))",
            common.display()
        )));
    }
}
