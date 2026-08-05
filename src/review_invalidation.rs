//! Decide which passing review domains become stale after a candidate change.

use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::path::{Component, Path};

const DOCUMENTATION: &str = "documentation";
const BEHAVIORS: &str = "behaviors";
const ARCHITECTURE: &str = "architecture";
const SKILLS: &str = "skills";
const TESTS: &str = "tests";

pub(crate) fn changed_paths_between(
    workspace: &Path,
    reviewed_commit: &str,
    candidate_commit: &str,
) -> Result<Vec<String>> {
    if reviewed_commit.is_empty() || candidate_commit.is_empty() {
        bail!("review invalidation requires two non-empty candidate commits");
    }
    let output = crate::git::run_raw(
        workspace,
        &[
            "diff",
            "--name-status",
            "-z",
            "--find-renames",
            "--diff-filter=ACDMRTUXB",
            reviewed_commit,
            candidate_commit,
        ],
    )?;
    if !output.status.success() {
        bail!(
            "cannot compare reviewed commit {reviewed_commit} with candidate {candidate_commit}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let mut fields = output.stdout.split(|byte| *byte == 0);
    let mut paths = Vec::new();
    while let Some(status) = fields.next().filter(|field| !field.is_empty()) {
        let status = std::str::from_utf8(status).context("Git diff status is not UTF-8")?;
        let path_count = if status.starts_with('R') || status.starts_with('C') {
            2
        } else {
            1
        };
        for _ in 0..path_count {
            let raw = fields
                .next()
                .filter(|field| !field.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("Git diff omitted a path after status {status:?}")
                })?;
            let path = std::str::from_utf8(raw).context("changed path is not UTF-8")?;
            if !paths.iter().any(|existing| existing == path) {
                paths.push(path.to_string());
            }
        }
    }
    Ok(paths)
}

pub(crate) fn affected_review_roles(paths: &[impl AsRef<str>]) -> Vec<&'static str> {
    let mut affected = BTreeSet::new();
    for raw in paths {
        let raw = raw.as_ref();
        let path = Path::new(raw);
        if raw.is_empty()
            || path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return crate::review::REVIEWERS.to_vec();
        }

        let mut add = |roles: &[&'static str]| affected.extend(roles.iter().copied());
        if raw.starts_with(".fluent/expertise/") {
            // Capture Learners exclusively own this post-review domain. Their
            // mutation ledger and final candidate gate validate it separately.
        } else if raw == "documentation/architecture.md" || raw == "expertise/architecture.md" {
            add(&[DOCUMENTATION, ARCHITECTURE]);
        } else if raw == "documentation/behaviors.md" || raw == "expertise/behaviors.md" {
            add(&[DOCUMENTATION, BEHAVIORS, TESTS]);
        } else if raw.starts_with("documentation/")
            || raw == "README.md"
            || raw == "LICENSE"
            || raw.starts_with("CHANGELOG")
        {
            add(&[DOCUMENTATION]);
        } else if raw == "expertise/tests.md" {
            add(&[DOCUMENTATION, TESTS]);
        } else if raw == "expertise/skills.md" {
            add(&[DOCUMENTATION, SKILLS]);
        } else if raw == "expertise/documentation.md" || raw == "expertise/README.md" {
            add(&[DOCUMENTATION]);
        } else if raw.starts_with("expertise/") {
            return crate::review::REVIEWERS.to_vec();
        } else if raw.starts_with("skills/") {
            add(&[DOCUMENTATION, SKILLS]);
        } else if raw.starts_with("skill-migrations/") {
            add(&[DOCUMENTATION, SKILLS, TESTS]);
        } else if raw.starts_with("prompts/") {
            add(&[DOCUMENTATION, BEHAVIORS, SKILLS, TESTS]);
        } else if raw.starts_with("tests/") {
            add(&[TESTS]);
        } else if raw.starts_with("src/")
            || raw.starts_with("scripts/")
            || raw.starts_with("tools/")
            || raw.starts_with("infrastructure/")
            || raw.starts_with("sandboxes/")
            || raw.starts_with(".fluent/hooks/")
            || raw == ".fluent/tester.yaml"
            || raw == ".fluent/extract-tester-results"
            || matches!(raw, "Cargo.toml" | "Cargo.lock" | "build.rs")
        {
            add(&[DOCUMENTATION, ARCHITECTURE, TESTS]);
        } else if matches!(raw, "AGENTS.md" | "CLAUDE.md") {
            return crate::review::REVIEWERS.to_vec();
        } else {
            return crate::review::REVIEWERS.to_vec();
        }
    }

    crate::review::REVIEWERS
        .iter()
        .copied()
        .filter(|role| affected.contains(role))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{affected_review_roles, changed_paths_between};
    use std::fs;

    #[test]
    fn known_paths_invalidate_only_their_review_domains() {
        assert_eq!(
            affected_review_roles(&["documentation/guide.md"]),
            vec!["documentation"]
        );
        assert_eq!(
            affected_review_roles(&["documentation/architecture.md"]),
            vec!["documentation", "architecture"]
        );
        assert_eq!(
            affected_review_roles(&["documentation/behaviors.md"]),
            vec!["documentation", "behaviors", "tests"]
        );
        assert_eq!(
            affected_review_roles(&["src/work_attempt_loop.rs"]),
            vec!["documentation", "architecture", "tests"]
        );
        assert_eq!(affected_review_roles(&["tests/binary.rs"]), vec!["tests"]);
        assert_eq!(
            affected_review_roles(&["skills/fluent.full/fluent.md"]),
            vec!["documentation", "skills"]
        );
        assert_eq!(
            affected_review_roles(&["prompts/write-user.md"]),
            vec!["documentation", "behaviors", "skills", "tests"]
        );
        assert_eq!(
            affected_review_roles(&["src/work_attempt_loop.rs", "skills/fluent.full/fluent.md",]),
            vec!["documentation", "architecture", "skills", "tests"]
        );
    }

    #[test]
    fn learner_owned_expertise_does_not_stale_pre_learner_reviews() {
        assert!(affected_review_roles(&[".fluent/expertise/testing.md"]).is_empty());
    }

    #[test]
    fn rename_paths_are_classified_together() {
        assert_eq!(
            affected_review_roles(&["documentation/old-guide.md", "documentation/new-guide.md",]),
            vec!["documentation"]
        );
    }

    #[test]
    fn unknown_or_unsafe_paths_invalidate_every_reviewer() {
        assert_eq!(
            affected_review_roles(&["unexpected/file.xyz"]),
            crate::review::REVIEWERS
        );
        assert_eq!(
            affected_review_roles(&["/absolute/path.rs"]),
            crate::review::REVIEWERS
        );
        assert_eq!(
            affected_review_roles(&["documentation/../src/lib.rs"]),
            crate::review::REVIEWERS
        );
    }

    #[test]
    fn changed_paths_include_both_sides_of_a_rename() {
        let repo = tempfile::TempDir::new().unwrap();
        crate::git::run(repo.path(), &["init", "-q"], "initialize review fixture").unwrap();
        crate::git::run(
            repo.path(),
            &["config", "user.name", "Fluent Test"],
            "configure review fixture",
        )
        .unwrap();
        crate::git::run(
            repo.path(),
            &["config", "user.email", "fluent@example.test"],
            "configure review fixture",
        )
        .unwrap();
        fs::create_dir_all(repo.path().join("documentation")).unwrap();
        fs::write(repo.path().join("documentation/old.md"), "old\n").unwrap();
        crate::git::run(repo.path(), &["add", "."], "stage review base").unwrap();
        crate::git::run(
            repo.path(),
            &["commit", "-q", "-m", "Add review base"],
            "commit review base",
        )
        .unwrap();
        let base =
            crate::git::run_stdout(repo.path(), &["rev-parse", "HEAD"], "read base").unwrap();
        crate::git::run(
            repo.path(),
            &["mv", "documentation/old.md", "documentation/new.md"],
            "rename documentation",
        )
        .unwrap();
        crate::git::run(
            repo.path(),
            &["commit", "-q", "-m", "Rename documentation"],
            "commit documentation rename",
        )
        .unwrap();
        let candidate =
            crate::git::run_stdout(repo.path(), &["rev-parse", "HEAD"], "read candidate").unwrap();

        assert_eq!(
            changed_paths_between(repo.path(), &base, &candidate).unwrap(),
            vec!["documentation/old.md", "documentation/new.md"]
        );
    }

    #[test]
    fn changed_paths_fail_closed_when_git_cannot_compare_commits() {
        let repo = tempfile::TempDir::new().unwrap();
        crate::git::run(repo.path(), &["init", "-q"], "initialize invalid fixture").unwrap();

        assert!(changed_paths_between(repo.path(), "missing", "also-missing").is_err());
    }

    #[test]
    fn changed_paths_include_deletions() {
        let repo = tempfile::TempDir::new().unwrap();
        crate::git::run(repo.path(), &["init", "-q"], "initialize deletion fixture").unwrap();
        crate::git::run(
            repo.path(),
            &["config", "user.name", "Fluent Test"],
            "configure deletion fixture",
        )
        .unwrap();
        crate::git::run(
            repo.path(),
            &["config", "user.email", "fluent@example.test"],
            "configure deletion fixture",
        )
        .unwrap();
        fs::create_dir_all(repo.path().join("tests")).unwrap();
        fs::write(repo.path().join("tests/obsolete.rs"), "obsolete\n").unwrap();
        crate::git::run(repo.path(), &["add", "."], "stage deletion base").unwrap();
        crate::git::run(
            repo.path(),
            &["commit", "-q", "-m", "Add obsolete test"],
            "commit deletion base",
        )
        .unwrap();
        let base =
            crate::git::run_stdout(repo.path(), &["rev-parse", "HEAD"], "read deletion base")
                .unwrap();
        fs::remove_file(repo.path().join("tests/obsolete.rs")).unwrap();
        crate::git::run(repo.path(), &["add", "-u"], "stage test deletion").unwrap();
        crate::git::run(
            repo.path(),
            &["commit", "-q", "-m", "Delete obsolete test"],
            "commit test deletion",
        )
        .unwrap();
        let candidate = crate::git::run_stdout(
            repo.path(),
            &["rev-parse", "HEAD"],
            "read deletion candidate",
        )
        .unwrap();

        assert_eq!(
            changed_paths_between(repo.path(), &base, &candidate).unwrap(),
            vec!["tests/obsolete.rs"]
        );
    }
}
