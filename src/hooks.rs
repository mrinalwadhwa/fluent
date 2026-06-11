use anyhow::{Context, Result};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Kind of hook, inferred from the script name prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookKind {
    /// `check-*` — gate. Non-zero exit blocks the corresponding
    /// lifecycle transition.
    Check,
    /// `fix-*` — autofix. Run when the corresponding check failed.
    /// Non-zero exit means the fix itself failed.
    Fix,
    /// `post-*` — notification. Runs after a phase reaches its
    /// terminal state. Non-zero exit is logged but does not block.
    Post,
    /// Unknown prefix. Treated as a gate (`Check`) by default.
    Unknown,
}

/// Outcome of one hook invocation.
#[derive(Debug, Clone)]
pub struct HookOutcome {
    pub name: String,
    pub kind: HookKind,
    pub script_path: PathBuf,
    pub log_path: PathBuf,
    pub exit_code: i32,
    pub passed: bool,
}

/// Context Factory passes to every hook as environment variables.
/// Fields are optional because different lifecycle events have
/// different ids available.
#[derive(Debug, Clone, Default)]
pub struct HookContext {
    pub work_item_id: Option<String>,
    pub attempt_id: Option<String>,
    pub task_id: Option<String>,
    pub merge_candidate_id: Option<String>,
    pub candidate_commit: Option<String>,
    pub artifact_dir: Option<PathBuf>,
    /// Where to write `<hook-name>.log`. Created if missing.
    pub log_dir: PathBuf,
}

/// Locate the hook script. Returns `None` if `<project_root>/.factory/hooks/<name>`
/// does not exist or is not executable.
pub fn find_hook(project_root: &Path, name: &str) -> Option<PathBuf> {
    let path = project_root.join(".factory/hooks").join(name);
    if !path.is_file() {
        return None;
    }
    let meta = fs::metadata(&path).ok()?;
    if meta.permissions().mode() & 0o111 == 0 {
        return None;
    }
    Some(path)
}

/// Infer the hook kind from its name prefix.
pub fn infer_kind(name: &str) -> HookKind {
    if name.starts_with("check-") {
        HookKind::Check
    } else if name.starts_with("fix-") {
        HookKind::Fix
    } else if name.starts_with("post-") {
        HookKind::Post
    } else {
        HookKind::Unknown
    }
}

/// Run a hook by name. Returns `Ok(None)` if the hook is absent or
/// not executable; `Ok(Some(outcome))` after the hook runs. Captures
/// stdout and stderr to `<log_dir>/<name>.log`.
pub fn run_hook(
    project_root: &Path,
    name: &str,
    working_dir: &Path,
    context: &HookContext,
) -> Result<Option<HookOutcome>> {
    let Some(script_path) = find_hook(project_root, name) else {
        return Ok(None);
    };
    let kind = infer_kind(name);
    fs::create_dir_all(&context.log_dir).with_context(|| {
        format!(
            "Failed to create hook log directory {}",
            context.log_dir.display()
        )
    })?;
    let log_path = context.log_dir.join(format!("{name}.log"));
    let log_file = fs::File::create(&log_path).with_context(|| {
        format!("Failed to create hook log file {}", log_path.display())
    })?;
    let log_file_dup = log_file
        .try_clone()
        .context("Failed to clone hook log file handle")?;

    let mut cmd = Command::new(&script_path);
    cmd.current_dir(working_dir);
    cmd.env("FACTORY_HOOK", name);
    if let Some(id) = &context.work_item_id {
        cmd.env("FACTORY_WORK_ITEM_ID", id);
    }
    if let Some(id) = &context.attempt_id {
        cmd.env("FACTORY_ATTEMPT_ID", id);
    }
    if let Some(id) = &context.task_id {
        cmd.env("FACTORY_TASK_ID", id);
    }
    if let Some(id) = &context.merge_candidate_id {
        cmd.env("FACTORY_MERGE_CANDIDATE_ID", id);
    }
    if let Some(commit) = &context.candidate_commit {
        cmd.env("FACTORY_CANDIDATE_COMMIT", commit);
    }
    if let Some(dir) = &context.artifact_dir {
        cmd.env("FACTORY_ARTIFACT_DIR", dir.to_string_lossy().to_string());
    }
    cmd.stdout(Stdio::from(log_file));
    cmd.stderr(Stdio::from(log_file_dup));

    let status = cmd
        .status()
        .with_context(|| format!("Failed to launch hook script {}", script_path.display()))?;
    let exit_code = status.code().unwrap_or(1);
    let passed = exit_code == 0;

    Ok(Some(HookOutcome {
        name: name.to_string(),
        kind,
        script_path,
        log_path,
        exit_code,
        passed,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::OpenOptionsExt;
    use tempfile::TempDir;

    fn write_hook(project_root: &Path, name: &str, script: &str) -> PathBuf {
        let hooks_dir = project_root.join(".factory/hooks");
        fs::create_dir_all(&hooks_dir).unwrap();
        let path = hooks_dir.join(name);
        let mut opts = fs::OpenOptions::new();
        opts.create(true).write(true).truncate(true).mode(0o755);
        let mut file = opts.open(&path).unwrap();
        use std::io::Write;
        file.write_all(script.as_bytes()).unwrap();
        drop(file);
        path
    }

    #[test]
    fn infers_kind_from_name() {
        assert_eq!(infer_kind("check-pre-land"), HookKind::Check);
        assert_eq!(infer_kind("fix-pre-land"), HookKind::Fix);
        assert_eq!(infer_kind("post-land"), HookKind::Post);
        assert_eq!(infer_kind("verify-pre-land"), HookKind::Unknown);
    }

    #[test]
    fn returns_none_when_hook_is_absent() {
        let tmp = TempDir::new().unwrap();
        let result = find_hook(tmp.path(), "check-pre-land");
        assert!(result.is_none());
    }

    #[test]
    fn returns_none_when_hook_is_not_executable() {
        let tmp = TempDir::new().unwrap();
        let hooks_dir = tmp.path().join(".factory/hooks");
        fs::create_dir_all(&hooks_dir).unwrap();
        fs::write(hooks_dir.join("check-pre-land"), "#!/bin/sh\nexit 0\n").unwrap();
        let result = find_hook(tmp.path(), "check-pre-land");
        assert!(result.is_none());
    }

    #[test]
    fn runs_hook_and_captures_log() {
        let tmp = TempDir::new().unwrap();
        write_hook(
            tmp.path(),
            "check-pre-land",
            "#!/bin/sh\nprintf 'hello stdout\\n'\nprintf 'hello stderr\\n' >&2\nexit 0\n",
        );
        let log_dir = tmp.path().join("logs");
        let context = HookContext {
            log_dir: log_dir.clone(),
            ..Default::default()
        };
        let outcome = run_hook(tmp.path(), "check-pre-land", tmp.path(), &context)
            .unwrap()
            .unwrap();
        assert_eq!(outcome.kind, HookKind::Check);
        assert_eq!(outcome.exit_code, 0);
        assert!(outcome.passed);
        let log = fs::read_to_string(&outcome.log_path).unwrap();
        assert!(log.contains("hello stdout"));
        assert!(log.contains("hello stderr"));
    }

    #[test]
    fn reports_non_zero_exit() {
        let tmp = TempDir::new().unwrap();
        write_hook(
            tmp.path(),
            "check-pre-land",
            "#!/bin/sh\nprintf 'boom\\n' >&2\nexit 7\n",
        );
        let log_dir = tmp.path().join("logs");
        let context = HookContext {
            log_dir,
            ..Default::default()
        };
        let outcome = run_hook(tmp.path(), "check-pre-land", tmp.path(), &context)
            .unwrap()
            .unwrap();
        assert_eq!(outcome.exit_code, 7);
        assert!(!outcome.passed);
    }

    #[test]
    fn passes_context_via_env() {
        let tmp = TempDir::new().unwrap();
        write_hook(
            tmp.path(),
            "check-pre-land",
            "#!/bin/sh\nprintf 'work=%s attempt=%s task=%s candidate=%s commit=%s\\n' \
                \"$FACTORY_WORK_ITEM_ID\" \"$FACTORY_ATTEMPT_ID\" \"$FACTORY_TASK_ID\" \
                \"$FACTORY_MERGE_CANDIDATE_ID\" \"$FACTORY_CANDIDATE_COMMIT\"\nexit 0\n",
        );
        let log_dir = tmp.path().join("logs");
        let context = HookContext {
            work_item_id: Some("work-1".into()),
            attempt_id: Some("attempt-1".into()),
            task_id: Some("attempt-1-write".into()),
            merge_candidate_id: Some("cand-1".into()),
            candidate_commit: Some("abc123".into()),
            log_dir,
            ..Default::default()
        };
        let outcome = run_hook(tmp.path(), "check-pre-land", tmp.path(), &context)
            .unwrap()
            .unwrap();
        let log = fs::read_to_string(&outcome.log_path).unwrap();
        assert!(
            log.contains("work=work-1 attempt=attempt-1 task=attempt-1-write candidate=cand-1 commit=abc123"),
            "log was: {log}"
        );
    }
}
