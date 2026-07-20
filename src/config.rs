use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::work_model::CoderMappingInputs;

// -------------------------------------------------------------------------
// Layered follow-up and scheduler policy
// -------------------------------------------------------------------------

/// Built-in follow-up mode used when no configuration supplies one.
pub const DEFAULT_FOLLOW_UP_MODE: FollowUpMode = FollowUpMode::Propose;
/// Built-in autonomous descendant limit per lineage.
pub const DEFAULT_DESCENDANT_LIMIT: u32 = 10;
/// Built-in automatic queue priority for learner corrections.
pub const DEFAULT_LEARNER_PRIORITY: i64 = 100;
/// Built-in automatic queue priority for post-merge corrections. Kept above the
/// learner priority so post-merge corrections outrank learner corrections.
pub const DEFAULT_POST_MERGE_PRIORITY: i64 = 200;
/// Built-in number of scheduler-managed Work Items allowed to run concurrently.
pub const DEFAULT_LOCAL_SCHEDULER_CONCURRENCY: u32 = 4;

/// Which configuration layer supplied a resolved leaf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSource {
    Project,
    User,
    Default,
}

impl ConfigSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::User => "user",
            Self::Default => "default",
        }
    }
}

impl fmt::Display for ConfigSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Follow-up mode applied when a corrective learner Observation is promoted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowUpMode {
    Propose,
    Execute,
}

impl FollowUpMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Propose => "propose",
            Self::Execute => "execute",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "propose" => Some(Self::Propose),
            "execute" => Some(Self::Execute),
            _ => None,
        }
    }
}

impl fmt::Display for FollowUpMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A resolved configuration value paired with the layer that supplied it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedLeaf<T> {
    pub value: T,
    pub source: ConfigSource,
}

/// Follow-up policy resolved leaf by leaf from project, then user, then
/// built-in defaults, with a canonical digest over the resolved values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFollowUpPolicy {
    pub mode: ResolvedLeaf<FollowUpMode>,
    pub descendant_limit: ResolvedLeaf<u32>,
    pub learner_priority: ResolvedLeaf<i64>,
    pub post_merge_priority: ResolvedLeaf<i64>,
    pub digest: String,
}

/// Local scheduler settings resolved from project, then user, then built-in
/// defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSchedulerConfig {
    pub max_local_concurrency: ResolvedLeaf<u32>,
}

/// A configured follow-up or scheduler value that could not be parsed or
/// validated. Names the configuration path and the affected key so the caller
/// can fail closed instead of silently substituting a lower-precedence value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowUpConfigError {
    pub path: PathBuf,
    pub key: String,
    pub detail: String,
}

impl fmt::Display for FollowUpConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid configuration at {}: key {:?}: {}",
            self.path.display(),
            self.key,
            self.detail
        )
    }
}

impl std::error::Error for FollowUpConfigError {}

/// One configuration layer: its precedence source, on-disk path, and parsed
/// document (absent when the file does not exist).
struct ConfigLayer {
    source: ConfigSource,
    path: PathBuf,
    doc: Option<serde_yaml::Value>,
}

fn load_layer(source: ConfigSource, path: PathBuf) -> Result<ConfigLayer, FollowUpConfigError> {
    let doc =
        match std::fs::read_to_string(&path) {
            Ok(text) => Some(serde_yaml::from_str::<serde_yaml::Value>(&text).map_err(
                |source| FollowUpConfigError {
                    path: path.clone(),
                    key: "<document>".to_string(),
                    detail: format!("could not parse YAML: {source}"),
                },
            )?),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                return Err(FollowUpConfigError {
                    path: path.clone(),
                    key: "<document>".to_string(),
                    detail: format!("could not read file: {err}"),
                });
            }
        };
    Ok(ConfigLayer { source, path, doc })
}

/// Look up a nested key path in a document. A key present but null is treated as
/// absent so a lower-precedence layer or the built-in default applies.
fn lookup<'a>(doc: &'a serde_yaml::Value, keys: &[&str]) -> Option<&'a serde_yaml::Value> {
    let mut current = doc;
    for key in keys {
        current = current.get(*key)?;
    }
    if current.is_null() {
        None
    } else {
        Some(current)
    }
}

/// Resolve one leaf across the ordered layers. The first layer that supplies the
/// key owns it: its value is validated by `convert`, and a failure there fails
/// closed rather than falling through to a lower-precedence layer.
fn resolve_leaf<T>(
    layers: &[ConfigLayer],
    keys: &[&str],
    default: T,
    convert: impl Fn(&serde_yaml::Value) -> Result<T, String>,
) -> Result<ResolvedLeaf<T>, FollowUpConfigError> {
    for layer in layers {
        let Some(doc) = layer.doc.as_ref() else {
            continue;
        };
        let Some(raw) = lookup(doc, keys) else {
            continue;
        };
        let value = convert(raw).map_err(|detail| FollowUpConfigError {
            path: layer.path.clone(),
            key: keys.join("."),
            detail,
        })?;
        return Ok(ResolvedLeaf {
            value,
            source: layer.source,
        });
    }
    Ok(ResolvedLeaf {
        value: default,
        source: ConfigSource::Default,
    })
}

fn convert_mode(raw: &serde_yaml::Value) -> Result<FollowUpMode, String> {
    let text = raw
        .as_str()
        .ok_or_else(|| "expected a string (`propose` or `execute`)".to_string())?;
    FollowUpMode::parse(text)
        .ok_or_else(|| format!("unknown follow-up mode {text:?}; expected `propose` or `execute`"))
}

fn convert_count(raw: &serde_yaml::Value) -> Result<u32, String> {
    let value = raw
        .as_i64()
        .ok_or_else(|| "expected an integer".to_string())?;
    if value < 0 {
        return Err(format!("expected a non-negative integer, found {value}"));
    }
    u32::try_from(value).map_err(|_| format!("value {value} is out of range"))
}

fn convert_positive_count(raw: &serde_yaml::Value) -> Result<u32, String> {
    let value = convert_count(raw)?;
    if value == 0 {
        return Err("expected a positive integer, found 0".to_string());
    }
    Ok(value)
}

fn convert_priority(raw: &serde_yaml::Value) -> Result<i64, String> {
    raw.as_i64()
        .ok_or_else(|| "expected an integer".to_string())
}

fn digest_hex(canonical: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Resolve the follow-up policy for a project, layering project over user over
/// built-in defaults. Malformed configured values fail closed.
pub fn resolve_follow_up_policy(
    project_root: &Path,
) -> Result<ResolvedFollowUpPolicy, FollowUpConfigError> {
    resolve_follow_up_policy_from(
        &project_config_path(project_root),
        user_config_path().as_deref(),
    )
}

fn resolve_follow_up_policy_from(
    project_path: &Path,
    user_path: Option<&Path>,
) -> Result<ResolvedFollowUpPolicy, FollowUpConfigError> {
    let layers = load_policy_layers(project_path, user_path)?;

    let mode = resolve_leaf(
        &layers,
        &["follow-up", "mode"],
        DEFAULT_FOLLOW_UP_MODE,
        convert_mode,
    )?;
    let descendant_limit = resolve_leaf(
        &layers,
        &["follow-up", "descendant-limit"],
        DEFAULT_DESCENDANT_LIMIT,
        convert_count,
    )?;
    let learner_priority = resolve_leaf(
        &layers,
        &["follow-up", "learner-priority"],
        DEFAULT_LEARNER_PRIORITY,
        convert_priority,
    )?;
    let post_merge_priority = resolve_leaf(
        &layers,
        &["follow-up", "post-merge-priority"],
        DEFAULT_POST_MERGE_PRIORITY,
        convert_priority,
    )?;

    let canonical = format!(
        "mode={};descendant-limit={};learner-priority={};post-merge-priority={}",
        mode.value.as_str(),
        descendant_limit.value,
        learner_priority.value,
        post_merge_priority.value,
    );

    Ok(ResolvedFollowUpPolicy {
        mode,
        descendant_limit,
        learner_priority,
        post_merge_priority,
        digest: digest_hex(&canonical),
    })
}

/// Resolve the local scheduler configuration for a project.
pub fn resolve_scheduler_config(
    project_root: &Path,
) -> Result<ResolvedSchedulerConfig, FollowUpConfigError> {
    resolve_scheduler_config_from(
        &project_config_path(project_root),
        user_config_path().as_deref(),
    )
}

fn resolve_scheduler_config_from(
    project_path: &Path,
    user_path: Option<&Path>,
) -> Result<ResolvedSchedulerConfig, FollowUpConfigError> {
    let layers = load_policy_layers(project_path, user_path)?;
    let max_local_concurrency = resolve_leaf(
        &layers,
        &["scheduler", "local-concurrency"],
        DEFAULT_LOCAL_SCHEDULER_CONCURRENCY,
        convert_positive_count,
    )?;
    Ok(ResolvedSchedulerConfig {
        max_local_concurrency,
    })
}

fn load_policy_layers(
    project_path: &Path,
    user_path: Option<&Path>,
) -> Result<Vec<ConfigLayer>, FollowUpConfigError> {
    let mut layers = vec![load_layer(
        ConfigSource::Project,
        project_path.to_path_buf(),
    )?];
    if let Some(user_path) = user_path {
        layers.push(load_layer(ConfigSource::User, user_path.to_path_buf())?);
    }
    Ok(layers)
}

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

    fn write_yaml(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn followup_policy_uses_project_user_default_precedence() {
        let dir = tempfile::tempdir().unwrap();
        // Project fixes the mode; user fixes the descendant limit; priorities
        // fall through to the built-in defaults.
        let project = write_yaml(dir.path(), "project.yaml", "follow-up:\n  mode: execute\n");
        let user = write_yaml(
            dir.path(),
            "user.yaml",
            "follow-up:\n  mode: propose\n  descendant-limit: 3\n",
        );

        let policy = resolve_follow_up_policy_from(&project, Some(&user)).unwrap();

        assert_eq!(policy.mode.value, FollowUpMode::Execute);
        assert_eq!(policy.mode.source, ConfigSource::Project);
        assert_eq!(policy.descendant_limit.value, 3);
        assert_eq!(policy.descendant_limit.source, ConfigSource::User);
        assert_eq!(policy.learner_priority.value, DEFAULT_LEARNER_PRIORITY);
        assert_eq!(policy.learner_priority.source, ConfigSource::Default);
        assert_eq!(policy.post_merge_priority.source, ConfigSource::Default);
        assert!(!policy.digest.is_empty());
    }

    #[test]
    fn followup_mode_defaults_to_propose() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("absent.yaml");

        let policy = resolve_follow_up_policy_from(&project, None).unwrap();

        assert_eq!(policy.mode.value, FollowUpMode::Propose);
        assert_eq!(policy.mode.source, ConfigSource::Default);
    }

    #[test]
    fn local_scheduler_concurrency_defaults_to_four() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("absent.yaml");

        let scheduler = resolve_scheduler_config_from(&project, None).unwrap();

        assert_eq!(scheduler.max_local_concurrency.value, 4);
        assert_eq!(
            scheduler.max_local_concurrency.source,
            ConfigSource::Default
        );
    }

    #[test]
    fn lineage_limit_defaults_to_ten() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("absent.yaml");

        let policy = resolve_follow_up_policy_from(&project, None).unwrap();

        assert_eq!(policy.descendant_limit.value, 10);
        assert_eq!(policy.descendant_limit.source, ConfigSource::Default);
    }

    #[test]
    fn default_post_merge_priority_exceeds_learner_priority() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("absent.yaml");

        let policy = resolve_follow_up_policy_from(&project, None).unwrap();

        assert!(
            policy.post_merge_priority.value > policy.learner_priority.value,
            "post-merge default {} must outrank learner default {}",
            policy.post_merge_priority.value,
            policy.learner_priority.value
        );
    }

    #[test]
    fn invalid_followup_policy_fails_closed_with_source_and_key() {
        let dir = tempfile::tempdir().unwrap();
        // A lower-precedence user layer supplies a valid mode; the project layer
        // supplies a malformed one. Resolution must fail closed and report the
        // project path and the affected key rather than silently substituting
        // the user value.
        let project = write_yaml(
            dir.path(),
            "project.yaml",
            "follow-up:\n  mode: sometimes\n",
        );
        let user = write_yaml(dir.path(), "user.yaml", "follow-up:\n  mode: propose\n");

        let error = resolve_follow_up_policy_from(&project, Some(&user)).unwrap_err();

        assert_eq!(error.path, project);
        assert!(
            error.key.contains("mode"),
            "expected key to name the mode leaf, got {:?}",
            error.key
        );
    }

    #[test]
    fn invalid_scheduler_concurrency_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let project = write_yaml(
            dir.path(),
            "project.yaml",
            "scheduler:\n  local-concurrency: 0\n",
        );

        let error = resolve_scheduler_config_from(&project, None).unwrap_err();

        assert_eq!(error.path, project);
        assert!(error.key.contains("local-concurrency"));
    }

    #[test]
    fn followup_policy_digest_is_stable_across_layers() {
        let dir = tempfile::tempdir().unwrap();
        // The same resolved values produce the same digest regardless of which
        // layer supplied each leaf.
        let all_project = write_yaml(
            dir.path(),
            "all.yaml",
            "follow-up:\n  mode: execute\n  descendant-limit: 5\n",
        );
        let split_user = write_yaml(
            dir.path(),
            "split.yaml",
            "follow-up:\n  descendant-limit: 5\n",
        );
        let split_project = write_yaml(
            dir.path(),
            "split-project.yaml",
            "follow-up:\n  mode: execute\n",
        );

        let from_project = resolve_follow_up_policy_from(&all_project, None).unwrap();
        let from_layers = resolve_follow_up_policy_from(&split_project, Some(&split_user)).unwrap();

        assert_eq!(from_project.digest, from_layers.digest);
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
