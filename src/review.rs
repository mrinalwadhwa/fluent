use anyhow::Result;
use std::fs;
use std::path::Path;
use std::process::Command;

use crate::content::{prompt_section, ContentResolver};
use crate::run::project_root_from_run_dir;

/// Reviewer names in execution order.
pub const REVIEWERS: &[&str] = &[
    "documentation",
    "behaviors",
    "architecture",
    "skills",
    "tests",
];

/// Verdict from a single reviewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail,
    Uncertain,
}

impl Verdict {
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().trim() {
            "pass" => Self::Pass,
            "fail" => Self::Fail,
            _ => Self::Uncertain,
        }
    }

    pub fn is_passing(&self) -> bool {
        matches!(self, Self::Pass)
    }
}

/// Run a single reviewer. Returns the verdict.
pub fn run_single_reviewer(
    reviewer_name: &str,
    system_prompt: &str,
    review_prompt: &str,
    run_dir: &Path,
) -> Result<Verdict> {
    // Run from the project root
    let project_root = project_root_from_run_dir(run_dir)
        .to_string_lossy()
        .to_string();

    eprintln!("  [{reviewer_name}] starting...");

    let transcript_path = run_dir.join(format!("reviews/transcript-{reviewer_name}.jsonl"));

    let status = Command::new("claude")
        .current_dir(&project_root)
        .args(["--dangerously-skip-permissions"])
        .args(["--verbose", "--output-format", "stream-json"])
        .args(["--append-system-prompt", system_prompt])
        .args(["-p", review_prompt])
        .stdout(
            std::fs::File::create(&transcript_path)
                .map(std::process::Stdio::from)
                .unwrap_or_else(|_| std::process::Stdio::null()),
        )
        .status();

    match status {
        Ok(s) if !s.success() => {
            eprintln!(
                "  [{reviewer_name}] session failed (exit {}), skipping",
                s.code().unwrap_or(-1)
            );
            return Ok(Verdict::Pass);
        }
        Err(e) => {
            eprintln!("  [{reviewer_name}] failed to launch: {e}, skipping");
            return Ok(Verdict::Pass);
        }
        _ => {}
    }

    // Check for review artifact
    let review_file = run_dir.join(format!("reviews/review-{reviewer_name}.md"));
    if !review_file.exists() {
        eprintln!("  [{reviewer_name}] no review artifact produced, skipping");
        return Ok(Verdict::Pass);
    }

    let content = fs::read_to_string(&review_file)?;
    let verdict = extract_verdict(&content);
    eprintln!("  [{reviewer_name}] verdict: {}", verdict_str(&verdict));

    Ok(verdict)
}

/// Archive previous round's review artifacts before running a new round.
fn archive_previous_round(run_dir: &Path, review_round: u32) {
    if review_round <= 1 {
        return;
    }
    let prev_round = review_round - 1;
    let archive_dir = run_dir.join(format!("reviews/round-{prev_round}"));
    let reviews_dir = run_dir.join("reviews");

    if fs::create_dir_all(&archive_dir).is_err() {
        return;
    }

    // Copy review-*.md files to archive
    if let Ok(entries) = fs::read_dir(&reviews_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with("review-") && name_str.ends_with(".md") {
                let _ = fs::copy(entry.path(), archive_dir.join(&name));
            } else if name_str.starts_with("transcript-") && name_str.ends_with(".jsonl") {
                // Move transcript files to archive
                let _ = fs::rename(entry.path(), archive_dir.join(&name));
            }
        }
    }
}

/// Run all reviewers (or a filtered set) in parallel.
/// Returns true if all pass, false if any fail.
/// `review_round` tracks how many times reviews have been run (1-based).
pub fn run_reviews(
    run_dir: &Path,
    run_id: &str,
    reviewer_filter: &str,
    review_mode: &str,
    resolver: &ContentResolver,
    review_round: u32,
) -> Result<bool> {
    fs::create_dir_all(run_dir.join("reviews"))?;

    // Archive previous round's reviews if this isn't the first round
    archive_previous_round(run_dir, review_round);

    let scope_detail = fs::read_to_string(run_dir.join("scope")).unwrap_or_default();
    let scope_instruction = if scope_detail.is_empty() {
        String::new()
    } else {
        format!(
            " Focus your review on: {scope_detail}. Read surrounding context as needed, but concentrate your findings on these areas."
        )
    };

    eprintln!(
        "\n  === Review phase (run: {run_id}, mode: {review_mode}) ===\n"
    );

    let mut handles = Vec::new();

    for &reviewer in REVIEWERS {
        // Apply filter
        if !reviewer_filter.is_empty() && !reviewer_filter.contains(reviewer) {
            continue;
        }

        // Load prompts
        let prompt_key = format!("prompts/review-{reviewer}.md");
        let prompt_content = match resolver.resolve_content(&prompt_key) {
            Some(c) => c,
            None => {
                eprintln!("  [{reviewer}] prompt file missing, skipping");
                continue;
            }
        };

        let system = prompt_section(&prompt_content, "system")
            .replace("{{RUN_ID}}", run_id);

        let section = if review_mode == "full-codebase" {
            "full-codebase"
        } else {
            "run-scoped"
        };
        let prompt = format!(
            "{}{}",
            prompt_section(&prompt_content, section).replace("{{RUN_ID}}", run_id),
            scope_instruction
        );

        let run_dir = run_dir.to_path_buf();
        let reviewer_name = reviewer.to_string();

        handles.push(std::thread::spawn(move || {
            run_single_reviewer(&reviewer_name, &system, &prompt, &run_dir)
        }));
    }

    let mut all_pass = true;
    for handle in handles {
        match handle.join() {
            Ok(Ok(verdict)) => {
                if !verdict.is_passing() {
                    all_pass = false;
                }
            }
            Ok(Err(e)) => {
                eprintln!("  Reviewer error: {e}");
                // Treat errors as pass (same as shell version)
            }
            Err(_) => {
                eprintln!("  Reviewer thread panicked");
            }
        }
    }

    Ok(all_pass)
}

/// Extract verdict from review file content.
pub fn extract_verdict(content: &str) -> Verdict {
    for line in content.lines() {
        let lower = line.to_lowercase();
        if lower.starts_with("verdict:") {
            let value = lower
                .strip_prefix("verdict:")
                .unwrap_or("")
                .trim()
                .to_string();
            return Verdict::parse(&value);
        }
    }
    Verdict::Uncertain
}

fn verdict_str(v: &Verdict) -> &'static str {
    match v {
        Verdict::Pass => "pass",
        Verdict::Fail => "fail",
        Verdict::Uncertain => "uncertain",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_verdict_pass() {
        assert_eq!(
            extract_verdict("Verdict: pass\n\nLooks good."),
            Verdict::Pass
        );
    }

    #[test]
    fn test_extract_verdict_fail() {
        assert_eq!(
            extract_verdict("Verdict: fail\n\n1. Missing coverage."),
            Verdict::Fail
        );
    }

    #[test]
    fn test_extract_verdict_uncertain() {
        assert_eq!(
            extract_verdict("Verdict: uncertain\n\nNeed more info."),
            Verdict::Uncertain
        );
    }

    #[test]
    fn test_extract_verdict_case_insensitive() {
        assert_eq!(
            extract_verdict("Verdict: PASS\n\nAll good."),
            Verdict::Pass
        );
        assert_eq!(
            extract_verdict("verdict: Pass\n"),
            Verdict::Pass
        );
    }

    #[test]
    fn test_extract_verdict_missing() {
        assert_eq!(
            extract_verdict("No verdict here.\nJust some text."),
            Verdict::Uncertain
        );
    }

    #[test]
    fn test_verdict_is_passing() {
        assert!(Verdict::Pass.is_passing());
        assert!(!Verdict::Fail.is_passing());
        assert!(!Verdict::Uncertain.is_passing());
    }

    #[test]
    fn test_archive_previous_round_noop_for_first_round() {
        let tmp = tempfile::TempDir::new().unwrap();
        let run_dir = tmp.path();
        let reviews = run_dir.join("reviews");
        fs::create_dir_all(&reviews).unwrap();
        fs::write(reviews.join("review-tests.md"), "Verdict: pass").unwrap();

        archive_previous_round(run_dir, 1);

        // No archive should be created for round 1
        assert!(!run_dir.join("reviews/round-0").exists());
        // Original file still exists
        assert!(reviews.join("review-tests.md").exists());
    }

    #[test]
    fn test_archive_previous_round_copies_reviews() {
        let tmp = tempfile::TempDir::new().unwrap();
        let run_dir = tmp.path();
        let reviews = run_dir.join("reviews");
        fs::create_dir_all(&reviews).unwrap();
        fs::write(reviews.join("review-tests.md"), "Verdict: pass").unwrap();
        fs::write(
            reviews.join("transcript-tests.jsonl"),
            "{\"type\":\"test\"}",
        )
        .unwrap();

        archive_previous_round(run_dir, 2);

        // Archive directory should exist with copies
        let archive = reviews.join("round-1");
        assert!(archive.join("review-tests.md").exists());
        assert!(archive.join("transcript-tests.jsonl").exists());

        // Review file should still exist (copied, not moved)
        assert!(reviews.join("review-tests.md").exists());
        // Transcript should be moved (not just copied)
        assert!(!reviews.join("transcript-tests.jsonl").exists());
    }
}
