use anyhow::{Result, bail};

use crate::coder::CoderKind;
use crate::config::FollowUpMode;
use crate::work_model::{CoderMapping, CoderMappingInputs, CoderModelPair, resolve_coder_mapping};

pub const FLUENT_GITIGNORE: &str = "\
# Fluent working state: everything here is ignored by default.
# Durable content is re-included below.
/*
!/.gitignore
!/expertise/
!/hooks/
!/Dockerfile
!/tester.yaml
!/extract-tester-results
";

const DURABLE_FLUENT_PATHS: &[&str] = &[
    ".fluent/expertise/INDEX.md",
    ".fluent/hooks/prepare-pre-review",
    ".fluent/hooks/check-pre-merge",
    ".fluent/hooks/fix-pre-merge",
    ".fluent/Dockerfile",
    ".fluent/tester.yaml",
    ".fluent/extract-tester-results",
];

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
        let role_inputs = CoderMappingInputs {
            write_coder: inputs.write_coder,
            write_model: inputs.write_model,
            write_effort: inputs.write_effort,
            review_coder: inputs.review_coder,
            review_model: inputs.review_model,
            review_effort: inputs.review_effort,
            global_coder: None,
        };
        let mapping = match profile {
            "codex-balanced" => curated_codex("gpt-5.6-terra").overlay_run_inputs(&role_inputs)?,
            "codex-stronger" => curated_codex("gpt-5.6-sol").overlay_run_inputs(&role_inputs)?,
            "claude-balanced" => curated_claude().overlay_run_inputs(&role_inputs)?,
            "codex-write-claude-review" => CoderMapping {
                write: codex_pair("gpt-5.6-terra"),
                review: claude_pair(),
            }
            .overlay_run_inputs(&role_inputs)?,
            "claude-write-codex-review" => CoderMapping {
                write: claude_pair(),
                review: codex_pair("gpt-5.6-terra"),
            }
            .overlay_run_inputs(&role_inputs)?,
            "opencode-default" => CoderMapping {
                write: opencode_pair(),
                review: opencode_pair(),
            }
            .overlay_run_inputs(&role_inputs)?,
            "custom" => {
                let values = [
                    (&role_inputs.write_coder, "--write-coder"),
                    (&role_inputs.write_model, "--write-model"),
                    (&role_inputs.write_effort, "--write-effort"),
                    (&role_inputs.review_coder, "--review-coder"),
                    (&role_inputs.review_model, "--review-model"),
                    (&role_inputs.review_effort, "--review-effort"),
                ];
                let missing = values
                    .iter()
                    .filter_map(|(value, flag)| value.is_none().then_some(*flag))
                    .collect::<Vec<_>>();
                if !missing.is_empty() {
                    bail!("--coder-profile custom requires {}", missing.join(", "));
                }
                resolve_coder_mapping(&role_inputs)?
            }
            other => bail!(
                "unknown --coder-profile {other:?}; expected codex-balanced, codex-stronger, claude-balanced, opencode-default, codex-write-claude-review, claude-write-codex-review, or custom"
            ),
        };
        Ok(Some(Self {
            mapping,
            follow_up_mode: mode,
        }))
    }
}

fn codex_pair(model: &str) -> CoderModelPair {
    CoderModelPair {
        coder: CoderKind::Codex,
        model: model.to_string(),
        effort: Some("medium".to_string()),
    }
}

fn claude_pair() -> CoderModelPair {
    CoderModelPair {
        coder: CoderKind::Claude,
        model: String::new(),
        effort: Some("medium".to_string()),
    }
}

/// An empty model lets OpenCode use the provider-qualified default from its own
/// configuration instead of a model name Fluent would have to guess.
fn opencode_pair() -> CoderModelPair {
    CoderModelPair {
        coder: CoderKind::Opencode,
        model: String::new(),
        effort: None,
    }
}

fn curated_codex(model: &str) -> CoderMapping {
    let pair = codex_pair(model);
    CoderMapping {
        write: pair.clone(),
        review: pair,
    }
}

fn curated_claude() -> CoderMapping {
    let pair = claude_pair();
    CoderMapping {
        write: pair.clone(),
        review: pair,
    }
}

/// Check selected providers and require a live response from each selected
/// Claude model before configured initialization mutates the project.
pub fn preflight_providers(mapping: &CoderMapping) -> Result<()> {
    let mut providers = Vec::new();
    for provider in [mapping.write.coder, mapping.review.coder] {
        if !providers.contains(&provider) {
            providers.push(provider);
        }
    }
    let mut failures = Vec::new();
    let mut claude_authenticated = false;
    for provider in providers {
        match crate::provider_readiness::ProviderReadiness::prepare(provider) {
            Ok(_) => {
                if provider == CoderKind::Claude {
                    claude_authenticated = true;
                }
            }
            Err(error) => failures.push(error.to_string()),
        }
    }
    if claude_authenticated && failures.is_empty() {
        let mut models = Vec::new();
        for pair in [&mapping.write, &mapping.review] {
            if pair.coder == CoderKind::Claude && !models.contains(&pair.model) {
                models.push(pair.model.clone());
            }
        }
        for model in models {
            if let Err(error) =
                crate::provider_readiness::ProviderReadiness::verify_claude_inference(&model)
            {
                failures.push(error.to_string());
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        bail!(
            "provider preflight failed:\n  {}\nChoose a different profile or retry after fixing the provider.",
            failures.join("\n  ")
        )
    }
}

/// Reject configured setup when Git would hide required project-owned state.
pub fn preflight_durable_paths(cwd: &std::path::Path) -> Result<()> {
    let ignored = crate::git::ignored_paths(cwd, DURABLE_FLUENT_PATHS)?;
    if ignored.is_empty() {
        return Ok(());
    }
    let conflicts = ignored
        .iter()
        .map(|rule| {
            format!(
                "  {}: {}:{} pattern {:?}",
                rule.path, rule.source, rule.line, rule.pattern
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    bail!(
        "FLUENT_INIT_DURABLE_PATH_IGNORED: Git ignore rules hide required Fluent project state:\n{conflicts}\n\
Remove or narrow the listed rules before retrying this command. Let Fluent's\n\
generated .fluent/.gitignore ignore runtime state such as .fluent/work/, or\n\
use equally narrow runtime-only exclusions instead of ignoring all of .fluent/."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured_profile(profile: &str) -> ConfiguredSetup {
        ConfiguredSetup::from_inputs(InitSetupInputs {
            coder_profile: Some(profile.into()),
            follow_up_mode: Some("propose".into()),
            ..Default::default()
        })
        .unwrap()
        .unwrap()
    }

    #[test]
    fn configured_profile_merges_both_roles_without_follow_up_for_propose() {
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
        for role in ["writer", "reviewer"] {
            assert_eq!(value["coders"][role]["model"], "gpt-5.6-terra");
        }
    }

    #[test]
    fn curated_profiles_resolve_exact_writer_and_reviewer_mappings() {
        let codex_balanced = configured_profile("codex-balanced");
        assert_eq!(codex_balanced.mapping.write.coder, CoderKind::Codex);
        assert_eq!(codex_balanced.mapping.write.model, "gpt-5.6-terra");
        assert_eq!(codex_balanced.mapping.review, codex_balanced.mapping.write);

        let codex_stronger = configured_profile("codex-stronger");
        assert_eq!(codex_stronger.mapping.write.model, "gpt-5.6-sol");
        assert_eq!(codex_stronger.mapping.review, codex_stronger.mapping.write);

        let claude_balanced = configured_profile("claude-balanced");
        assert_eq!(claude_balanced.mapping.write.coder, CoderKind::Claude);
        assert!(claude_balanced.mapping.write.model.is_empty());
        assert_eq!(
            claude_balanced.mapping.review,
            claude_balanced.mapping.write
        );

        let codex_writer = configured_profile("codex-write-claude-review");
        assert_eq!(codex_writer.mapping.write.coder, CoderKind::Codex);
        assert_eq!(codex_writer.mapping.write.model, "gpt-5.6-terra");
        assert_eq!(codex_writer.mapping.review.coder, CoderKind::Claude);
        assert!(codex_writer.mapping.review.model.is_empty());

        let claude_writer = configured_profile("claude-write-codex-review");
        assert_eq!(claude_writer.mapping.write.coder, CoderKind::Claude);
        assert!(claude_writer.mapping.write.model.is_empty());
        assert_eq!(claude_writer.mapping.review.coder, CoderKind::Codex);
        assert_eq!(claude_writer.mapping.review.model, "gpt-5.6-terra");
    }

    #[test]
    fn configured_profile_opencode_default_sets_both_roles() {
        let opencode = configured_profile("opencode-default");
        assert_eq!(opencode.mapping.write.coder, CoderKind::Opencode);
        assert!(opencode.mapping.write.model.is_empty());
        assert_eq!(opencode.mapping.write.effort, None);
        assert_eq!(opencode.mapping.review, opencode.mapping.write);
    }

    #[test]
    fn configured_profile_lists_opencode_default() {
        let error = ConfiguredSetup::from_inputs(InitSetupInputs {
            coder_profile: Some("unknown-profile".into()),
            follow_up_mode: Some("propose".into()),
            ..Default::default()
        })
        .err()
        .expect("unknown profile must be rejected")
        .to_string();
        assert!(
            error.contains("opencode-default"),
            "profile list should include opencode-default: {error}"
        );
    }

    #[test]
    fn configured_setup_rejects_incomplete_custom_flags() {
        let incomplete = ConfiguredSetup::from_inputs(InitSetupInputs {
            coder_profile: Some("custom".into()),
            follow_up_mode: Some("propose".into()),
            write_coder: Some("codex".into()),
            ..Default::default()
        });
        assert!(incomplete.is_err());
    }

    #[test]
    fn complete_custom_profile_resolves_both_roles() {
        let setup = ConfiguredSetup::from_inputs(InitSetupInputs {
            coder_profile: Some("custom".into()),
            follow_up_mode: Some("propose".into()),
            write_coder: Some("claude".into()),
            write_model: Some("write-model".into()),
            write_effort: Some("high".into()),
            review_coder: Some("codex".into()),
            review_model: Some("review-model".into()),
            review_effort: Some("medium".into()),
        })
        .unwrap()
        .unwrap();
        assert_eq!(setup.mapping.write.coder, CoderKind::Claude);
        assert_eq!(setup.mapping.write.model, "write-model");
        assert_eq!(setup.mapping.write.effort.as_deref(), Some("high"));
        assert_eq!(setup.mapping.review.coder, CoderKind::Codex);
        assert_eq!(setup.mapping.review.model, "review-model");
        assert_eq!(setup.mapping.review.effort.as_deref(), Some("medium"));
    }

    #[test]
    fn curated_profile_accepts_explicit_role_overrides() {
        let overlaid = ConfiguredSetup::from_inputs(InitSetupInputs {
            coder_profile: Some("codex-balanced".into()),
            follow_up_mode: Some("propose".into()),
            write_coder: Some("claude".into()),
            ..Default::default()
        })
        .unwrap()
        .unwrap();
        assert_eq!(overlaid.mapping.write.coder, CoderKind::Claude);
        assert_eq!(overlaid.mapping.write.model, "gpt-5.6-terra");
        assert_eq!(overlaid.mapping.review.coder, CoderKind::Codex);
    }
}
