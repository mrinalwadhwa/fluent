use anyhow::Result;
use std::fs;
use std::path::Path;
use std::process::Command;

use crate::review;
use crate::run::project_root_from_run_dir;

/// Generate a run report at `<run_dir>/report.md`.
pub fn generate_report(run_dir: &Path, run_id: &str, session_count: u32) -> Result<()> {
    let report_path = run_dir.join("report.md");

    let status = fs::read_to_string(run_dir.join("status")).unwrap_or_else(|_| "unknown".into());
    let mode = fs::read_to_string(run_dir.join("mode")).unwrap_or_else(|_| "build".into());

    let brief_summary = fs::read_to_string(run_dir.join("brief.md"))
        .ok()
        .map(|content| {
            content
                .lines()
                .filter(|l| !l.starts_with('#') && !l.is_empty())
                .take(3)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();

    let mut report = String::new();
    report.push_str("# Run Report\n\n");
    report.push_str(&format!("Run: {run_id}\n"));
    report.push_str(&format!("Status: {status}\n"));
    report.push_str(&format!("Mode: {mode}\n"));
    report.push_str(&format!("Sessions: {session_count}\n\n"));

    report.push_str("## Brief\n\n");
    if brief_summary.is_empty() {
        report.push_str("(no brief)\n\n");
    } else {
        report.push_str(&format!("{brief_summary}\n\n"));
    }

    report.push_str("## Reviewer verdicts\n\n");
    let reviews_dir = run_dir.join("reviews");
    if reviews_dir.is_dir() {
        let mut has_reviews = false;
        if let Ok(entries) = fs::read_dir(&reviews_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .strip_prefix("review-")
                    .unwrap_or("");
                if name.is_empty() {
                    continue;
                }
                has_reviews = true;
                let verdict_str = fs::read_to_string(&path)
                    .ok()
                    .map(|c| {
                        let v = review::extract_verdict(&c);
                        match v {
                            review::Verdict::Pass => "pass".to_string(),
                            review::Verdict::Fail => "fail".to_string(),
                            review::Verdict::Uncertain => "uncertain".to_string(),
                        }
                    })
                    .unwrap_or_else(|| "no verdict".into());
                report.push_str(&format!("- **{name}**: {verdict_str}\n"));
            }
        }
        if !has_reviews {
            report.push_str("(no reviews)\n");
        }
        report.push('\n');
    } else {
        report.push_str("(no reviews)\n\n");
    }

    report.push_str("## Key findings\n\n");
    if reviews_dir.is_dir() {
        let mut finding_count = 0;
        if let Ok(entries) = fs::read_dir(&reviews_dir) {
            for entry in entries.flatten() {
                if let Ok(content) = fs::read_to_string(entry.path()) {
                    finding_count += content
                        .lines()
                        .filter(|l| {
                            l.starts_with(|c: char| c.is_ascii_digit())
                        })
                        .count();
                }
            }
        }
        if finding_count > 0 {
            report.push_str("Reviewers produced findings. See reviews/ for details.\n\n");
        } else {
            report.push_str("No findings.\n\n");
        }
    } else {
        report.push_str("(no reviews)\n\n");
    }

    report.push_str("## Artifacts\n\n");
    for artifact in &[
        "brief.md",
        "behaviors.diff.md",
        "approach.md",
        "plan.md",
    ] {
        if run_dir.join(artifact).exists() {
            report.push_str(&format!("- {artifact}\n"));
        }
    }
    report.push('\n');

    // Commit log — git log from source branch to HEAD
    report.push_str("## Commits\n\n");
    let worktree_root = project_root_from_run_dir(run_dir)
        .to_string_lossy()
        .to_string();
    if Path::new(&worktree_root).join(".git").exists()
        || Command::new("git")
            .args(["-C", &worktree_root, "rev-parse", "--git-dir"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    {
        let source_branch = fs::read_to_string(run_dir.join("source-branch"))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "main".into());
        let range = format!("{source_branch}..HEAD");
        if let Ok(output) = Command::new("git")
            .args(["-C", &worktree_root, "log", "--oneline", &range])
            .output()
        {
            let log = String::from_utf8_lossy(&output.stdout);
            if log.trim().is_empty() {
                report.push_str("(no commits)\n");
            } else {
                for line in log.lines() {
                    report.push_str(&format!("- {line}\n"));
                }
            }
        } else {
            report.push_str("(no git repo)\n");
        }
    } else {
        report.push_str("(no git repo)\n");
    }
    report.push('\n');

    // Session summary from sessions.log
    report.push_str("## Sessions\n\n");
    match fs::read_to_string(run_dir.join("sessions.log")) {
        Ok(log) if !log.trim().is_empty() => {
            report.push_str(&log);
            if !log.ends_with('\n') {
                report.push('\n');
            }
        }
        _ => {
            report.push_str("(no session log)\n");
        }
    }
    report.push('\n');

    fs::write(&report_path, &report)?;
    eprintln!("  Report written to {}", report_path.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_generate_report() {
        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path();
        fs::write(run_dir.join("status"), "complete").unwrap();
        fs::write(run_dir.join("brief.md"), "# Brief\n\nDo the thing").unwrap();
        fs::create_dir(run_dir.join("reviews")).unwrap();
        fs::write(
            run_dir.join("reviews/review-tests.md"),
            "Verdict: pass\n\nAll good.",
        )
        .unwrap();

        generate_report(run_dir, "test-run", 3).unwrap();

        let report = fs::read_to_string(run_dir.join("report.md")).unwrap();
        assert!(report.contains("Run: test-run"));
        assert!(report.contains("Status: complete"));
        assert!(report.contains("Sessions: 3"));
        assert!(report.contains("Do the thing"));
    }

    #[test]
    fn test_generate_report_no_brief() {
        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path();
        fs::write(run_dir.join("status"), "failed").unwrap();

        generate_report(run_dir, "test-run", 1).unwrap();

        let report = fs::read_to_string(run_dir.join("report.md")).unwrap();
        assert!(report.contains("(no brief)"));
    }

    #[test]
    fn test_generate_report_includes_sessions_log() {
        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path();
        fs::write(run_dir.join("status"), "complete").unwrap();
        fs::write(
            run_dir.join("sessions.log"),
            "session=1 exit=0 duration=42s status=complete\n",
        )
        .unwrap();

        generate_report(run_dir, "test-run", 1).unwrap();

        let report = fs::read_to_string(run_dir.join("report.md")).unwrap();
        assert!(report.contains("## Sessions"));
        assert!(report.contains("session=1 exit=0 duration=42s status=complete"));
    }

    #[test]
    fn test_generate_report_no_sessions_log() {
        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path();
        fs::write(run_dir.join("status"), "complete").unwrap();

        generate_report(run_dir, "test-run", 1).unwrap();

        let report = fs::read_to_string(run_dir.join("report.md")).unwrap();
        assert!(report.contains("## Sessions"));
        assert!(report.contains("(no session log)"));
    }

    #[test]
    fn test_generate_report_includes_commits_section() {
        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path();
        fs::write(run_dir.join("status"), "complete").unwrap();

        generate_report(run_dir, "test-run", 1).unwrap();

        let report = fs::read_to_string(run_dir.join("report.md")).unwrap();
        assert!(report.contains("## Commits"));
        // No .git dir in tmpdir — should show fallback
        assert!(report.contains("(no git repo)"));
    }

    #[test]
    fn test_generate_report_empty_sessions_log() {
        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path();
        fs::write(run_dir.join("status"), "complete").unwrap();
        fs::write(run_dir.join("sessions.log"), "").unwrap();

        generate_report(run_dir, "test-run", 1).unwrap();

        let report = fs::read_to_string(run_dir.join("report.md")).unwrap();
        assert!(report.contains("(no session log)"));
    }
}
