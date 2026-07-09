use anyhow::{Context, Result, bail};
use std::env;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

use crate::coder::CoderKind;
use crate::content::ContentResolver;

/// Rendered sandbox profile that cleans up on drop.
pub struct SandboxProfile {
    _temp_file: NamedTempFile,
    pub path: PathBuf,
}

/// Render a Claude Seatbelt sandbox profile with placeholder substitution.
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

/// Render a Claude Seatbelt sandbox profile with multiple writable roots.
pub fn render_profile_for_roots(
    resolver: &ContentResolver,
    home: &str,
    sandbox_roots: &[PathBuf],
) -> Result<SandboxProfile> {
    render_profile_for_roots_for_coder(resolver, home, sandbox_roots, CoderKind::Claude)
}

/// Render a Seatbelt sandbox profile with common rules plus the coder layer.
pub fn render_profile_for_roots_for_coder(
    resolver: &ContentResolver,
    home: &str,
    sandbox_roots: &[PathBuf],
    coder_kind: CoderKind,
) -> Result<SandboxProfile> {
    render_profile_for_access_for_coder(resolver, home, sandbox_roots, &[], coder_kind)
}

/// Render a Seatbelt sandbox profile with writable and read-only roots.
pub fn render_profile_for_access_for_coder(
    resolver: &ContentResolver,
    home: &str,
    writable_roots: &[PathBuf],
    readable_roots: &[PathBuf],
    coder_kind: CoderKind,
) -> Result<SandboxProfile> {
    render_profile_for_access(
        resolver,
        home,
        writable_roots,
        readable_roots,
        Some(coder_kind),
    )
}

/// Render a Seatbelt sandbox profile with common rules only (no tool overlay).
pub fn render_profile_common_only(
    resolver: &ContentResolver,
    home: &str,
    writable_roots: &[PathBuf],
    readable_roots: &[PathBuf],
) -> Result<SandboxProfile> {
    render_profile_for_access(resolver, home, writable_roots, readable_roots, None)
}

fn render_profile_for_access(
    resolver: &ContentResolver,
    home: &str,
    writable_roots: &[PathBuf],
    readable_roots: &[PathBuf],
    coder_kind: Option<CoderKind>,
) -> Result<SandboxProfile> {
    if writable_roots.is_empty() {
        bail!("At least one writable sandbox root is required");
    }
    let common = resolver
        .resolve_content("sandbox/common.sb")
        .context("Common sandbox profile not found")?;

    let combined = if let Some(kind) = coder_kind {
        let specific_path = sandbox_profile_path(kind);
        let specific = resolver
            .resolve_content(specific_path)
            .with_context(|| format!("Sandbox profile {specific_path} not found"))?;
        format!("{common}\n{specific}")
    } else {
        common
    };

    let root_rules = render_root_rules(writable_roots, readable_roots);
    let primary_root = writable_roots[0].to_string_lossy();
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

    let temp_file = NamedTempFile::with_prefix("fluent-sandbox-")?;
    std::fs::write(temp_file.path(), &rendered)?;

    let path = temp_file.path().to_path_buf();
    Ok(SandboxProfile {
        _temp_file: temp_file,
        path,
    })
}

fn sandbox_profile_path(coder_kind: CoderKind) -> &'static str {
    match coder_kind {
        CoderKind::Claude => "sandbox/claude-code.sb",
        CoderKind::Codex => "sandbox/codex.sb",
        CoderKind::Pi => "sandbox/pi.sb",
    }
}

fn render_root_rules(writable_roots: &[PathBuf], readable_roots: &[PathBuf]) -> String {
    let writable_rules = writable_roots
        .iter()
        .map(|root| {
            let root = sbpl_string(root);
            format!("(allow file-read*  (subpath {root}))\n(allow file-write* (subpath {root}))")
        })
        .collect::<Vec<_>>();
    let readable_rules = readable_roots
        .iter()
        .map(|root| {
            let root = sbpl_string(root);
            format!("(allow file-read*  (subpath {root}))")
        })
        .collect::<Vec<_>>();
    writable_rules
        .into_iter()
        .chain(readable_rules)
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
    if !command_exists("sandbox-exec") {
        bail!("sandbox-exec not found (macOS only)");
    }
    check_coder_prerequisite(coder_kind)?;
    Ok(())
}

/// Check that the selected coder is available.
pub fn check_coder_prerequisite(coder_kind: CoderKind) -> Result<()> {
    let command = coder_kind.as_str();
    if !command_exists(command) {
        bail!("{command} not found in PATH");
    }
    Ok(())
}

fn command_exists(name: &str) -> bool {
    env::var_os("PATH")
        .map(|paths| {
            env::split_paths(&paths).any(|dir| {
                let candidate = dir.join(name);
                candidate.is_file() && is_executable(&candidate)
            })
        })
        .unwrap_or(false)
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.exists()
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

    #[test]
    fn test_render_profile_contains_read_only_roots() {
        let resolver = ContentResolver::new(None);
        let writable_root = PathBuf::from("/Users/test/workspace/artifacts");
        let readable_root = PathBuf::from("/Users/test/workspace/candidate");
        let profile = render_profile_for_access_for_coder(
            &resolver,
            "/Users/test",
            std::slice::from_ref(&writable_root),
            std::slice::from_ref(&readable_root),
            CoderKind::Claude,
        )
        .unwrap();

        let content = std::fs::read_to_string(&profile.path).unwrap();
        assert!(
            content.contains("(allow file-write* (subpath \"/Users/test/workspace/artifacts\"))"),
            "{content}"
        );
        assert!(
            content.contains("(allow file-read*  (subpath \"/Users/test/workspace/candidate\"))"),
            "{content}"
        );
        assert!(
            !content.contains("(allow file-write* (subpath \"/Users/test/workspace/candidate\"))"),
            "{content}"
        );
    }

    #[test]
    fn test_render_profile_uses_codex_specific_layer() {
        let resolver = ContentResolver::new(None);
        let profile = render_profile_for_roots_for_coder(
            &resolver,
            "/Users/test",
            &[PathBuf::from("/Users/test/workspace/run")],
            CoderKind::Codex,
        )
        .unwrap();

        let content = std::fs::read_to_string(&profile.path).unwrap();
        assert!(content.contains("Codex CLI -- profile-specific Seatbelt rules"));
        assert!(content.contains("/Users/test/.codex"));
        assert!(!content.contains("Claude Code CLI -- profile-specific Seatbelt rules"));
    }

    #[test]
    fn rendered_profile_with_no_overlay_uses_common_only() {
        let resolver = ContentResolver::new(None);
        let profile = render_profile_common_only(
            &resolver,
            "/Users/test",
            &[PathBuf::from("/Users/test/workspace")],
            &[],
        )
        .unwrap();

        let content = std::fs::read_to_string(&profile.path).unwrap();
        assert!(content.contains("(version 1)"));
        assert!(content.contains("(deny default)"));
        assert!(
            !content.contains("Claude Code CLI -- profile-specific"),
            "common-only profile should not contain Claude-specific overlay"
        );
        assert!(
            !content.contains("Codex CLI -- profile-specific"),
            "common-only profile should not contain Codex-specific overlay"
        );
        assert!(
            !content.contains("Pi CLI -- profile-specific"),
            "common-only profile should not contain Pi-specific overlay"
        );
    }

    #[test]
    fn rendered_profile_grants_workspace_and_artifact_writable() {
        let resolver = ContentResolver::new(None);
        let workspace = PathBuf::from("/Users/test/workspace/candidate");
        let artifact = PathBuf::from("/Users/test/.fluent/artifacts/tester");
        let profile = render_profile_common_only(
            &resolver,
            "/Users/test",
            &[workspace.clone(), artifact.clone()],
            &[],
        )
        .unwrap();

        let content = std::fs::read_to_string(&profile.path).unwrap();
        assert!(
            content.contains("(allow file-write* (subpath \"/Users/test/workspace/candidate\"))"),
            "workspace should be writable: {content}"
        );
        assert!(
            content.contains(
                "(allow file-write* (subpath \"/Users/test/.fluent/artifacts/tester\"))"
            ),
            "artifact dir should be writable: {content}"
        );
    }

    #[test]
    fn rendered_profile_grants_private_ancestor_metadata() {
        // git's realpath() lstats the bare /private node while resolving a
        // /private/var/folders temp path; without metadata access it fails with
        // "Invalid path '/private'". Tests that git-init in system temp rely on this.
        let resolver = ContentResolver::new(None);
        let profile = render_profile_common_only(
            &resolver,
            "/Users/test",
            &[PathBuf::from("/Users/test/workspace")],
            &[],
        )
        .unwrap();

        let content = std::fs::read_to_string(&profile.path).unwrap();
        assert!(
            content.contains("(allow file-read-metadata (literal \"/private\"))"),
            "common profile should grant metadata on /private so realpath can \
             traverse into system temp: {content}"
        );
    }

    #[test]
    fn test_render_profile_uses_claude_specific_layer() {
        let resolver = ContentResolver::new(None);
        let profile = render_profile_for_roots_for_coder(
            &resolver,
            "/Users/test",
            &[PathBuf::from("/Users/test/workspace/run")],
            CoderKind::Claude,
        )
        .unwrap();

        let content = std::fs::read_to_string(&profile.path).unwrap();
        assert!(content.contains("Claude Code CLI -- profile-specific Seatbelt rules"));
        assert!(!content.contains("Codex CLI -- profile-specific Seatbelt rules"));
    }
}
