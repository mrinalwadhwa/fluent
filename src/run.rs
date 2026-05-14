use anyhow::{bail, Context, Result};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// Status values a run can have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunStatus {
    Briefed,
    BehaviorsDefined,
    ApproachDesigned,
    Planned,
    Executing,
    RateLimited,
    NeedsUser,
    Complete,
    Failed,
    Unknown(String),
}

impl RunStatus {
    pub fn parse(s: &str) -> Self {
        match s.trim() {
            "briefed" => Self::Briefed,
            "behaviors-defined" => Self::BehaviorsDefined,
            "approach-designed" => Self::ApproachDesigned,
            "planned" => Self::Planned,
            "executing" => Self::Executing,
            "rate-limited" => Self::RateLimited,
            "needs-user" => Self::NeedsUser,
            "complete" => Self::Complete,
            "failed" => Self::Failed,
            other => Self::Unknown(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Briefed => "briefed",
            Self::BehaviorsDefined => "behaviors-defined",
            Self::ApproachDesigned => "approach-designed",
            Self::Planned => "planned",
            Self::Executing => "executing",
            Self::RateLimited => "rate-limited",
            Self::NeedsUser => "needs-user",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::Unknown(s) => s.as_str(),
        }
    }

    /// Whether this status means the run is active and eligible for scanning.
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Planned | Self::Executing)
    }

    /// Whether this status is terminal.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Complete | Self::Failed | Self::NeedsUser)
    }

    /// Whether this status is resumable (needs-user or failed).
    pub fn is_resumable(&self) -> bool {
        matches!(self, Self::NeedsUser | Self::Failed)
    }
}

impl fmt::Display for RunStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A resolved run with its ID, directory, and metadata.
#[derive(Debug, Clone)]
pub struct Run {
    pub id: String,
    pub dir: PathBuf,
}

impl Run {
    pub fn status(&self) -> Result<RunStatus> {
        let path = self.dir.join("status");
        match fs::read_to_string(&path) {
            Ok(s) => Ok(RunStatus::parse(&s)),
            Err(_) => Ok(RunStatus::Unknown("-".into())),
        }
    }

    pub fn set_status(&self, status: &RunStatus) -> Result<()> {
        fs::write(self.dir.join("status"), status.as_str())
            .context("Failed to write run status")
    }

    pub fn runtime(&self) -> String {
        fs::read_to_string(self.dir.join("runtime"))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "-".into())
    }

    pub fn brief_summary(&self) -> String {
        let path = self.dir.join("brief.md");
        match fs::read_to_string(&path) {
            Ok(content) => {
                content
                    .lines()
                    .filter(|l| !l.starts_with('#') && !l.is_empty())
                    .next()
                    .map(|l| {
                        if l.len() > 50 {
                            format!("{}...", &l[..47])
                        } else {
                            l.to_string()
                        }
                    })
                    .unwrap_or_else(|| "(brief exists)".into())
            }
            Err(_) => "-".into(),
        }
    }

    pub fn mode(&self) -> String {
        fs::read_to_string(self.dir.join("mode"))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "build".into())
    }

    pub fn reviewer_filter(&self) -> String {
        fs::read_to_string(self.dir.join("reviewers"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    pub fn scope(&self) -> Option<String> {
        fs::read_to_string(self.dir.join("scope"))
            .ok()
            .map(|s| s.trim().to_string())
    }

    pub fn has_handoff(&self) -> bool {
        self.dir.join("handoff.md").exists()
    }

    /// Count the number of session directories under this run.
    pub fn session_count(&self) -> usize {
        let sessions_dir = self.dir.join("sessions");
        if !sessions_dir.is_dir() {
            return 0;
        }
        fs::read_dir(&sessions_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().is_dir())
                    .count()
            })
            .unwrap_or(0)
    }

    /// Check whether all review artifacts have a passing verdict.
    pub fn reviews_passed(&self) -> Option<bool> {
        let reviews_dir = self.dir.join("reviews");
        if !reviews_dir.is_dir() {
            return None;
        }
        let mut found_any = false;
        let entries = fs::read_dir(&reviews_dir).ok()?;
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with("review-") && name_str.ends_with(".md") {
                found_any = true;
                let content = fs::read_to_string(entry.path()).unwrap_or_default();
                for line in content.lines() {
                    let lower = line.to_lowercase();
                    if lower.starts_with("verdict:") {
                        let value = lower
                            .strip_prefix("verdict:")
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        if value != "pass" {
                            return Some(false);
                        }
                    }
                }
            }
        }
        if found_any { Some(true) } else { None }
    }

    /// Extract what the agent needs from the handoff file.
    ///
    /// Looks for "Open questions" section first, then falls back to the
    /// first non-heading, non-empty line.
    pub fn handoff_need(&self) -> Option<String> {
        let content = fs::read_to_string(self.dir.join("handoff.md")).ok()?;
        // Look for "Open questions" section
        let mut in_section = false;
        for line in content.lines() {
            if line.starts_with('#') && line.to_lowercase().contains("open question") {
                in_section = true;
                continue;
            }
            if in_section {
                if line.starts_with('#') {
                    break;
                }
                let trimmed = line.trim().trim_start_matches("- ");
                if !trimmed.is_empty() {
                    let s = if trimmed.len() > 80 {
                        format!("{}...", &trimmed[..77])
                    } else {
                        trimmed.to_string()
                    };
                    return Some(s);
                }
            }
        }
        None
    }

    /// Build the notification body for a status change.
    pub fn notification_body(&self) -> String {
        let brief = self.brief_summary();
        let status = self
            .status()
            .map(|s| s.to_string())
            .unwrap_or_else(|_| "unknown".into());

        let mut body = format!("{}: {}", self.id, status);
        if brief != "-" {
            body.push_str(&format!("\n{brief}"));
        }

        match status.as_str() {
            "complete" => {
                let sessions = self.session_count();
                if sessions > 0 {
                    body.push_str(&format!("\n{sessions} sessions"));
                }
                match self.reviews_passed() {
                    Some(true) => body.push_str(", reviews passed"),
                    Some(false) => body.push_str(", reviews failed"),
                    None => {}
                }
            }
            "needs-user" => {
                if let Some(need) = self.handoff_need() {
                    body.push_str(&format!("\n{need}"));
                }
            }
            _ => {}
        }

        body
    }

    /// Derive the project root from the run directory.
    ///
    /// The run directory is at `<project>/.factory/runs/<id>`, so the
    /// project root is three levels up.
    pub fn project_root(&self) -> PathBuf {
        project_root_from_run_dir(&self.dir)
    }

    pub fn handle(&self) -> Option<String> {
        fs::read_to_string(self.dir.join("handle"))
            .ok()
            .map(|s| s.trim().to_string())
    }

    /// Get the run directory inside the worktree, if one exists.
    pub fn worktree_run_dir(&self) -> Option<PathBuf> {
        let wt_path = fs::read_to_string(self.dir.join("worktree"))
            .ok()
            .map(|s| s.trim().to_string())?;
        let wt_dir = PathBuf::from(&wt_path);
        let wt_run_dir = wt_dir.join(format!(".factory/runs/{}", self.id));
        if wt_run_dir.is_dir() {
            Some(wt_run_dir)
        } else {
            None
        }
    }
}

/// Derive the project root from a run directory path.
///
/// Run directories are at `<project>/.factory/runs/<id>` — three levels up.
pub fn project_root_from_run_dir(run_dir: &Path) -> PathBuf {
    run_dir
        .ancestors()
        .nth(3)
        .unwrap_or(Path::new("."))
        .to_path_buf()
}

/// Resolve a run ID from the given search root.
///
/// Priority chain:
/// 1. Explicit run_id (from --run-id flag)
/// 2. FACTORY_RUN_ID env var
/// 3. .factory/active-run file
/// 4. Scan .factory/runs/ for active runs
pub fn resolve_run(search_root: &Path, explicit_id: Option<&str>) -> Result<Run> {
    let runs_dir = search_root.join(".factory/runs");

    // 1. Explicit run ID
    if let Some(id) = explicit_id {
        let dir = runs_dir.join(id);
        if !dir.is_dir() {
            bail!("Run directory not found: {}", dir.display());
        }
        return Ok(Run {
            id: id.to_string(),
            dir,
        });
    }

    // 2. FACTORY_RUN_ID env var
    if let Ok(id) = std::env::var("FACTORY_RUN_ID") {
        if !id.is_empty() {
            let dir = runs_dir.join(&id);
            if !dir.is_dir() {
                bail!("Run directory not found: {}", dir.display());
            }
            return Ok(Run { id, dir });
        }
    }

    // 3. active-run pointer
    let active_run_path = search_root.join(".factory/active-run");
    if active_run_path.exists() {
        let id = fs::read_to_string(&active_run_path)
            .context("Failed to read active-run")?
            .trim()
            .to_string();
        let dir = runs_dir.join(&id);
        if dir.is_dir() {
            return Ok(Run { id, dir });
        }
        // Stale pointer — fall through to scan
    }

    // 4. Scan for active run
    if runs_dir.is_dir() {
        if let Some(run) = scan_active_run(&runs_dir)? {
            return Ok(run);
        }
    }

    bail!("No active run found. Create a brief and plan first.")
}

/// Resolve a run that is resumable (needs-user or failed).
pub fn resolve_resumable_run(search_root: &Path, explicit_id: Option<&str>) -> Result<Run> {
    let runs_dir = search_root.join(".factory/runs");

    if let Some(id) = explicit_id {
        let dir = runs_dir.join(id);
        if !dir.is_dir() {
            bail!("Run directory not found: {}", dir.display());
        }
        return Ok(Run {
            id: id.to_string(),
            dir,
        });
    }

    if runs_dir.is_dir() {
        for entry in fs::read_dir(&runs_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let status_path = path.join("status");
            if status_path.exists() {
                let s = fs::read_to_string(&status_path).unwrap_or_default();
                let status = RunStatus::parse(&s);
                if status.is_resumable() {
                    let id = entry.file_name().to_string_lossy().to_string();
                    return Ok(Run { id, dir: path });
                }
            }
        }
    }

    bail!("No run found needing resume.")
}

/// Scan .factory/runs/ for an active run (planned or executing).
fn scan_active_run(runs_dir: &Path) -> Result<Option<Run>> {
    let mut found: Option<Run> = None;

    for entry in fs::read_dir(runs_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let status_path = path.join("status");
        if status_path.exists() {
            let s = fs::read_to_string(&status_path).unwrap_or_default();
            let status = RunStatus::parse(&s);
            if status.is_active() {
                let id = entry.file_name().to_string_lossy().to_string();
                found = Some(Run { id, dir: path });
            }
        }
    }

    Ok(found)
}

/// List all runs in a search root.
pub fn list_runs(search_root: &Path) -> Result<Vec<Run>> {
    let runs_dir = search_root.join(".factory/runs");
    let mut runs = Vec::new();

    if !runs_dir.is_dir() {
        return Ok(runs);
    }

    for entry in fs::read_dir(&runs_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().to_string();
        runs.push(Run { id, dir: path });
    }

    runs.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(runs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    fn setup_test_project() -> TempDir {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".factory/runs")).unwrap();
        tmp
    }

    fn create_run(root: &Path, id: &str, status: &str) {
        let run_dir = root.join(format!(".factory/runs/{id}"));
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("status"), status).unwrap();
        fs::write(run_dir.join("brief.md"), format!("Brief for {id}")).unwrap();
    }

    #[test]
    fn test_run_status_parse() {
        assert_eq!(RunStatus::parse("planned"), RunStatus::Planned);
        assert_eq!(RunStatus::parse("executing"), RunStatus::Executing);
        assert_eq!(RunStatus::parse("complete"), RunStatus::Complete);
        assert_eq!(RunStatus::parse("needs-user"), RunStatus::NeedsUser);
        assert_eq!(RunStatus::parse("failed"), RunStatus::Failed);
        assert_eq!(RunStatus::parse("rate-limited"), RunStatus::RateLimited);
        assert_eq!(
            RunStatus::parse("briefed"),
            RunStatus::Briefed
        );
        assert_eq!(
            RunStatus::parse("behaviors-defined"),
            RunStatus::BehaviorsDefined
        );
        assert_eq!(
            RunStatus::parse("approach-designed"),
            RunStatus::ApproachDesigned
        );
    }

    #[test]
    fn test_status_is_active() {
        assert!(RunStatus::Planned.is_active());
        assert!(RunStatus::Executing.is_active());
        assert!(!RunStatus::Complete.is_active());
        assert!(!RunStatus::NeedsUser.is_active());
        assert!(!RunStatus::Failed.is_active());
    }

    #[test]
    fn test_status_is_resumable() {
        assert!(RunStatus::NeedsUser.is_resumable());
        assert!(RunStatus::Failed.is_resumable());
        assert!(!RunStatus::Planned.is_resumable());
        assert!(!RunStatus::Executing.is_resumable());
        assert!(!RunStatus::Complete.is_resumable());
    }

    #[test]
    fn test_resolve_explicit_id() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "test-run", "planned");

        let run = resolve_run(tmp.path(), Some("test-run")).unwrap();
        assert_eq!(run.id, "test-run");
    }

    #[test]
    fn test_resolve_explicit_id_missing() {
        let tmp = setup_test_project();
        let result = resolve_run(tmp.path(), Some("nonexistent"));
        assert!(result.is_err());
    }

    #[test]
    #[serial]
    fn test_resolve_active_run_pointer() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "run-from-pointer", "planned");
        fs::write(
            tmp.path().join(".factory/active-run"),
            "run-from-pointer",
        )
        .unwrap();

        let run = resolve_run(tmp.path(), None).unwrap();
        assert_eq!(run.id, "run-from-pointer");
    }

    #[test]
    #[serial]
    fn test_resolve_scan_ignores_complete() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "run-done", "complete");
        create_run(tmp.path(), "run-active", "planned");

        let run = resolve_run(tmp.path(), None).unwrap();
        assert_eq!(run.id, "run-active");
    }

    #[test]
    #[serial]
    fn test_resolve_scan_finds_executing() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "run-exec", "executing");

        let run = resolve_run(tmp.path(), None).unwrap();
        assert_eq!(run.id, "run-exec");
    }

    #[test]
    #[serial]
    fn test_resolve_scan_skips_needs_user() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "run-nu", "needs-user");
        create_run(tmp.path(), "run-plan", "planned");

        let run = resolve_run(tmp.path(), None).unwrap();
        assert_eq!(run.id, "run-plan");
    }

    #[test]
    #[serial]
    fn test_resolve_scan_skips_failed() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "run-fail", "failed");
        create_run(tmp.path(), "run-plan", "planned");

        let run = resolve_run(tmp.path(), None).unwrap();
        assert_eq!(run.id, "run-plan");
    }

    #[test]
    #[serial]
    fn test_resolve_scan_mixed_statuses() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "run-complete", "complete");
        create_run(tmp.path(), "run-failed", "failed");
        create_run(tmp.path(), "run-needs-user", "needs-user");
        create_run(tmp.path(), "run-active", "executing");

        let run = resolve_run(tmp.path(), None).unwrap();
        assert_eq!(run.id, "run-active");
    }

    #[test]
    #[serial]
    fn test_resolve_no_active_run() {
        let tmp = setup_test_project();
        let result = resolve_run(tmp.path(), None);
        assert!(result.is_err());
    }

    #[test]
    #[serial]
    fn test_resolve_stale_active_run_pointer() {
        let tmp = setup_test_project();
        // Pointer points to non-existent run, but there's an active one
        fs::write(tmp.path().join(".factory/active-run"), "nonexistent").unwrap();
        create_run(tmp.path(), "run-active", "planned");

        let run = resolve_run(tmp.path(), None).unwrap();
        assert_eq!(run.id, "run-active");
    }

    #[test]
    #[serial]
    fn test_env_overrides_active_run() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "run-file", "planned");
        create_run(tmp.path(), "run-env", "planned");
        fs::write(tmp.path().join(".factory/active-run"), "run-file").unwrap();

        // SAFETY: Guarded by #[serial] — no other test runs concurrently.
        unsafe { std::env::set_var("FACTORY_RUN_ID", "run-env") };
        let run = resolve_run(tmp.path(), None).unwrap();
        assert_eq!(run.id, "run-env");
        unsafe { std::env::remove_var("FACTORY_RUN_ID") };
    }

    #[test]
    #[serial]
    fn test_resolve_resumable_finds_needs_user() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "run-paused", "needs-user");
        create_run(tmp.path(), "run-active", "executing");

        let run = resolve_resumable_run(tmp.path(), None).unwrap();
        assert_eq!(run.id, "run-paused");
    }

    #[test]
    #[serial]
    fn test_resolve_resumable_finds_failed() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "run-broken", "failed");

        let run = resolve_resumable_run(tmp.path(), None).unwrap();
        assert_eq!(run.id, "run-broken");
    }

    #[test]
    #[serial]
    fn test_resolve_resumable_skips_planned() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "run-planned", "planned");

        let result = resolve_resumable_run(tmp.path(), None);
        assert!(result.is_err());
    }

    #[test]
    fn test_list_runs() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "run-a", "planned");
        create_run(tmp.path(), "run-b", "complete");

        let runs = list_runs(tmp.path()).unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].id, "run-a");
        assert_eq!(runs[1].id, "run-b");
    }

    #[test]
    fn test_list_runs_empty() {
        let tmp = setup_test_project();
        let runs = list_runs(tmp.path()).unwrap();
        assert!(runs.is_empty());
    }

    #[test]
    fn test_run_brief_summary() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "test-run", "planned");
        let run = Run {
            id: "test-run".into(),
            dir: tmp.path().join(".factory/runs/test-run"),
        };
        assert!(run.brief_summary().contains("Brief for test-run"));
    }

    #[test]
    fn test_run_brief_summary_missing() {
        let tmp = setup_test_project();
        let run_dir = tmp.path().join(".factory/runs/no-brief");
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("status"), "planned").unwrap();
        let run = Run {
            id: "no-brief".into(),
            dir: run_dir,
        };
        assert_eq!(run.brief_summary(), "-");
    }

    #[test]
    fn test_status_display_includes_runtime() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "run-runtime-test", "executing");
        let run_dir = tmp.path().join(".factory/runs/run-runtime-test");
        fs::write(run_dir.join("runtime"), "local").unwrap();

        let run = Run {
            id: "run-runtime-test".into(),
            dir: run_dir,
        };
        assert_eq!(run.runtime(), "local");
    }

    #[test]
    fn test_status_display_missing_runtime() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "run-no-runtime", "planned");

        let run = Run {
            id: "run-no-runtime".into(),
            dir: tmp.path().join(".factory/runs/run-no-runtime"),
        };
        assert_eq!(run.runtime(), "-");
    }

    #[test]
    fn test_accessors_trim_trailing_newlines() {
        let tmp = setup_test_project();
        let run_dir = tmp.path().join(".factory/runs/trim-test");
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("status"), "planned").unwrap();
        fs::write(run_dir.join("runtime"), "fargate\n").unwrap();
        fs::write(run_dir.join("mode"), "review\n").unwrap();
        fs::write(run_dir.join("reviewers"), "review-tests\n").unwrap();
        fs::write(run_dir.join("scope"), "src/\n").unwrap();
        fs::write(run_dir.join("handle"), "abc123\n").unwrap();

        let run = Run {
            id: "trim-test".into(),
            dir: run_dir,
        };
        assert_eq!(run.runtime(), "fargate");
        assert_eq!(run.mode(), "review");
        assert_eq!(run.reviewer_filter(), "review-tests");
        assert_eq!(run.scope(), Some("src/".into()));
        assert_eq!(run.handle(), Some("abc123".into()));
    }

    #[test]
    fn test_status_unknown_parse() {
        let status = RunStatus::parse("something-new");
        assert_eq!(status, RunStatus::Unknown("something-new".to_string()));
        assert_eq!(status.as_str(), "something-new");
        assert!(!status.is_active());
        assert!(!status.is_terminal());
        assert!(!status.is_resumable());
    }

    #[test]
    fn test_status_is_terminal() {
        assert!(RunStatus::Complete.is_terminal());
        assert!(RunStatus::Failed.is_terminal());
        assert!(RunStatus::NeedsUser.is_terminal());
        assert!(!RunStatus::Planned.is_terminal());
        assert!(!RunStatus::Executing.is_terminal());
        assert!(!RunStatus::Briefed.is_terminal());
        assert!(!RunStatus::Unknown("x".into()).is_terminal());
    }

    #[test]
    fn test_all_known_statuses_roundtrip() {
        for s in &[
            "briefed",
            "behaviors-defined",
            "approach-designed",
            "planned",
            "executing",
            "rate-limited",
            "needs-user",
            "complete",
            "failed",
        ] {
            let status = RunStatus::parse(s);
            assert_eq!(status.as_str(), *s);
        }
    }

    #[test]
    fn test_session_count() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "run-s", "complete");
        let run_dir = tmp.path().join(".factory/runs/run-s");
        fs::create_dir_all(run_dir.join("sessions/session-1")).unwrap();
        fs::create_dir_all(run_dir.join("sessions/session-2")).unwrap();

        let run = Run {
            id: "run-s".into(),
            dir: run_dir,
        };
        assert_eq!(run.session_count(), 2);
    }

    #[test]
    fn test_session_count_no_sessions() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "run-ns", "complete");
        let run = Run {
            id: "run-ns".into(),
            dir: tmp.path().join(".factory/runs/run-ns"),
        };
        assert_eq!(run.session_count(), 0);
    }

    #[test]
    fn test_reviews_passed_all_pass() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "run-rp", "complete");
        let run_dir = tmp.path().join(".factory/runs/run-rp");
        fs::create_dir_all(run_dir.join("reviews")).unwrap();
        fs::write(run_dir.join("reviews/review-tests.md"), "Verdict: pass\n\nLooks good.").unwrap();
        fs::write(run_dir.join("reviews/review-style.md"), "Verdict: pass\n\nClean.").unwrap();

        let run = Run {
            id: "run-rp".into(),
            dir: run_dir,
        };
        assert_eq!(run.reviews_passed(), Some(true));
    }

    #[test]
    fn test_reviews_passed_one_fails() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "run-rf", "complete");
        let run_dir = tmp.path().join(".factory/runs/run-rf");
        fs::create_dir_all(run_dir.join("reviews")).unwrap();
        fs::write(run_dir.join("reviews/review-tests.md"), "Verdict: pass").unwrap();
        fs::write(run_dir.join("reviews/review-style.md"), "Verdict: fail").unwrap();

        let run = Run {
            id: "run-rf".into(),
            dir: run_dir,
        };
        assert_eq!(run.reviews_passed(), Some(false));
    }

    #[test]
    fn test_reviews_passed_no_reviews() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "run-nr", "complete");
        let run = Run {
            id: "run-nr".into(),
            dir: tmp.path().join(".factory/runs/run-nr"),
        };
        assert_eq!(run.reviews_passed(), None);
    }

    #[test]
    fn test_handoff_need() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "run-h", "needs-user");
        let run_dir = tmp.path().join(".factory/runs/run-h");
        fs::write(
            run_dir.join("handoff.md"),
            "## Run run-h\n\n### Open questions\n- Need API key for service X\n- Unclear scope\n",
        )
        .unwrap();

        let run = Run {
            id: "run-h".into(),
            dir: run_dir,
        };
        assert_eq!(run.handoff_need(), Some("Need API key for service X".into()));
    }

    #[test]
    fn test_handoff_need_missing() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "run-hm", "needs-user");
        let run = Run {
            id: "run-hm".into(),
            dir: tmp.path().join(".factory/runs/run-hm"),
        };
        assert_eq!(run.handoff_need(), None);
    }

    #[test]
    fn test_notification_body_complete() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "run-nc", "complete");
        let run_dir = tmp.path().join(".factory/runs/run-nc");
        fs::create_dir_all(run_dir.join("sessions/session-1")).unwrap();
        fs::create_dir_all(run_dir.join("sessions/session-2")).unwrap();
        fs::create_dir_all(run_dir.join("sessions/session-3")).unwrap();
        fs::create_dir_all(run_dir.join("reviews")).unwrap();
        fs::write(run_dir.join("reviews/review-tests.md"), "Verdict: pass").unwrap();

        let run = Run {
            id: "run-nc".into(),
            dir: run_dir,
        };
        let body = run.notification_body();
        assert!(body.contains("run-nc: complete"));
        assert!(body.contains("Brief for run-nc"));
        assert!(body.contains("3 sessions"));
        assert!(body.contains("reviews passed"));
    }

    #[test]
    fn test_notification_body_needs_user() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "run-nu", "needs-user");
        let run_dir = tmp.path().join(".factory/runs/run-nu");
        fs::write(
            run_dir.join("handoff.md"),
            "## Run\n### Open questions\n- Which database to use?\n",
        )
        .unwrap();

        let run = Run {
            id: "run-nu".into(),
            dir: run_dir,
        };
        let body = run.notification_body();
        assert!(body.contains("run-nu: needs-user"));
        assert!(body.contains("Which database to use?"));
    }

    #[test]
    fn test_notification_body_failed() {
        let tmp = setup_test_project();
        create_run(tmp.path(), "run-nf", "failed");

        let run = Run {
            id: "run-nf".into(),
            dir: tmp.path().join(".factory/runs/run-nf"),
        };
        let body = run.notification_body();
        assert!(body.contains("run-nf: failed"));
        assert!(body.contains("Brief for run-nf"));
    }
}
