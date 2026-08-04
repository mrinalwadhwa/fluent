use anyhow::{Context, Result, bail};
use std::env;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::NamedTempFile;

use crate::coder::CoderKind;
use crate::content::ContentResolver;

/// Rendered sandbox profile that cleans up on drop.
pub struct SandboxProfile {
    _temp_file: NamedTempFile,
    pub path: PathBuf,
}

/// The enclosing host could not apply Fluent's production Seatbelt profile.
///
/// This is distinct from a coder failure: the workload never started, so callers
/// can pause and later retry the same Task without charging its work budget.
#[derive(Debug)]
pub struct HostSandboxPreflightError {
    message: String,
}

impl fmt::Display for HostSandboxPreflightError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "host sandbox preflight failed: {}", self.message)
    }
}

impl std::error::Error for HostSandboxPreflightError {}

impl HostSandboxPreflightError {
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Apply an already-rendered production profile to a harmless command.
///
/// macOS is the only platform where Fluent uses Seatbelt. Keeping this probe
/// adjacent to profile rendering makes it impossible for launch planners to
/// accidentally test a weaker boundary than the one they later hand to a coder.
pub fn preflight_profile(profile: &SandboxProfile) -> Result<()> {
    #[cfg(feature = "test-support")]
    if let Some(result) = test_preflight_result() {
        return result;
    }
    #[cfg(target_os = "macos")]
    {
        preflight_profile_with(profile, |profile_path| {
            Command::new(sandbox_exec_program())
                .arg("-f")
                .arg(profile_path)
                .arg("/usr/bin/true")
                .output()
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = profile;
        Ok(())
    }
}

/// Authenticate through the prepared Codex launcher inside the exact profile
/// that the autonomous role will retain for its model launch.
pub fn preflight_codex_launcher(
    profile: &SandboxProfile,
    worker: &crate::codex_worker::CodexWorkerEnvironment,
) -> Result<()> {
    #[cfg(feature = "test-support")]
    if let Some(result) = test_preflight_result() {
        return result;
    }
    #[cfg(target_os = "macos")]
    {
        let output =
            codex_preflight_command(&profile.path, worker.launcher().executable(), worker.home())
                .output()
                .map_err(|error| HostSandboxPreflightError {
                    message: format!(
                        "could not execute prepared Codex launcher {}: {error}",
                        worker.launcher().executable().display()
                    ),
                })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let detail = if stderr.is_empty() {
                format!("prepared Codex launcher exited with {}", output.status)
            } else {
                stderr
            };
            return Err(HostSandboxPreflightError { message: detail }.into());
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (profile, worker);
        Ok(())
    }
}

fn codex_preflight_command(profile_path: &Path, launcher: &Path, worker_home: &Path) -> Command {
    let mut command = Command::new(sandbox_exec_program());
    command
        .arg("-f")
        .arg(profile_path)
        .arg(launcher)
        .args(["login", "status"])
        // The production profile does not grant the caller's arbitrary current
        // directory. Run this read-only authentication probe from the system
        // temp tree, which every autonomous Codex profile already permits.
        .current_dir(env::temp_dir())
        .env("CODEX_HOME", worker_home);
    command
}

/// Enable a deterministic probe outcome only for test-support binaries.
///
/// Production builds always execute the system-owned Seatbelt launcher. The
/// test runner opts into this narrow seam so integration fixtures can control
/// the host capability without replacing the trusted launcher through `PATH`.
#[cfg(feature = "test-support")]
fn test_preflight_result() -> Option<Result<()>> {
    match env::var("FLUENT_TEST_HOST_SANDBOX_PREFLIGHT")
        .ok()
        .as_deref()
    {
        Some("pass" | "render-pass") => Some(Ok(())),
        Some("fail") => Some(Err(HostSandboxPreflightError {
            message: "test host sandbox preflight failure".to_string(),
        }
        .into())),
        _ => None,
    }
}

/// Select the system-owned Seatbelt launcher.
///
/// The preflight must exercise the same trusted production boundary in every
/// build. Tests that need to control the probe inject its executor through
/// [`preflight_profile_with`] instead of replacing a security boundary via
/// `PATH`.
fn sandbox_exec_program() -> &'static str {
    "/usr/bin/sandbox-exec"
}

/// Run a rendered profile through a caller-supplied probe executor.
///
/// Production calls this through [`preflight_profile`] with the trusted absolute
/// Seatbelt launcher. Route tests can inject only the probe execution, while
/// retaining the real rendered profile and every surrounding state transition.
pub fn preflight_profile_with(
    profile: &SandboxProfile,
    run_probe: impl FnOnce(&Path) -> std::io::Result<std::process::Output>,
) -> Result<()> {
    let output = run_probe(&profile.path).map_err(|error| HostSandboxPreflightError {
        message: format!("could not execute sandbox-exec: {error}"),
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            format!("sandbox-exec exited with {}", output.status)
        } else {
            stderr
        };
        return Err(HostSandboxPreflightError { message: detail }.into());
    }
    Ok(())
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

/// Render an autonomous Claude profile whose mutable provider state lives under
/// a Fluent-managed home. The operator's project/session tree is denied even
/// though the shared profile keeps legacy interactive Claude grants.
pub fn render_profile_for_access_for_autonomous_claude(
    resolver: &ContentResolver,
    source_home: &str,
    writable_roots: &[PathBuf],
    readable_roots: &[PathBuf],
) -> Result<SandboxProfile> {
    let profile = render_profile_for_access(
        resolver,
        source_home,
        writable_roots,
        readable_roots,
        &[],
        Some(CoderKind::Claude),
        None,
    )?;
    let source_home = PathBuf::from(source_home);
    let canonical_source_home =
        std::fs::canonicalize(&source_home).unwrap_or_else(|_| source_home.clone());
    let mut source_projects = vec![source_home.join(".claude/projects")];
    let canonical_projects = canonical_source_home.join(".claude/projects");
    if canonical_projects != source_projects[0] {
        source_projects.push(canonical_projects);
    }
    let deny = source_projects
        .iter()
        .flat_map(|projects| {
            [
                format!("(deny file-read* (subpath {}))", sbpl_string(projects)),
                format!("(deny file-write* (subpath {}))", sbpl_string(projects)),
            ]
        })
        .collect::<Vec<_>>()
        .join("\n");
    // File rules are resolved by the most specific matching operation and
    // filter. Keep these explicit denials after the shared Claude grants so a
    // broad home or macOS temp allowance cannot win for the project tree.
    let content = format!("{}\n{deny}\n", std::fs::read_to_string(&profile.path)?);
    std::fs::write(&profile.path, content)?;
    Ok(profile)
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
    let mut traversal_roots = writable_roots
        .iter()
        .chain(readable_roots)
        .flat_map(|root| root.ancestors().skip(1))
        .filter(|ancestor| ancestor.parent().is_some())
        .map(Path::to_path_buf)
        .collect::<Vec<_>>();
    traversal_roots.sort();
    traversal_roots.dedup();
    let traversal_rules = traversal_roots
        .iter()
        .map(|root| format!("(allow file-read-metadata (literal {}))", sbpl_string(root)))
        .collect::<Vec<_>>();
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
    traversal_rules
        .into_iter()
        .chain(writable_rules)
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
    check_sandbox_prerequisite()?;
    check_coder_prerequisite(coder_kind)?;
    Ok(())
}

pub fn check_sandbox_prerequisite() -> Result<()> {
    if !command_exists("sandbox-exec") {
        bail!("sandbox-exec not found (macOS only)");
    }
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
    fn task_host_sandbox_preflight_runs_before_reservation_or_launch() {
        let resolver = ContentResolver::new(None);
        let profile = render_profile(&resolver, "/Users/test", "/Users/test/project").unwrap();
        let expected_path = profile.path.clone();
        let observed_path = std::cell::RefCell::new(None);

        preflight_profile_with(&profile, |path| {
            *observed_path.borrow_mut() = Some(path.to_path_buf());
            Command::new("/usr/bin/true").output()
        })
        .unwrap();

        assert_eq!(observed_path.into_inner(), Some(expected_path));
    }

    #[test]
    fn host_sandbox_preflight_uses_the_system_launcher() {
        assert_eq!(sandbox_exec_program(), "/usr/bin/sandbox-exec");
    }

    #[test]
    fn codex_launcher_preflight_uses_a_profile_readable_working_directory() {
        let profile = PathBuf::from("/tmp/profile.sb");
        let launcher = PathBuf::from("/opt/codex/bin/codex");
        let worker_home = PathBuf::from("/tmp/fluent-codex-worker");

        let command = codex_preflight_command(&profile, &launcher, &worker_home);

        assert_eq!(command.get_current_dir(), Some(env::temp_dir().as_path()));
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
    fn external_launcher_root_grants_only_metadata_to_enclosing_home() {
        let resolver = ContentResolver::new(None);
        let operator_home = PathBuf::from("/Users/operator");
        let launcher_root = operator_home.join(".nvm/versions/node/lib/node_modules/@openai/codex");
        let profile = render_profile_for_access_for_coder(
            &resolver,
            "/isolated/home",
            &[PathBuf::from("/workspace")],
            &[launcher_root.clone()],
            CoderKind::Codex,
        )
        .unwrap();

        let content = std::fs::read_to_string(&profile.path).unwrap();

        assert!(content.contains(&format!(
            "(allow file-read*  (subpath {}))",
            sbpl_string(&launcher_root)
        )));
        assert!(content.contains(&format!(
            "(allow file-read-metadata (literal {}))",
            sbpl_string(&operator_home)
        )));
        assert!(!content.contains(&format!(
            "(allow file-read*  (subpath {}))",
            sbpl_string(&operator_home)
        )));
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
    fn autonomous_codex_profile_grants_only_worker_home() {
        let resolver = ContentResolver::new(None);
        let worker_home = PathBuf::from("/private/tmp/fluent-codex-worker");
        let profile = render_profile_for_access_for_coder_with_codex_home(
            &resolver,
            "/Users/test",
            &[
                PathBuf::from("/Users/test/workspace/run"),
                worker_home.clone(),
            ],
            &[],
            CoderKind::Codex,
            Some(&worker_home),
        )
        .unwrap();

        let content = std::fs::read_to_string(&profile.path).unwrap();
        assert!(
            content.contains("/private/tmp/fluent-codex-worker"),
            "{content}"
        );
        assert!(!content.contains("/Users/test/.codex"), "{content}");
    }

    #[test]
    fn interactive_codex_profile_preserves_source_home_access() {
        let resolver = ContentResolver::new(None);
        let profile = render_profile_for_roots_for_coder(
            &resolver,
            "/Users/test",
            &[PathBuf::from("/Users/test/workspace/run")],
            CoderKind::Codex,
        )
        .unwrap();

        let content = std::fs::read_to_string(&profile.path).unwrap();
        assert!(content.contains("/Users/test/.codex"), "{content}");
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

    #[test]
    fn autonomous_claude_profile_denies_operator_project_state() {
        let resolver = ContentResolver::new(None);
        let worker_home = PathBuf::from("/workspace/artifacts/claude-home");
        let profile = render_profile_for_access_for_autonomous_claude(
            &resolver,
            "/Users/operator",
            &[PathBuf::from("/workspace/candidate"), worker_home.clone()],
            &[],
        )
        .unwrap();

        let content = std::fs::read_to_string(&profile.path).unwrap();
        assert!(
            content.contains("(deny file-read* (subpath \"/Users/operator/.claude/projects\"))")
        );
        assert!(
            content.contains("(deny file-write* (subpath \"/Users/operator/.claude/projects\"))")
        );
        assert!(content.contains(&format!(
            "(allow file-write* (subpath {}))",
            sbpl_string(&worker_home)
        )));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn autonomous_claude_profile_confines_memory_writes_to_worker_home() {
        let source = tempfile::tempdir().unwrap();
        let managed = tempfile::tempdir().unwrap();
        let source_memory = source.path().join(".claude/projects/project/memory");
        let worker_home = managed.path().join("claude-home");
        std::fs::create_dir_all(&source_memory).unwrap();
        std::fs::create_dir_all(worker_home.join(".claude/projects/project/memory")).unwrap();
        let resolver = ContentResolver::new(None);
        let profile = render_profile_for_access_for_autonomous_claude(
            &resolver,
            &source.path().to_string_lossy(),
            std::slice::from_ref(&worker_home),
            &[],
        )
        .unwrap();
        if let Err(error) = preflight_profile(&profile) {
            if error.to_string().contains("Operation not permitted") {
                // A parent Seatbelt profile cannot apply a nested profile. The
                // production host preflight and unsandboxed macOS CI exercise the
                // command-level assertions below.
                return;
            }
            panic!("autonomous Claude profile preflight failed: {error:#}");
        }

        let managed_status = Command::new(sandbox_exec_program())
            .args(["-f", profile.path.to_str().unwrap(), "/usr/bin/touch"])
            .arg(worker_home.join(".claude/projects/project/memory/managed"))
            .status()
            .unwrap();
        assert!(managed_status.success());

        let escaped = source_memory.join("escaped");
        let escaped_status = Command::new(sandbox_exec_program())
            .args(["-f", profile.path.to_str().unwrap(), "/usr/bin/touch"])
            .arg(&escaped)
            .status()
            .unwrap();
        assert!(!escaped_status.success());
        assert!(!escaped.exists());
    }
}
