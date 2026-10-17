//! Classify immutable Tester results and failed-command recheck artifacts.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::work_model::{Attempt, RequiredCheckSource, work_artifact_path};

/// A reviewed host pass for the Writer's canonical command can cover the same
/// sandbox-only failures after Learning adds expertise. Bind the allowance to
/// the original Tester failures and recorded comparison inputs, and reject any
/// executable or check change. Historical inspection uses the landing snapshot,
/// since mutable comparison refs may have moved after that run.
pub(crate) fn reviewed_host_proof_covers_expertise_only_merge(
    project_root: &Path,
    attempt: &Attempt,
    merged_commit: &str,
    current_path: &Path,
) -> Result<bool> {
    let current: crate::tester::TesterResults = serde_json::from_slice(&fs::read(current_path)?)?;
    if current.error.is_some()
        || current.candidate_commit.as_deref() != Some(merged_commit)
        || current.commands.iter().any(|command| {
            command
                .process_settlement
                .as_ref()
                .is_some_and(|settlement| !settlement.leaks.is_empty())
        })
    {
        return Ok(false);
    }
    for recovery in attempt.evidence_recoveries.iter().rev() {
        let Some(check) = recovery.required_check.as_ref() else {
            continue;
        };
        if check.work_item_id != attempt.work_item_id || check.attempt_id != attempt.id {
            continue;
        }
        let RequiredCheckSource::CanonicalTester {
            command_index,
            results,
            manifest,
            ..
        } = &check.source
        else {
            continue;
        };
        if current
            .commands
            .get(*command_index)
            .is_none_or(|command| command.exit_code == 0 || command.command != check.command)
        {
            continue;
        }
        let ancestor = crate::git::run_raw(
            project_root,
            &[
                "merge-base",
                "--is-ancestor",
                &check.candidate_commit,
                merged_commit,
            ],
        )?;
        if !ancestor.status.success() {
            continue;
        }
        let changed = crate::git::run_stdout(
            project_root,
            &[
                "diff",
                "--name-only",
                "--no-renames",
                &check.candidate_commit,
                merged_commit,
            ],
            "inspect post-proof changes before carrying reviewed host proof",
        )?;
        if changed
            .lines()
            .any(|path| !path.starts_with(".fluent/expertise/"))
        {
            continue;
        }
        if crate::verification_assessment::validate_required_check_acceptance_at(
            project_root,
            attempt,
            check,
            crate::check_applicability::evaluation_time(),
        )
        .is_err()
        {
            return Ok(false);
        }
        let (_, source_manifest_bytes) =
            crate::verification_assessment::bound_recovery_artifact(project_root, &manifest.path)?;
        let source_manifest: crate::tester::CanonicalRunManifest =
            serde_json::from_slice(&source_manifest_bytes)?;
        let current_manifest: crate::tester::CanonicalRunManifest = serde_json::from_slice(
            &fs::read(current_path.with_file_name("canonical-run.json"))?,
        )?;
        if current_manifest.schema_version != 1
            || !current_manifest.full_run
            || current_manifest.config_digest != source_manifest.config_digest
            || current_manifest.extractor_digest != source_manifest.extractor_digest
            || current_manifest.commands != source_manifest.commands
            || current_manifest.commands.len() != current.commands.len()
        {
            return Ok(false);
        }
        let Some(definition) = current_manifest.commands.get(*command_index) else {
            return Ok(false);
        };
        let applicability = match current_manifest.applicability.get(*command_index) {
            Some(binding) => binding.clone(),
            None if definition.applicability.is_default() => Default::default(),
            None => return Ok(false),
        };
        // Acceptance above validates the host document against this saved
        // contract. The landing run must have captured exactly the same inputs.
        if applicability.definition != definition.applicability
            || applicability != check.applicability
        {
            return Ok(false);
        }
        let (actual, source_bytes) =
            crate::verification_assessment::bound_recovery_artifact(project_root, &results.path)?;
        if actual != *results {
            return Ok(false);
        }
        let source: crate::tester::TesterResults = serde_json::from_slice(&source_bytes)?;
        return Ok(
            source.candidate_commit.as_deref() == Some(check.candidate_commit.as_str())
                && reviewed_command_covers_failures(
                    &source,
                    &current,
                    *command_index,
                    &check.command,
                ),
        );
    }
    Ok(false)
}

fn reviewed_command_covers_failures(
    source: &crate::tester::TesterResults,
    current: &crate::tester::TesterResults,
    command_index: usize,
    command: &str,
) -> bool {
    let Some((source_command, current_command)) = source
        .commands
        .get(command_index)
        .zip(current.commands.get(command_index))
    else {
        return false;
    };
    let command_identities = source
        .commands
        .iter()
        .map(|entry| (entry.command.as_str(), entry.test_harness.as_str()))
        .collect::<Vec<_>>();
    if source.error.is_some()
        || current.error.is_some()
        || !command_identities.iter().copied().eq(current
            .commands
            .iter()
            .map(|entry| (entry.command.as_str(), entry.test_harness.as_str())))
        || command != source_command.command
        || current_command.command != source_command.command
        || current_command.test_harness != source_command.test_harness
        || source_command.exit_code == 0
        || current_command.exit_code == 0
        || [source, current].iter().any(|results| {
            results.commands.iter().enumerate().any(|(index, command)| {
                (command.exit_code != 0 && index != command_index)
                    || command
                        .process_settlement
                        .as_ref()
                        .is_some_and(|settlement| !settlement.leaks.is_empty())
            })
        })
    {
        return false;
    }
    let source_failures = source
        .tests
        .iter()
        .filter(|test| test.status == "fail" && test.test_harness == source_command.test_harness)
        .map(|test| test.id.as_str())
        .collect::<HashSet<_>>();
    let current_failures = current
        .tests
        .iter()
        .filter(|test| test.status == "fail")
        .collect::<Vec<_>>();
    !current_failures.is_empty()
        && current_failures.iter().all(|test| {
            crate::verification_assessment::failed_test_covered_by_accepted_command(
                &command_identities,
                &[command_index],
                test,
            ) && source_failures.contains(test.id.as_str())
        })
}

pub(crate) fn tester_failures(tester_results_path: &Path) -> usize {
    let content = match fs::read_to_string(tester_results_path) {
        Ok(content) => content,
        Err(_) => return 0,
    };
    let results: crate::tester::TesterResults = match serde_json::from_str(&content) {
        Ok(results) => results,
        Err(_) => return 0,
    };
    results
        .tests
        .iter()
        .filter(|test| test.status == "fail")
        .count()
}

pub(crate) fn failing_ids(path: &Path) -> HashSet<String> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => return HashSet::new(),
    };
    let results: crate::tester::TesterResults = match serde_json::from_str(&content) {
        Ok(results) => results,
        Err(_) => return HashSet::new(),
    };
    results
        .tests
        .iter()
        .filter(|test| test.status == "fail")
        .map(|test| test.id.clone())
        .collect()
}

pub(crate) fn introduced_tester_failures(
    current_path: &Path,
    baseline_path: Option<&Path>,
) -> usize {
    match baseline_path {
        Some(baseline_path) => {
            let current = failing_ids(current_path);
            let baseline = failing_ids(baseline_path);
            current.difference(&baseline).count()
        }
        None => tester_failures(current_path),
    }
}

pub(crate) fn introduced_tester_failures_except_command(
    current_path: &Path,
    baseline_path: Option<&Path>,
    command_index: usize,
    command: &str,
) -> usize {
    let mut current = failing_ids(current_path);
    current.remove(&format!("command-{command_index}:{command}"));
    match baseline_path {
        Some(baseline_path) => {
            let baseline = failing_ids(baseline_path);
            current.difference(&baseline).count()
        }
        None => current.len(),
    }
}

pub(crate) fn unexplained_failed_tester_commands(
    tester_results_path: &Path,
    baseline_path: Option<&Path>,
) -> usize {
    let current: crate::tester::TesterResults = match fs::read(tester_results_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<crate::tester::TesterResults>(&bytes).ok())
    {
        Some(results) => results,
        None => return 0,
    };
    let baseline = baseline_path
        .and_then(|path| fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<crate::tester::TesterResults>(&bytes).ok());
    current
        .commands
        .iter()
        .filter(|command| command.exit_code != 0)
        .count()
        - preexisting_failed_tester_commands(&current, baseline.as_ref()).len()
}

/// Attribute a failed command only through matching native baseline failures.
pub(crate) fn preexisting_failed_tester_commands(
    current: &crate::tester::TesterResults,
    baseline: Option<&crate::tester::TesterResults>,
) -> Vec<usize> {
    let Some(baseline) =
        baseline.filter(|baseline| current.error.is_none() && baseline.error.is_none())
    else {
        return vec![];
    };
    current
        .commands
        .iter()
        .enumerate()
        .filter(|(index, command)| {
            if command.exit_code == 0 {
                return false;
            }
            let Some(baseline_command) = baseline.commands.get(*index) else {
                return false;
            };
            let command_identity_matches = command.command == baseline_command.command
                && command.test_harness == baseline_command.test_harness
                && command.exit_code == baseline_command.exit_code;
            let settlements_are_clean = [command, baseline_command].iter().all(|command| {
                command
                    .process_settlement
                    .as_ref()
                    .is_none_or(|settlement| settlement.leaks.is_empty())
            });
            let harness_is_unambiguous = current
                .commands
                .iter()
                .filter(|candidate| {
                    candidate.exit_code != 0 && candidate.test_harness == command.test_harness
                })
                .count()
                == 1
                && baseline
                    .commands
                    .iter()
                    .filter(|candidate| {
                        candidate.exit_code != 0 && candidate.test_harness == command.test_harness
                    })
                    .count()
                    == 1;
            let current_failures = current
                .tests
                .iter()
                .filter(|test| test.status == "fail" && test.test_harness == command.test_harness)
                .map(|test| test.id.as_str())
                .collect::<HashSet<_>>();
            let baseline_failures = baseline
                .tests
                .iter()
                .filter(|test| test.status == "fail" && test.test_harness == command.test_harness)
                .map(|test| test.id.as_str())
                .collect::<HashSet<_>>();
            command_identity_matches
                && settlements_are_clean
                && harness_is_unambiguous
                && !current_failures.is_empty()
                && current_failures.is_subset(&baseline_failures)
        })
        .map(|(index, _)| index)
        .collect()
}

pub(crate) fn baseline_tester_results_path(
    project_root: &Path,
    attempt: &Attempt,
) -> Option<PathBuf> {
    let artifact = format!("{}-baseline-tester", attempt.id);
    let path = project_root
        .join(work_artifact_path(
            &attempt.work_item_id,
            &attempt.id,
            &artifact,
        ))
        .join("tester-results.json");
    path.is_file().then_some(path)
}

pub(crate) use crate::verification_assessment::failed_command_recheck_correction_artifacts;

#[cfg(test)]
mod reviewed_host_proof_tests {
    use crate::tester::{CommandResult, Summary, TestResult, TesterResults};

    #[test]
    fn reviewed_host_proof_binds_source_and_expertise_ancestry() {
        use crate::recovery_contracts::verification_tests::{
            accept_first_check, unique_harness_fixture,
        };
        use crate::verification_assessment::assess_canonical_checks;
        use std::fs;

        let (root, mut item) = unique_harness_fixture();
        accept_first_check(root.path(), &mut item);
        let attempt = &item.attempts[0];
        let source_path = assess_canonical_checks(root.path(), attempt)
            .unwrap()
            .results_path;
        let source_bytes = fs::read(&source_path).unwrap();
        let source: TesterResults = serde_json::from_slice(&source_bytes).unwrap();
        fs::create_dir_all(root.path().join(".fluent/expertise")).unwrap();
        fs::write(
            root.path().join(".fluent/expertise/lesson.md"),
            "Retain reviewed proof.\n",
        )
        .unwrap();
        crate::git::run(
            root.path(),
            &["add", ".fluent/expertise/lesson.md"],
            "stage expertise",
        )
        .unwrap();
        crate::git::run(
            root.path(),
            &["commit", "-qm", "Record lesson"],
            "commit expertise",
        )
        .unwrap();
        let merged = crate::git::run_stdout(
            root.path(),
            &["rev-parse", "HEAD"],
            "read expertise candidate",
        )
        .unwrap();
        let mut current = source.clone();
        current.candidate_commit = Some(merged.clone());
        let current_path = root.path().join("landing-results.json");
        fs::copy(
            source_path.with_file_name("canonical-run.json"),
            current_path.with_file_name("canonical-run.json"),
        )
        .unwrap();
        let check =
            |attempt: &crate::work_model::Attempt, candidate: &str, results: &TesterResults| {
                fs::write(&current_path, serde_json::to_vec(results).unwrap()).unwrap();
                super::reviewed_host_proof_covers_expertise_only_merge(
                    root.path(),
                    attempt,
                    candidate,
                    &current_path,
                )
                .unwrap_or(false)
            };
        assert!(check(attempt, &merged, &current));
        assert!(
            !check(attempt, &merged, &source),
            "current candidate must match"
        );
        let mut new_failure = current.clone();
        let mut test = new_failure.tests[0].clone();
        test.id = "tests/native.sh::new_regression".into();
        new_failure.tests.push(test);
        new_failure.summary.fail += 1;
        new_failure.summary.total += 1;
        assert!(!check(attempt, &merged, &new_failure));
        let mut unreviewed = attempt.clone();
        unreviewed.evidence_recoveries[0].state =
            crate::work_model::EvidenceRecoveryState::Reviewing;
        assert!(!check(&unreviewed, &merged, &current));
        let recovery = &attempt.evidence_recoveries[0];
        let review = attempt
            .tasks
            .iter()
            .find(|task| Some(&task.id) == recovery.targets[0].review_task_id.as_ref())
            .unwrap();
        for path in [
            source_path.clone(),
            root.path().join(&recovery.attachment.snapshot_path),
            root.path()
                .join(&review.artifact_area.as_ref().unwrap().path)
                .join("review.md"),
        ] {
            let original = fs::read(&path).unwrap();
            fs::write(&path, b"changed retained evidence\n").unwrap();
            assert!(!check(attempt, &merged, &current), "{}", path.display());
            fs::write(path, original).unwrap();
        }
        assert!(check(attempt, &merged, &current));
        assert_eq!(fs::read(&source_path).unwrap(), source_bytes);
        let ancestor = crate::git::run_stdout(
            root.path(),
            &["rev-parse", "HEAD~2"],
            "read earlier candidate",
        )
        .unwrap();
        let mut earlier = current.clone();
        earlier.candidate_commit = Some(ancestor.clone());
        assert!(!check(attempt, &ancestor, &earlier));
        fs::write(root.path().join("tests/native.sh"), "#!/bin/sh\nexit 1\n").unwrap();
        crate::git::run(
            root.path(),
            &["add", "tests/native.sh"],
            "stage executable change",
        )
        .unwrap();
        crate::git::run(
            root.path(),
            &["commit", "-qm", "Change executable"],
            "commit executable change",
        )
        .unwrap();
        let changed = crate::git::run_stdout(
            root.path(),
            &["rev-parse", "HEAD"],
            "read changed candidate",
        )
        .unwrap();
        current.candidate_commit = Some(changed.clone());
        assert!(!check(attempt, &changed, &current));
    }

    #[test]
    fn reviewed_host_proof_binds_landing_comparison_inputs() {
        use crate::recovery_contracts::verification_tests::{
            accept_first_check, unique_harness_fixture_with_comparison,
        };
        use crate::verification_assessment::assess_canonical_checks;
        use std::fs;

        let (root, mut item) = unique_harness_fixture_with_comparison(true);
        accept_first_check(root.path(), &mut item);
        let attempt = &item.attempts[0];
        let assessment = assess_canonical_checks(root.path(), attempt).unwrap();
        let original_ref =
            assessment.checks[0].applicability.comparison_commits["baseline"].clone();
        let retained_paths = [
            assessment.results_path.clone(),
            assessment.results_path.with_file_name("canonical-run.json"),
            root.path()
                .join(&attempt.evidence_recoveries[0].attachment.snapshot_path),
        ];
        let retained = retained_paths.map(|path| {
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        });
        fs::create_dir_all(root.path().join(".fluent/expertise")).unwrap();
        fs::write(
            root.path().join(".fluent/expertise/lesson.md"),
            "Retain comparison inputs.\n",
        )
        .unwrap();
        crate::git::run(
            root.path(),
            &["add", ".fluent/expertise/lesson.md"],
            "stage lesson",
        )
        .unwrap();
        crate::git::run(
            root.path(),
            &["commit", "-qm", "Record lesson"],
            "commit lesson",
        )
        .unwrap();
        let merged =
            crate::git::run_stdout(root.path(), &["rev-parse", "HEAD"], "read candidate").unwrap();
        let area = root.path().join(crate::work_model::work_artifact_path(
            &item.id,
            &attempt.id,
            "landing",
        ));
        let results_path = area.join("tester-results.json");
        let manifest_path = area.join("canonical-run.json");
        fs::create_dir_all(&area).unwrap();
        let run = || {
            crate::tester::run(
                root.path(),
                &area,
                true,
                &crate::content::ContentResolver::new(Some(root.path())),
            )
            .unwrap();
            crate::tester::bind_candidate_commit(&area, &merged).unwrap();
        };
        let covered = || {
            super::reviewed_host_proof_covers_expertise_only_merge(
                root.path(),
                attempt,
                &merged,
                &results_path,
            )
            .unwrap()
        };
        run();
        assert!(
            covered(),
            "unchanged comparison inputs retain reviewed coverage"
        );
        let original_manifest = fs::read(&manifest_path).unwrap();
        let original_results: TesterResults =
            serde_json::from_slice(&fs::read(&results_path).unwrap()).unwrap();

        crate::git::run(
            root.path(),
            &["branch", "-f", "baseline", &merged],
            "move comparison ref",
        )
        .unwrap();
        run();
        let changed_results: TesterResults =
            serde_json::from_slice(&fs::read(&results_path).unwrap()).unwrap();
        assert_eq!(
            original_results.candidate_commit,
            changed_results.candidate_commit
        );
        assert!(
            original_results
                .tests
                .iter()
                .map(|test| (&test.id, &test.test_harness, &test.status))
                .eq(changed_results.tests.iter().map(|test| (
                    &test.id,
                    &test.test_harness,
                    &test.status
                )))
        );
        assert!(
            !covered(),
            "the same failures against a moved comparison ref need new proof"
        );

        crate::git::run(
            root.path(),
            &["branch", "-f", "baseline", &original_ref],
            "restore ref",
        )
        .unwrap();
        assert!(
            !covered(),
            "history uses the landing run's recorded inputs, not today's ref"
        );
        fs::write(&manifest_path, &original_manifest).unwrap();
        crate::git::run(
            root.path(),
            &["branch", "-f", "baseline", &merged],
            "move ref after run",
        )
        .unwrap();
        assert!(
            covered(),
            "later ref movement does not rewrite the historical run"
        );
        let mut missing: crate::tester::CanonicalRunManifest =
            serde_json::from_slice(&original_manifest).unwrap();
        missing.applicability.clear();
        fs::write(&manifest_path, serde_json::to_vec(&missing).unwrap()).unwrap();
        assert!(
            !covered(),
            "non-default applicability requires a recorded binding"
        );
        for (path, bytes) in retained {
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
    }

    fn result(failures: &[(&str, &str)]) -> TesterResults {
        TesterResults {
            candidate_commit: Some("candidate".into()),
            commands: vec![
                CommandResult {
                    failure_cause: None,
                    command: "cargo nextest run".into(),
                    test_harness: "cargo-nextest".into(),
                    exit_code: 100,
                    duration_ms: 1,
                    stdout_log: "commands/0-stdout.log".into(),
                    stderr_log: "commands/0-stderr.log".into(),
                    process_settlement: None,
                },
                CommandResult {
                    failure_cause: None,
                    command: "bash tests/behaviors.sh".into(),
                    test_harness: "shell-harness".into(),
                    exit_code: 0,
                    duration_ms: 1,
                    stdout_log: "commands/1-stdout.log".into(),
                    stderr_log: "commands/1-stderr.log".into(),
                    process_settlement: None,
                },
            ],
            tests: failures
                .iter()
                .map(|(id, harness)| TestResult {
                    identity: None,
                    id: (*id).into(),
                    test_harness: (*harness).into(),
                    status: "fail".into(),
                    duration_ms: Some(1),
                    failure_excerpt: None,
                })
                .collect(),
            summary: Summary {
                total: failures.len(),
                pass: 0,
                fail: failures.len(),
                skipped: 0,
            },
            error: None,
        }
    }

    #[test]
    fn accepted_command_covers_only_its_original_failed_tests() {
        let source = result(&[("terminal::control", "cargo-nextest")]);
        let same = result(&[("terminal::control", "cargo-nextest")]);
        assert!(super::reviewed_command_covers_failures(
            &source,
            &same,
            0,
            "cargo nextest run"
        ));

        let new_failure = result(&[
            ("terminal::control", "cargo-nextest"),
            ("terminal::new_regression", "cargo-nextest"),
        ]);
        assert!(!super::reviewed_command_covers_failures(
            &source,
            &new_failure,
            0,
            "cargo nextest run"
        ));
        let other_harness = result(&[("terminal::control", "shell-harness")]);
        assert!(!super::reviewed_command_covers_failures(
            &source,
            &other_harness,
            0,
            "cargo nextest run"
        ));
        let mut other_command = same.clone();
        other_command.commands[1].exit_code = 1;
        assert!(!super::reviewed_command_covers_failures(
            &source,
            &other_command,
            0,
            "cargo nextest run"
        ));
        let mut changed_command = same;
        changed_command.commands[0]
            .command
            .push_str(" --skip regression");
        assert!(!super::reviewed_command_covers_failures(
            &source,
            &changed_command,
            0,
            "cargo nextest run"
        ));

        let mut duplicate = source.clone();
        duplicate.commands[1].test_harness = "cargo-nextest".into();
        assert!(!super::reviewed_command_covers_failures(
            &duplicate,
            &duplicate,
            0,
            "cargo nextest run"
        ));
        duplicate.tests[0].id = "command-0:cargo nextest run".into();
        assert!(super::reviewed_command_covers_failures(
            &duplicate,
            &duplicate,
            0,
            "cargo nextest run"
        ));
        let mut missing_command = source.clone();
        missing_command.commands.pop();
        assert!(!super::reviewed_command_covers_failures(
            &source,
            &missing_command,
            0,
            "cargo nextest run"
        ));
    }
}
