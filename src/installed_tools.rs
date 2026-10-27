//! Resolve installed Node tools separately from mutable worker dependencies.

use anyhow::{Context, Result, ensure};
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Default)]
pub(crate) struct NodeToolAccess {
    pub(crate) roots: Vec<PathBuf>,
    pub(crate) search_directories: Vec<PathBuf>,
}

/// Admit the selected installation only for a Node project. Do not replace a
/// broken first PATH entry with a different installation farther down PATH.
pub(crate) fn node_access(project: &Path, search_path: Option<&OsStr>) -> Result<NodeToolAccess> {
    if !project.join("package.json").is_file() {
        return Ok(NodeToolAccess::default());
    }
    let search_path = search_path.context("Node prerequisite: PATH is unavailable")?;
    let mut roots = Vec::new();
    for (tool, script) in [
        ("node", None),
        ("npm", Some("npm-cli.js")),
        ("npx", Some("npx-cli.js")),
    ] {
        let launcher = std::env::split_paths(search_path)
            .map(|directory| directory.join(tool))
            .find(|path| fs::symlink_metadata(path).is_ok())
            .with_context(|| {
                format!("Node prerequisite: selected {tool} is unavailable on PATH")
            })?;
        ensure!(
            launcher.is_absolute(),
            "Node prerequisite: {tool} must use an absolute PATH entry: {}",
            launcher.display()
        );
        let target = fs::canonicalize(&launcher).with_context(|| {
            format!(
                "Node prerequisite: resolve selected {tool} launcher {}",
                launcher.display()
            )
        })?;
        let metadata = fs::metadata(&target)?;
        ensure!(
            metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
            "Node prerequisite: selected {tool} launcher {} must resolve to an executable file: {}",
            launcher.display(),
            target.display()
        );
        if let Some(script) = script {
            ensure!(
                target.file_name() == Some(OsStr::new(script))
                    && target.parent().and_then(Path::file_name) == Some(OsStr::new("bin")),
                "Node prerequisite: unsupported {tool} launcher {} resolves to {}; select an installed npm bin/{script}",
                launcher.display(),
                target.display()
            );
            let package = target
                .parent()
                .and_then(Path::parent)
                .context("Node prerequisite: npm package has no root")?;
            if !roots.iter().any(|root| root == package) {
                validate_package_tree(package, package)?;
                let manifest = package.join("package.json");
                let data: serde_json::Value =
                    serde_json::from_slice(&fs::read(&manifest).with_context(|| {
                        format!(
                            "Node prerequisite: read npm manifest {}",
                            manifest.display()
                        )
                    })?)
                    .with_context(|| {
                        format!(
                            "Node prerequisite: invalid npm manifest {}",
                            manifest.display()
                        )
                    })?;
                ensure!(
                    data.get("name").and_then(|v| v.as_str()) == Some("npm"),
                    "Node prerequisite: selected {tool} target {} is not an npm package",
                    target.display()
                );
                roots.push(package.to_path_buf());
            }
        }
        roots.push(launcher);
        roots.push(target);
    }
    roots.sort();
    roots.dedup();
    // libuv stops searching PATH on EPERM. Permit directory traversal so a
    // missing command yields ENOENT and lookup can reach the selected tool.
    // Do not admit the contents of these unselected tool directories.
    let mut search_directories = Vec::new();
    for entry in std::env::split_paths(search_path).filter(|path| path.is_absolute()) {
        for directory in entry.ancestors().filter(|path| path.parent().is_some()) {
            if directory.is_dir() {
                search_directories.push(directory.to_path_buf());
            }
        }
        search_directories.extend(search_directory_traversal(&entry));
    }
    search_directories.sort();
    search_directories.dedup();
    Ok(NodeToolAccess {
        roots,
        search_directories,
    })
}

/// Retain each directory and symlink visited during PATH lookup. Canonicalizing
/// only the endpoint loses intermediate profile links and their parents.
fn search_directory_traversal(path: &Path) -> Vec<PathBuf> {
    let mut pending: VecDeque<OsString> = path
        .components()
        .map(|component| component.as_os_str().to_owned())
        .collect();
    let mut resolved = PathBuf::new();
    let mut directories = Vec::new();
    // Bound cycles and concurrent link changes in unselected PATH entries.
    let mut links_remaining = 40;
    while let Some(component) = pending.pop_front() {
        match component.to_str() {
            Some("/") => {
                resolved = PathBuf::from("/");
                continue;
            }
            Some(".") => continue,
            Some("..") => {
                resolved.pop();
                continue;
            }
            _ => resolved.push(component),
        }
        let Ok(metadata) = fs::symlink_metadata(&resolved) else {
            break;
        };
        if metadata.file_type().is_symlink() {
            directories.push(resolved.clone());
            if links_remaining == 0 {
                break;
            }
            links_remaining -= 1;
            let Ok(target) = fs::read_link(&resolved) else {
                break;
            };
            resolved.pop();
            for component in target.components().rev() {
                pending.push_front(component.as_os_str().to_owned());
            }
        } else if metadata.is_dir() {
            directories.push(resolved.clone());
        } else {
            break;
        }
    }
    directories
}

/// A package grant must not turn an internal link into access to another
/// installation or private configuration. Walk physical directories only.
fn validate_package_tree(root: &Path, directory: &Path) -> Result<()> {
    for entry in fs::read_dir(directory).with_context(|| {
        format!(
            "Node prerequisite: inspect npm package {}",
            directory.display()
        )
    })? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            let target = path.canonicalize().with_context(|| {
                format!(
                    "Node prerequisite: resolve npm package link {}",
                    path.display()
                )
            })?;
            ensure!(
                target.starts_with(root),
                "Node prerequisite: npm package link {} escapes {} to {}",
                path.display(),
                root.display(),
                target.display()
            );
        } else if kind.is_dir() {
            validate_package_tree(root, &path)?;
        } else {
            ensure!(
                kind.is_file(),
                "Node prerequisite: unsupported npm package entry {}",
                path.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn search_directory_traversal_resolves_links_before_parent_components() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let profiles = root.join("profiles");
        let aliases = root.join("aliases");
        let release = root.join("store/release");
        for directory in [
            &profiles,
            &aliases.join("bin"),
            &release.join("share"),
            &release.join("bin"),
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        symlink(release.join("share"), aliases.join("active")).unwrap();
        symlink("../aliases/active/../bin", profiles.join("current")).unwrap();
        fs::write(release.join("bin/unrelated"), "private contents").unwrap();

        let directories = search_directory_traversal(&profiles.join("current"));
        for expected in [
            profiles.join("current"),
            aliases.clone(),
            aliases.join("active"),
            root.join("store"),
            release.clone(),
            release.join("share"),
            release.join("bin"),
        ] {
            assert!(
                directories.contains(&expected),
                "missing {}",
                expected.display()
            );
        }
        assert!(!directories.contains(&aliases.join("bin")));
        assert!(!directories.contains(&release.join("bin/unrelated")));
    }

    #[test]
    fn search_directory_traversal_bounds_cycles_and_stops_at_unavailable_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        symlink("loop-b", root.join("loop-a")).unwrap();
        symlink("loop-a", root.join("loop-b")).unwrap();
        symlink("missing/bin", root.join("dangling")).unwrap();
        fs::write(root.join("file"), "not a directory").unwrap();

        let cycle = search_directory_traversal(&root.join("loop-a/bin"));
        assert!(cycle.contains(&root.join("loop-a")));
        assert!(cycle.contains(&root.join("loop-b")));
        assert!(cycle.len() <= root.ancestors().count() + 41);
        assert!(!cycle.contains(&root.join("loop-a/bin")));
        let dangling = search_directory_traversal(&root.join("dangling"));
        assert!(dangling.contains(&root.join("dangling")));
        assert!(!dangling.contains(&root.join("missing")));
        assert_eq!(
            search_directory_traversal(&root.join("file/bin")),
            search_directory_traversal(&root),
        );
    }
}
