use anyhow::{Context, Result, bail};
use std::env;
use std::fmt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tempfile::NamedTempFile;

use crate::coder::CoderKind;
use crate::content::ContentResolver;

pub const HOST_SANDBOX_BLOCKED_CODE: &str = "FLUENT_HOST_SANDBOX_BLOCKED";
pub const HOST_SANDBOX_RECOVERY_GUIDANCE: &str = "Run this Fluent command outside the enclosing \
Codex or Claude Code sandbox through that agent's normal per-command approval flow, then retry \
the same command. Fluent will still sandbox its workers; do not pass --no-sandbox or grant the \
host agent full-session access to recover this boundary.";

/// Rendered sandbox profile that cleans up on drop.
pub struct SandboxProfile {
    _temp_file: NamedTempFile,
    pub path: PathBuf,
}

/// Add only one invocation's private socket directory to an existing worker
/// profile. The caller retains the returned file until the server has started.
pub fn profile_with_terminal_socket(
    profile: &SandboxProfile,
    socket_dir: &Path,
) -> Result<SandboxProfile> {
    profile_with_terminal_sockets(profile, socket_dir, &[])
}

/// Grant a managed terminal only its tmux namespace and invocation-owned
/// provider sockets. The latter carry native protocol traffic inside the same
/// worker boundary; the renderer never receives this authority.
pub fn profile_with_terminal_sockets(
    profile: &SandboxProfile,
    socket_dir: &Path,
    provider_socket_dirs: &[PathBuf],
) -> Result<SandboxProfile> {
    profile_with_terminal_resources(profile, socket_dir, provider_socket_dirs, &[], &[])
}

/// Grant exact provider runtime files alongside the invocation's private
/// socket directories. Shared provider parents receive traversal only.
pub fn profile_with_terminal_resources(
    profile: &SandboxProfile,
    socket_dir: &Path,
    provider_socket_dirs: &[PathBuf],
    provider_runtime_sockets: &[PathBuf],
    provider_runtime_files: &[PathBuf],
) -> Result<SandboxProfile> {
    anyhow::ensure!(
        socket_dir.is_absolute(),
        "terminal socket directory must be absolute"
    );
    let mut content = std::fs::read_to_string(&profile.path)?;
    content = content.replace(
        "(allow file-write* (subpath \"/private/tmp\"))",
        "; managed terminals use an invocation-private socket grant",
    );
    content = content.replace(
        "(allow file-read* (subpath \"/private/tmp\"))",
        "; managed terminals read only their private socket namespace",
    );
    content = content.replace(
        "(allow file-read* (subpath \"/tmp\"))",
        "; /tmp aliases the private temporary namespace",
    );
    let physical = socket_dir.canonicalize()?;
    for dir in std::iter::once(socket_dir).chain(provider_socket_dirs.iter().map(PathBuf::as_path))
    {
        let physical_dir = dir.canonicalize()?;
        for dir in [dir, physical_dir.as_path()] {
            content.push_str(&format!(
                "\n(allow file-read* (subpath {}))",
                sbpl_string(dir)
            ));
            content.push_str(&format!(
                "\n(allow file-write* (subpath {}))",
                sbpl_string(dir)
            ));
        }
    }
    content
        .push_str("\n(deny network-outbound (local unix-socket (path-prefix \"/private/tmp/\")))");
    content.push_str(&format!(
        "\n(allow network-bind (local unix-socket (path-prefix {})))",
        sbpl_string(&physical)
    ));
    content.push_str(&format!(
        "\n(allow network-inbound (local unix-socket (path-prefix {})))",
        sbpl_string(&physical)
    ));
    content.push_str(&format!(
        "\n(allow network-outbound (local unix-socket (path-prefix {})))",
        sbpl_string(&physical)
    ));
    for dir in provider_socket_dirs {
        let physical = dir.canonicalize()?;
        for operation in ["network-bind", "network-inbound", "network-outbound"] {
            content.push_str(&format!(
                "\n(allow {operation} (local unix-socket (path-prefix {})))",
                sbpl_string(&physical)
            ));
        }
    }
    for file in provider_runtime_sockets
        .iter()
        .chain(provider_runtime_files)
    {
        anyhow::ensure!(file.is_absolute(), "provider runtime file must be absolute");
        let parent = file
            .parent()
            .context("provider runtime file has no parent")?;
        let physical_parent = parent.canonicalize()?;
        let physical_file = physical_parent.join(
            file.file_name()
                .context("provider runtime file has no name")?,
        );
        for path in parent
            .ancestors()
            .chain(physical_parent.ancestors())
            .chain(std::iter::once(Path::new("/tmp")))
            .filter(|path| path.parent().is_some())
        {
            content.push_str(&format!(
                "\n(allow file-read-metadata (literal {}))",
                sbpl_string(path)
            ));
        }
        for path in [file, physical_file.as_path()] {
            for operation in ["file-read*", "file-write*"] {
                content.push_str(&format!(
                    "\n(allow {operation} (literal {}))",
                    sbpl_string(path)
                ));
            }
            if provider_runtime_sockets.contains(file) {
                for operation in ["network-bind", "network-inbound", "network-outbound"] {
                    content.push_str(&format!(
                        "\n(allow {operation} (local unix-socket (path-literal {})))",
                        sbpl_string(path)
                    ));
                }
            }
        }
    }
    let temp_file = NamedTempFile::with_prefix("fluent-terminal-sandbox-")?;
    std::fs::write(temp_file.path(), content)?;
    Ok(SandboxProfile {
        path: temp_file.path().into(),
        _temp_file: temp_file,
    })
}

/// Render the immutable Tester policy in memory and publish its first file
/// inside validated host-only storage. Candidate and user overrides cannot
/// change the policy identity approved by the operator.
pub(crate) fn render_protected_tester_profile(
    home: &str,
    writable_roots: &[PathBuf],
    readable_roots: &[PathBuf],
    denied_write_roots: &[PathBuf],
    directory: &Path,
) -> Result<SandboxProfile> {
    let content = render_profile_content(
        include_str!("../sandboxes/common.sb").to_owned(),
        home,
        writable_roots,
        readable_roots,
        denied_write_roots,
        None,
        true,
    )?;
    private_tester_profile(content.as_bytes(), directory)
}

/// Give only one approved Tester command the exact process-table read rule.
/// Keep the baseline profile available for adjacent commands and extraction.
pub(crate) fn protect_process_table_profile(
    baseline: &SandboxProfile,
    directory: &Path,
) -> Result<SandboxProfile> {
    let mut content = std::fs::read(&baseline.path)?;
    content.extend_from_slice(b"\n");
    content.extend_from_slice(crate::tester_permissions::PROCESS_TABLE_RULE.as_bytes());
    content.extend_from_slice(b"\n");
    private_tester_profile(&content, directory)
}

fn private_tester_profile(content: &[u8], directory: &Path) -> Result<SandboxProfile> {
    anyhow::ensure!(
        directory == std::fs::canonicalize(directory)?,
        "protected profile directory must be canonical"
    );
    let metadata = std::fs::symlink_metadata(directory)?;
    anyhow::ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.permissions().mode() & 0o077 == 0,
        "protected profile directory has unsafe ownership or permissions"
    );
    let mut protected = NamedTempFile::new_in(directory)?;
    use std::io::Write;
    protected.write_all(content)?;
    protected.as_file().sync_all()?;
    let path = protected.path().to_path_buf();
    Ok(SandboxProfile {
        _temp_file: protected,
        path,
    })
}

/// Resolve the account home independently of a worker-controlled HOME value.
pub(crate) fn operator_home() -> Result<PathBuf> {
    let mut password = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut buffer = vec![0u8; 16 * 1024];
    let mut result = std::ptr::null_mut();
    // SAFETY: getpwuid_r writes within the supplied passwd and buffer. Its
    // returned pointer remains valid while the buffer is alive.
    let code = unsafe {
        libc::getpwuid_r(
            libc::geteuid(),
            password.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    anyhow::ensure!(
        code == 0 && !result.is_null(),
        "cannot resolve operator home"
    );
    // SAFETY: successful getpwuid_r initialized pw_dir in the live buffer.
    let home = unsafe {
        std::ffi::CStr::from_ptr((*result).pw_dir)
            .to_str()
            .context("operator home is not UTF-8")?
            .to_owned()
    };
    Ok(PathBuf::from(home))
}

/// Deny existing source product entries after the profile's broad temporary
/// directory grants. Include the physical path because macOS resolves /var to
/// /private/var before applying Seatbelt rules.
pub(crate) fn deny_writer_source_entries(
    profile: &SandboxProfile,
    source: &Path,
    candidate: &Path,
    task_artifact: &Path,
    writer_cache: &Path,
    claude_home: Option<&Path>,
) -> Result<()> {
    let physical_source = std::fs::canonicalize(source)?;
    // The Writer profile is rendered before the worktree is created. Resolve
    // its existing parent without creating the workspace during preflight.
    let physical_candidate = match std::fs::canonicalize(candidate) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = candidate
                .parent()
                .context("Writer candidate has no parent")?;
            std::fs::canonicalize(parent)?.join(
                candidate
                    .file_name()
                    .context("Writer candidate has no file name")?,
            )
        }
        Err(error) => return Err(error.into()),
    };
    let artifact_root = source.join(".fluent/work/artifacts");
    let relative_artifact = task_artifact
        .strip_prefix(&artifact_root)
        .context("Writer artifact grant must stay inside the Work artifact tree")?;
    anyhow::ensure!(
        relative_artifact.components().count() >= 3
            && relative_artifact
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_))),
        "Writer artifact grant must name one Task artifact area"
    );
    let mut identity = relative_artifact.components();
    let work_item = identity
        .next()
        .context("Writer artifact has no Work Item")?;
    let attempt = identity.next().context("Writer artifact has no Attempt")?;
    let expected_cache = source
        .join(".fluent/work/cache/writers")
        .join(work_item.as_os_str())
        .join(attempt.as_os_str());
    anyhow::ensure!(
        writer_cache == expected_cache,
        "Writer cache grant must name the exact current Attempt"
    );
    if let Some(home) = claude_home {
        // Recovery artifacts can nest below the Attempt. Keep the home bound
        // to the same validated Work Item and Attempt as the dependency cache.
        let expected_home = artifact_root
            .join(work_item.as_os_str())
            .join(attempt.as_os_str())
            .join("claude-home");
        anyhow::ensure!(
            home == expected_home,
            "Writer Claude home grant must name the exact current Attempt"
        );
    }
    if physical_source == physical_candidate {
        return Ok(());
    }
    let mut content = std::fs::read_to_string(&profile.path)?;
    // Git must be able to enter the source checkout to verify the candidate's
    // common directory and registration. A literal directory read permits that
    // operation without granting any source descendants or directory writes.
    for root in [source, physical_source.as_path()] {
        content.push_str(&format!(
            "\n(allow file-read* (literal {}))",
            sbpl_string(root)
        ));
    }
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        if candidate.starts_with(&path)
            || physical_candidate.starts_with(physical_source.join(entry.file_name()))
            || entry.file_name() == ".git"
        {
            continue;
        }
        if entry.file_name() == ".fluent" {
            for child in std::fs::read_dir(&path)? {
                let child = child?;
                if child.file_name() == "work" {
                    append_writer_source_rule(
                        &mut content,
                        source,
                        &physical_source,
                        &child.path(),
                        "deny",
                    );
                    let progress = child
                        .path()
                        .join("progress")
                        .join(work_item.as_os_str())
                        .join(attempt.as_os_str());
                    append_writer_source_rule(
                        &mut content,
                        source,
                        &physical_source,
                        &progress,
                        "allow",
                    );
                    append_writer_source_rule(
                        &mut content,
                        source,
                        &physical_source,
                        task_artifact,
                        "allow",
                    );
                    append_writer_source_rule(
                        &mut content,
                        source,
                        &physical_source,
                        writer_cache,
                        "allow",
                    );
                    if let Some(home) = claude_home {
                        append_writer_source_rule(
                            &mut content,
                            source,
                            &physical_source,
                            home,
                            "allow",
                        );
                    }
                } else {
                    append_writer_source_rule(
                        &mut content,
                        source,
                        &physical_source,
                        &child.path(),
                        "deny",
                    );
                }
            }
            continue;
        }
        append_writer_source_rule(&mut content, source, &physical_source, &path, "deny");
    }
    std::fs::write(&profile.path, content)?;
    Ok(())
}

fn append_writer_source_rule(
    content: &mut String,
    source: &Path,
    physical_source: &Path,
    path: &Path,
    decision: &str,
) {
    content.push_str(&format!(
        "\n({decision} file-write* (subpath {}))",
        sbpl_string(path)
    ));
    let physical_path = physical_source.join(path.strip_prefix(source).unwrap());
    if physical_path != path {
        content.push_str(&format!(
            "\n({decision} file-write* (subpath {}))",
            sbpl_string(&physical_path)
        ));
    }
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
        write!(
            formatter,
            "{HOST_SANDBOX_BLOCKED_CODE}: host sandbox preflight failed: {}\n\
             {HOST_SANDBOX_RECOVERY_GUIDANCE}",
            self.message,
        )
    }
}

impl std::error::Error for HostSandboxPreflightError {}

impl HostSandboxPreflightError {
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// A provider process started, but its runtime could not inspect inherited
/// process streams inside Fluent's production Seatbelt boundary. This is a host
/// boundary failure rather than model work and is therefore safely resumable.
#[derive(Debug)]
pub(crate) struct HostSandboxLaunchError {
    message: String,
}

impl fmt::Display for HostSandboxLaunchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{HOST_SANDBOX_BLOCKED_CODE}: provider launch was blocked by the sandbox boundary: {}",
            self.message
        )
    }
}

impl std::error::Error for HostSandboxLaunchError {}

impl HostSandboxLaunchError {
    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

/// Recognize the narrow Claude/Bun startup failure seen when an inherited
/// stdout or stderr descriptor points outside the worker's readable boundary.
/// Requiring both the fstat denial and stream-construction evidence avoids
/// treating ordinary workspace permission errors as host-sandbox failures.
pub(crate) fn classify_host_sandbox_launch_stderr(path: &Path) -> Option<HostSandboxLaunchError> {
    let diagnostic = std::fs::read_to_string(path).ok()?;
    let denied_fstat = diagnostic.contains("EPERM: operation not permitted, fstat");
    let inherited_stream = diagnostic.contains("new WriteStream")
        || diagnostic.contains("process.stderr.fd")
        || diagnostic.contains("process.stdout.fd");
    (denied_fstat && inherited_stream).then(|| HostSandboxLaunchError {
        message: format!(
            "Claude's runtime could not inspect its standard stream; diagnostic saved at {}",
            path.display()
        ),
    })
}

/// Claude 2.1.220's Bash tool ignores TMPDIR and creates per-session command
/// state beneath `/private/tmp/claude-<uid>/<workspace-key>/`. Learner profiles
/// intentionally remove the shared temp-tree grant, so grant only the current
/// workspace's namespace rather than restoring write access to `/private/tmp`.
#[cfg(unix)]
pub(crate) fn claude_bash_temp_root(workspace_path: &Path) -> PathBuf {
    // SAFETY: geteuid has no preconditions and does not mutate process state.
    let uid = unsafe { libc::geteuid() };
    let workspace_key: String = workspace_path
        .to_string_lossy()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '-'
            }
        })
        .collect();
    PathBuf::from(format!("/private/tmp/claude-{uid}/{workspace_key}"))
}

#[cfg(not(unix))]
pub(crate) fn claude_bash_temp_root(workspace_path: &Path) -> PathBuf {
    std::env::temp_dir()
        .join("claude")
        .join(workspace_path.to_string_lossy().replace(['/', '\\'], "-"))
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

/// Verify that an already-rendered profile can read every existing root in a
/// worker launch plan.
///
/// Run this before reserving the model call. A profile that launches correctly
/// but cannot open its declared candidate or evidence inputs is not a usable
/// worker boundary.
pub fn preflight_readable_paths(profile: &SandboxProfile, paths: &[PathBuf]) -> Result<()> {
    #[cfg(feature = "test-support")]
    if let Some(result) = test_reviewer_read_preflight_result() {
        return result;
    }
    #[cfg(feature = "test-support")]
    if env::var_os("FLUENT_TEST_WORKER_READ_PROBE").is_none()
        && let Some(result) = test_preflight_result()
    {
        return result;
    }
    #[cfg(target_os = "macos")]
    {
        let mut existing = Vec::new();
        for path in paths {
            match std::fs::metadata(path) {
                Ok(_) => existing.push(path),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(HostSandboxPreflightError {
                        message: format!(
                            "worker cannot inspect declared path {}: {error}",
                            path.display()
                        ),
                    }
                    .into());
                }
            }
        }
        if existing.is_empty() {
            return Ok(());
        }

        let files = existing
            .iter()
            .copied()
            .filter(|path| path.is_file())
            .collect::<Vec<_>>();
        let directories = existing
            .iter()
            .copied()
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        let run_probe = |program: &str, args: &[&str], probe_paths: &[&PathBuf]| {
            // Replace only probe execution in deterministic route tests. The
            // ordinary build always applies the trusted system launcher.
            #[cfg(feature = "test-support")]
            let launcher = env::var_os("FLUENT_TEST_WORKER_READ_PROBE")
                .unwrap_or_else(|| sandbox_exec_program().into());
            #[cfg(not(feature = "test-support"))]
            let launcher = sandbox_exec_program();
            let mut command = Command::new(launcher);
            command
                .arg("-f")
                .arg(&profile.path)
                .arg(program)
                .args(args)
                .args(probe_paths)
                .stdout(Stdio::null());
            command.output()
        };
        for (kind, program, args, group) in [
            ("file", "/bin/cat", Vec::new(), files),
            ("directory", "/bin/ls", vec!["-1"], directories),
        ] {
            if group.is_empty() {
                continue;
            }
            let output =
                run_probe(program, &args, &group).map_err(|error| HostSandboxPreflightError {
                    message: format!("could not probe worker {kind} read paths: {error}"),
                })?;
            if output.status.success() {
                continue;
            }

            for path in group {
                let output = run_probe(program, &args, &[path]).map_err(|error| {
                    HostSandboxPreflightError {
                        message: format!(
                            "could not probe worker read path {}: {error}",
                            path.display()
                        ),
                    }
                })?;
                if output.status.success() {
                    continue;
                }
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let detail = if stderr.is_empty() {
                    format!("{program} exited with {}", output.status)
                } else {
                    stderr
                };
                return Err(HostSandboxPreflightError {
                    message: format!(
                        "worker cannot read declared path {}: {detail}",
                        path.display()
                    ),
                }
                .into());
            }
            return Err(HostSandboxPreflightError {
                message: format!("worker {kind} read-path probe failed without identifying a path"),
            }
            .into());
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (profile, paths);
        Ok(())
    }
}

/// Verify that the enclosing host lets Fluent establish its own worker sandbox.
///
/// This probe applies Fluent's production common Seatbelt layer to a harmless
/// process. Call it before provider authentication or durable execution state so
/// an outer agent sandbox cannot masquerade as a provider login failure or claim
/// work that the host cannot run.
pub fn preflight_host_execution(project_root: &Path, resolver: &ContentResolver) -> Result<()> {
    check_sandbox_prerequisite()?;
    let home = env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let project_root =
        std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    let profile = render_profile_common_only(resolver, &home, &[project_root], &[])?;
    preflight_profile(&profile)
}

/// Authenticate through the prepared Codex launcher inside the exact profile
/// that the autonomous role will retain for its model launch.
pub fn preflight_codex_launcher(
    profile: &SandboxProfile,
    worker: &crate::codex_worker::CodexWorkerEnvironment,
) -> Result<()> {
    // Exercise this later gate independently of the coordinator's host probe.
    #[cfg(feature = "test-support")]
    if env::var("FLUENT_TEST_CODEX_SANDBOX_PREFLIGHT").as_deref() == Ok("fail") {
        return Err(HostSandboxPreflightError {
            message: "test private Codex sandbox preflight failure".into(),
        }
        .into());
    }
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
        // directory. The staged private home is an explicit capability even
        // when a strict profile denies the shared system temporary directory.
        .current_dir(worker_home)
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

/// Fail only the Reviewer read-path probe in route tests. The general host
/// preflight can still pass, proving that this later gate prevents reservation.
#[cfg(feature = "test-support")]
fn test_reviewer_read_preflight_result() -> Option<Result<()>> {
    match env::var("FLUENT_TEST_REVIEWER_READ_PREFLIGHT")
        .ok()
        .as_deref()
    {
        Some("fail") => Some(Err(HostSandboxPreflightError {
            message: "test reviewer read-path preflight failure".to_string(),
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

/// Build a command under the exact rendered Seatbelt profile. Production
/// always uses the system launcher; route tests may replace only that launcher
/// in test-support builds.
pub(crate) fn command_under_profile(profile: &SandboxProfile, program: &Path) -> Command {
    #[cfg(feature = "test-support")]
    let launcher = env::var_os("FLUENT_TEST_FOCUSED_EVIDENCE_SANDBOX_EXEC")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(sandbox_exec_program()));
    #[cfg(not(feature = "test-support"))]
    let launcher = PathBuf::from(sandbox_exec_program());
    let mut command = Command::new(launcher);
    command.arg("-f").arg(&profile.path).arg(program);
    command
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
        true,
    )
}

/// Render a Codex profile whose private home and explicit scope roots replace
/// access to the runner owner's home.
pub fn render_profile_for_scoped_codex_provider(
    resolver: &ContentResolver,
    private_home: &Path,
    writable_roots: &[PathBuf],
    readable_roots: &[PathBuf],
    codex_home: &Path,
) -> Result<SandboxProfile> {
    // Include the overlay's private home in root rendering so realpath can
    // traverse its ancestors without reading unrelated temporary contents.
    let mut readable = readable_roots.to_vec();
    readable.push(codex_home.to_path_buf());
    let profile = render_profile_for_access(
        resolver,
        &private_home.to_string_lossy(),
        writable_roots,
        &readable,
        &[],
        Some(CoderKind::Codex),
        Some(codex_home),
        false,
    )?;
    let mut content = std::fs::read_to_string(&profile.path)?;
    for broad_grant in [
        "(allow file-read* (subpath \"/private/var\"))",
        "(allow file-read* (subpath \"/private/tmp\"))",
        "(allow file-read* (subpath \"/var\"))",
        "(allow file-read* (subpath \"/tmp\"))",
        "(allow file-read*  (subpath \"/private/var/folders\"))",
        "(allow file-write* (subpath \"/private/var/folders\"))",
        "(allow file-write* (subpath \"/private/tmp\"))",
        "(allow file-read* (subpath \"/Volumes\"))",
    ] {
        content = content.replace(
            broad_grant,
            "; scoped providers do not receive shared filesystem access",
        );
    }
    std::fs::write(&profile.path, content)?;
    Ok(profile)
}

/// Admit installed tools read-only while allowing compiled workspace programs.
/// Keep these grants separate from the provider's private mutable home.
pub(crate) fn render_profile_for_scoped_codex_provider_with_tools(
    resolver: &ContentResolver,
    private_home: &Path,
    writable_roots: &[PathBuf],
    readable_roots: &[PathBuf],
    codex_home: &Path,
    installed_tools: &[PathBuf],
) -> Result<SandboxProfile> {
    let mut readable = readable_roots.to_vec();
    readable.extend_from_slice(installed_tools);
    let profile = render_profile_for_scoped_codex_provider(
        resolver,
        private_home,
        writable_roots,
        &readable,
        codex_home,
    )?;
    let mut content = std::fs::read_to_string(&profile.path)?;
    for root in writable_roots.iter().chain(installed_tools) {
        content.push_str(&format!(
            "\n(allow file-map-executable (subpath {}))",
            sbpl_string(root)
        ));
    }
    for root in installed_tools {
        content.push_str(&format!(
            "\n(deny file-write* (subpath {}))",
            sbpl_string(root)
        ));
    }
    std::fs::write(&profile.path, content)?;
    Ok(profile)
}

/// Admit installed tools read-only and map native dependencies built in the
/// assigned workspace. Tool parents receive only metadata traversal.
pub(crate) fn admit_installed_tools(
    profile: &SandboxProfile,
    tools: &crate::installed_tools::NodeToolAccess,
    workspace: &Path,
) -> Result<()> {
    let roots = &tools.roots;
    if roots.is_empty() {
        return Ok(());
    }
    let mut content = std::fs::read_to_string(&profile.path)?;
    content.push('\n');
    content.push_str(&render_root_rules(&[], roots));
    for directory in &tools.search_directories {
        content.push_str(&format!(
            "\n(allow file-read-metadata (literal {}))",
            sbpl_string(directory)
        ));
    }
    content.push_str(&format!(
        "\n(allow file-map-executable (subpath {}))",
        sbpl_string(workspace)
    ));
    for root in roots {
        content.push_str(&format!(
            "\n(allow file-map-executable (subpath {0}))\n(deny file-write* (subpath {0}))",
            sbpl_string(root)
        ));
    }
    std::fs::write(&profile.path, content)?;
    Ok(())
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
        true,
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
        true,
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
        true,
    )
}

/// Render the common tool profile and deny access to exact user-state trees
/// that a host-owned check never needs. Keep these denials after the common
/// grants so broad provider compatibility rules cannot expose agent sessions.
pub fn render_profile_common_only_with_denied_access(
    resolver: &ContentResolver,
    home: &str,
    writable_roots: &[PathBuf],
    readable_roots: &[PathBuf],
    denied_read_roots: &[PathBuf],
    denied_write_roots: &[PathBuf],
) -> Result<SandboxProfile> {
    let profile = render_profile_common_only(resolver, home, writable_roots, readable_roots)?;
    let mut rules = Vec::new();
    rules.extend(
        denied_read_roots
            .iter()
            .map(|root| format!("(deny file-read* (subpath {}))", sbpl_string(root))),
    );
    rules.extend(
        denied_write_roots
            .iter()
            .map(|root| format!("(deny file-write* (subpath {}))", sbpl_string(root))),
    );
    if !rules.is_empty() {
        let content = format!(
            "{}\n{}\n",
            std::fs::read_to_string(&profile.path)?,
            rules.join("\n")
        );
        std::fs::write(&profile.path, content)?;
    }
    Ok(profile)
}

fn render_profile_for_access(
    resolver: &ContentResolver,
    home: &str,
    writable_roots: &[PathBuf],
    readable_roots: &[PathBuf],
    denied_write_roots: &[PathBuf],
    coder_kind: Option<CoderKind>,
    codex_home: Option<&Path>,
    deny_codex_source_home: bool,
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

    let rendered = render_profile_content(
        combined,
        home,
        writable_roots,
        readable_roots,
        denied_write_roots,
        codex_home,
        deny_codex_source_home,
    )?;

    let temp_file = NamedTempFile::with_prefix("fluent-sandbox-")?;
    std::fs::write(temp_file.path(), &rendered)?;

    let path = temp_file.path().to_path_buf();
    Ok(SandboxProfile {
        _temp_file: temp_file,
        path,
    })
}

fn render_profile_content(
    combined: String,
    home: &str,
    writable_roots: &[PathBuf],
    readable_roots: &[PathBuf],
    denied_write_roots: &[PathBuf],
    codex_home: Option<&Path>,
    deny_codex_source_home: bool,
) -> Result<String> {
    anyhow::ensure!(
        !writable_roots.is_empty(),
        "At least one writable sandbox root is required"
    );
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
    let operator_home = operator_home()?;
    anyhow::ensure!(
        operator_home == std::fs::canonicalize(&operator_home)?,
        "operator home must be canonical"
    );
    let permission_store = operator_home.join(".config/fluent/tester-permissions");
    deny_rules.push(format!(
        "(deny file-read-data (subpath {}))",
        sbpl_string(&permission_store)
    ));
    deny_rules.push(format!(
        "(deny file-write* (subpath {}))",
        sbpl_string(&permission_store)
    ));
    // Autonomous Codex workers use a staged home.  Deny the interactive source
    // explicitly before common.sb's broad home read grants so the profile cannot
    // reach hooks, configuration, sessions, or the original credentials.
    if deny_codex_source_home && let Some(worker_home) = codex_home {
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

    Ok(rendered)
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

pub(crate) fn sbpl_string(path: &Path) -> String {
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

    #[test]
    fn protected_tester_profile_uses_private_canonical_directory() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let private = root.join("profiles");
        std::fs::create_dir(&private).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        // A hostile source pathname in a shared temporary directory cannot be
        // copied: this renderer accepts only trusted policy and root inputs.
        let hostile = root.join("fluent-sandbox-hostile");
        std::fs::write(&hostile, "(allow default)").unwrap();
        let protected = render_protected_tester_profile(
            "/staged/worker-home",
            &[root.join("workspace")],
            &[],
            &[],
            &private,
        )
        .unwrap();
        let bytes = std::fs::read_to_string(&protected.path).unwrap();
        assert!(bytes.contains("(deny default)"));
        assert!(!bytes.contains("(allow default)"));
        assert!(protected.path.starts_with(&private));
        assert_eq!(
            std::fs::metadata(&protected.path)
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );

        let alias = root.join("alias");
        std::os::unix::fs::symlink(&private, &alias).unwrap();
        assert!(
            render_protected_tester_profile(
                "/staged/worker-home",
                &[root.join("workspace")],
                &[],
                &[],
                &alias,
            )
            .is_err()
        );
    }

    #[test]
    fn protected_tester_profile_never_uses_shared_temp_storage() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let private = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        let previous = std::env::var_os("TMPDIR");
        // Make shared temporary publication impossible. The old render-then-copy
        // path would fail here before it could create a protected profile.
        unsafe {
            std::env::set_var("TMPDIR", private.join("unavailable-shared-temp"));
        }
        let shared = NamedTempFile::with_prefix("fluent-sandbox-");
        let protected = render_protected_tester_profile(
            "/staged/worker-home",
            &[private.join("workspace")],
            &[],
            &[],
            &private,
        );
        unsafe {
            if let Some(previous) = previous {
                std::env::set_var("TMPDIR", previous);
            } else {
                std::env::remove_var("TMPDIR");
            }
        }
        assert!(
            shared.is_err(),
            "the adversarial shared destination must be unavailable"
        );
        let protected = protected.expect("publish directly into protected storage");
        assert_eq!(protected.path.parent(), Some(private.as_path()));
        let content = std::fs::read_to_string(&protected.path).unwrap();
        assert!(content.contains("(deny default)"));
        assert!(content.contains("tester-permissions"));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn process_table_rule_matches_four_element_mib_name() {
        // Ask only for the MIB's name; do not enumerate host processes. XNU
        // preserves the fourth numeric component when it renders this name.
        let mut mib = [
            0,
            1,
            libc::CTL_KERN,
            libc::KERN_PROC,
            libc::KERN_PROC_ALL,
            0,
        ];
        let mut name = [0u8; 128];
        let mut length = name.len();
        let result = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as _,
                name.as_mut_ptr().cast(),
                &mut length,
                std::ptr::null_mut(),
                0,
            )
        };
        assert_eq!(
            result,
            0,
            "MIB name lookup: {}",
            std::io::Error::last_os_error()
        );
        let name = std::ffi::CStr::from_bytes_with_nul(&name[..length])
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(
            crate::tester_permissions::PROCESS_TABLE_RULE,
            format!("(allow sysctl-read (sysctl-name {name:?}))"),
            "the exact rule must match the consumer's four-element MIB"
        );
    }

    #[test]
    fn process_table_profile_adds_only_the_declared_read_rule() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let private = root.join("profiles");
        std::fs::create_dir(&private).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        let baseline = render_profile_common_only(
            &ContentResolver::new(None),
            "/staged/worker-home",
            &[root.join("workspace")],
            &[],
        )
        .unwrap();
        let original = std::fs::read_to_string(&baseline.path).unwrap();
        assert!(!original.contains("kern.proc.all"));
        let granted = protect_process_table_profile(&baseline, &private).unwrap();
        let content = std::fs::read_to_string(&granted.path).unwrap();
        assert_eq!(
            content,
            format!(
                "{original}\n{}\n",
                crate::tester_permissions::PROCESS_TABLE_RULE
            )
        );
        assert_eq!(std::fs::read_to_string(&baseline.path).unwrap(), original);
        assert!(granted.path.starts_with(&private));
    }

    #[test]
    fn worker_profiles_cannot_read_or_write_operator_permission_store() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile = render_profile_common_only(
            &ContentResolver::new(None),
            "/staged/worker-home",
            &[tmp.path().to_path_buf()],
            &[],
        )
        .unwrap();
        let content = std::fs::read_to_string(profile.path).unwrap();
        let authority = operator_home()
            .unwrap()
            .join(".config/fluent/tester-permissions");
        let quoted = sbpl_string(&authority);
        assert!(content.contains(&format!("(deny file-read-data (subpath {quoted}))")));
        assert!(content.contains(&format!("(deny file-write* (subpath {quoted}))")));
    }

    #[test]
    fn scoped_writer_tools_allow_mapping_and_deny_installed_writes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let private = tmp.path().join("private");
        let installed = tmp.path().join("installed");
        let workspace = tmp.path().join("workspace");
        let profile = render_profile_for_scoped_codex_provider_with_tools(
            &ContentResolver::new(None),
            &private,
            &[workspace.clone()],
            &[],
            &private,
            &[installed.clone()],
        )
        .unwrap();
        let text = std::fs::read_to_string(profile.path).unwrap();
        assert!(text.contains(&format!(
            "(allow file-map-executable (subpath {}))",
            sbpl_string(&installed)
        )));
        assert!(text.contains(&format!(
            "(deny file-write* (subpath {}))",
            sbpl_string(&installed)
        )));
        assert!(text.contains(&format!(
            "(allow file-map-executable (subpath {}))",
            sbpl_string(&workspace)
        )));
        assert!(!text.contains("(allow file-write* (subpath \"/private/tmp\"))"));
    }
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
    fn host_sandbox_failure_explains_scoped_agent_recovery() {
        let error = HostSandboxPreflightError {
            message: "sandbox_apply: Operation not permitted".to_string(),
        };
        let rendered = error.to_string();

        assert!(rendered.contains(HOST_SANDBOX_BLOCKED_CODE));
        assert!(rendered.contains("outside the enclosing Codex or Claude Code sandbox"));
        assert!(rendered.contains("normal per-command approval flow"));
        assert!(rendered.contains("Fluent will still sandbox its workers"));
        assert!(rendered.contains("do not pass --no-sandbox"));
        assert!(rendered.contains("full-session access"));
    }

    #[test]
    fn claude_stream_fstat_denial_is_a_host_sandbox_launch_failure() {
        let dir = tempfile::tempdir().unwrap();
        let diagnostic = dir.path().join("provider-stderr.log");
        std::fs::write(
            &diagnostic,
            "EPERM: operation not permitted, fstat\n    at new WriteStream\nprocess.stderr.fd\n",
        )
        .unwrap();

        let failure = classify_host_sandbox_launch_stderr(&diagnostic)
            .expect("the Claude/Bun inherited-stream failure must be recognized");
        assert!(failure.message().contains("standard stream"));
        assert!(failure.message().contains("provider-stderr.log"));
    }

    #[test]
    fn unrelated_permission_failure_is_not_a_host_sandbox_launch_failure() {
        let dir = tempfile::tempdir().unwrap();
        let diagnostic = dir.path().join("provider-stderr.log");
        std::fs::write(
            &diagnostic,
            "EPERM: operation not permitted, open '/project/private-file'\n",
        )
        .unwrap();

        assert!(classify_host_sandbox_launch_stderr(&diagnostic).is_none());
    }

    #[test]
    fn claude_bash_temp_root_is_scoped_to_uid_and_workspace() {
        let first = claude_bash_temp_root(Path::new("/workspace/attempt-a"));
        let second = claude_bash_temp_root(Path::new("/workspace/attempt-b"));
        let dotted = claude_bash_temp_root(Path::new("/workspace/.scratch/release-0.2"));

        assert!(first.to_string_lossy().starts_with("/private/tmp/claude-"));
        assert!(first.ends_with("-workspace-attempt-a"));
        assert!(dotted.ends_with("-workspace--scratch-release-0-2"));
        assert_ne!(first, second);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn confined_profile_writes_only_its_claude_bash_temp_namespace() {
        let workspace = tempfile::tempdir().unwrap();
        let own_temp = claude_bash_temp_root(workspace.path());
        let sibling_temp = own_temp.with_file_name(format!(
            "{}-sibling",
            own_temp.file_name().unwrap().to_string_lossy()
        ));
        let resolver = ContentResolver::new(None);
        let profile = render_profile_for_access_for_coder_with_denied_writes(
            &resolver,
            "/Users/test",
            std::slice::from_ref(&own_temp),
            &[],
            &[],
            CoderKind::Claude,
        )
        .unwrap();

        let content = std::fs::read_to_string(&profile.path).unwrap();
        let own_rule = format!("(allow file-write* (subpath {}))", sbpl_string(&own_temp));
        let sibling_rule = format!(
            "(allow file-write* (subpath {}))",
            sbpl_string(&sibling_temp)
        );
        assert!(
            content.contains(&own_rule),
            "the profile must grant the task-owned Claude temp namespace"
        );
        assert!(
            !content.contains(&sibling_rule),
            "the profile must not grant a sibling Claude temp namespace"
        );
        assert!(
            !content.contains("(allow file-write* (subpath \"/private/tmp\"))"),
            "the profile must not grant the shared temp namespace"
        );

        let sandbox_usable = Command::new("/usr/bin/sandbox-exec")
            .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if sandbox_usable {
            let own_session = own_temp.join("session");
            let own = Command::new("/usr/bin/sandbox-exec")
                .arg("-f")
                .arg(&profile.path)
                .arg("/bin/mkdir")
                .arg("-p")
                .arg(&own_session)
                .output()
                .unwrap();
            assert!(
                own.status.success(),
                "the task-owned Claude temp must be writable"
            );

            let sibling = Command::new("/usr/bin/sandbox-exec")
                .arg("-f")
                .arg(&profile.path)
                .arg("/bin/mkdir")
                .arg("-p")
                .arg(sibling_temp.join("session"))
                .output()
                .unwrap();
            assert!(
                !sibling.status.success(),
                "a sibling Claude temp namespace must remain denied"
            );
        }

        let _ = std::fs::remove_dir_all(own_temp);
    }

    #[test]
    fn codex_preflight_uses_the_explicit_private_home() {
        let profile = PathBuf::from("/tmp/profile.sb");
        let launcher = PathBuf::from("/opt/codex/bin/codex");
        let worker_home = PathBuf::from("/tmp/fluent-codex-worker");

        let command = codex_preflight_command(&profile, &launcher, &worker_home);

        assert_eq!(command.get_current_dir(), Some(worker_home.as_path()));
    }

    #[test]
    fn codex_preflight_grants_private_home_ancestor_metadata_only() {
        let home = PathBuf::from("/private/var/folders/isolated/T/worker/codex");
        let profile = render_profile_for_scoped_codex_provider(
            &ContentResolver::new(None),
            &home,
            &[PathBuf::from("/Users/test/project")],
            &[],
            &home,
        )
        .unwrap();
        let content = std::fs::read_to_string(profile.path).unwrap();
        for ancestor in home.ancestors().skip(1).filter(|p| p.parent().is_some()) {
            assert!(
                content.contains(&format!(
                    "(allow file-read-metadata (literal {}))",
                    sbpl_string(ancestor)
                )),
                "missing traversal for {}",
                ancestor.display()
            );
            for operation in ["file-read*", "file-write*"] {
                assert!(
                    !content.contains(&format!(
                        "(allow {operation} (subpath {}))",
                        sbpl_string(ancestor)
                    )),
                    "broad access to {}",
                    ancestor.display()
                );
                assert!(
                    !content.contains(&format!(
                        "(allow {operation}  (subpath {}))",
                        sbpl_string(ancestor)
                    )),
                    "broad access to {}",
                    ancestor.display()
                );
            }
        }
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

    #[cfg(target_os = "macos")]
    #[test]
    fn readable_path_preflight_accepts_exact_file_and_rejects_sibling() {
        let host_preflight_override = std::env::var_os("FLUENT_TEST_HOST_SANDBOX_PREFLIGHT");
        unsafe { std::env::remove_var("FLUENT_TEST_HOST_SANDBOX_PREFLIGHT") };
        let root = tempfile::tempdir_in("target").unwrap();
        let artifact = root.path().join("review");
        let evidence = root.path().join("writer/evidence.json");
        let completion = root.path().join("writer/completion.json");
        let sibling = root.path().join("writer/private.json");
        std::fs::create_dir_all(&artifact).unwrap();
        std::fs::create_dir_all(evidence.parent().unwrap()).unwrap();
        std::fs::write(&evidence, "allowed\n").unwrap();
        std::fs::write(&completion, "allowed\n").unwrap();
        std::fs::write(&sibling, "denied\n").unwrap();

        let resolver = ContentResolver::new(None);
        let profile = render_profile_for_access_for_coder(
            &resolver,
            "/Users/test",
            std::slice::from_ref(&artifact),
            &[evidence.clone(), completion.clone()],
            CoderKind::Codex,
        )
        .unwrap();

        if let Err(error) = preflight_profile(&profile) {
            if let Some(value) = host_preflight_override {
                unsafe { std::env::set_var("FLUENT_TEST_HOST_SANDBOX_PREFLIGHT", value) };
            }
            if error.to_string().contains("Operation not permitted") {
                return;
            }
            panic!("reviewer profile preflight failed: {error:#}");
        }

        let allowed = preflight_readable_paths(&profile, &[artifact, evidence, completion]);
        let denied = preflight_readable_paths(&profile, std::slice::from_ref(&sibling));
        if let Some(value) = host_preflight_override {
            unsafe { std::env::set_var("FLUENT_TEST_HOST_SANDBOX_PREFLIGHT", value) };
        }

        allowed.unwrap();
        let error = denied.expect_err("an undeclared sibling must remain unreadable");
        assert!(
            error
                .to_string()
                .contains("worker cannot read declared path"),
            "{error:#}"
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
    fn writer_profile_blocks_actual_source_write_in_temporary_checkout() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let candidate = root.path().join("candidate");
        let artifact = source.join(".fluent/work/artifacts/work/attempt/task");
        let cache = source.join(".fluent/work/cache/writers/work/attempt");
        let sibling_cache = source.join(".fluent/work/cache/writers/work/other-attempt");
        let sibling_artifact = source.join(".fluent/work/artifacts/work/attempt/other-task");
        let gate_artifact = source.join(".fluent/work/artifacts/work/attempt/candidate-gates");
        let model = source.join(".fluent/work/items/work.json");
        std::fs::create_dir_all(&artifact).unwrap();
        std::fs::create_dir_all(&sibling_artifact).unwrap();
        std::fs::create_dir_all(&gate_artifact).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::create_dir_all(&sibling_cache).unwrap();
        std::fs::create_dir_all(model.parent().unwrap()).unwrap();
        std::fs::write(&model, "host-owned").unwrap();
        std::fs::create_dir_all(source.join(".git")).unwrap();
        std::fs::create_dir_all(source.join("src")).unwrap();
        std::fs::create_dir_all(&candidate).unwrap();
        std::fs::write(source.join("src/lib.rs"), "source").unwrap();
        let profile = render_profile_common_only(
            &ContentResolver::new(None),
            "/Users/test",
            &[candidate.clone(), source.clone(), artifact.clone()],
            &[],
        )
        .unwrap();
        let other_home = source.join(".fluent/work/artifacts/work/other-attempt/claude-home");
        assert!(
            deny_writer_source_entries(
                &profile,
                &source,
                &candidate,
                &artifact,
                &cache,
                Some(&other_home),
            )
            .is_err(),
            "a Writer cannot grant a different Attempt's Claude home"
        );
        deny_writer_source_entries(&profile, &source, &candidate, &artifact, &cache, None).unwrap();
        let content = std::fs::read_to_string(&profile.path).unwrap();
        let denial = format!(
            "(deny file-write* (subpath {}))",
            sbpl_string(&source.join("src"))
        );
        let physical_source = std::fs::canonicalize(&source).unwrap();
        let physical_denial = format!(
            "(deny file-write* (subpath {}))",
            sbpl_string(&physical_source.join("src"))
        );
        assert!(
            content.contains(&physical_denial),
            "physical source path must be denied"
        );
        assert!(
            content.rfind(&denial).unwrap()
                > content
                    .rfind(&format!(
                        "(allow file-write* (subpath {}))",
                        sbpl_string(&source)
                    ))
                    .unwrap()
        );
        assert!(!content.contains(&format!(
            "(deny file-write* (subpath {}))",
            sbpl_string(&source.join(".git"))
        )));
        assert!(content.contains(&format!(
            "(deny file-write* (subpath {}))",
            sbpl_string(&source.join(".fluent/work"))
        )));
        for root in [&source, &physical_source] {
            assert!(!content.contains(&format!(
                "(allow file-write* (subpath {}))",
                sbpl_string(&root.join(".fluent/work/artifacts"))
            )));
            assert!(content.contains(&format!(
                "(allow file-write* (subpath {}))",
                sbpl_string(&root.join(".fluent/work/artifacts/work/attempt/task"))
            )));
            assert!(content.contains(&format!(
                "(allow file-write* (subpath {}))",
                sbpl_string(&root.join(".fluent/work/cache/writers/work/attempt"))
            )));
            assert!(!content.contains(&format!(
                "(allow file-write* (subpath {}))",
                sbpl_string(&root.join(".fluent/work/cache/writers/work/other-attempt"))
            )));
        }
        #[cfg(target_os = "macos")]
        {
            if let Err(error) = preflight_profile_with(&profile, |profile_path| {
                std::process::Command::new("/usr/bin/sandbox-exec")
                    .arg("-f")
                    .arg(profile_path)
                    .arg("/usr/bin/true")
                    .output()
            }) {
                if std::env::var_os("FLUENT_TEST_REQUIRE_LIVE_SOURCE_CONFINEMENT").is_none()
                    && error.to_string().contains("Operation not permitted")
                {
                    return;
                }
                panic!("live Writer source-confinement preflight failed: {error:#}");
            }
            let output = std::process::Command::new("/usr/bin/sandbox-exec")
                .arg("-f").arg(&profile.path)
                .args(["/bin/sh", "-c", "if /bin/echo changed > \"$1\"; then exit 10; fi; /bin/echo candidate > \"$2\" || exit 11; /bin/echo git > \"$3\" || exit 12; /bin/echo artifact > \"$4\" || exit 13; if /bin/echo model > \"$5\"; then exit 14; fi; if /bin/echo sibling > \"$6\"; then exit 15; fi; if /bin/echo gate > \"$7\"; then exit 16; fi; /bin/echo cache > \"$8\" || exit 17; if /bin/echo sibling-cache > \"$9\"; then exit 18; fi", "writer-profile-probe"])
                .arg(source.join("src/lib.rs"))
                .arg(candidate.join("change.txt"))
                .arg(source.join(".git/test"))
                .arg(artifact.join("test"))
                .arg(&model)
                .arg(sibling_artifact.join("test"))
                .arg(gate_artifact.join("test"))
                .arg(cache.join("test"))
                .arg(sibling_cache.join("test"))
                .output().unwrap();
            assert!(
                output.status.success(),
                "Writer source-confinement child exited with {} (10: source write allowed; 11: candidate write denied; 12: Git write denied; 13: Task-artifact write denied; 14: Work-model write allowed; 15: sibling artifact write allowed; 16: gate artifact write allowed). stderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                std::fs::read_to_string(source.join("src/lib.rs")).unwrap(),
                "source"
            );
            assert_eq!(std::fs::read_to_string(&model).unwrap(), "host-owned");
        }
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
        if let Err(error) = preflight_profile_with(&profile, |profile_path| {
            Command::new(sandbox_exec_program())
                .args(["-f", profile_path.to_str().unwrap(), "/usr/bin/true"])
                .output()
        }) {
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
