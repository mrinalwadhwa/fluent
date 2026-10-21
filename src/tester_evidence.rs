//! Classify immutable Tester results and failed-command recheck artifacts.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::work_model::{Attempt, RequiredCheckSource, work_artifact_path};

/// Reviewed host passes can cover their exact canonical command failures after
/// Learning adds expertise. Other failed commands must match the target baseline
/// when one is supplied. Validate every proof before classifying failures. Bind the
/// allowance to the original Tester and comparison inputs, and reject any
/// executable or check change. Historical inspection uses the landing snapshot,
/// since mutable comparison refs may have moved after that run.
pub(crate) fn reviewed_host_proof_covers_expertise_only_merge(
    project_root: &Path,
    attempt: &Attempt,
    merged_commit: &str,
    current_path: &Path,
    baseline_path: Option<&Path>,
) -> Result<bool> {
    let current: crate::tester::TesterResults = serde_json::from_slice(&fs::read(current_path)?)?;
    let baseline = baseline_path
        .map(|path| -> Result<crate::tester::TesterResults> {
            Ok(serde_json::from_slice(&fs::read(path)?)?)
        })
        .transpose()?;
    if current.error.is_some()
        || baseline
            .as_ref()
            .is_some_and(|baseline| baseline.error.is_some())
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
    let mut reviewed_sources = BTreeMap::new();
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
        if reviewed_sources.contains_key(command_index) {
            continue;
        }
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
        if source.candidate_commit.as_deref() != Some(check.candidate_commit.as_str()) {
            return Ok(false);
        }
        reviewed_sources.insert(*command_index, source);
    }
    Ok(reviewed_commands_cover_failures(
        &reviewed_sources,
        &current,
        baseline.as_ref(),
    ))
}

fn reviewed_commands_cover_failures(
    sources: &BTreeMap<usize, crate::tester::TesterResults>,
    current: &crate::tester::TesterResults,
    baseline: Option<&crate::tester::TesterResults>,
) -> bool {
    let command_identities = current
        .commands
        .iter()
        .map(|entry| (entry.command.as_str(), entry.test_harness.as_str()))
        .collect::<Vec<_>>();
    if sources.is_empty()
        || sources.iter().any(|(index, source)| {
            source
                .commands
                .get(*index)
                .zip(current.commands.get(*index))
                .is_none_or(|(source_command, current_command)| {
                    source_command.exit_code == 0 || current_command.exit_code == 0
                })
        })
        || sources
            .values()
            .chain(std::iter::once(current))
            .any(|results| {
                let preexisting = preexisting_failed_tester_commands(results, baseline);
                results.error.is_some()
                    || !command_identities.iter().copied().eq(results
                        .commands
                        .iter()
                        .map(|entry| (entry.command.as_str(), entry.test_harness.as_str())))
                    || results.commands.iter().enumerate().any(|(index, command)| {
                        (command.exit_code != 0
                            && !sources.contains_key(&index)
                            && !preexisting.contains(&index))
                            || command
                                .process_settlement
                                .as_ref()
                                .is_some_and(|settlement| !settlement.leaks.is_empty())
                    })
            })
    {
        return false;
    }
    let baseline_failures = baseline
        .into_iter()
        .flat_map(|results| results.tests.iter())
        .filter(|test| test.status == "fail")
        .map(|test| (test.id.as_str(), test.test_harness.as_str()))
        .collect::<HashSet<_>>();
    let accepted = sources.keys().copied().collect::<Vec<_>>();
    let current_failures = current
        .tests
        .iter()
        .filter(|test| test.status == "fail")
        .collect::<Vec<_>>();
    (baseline.is_some() || !current_failures.is_empty())
        && sources
            .values()
            .flat_map(|source| &source.tests)
            .filter(|test| test.status == "fail")
            .all(|test| {
                baseline_failures.contains(&(test.id.as_str(), test.test_harness.as_str()))
                    || crate::verification_assessment::failed_test_covered_by_accepted_command(
                        &command_identities,
                        &accepted,
                        test,
                    )
            })
        && current_failures.iter().all(|test| {
            baseline_failures.contains(&(test.id.as_str(), test.test_harness.as_str()))
                || sources.iter().any(|(index, source)| {
                    // Keep the owning command's original result attached to its
                    // attribution; a different proof's failures cannot replace it.
                    crate::verification_assessment::failed_test_covered_by_accepted_command(
                        &command_identities,
                        &[*index],
                        test,
                    ) && source.tests.iter().any(|original| {
                        original.status == "fail"
                            && original.id == test.id
                            && original.test_harness == test.test_harness
                    })
                })
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
    fn reviewed_host_proof_rejects_ambiguous_native_failures() {
        use std::collections::BTreeMap;

        let mut current = result(&[
            ("native::first", "cargo-nextest"),
            ("native::second", "shell-harness"),
        ]);
        current.commands[1].exit_code = 1;
        let sources = BTreeMap::from([(0, current.clone()), (1, current.clone())]);
        let mut baseline = current.clone();
        baseline
            .commands
            .iter_mut()
            .for_each(|command| command.exit_code = 0);
        baseline.tests.clear();
        assert!(super::reviewed_commands_cover_failures(
            &sources,
            &current,
            Some(&baseline)
        ));

        for index in 0..2 {
            let mut changed = sources.clone();
            // The other original result still contains the failure, but cannot
            // supply it on behalf of the proof for this command.
            changed.get_mut(&index).unwrap().tests.remove(index);
            assert!(
                !super::reviewed_commands_cover_failures(&changed, &current, Some(&baseline)),
                "command {index} needs its own original failure"
            );
            let mut changed = sources.clone();
            let mut unexpected = current.tests[index].clone();
            unexpected.id = "unrelated::new_failure".into();
            unexpected.test_harness = "cargo-test".into();
            changed.get_mut(&index).unwrap().tests.push(unexpected);
            assert!(
                !super::reviewed_commands_cover_failures(&changed, &current, Some(&baseline)),
                "an unrelated original failure blocks even when it disappears at landing"
            );
        }
        let mut new_failure = current.clone();
        new_failure.tests[1].id = "native::new_failure".into();
        assert!(!super::reviewed_commands_cover_failures(
            &sources,
            &new_failure,
            Some(&baseline)
        ));

        current.commands[0].test_harness = "shell-harness".into();
        current.tests[0].test_harness = "shell-harness".into();
        let sources = BTreeMap::from([(0, current.clone()), (1, current.clone())]);
        assert!(
            !super::reviewed_commands_cover_failures(&sources, &current, Some(&baseline)),
            "accepting both commands cannot disambiguate a shared harness"
        );
        let mut no_current_failures = current.clone();
        no_current_failures.tests.clear();
        assert!(
            !super::reviewed_commands_cover_failures(
                &sources,
                &no_current_failures,
                Some(&baseline)
            ),
            "ambiguous original failures still block"
        );

        // Baseline identities include the harness as well as the test id.
        baseline.tests = current.tests.clone();
        assert!(super::reviewed_commands_cover_failures(
            &sources,
            &current,
            Some(&baseline)
        ));
        baseline.tests[1].test_harness = "cargo-test".into();
        assert!(!super::reviewed_commands_cover_failures(
            &sources,
            &current,
            Some(&baseline)
        ));

        // Even identical command text has a distinct synthetic occurrence.
        current.commands[1].command = current.commands[0].command.clone();
        for (index, test) in current.tests.iter_mut().enumerate() {
            test.id = format!("command-{index}:{}", current.commands[index].command);
        }
        let mut sources = BTreeMap::from([(0, current.clone()), (1, current.clone())]);
        assert!(super::reviewed_commands_cover_failures(
            &sources, &current, None
        ));
        sources.get_mut(&1).unwrap().tests.pop();
        assert!(!super::reviewed_commands_cover_failures(
            &sources, &current, None
        ));
    }

    #[test]
    fn reviewed_host_proof_covers_multiple_commands() {
        use crate::recovery_contracts::verification_tests::{accept_check, fixture};
        use crate::verification_assessment::assess_canonical_checks;

        let (root, mut item) = fixture(false);
        accept_check(root.path(), &mut item, 0);
        accept_check(root.path(), &mut item, 1);
        let attempt = &item.attempts[0];
        let assessment = assess_canonical_checks(root.path(), attempt).unwrap();
        let original = std::fs::read(&assessment.results_path).unwrap();
        let candidate = &assessment.checks[0].candidate_commit;
        let baseline_path = root.path().join("baseline.json");
        let mut baseline: TesterResults = serde_json::from_slice(&original).unwrap();
        baseline
            .commands
            .iter_mut()
            .for_each(|command| command.exit_code = 0);
        std::fs::write(&baseline_path, serde_json::to_vec(&baseline).unwrap()).unwrap();
        assert!(
            super::reviewed_host_proof_covers_expertise_only_merge(
                root.path(),
                attempt,
                candidate,
                &assessment.results_path,
                Some(&baseline_path),
            )
            .unwrap()
        );
        assert_eq!(std::fs::read(&assessment.results_path).unwrap(), original);

        // The remaining canonical command needs native baseline attribution.
        let mut current = result(&[("baseline::failure", "cargo-test")]);
        current.commands[1].exit_code = 1;
        let mut third = current.commands[0].clone();
        third.command = "cargo test".into();
        third.test_harness = "cargo-test".into();
        current.commands.push(third);
        let sources =
            std::collections::BTreeMap::from([(0, current.clone()), (1, current.clone())]);
        let mut baseline = current.clone();
        baseline.commands[0].exit_code = 0;
        baseline.commands[1].exit_code = 0;
        assert!(super::reviewed_commands_cover_failures(
            &sources,
            &current,
            Some(&baseline)
        ));
        baseline.commands[2].exit_code = 0;
        assert!(
            !super::reviewed_commands_cover_failures(&sources, &current, Some(&baseline)),
            "an unreviewed third command cannot borrow the other proofs"
        );
    }

    #[test]
    fn reviewed_host_proof_rejects_missing_or_changed_command() {
        use crate::recovery_contracts::verification_tests::{accept_check, fixture};
        use crate::verification_assessment::assess_canonical_checks;
        use crate::work_model::{EvidenceRecoveryState, RequiredCheckSource, TaskStatus};
        use std::fs;

        let (root, mut item) = fixture(false);
        accept_check(root.path(), &mut item, 0);
        accept_check(root.path(), &mut item, 1);
        let attempt = &item.attempts[0];
        let assessment = assess_canonical_checks(root.path(), attempt).unwrap();
        let source_bytes = fs::read(&assessment.results_path).unwrap();
        let current_path = root.path().join("landing-results.json");
        let manifest_path = current_path.with_file_name("canonical-run.json");
        let manifest_bytes =
            fs::read(assessment.results_path.with_file_name("canonical-run.json")).unwrap();
        fs::write(&manifest_path, &manifest_bytes).unwrap();
        let current: TesterResults = serde_json::from_slice(&source_bytes).unwrap();
        let baseline_path = root.path().join("baseline.json");
        let mut baseline = current.clone();
        baseline
            .commands
            .iter_mut()
            .for_each(|command| command.exit_code = 0);
        fs::write(&baseline_path, serde_json::to_vec(&baseline).unwrap()).unwrap();
        let covered = |attempt: &crate::work_model::Attempt, current: &TesterResults| {
            fs::write(&current_path, serde_json::to_vec(current).unwrap()).unwrap();
            super::reviewed_host_proof_covers_expertise_only_merge(
                root.path(),
                attempt,
                current.candidate_commit.as_deref().unwrap(),
                &current_path,
                Some(&baseline_path),
            )
            .unwrap_or(false)
        };
        assert!(covered(attempt, &current));
        // Change each proof independently so iteration order cannot hide a bad binding.
        for index in 0..2 {
            for change in ["missing", "unreviewed", "candidate", "position", "review"] {
                let mut changed = attempt.clone();
                match change {
                    "missing" => {
                        changed.evidence_recoveries.remove(index);
                    }
                    "unreviewed" => {
                        changed.evidence_recoveries[index].state = EvidenceRecoveryState::Reviewing
                    }
                    "candidate" => {
                        changed.evidence_recoveries[index]
                            .required_check
                            .as_mut()
                            .unwrap()
                            .candidate_commit = "0000000000000000000000000000000000000000".into()
                    }
                    "position" => {
                        let RequiredCheckSource::CanonicalTester { command_index, .. } =
                            &mut changed.evidence_recoveries[index]
                                .required_check
                                .as_mut()
                                .unwrap()
                                .source
                        else {
                            unreachable!()
                        };
                        *command_index = 1 - index;
                    }
                    "review" => {
                        let id = changed.evidence_recoveries[index].targets[0]
                            .review_task_id
                            .as_ref()
                            .unwrap();
                        changed
                            .tasks
                            .iter_mut()
                            .find(|task| &task.id == id)
                            .unwrap()
                            .status = TaskStatus::Failed;
                    }
                    _ => unreachable!(),
                }
                assert!(!covered(&changed, &current), "proof {index}: {change}");
            }
            let recovery = &attempt.evidence_recoveries[index];
            let review = attempt
                .tasks
                .iter()
                .find(|task| Some(&task.id) == recovery.targets[0].review_task_id.as_ref())
                .unwrap();
            for path in [
                root.path().join(&recovery.attachment.snapshot_path),
                root.path()
                    .join(&review.artifact_area.as_ref().unwrap().path)
                    .join("review.md"),
            ] {
                let bytes = fs::read(&path).unwrap();
                fs::write(&path, b"changed evidence").unwrap();
                assert!(!covered(attempt, &current), "{}", path.display());
                fs::write(path, bytes).unwrap();
            }
        }
        for change in ["config", "extractor", "order", "applicability"] {
            let mut manifest: crate::tester::CanonicalRunManifest =
                serde_json::from_slice(&manifest_bytes).unwrap();
            match change {
                "config" => manifest.config_digest.push_str("changed"),
                "extractor" => manifest.extractor_digest = "changed".into(),
                "order" => manifest.commands.swap(0, 1),
                "applicability" => {
                    manifest.applicability[1]
                        .comparison_commits
                        .insert("baseline".into(), "changed".into());
                }
                _ => unreachable!(),
            }
            fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
            assert!(!covered(attempt, &current), "{change}");
        }
        fs::write(&manifest_path, &manifest_bytes).unwrap();
        let mut third = current.clone();
        third.commands.push(current.commands[1].clone());
        assert!(
            !covered(attempt, &third),
            "a third command needs its own proof"
        );
        let mut leaking = current.clone();
        leaking.commands[1].process_settlement = Some(crate::tester::ProcessSettlementEvidence {
            settlement_directory: "settlement".into(),
            leaks: vec![crate::tester::ProcessLeak {
                pid: 123,
                kind: "descendant".into(),
                settlement_directory: "settlement".into(),
            }],
        });
        assert!(!covered(attempt, &leaking));
        fs::write(&assessment.results_path, b"changed source").unwrap();
        assert!(!covered(attempt, &current));
        fs::write(&assessment.results_path, &source_bytes).unwrap();
        assert!(covered(attempt, &current));

        // Keep the evidence and Review attachment consistent while expiring one proof.
        let (live_root, mut live) =
            crate::recovery_contracts::verification_tests::fixture_with_lifetime(false, Some(60));
        accept_check(live_root.path(), &mut live, 0);
        accept_check(live_root.path(), &mut live, 1);
        let assessment = assess_canonical_checks(live_root.path(), &live.attempts[0]).unwrap();
        let candidate = assessment.checks[0].candidate_commit.clone();
        let attempt = &mut live.attempts[0];
        let recovery = &mut attempt.evidence_recoveries[0];
        let proof_path = live_root.path().join(&recovery.attachment.snapshot_path);
        let mut proof: serde_json::Value =
            serde_json::from_slice(&fs::read(&proof_path).unwrap()).unwrap();
        proof["run_at"] = "2000-01-01T00:00:00Z".into();
        let bytes = serde_json::to_vec(&proof).unwrap();
        fs::write(proof_path, &bytes).unwrap();
        recovery.attachment.digest = crate::verification_policy::digest(&bytes);
        attempt
            .tasks
            .iter_mut()
            .find(|task| Some(&task.id) == recovery.targets[0].review_task_id.as_ref())
            .unwrap()
            .evidence_review_context
            .as_mut()
            .unwrap()
            .attachment = recovery.attachment.clone();
        let error = crate::verification_assessment::validate_required_check_acceptance_at(
            live_root.path(),
            attempt,
            &assessment.checks[0],
            crate::check_applicability::evaluation_time(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("expired"), "{error}");
        assert!(
            !super::reviewed_host_proof_covers_expertise_only_merge(
                live_root.path(),
                attempt,
                &candidate,
                &assessment.results_path,
                Some(&baseline_path),
            )
            .unwrap()
        );
    }

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
                    None,
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
                None,
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
        assert!(super::reviewed_commands_cover_failures(
            &std::collections::BTreeMap::from([(0, source.clone())]),
            &same,
            None,
        ));

        let new_failure = result(&[
            ("terminal::control", "cargo-nextest"),
            ("terminal::new_regression", "cargo-nextest"),
        ]);
        assert!(!super::reviewed_commands_cover_failures(
            &std::collections::BTreeMap::from([(0, source.clone())]),
            &new_failure,
            None,
        ));
        let other_harness = result(&[("terminal::control", "shell-harness")]);
        assert!(!super::reviewed_commands_cover_failures(
            &std::collections::BTreeMap::from([(0, source.clone())]),
            &other_harness,
            None,
        ));
        let mut other_command = same.clone();
        other_command.commands[1].exit_code = 1;
        assert!(!super::reviewed_commands_cover_failures(
            &std::collections::BTreeMap::from([(0, source.clone())]),
            &other_command,
            None,
        ));
        let mut changed_command = same;
        changed_command.commands[0]
            .command
            .push_str(" --skip regression");
        assert!(!super::reviewed_commands_cover_failures(
            &std::collections::BTreeMap::from([(0, source.clone())]),
            &changed_command,
            None,
        ));

        let mut duplicate = source.clone();
        duplicate.commands[1].test_harness = "cargo-nextest".into();
        assert!(!super::reviewed_commands_cover_failures(
            &std::collections::BTreeMap::from([(0, duplicate.clone())]),
            &duplicate,
            None,
        ));
        duplicate.tests[0].id = "command-0:cargo nextest run".into();
        assert!(super::reviewed_commands_cover_failures(
            &std::collections::BTreeMap::from([(0, duplicate.clone())]),
            &duplicate,
            None,
        ));
        let mut missing_command = source.clone();
        missing_command.commands.pop();
        assert!(!super::reviewed_commands_cover_failures(
            &std::collections::BTreeMap::from([(0, source.clone())]),
            &missing_command,
            None,
        ));
    }
}
