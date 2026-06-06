use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactoryConfig {
    pub checks: Vec<ProjectCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectCheck {
    pub name: String,
    pub command: String,
    pub fix_command: Option<String>,
    pub autofix: bool,
    pub run_before_land: bool,
}

pub fn load_factory_config(project_root: &Path) -> Result<Option<FactoryConfig>> {
    let path = project_root.join(".factory/config.toml");
    if !path.exists() {
        return Ok(None);
    }

    let content =
        fs::read_to_string(&path).with_context(|| format!("Failed to read {}", path.display()))?;
    parse_factory_config(&content)
        .with_context(|| format!("Failed to parse {}", path.display()))
        .map(Some)
}

fn parse_factory_config(content: &str) -> Result<FactoryConfig> {
    let raw: RawFactoryConfig = toml::from_str(content)?;
    let mut checks = Vec::new();

    for (name, check) in raw.checks {
        let Some(command) = check.command else {
            bail!("Check '{}' must define a command", name);
        };
        if command.trim().is_empty() {
            bail!("Check '{}' must define a non-empty command", name);
        }

        checks.push(ProjectCheck {
            name,
            command,
            fix_command: check.fix_command,
            autofix: check.autofix,
            run_before_land: check.run_before_land,
        });
    }

    Ok(FactoryConfig { checks })
}

#[derive(Debug, Deserialize)]
struct RawFactoryConfig {
    #[serde(default)]
    checks: BTreeMap<String, RawProjectCheck>,
}

#[derive(Debug, Deserialize)]
struct RawProjectCheck {
    command: Option<String>,
    fix_command: Option<String>,
    #[serde(default)]
    autofix: bool,
    #[serde(default)]
    run_before_land: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn missing_config_loads_none() {
        let tmp = TempDir::new().unwrap();

        assert_eq!(load_factory_config(tmp.path()).unwrap(), None);
    }

    #[test]
    fn parses_project_checks() {
        let config = parse_factory_config(
            r#"
[checks."format"]
command = "cargo fmt --all -- --check" # inline comments are TOML
fix_command = "cargo fmt --all"
autofix = true
run_before_land = true
"#,
        )
        .unwrap();

        assert_eq!(
            config.checks,
            vec![ProjectCheck {
                name: "format".into(),
                command: "cargo fmt --all -- --check".into(),
                fix_command: Some("cargo fmt --all".into()),
                autofix: true,
                run_before_land: true,
            }]
        );
    }

    #[test]
    fn rejects_check_without_command() {
        let err = parse_factory_config(
            r#"
[checks.format]
run_before_land = true
"#,
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("Check 'format' must define a command"),
            "{err:#}"
        );
    }

    #[test]
    fn rejects_check_with_empty_command() {
        let err = parse_factory_config(
            r#"
[checks.format]
command = "   "
run_before_land = true
"#,
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("Check 'format' must define a non-empty command"),
            "{err:#}"
        );
    }
}
