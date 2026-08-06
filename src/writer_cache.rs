//! Attempt-scoped Cargo caches for autonomous Writer tasks.

use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use crate::work_model::WorkModelStore;

const WRITER_CACHE_RELATIVE_ROOT: &str = ".fluent/work/cache/writers";
const WRITER_CACHE_LOCK_RELATIVE_PATH: &str = ".fluent/work/locks/writer-cache.lock";
const ADMITTED_CARGO_DIRS: &[&str] = &["registry", "git"];

/// One private dependency cache shared by every Writer round in an exact
/// Work Item Attempt. It is rebuildable execution state, never durable evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WriterDependencyCache {
    project_root: PathBuf,
    root: PathBuf,
    cargo_home: PathBuf,
}

impl WriterDependencyCache {
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    #[cfg(test)]
    pub(crate) fn cargo_home(&self) -> &Path {
        &self.cargo_home
    }

    pub(crate) fn launch_env(&self) -> Vec<(String, String)> {
        vec![(
            "CARGO_HOME".to_string(),
            self.cargo_home.to_string_lossy().into_owned(),
        )]
    }
}

/// Resolve and validate the future cache path without creating it. This is safe
/// to run before a Writer Task reservation.
pub(crate) fn preflight_writer_dependency_cache(
    project_root: &Path,
    work_item_id: &str,
    attempt_id: &str,
) -> Result<WriterDependencyCache> {
    validate_cache_key("Work Item", work_item_id)?;
    validate_cache_key("Attempt", attempt_id)?;
    let root = writer_cache_root(project_root)
        .join(work_item_id)
        .join(attempt_id);
    reject_symlinked_existing_components(project_root, &root)?;
    Ok(WriterDependencyCache {
        project_root: project_root.to_path_buf(),
        cargo_home: root.join("cargo-home"),
        root,
    })
}

/// Materialize an exact-Attempt cache and seed only Cargo dependency stores.
/// Credentials, configuration, binaries, and every other user Cargo-home entry
/// remain outside the Writer boundary.
pub(crate) fn prepare_writer_dependency_cache(
    cache: &WriterDependencyCache,
    canonical_cargo_home: Option<&Path>,
) -> Result<()> {
    reject_symlinked_existing_components(&cache.project_root, &cache.cargo_home)?;
    fs::create_dir_all(&cache.cargo_home).with_context(|| {
        format!(
            "create private Writer Cargo home {}",
            cache.cargo_home.display()
        )
    })?;
    reject_symlinked_existing_components(&cache.project_root, &cache.cargo_home)?;

    if let Some(source_home) = canonical_cargo_home {
        for name in ADMITTED_CARGO_DIRS {
            seed_dependency_tree(source_home, &cache.cargo_home, name)?;
        }
    }
    validate_private_cache_symlinks(&cache.cargo_home, &cache.cargo_home)?;
    Ok(())
}

/// Reclaim stale caches and prepare one launch while holding the project cache
/// lock. Concurrent Writer Tasks cannot race a seed copy against reclamation.
pub(crate) fn prepare_writer_dependency_cache_for_launch(
    cache: &WriterDependencyCache,
    store: &WorkModelStore,
    canonical_cargo_home: Option<&Path>,
) -> Result<()> {
    let lock_path = cache.project_root.join(WRITER_CACHE_LOCK_RELATIVE_PATH);
    let _lease = crate::lease::acquire_blocking(&lock_path)
        .with_context(|| format!("acquire Writer cache lock {}", lock_path.display()))?;
    reclaim_terminal_writer_caches(&cache.project_root, store)?;
    prepare_writer_dependency_cache(cache, canonical_cargo_home)
}

/// Resolve the host Cargo home used only as an admitted seed source. Writers are
/// never granted this path.
pub(crate) fn canonical_cargo_home_from_env() -> Option<PathBuf> {
    #[cfg(any(test, feature = "test-support"))]
    {
        return std::env::var_os("FLUENT_TEST_CANONICAL_CARGO_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
    }
    #[cfg(not(any(test, feature = "test-support")))]
    {
        resolve_canonical_cargo_home(std::env::var_os("CARGO_HOME"), std::env::var_os("HOME"))
    }
}

#[cfg(not(any(test, feature = "test-support")))]
fn resolve_canonical_cargo_home(
    cargo_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    cargo_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .map(|home| home.join(".cargo"))
        })
}

/// Remove exact-Attempt caches not referenced by any nonterminal Work Item.
pub(crate) fn reclaim_terminal_writer_caches(
    project_root: &Path,
    store: &WorkModelStore,
) -> Result<()> {
    let mut active = HashSet::new();
    for item in store.list_work_items()? {
        if crate::cleanup::work_item_is_cleanable(&item) {
            continue;
        }
        for attempt in &item.attempts {
            active.insert((OsString::from(&item.id), OsString::from(&attempt.id)));
        }
    }

    let root = writer_cache_root(project_root);
    reject_symlinked_existing_components(project_root, &root)?;
    let work_entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("read Writer dependency cache root"),
    };
    for work_entry in work_entries {
        let work_entry = work_entry?;
        let work_path = work_entry.path();
        require_managed_directory(&work_path)?;
        for attempt_entry in fs::read_dir(&work_path)? {
            let attempt_entry = attempt_entry?;
            let attempt_path = attempt_entry.path();
            require_managed_directory(&attempt_path)?;
            let key = (work_entry.file_name(), attempt_entry.file_name());
            if !active.contains(&key) {
                fs::remove_dir_all(&attempt_path).with_context(|| {
                    format!(
                        "remove retired Writer dependency cache {}",
                        attempt_path.display()
                    )
                })?;
            }
        }
        if fs::read_dir(&work_path)?.next().is_none() {
            fs::remove_dir(&work_path).with_context(|| {
                format!(
                    "remove empty Writer cache directory {}",
                    work_path.display()
                )
            })?;
        }
    }
    Ok(())
}

fn writer_cache_root(project_root: &Path) -> PathBuf {
    project_root.join(WRITER_CACHE_RELATIVE_ROOT)
}

fn validate_cache_key(kind: &str, value: &str) -> Result<()> {
    let path = Path::new(value);
    if value.is_empty()
        || !matches!(
            path.components().collect::<Vec<_>>().as_slice(),
            [Component::Normal(_)]
        )
    {
        bail!("{kind} id {value:?} is not a safe Writer cache key");
    }
    Ok(())
}

fn reject_symlinked_existing_components(project_root: &Path, target: &Path) -> Result<()> {
    let relative = target.strip_prefix(project_root).with_context(|| {
        format!(
            "Writer cache path {} is outside project root {}",
            target.display(),
            project_root.display()
        )
    })?;
    let mut current = project_root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!(
                    "private Writer cache path must not be a symlink: {}",
                    current.display()
                );
            }
            Ok(metadata) if !metadata.is_dir() => {
                bail!(
                    "private Writer cache path must be a directory: {}",
                    current.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => break,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("inspect private Writer cache path {}", current.display())
                });
            }
        }
    }
    Ok(())
}

fn seed_dependency_tree(source_home: &Path, private_home: &Path, name: &str) -> Result<()> {
    let source = source_home.join(name);
    let destination = private_home.join(name);
    match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.is_dir() || metadata.file_type().is_symlink() => {
            return validate_private_cache_symlinks(private_home, &destination);
        }
        Ok(_) => {
            bail!(
                "private Writer dependency cache must be a directory: {}",
                destination.display()
            );
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "inspect private Writer dependency cache {}",
                    destination.display()
                )
            });
        }
    }
    let metadata = match fs::symlink_metadata(&source) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect admitted Cargo cache {}", source.display()));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "admitted Cargo dependency cache must be a directory, not a symlink: {}",
            source.display()
        );
    }
    crate::prep::copy_private_cache_dir(&source, &destination)?;
    if let Err(error) = validate_private_cache_symlinks(private_home, &destination) {
        let _ = fs::remove_dir_all(&destination);
        return Err(error);
    }
    Ok(())
}

fn validate_private_cache_symlinks(root: &Path, path: &Path) -> Result<()> {
    let canonical_root = fs::canonicalize(root)
        .with_context(|| format!("canonicalize private Writer cache {}", root.display()))?;
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect private Writer cache {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        let resolved = fs::canonicalize(path).with_context(|| {
            format!(
                "private Writer cache symlink must resolve inside its cache: {}",
                path.display()
            )
        })?;
        if !resolved.starts_with(&canonical_root) {
            bail!(
                "dependency symlink escapes private Writer cache: {} -> {}",
                path.display(),
                resolved.display()
            );
        }
    } else if !metadata.is_dir() {
        bail!(
            "private Writer dependency cache must be a directory: {}",
            path.display()
        );
    }
    validate_private_cache_symlinks_inner(&canonical_root, path)
}

fn validate_private_cache_symlinks_inner(canonical_root: &Path, path: &Path) -> Result<()> {
    for entry in fs::read_dir(path)
        .with_context(|| format!("read private Writer cache {}", path.display()))?
    {
        let entry = entry?;
        let entry_path = entry.path();
        let metadata = fs::symlink_metadata(&entry_path)?;
        if metadata.file_type().is_symlink() {
            let resolved = fs::canonicalize(&entry_path).with_context(|| {
                format!(
                    "private Writer cache symlink must resolve inside its cache: {}",
                    entry_path.display()
                )
            })?;
            if !resolved.starts_with(canonical_root) {
                bail!(
                    "dependency symlink escapes private Writer cache: {} -> {}",
                    entry_path.display(),
                    resolved.display()
                );
            }
        } else if metadata.is_dir() {
            validate_private_cache_symlinks_inner(canonical_root, &entry_path)?;
        }
    }
    Ok(())
}

fn require_managed_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "private Writer cache entry must be a directory, not a symlink: {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_model::{
        AttemptStatus, ReviewContext, TaskArtifactArea, TaskKind, TaskStatus, WorkItem,
        WorkModelStore, WorkspaceRef,
    };
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    #[test]
    fn writer_rounds_reuse_exact_attempt_cache_without_cross_attempt_sharing() {
        let tmp = TempDir::new().unwrap();

        let first = preflight_writer_dependency_cache(tmp.path(), "work-1", "attempt-1")
            .expect("first cache path is valid");
        let correction = preflight_writer_dependency_cache(tmp.path(), "work-1", "attempt-1")
            .expect("correction cache path is valid");
        let next_attempt = preflight_writer_dependency_cache(tmp.path(), "work-1", "attempt-2")
            .expect("next Attempt cache path is valid");

        assert_eq!(first, correction);
        assert_ne!(first.root(), next_attempt.root());
    }

    #[test]
    fn writer_dependency_cache_stays_outside_durable_artifacts() {
        let tmp = TempDir::new().unwrap();
        let cache = preflight_writer_dependency_cache(tmp.path(), "work-1", "attempt-1").unwrap();

        assert!(
            cache
                .root()
                .starts_with(tmp.path().join(".fluent/work/cache/writers"))
        );
        assert!(
            !cache
                .root()
                .starts_with(tmp.path().join(".fluent/work/artifacts"))
        );
    }

    #[test]
    fn unsafe_work_and_attempt_ids_cannot_select_cache_paths() {
        let tmp = TempDir::new().unwrap();

        for (work_item_id, attempt_id) in [
            ("../outside", "attempt-1"),
            ("work/child", "attempt-1"),
            ("work-1", "../outside"),
            ("work-1", "attempt/child"),
        ] {
            assert!(
                preflight_writer_dependency_cache(tmp.path(), work_item_id, attempt_id).is_err(),
                "unsafe pair {work_item_id:?}/{attempt_id:?} must be rejected"
            );
        }
    }

    #[test]
    fn preparation_admits_only_dependency_trees_from_the_canonical_cargo_home() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("canonical-cargo-home");
        fs::create_dir_all(source.join("registry/cache")).unwrap();
        fs::create_dir_all(source.join("git/db")).unwrap();
        fs::create_dir_all(source.join("bin")).unwrap();
        fs::write(source.join("registry/cache/crate"), "registry").unwrap();
        fs::write(source.join("git/db/dependency"), "git").unwrap();
        fs::write(source.join("credentials.toml"), "secret").unwrap();
        fs::write(source.join("config.toml"), "[net]").unwrap();
        fs::write(source.join("bin/tool"), "binary").unwrap();

        let cache = preflight_writer_dependency_cache(tmp.path(), "work-1", "attempt-1").unwrap();
        prepare_writer_dependency_cache(&cache, Some(&source)).unwrap();

        assert_eq!(
            fs::read_to_string(cache.cargo_home().join("registry/cache/crate")).unwrap(),
            "registry"
        );
        assert_eq!(
            fs::read_to_string(cache.cargo_home().join("git/db/dependency")).unwrap(),
            "git"
        );
        assert!(!cache.cargo_home().join("credentials.toml").exists());
        assert!(!cache.cargo_home().join("config.toml").exists());
        assert!(!cache.cargo_home().join("bin").exists());
    }

    #[test]
    fn private_dependency_writes_do_not_mutate_the_admitted_source() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("canonical-cargo-home");
        fs::create_dir_all(source.join("registry/cache")).unwrap();
        fs::write(source.join("registry/cache/crate"), "canonical").unwrap();
        let cache = preflight_writer_dependency_cache(tmp.path(), "work-1", "attempt-1").unwrap();
        prepare_writer_dependency_cache(&cache, Some(&source)).unwrap();

        fs::write(cache.cargo_home().join("registry/cache/crate"), "private").unwrap();

        assert_eq!(
            fs::read_to_string(source.join("registry/cache/crate")).unwrap(),
            "canonical"
        );
    }

    #[cfg(unix)]
    #[test]
    fn preflight_rejects_a_symlinked_managed_cache_ancestor() {
        let tmp = TempDir::new().unwrap();
        let external = tmp.path().join("external");
        fs::create_dir_all(&external).unwrap();
        fs::create_dir_all(tmp.path().join(".fluent/work/cache/writers")).unwrap();
        std::os::unix::fs::symlink(
            &external,
            tmp.path().join(".fluent/work/cache/writers/work-1"),
        )
        .unwrap();

        let error =
            preflight_writer_dependency_cache(tmp.path(), "work-1", "attempt-1").unwrap_err();
        assert!(format!("{error:#}").contains("must not be a symlink"));
    }

    #[cfg(unix)]
    #[test]
    fn preparation_rejects_a_dependency_symlink_that_escapes_the_private_cache() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("canonical-cargo-home");
        let external = tmp.path().join("external");
        fs::create_dir_all(source.join("registry")).unwrap();
        fs::create_dir_all(&external).unwrap();
        fs::write(external.join("secret"), "outside").unwrap();
        std::os::unix::fs::symlink(external.join("secret"), source.join("registry/escape"))
            .unwrap();

        let cache = preflight_writer_dependency_cache(tmp.path(), "work-1", "attempt-1").unwrap();
        let error = prepare_writer_dependency_cache(&cache, Some(&source)).unwrap_err();
        assert!(format!("{error:#}").contains("escapes private Writer cache"));
    }

    #[cfg(unix)]
    #[test]
    fn preparation_rejects_a_cache_leaf_symlink_before_writing_through_it() {
        let tmp = TempDir::new().unwrap();
        let external = tmp.path().join("external");
        fs::create_dir_all(&external).unwrap();
        let cache = preflight_writer_dependency_cache(tmp.path(), "work-1", "attempt-1").unwrap();
        fs::create_dir_all(cache.root()).unwrap();
        std::os::unix::fs::symlink(&external, cache.cargo_home()).unwrap();

        let error = prepare_writer_dependency_cache(&cache, None).unwrap_err();

        assert!(format!("{error:#}").contains("must not be a symlink"));
        assert!(fs::read_dir(&external).unwrap().next().is_none());
    }

    #[test]
    fn reclamation_preserves_a_cache_referenced_by_nonterminal_work() {
        let tmp = TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let active = work_item("active", AttemptStatus::Executing, TaskStatus::Executing);
        store.write_work_item(&active).unwrap();

        let active_cache =
            preflight_writer_dependency_cache(tmp.path(), "active", "attempt-1").unwrap();
        prepare_writer_dependency_cache(&active_cache, None).unwrap();

        reclaim_terminal_writer_caches(tmp.path(), &store).unwrap();

        assert!(active_cache.root().is_dir());
    }

    #[test]
    fn reclamation_retires_a_cache_after_work_becomes_terminal() {
        let tmp = TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let terminal = work_item("terminal", AttemptStatus::Complete, TaskStatus::Complete);
        store.write_work_item(&terminal).unwrap();
        let terminal_cache =
            preflight_writer_dependency_cache(tmp.path(), "terminal", "attempt-1").unwrap();
        prepare_writer_dependency_cache(&terminal_cache, None).unwrap();

        reclaim_terminal_writer_caches(tmp.path(), &store).unwrap();

        assert!(!terminal_cache.root().exists());
    }

    #[cfg(unix)]
    #[test]
    fn reclamation_rejects_a_symlink_without_touching_its_target() {
        let tmp = TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let external = tmp.path().join("external");
        fs::create_dir_all(&external).unwrap();
        fs::write(external.join("preserved"), "outside").unwrap();
        fs::create_dir_all(tmp.path().join(WRITER_CACHE_RELATIVE_ROOT)).unwrap();
        std::os::unix::fs::symlink(
            &external,
            tmp.path().join(WRITER_CACHE_RELATIVE_ROOT).join("escaped"),
        )
        .unwrap();

        let error = reclaim_terminal_writer_caches(tmp.path(), &store).unwrap_err();

        assert!(format!("{error:#}").contains("not a symlink"));
        assert_eq!(
            fs::read_to_string(external.join("preserved")).unwrap(),
            "outside"
        );
    }

    #[test]
    fn an_admitted_git_dependency_builds_offline_from_the_private_cache() {
        let tmp = TempDir::new().unwrap();
        let dependency = tmp.path().join("dependency");
        let app = tmp.path().join("app");
        let seed_home = tmp.path().join("canonical-cargo-home");
        fs::create_dir_all(dependency.join("src")).unwrap();
        fs::write(
            dependency.join("Cargo.toml"),
            "[package]\nname = \"cached-dependency\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::write(
            dependency.join("src/lib.rs"),
            "pub fn value() -> u8 { 7 }\n",
        )
        .unwrap();
        crate::git::run(
            &dependency,
            &["init", "-q"],
            "initialize dependency fixture",
        )
        .unwrap();
        crate::git::run(
            &dependency,
            &["config", "user.name", "Test Writer"],
            "configure dependency fixture name",
        )
        .unwrap();
        crate::git::run(
            &dependency,
            &["config", "user.email", "writer@example.invalid"],
            "configure dependency fixture email",
        )
        .unwrap();
        crate::git::run(&dependency, &["add", "."], "stage dependency fixture").unwrap();
        crate::git::run(
            &dependency,
            &["commit", "-q", "-m", "seed dependency"],
            "commit dependency fixture",
        )
        .unwrap();

        fs::create_dir_all(app.join("src")).unwrap();
        let dependency_url = format!("file://{}", dependency.display());
        fs::write(
            app.join("Cargo.toml"),
            format!(
                "[package]\nname = \"offline-app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\ncached-dependency = {{ git = {dependency_url:?} }}\n"
            ),
        )
        .unwrap();
        fs::write(
            app.join("src/lib.rs"),
            "#[test]\nfn uses_cached_dependency() { assert_eq!(cached_dependency::value(), 7); }\n",
        )
        .unwrap();
        run(Command::new("cargo")
            .args(["generate-lockfile"])
            .env("CARGO_HOME", &seed_home)
            .current_dir(&app));

        let cache = preflight_writer_dependency_cache(tmp.path(), "work-1", "attempt-1").unwrap();
        prepare_writer_dependency_cache(&cache, Some(&seed_home)).unwrap();
        fs::remove_dir_all(&dependency).unwrap();

        let mut command = Command::new("cargo");
        command
            .args(["test", "--offline"])
            .env("CARGO_HOME", cache.cargo_home())
            .current_dir(&app);
        run(&mut command);
    }

    fn work_item(id: &str, attempt_status: AttemptStatus, task_status: TaskStatus) -> WorkItem {
        let mut item = WorkItem {
            id: id.to_string(),
            title: id.to_string(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();
        item.attempts[0].status = attempt_status;
        item.attempts[0].tasks[0].status = task_status;
        if item.attempts[0].tasks[0].status == TaskStatus::Complete {
            let task = &mut item.attempts[0].tasks[0];
            task.kind = TaskKind::Review;
            task.workspace_access.writes.clear();
            task.workspace_access.reads = vec![WorkspaceRef {
                id: "candidate".to_string(),
                path: "candidate".to_string(),
            }];
            task.review_context = Some(ReviewContext {
                candidate_workspace_id: "candidate".to_string(),
                candidate_workspace_path: "candidate".to_string(),
                source_branch: "main".to_string(),
                candidate_commit: "abc123".to_string(),
                base_commit: None,
            });
            task.artifact_area = Some(TaskArtifactArea {
                path: crate::work_model::work_artifact_path(id, "attempt-1", &task.id),
            });
        }
        item
    }

    fn run(command: &mut Command) {
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "command failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
