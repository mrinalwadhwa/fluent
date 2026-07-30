use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Authentication failures that a user can recover by logging Codex in again.
#[derive(Debug)]
pub struct CodexAuthError {
    message: String,
}

impl std::fmt::Display for CodexAuthError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CodexAuthError {}

impl CodexAuthError {
    fn new(reason: impl std::fmt::Display) -> Self {
        Self {
            message: format!(
                "Codex authentication is unavailable: {reason}. Run `codex login`, then resume with `fluent attempt run`."
            ),
        }
    }
}

/// Private Codex state for one autonomous launch.
///
/// The temporary directory removes the copied authentication state when this
/// value drops. Only `auth.json` is staged; configuration, hooks, sessions,
/// logs, and cache remain in the interactive home.
pub struct CodexWorkerEnvironment {
    home_guard: tempfile::TempDir,
    home: PathBuf,
}

impl CodexWorkerEnvironment {
    /// Prepare a private worker home from the effective source `CODEX_HOME`.
    pub fn prepare() -> std::result::Result<Self, CodexAuthError> {
        #[cfg(feature = "test-support")]
        if env::var_os("FLUENT_TEST_HERMETIC_PROVIDERS").is_some() {
            return Self::prepare_hermetic_fixture_worker();
        }

        #[cfg(test)]
        {
            return Self::prepare_test_worker();
        }

        #[cfg(not(test))]
        Self::prepare_from_with_environment_auth(&effective_source_home(), has_environment_auth())
    }

    /// Give launch-route unit tests a private, authenticated worker home without
    /// reading the developer's interactive Codex state. External tests exercise
    /// the public production entry point with an explicit authentication source.
    #[cfg(test)]
    fn prepare_test_worker() -> std::result::Result<Self, CodexAuthError> {
        Self::prepare_hermetic_fixture_worker()
    }

    #[cfg(any(test, feature = "test-support"))]
    fn prepare_hermetic_fixture_worker() -> std::result::Result<Self, CodexAuthError> {
        let source = tempfile::tempdir().map_err(|error| {
            CodexAuthError::new(format!("cannot create test authentication source: {error}"))
        })?;
        fs::write(source.path().join("auth.json"), "test authentication").map_err(|error| {
            CodexAuthError::new(format!("cannot write test authentication source: {error}"))
        })?;
        Self::prepare_from_with_environment_auth(source.path(), false)
    }

    #[cfg(test)]
    fn prepare_from(source_home: &Path) -> std::result::Result<Self, CodexAuthError> {
        Self::prepare_from_with_environment_auth(source_home, false)
    }

    fn prepare_from_with_environment_auth(
        source_home: &Path,
        environment_auth: bool,
    ) -> std::result::Result<Self, CodexAuthError> {
        Self::prepare_from_with_environment_auth_in(source_home, environment_auth, None)
    }

    fn prepare_from_with_environment_auth_in(
        source_home: &Path,
        environment_auth: bool,
        temporary_root: Option<&Path>,
    ) -> std::result::Result<Self, CodexAuthError> {
        let mut builder = tempfile::Builder::new();
        builder.prefix("fluent-codex-worker-");
        let home_guard = match temporary_root {
            Some(root) => builder.tempdir_in(root),
            None => builder.tempdir(),
        }
        .map_err(|error| {
            CodexAuthError::new(format!("cannot create a private worker home: {error}"))
        })?;
        set_private_mode(home_guard.path(), 0o700).map_err(|error| {
            CodexAuthError::new(format!("cannot secure the worker home: {error}"))
        })?;
        let home = fs::canonicalize(home_guard.path()).map_err(|error| {
            CodexAuthError::new(format!("cannot resolve the worker home: {error}"))
        })?;

        if !environment_auth {
            let source_auth = source_home.join("auth.json");
            let worker_auth = home.join("auth.json");
            fs::copy(&source_auth, &worker_auth).map_err(|error| {
                CodexAuthError::new(format!(
                    "cannot copy authentication from {}: {error}",
                    source_auth.display()
                ))
            })?;
            set_private_mode(&worker_auth, 0o600).map_err(|error| {
                CodexAuthError::new(format!("cannot secure copied authentication: {error}"))
            })?;
        }

        Ok(Self { home_guard, home })
    }

    /// The only Codex state directory an autonomous launch may use.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Add this worker's home to a per-launch command environment.
    pub fn launch_env(&self) -> (String, String) {
        (
            "CODEX_HOME".to_string(),
            self.home.to_string_lossy().to_string(),
        )
    }

    /// Ask Codex itself whether this worker home is authenticated.
    pub fn preflight(&self) -> std::result::Result<(), CodexAuthError> {
        #[cfg(test)]
        {
            // Route unit tests inject a recording coder, not a Codex CLI. Keep
            // their focus on launch wiring; `preflight_with` tests the CLI
            // contract directly and external tests cover the production route.
            return Ok(());
        }

        #[cfg(not(test))]
        self.preflight_with("codex")
    }

    fn preflight_with(&self, binary: &str) -> std::result::Result<(), CodexAuthError> {
        let status = Command::new(binary)
            // The private worker home contains authentication only, so login
            // status cannot load interactive configuration or hooks. Keep
            // exec-only flags off this command: Codex does not accept
            // `--ignore-user-config` at the top-level login boundary.
            .args(["login", "status"])
            .env("CODEX_HOME", self.home())
            .status()
            .map_err(|error| {
                CodexAuthError::new(format!("cannot run `codex login status`: {error}"))
            })?;
        if status.success() {
            Ok(())
        } else {
            Err(CodexAuthError::new(
                "`codex login status` reports no valid login",
            ))
        }
    }
}

/// Return the interactive Codex home that supplies autonomous authentication.
pub fn effective_source_home() -> PathBuf {
    effective_source_home_from(
        env::var_os("CODEX_HOME").map(PathBuf::from),
        env::var_os("HOME").map(PathBuf::from),
    )
}

fn effective_source_home_from(codex_home: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    canonical_existing_path(
        codex_home
            .or_else(|| home.map(|home| home.join(".codex")))
            .unwrap_or_else(|| PathBuf::from(".codex")),
    )
}

fn canonical_existing_path(path: PathBuf) -> PathBuf {
    fs::canonicalize(&path).unwrap_or(path)
}

fn has_environment_auth() -> bool {
    [
        "OPENAI_API_KEY",
        "CODEX_API_KEY",
        "CODEX_ACCESS_TOKEN",
        "CODEX_AUTH_JSON",
    ]
    .iter()
    .any(|name| env::var_os(name).is_some_and(|value| !value.is_empty()))
}

#[cfg(unix)]
fn set_private_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("set permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_home_stages_only_auth_from_effective_codex_home() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("auth.json"), "auth").unwrap();
        fs::write(source.path().join("config.toml"), "hooks = true").unwrap();
        fs::create_dir(source.path().join("sessions")).unwrap();

        let worker = CodexWorkerEnvironment::prepare_from(source.path()).unwrap();

        assert_eq!(
            fs::read_to_string(worker.home().join("auth.json")).unwrap(),
            "auth"
        );
        assert!(!worker.home().join("config.toml").exists());
        assert!(!worker.home().join("sessions").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(worker.home()).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(worker.home().join("auth.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn worker_home_is_removed_after_launch_scope() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("auth.json"), "auth").unwrap();
        let worker = CodexWorkerEnvironment::prepare_from(source.path()).unwrap();
        let path = worker.home().to_path_buf();
        drop(worker);
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn worker_home_exposes_canonical_path_from_aliased_temp_root() {
        use std::os::unix::fs::symlink;

        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("auth.json"), "auth").unwrap();
        let real_root = tempfile::tempdir().unwrap();
        let alias_parent = tempfile::tempdir().unwrap();
        let alias = alias_parent.path().join("temporary-root");
        symlink(real_root.path(), &alias).unwrap();

        let worker = CodexWorkerEnvironment::prepare_from_with_environment_auth_in(
            source.path(),
            false,
            Some(&alias),
        )
        .unwrap();

        assert_eq!(worker.home(), fs::canonicalize(worker.home()).unwrap());
        assert!(!worker.home().starts_with(&alias));
        assert_eq!(worker.launch_env().1, worker.home().to_string_lossy());
    }

    #[test]
    fn canonical_existing_path_preserves_missing_path_spelling() {
        let missing = PathBuf::from("missing-codex-home");
        assert_eq!(canonical_existing_path(missing.clone()), missing);
    }

    #[cfg(unix)]
    #[test]
    fn effective_source_home_canonicalizes_an_existing_aliased_home() {
        use std::os::unix::fs::symlink;

        let real_home = tempfile::tempdir().unwrap();
        let alias_parent = tempfile::tempdir().unwrap();
        let alias = alias_parent.path().join("codex-home");
        symlink(real_home.path(), &alias).unwrap();

        assert_eq!(
            effective_source_home_from(Some(alias), None),
            fs::canonicalize(real_home.path()).unwrap()
        );
    }

    #[test]
    fn login_status_preflight_accepts_authenticated_worker_home() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("auth.json"), "auth").unwrap();
        let worker = CodexWorkerEnvironment::prepare_from(source.path()).unwrap();
        let fake = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            fake.path(),
            "#!/bin/sh\ntest \"$1 $2\" = 'login status'\ntest -z \"${3:-}\"\ntest -f \"$CODEX_HOME/auth.json\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        set_private_mode(fake.path(), 0o700).unwrap();

        worker
            .preflight_with(&fake.path().to_string_lossy())
            .unwrap();
    }

    #[test]
    fn preflight_failure_names_login_and_resume_actions() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("auth.json"), "auth").unwrap();
        let worker = CodexWorkerEnvironment::prepare_from(source.path()).unwrap();
        let fake = tempfile::NamedTempFile::new().unwrap();
        fs::write(fake.path(), "#!/bin/sh\nexit 1\n").unwrap();
        #[cfg(unix)]
        set_private_mode(fake.path(), 0o700).unwrap();

        let error = worker
            .preflight_with(&fake.path().to_string_lossy())
            .unwrap_err();
        assert!(error.to_string().contains("codex login"));
        assert!(error.to_string().contains("fluent attempt run"));
    }

    #[test]
    fn environment_auth_does_not_require_source_auth_file() {
        let source = tempfile::tempdir().unwrap();
        let worker =
            CodexWorkerEnvironment::prepare_from_with_environment_auth(source.path(), true)
                .unwrap();
        assert!(!worker.home().join("auth.json").exists());
    }
}
