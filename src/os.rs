use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

use crate::coder::CoderKind;
use crate::content::ContentResolver;

/// Rendered sandbox profile that cleans up on drop.
pub struct SandboxProfile {
    _temp_file: NamedTempFile,
    pub path: PathBuf,
}

/// Render a Seatbelt sandbox profile with placeholder substitution.
///
/// Concatenates common.sb + claude-code.sb and substitutes:
/// - `_HOME_` -> user's home directory
/// - `_SANDBOX_ROOT_` -> the sandbox file-access root
pub fn render_profile(
    resolver: &ContentResolver,
    home: &str,
    sandbox_root: &str,
) -> Result<SandboxProfile> {
    render_profile_for_roots(resolver, home, &[PathBuf::from(sandbox_root)])
}

/// Render a Seatbelt sandbox profile with multiple writable roots.
pub fn render_profile_for_roots(
    resolver: &ContentResolver,
    home: &str,
    sandbox_roots: &[PathBuf],
) -> Result<SandboxProfile> {
    if sandbox_roots.is_empty() {
        bail!("At least one sandbox root is required");
    }
    let common = resolver
        .resolve_content("sandbox/common.sb")
        .context("Common sandbox profile not found")?;
    let specific = resolver
        .resolve_content("sandbox/claude-code.sb")
        .context("Claude Code sandbox profile not found")?;

    let combined = format!("{common}\n{specific}");
    let root_rules = render_root_rules(sandbox_roots);
    let primary_root = sandbox_roots[0].to_string_lossy();
    let combined = if combined.contains("_SANDBOX_ROOT_RULES_") {
        combined.replace("_SANDBOX_ROOT_RULES_", &root_rules)
    } else {
        combined.replace(
            "(allow file-read*  (subpath \"_SANDBOX_ROOT_\"))\n(allow file-write* (subpath \"_SANDBOX_ROOT_\"))",
            &root_rules,
        )
    };
    let rendered = combined
        .replace("_HOME_", home)
        .replace("_SANDBOX_ROOT_", &primary_root);

    let temp_file = NamedTempFile::with_prefix("factory-sandbox-")?;
    std::fs::write(temp_file.path(), &rendered)?;

    let path = temp_file.path().to_path_buf();
    Ok(SandboxProfile {
        _temp_file: temp_file,
        path,
    })
}

fn render_root_rules(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|root| {
            let root = sbpl_string(root);
            format!("(allow file-read*  (subpath {root}))\n(allow file-write* (subpath {root}))")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn sbpl_string(path: &Path) -> String {
    let escaped = path
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Check that sandbox prerequisites are available.
pub fn check_prerequisites() -> Result<()> {
    check_prerequisites_for(CoderKind::Claude)
}

/// Check that sandbox prerequisites and the selected coder are available.
pub fn check_prerequisites_for(coder_kind: CoderKind) -> Result<()> {
    if coder_kind == CoderKind::Claude && !command_exists("sandbox-exec") {
        bail!("sandbox-exec not found (macOS only)");
    }
    let command = coder_kind.as_str();
    if !command_exists(command) {
        bail!("{command} not found in PATH");
    }
    Ok(())
}

fn command_exists(name: &str) -> bool {
    std::process::Command::new("which")
        .arg(name)
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_profile_substitution() {
        let resolver = ContentResolver::new(None);
        let profile = render_profile(&resolver, "/Users/test", "/Users/test/project").unwrap();

        let content = std::fs::read_to_string(&profile.path).unwrap();
        assert!(content.contains("/Users/test"));
        assert!(content.contains("/Users/test/project"));
        assert!(!content.contains("_HOME_"));
        assert!(!content.contains("_SANDBOX_ROOT_"));
        assert!(!content.contains("_SANDBOX_ROOT_RULES_"));
    }

    #[test]
    fn test_render_profile_contains_seatbelt_version() {
        let resolver = ContentResolver::new(None);
        let profile = render_profile(&resolver, "/Users/test", "/Users/test/project").unwrap();

        let content = std::fs::read_to_string(&profile.path).unwrap();
        assert!(content.contains("(version 1)"));
        assert!(content.contains("(deny default)"));
    }

    #[test]
    fn test_render_profile_contains_multiple_roots() {
        let resolver = ContentResolver::new(None);
        let profile = render_profile_for_roots(
            &resolver,
            "/Users/test",
            &[
                PathBuf::from("/Users/test/workspace/run"),
                PathBuf::from("/Users/test/workspace/main/.git"),
            ],
        )
        .unwrap();

        let content = std::fs::read_to_string(&profile.path).unwrap();
        assert!(content.contains("/Users/test/workspace/run"), "{content}");
        assert!(
            content.contains("/Users/test/workspace/main/.git"),
            "{content}"
        );
    }
}
