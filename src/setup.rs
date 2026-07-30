use anyhow::{Result, bail};

use crate::coder::CoderKind;
use crate::config::FollowUpMode;
use crate::work_model::{CoderMapping, CoderMappingInputs, CoderModelPair, resolve_coder_mapping};

/// CLI-only values for deterministic configured first-time setup.
#[derive(Debug, Default)]
pub struct InitSetupInputs {
    pub coder_profile: Option<String>,
    pub follow_up_mode: Option<String>,
    pub write_coder: Option<String>,
    pub write_model: Option<String>,
    pub write_effort: Option<String>,
    pub review_coder: Option<String>,
    pub review_model: Option<String>,
    pub review_effort: Option<String>,
    pub behavior_tests_coder: Option<String>,
    pub behavior_tests_model: Option<String>,
    pub behavior_tests_effort: Option<String>,
}

pub struct ConfiguredSetup {
    pub mapping: CoderMapping,
    pub follow_up_mode: FollowUpMode,
}

impl ConfiguredSetup {
    /// Return None for legacy bare init, or validate a complete configured setup.
    pub fn from_inputs(inputs: InitSetupInputs) -> Result<Option<Self>> {
        let custom_present = [
            &inputs.write_coder,
            &inputs.write_model,
            &inputs.write_effort,
            &inputs.review_coder,
            &inputs.review_model,
            &inputs.review_effort,
            &inputs.behavior_tests_coder,
            &inputs.behavior_tests_model,
            &inputs.behavior_tests_effort,
        ]
        .iter()
        .any(|value| value.is_some());
        let configured =
            inputs.coder_profile.is_some() || inputs.follow_up_mode.is_some() || custom_present;
        if !configured {
            return Ok(None);
        }
        let mode = match inputs.follow_up_mode.as_deref() {
            Some("propose") => FollowUpMode::Propose,
            Some("execute") => FollowUpMode::Execute,
            Some(other) => {
                bail!("unknown --follow-up-mode {other:?}; expected `propose` or `execute`")
            }
            None => bail!("configured init requires --follow-up-mode"),
        };
        let profile = inputs
            .coder_profile
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("configured init requires --coder-profile"))?;
        let mapping = match profile {
            "codex-balanced" => {
                if custom_present {
                    bail!("--coder-profile codex-balanced does not accept custom role flags");
                }
                curated("gpt-5.6-terra")
            }
            "codex-stronger" => {
                if custom_present {
                    bail!("--coder-profile codex-stronger does not accept custom role flags");
                }
                curated("gpt-5.6-sol")
            }
            "custom" => {
                let values = [
                    (&inputs.write_coder, "--write-coder"),
                    (&inputs.write_model, "--write-model"),
                    (&inputs.write_effort, "--write-effort"),
                    (&inputs.review_coder, "--review-coder"),
                    (&inputs.review_model, "--review-model"),
                    (&inputs.review_effort, "--review-effort"),
                    (&inputs.behavior_tests_coder, "--behavior-tests-coder"),
                    (&inputs.behavior_tests_model, "--behavior-tests-model"),
                    (&inputs.behavior_tests_effort, "--behavior-tests-effort"),
                ];
                let missing = values
                    .iter()
                    .filter_map(|(value, flag)| value.is_none().then_some(*flag))
                    .collect::<Vec<_>>();
                if !missing.is_empty() {
                    bail!("--coder-profile custom requires {}", missing.join(", "));
                }
                resolve_coder_mapping(&CoderMappingInputs {
                    write_coder: inputs.write_coder,
                    write_model: inputs.write_model,
                    write_effort: inputs.write_effort,
                    review_coder: inputs.review_coder,
                    review_model: inputs.review_model,
                    review_effort: inputs.review_effort,
                    behavior_tests_coder: inputs.behavior_tests_coder,
                    behavior_tests_model: inputs.behavior_tests_model,
                    behavior_tests_effort: inputs.behavior_tests_effort,
                    global_coder: None,
                })?
            }
            other => bail!(
                "unknown --coder-profile {other:?}; expected codex-balanced, codex-stronger, or custom"
            ),
        };
        Ok(Some(Self {
            mapping,
            follow_up_mode: mode,
        }))
    }
}

fn curated(model: &str) -> CoderMapping {
    let pair = CoderModelPair {
        coder: CoderKind::Codex,
        model: model.to_string(),
        effort: Some("medium".to_string()),
    };
    CoderMapping {
        write: pair.clone(),
        review: pair.clone(),
        behavior_tests: pair,
    }
}

/// Check selected providers without asking any model to generate output.
pub fn preflight_providers(mapping: &CoderMapping) -> Result<()> {
    let mut providers = Vec::new();
    for provider in [
        mapping.write.coder,
        mapping.review.coder,
        mapping.behavior_tests.coder,
    ] {
        if !providers.contains(&provider) {
            providers.push(provider);
        }
    }
    let failures = providers
        .into_iter()
        .filter_map(|provider| {
            crate::provider_readiness::ProviderReadiness::prepare(provider).err()
        })
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if failures.is_empty() {
        Ok(())
    } else {
        bail!(
            "provider preflight failed:\n  {}\nChoose a different profile or retry after fixing the provider.",
            failures.join("\n  ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configured_profile_merges_all_roles_without_follow_up_for_propose() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join(".fluent")).unwrap();
        std::fs::write(
            directory.path().join(".fluent/config.yaml"),
            "unrelated: retained\n",
        )
        .unwrap();
        let setup = ConfiguredSetup::from_inputs(InitSetupInputs {
            coder_profile: Some("codex-balanced".into()),
            follow_up_mode: Some("propose".into()),
            ..Default::default()
        })
        .unwrap()
        .unwrap();
        crate::config::apply_project_coder_profile(
            directory.path(),
            &setup.mapping,
            setup.follow_up_mode,
        )
        .unwrap();
        let value: serde_yaml::Value = serde_yaml::from_str(
            &std::fs::read_to_string(directory.path().join(".fluent/config.yaml")).unwrap(),
        )
        .unwrap();
        assert_eq!(value["unrelated"], "retained");
        assert!(value.get("follow-up").is_none());
        for role in ["writer", "reviewer", "behavior-tests"] {
            assert_eq!(value["coders"][role]["model"], "gpt-5.6-terra");
        }
    }

    #[test]
    fn configured_setup_rejects_incomplete_and_conflicting_flags() {
        let incomplete = ConfiguredSetup::from_inputs(InitSetupInputs {
            coder_profile: Some("custom".into()),
            follow_up_mode: Some("propose".into()),
            write_coder: Some("codex".into()),
            ..Default::default()
        });
        assert!(incomplete.is_err());
        let conflicting = ConfiguredSetup::from_inputs(InitSetupInputs {
            coder_profile: Some("codex-balanced".into()),
            follow_up_mode: Some("propose".into()),
            write_coder: Some("codex".into()),
            ..Default::default()
        });
        assert!(conflicting.is_err());
    }
}
