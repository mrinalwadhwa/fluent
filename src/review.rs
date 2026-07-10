use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Reviewer names in execution order.
pub const REVIEWERS: &[&str] = &[
    "documentation",
    "behaviors",
    "architecture",
    "skills",
    "tests",
];

/// Verdict from a single reviewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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

/// Effective review outcome for a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewState {
    pub state: ReviewStateKind,
    pub round: u32,
    pub source: ReviewStateSource,
    pub verdicts: BTreeMap<String, Verdict>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl ReviewState {
    pub fn from_verdicts(round: u32, verdicts: BTreeMap<String, Verdict>) -> Self {
        let state = if verdicts.values().all(Verdict::is_passing) {
            ReviewStateKind::Passed
        } else if verdicts
            .values()
            .any(|verdict| matches!(verdict, Verdict::Fail))
        {
            ReviewStateKind::Failed
        } else {
            ReviewStateKind::Uncertain
        };
        Self {
            state,
            round,
            source: ReviewStateSource::Reviewers,
            verdicts,
            max_rounds: None,
            reason: None,
        }
    }

    pub fn accepted_review_limit(
        round: u32,
        max_rounds: u32,
        verdicts: BTreeMap<String, Verdict>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            state: ReviewStateKind::AcceptedReviewLimit,
            round,
            source: ReviewStateSource::ReviewLimit,
            verdicts,
            max_rounds: Some(max_rounds),
            reason: Some(reason.into()),
        }
    }

    pub fn is_accepted(&self) -> bool {
        matches!(
            self.state,
            ReviewStateKind::Passed | ReviewStateKind::AcceptedReviewLimit
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewStateKind {
    Passed,
    Failed,
    Uncertain,
    AcceptedReviewLimit,
}

impl ReviewStateKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Uncertain => "uncertain",
            Self::AcceptedReviewLimit => "accepted-review-limit",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewStateSource {
    Reviewers,
    ReviewLimit,
}

impl ReviewStateSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Reviewers => "reviewers",
            Self::ReviewLimit => "review-limit",
        }
    }
}

pub fn write_review_state(run_dir: &Path, state: &ReviewState) -> Result<()> {
    let content = serde_json::to_string_pretty(state)?;
    crate::atomic_write::atomic_write(
        &run_dir.join("review-state.json"),
        format!("{content}\n").as_bytes(),
    )?;
    Ok(())
}

pub fn read_review_state(run_dir: &Path) -> Option<Result<ReviewState>> {
    let path = run_dir.join("review-state.json");
    if !path.exists() {
        return None;
    }
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) => return Some(Err(error.into())),
    };
    Some(serde_json::from_str(&content).map_err(Into::into))
}

pub enum ReviewStateRead {
    Missing,
    Present(ReviewState),
    Invalid(String),
}

pub fn review_state_at(run_dir: &Path) -> ReviewStateRead {
    match read_review_state(run_dir) {
        Some(Ok(state)) => ReviewStateRead::Present(state),
        Some(Err(error)) => ReviewStateRead::Invalid(error.to_string()),
        None => ReviewStateRead::Missing,
    }
}

pub fn effective_review_state(primary_dir: &Path, fallback_dir: &Path) -> ReviewStateRead {
    match review_state_at(primary_dir) {
        ReviewStateRead::Missing if primary_dir != fallback_dir => review_state_at(fallback_dir),
        state => state,
    }
}

/// Determine whether reviews at a run artifact directory are accepted.
///
/// `review-state.json` is the durable review subsystem source of truth
/// when present. Older runs without review state fall back to top-level
/// `reviews/review-*.md` artifacts.
pub fn reviews_accepted_at(run_dir: &Path) -> Option<bool> {
    if let Some(state) = read_review_state(run_dir) {
        return Some(state.map(|state| state.is_accepted()).unwrap_or(false));
    }

    let verdicts = current_review_verdicts(run_dir);
    if verdicts.is_empty() {
        return None;
    }

    Some(verdicts.values().all(Verdict::is_passing))
}

/// Determine effective review acceptance using a live directory first.
pub fn effective_reviews_accepted(primary_dir: &Path, fallback_dir: &Path) -> Option<bool> {
    reviews_accepted_at(primary_dir).or_else(|| {
        if primary_dir == fallback_dir {
            None
        } else {
            reviews_accepted_at(fallback_dir)
        }
    })
}

pub fn current_review_verdicts(run_dir: &Path) -> BTreeMap<String, Verdict> {
    let reviews_dir = run_dir.join("reviews");
    let mut verdicts = BTreeMap::new();
    let Ok(entries) = fs::read_dir(&reviews_dir) else {
        return verdicts;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.strip_prefix("review-"))
        else {
            continue;
        };
        if !path
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .map(|file_name| file_name.ends_with(".md"))
            .unwrap_or(false)
        {
            continue;
        }
        let content = fs::read_to_string(&path).unwrap_or_default();
        verdicts.insert(name.to_string(), extract_verdict(&content));
    }
    verdicts
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

/// Reviewer's self-reported judgment about whether the writer made
/// progress on prior-round concerns. Emitted as a `Progress:` line in
/// review.md. Round 1 has no prior review to compare against, so
/// reviewers report `Progress: first-pass`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Progress {
    /// Writer addressed at least one prior-round concern from this
    /// reviewer (regardless of whether new concerns surfaced).
    Yes,
    /// Writer did not address this reviewer's prior-round concerns.
    No,
    /// Writer partially addressed prior-round concerns.
    Partial,
    /// First review round for this reviewer; no prior review exists.
    FirstPass,
    /// Reviewer did not emit a `Progress:` line.
    Missing,
}

impl Progress {
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().trim() {
            "yes" => Self::Yes,
            "no" => Self::No,
            "partial" => Self::Partial,
            "first-pass" | "first pass" | "firstpass" => Self::FirstPass,
            _ => Self::Missing,
        }
    }
}

/// Extract the `Progress:` line from review file content. Returns
/// `Progress::Missing` if no such line exists.
pub fn extract_progress(content: &str) -> Progress {
    for line in content.lines() {
        let lower = line.to_lowercase();
        if lower.starts_with("progress:") {
            let value = lower.strip_prefix("progress:").unwrap_or("").trim();
            return Progress::parse(value);
        }
    }
    Progress::Missing
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
        assert_eq!(extract_verdict("Verdict: PASS\n\nAll good."), Verdict::Pass);
        assert_eq!(extract_verdict("verdict: Pass\n"), Verdict::Pass);
    }

    #[test]
    fn test_extract_verdict_missing() {
        assert_eq!(
            extract_verdict("No verdict here.\nJust some text."),
            Verdict::Uncertain
        );
    }

    #[test]
    fn test_extract_progress_yes() {
        assert_eq!(
            extract_progress("Verdict: fail\nProgress: yes\n\nNew concerns surfaced."),
            Progress::Yes
        );
    }

    #[test]
    fn test_extract_progress_no() {
        assert_eq!(
            extract_progress("Verdict: fail\nProgress: no\n\nSame concerns persist."),
            Progress::No
        );
    }

    #[test]
    fn test_extract_progress_partial() {
        assert_eq!(
            extract_progress("Verdict: fail\nProgress: partial\n"),
            Progress::Partial
        );
    }

    #[test]
    fn test_extract_progress_first_pass() {
        assert_eq!(
            extract_progress("Verdict: pass\nProgress: first-pass\n"),
            Progress::FirstPass
        );
        assert_eq!(
            extract_progress("Verdict: pass\nProgress: First Pass\n"),
            Progress::FirstPass
        );
    }

    #[test]
    fn test_extract_progress_case_insensitive() {
        assert_eq!(extract_progress("Progress: YES\n"), Progress::Yes);
        assert_eq!(extract_progress("progress: No\n"), Progress::No);
    }

    #[test]
    fn test_extract_progress_missing() {
        assert_eq!(
            extract_progress("Verdict: pass\n\nNo progress line."),
            Progress::Missing
        );
    }

    #[test]
    fn test_verdict_is_passing() {
        assert!(Verdict::Pass.is_passing());
        assert!(!Verdict::Fail.is_passing());
        assert!(!Verdict::Uncertain.is_passing());
    }

    #[test]
    fn test_review_state_round_trip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut verdicts = BTreeMap::new();
        verdicts.insert("architecture".to_string(), Verdict::Pass);
        verdicts.insert("tests".to_string(), Verdict::Pass);
        let state = ReviewState::from_verdicts(2, verdicts);

        write_review_state(tmp.path(), &state).unwrap();

        let parsed = read_review_state(tmp.path()).unwrap().unwrap();
        assert_eq!(parsed.state, ReviewStateKind::Passed);
        assert_eq!(parsed.round, 2);
        assert_eq!(parsed.source, ReviewStateSource::Reviewers);
        assert!(parsed.is_accepted());
    }

    #[test]
    fn test_review_state_aggregates_non_passing_verdicts() {
        let mut verdicts = BTreeMap::new();
        verdicts.insert("architecture".to_string(), Verdict::Pass);
        verdicts.insert("tests".to_string(), Verdict::Uncertain);
        let state = ReviewState::from_verdicts(1, verdicts);
        assert_eq!(state.state, ReviewStateKind::Uncertain);
        assert!(!state.is_accepted());

        let mut verdicts = BTreeMap::new();
        verdicts.insert("architecture".to_string(), Verdict::Fail);
        verdicts.insert("tests".to_string(), Verdict::Uncertain);
        let state = ReviewState::from_verdicts(1, verdicts);
        assert_eq!(state.state, ReviewStateKind::Failed);
        assert!(!state.is_accepted());
    }

    #[test]
    fn test_review_state_accepts_review_limit() {
        let mut verdicts = BTreeMap::new();
        verdicts.insert("tests".to_string(), Verdict::Fail);

        let state = ReviewState::accepted_review_limit(
            11,
            10,
            verdicts,
            "Review round limit reached with a clean worktree.",
        );

        assert_eq!(state.state, ReviewStateKind::AcceptedReviewLimit);
        assert_eq!(state.source, ReviewStateSource::ReviewLimit);
        assert_eq!(state.max_rounds, Some(10));
        assert!(state.is_accepted());
    }

    #[test]
    fn test_read_review_state_rejects_malformed_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        fs::write(
            tmp.path().join("review-state.json"),
            "{\"state\":\"unknown\"}",
        )
        .unwrap();

        assert!(read_review_state(tmp.path()).unwrap().is_err());
    }

    #[test]
    fn test_reviews_accepted_prefers_review_state() {
        let tmp = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("reviews")).unwrap();
        fs::write(tmp.path().join("reviews/review-tests.md"), "Verdict: fail").unwrap();
        let mut verdicts = BTreeMap::new();
        verdicts.insert("tests".to_string(), Verdict::Fail);
        write_review_state(
            tmp.path(),
            &ReviewState::accepted_review_limit(
                10,
                10,
                verdicts,
                "Review round limit reached with a clean worktree.",
            ),
        )
        .unwrap();

        assert_eq!(reviews_accepted_at(tmp.path()), Some(true));
    }

    #[test]
    fn test_reviews_accepted_falls_back_to_artifacts() {
        let tmp = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("reviews")).unwrap();
        fs::write(tmp.path().join("reviews/review-tests.md"), "Verdict: pass").unwrap();
        fs::write(
            tmp.path().join("reviews/review-architecture.md"),
            "Verdict: pass",
        )
        .unwrap();

        assert_eq!(reviews_accepted_at(tmp.path()), Some(true));

        fs::write(
            tmp.path().join("reviews/review-architecture.md"),
            "Verdict: uncertain",
        )
        .unwrap();
        assert_eq!(reviews_accepted_at(tmp.path()), Some(false));
    }

    #[test]
    fn test_effective_reviews_accepted_uses_primary_first() {
        let tmp = tempfile::TempDir::new().unwrap();
        let primary = tmp.path().join("live");
        let fallback = tmp.path().join("source");
        fs::create_dir_all(primary.join("reviews")).unwrap();
        fs::create_dir_all(fallback.join("reviews")).unwrap();
        fs::write(primary.join("reviews/review-tests.md"), "Verdict: pass").unwrap();
        fs::write(fallback.join("reviews/review-tests.md"), "Verdict: fail").unwrap();

        assert_eq!(effective_reviews_accepted(&primary, &fallback), Some(true));
    }
}
