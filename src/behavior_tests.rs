use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BehaviorTestsResults {
    pub ran_at: String,
    pub candidate_commit: String,
    pub commands_run: Vec<String>,
    pub summary: Summary,
    pub behaviors: Vec<BehaviorResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_failure: Option<CommandFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BehaviorResult {
    pub anchor: String,
    pub test_refs: Vec<String>,
    pub status: BehaviorStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_excerpt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub untestable_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BehaviorStatus {
    Pass,
    Fail,
    Untestable,
    MissingTestRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub behaviors_total: u64,
    pub tested_passing: u64,
    pub tested_failing: u64,
    pub untestable: u64,
    pub missing_test_ref: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandFailure {
    pub command: String,
    pub error_excerpt: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn behavior_tests_results_round_trip() {
        let results = BehaviorTestsResults {
            ran_at: "2026-06-13T07:00:00+00:00".to_string(),
            candidate_commit: "abc123".to_string(),
            commands_run: vec!["cargo nextest run --test binary".to_string()],
            summary: Summary {
                behaviors_total: 4,
                tested_passing: 2,
                tested_failing: 1,
                untestable: 1,
                missing_test_ref: 0,
            },
            behaviors: vec![
                BehaviorResult {
                    anchor: "version-reporting".to_string(),
                    test_refs: vec![
                        "tests/binary.rs (version_prints_package_version_and_commit)".to_string(),
                    ],
                    status: BehaviorStatus::Pass,
                    duration_ms: Some(120),
                    failure_excerpt: None,
                    untestable_reason: None,
                },
                BehaviorResult {
                    anchor: "credential-injection".to_string(),
                    test_refs: vec![],
                    status: BehaviorStatus::Untestable,
                    duration_ms: None,
                    failure_excerpt: None,
                    untestable_reason: Some(
                        "Requires external credential store not available in test".to_string(),
                    ),
                },
            ],
            command_failure: None,
        };

        let json = serde_json::to_string_pretty(&results).unwrap();
        let parsed: BehaviorTestsResults = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, results);
    }

    #[test]
    fn behavior_status_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&BehaviorStatus::Pass).unwrap(),
            r#""pass""#
        );
        assert_eq!(
            serde_json::to_string(&BehaviorStatus::Fail).unwrap(),
            r#""fail""#
        );
        assert_eq!(
            serde_json::to_string(&BehaviorStatus::Untestable).unwrap(),
            r#""untestable""#
        );
        assert_eq!(
            serde_json::to_string(&BehaviorStatus::MissingTestRef).unwrap(),
            r#""missing_test_ref""#
        );
    }

    #[test]
    fn command_failure_results_round_trip() {
        let results = BehaviorTestsResults {
            ran_at: "2026-06-13T07:00:00+00:00".to_string(),
            candidate_commit: "abc123".to_string(),
            commands_run: vec!["cargo nextest run --test binary".to_string()],
            summary: Summary {
                behaviors_total: 0,
                tested_passing: 0,
                tested_failing: 0,
                untestable: 0,
                missing_test_ref: 0,
            },
            behaviors: Vec::new(),
            command_failure: Some(CommandFailure {
                command: "cargo nextest run --test binary".to_string(),
                error_excerpt: "error[E0433]: failed to resolve".to_string(),
            }),
        };

        let json = serde_json::to_string_pretty(&results).unwrap();
        let parsed: BehaviorTestsResults = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, results);
        assert!(parsed.command_failure.is_some());
    }
}
