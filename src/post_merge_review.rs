//! Post-merge review: deterministic merges queue a review-only Attempt
//! against the target branch's current HEAD. Reviews fan out in
//! parallel (using the existing review skill set). Findings auto-create
//! a post-merge-review-fix Work Item that runs through the write→review loop and
//! auto-merges on pass.
//!
//! The merge command spawns a detached child that sleeps a debounce
//! window before running. Multiple merges within the window coalesce —
//! the latest child sees the latest entry and reviews the cumulative
//! range; earlier children find newer entries and exit.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::coder::CoderKind;
use crate::content::ContentResolver;
use crate::git;
use crate::review;
use crate::work_attempt_loop::{self, WorkAttemptRunConfig, WorkAttemptRunOutcome};
use crate::work_merge_executor::{self, WorkMergeConfig};
use crate::work_model::{
    ArtifactRef, AttemptStatus, PlanningContext, TaskKind, TaskStatus, WorkItem, WorkModelStore,
};

const DEFAULT_DEBOUNCE_SECONDS: u64 = 60;
const DEFAULT_MAX_POST_MERGE_REVIEW_FIX_DEPTH: u64 = 5;
const POST_MERGE_REVIEW_FIX_DEPTH_ENV: &str = "FLUENT_POST_MERGE_REVIEW_FIX_DEPTH";

pub fn debounce_seconds() -> u64 {
    std::env::var("FLUENT_POST_MERGE_DEBOUNCE_SECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_DEBOUNCE_SECONDS)
}

pub fn max_post_merge_review_fix_depth() -> u64 {
    std::env::var("FLUENT_MAX_POST_MERGE_REVIEW_FIX_DEPTH")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAX_POST_MERGE_REVIEW_FIX_DEPTH)
}

pub fn current_depth() -> u64 {
    std::env::var(POST_MERGE_REVIEW_FIX_DEPTH_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
}

pub fn fix_depth_for(wi: &WorkItem) -> u64 {
    wi.post_merge_review_fix_depth.unwrap_or(0)
}

pub fn should_spawn_post_merge_review(fix_depth: u64) -> bool {
    fix_depth < max_post_merge_review_fix_depth()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueueEntry {
    pub target_branch: String,
    pub merged_commit: String,
    pub merged_at_unix: u64,
    pub source_work_item_id: String,
    pub source_merge_candidate_id: String,
    #[serde(default)]
    pub base_commit: String,
    #[serde(default)]
    pub fix_depth: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Queue {
    #[serde(default)]
    pub entries: Vec<QueueEntry>,
}

pub fn queue_path(project_root: &Path) -> PathBuf {
    project_root.join(".fluent/work/post-merge-review-queue.json")
}

pub fn append_entry(project_root: &Path, entry: QueueEntry) -> Result<()> {
    let path = queue_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("create post-merge-review queue directory")?;
    }
    let mut queue = load_queue(project_root)?;
    queue.entries.push(entry);
    save_queue(project_root, &queue)
}

pub fn load_queue(project_root: &Path) -> Result<Queue> {
    let path = queue_path(project_root);
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).context("parse post-merge-review queue"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Queue::default()),
        Err(error) => Err(error).context("read post-merge-review queue"),
    }
}

pub fn save_queue(project_root: &Path, queue: &Queue) -> Result<()> {
    let path = queue_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("create post-merge-review queue directory")?;
    }
    let json = serde_json::to_string_pretty(queue).context("serialize post-merge-review queue")?;
    fs::write(&path, json).context("write post-merge-review queue")
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Spawn a detached Fluent subprocess that will run the post-merge
/// review after the debounce window. Standard streams are redirected
/// to a log file so the parent merge command can return immediately.
///
/// The `fix_depth` parameter controls recursion bounding: when it
/// reaches the cap, the spawn is skipped entirely.
pub fn spawn_detached_runner(
    project_root: &Path,
    debounce_secs: u64,
    fix_depth: u64,
) -> Result<()> {
    if !should_spawn_post_merge_review(fix_depth) {
        eprintln!(
            "  Skipping post-merge review spawn: fix depth {fix_depth} >= cap {}",
            max_post_merge_review_fix_depth()
        );
        return Ok(());
    }
    let log_dir = project_root.join(".fluent/work");
    fs::create_dir_all(&log_dir).context("create work dir for post-merge log")?;
    let log_path = log_dir.join("post-merge-review.log");
    let log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open log {}", log_path.display()))?;
    let log_clone = log_file
        .try_clone()
        .context("clone log handle for stderr")?;
    crate::credential::force_refresh_oauth_token()
        .context("force fresh OAuth token before detached spawn")?;
    let fluent_bin = std::env::current_exe().context("locate fluent binary")?;
    let mut cmd = Command::new(&fluent_bin);
    cmd.current_dir(project_root)
        .args([
            "post-merge-review",
            "--debounce-seconds",
            &debounce_secs.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_clone));
    cmd.spawn()
        .with_context(|| format!("spawn detached {fluent_bin:?} for post-merge review"))?;
    Ok(())
}

/// Run pending post-merge reviews. Called by CLI / detached child.
///
/// For each unique target branch, finds the latest queued entry. If
/// the entry is at least `debounce_secs` old, processes it (clears all
/// entries for that branch through this entry); otherwise leaves the
/// queue alone — the child responsible for the later entry will handle
/// it.
pub fn run(
    project_root: &Path,
    debounce_secs: u64,
    target_filter: Option<&str>,
) -> Result<RunOutcome> {
    // Wait the debounce so that follow-up merges within the window
    // can coalesce into the same review pass.
    if debounce_secs > 0 {
        std::thread::sleep(std::time::Duration::from_secs(debounce_secs));
    }

    let mut queue = load_queue(project_root)?;
    let now = now_unix();
    let mut outcome = RunOutcome::default();

    auto_prune_orphan_review_only_worktrees(project_root);

    // Find candidate branches: latest entry per branch that's at least
    // debounce_secs old, and track the maximum fix_depth across all
    // coalesced entries per branch.
    let mut latest_by_branch: std::collections::BTreeMap<String, QueueEntry> =
        std::collections::BTreeMap::new();
    let mut max_depth_by_branch: std::collections::BTreeMap<String, u64> =
        std::collections::BTreeMap::new();
    for entry in &queue.entries {
        if let Some(filter) = target_filter
            && filter != entry.target_branch
        {
            continue;
        }
        let depth = max_depth_by_branch
            .entry(entry.target_branch.clone())
            .or_insert(0);
        if entry.fix_depth > *depth {
            *depth = entry.fix_depth;
        }
        let pending = latest_by_branch.get(&entry.target_branch);
        if pending
            .map(|p| p.merged_at_unix < entry.merged_at_unix)
            .unwrap_or(true)
        {
            latest_by_branch.insert(entry.target_branch.clone(), entry.clone());
        }
    }

    let mut branches_to_process: Vec<(QueueEntry, u64)> = Vec::new();
    for (_branch, entry) in latest_by_branch {
        if now.saturating_sub(entry.merged_at_unix) >= debounce_secs {
            let depth = max_depth_by_branch
                .get(&entry.target_branch)
                .copied()
                .unwrap_or(0);
            branches_to_process.push((entry, depth));
        }
    }

    if branches_to_process.is_empty() {
        return Ok(outcome);
    }

    let mut succeeded_branches: Vec<&QueueEntry> = Vec::new();
    let inflight_store = WorkModelStore::new(project_root);
    for (entry, fix_depth) in &branches_to_process {
        match crate::review_only_worktree::detect_in_flight(
            &inflight_store,
            &entry.target_branch,
            None,
        ) {
            Ok(Some(in_flight)) => {
                eprintln!(
                    "  Deferring post-merge review for {}: review-only worktree in flight via Work Item {:?} Attempt {:?}",
                    entry.target_branch, in_flight.work_item_id, in_flight.attempt_id
                );
                continue;
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!(
                    "  In-flight check for {} failed: {error}",
                    entry.target_branch
                );
                outcome.errors.push(format!(
                    "branch {}: in-flight check: {error}",
                    entry.target_branch
                ));
                continue;
            }
        }
        eprintln!(
            "  Running post-merge review for {} at {}",
            entry.target_branch, entry.merged_commit
        );
        match review_one(project_root, entry, *fix_depth) {
            Ok(per) => {
                succeeded_branches.push(entry);
                outcome.reviewed.push(per);
            }
            Err(error) => {
                eprintln!(
                    "  Post-merge review for {} failed: {error}",
                    entry.target_branch
                );
                outcome
                    .errors
                    .push(format!("branch {}: {error}", entry.target_branch));
            }
        }
    }

    queue.entries.retain(|entry| {
        !succeeded_branches.iter().any(|p| {
            p.target_branch == entry.target_branch && entry.merged_at_unix <= p.merged_at_unix
        })
    });
    save_queue(project_root, &queue)?;

    Ok(outcome)
}

#[derive(Debug, Default)]
pub struct RunOutcome {
    pub reviewed: Vec<PerBranchOutcome>,
    pub errors: Vec<String>,
}

#[derive(Debug)]
pub struct PerBranchOutcome {
    pub target_branch: String,
    pub merged_commit: String,
    pub findings: Vec<ArtifactRef>,
    pub post_merge_review_fix_work_item: Option<String>,
}

/// Best-effort orphan cleanup at the start of every post-merge review
/// queue pass. Reuses the manual-prune path with default (orphan-only)
/// semantics: live worktrees are left alone, and in-use worktrees are
/// skipped regardless of orphan status. Failures are logged but do not
/// abort the queue pass — orphan cleanup is convenience, not safety.
fn auto_prune_orphan_review_only_worktrees(project_root: &Path) {
    let store = WorkModelStore::new(project_root);
    let options = crate::review_only_worktree::PruneOptions {
        all: false,
        dry_run: false,
    };
    match crate::review_only_worktree::prune(&store, project_root, options) {
        Ok(report) => {
            let mut removed = 0_usize;
            let mut skipped_in_use = 0_usize;
            for entry in &report.entries {
                match entry {
                    crate::review_only_worktree::PruneEntry::Removed { path } => {
                        removed += 1;
                        eprintln!(
                            "  Auto-pruned orphan review-only worktree {}",
                            path.display()
                        );
                    }
                    crate::review_only_worktree::PruneEntry::SkippedInUse { path, in_flight } => {
                        skipped_in_use += 1;
                        eprintln!(
                            "  Auto-prune skipped in-use review-only worktree {} (Work Item {:?} Attempt {:?})",
                            path.display(),
                            in_flight.work_item_id,
                            in_flight.attempt_id
                        );
                    }
                    _ => {}
                }
            }
            if removed > 0 || skipped_in_use > 0 {
                eprintln!("  Auto-prune: removed {removed}, skipped in-use {skipped_in_use}");
            }
        }
        Err(error) => {
            eprintln!("  Auto-prune of review-only worktrees failed: {error:#}");
        }
    }
}

fn review_one(project_root: &Path, entry: &QueueEntry, fix_depth: u64) -> Result<PerBranchOutcome> {
    let store = WorkModelStore::new(project_root);
    let short = &entry.merged_commit[..8.min(entry.merged_commit.len())];
    let work_item_id = format!(
        "post-merge-{}-{}",
        entry.target_branch.replace('/', "-"),
        short
    );

    if store.read_work_item(&work_item_id).is_ok() {
        return Ok(PerBranchOutcome {
            target_branch: entry.target_branch.clone(),
            merged_commit: entry.merged_commit.clone(),
            findings: Vec::new(),
            post_merge_review_fix_work_item: None,
        });
    }

    let brief = format!(
        "Post-merge review of `{}` at commit `{}`. Triggered by Work Item `{}` merging into `{}`.",
        entry.target_branch, short, entry.source_work_item_id, entry.target_branch
    );
    let mut item = WorkItem {
        id: work_item_id.clone(),
        title: format!("Post-merge review of {} at {}", entry.target_branch, short),
        planning_context: Some(PlanningContext {
            brief: Some(brief),
            behaviors: None,
            approach: None,
            plan: None,
            combined: None,
        }),
        instructions: None,
        abandonment: None,
        post_merge_review_fix_depth: None,
        attempts: Vec::new(),
        merge_candidates: Vec::new(),
    };
    let attempt_id = "attempt-1";
    let base_commit = if entry.base_commit.is_empty() {
        None
    } else {
        Some(entry.base_commit.clone())
    };
    item.add_post_merge_review_attempt(
        attempt_id,
        review::REVIEWERS,
        &entry.target_branch,
        &entry.merged_commit,
        base_commit,
    )
    .map_err(|e| anyhow::anyhow!("create post-merge review Attempt: {e}"))?;
    store
        .create_work_item(&item)
        .map_err(|e| anyhow::anyhow!("write post-merge review Work Item: {e}"))?;

    let resolver = ContentResolver::new(Some(project_root));
    if let Err(error) = work_attempt_loop::run_attempt(WorkAttemptRunConfig {
        project_root,
        store: &store,
        work_item_id: &work_item_id,
        attempt_id,
        resolver: &resolver,
        extra_args: &[],
        no_sandbox: true,
    }) {
        eprintln!(
            "  Post-merge review for {} failed: {error:#}",
            entry.target_branch
        );
    }

    let item = store
        .read_work_item(&work_item_id)
        .map_err(|e| anyhow::anyhow!("read post-merge review Work Item: {e}"))?;
    let attempt = item
        .attempts
        .iter()
        .find(|a| a.id == attempt_id)
        .ok_or_else(|| anyhow::anyhow!("Attempt {attempt_id} not found"))?;

    let mut findings = Vec::new();
    for task in &attempt.tasks {
        if task.kind != TaskKind::Review {
            continue;
        }
        if task.status != TaskStatus::Complete && task.status != TaskStatus::Failed {
            continue;
        }
        let Some(area) = task.artifact_area.as_ref() else {
            continue;
        };
        let review_path = project_root.join(&area.path).join("review.md");
        let content = match fs::read_to_string(&review_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        match review::extract_verdict(&content) {
            review::Verdict::Fail | review::Verdict::Uncertain => {
                findings.push(ArtifactRef {
                    producer_id: task.id.clone(),
                    path: format!("{}/review.md", area.path),
                });
            }
            review::Verdict::Pass => {}
        }
    }

    let review_tasks: Vec<_> = attempt
        .tasks
        .iter()
        .filter(|t| t.kind == TaskKind::Review)
        .collect();
    if !review_tasks.is_empty()
        && review_tasks.iter().all(|t| t.status == TaskStatus::Failed)
        && findings.is_empty()
    {
        bail!(
            "all review tasks failed without findings for {}; review is stale",
            entry.target_branch
        );
    }

    let post_merge_review_fix_work_item = if findings.is_empty() {
        None
    } else {
        match auto_run_post_merge_review_fix(project_root, &store, &entry.target_branch, &findings, fix_depth)
        {
            Ok(id) => Some(id),
            Err(error) => {
                eprintln!(
                    "  Forward-fix auto-run failed for {}: {error}; \
                     synthetic Work Item {work_item_id:?} left intact for inspection",
                    entry.target_branch
                );
                None
            }
        }
    };

    Ok(PerBranchOutcome {
        target_branch: entry.target_branch.clone(),
        merged_commit: entry.merged_commit.clone(),
        findings,
        post_merge_review_fix_work_item,
    })
}

/// Create a post-merge-review-fix Work Item from review findings, run its first
/// Attempt, and (on Merge Candidate ready) auto-merge it. Recursion is
/// bounded by `FLUENT_MAX_POST_MERGE_REVIEW_FIX_DEPTH`: the spawned post-merge
/// review for the auto-merge runs with depth+1 in its environment.
fn auto_run_post_merge_review_fix(
    project_root: &Path,
    store: &WorkModelStore,
    parent_branch: &str,
    findings: &[ArtifactRef],
    fix_depth: u64,
) -> Result<String> {
    let id = create_post_merge_review_fix_work_item(store, parent_branch, findings, fix_depth)?;
    let head_before = git::run_stdout(
        project_root,
        &["rev-parse", parent_branch],
        "record target branch HEAD before fix",
    )?;
    let resolver = ContentResolver::new(Some(project_root));
    let coder_kind = CoderKind::resolve(None)?;
    let run_result = work_attempt_loop::run_attempt(WorkAttemptRunConfig {
        project_root,
        store,
        work_item_id: &id,
        attempt_id: "attempt-1",
        resolver: &resolver,
        extra_args: &[],
        no_sandbox: false,
    })?;
    let mut had_merge_candidate = false;
    for outcome in &run_result.outcomes {
        if let WorkAttemptRunOutcome::MergeCandidateReady { candidate_id } = outcome {
            had_merge_candidate = true;
            // Bump the post-merge-review-fix depth so a spawned post-merge-review
            // child sees the new depth and can stop recursing eventually.
            let next_depth = current_depth() + 1;
            unsafe {
                std::env::set_var(POST_MERGE_REVIEW_FIX_DEPTH_ENV, next_depth.to_string());
            }
            let merge_config = WorkMergeConfig {
                project_root,
                store,
                work_item_id: &id,
                merge_candidate_id: candidate_id,
                resolver: &resolver,
                extra_args: &[],
                coder_kind,
                no_sandbox: true,
                skip_post_merge_review: false,
            };
            if let Err(error) = work_merge_executor::merge_candidate(merge_config) {
                eprintln!("  Forward-fix auto-merge failed: {error}");
            }
            break;
        }
    }
    reconcile_impossible_state(
        project_root,
        store,
        &id,
        "attempt-1",
        parent_branch,
        &head_before,
        had_merge_candidate,
    );
    Ok(id)
}

/// Defense-in-depth: if the target branch advanced during the fix
/// Attempt without the Attempt producing a Merge Candidate, something
/// bypassed the normal land path. Mark the Attempt `needs-user` so the
/// operator gets a diagnostic instead of a bare `failed`.
fn reconcile_impossible_state(
    project_root: &Path,
    store: &WorkModelStore,
    work_item_id: &str,
    attempt_id: &str,
    target_branch: &str,
    head_before: &str,
    had_merge_candidate: bool,
) {
    if had_merge_candidate {
        return;
    }
    let head_after = match git::run_stdout(
        project_root,
        &["rev-parse", target_branch],
        "resolve target branch HEAD after fix",
    ) {
        Ok(h) => h,
        Err(_) => return,
    };
    if head_after.trim() == head_before.trim() {
        return;
    }
    eprintln!(
        "  reconcile_impossible_state: target branch {target_branch} advanced \
         from {} to {} without a Merge Candidate; marking {work_item_id} as needs-user",
        head_before.trim(),
        head_after.trim(),
    );
    let mut item = match store.read_work_item(work_item_id) {
        Ok(item) => item,
        Err(_) => return,
    };
    if let Some(attempt) = item.attempts.iter_mut().find(|a| a.id == attempt_id) {
        crate::work_model::set_attempt_terminal(attempt, AttemptStatus::NeedsUser);
    }
    let _ = store.write_work_item(&item);
}

/// Forward-fix Work Item creation helper. Builds a Work Item whose
/// planning context describes the failed findings from the post-merge
/// review. The caller invokes the normal attempt loop on it.
pub fn create_post_merge_review_fix_work_item(
    store: &WorkModelStore,
    parent_branch: &str,
    findings: &[ArtifactRef],
    fix_depth: u64,
) -> Result<String> {
    let id = format!(
        "post-merge-review-fix-{}-{}",
        parent_branch.replace('/', "-"),
        now_unix()
    );
    let brief = format_findings_as_brief(findings);
    let planning_context = PlanningContext {
        brief: Some(brief),
        behaviors: None,
        approach: None,
        plan: None,
        combined: None,
    };
    let mut item = WorkItem {
        id: id.clone(),
        title: format!("Post-merge post-merge-review fix for {parent_branch}"),
        planning_context: Some(planning_context),
        instructions: None,
        abandonment: None,
        post_merge_review_fix_depth: Some(fix_depth + 1),
        attempts: Vec::new(),
        merge_candidates: Vec::new(),
    };
    item.add_initial_attempt("attempt-1")
        .map_err(|e| anyhow::anyhow!("post-merge-review-fix add_initial_attempt: {e}"))?;
    store
        .create_work_item(&item)
        .map_err(|e| anyhow::anyhow!("write post-merge-review-fix Work Item: {e}"))?;
    Ok(id)
}

fn format_findings_as_brief(findings: &[ArtifactRef]) -> String {
    let mut text = String::from(
        "Post-merge review surfaced findings against the merged HEAD. Address each finding by following the write→review loop.\n\nFindings:\n",
    );
    for artifact in findings {
        text.push_str(&format!("- {}\n", artifact.path));
    }
    text
}

/// Append the current entry to the queue and spawn the detached
/// post-merge review runner. Called by the merge executor right after
/// a successful fast-forward.
pub fn queue_and_spawn(
    project_root: &Path,
    entry: QueueEntry,
    debounce_secs: u64,
    fix_depth: u64,
) -> Result<()> {
    append_entry(project_root, entry)?;
    spawn_detached_runner(project_root, debounce_secs, fix_depth)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn append_and_load_round_trip() {
        let tmp = TempDir::new().unwrap();
        let entry = QueueEntry {
            target_branch: "main".into(),
            merged_commit: "abc".into(),
            merged_at_unix: 42,
            source_work_item_id: "work-1".into(),
            source_merge_candidate_id: "attempt-1-merge-candidate".into(),
            base_commit: String::new(),
            fix_depth: 0,
        };
        append_entry(tmp.path(), entry.clone()).unwrap();
        let queue = load_queue(tmp.path()).unwrap();
        assert_eq!(queue.entries.len(), 1);
        assert_eq!(queue.entries[0], entry);
    }

    #[test]
    fn run_skips_when_within_debounce_window() {
        let tmp = TempDir::new().unwrap();
        let now = now_unix();
        append_entry(
            tmp.path(),
            QueueEntry {
                target_branch: "main".into(),
                merged_commit: "abc".into(),
                merged_at_unix: now,
                source_work_item_id: "work-1".into(),
                source_merge_candidate_id: "attempt-1-merge-candidate".into(),
                base_commit: String::new(),
                fix_depth: 0,
            },
        )
        .unwrap();
        // debounce 0 + immediate run → should process
        let outcome = run(tmp.path(), 0, None).unwrap();
        assert_eq!(outcome.reviewed.len(), 1);
        let queue = load_queue(tmp.path()).unwrap();
        assert!(queue.entries.is_empty());
    }

    #[test]
    fn run_coalesces_per_branch() {
        let tmp = TempDir::new().unwrap();
        let now = now_unix();
        for n in 0..3 {
            append_entry(
                tmp.path(),
                QueueEntry {
                    target_branch: "main".into(),
                    merged_commit: format!("commit-{n}"),
                    merged_at_unix: now - 5 + n,
                    source_work_item_id: format!("work-{n}"),
                    source_merge_candidate_id: format!("attempt-{n}-merge-candidate"),
                    base_commit: String::new(),
                    fix_depth: 0,
                },
            )
            .unwrap();
        }
        let outcome = run(tmp.path(), 0, None).unwrap();
        assert_eq!(outcome.reviewed.len(), 1);
        let queue = load_queue(tmp.path()).unwrap();
        assert!(queue.entries.is_empty());
    }

    fn find_no_sandbox_value_after(source: &str, anchor: &str, context: &str) -> String {
        let anchor_pos = source
            .find(anchor)
            .unwrap_or_else(|| panic!("{context}: anchor {anchor:?} not found"));
        let after = &source[anchor_pos..];
        let field_pos = after
            .find("no_sandbox:")
            .unwrap_or_else(|| panic!("{context}: no_sandbox field not found after anchor"));
        let field_text = &after[field_pos..field_pos + 30.min(after.len() - field_pos)];
        field_text.to_string()
    }

    #[test]
    fn post_merge_fix_attempt_runs_sandboxed() {
        let source = include_str!("post_merge_review.rs");
        let field =
            find_no_sandbox_value_after(source, "fn auto_run_post_merge_review_fix", "fix attempt");
        assert!(
            field.contains("false"),
            "post-merge-review-fix Attempt must run sandboxed (no_sandbox: false), found: {field}"
        );
    }

    #[test]
    fn post_merge_fix_merge_step_remains_unsandboxed() {
        let source = include_str!("post_merge_review.rs");
        let field =
            find_no_sandbox_value_after(source, "let merge_config = WorkMergeConfig", "merge step");
        assert!(
            field.contains("true"),
            "merge step must remain unsandboxed (no_sandbox: true), found: {field}"
        );
    }

    fn init_git_repo(dir: &Path) {
        crate::git::run(dir, &["init", "-b", "main"], "init").unwrap();
        crate::git::run(dir, &["config", "user.email", "test@test"], "cfg email").unwrap();
        crate::git::run(dir, &["config", "user.name", "Test"], "cfg name").unwrap();
        std::fs::write(dir.join("README.md"), "init").unwrap();
        crate::git::run(dir, &["add", "README.md"], "add").unwrap();
        crate::git::run(dir, &["commit", "-m", "initial"], "commit").unwrap();
    }

    #[test]
    fn reconcile_marks_needs_user_when_target_advanced_without_candidate() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let store = WorkModelStore::new(tmp.path());
        let head_before =
            crate::git::run_stdout(tmp.path(), &["rev-parse", "main"], "get head").unwrap();

        let mut item = WorkItem {
            id: "fix-1".to_string(),
            title: "test fix".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        item.add_initial_attempt("attempt-1").unwrap();
        store.create_work_item(&item).unwrap();

        // Simulate the target branch advancing (a commit lands on main)
        std::fs::write(tmp.path().join("extra.txt"), "change").unwrap();
        crate::git::run(tmp.path(), &["add", "extra.txt"], "add").unwrap();
        crate::git::run(tmp.path(), &["commit", "-m", "advance"], "commit").unwrap();

        reconcile_impossible_state(
            tmp.path(),
            &store,
            "fix-1",
            "attempt-1",
            "main",
            &head_before,
            false,
        );

        let stored = store.read_work_item("fix-1").unwrap();
        assert_eq!(
            stored.attempts[0].status,
            AttemptStatus::NeedsUser,
            "target advanced without Merge Candidate → needs-user"
        );
    }

    #[test]
    fn reconcile_is_noop_when_merge_candidate_produced() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let store = WorkModelStore::new(tmp.path());
        let head_before =
            crate::git::run_stdout(tmp.path(), &["rev-parse", "main"], "get head").unwrap();

        let mut item = WorkItem {
            id: "fix-2".to_string(),
            title: "test fix".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        item.add_initial_attempt("attempt-1").unwrap();
        store.create_work_item(&item).unwrap();

        // Target branch advances, but we had a merge candidate
        std::fs::write(tmp.path().join("extra.txt"), "change").unwrap();
        crate::git::run(tmp.path(), &["add", "extra.txt"], "add").unwrap();
        crate::git::run(tmp.path(), &["commit", "-m", "advance"], "commit").unwrap();

        reconcile_impossible_state(
            tmp.path(),
            &store,
            "fix-2",
            "attempt-1",
            "main",
            &head_before,
            true,
        );

        let stored = store.read_work_item("fix-2").unwrap();
        assert_eq!(
            stored.attempts[0].status,
            AttemptStatus::Planned,
            "had_merge_candidate=true → reconcile is a no-op"
        );
    }

    #[test]
    fn reconcile_is_noop_when_target_unchanged() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let store = WorkModelStore::new(tmp.path());
        let head_before =
            crate::git::run_stdout(tmp.path(), &["rev-parse", "main"], "get head").unwrap();

        let mut item = WorkItem {
            id: "fix-3".to_string(),
            title: "test fix".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        item.add_initial_attempt("attempt-1").unwrap();
        store.create_work_item(&item).unwrap();

        // Target branch does NOT advance, no merge candidate
        reconcile_impossible_state(
            tmp.path(),
            &store,
            "fix-3",
            "attempt-1",
            "main",
            &head_before,
            false,
        );

        let stored = store.read_work_item("fix-3").unwrap();
        assert_eq!(
            stored.attempts[0].status,
            AttemptStatus::Planned,
            "target unchanged → reconcile is a no-op"
        );
    }

    #[test]
    fn spawn_detached_runner_forces_fresh_credential() {
        let source = include_str!("post_merge_review.rs");
        let fn_start = source
            .find("fn spawn_detached_runner(")
            .expect("spawn_detached_runner function not found");
        let body = &source[fn_start..];
        let refresh_pos = body
            .find("force_refresh_oauth_token()")
            .expect("force_refresh_oauth_token() call not found in spawn_detached_runner");
        let spawn_pos = body
            .find("cmd.spawn()")
            .expect("cmd.spawn() call not found in spawn_detached_runner");
        assert!(
            refresh_pos < spawn_pos,
            "force_refresh_oauth_token() must precede cmd.spawn() in spawn_detached_runner"
        );
    }

    #[test]
    fn no_source_branch_mutation_guard_on_general_write_path() {
        let source = include_str!("work_task_executor.rs");
        assert!(
            !source.contains("ensure_source_branch_unchanged"),
            "general write path must NOT have a source-branch mutation guard"
        );
    }

    #[test]
    fn post_merge_fix_landed_records_merged_not_failed() {
        let source = include_str!("post_merge_review.rs");

        // The fix Attempt is sandboxed — the coder cannot write to the target
        // branch directly, so the ONLY way to land is through merge_candidate
        // which records the Work Item as merged.
        let fix_field = find_no_sandbox_value_after(
            source,
            "fn auto_run_post_merge_review_fix",
            "fix attempt sandbox",
        );
        assert!(
            fix_field.contains("false"),
            "fix Attempt must be sandboxed (no_sandbox: false): {fix_field}"
        );

        // The merge step calls work_merge_executor::merge_candidate, which
        // records the Work Item as merged on success.
        assert!(
            source.contains("work_merge_executor::merge_candidate(merge_config)"),
            "auto_run_post_merge_review_fix must land through work_merge_executor::merge_candidate"
        );

        // When a merge candidate WAS produced, reconcile_impossible_state is a
        // no-op — it does not overwrite the merged status set by the merge executor.
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let store = WorkModelStore::new(tmp.path());
        let head_before =
            crate::git::run_stdout(tmp.path(), &["rev-parse", "main"], "head").unwrap();

        let mut item = WorkItem {
            id: "b1-fix".to_string(),
            title: "B1 fix".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        item.add_initial_attempt("attempt-1").unwrap();
        store.create_work_item(&item).unwrap();

        // Target advanced (as it would after a successful merge_candidate call)
        std::fs::write(tmp.path().join("fix.txt"), "landed").unwrap();
        crate::git::run(tmp.path(), &["add", "fix.txt"], "add").unwrap();
        crate::git::run(tmp.path(), &["commit", "-m", "fix landed"], "commit").unwrap();

        reconcile_impossible_state(
            tmp.path(),
            &store,
            "b1-fix",
            "attempt-1",
            "main",
            &head_before,
            true,
        );

        let stored = store.read_work_item("b1-fix").unwrap();
        assert_eq!(
            stored.attempts[0].status,
            AttemptStatus::Planned,
            "reconcile must not override status when a merge candidate was produced"
        );
    }

    #[test]
    fn post_merge_fix_that_does_not_pass_leaves_target_branch_unchanged() {
        let source = include_str!("post_merge_review.rs");

        // The fix Attempt is sandboxed — the coder's writable roots exclude the
        // target branch's worktree, so a non-passing fix cannot mutate the target.
        let fix_field = find_no_sandbox_value_after(
            source,
            "fn auto_run_post_merge_review_fix",
            "fix attempt sandbox",
        );
        assert!(
            fix_field.contains("false"),
            "fix Attempt must be sandboxed (no_sandbox: false): {fix_field}"
        );

        // If the target branch somehow advanced without a merge candidate,
        // reconcile_impossible_state marks needs-user (never bare failed).
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let store = WorkModelStore::new(tmp.path());
        let head_before =
            crate::git::run_stdout(tmp.path(), &["rev-parse", "main"], "head").unwrap();

        let mut item = WorkItem {
            id: "b2-fix".to_string(),
            title: "B2 fix".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        item.add_initial_attempt("attempt-1").unwrap();
        store.create_work_item(&item).unwrap();

        // Simulate unexpected target advancement without a merge candidate
        std::fs::write(tmp.path().join("rogue.txt"), "unexpected").unwrap();
        crate::git::run(tmp.path(), &["add", "rogue.txt"], "add").unwrap();
        crate::git::run(tmp.path(), &["commit", "-m", "rogue advance"], "commit").unwrap();

        reconcile_impossible_state(
            tmp.path(),
            &store,
            "b2-fix",
            "attempt-1",
            "main",
            &head_before,
            false,
        );

        let stored = store.read_work_item("b2-fix").unwrap();
        assert_eq!(
            stored.attempts[0].status,
            AttemptStatus::NeedsUser,
            "unexpected target advancement must result in needs-user, not bare failed"
        );
    }

    #[test]
    fn fix_work_item_records_incremented_fix_depth() {
        let tmp = TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let findings = vec![ArtifactRef {
            producer_id: "review-1".to_string(),
            path: "reviews/review.md".to_string(),
        }];
        let id = create_post_merge_review_fix_work_item(&store, "main", &findings, 2).unwrap();
        let wi = store.read_work_item(&id).unwrap();
        assert_eq!(
            wi.post_merge_review_fix_depth,
            Some(3),
            "fix Work Item at depth 2 should record depth 3"
        );
    }

    #[test]
    fn fix_depth_defaults_to_zero_for_non_fix_work_item() {
        let wi = WorkItem {
            id: "regular-work".to_string(),
            title: "A regular work item".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        assert_eq!(fix_depth_for(&wi), 0);
    }

    #[test]
    fn does_not_spawn_post_merge_review_at_or_above_fix_depth_cap() {
        let cap = max_post_merge_review_fix_depth();
        assert!(
            should_spawn_post_merge_review(0),
            "depth 0 is below cap"
        );
        assert!(
            should_spawn_post_merge_review(cap - 1),
            "depth just below cap should spawn"
        );
        assert!(
            !should_spawn_post_merge_review(cap),
            "depth at cap should not spawn"
        );
        assert!(
            !should_spawn_post_merge_review(cap + 1),
            "depth above cap should not spawn"
        );
    }
}
