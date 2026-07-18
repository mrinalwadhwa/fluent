use serde::Deserialize;
use std::path::Path;

use crate::work_model::CoderMappingInputs;

#[derive(Debug, Deserialize, Default)]
struct Config {
    #[serde(default)]
    coders: CoderConfig,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
struct CoderConfig {
    #[serde(default)]
    writer: RoleConfig,
    #[serde(default)]
    reviewer: RoleConfig,
    #[serde(default)]
    behavior_tests: RoleConfig,
}

#[derive(Debug, Deserialize, Default)]
struct RoleConfig {
    #[serde(default)]
    coder: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
}

fn user_config_path() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(Path::new(&home).join(".config/fluent/config.yaml"))
}

fn project_config_path(project_root: &Path) -> std::path::PathBuf {
    project_root.join(".fluent/config.yaml")
}

fn read_config(path: &Path) -> Option<Config> {
    let contents = std::fs::read_to_string(path).ok()?;
    serde_yaml::from_str(&contents).ok()
}

fn config_to_inputs(config: &Config) -> CoderMappingInputs {
    CoderMappingInputs {
        write_coder: config.coders.writer.coder.clone(),
        write_model: config.coders.writer.model.clone(),
        review_coder: config.coders.reviewer.coder.clone(),
        review_model: config.coders.reviewer.model.clone(),
        behavior_tests_coder: config.coders.behavior_tests.coder.clone(),
        behavior_tests_model: config.coders.behavior_tests.model.clone(),
        global_coder: None,
        write_effort: config.coders.writer.effort.clone(),
        review_effort: config.coders.reviewer.effort.clone(),
        behavior_tests_effort: config.coders.behavior_tests.effort.clone(),
    }
}

/// Read user (`~/.config/fluent/config.yaml`) then project
/// (`.fluent/config.yaml`) config, layering project over user.
pub fn from_config(project_root: &Path) -> CoderMappingInputs {
    let user = user_config_path().and_then(|p| read_config(&p));
    let project = read_config(&project_config_path(project_root));

    let base = user.as_ref().map(config_to_inputs).unwrap_or_default();

    match project {
        Some(ref proj) => {
            let proj_inputs = config_to_inputs(proj);
            base.merge(proj_inputs)
        }
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_config() {
        let yaml = r#"
coders:
  writer:
    coder: claude
    model: claude-sonnet-4-6
    effort: high
  reviewer:
    coder: codex
    model: o3
  behavior-tests:
    coder: claude
"#;
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.coders.writer.coder.as_deref(), Some("claude"));
        assert_eq!(
            config.coders.writer.model.as_deref(),
            Some("claude-sonnet-4-6")
        );
        assert_eq!(config.coders.writer.effort.as_deref(), Some("high"));
        assert_eq!(config.coders.reviewer.coder.as_deref(), Some("codex"));
        assert_eq!(config.coders.reviewer.model.as_deref(), Some("o3"));
        assert_eq!(
            config.coders.behavior_tests.coder.as_deref(),
            Some("claude")
        );
        assert!(config.coders.behavior_tests.model.is_none());
    }

    #[test]
    fn parses_partial_config() {
        let yaml = r#"
coders:
  writer:
    model: claude-sonnet-4-6
"#;
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert!(config.coders.writer.coder.is_none());
        assert_eq!(
            config.coders.writer.model.as_deref(),
            Some("claude-sonnet-4-6")
        );
        assert!(config.coders.reviewer.coder.is_none());
    }

    #[test]
    fn parses_empty_config() {
        let yaml = "{}";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert!(config.coders.writer.coder.is_none());
        assert!(config.coders.writer.model.is_none());
    }

    #[test]
    fn config_to_inputs_maps_all_fields() {
        let yaml = r#"
coders:
  writer:
    coder: claude
    model: claude-sonnet-4-6
    effort: high
  reviewer:
    coder: codex
    model: o3
  behavior-tests:
    coder: claude
    model: claude-opus-4-6
    effort: low
"#;
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        let inputs = config_to_inputs(&config);
        assert_eq!(inputs.write_coder.as_deref(), Some("claude"));
        assert_eq!(inputs.write_model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(inputs.write_effort.as_deref(), Some("high"));
        assert_eq!(inputs.review_coder.as_deref(), Some("codex"));
        assert_eq!(inputs.review_model.as_deref(), Some("o3"));
        assert_eq!(inputs.behavior_tests_coder.as_deref(), Some("claude"));
        assert_eq!(
            inputs.behavior_tests_model.as_deref(),
            Some("claude-opus-4-6")
        );
        assert_eq!(inputs.behavior_tests_effort.as_deref(), Some("low"));
    }

    #[test]
    fn from_config_reads_project_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join(".fluent");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.yaml"),
            "coders:\n  writer:\n    model: custom-model\n",
        )
        .unwrap();

        let inputs = from_config(dir.path());
        assert_eq!(inputs.write_model.as_deref(), Some("custom-model"));
    }

    #[test]
    fn from_config_returns_empty_when_no_config() {
        let dir = tempfile::tempdir().unwrap();
        let inputs = from_config(dir.path());
        assert!(inputs.write_coder.is_none());
        assert!(inputs.write_model.is_none());
    }

    #[test]
    fn project_config_overrides_user_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join(".fluent");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.yaml"),
            "coders:\n  writer:\n    model: project-model\n",
        )
        .unwrap();

        let inputs = from_config(dir.path());
        assert_eq!(inputs.write_model.as_deref(), Some("project-model"));
    }

    #[test]
    fn config_round_trips_through_inputs() {
        let yaml = r#"
coders:
  writer:
    coder: codex
    model: o4-mini
  reviewer:
    coder: claude
"#;
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        let inputs = config_to_inputs(&config);
        assert_eq!(inputs.write_coder.as_deref(), Some("codex"));
        assert_eq!(inputs.write_model.as_deref(), Some("o4-mini"));
        assert_eq!(inputs.review_coder.as_deref(), Some("claude"));
        assert!(inputs.review_model.is_none());
        assert!(inputs.behavior_tests_coder.is_none());
    }
}
