use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

use crate::atomic_write::atomic_write;
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
            &inputs.write_coder, &inputs.write_model, &inputs.write_effort,
            &inputs.review_coder, &inputs.review_model, &inputs.review_effort,
            &inputs.behavior_tests_coder, &inputs.behavior_tests_model, &inputs.behavior_tests_effort,
        ].iter().any(|value| value.is_some());
        let configured = inputs.coder_profile.is_some() || inputs.follow_up_mode.is_some() || custom_present;
        if !configured {
            return Ok(None);
        }
        let mode = match inputs.follow_up_mode.as_deref() {
            Some("propose") => FollowUpMode::Propose,
            Some("execute") => FollowUpMode::Execute,
            Some(other) => bail!("unknown --follow-up-mode {other:?}; expected `propose` or `execute`"),
            None => bail!("configured init requires --follow-up-mode"),
        };
        let profile = inputs.coder_profile.as_deref()
            .ok_or_else(|| anyhow::anyhow!("configured init requires --coder-profile"))?;
        let mapping = match profile {
            "codex-balanced" => {
                if custom_present { bail!("--coder-profile codex-balanced does not accept custom role flags"); }
                curated("gpt-5.6-terra")
            }
            "codex-stronger" => {
                if custom_present { bail!("--coder-profile codex-stronger does not accept custom role flags"); }
                curated("gpt-5.6-sol")
            }
            "custom" => {
                let values = [
                    (&inputs.write_coder, "--write-coder"), (&inputs.write_model, "--write-model"), (&inputs.write_effort, "--write-effort"),
                    (&inputs.review_coder, "--review-coder"), (&inputs.review_model, "--review-model"), (&inputs.review_effort, "--review-effort"),
                    (&inputs.behavior_tests_coder, "--behavior-tests-coder"), (&inputs.behavior_tests_model, "--behavior-tests-model"), (&inputs.behavior_tests_effort, "--behavior-tests-effort"),
                ];
                let missing = values.iter().filter_map(|(value, flag)| value.is_none().then_some(*flag)).collect::<Vec<_>>();
                if !missing.is_empty() { bail!("--coder-profile custom requires {}", missing.join(", ")); }
                resolve_coder_mapping(&CoderMappingInputs {
                    write_coder: inputs.write_coder, write_model: inputs.write_model, write_effort: inputs.write_effort,
                    review_coder: inputs.review_coder, review_model: inputs.review_model, review_effort: inputs.review_effort,
                    behavior_tests_coder: inputs.behavior_tests_coder, behavior_tests_model: inputs.behavior_tests_model, behavior_tests_effort: inputs.behavior_tests_effort,
                    global_coder: None,
                })?
            }
            other => bail!("unknown --coder-profile {other:?}; expected codex-balanced, codex-stronger, or custom"),
        };
        Ok(Some(Self { mapping, follow_up_mode: mode }))
    }
}

fn curated(model: &str) -> CoderMapping {
    let pair = CoderModelPair { coder: CoderKind::Codex, model: model.to_string(), effort: Some("medium".to_string()) };
    CoderMapping { write: pair.clone(), review: pair.clone(), behavior_tests: pair }
}

/// Check selected providers without asking any model to generate output.
pub fn preflight_providers(mapping: &CoderMapping) -> Result<()> {
    let mut providers = Vec::new();
    for provider in [mapping.write.coder, mapping.review.coder, mapping.behavior_tests.coder] {
        if !providers.contains(&provider) {
            providers.push(provider);
        }
    }
    let failures = providers.into_iter().filter_map(|provider| provider_ready(provider).err()).map(|error| error.to_string()).collect::<Vec<_>>();
    if failures.is_empty() { Ok(()) } else { bail!("provider preflight failed:\n  {}\nChoose a different profile or retry after fixing the provider.", failures.join("\n  ")) }
}

fn provider_ready(provider: CoderKind) -> Result<()> {
    let program = provider.as_str();
    if provider == CoderKind::Codex {
        let worker = crate::codex_worker::CodexWorkerEnvironment::prepare()
            .map_err(anyhow::Error::new)?;
        return worker.preflight().map_err(anyhow::Error::new);
    }
    let mut command = Command::new(program);
    match provider {
        CoderKind::Claude => { command.args(["auth", "status", "--json"]); }
        CoderKind::Pi => {
            command.arg("--version");
        }
        CoderKind::Codex => unreachable!("Codex returns above"),
    }
    let status = command.status().with_context(|| format!("{program}: command is not installed"))?;
    if status.success() { Ok(()) } else { bail!("{program}: authentication is unavailable") }
}

/// Atomically update only setup-owned configuration leaves and return the saved mapping.
pub fn apply_project_config(root: &Path, mapping: &CoderMapping, mode: FollowUpMode) -> Result<CoderMapping> {
    let path = root.join(".fluent/config.yaml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let mut document = if text.trim().is_empty() { serde_yaml::Value::Mapping(Default::default()) } else { serde_yaml::from_str(&text).context("parse existing project configuration")? };
    let root_map = document.as_mapping_mut().ok_or_else(|| anyhow::anyhow!("project configuration must be a YAML mapping"))?;
    let coders = mapping_at_mut(root_map, "coders")?;
    write_role(coders, "writer", &mapping.write)?;
    write_role(coders, "reviewer", &mapping.review)?;
    write_role(coders, "behavior-tests", &mapping.behavior_tests)?;
    if mode == FollowUpMode::Execute { mapping_at_mut(root_map, "follow-up")?.insert("mode".into(), "execute".into()); }
    let rendered = serde_yaml::to_string(&document).context("serialize project configuration")?;
    atomic_write(&path, rendered.as_bytes()).with_context(|| format!("write {}", path.display()))?;
    Ok(mapping.clone())
}

fn mapping_at_mut<'a>(map: &'a mut serde_yaml::Mapping, key: &str) -> Result<&'a mut serde_yaml::Mapping> {
    let value = map.entry(key.into()).or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));
    value.as_mapping_mut().ok_or_else(|| anyhow::anyhow!("configuration key {key:?} must be a mapping"))
}

fn write_role(coders: &mut serde_yaml::Mapping, name: &str, pair: &CoderModelPair) -> Result<()> {
    let role = mapping_at_mut(coders, name)?;
    role.insert("coder".into(), pair.coder.as_str().into());
    role.insert("model".into(), pair.model.clone().into());
    role.insert("effort".into(), pair.effort.clone().unwrap_or_default().into());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configured_profile_merges_all_roles_without_follow_up_for_propose() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join(".fluent")).unwrap();
        std::fs::write(directory.path().join(".fluent/config.yaml"), "unrelated: retained\n").unwrap();
        let setup = ConfiguredSetup::from_inputs(InitSetupInputs { coder_profile: Some("codex-balanced".into()), follow_up_mode: Some("propose".into()), ..Default::default() }).unwrap().unwrap();
        apply_project_config(directory.path(), &setup.mapping, setup.follow_up_mode).unwrap();
        let value: serde_yaml::Value = serde_yaml::from_str(&std::fs::read_to_string(directory.path().join(".fluent/config.yaml")).unwrap()).unwrap();
        assert_eq!(value["unrelated"], "retained");
        assert!(value.get("follow-up").is_none());
        for role in ["writer", "reviewer", "behavior-tests"] { assert_eq!(value["coders"][role]["model"], "gpt-5.6-terra"); }
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
