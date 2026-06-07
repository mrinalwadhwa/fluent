use anyhow::Result;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::review::{self, ReviewStateRead, Verdict};
use crate::run::{self, RunStatus};

const RECENT_SESSION_LIMIT: usize = 5;

/// Build a deterministic operator summary for one run.
pub fn summarize_run(search_root: &Path, run_id: Option<&str>) -> Result<String> {
    let run = run::resolve_run(search_root, run_id)?;
    let live_dir = run.live_artifact_dir();
    let status = run.effective_status()?;

    let mut output = String::new();
    output.push_str("Run\n");
    output.push_str(&format!("  ID: {}\n", run.id));
    output.push_str(&format!("  Status: {status}\n"));
    output.push_str(&format!("  Phase: {}\n", phase_label(&status)));
    output.push_str(&format!("  Runtime: {}\n", run.runtime()));
    if live_dir != run.dir {
        output.push_str(&format!("  Artifacts: {}\n", live_dir.display()));
    }
    output.push('\n');

    output.push_str("Brief\n");
    for line in brief_lines(&run.dir) {
        output.push_str(&format!("  {line}\n"));
    }
    output.push('\n');

    output.push_str("Agents\n");
    for line in agent_lines(&run, &live_dir, &status) {
        output.push_str(&format!("  {line}\n"));
    }
    output.push('\n');

    output.push_str("Recent sessions\n");
    let session_lines = recent_session_lines(&live_dir).or_else(|| recent_session_lines(&run.dir));
    match session_lines {
        Some(lines) => {
            for line in lines {
                output.push_str(&format!("  {line}\n"));
            }
        }
        None => output.push_str("  (none)\n"),
    }
    output.push('\n');

    output.push_str("Reviewer verdicts\n");
    let review_state = review::effective_review_state(&live_dir, &run.dir);
    let verdicts = match review_state {
        ReviewStateRead::Present(state) => {
            output.push_str(&format!(
                "  State: {} ({})\n",
                state.state.as_str(),
                state.source.as_str()
            ));
            Some(state.verdicts)
        }
        ReviewStateRead::Invalid(error) => {
            output.push_str(&format!("  State: invalid review-state.json ({error})\n"));
            None
        }
        ReviewStateRead::Missing => {
            reviewer_verdicts(&live_dir).or_else(|| reviewer_verdicts(&run.dir))
        }
    };
    match verdicts {
        Some(verdicts) => {
            for (reviewer, verdict) in verdicts {
                output.push_str(&format!("  {reviewer}: {}\n", verdict_label(&verdict)));
            }
        }
        None => output.push_str("  (none)\n"),
    }
    output.push('\n');

    output.push_str("Handoff\n");
    match handoff_context(&live_dir).or_else(|| handoff_context(&run.dir)) {
        Some(context) => output.push_str(&format!("  {context}\n")),
        None => output.push_str("  (none)\n"),
    }
    output.push('\n');

    output.push_str("Report\n");
    if live_dir.join("report.md").exists() || run.dir.join("report.md").exists() {
        output.push_str("  Available: report.md\n");
    } else {
        output.push_str("  (none)\n");
    }
    output.push('\n');

    output.push_str("Next\n");
    output.push_str(&format!("  {}\n", next_action(&status)));

    Ok(output)
}

fn phase_label(status: &RunStatus) -> &'static str {
    match status {
        RunStatus::Briefed => "brief captured",
        RunStatus::BehaviorsDefined => "behaviors defined",
        RunStatus::ApproachDesigned => "approach designed",
        RunStatus::Planned => "ready to run",
        RunStatus::Executing => "authoring",
        RunStatus::Reviewing => "reviewing",
        RunStatus::RateLimited => "rate limited",
        RunStatus::NeedsUser => "needs user",
        RunStatus::Complete => "complete",
        RunStatus::Failed => "failed",
        RunStatus::Landed => "landed",
        RunStatus::Unknown(_) => "unknown",
    }
}

fn agent_lines(run: &run::Run, live_dir: &Path, status: &RunStatus) -> Vec<String> {
    let mut lines = Vec::new();
    let coder = read_trimmed(&live_dir.join("coder"))
        .or_else(|| read_trimmed(&run.dir.join("coder")))
        .unwrap_or_else(|| "unknown".to_string());
    lines.push(format!("Author: {coder} ({})", author_state(status)));

    match review::effective_review_state(live_dir, &run.dir) {
        ReviewStateRead::Present(state) => {
            lines.push(format!(
                "Reviewers: {} ({})",
                state.state.as_str(),
                state.source.as_str()
            ));
        }
        ReviewStateRead::Invalid(_) => {
            lines.push("Reviewers: invalid review-state.json".to_string());
        }
        ReviewStateRead::Missing => {
            if let Some(verdicts) =
                reviewer_verdicts(live_dir).or_else(|| reviewer_verdicts(&run.dir))
            {
                lines.push(format!("Reviewers: recent ({} verdicts)", verdicts.len()));
            } else if matches!(status, RunStatus::Reviewing) {
                lines.push("Reviewers: active".to_string());
            }
        }
    }

    for child in child_lines(run) {
        lines.push(child);
    }

    lines
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn author_state(status: &RunStatus) -> &'static str {
    match status {
        RunStatus::Briefed
        | RunStatus::BehaviorsDefined
        | RunStatus::ApproachDesigned
        | RunStatus::Planned => "pending",
        RunStatus::Executing => "active",
        RunStatus::Reviewing => "waiting for review",
        RunStatus::RateLimited => "rate limited",
        RunStatus::NeedsUser => "blocked",
        RunStatus::Complete | RunStatus::Landed => "recent",
        RunStatus::Failed => "stopped",
        RunStatus::Unknown(_) => "unknown",
    }
}

fn child_lines(run: &run::Run) -> Vec<String> {
    let Some(children) = read_trimmed(&run.dir.join("children")) else {
        return Vec::new();
    };
    let project_root = run.project_root();
    children
        .lines()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .take(5)
        .filter_map(|id| {
            let child_dir = project_root.join(format!(".factory/runs/{id}"));
            if !child_dir.is_dir() {
                return None;
            }
            let child = run::Run {
                id: id.to_string(),
                dir: child_dir,
            };
            let status = child
                .effective_status()
                .map(|status| status.to_string())
                .unwrap_or_else(|_| "unknown".to_string());
            Some(format!(
                "Child {id}: {status} - {}",
                truncate_line(&child.brief_summary())
            ))
        })
        .collect()
}

fn brief_lines(run_dir: &Path) -> Vec<String> {
    fs::read_to_string(run_dir.join("brief.md"))
        .ok()
        .map(|content| {
            content
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .take(3)
                .map(truncate_line)
                .collect::<Vec<_>>()
        })
        .filter(|lines| !lines.is_empty())
        .unwrap_or_else(|| vec!["(none)".to_string()])
}

fn recent_session_lines(run_dir: &Path) -> Option<Vec<String>> {
    let content = fs::read_to_string(run_dir.join("sessions.log")).ok()?;
    let lines = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(truncate_line)
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return None;
    }
    let start = lines.len().saturating_sub(RECENT_SESSION_LIMIT);
    Some(lines[start..].to_vec())
}

fn reviewer_verdicts(run_dir: &Path) -> Option<BTreeMap<String, Verdict>> {
    let reviews_dir = run_dir.join("reviews");
    if !reviews_dir.is_dir() {
        return None;
    }

    let mut verdicts = BTreeMap::new();
    for path in review_files(&reviews_dir) {
        let Some(name) = reviewer_name(&path) else {
            continue;
        };
        let content = fs::read_to_string(path).unwrap_or_default();
        verdicts.insert(name, review::extract_verdict(&content));
    }

    if verdicts.is_empty() {
        None
    } else {
        Some(verdicts)
    }
}

fn review_files(reviews_dir: &Path) -> Vec<PathBuf> {
    let mut files = fs::read_dir(reviews_dir)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(|entry| entry.ok()))
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with("review-") && name.ends_with(".md"))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn reviewer_name(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.strip_prefix("review-"))
        .map(str::to_string)
}

fn handoff_context(run_dir: &Path) -> Option<String> {
    let content = fs::read_to_string(run_dir.join("handoff.md")).ok()?;
    first_open_question(&content)
        .or_else(|| first_explicit_action_line(&content))
        .or_else(|| first_actionable_line(&content))
        .map(|line| truncate_line(&line))
}

fn first_open_question(content: &str) -> Option<String> {
    let mut in_section = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') && trimmed.to_lowercase().contains("open question") {
            in_section = true;
            continue;
        }
        if in_section && trimmed.starts_with('#') {
            return None;
        }
        if in_section {
            let item = trimmed.trim_start_matches("- ").trim();
            if !item.is_empty() {
                return Some(item.to_string());
            }
        }
    }
    None
}

fn first_explicit_action_line(content: &str) -> Option<String> {
    content
        .lines()
        .map(str::trim)
        .filter(|line| {
            line.starts_with("Question:")
                || line.starts_with("Next:")
                || line.starts_with("Next step:")
                || line.starts_with("Next steps:")
                || line.starts_with("Action:")
                || line.starts_with("Blocked:")
        })
        .map(str::to_string)
        .find(|line| !line.is_empty())
}

fn first_actionable_line(content: &str) -> Option<String> {
    content
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with('#')
                && !line.starts_with("Brief:")
                && !line.starts_with("Status:")
        })
        .map(|line| line.trim_start_matches("- ").trim().to_string())
        .find(|line| !line.is_empty())
}

fn verdict_label(verdict: &Verdict) -> &'static str {
    match verdict {
        Verdict::Pass => "pass",
        Verdict::Fail => "fail",
        Verdict::Uncertain => "uncertain",
    }
}

fn next_action(status: &RunStatus) -> &'static str {
    match status {
        RunStatus::Briefed => "define behaviors.",
        RunStatus::BehaviorsDefined => "design the approach.",
        RunStatus::ApproachDesigned => "write the execution plan.",
        RunStatus::Planned => "start or resume the run.",
        RunStatus::Executing => "author work is still in progress.",
        RunStatus::Reviewing => "wait for reviewers to finish.",
        RunStatus::RateLimited => "wait for the session loop to retry.",
        RunStatus::NeedsUser => "read handoff.md and answer the open question.",
        RunStatus::Complete => "ready to land if checks still pass.",
        RunStatus::Failed => "inspect handoff and failure evidence before resuming.",
        RunStatus::Landed => "no action needed.",
        RunStatus::Unknown(_) => "inspect run artifacts.",
    }
}

fn truncate_line(line: &str) -> String {
    let line = line.trim();
    const LIMIT: usize = 160;
    if line.chars().count() <= LIMIT {
        line.to_string()
    } else {
        format!("{}...", line.chars().take(LIMIT - 3).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn summarize_includes_artifacts() {
        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path().join(".factory/runs/test-run");
        fs::create_dir_all(run_dir.join("reviews")).unwrap();
        fs::write(tmp.path().join(".factory/active-run"), "test-run").unwrap();
        fs::write(run_dir.join("status"), "needs-user").unwrap();
        fs::write(run_dir.join("runtime"), "local").unwrap();
        fs::write(run_dir.join("brief.md"), "# Brief\n\nBuild a summary").unwrap();
        fs::write(
            run_dir.join("sessions.log"),
            "session=1 exit=0 duration=10s status=executing\nreview=1 duration=3s verdict=fail\n",
        )
        .unwrap();
        fs::write(run_dir.join("reviews/review-tests.md"), "Verdict: fail").unwrap();
        fs::write(
            run_dir.join("handoff.md"),
            "### Open questions\n- Should we land this?\n",
        )
        .unwrap();
        fs::write(run_dir.join("report.md"), "# Run Report").unwrap();

        let summary = summarize_run(tmp.path(), None).unwrap();

        assert!(summary.contains("ID: test-run"));
        assert!(summary.contains("Status: needs-user"));
        assert!(summary.contains("Build a summary"));
        assert!(summary.contains("review=1 duration=3s verdict=fail"));
        assert!(summary.contains("tests: fail"));
        assert!(summary.contains("Should we land this?"));
        assert!(summary.contains("Available: report.md"));
        assert!(summary.contains("read handoff.md"));
    }

    #[test]
    fn summarize_prefers_review_state() {
        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path().join(".factory/runs/test-run");
        fs::create_dir_all(run_dir.join("reviews")).unwrap();
        fs::write(tmp.path().join(".factory/active-run"), "test-run").unwrap();
        fs::write(run_dir.join("status"), "complete").unwrap();
        fs::write(run_dir.join("runtime"), "local").unwrap();
        fs::write(run_dir.join("brief.md"), "# Brief\n\nBuild a summary").unwrap();
        fs::write(run_dir.join("reviews/review-tests.md"), "Verdict: fail").unwrap();
        fs::write(
            run_dir.join("review-state.json"),
            r#"{
  "state": "accepted-review-limit",
  "round": 11,
  "source": "review-limit",
  "verdicts": {
    "tests": "fail"
  },
  "max_rounds": 10,
  "reason": "Review round limit reached with a clean worktree."
}
"#,
        )
        .unwrap();

        let summary = summarize_run(tmp.path(), None).unwrap();

        assert!(summary.contains("Reviewers: accepted-review-limit (review-limit)"));
        assert!(summary.contains("State: accepted-review-limit (review-limit)"));
        assert!(summary.contains("tests: fail"));
    }

    #[test]
    fn summarize_reports_invalid_review_state_without_artifact_fallback() {
        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path().join(".factory/runs/test-run");
        fs::create_dir_all(run_dir.join("reviews")).unwrap();
        fs::write(tmp.path().join(".factory/active-run"), "test-run").unwrap();
        fs::write(run_dir.join("status"), "complete").unwrap();
        fs::write(run_dir.join("runtime"), "local").unwrap();
        fs::write(run_dir.join("brief.md"), "# Brief\n\nBuild a summary").unwrap();
        fs::write(run_dir.join("reviews/review-tests.md"), "Verdict: pass").unwrap();
        fs::write(run_dir.join("review-state.json"), r#"{"state":"unknown"}"#).unwrap();

        let summary = summarize_run(tmp.path(), None).unwrap();

        assert!(summary.contains("Reviewers: invalid review-state.json"));
        assert!(summary.contains("State: invalid review-state.json"));
        assert!(!summary.contains("tests: pass"));
    }
}
