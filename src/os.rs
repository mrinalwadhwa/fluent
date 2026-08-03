use anyhow::{Context, Result, bail};
use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

use crate::coder::CoderKind;
use crate::content::ContentResolver;
use crate::linux_sandbox;

/// Rendered sandbox profile that cleans up on drop.
pub struct SandboxProfile {
    _temp_file: NamedTempFile,
    pub path: PathBuf,
}

/// Kernel mechanism that confines a coder or Tester command on this host.
///
/// Seatbelt and Landlock differ enough that the rendered profile is not a
/// common format with two writers: Seatbelt takes ordered allow/deny rules
/// where the first match wins, Landlock takes an allowlist whose rules only
/// ever union. Each backend renders its own file and names its own launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxBackend {
    Seatbelt,
    Landlock,
}

/// Backend this host confines with, unless `FLUENT_SANDBOX_BACKEND` names
/// `seatbelt` or `landlock`.
///
/// The override exists for tests that stand a mock launcher on `PATH` to
/// exercise the orchestration around the sandbox: Seatbelt's launcher is a
/// binary and can be replaced, while Landlock is a kernel facility and cannot.
/// It selects which backend renders and launches, never whether confinement
/// happens — naming a backend the host cannot run makes the launch fail. Any
/// other value leaves the host default in place.
pub fn backend() -> SandboxBackend {
    match env::var("FLUENT_SANDBOX_BACKEND").as_deref() {
        Ok("seatbelt") => SandboxBackend::Seatbelt,
        Ok("landlock") => SandboxBackend::Landlock,
        _ if cfg!(target_os = "macos") => SandboxBackend::Seatbelt,
        _ => SandboxBackend::Landlock,
    }
}

/// Program that applies a rendered profile and then runs the confined command.
///
/// `trusted` asks for a path that a `PATH` entry cannot shadow. Landlock has no
/// launcher binary of its own, so Fluent re-executes itself; `current_exe`
/// resolves through `/proc/self/exe` and is already unshadowable, which is why
/// both cases return the same path there.
pub fn sandbox_launcher(trusted: bool) -> OsString {
    match backend() {
        SandboxBackend::Seatbelt if trusted => OsString::from("/usr/bin/sandbox-exec"),
        SandboxBackend::Seatbelt => OsString::from("sandbox-exec"),
        SandboxBackend::Landlock => env::current_exe()
            .map(PathBuf::into_os_string)
            .unwrap_or_else(|_| OsString::from("fluent")),
    }
}

/// Arguments that sit between the launcher and the command being confined.
pub fn sandbox_launcher_args(profile: &str) -> Vec<String> {
    match backend() {
        SandboxBackend::Seatbelt => vec!["-f".to_string(), profile.to_string()],
        SandboxBackend::Landlock => vec![
            "sandbox-run".to_string(),
            "--policy".to_string(),
            profile.to_string(),
            "--".to_string(),
        ],
    }
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
    render_profile_for_access_for_coder_with_codex_home(
        resolver,
        home,
        writable_roots,
        readable_roots,
        coder_kind,
        None,
    )
}

/// Render a profile for an autonomous coder, optionally replacing Codex's
/// interactive home grant with its private worker home.
pub fn render_profile_for_access_for_coder_with_codex_home(
    resolver: &ContentResolver,
    home: &str,
    writable_roots: &[PathBuf],
    readable_roots: &[PathBuf],
    coder_kind: CoderKind,
    codex_home: Option<&Path>,
) -> Result<SandboxProfile> {
    render_profile_for_access(
        resolver,
        home,
        writable_roots,
        readable_roots,
        &[],
        Some(coder_kind),
        codex_home,
    )
}

/// Render a coder profile with explicit write denials that override broad
/// common temporary-directory grants.
pub fn render_profile_for_access_for_coder_with_denied_writes(
    resolver: &ContentResolver,
    home: &str,
    writable_roots: &[PathBuf],
    readable_roots: &[PathBuf],
    denied_write_roots: &[PathBuf],
    coder_kind: CoderKind,
) -> Result<SandboxProfile> {
    render_profile_for_access_for_coder_with_denied_writes_and_codex_home(
        resolver,
        home,
        writable_roots,
        readable_roots,
        denied_write_roots,
        coder_kind,
        None,
    )
}

/// Render a confined coder profile and, for Codex, grant only the supplied
/// worker home rather than the interactive user's source home.
pub fn render_profile_for_access_for_coder_with_denied_writes_and_codex_home(
    resolver: &ContentResolver,
    home: &str,
    writable_roots: &[PathBuf],
    readable_roots: &[PathBuf],
    denied_write_roots: &[PathBuf],
    coder_kind: CoderKind,
    codex_home: Option<&Path>,
) -> Result<SandboxProfile> {
    let profile = render_profile_for_access(
        resolver,
        home,
        writable_roots,
        readable_roots,
        denied_write_roots,
        Some(coder_kind),
        codex_home,
    )?;
    if backend() == SandboxBackend::Seatbelt {
        let content = std::fs::read_to_string(&profile.path)?
            .replace(
                "(allow file-write* (subpath \"/private/var/folders\"))",
                "; handoff-only profiles do not grant the shared macOS temp tree",
            )
            .replace(
                "(allow file-write* (subpath \"/private/tmp\"))",
                "; handoff-only profiles do not grant shared /private/tmp",
            );
        std::fs::write(&profile.path, content)?;
    }
    Ok(profile)
}

/// Render a Seatbelt sandbox profile with common rules only (no tool overlay).
pub fn render_profile_common_only(
    resolver: &ContentResolver,
    home: &str,
    writable_roots: &[PathBuf],
    readable_roots: &[PathBuf],
) -> Result<SandboxProfile> {
    render_profile_for_access(
        resolver,
        home,
        writable_roots,
        readable_roots,
        &[],
        None,
        None,
    )
}

fn render_profile_for_access(
    resolver: &ContentResolver,
    home: &str,
    writable_roots: &[PathBuf],
    readable_roots: &[PathBuf],
    denied_write_roots: &[PathBuf],
    coder_kind: Option<CoderKind>,
    codex_home: Option<&Path>,
) -> Result<SandboxProfile> {
    match backend() {
        SandboxBackend::Seatbelt => render_seatbelt_profile(
            resolver,
            home,
            writable_roots,
            readable_roots,
            denied_write_roots,
            coder_kind,
            codex_home,
        ),
        SandboxBackend::Landlock => render_landlock_policy(
            home,
            writable_roots,
            readable_roots,
            denied_write_roots,
            coder_kind,
            codex_home,
        ),
    }
}

/// Render a Landlock policy and write it where the launcher will read it.
///
/// A handoff-only profile — one that names denied write roots — also loses the
/// shared temp trees, matching what the Seatbelt renderer strips from
/// `common.sb`: a writable `/tmp` is a channel out of the confinement the
/// denials exist to create.
fn render_landlock_policy(
    home: &str,
    writable_roots: &[PathBuf],
    readable_roots: &[PathBuf],
    denied_write_roots: &[PathBuf],
    coder_kind: Option<CoderKind>,
    codex_home: Option<&Path>,
) -> Result<SandboxProfile> {
    let policy = linux_sandbox::render(&linux_sandbox::PolicyRequest {
        home: Path::new(home),
        writable_roots,
        readable_roots,
        denied_write_roots,
        coder_kind,
        codex_home,
        grant_shared_temp: denied_write_roots.is_empty(),
    })?;

    let temp_file = NamedTempFile::with_prefix("fluent-sandbox-")?;
    std::fs::write(temp_file.path(), linux_sandbox::serialize(&policy)?)?;
    let path = temp_file.path().to_path_buf();
    Ok(SandboxProfile {
        _temp_file: temp_file,
        path,
    })
}

fn render_seatbelt_profile(
    resolver: &ContentResolver,
    home: &str,
    writable_roots: &[PathBuf],
    readable_roots: &[PathBuf],
    denied_write_roots: &[PathBuf],
    coder_kind: Option<CoderKind>,
    codex_home: Option<&Path>,
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
    let mut deny_rules = denied_write_roots
        .iter()
        .map(|root| format!("(deny file-write* (subpath {}))", sbpl_string(root)))
        .collect::<Vec<_>>();
    // Autonomous Codex workers use a staged home.  Deny the interactive source
    // explicitly before common.sb's broad home read grants so the profile cannot
    // reach hooks, configuration, sessions, or the original credentials.
    if let Some(worker_home) = codex_home {
        let source_home = crate::codex_worker::effective_source_home();
        if worker_home != source_home {
            let source = sbpl_string(&source_home);
            deny_rules.push(format!("(deny file-read* (subpath {source}))"));
            deny_rules.push(format!("(deny file-write* (subpath {source}))"));
        }
    }
    let deny_rules = deny_rules.join("\n");
    let combined = if deny_rules.is_empty() {
        combined
    } else {
        combined.replacen(
            "(deny default)",
            &format!("(deny default)\n{deny_rules}"),
            1,
        )
    };
    // `_CODEX_HOME_` contains `_HOME_`; replace the more specific token first.
    let rendered = combined
        .replace(
            "_CODEX_HOME_",
            &codex_home
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("{home}/.codex")),
        )
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

/// Report whether a rendered profile grants `target` as a writable root.
///
/// Tests assert what the renderer emitted for a named root, not whether the
/// path is reachable by some broader system grant — a workspace under `/tmp` is
/// writable on both backends, which is why handoff profiles drop the shared
/// temp trees. The two backends spell the same grant differently, so the
/// comparison is on the root, not on the syntax.
#[cfg(test)]
pub(crate) fn profile_grants_write(profile_text: &str, target: &Path) -> bool {
    profile_grants(profile_text, target, true)
}

/// Report whether a rendered profile grants `target` as a readable root.
#[cfg(test)]
pub(crate) fn profile_grants_read(profile_text: &str, target: &Path) -> bool {
    profile_grants(profile_text, target, false)
}

#[cfg(test)]
fn profile_grants(profile_text: &str, target: &Path, writable: bool) -> bool {
    match backend() {
        SandboxBackend::Seatbelt => {
            let line = if writable {
                format!(
                    "(allow file-write* (subpath \"{}\"))",
                    target.to_string_lossy()
                )
            } else {
                format!(
                    "(allow file-read*  (subpath \"{}\"))",
                    target.to_string_lossy()
                )
            };
            profile_text.contains(&line)
        }
        SandboxBackend::Landlock => {
            let policy: linux_sandbox::Policy =
                serde_json::from_str(profile_text).expect("a rendered Landlock policy");
            policy.rules.iter().any(|rule| {
                rule.path == target
                    && match rule.access {
                        linux_sandbox::Access::ReadWrite => true,
                        linux_sandbox::Access::Read => !writable,
                        linux_sandbox::Access::List => false,
                    }
            })
        }
    }
}

/// Shared temporary trees a coder may write. Handoff-only profiles withhold
/// them so a confined coder cannot pass files around the confinement.
#[cfg(test)]
pub(crate) fn shared_temp_roots() -> Vec<PathBuf> {
    match backend() {
        SandboxBackend::Seatbelt => vec![
            PathBuf::from("/private/var/folders"),
            PathBuf::from("/private/tmp"),
        ],
        SandboxBackend::Landlock => linux_sandbox::SYSTEM_WRITE
            .iter()
            .map(PathBuf::from)
            .collect(),
    }
}

/// Check that sandbox prerequisites are available.
pub fn check_prerequisites() -> Result<()> {
    check_prerequisites_for(CoderKind::Claude)
}

/// Check that sandbox prerequisites and the selected coder are available.
pub fn check_prerequisites_for(coder_kind: CoderKind) -> Result<()> {
    match backend() {
        SandboxBackend::Seatbelt => {
            if !command_exists("sandbox-exec") {
                bail!("sandbox-exec not found; Fluent confines coders with Seatbelt on macOS");
            }
        }
        SandboxBackend::Landlock => {
            if !linux_sandbox::is_available() {
                bail!(
                    "this kernel offers no Landlock; Fluent confines coders with it on Linux, \
                     which needs Linux 5.13 or newer with the LSM enabled"
                );
            }
        }
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

    /// The public renderers dispatch on the host, so these drive the Seatbelt
    /// writer directly and keep asserting SBPL on a Linux builder.
    fn seatbelt(
        home: &str,
        writable: &[PathBuf],
        readable: &[PathBuf],
        coder: Option<CoderKind>,
        codex_home: Option<&Path>,
    ) -> SandboxProfile {
        render_seatbelt_profile(
            &ContentResolver::new(None),
            home,
            writable,
            readable,
            &[],
            coder,
            codex_home,
        )
        .unwrap()
    }

    #[test]
    fn test_render_profile_substitution() {
        let profile = seatbelt("/Users/test", &[PathBuf::from("/Users/test/project")], &[], Some(CoderKind::Claude), None);

        let content = std::fs::read_to_string(&profile.path).unwrap();
        assert!(content.contains("/Users/test"));
        assert!(content.contains("/Users/test/project"));
        assert!(!content.contains("_HOME_"));
        assert!(!content.contains("_SANDBOX_ROOT_"));
        assert!(!content.contains("_SANDBOX_ROOT_RULES_"));
    }

    #[test]
    fn test_render_profile_contains_seatbelt_version() {
        let profile = seatbelt("/Users/test", &[PathBuf::from("/Users/test/project")], &[], Some(CoderKind::Claude), None);

        let content = std::fs::read_to_string(&profile.path).unwrap();
        assert!(content.contains("(version 1)"));
        assert!(content.contains("(deny default)"));
    }

    #[test]
    fn test_render_profile_contains_multiple_roots() {
        let profile = seatbelt("/Users/test", &[
                PathBuf::from("/Users/test/workspace/run"),
                PathBuf::from("/Users/test/workspace/main/.git"),
            ], &[], Some(CoderKind::Claude), None);

        let content = std::fs::read_to_string(&profile.path).unwrap();
        assert!(content.contains("/Users/test/workspace/run"), "{content}");
        assert!(
            content.contains("/Users/test/workspace/main/.git"),
            "{content}"
        );
    }

    #[test]
    fn test_render_profile_contains_read_only_roots() {
        let writable_root = PathBuf::from("/Users/test/workspace/artifacts");
        let readable_root = PathBuf::from("/Users/test/workspace/candidate");
        let profile = seatbelt("/Users/test", std::slice::from_ref(&writable_root), std::slice::from_ref(&readable_root), Some(CoderKind::Claude), None);

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
        let profile = seatbelt("/Users/test", &[PathBuf::from("/Users/test/workspace/run")], &[], Some(CoderKind::Codex), None);

        let content = std::fs::read_to_string(&profile.path).unwrap();
        assert!(content.contains("Codex CLI -- profile-specific Seatbelt rules"));
        assert!(content.contains("/Users/test/.codex"));
        assert!(!content.contains("Claude Code CLI -- profile-specific Seatbelt rules"));
    }

    #[test]
    fn autonomous_codex_profile_grants_only_worker_home() {
        let worker_home = PathBuf::from("/private/tmp/fluent-codex-worker");
        let profile = seatbelt("/Users/test", &[
                PathBuf::from("/Users/test/workspace/run"),
                worker_home.clone(),
            ], &[], Some(CoderKind::Codex), Some(&worker_home));

        let content = std::fs::read_to_string(&profile.path).unwrap();
        assert!(
            content.contains("/private/tmp/fluent-codex-worker"),
            "{content}"
        );
        assert!(!content.contains("/Users/test/.codex"), "{content}");
    }

    #[test]
    fn interactive_codex_profile_preserves_source_home_access() {
        let profile = seatbelt("/Users/test", &[PathBuf::from("/Users/test/workspace/run")], &[], Some(CoderKind::Codex), None);

        let content = std::fs::read_to_string(&profile.path).unwrap();
        assert!(content.contains("/Users/test/.codex"), "{content}");
    }

    #[test]
    fn rendered_profile_with_no_overlay_uses_common_only() {
        let profile = seatbelt("/Users/test", &[PathBuf::from("/Users/test/workspace")], &[], None, None);

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
        let workspace = PathBuf::from("/Users/test/workspace/candidate");
        let artifact = PathBuf::from("/Users/test/.fluent/artifacts/tester");
        let profile = seatbelt("/Users/test", &[workspace.clone(), artifact.clone()], &[], None, None);

        let content = std::fs::read_to_string(&profile.path).unwrap();
        assert!(
            content.contains("(allow file-write* (subpath \"/Users/test/workspace/candidate\"))"),
            "workspace should be writable: {content}"
        );
        assert!(
            content
                .contains("(allow file-write* (subpath \"/Users/test/.fluent/artifacts/tester\"))"),
            "artifact dir should be writable: {content}"
        );
    }

    #[test]
    fn rendered_profile_grants_private_ancestor_metadata() {
        // git's realpath() lstats the bare /private node while resolving a
        // /private/var/folders temp path; without metadata access it fails with
        // "Invalid path '/private'". Tests that git-init in system temp rely on this.
        let profile = seatbelt("/Users/test", &[PathBuf::from("/Users/test/workspace")], &[], None, None);

        let content = std::fs::read_to_string(&profile.path).unwrap();
        assert!(
            content.contains("(allow file-read-metadata (literal \"/private\"))"),
            "common profile should grant metadata on /private so realpath can \
             traverse into system temp: {content}"
        );
    }

    #[test]
    fn test_render_profile_uses_claude_specific_layer() {
        let profile = seatbelt("/Users/test", &[PathBuf::from("/Users/test/workspace/run")], &[], Some(CoderKind::Claude), None);

        let content = std::fs::read_to_string(&profile.path).unwrap();
        assert!(content.contains("Claude Code CLI -- profile-specific Seatbelt rules"));
        assert!(!content.contains("Codex CLI -- profile-specific Seatbelt rules"));
    }
}
