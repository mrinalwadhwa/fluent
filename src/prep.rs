use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Toolchain {
    pub name: &'static str,
    pub marker_file: &'static str,
    pub dirs: &'static [&'static str],
}

pub const TOOLCHAINS: &[Toolchain] = &[
    Toolchain {
        name: "rust",
        marker_file: "Cargo.toml",
        dirs: &["target"],
    },
    Toolchain {
        name: "node",
        marker_file: "package.json",
        dirs: &["node_modules", "dist", ".next", "build"],
    },
    Toolchain {
        name: "maven",
        marker_file: "pom.xml",
        dirs: &["target"],
    },
    Toolchain {
        name: "gradle",
        marker_file: "build.gradle",
        dirs: &["build", ".gradle"],
    },
];

pub fn detect_toolchain(candidate_workspace: &Path) -> Option<&'static Toolchain> {
    TOOLCHAINS
        .iter()
        .find(|tc| candidate_workspace.join(tc.marker_file).exists())
}

pub fn populate_reviewer_cache(
    candidate: &Path,
    artifact_dir: &Path,
    toolchain: &Toolchain,
) -> Result<()> {
    for dir_name in toolchain.dirs {
        let src = candidate.join(dir_name);
        let metadata = match std::fs::symlink_metadata(&src) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("failed to inspect {}", src.display()));
            }
        };
        if metadata.file_type().is_symlink() {
            anyhow::bail!(
                "canonical build directory {} must not be a symlink",
                src.display()
            );
        }
        if !metadata.is_dir() {
            continue;
        }
        let dst = artifact_dir.join(dir_name);
        copy_dir_with_fallback(&src, &dst).with_context(|| {
            format!(
                "Failed to copy {} build directory {} to {}",
                toolchain.name,
                src.display(),
                dst.display()
            )
        })?;
    }
    Ok(())
}

/// Return the canonical build-cache directories present directly under `root`.
fn managed_cache_dirs(root: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    for name in TOOLCHAINS.iter().flat_map(|toolchain| toolchain.dirs) {
        let path = root.join(name);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
            }
        };
        if metadata.file_type().is_symlink() {
            anyhow::bail!(
                "canonical managed cache {} must not be a symlink",
                path.display()
            );
        }
        if metadata.is_dir() && !dirs.contains(&path) {
            dirs.push(path);
        }
    }
    Ok(dirs)
}

/// Sum regular-file lengths below canonical cache directories without following
/// symlinks. Metadata failures and arithmetic overflow fail closed.
pub fn managed_cache_bytes(root: &Path) -> Result<u64> {
    let mut total = 0_u64;
    for path in managed_cache_dirs(root)? {
        total = total
            .checked_add(directory_logical_bytes(&path)?)
            .ok_or_else(|| {
                anyhow::anyhow!("managed cache size overflow below {}", root.display())
            })?;
    }
    Ok(total)
}

pub fn toolchain_cache_bytes(root: &Path, toolchain: &Toolchain) -> Result<u64> {
    let mut total = 0_u64;
    for name in toolchain.dirs {
        let path = root.join(name);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
            }
        };
        if metadata.file_type().is_symlink() {
            anyhow::bail!(
                "canonical build directory {} must not be a symlink",
                path.display()
            );
        }
        if metadata.is_dir() {
            total = total
                .checked_add(directory_logical_bytes(&path)?)
                .ok_or_else(|| {
                    anyhow::anyhow!("build cache size overflow below {}", root.display())
                })?;
        }
    }
    Ok(total)
}

fn directory_logical_bytes(path: &Path) -> Result<u64> {
    let mut total = 0_u64;
    for entry in std::fs::read_dir(path)
        .with_context(|| format!("failed to read managed cache directory {}", path.display()))?
    {
        let entry = entry?;
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            total = total
                .checked_add(directory_logical_bytes(&entry.path())?)
                .ok_or_else(|| {
                    anyhow::anyhow!("managed cache size overflow below {}", path.display())
                })?;
        } else if metadata.is_file() {
            total = total.checked_add(metadata.len()).ok_or_else(|| {
                anyhow::anyhow!("managed cache size overflow below {}", path.display())
            })?;
        }
    }
    Ok(total)
}

/// Return free bytes on the filesystem that contains `path`.
pub fn filesystem_free_bytes(path: &Path) -> Result<u64> {
    let stat = rustix::fs::statvfs(path).map_err(|error| {
        anyhow::anyhow!(
            "failed to inspect free space at {}: {error}",
            path.display()
        )
    })?;
    u64::try_from(stat.f_bavail)
        .ok()
        .and_then(|blocks| blocks.checked_mul(u64::from(stat.f_frsize)))
        .ok_or_else(|| anyhow::anyhow!("free-space calculation overflow at {}", path.display()))
}

/// Remove only canonical managed cache directories, preserving every other
/// reviewer artifact.
pub fn remove_managed_cache_dirs(root: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    for name in TOOLCHAINS.iter().flat_map(|toolchain| toolchain.dirs) {
        let path = root.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() || metadata.file_type().is_symlink() => {
                if !dirs.contains(&path) {
                    dirs.push(path);
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
            }
        }
    }
    for path in &dirs {
        let result = if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
            std::fs::remove_file(path)
        } else {
            std::fs::remove_dir_all(path)
        };
        result.with_context(|| {
            format!("failed to remove managed reviewer cache {}", path.display())
        })?;
    }
    Ok(dirs)
}

fn copy_dir_with_fallback(src: &Path, dst: &Path) -> Result<()> {
    if cfg!(target_os = "macos") {
        if try_cp(src, dst, &["-cR"]) {
            return Ok(());
        }
    } else {
        if try_cp(src, dst, &["-R", "--reflink=auto"]) {
            return Ok(());
        }
    }
    if try_cp(src, dst, &["-lR"]) {
        return Ok(());
    }
    if try_cp(src, dst, &["-R"]) {
        return Ok(());
    }
    anyhow::bail!(
        "All copy strategies failed for {} -> {}",
        src.display(),
        dst.display()
    )
}

fn try_cp(src: &Path, dst: &Path, flags: &[&str]) -> bool {
    if dst.exists() {
        let _ = std::fs::remove_dir_all(dst);
    }
    Command::new("cp")
        .args(flags)
        .arg(src)
        .arg(dst)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn detects_rust_toolchain() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]").unwrap();
        let tc = detect_toolchain(tmp.path()).unwrap();
        assert_eq!(tc.name, "rust");
        assert_eq!(tc.dirs, &["target"]);
    }

    #[test]
    fn detects_node_toolchain() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("package.json"), "{}").unwrap();
        let tc = detect_toolchain(tmp.path()).unwrap();
        assert_eq!(tc.name, "node");
        assert_eq!(tc.dirs, &["node_modules", "dist", ".next", "build"]);
    }

    #[test]
    fn detects_maven_toolchain() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("pom.xml"), "<project/>").unwrap();
        let tc = detect_toolchain(tmp.path()).unwrap();
        assert_eq!(tc.name, "maven");
    }

    #[test]
    fn detects_gradle_toolchain() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("build.gradle"), "").unwrap();
        let tc = detect_toolchain(tmp.path()).unwrap();
        assert_eq!(tc.name, "gradle");
        assert_eq!(tc.dirs, &["build", ".gradle"]);
    }

    #[test]
    fn returns_none_when_no_marker() {
        let tmp = TempDir::new().unwrap();
        assert!(detect_toolchain(tmp.path()).is_none());
    }

    #[test]
    fn first_matching_toolchain_wins() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
        std::fs::write(tmp.path().join("package.json"), "").unwrap();
        let tc = detect_toolchain(tmp.path()).unwrap();
        assert_eq!(tc.name, "rust");
    }

    #[test]
    fn copies_existing_dirs_and_skips_missing() {
        let tmp = TempDir::new().unwrap();
        let candidate = tmp.path().join("candidate");
        let artifact = tmp.path().join("artifact");
        std::fs::create_dir_all(&candidate).unwrap();
        std::fs::create_dir_all(&artifact).unwrap();

        let target_dir = candidate.join("target");
        std::fs::create_dir_all(target_dir.join("debug")).unwrap();
        std::fs::write(target_dir.join("debug/fluent"), "binary").unwrap();

        let tc = Toolchain {
            name: "rust",
            marker_file: "Cargo.toml",
            dirs: &["target", "nonexistent"],
        };

        populate_reviewer_cache(&candidate, &artifact, &tc).unwrap();

        assert!(artifact.join("target/debug/fluent").is_file());
        assert_eq!(
            std::fs::read_to_string(artifact.join("target/debug/fluent")).unwrap(),
            "binary"
        );
        assert!(!artifact.join("nonexistent").exists());
    }

    #[test]
    fn copies_multiple_node_dirs() {
        let tmp = TempDir::new().unwrap();
        let candidate = tmp.path().join("candidate");
        let artifact = tmp.path().join("artifact");
        std::fs::create_dir_all(&candidate).unwrap();
        std::fs::create_dir_all(&artifact).unwrap();

        std::fs::create_dir_all(candidate.join("node_modules/pkg")).unwrap();
        std::fs::write(candidate.join("node_modules/pkg/index.js"), "module").unwrap();
        std::fs::create_dir_all(candidate.join("dist")).unwrap();
        std::fs::write(candidate.join("dist/bundle.js"), "bundle").unwrap();

        let tc = &TOOLCHAINS[1]; // node
        populate_reviewer_cache(&candidate, &artifact, tc).unwrap();

        assert!(artifact.join("node_modules/pkg/index.js").is_file());
        assert!(artifact.join("dist/bundle.js").is_file());
        assert!(!artifact.join(".next").exists());
        assert!(!artifact.join("build").exists());
    }

    #[test]
    fn no_error_when_all_dirs_missing() {
        let tmp = TempDir::new().unwrap();
        let candidate = tmp.path().join("candidate");
        let artifact = tmp.path().join("artifact");
        std::fs::create_dir_all(&candidate).unwrap();
        std::fs::create_dir_all(&artifact).unwrap();

        let tc = &TOOLCHAINS[0]; // rust — no target/ dir
        populate_reviewer_cache(&candidate, &artifact, tc).unwrap();
        assert!(!artifact.join("target").exists());
    }

    #[test]
    fn managed_cache_bytes_counts_regular_files_without_following_links() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("target/nested")).unwrap();
        std::fs::write(tmp.path().join("target/nested/output"), b"1234").unwrap();
        std::fs::write(tmp.path().join("notes"), b"preserved").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(tmp.path().join("notes"), tmp.path().join("target/link"))
            .unwrap();
        assert_eq!(managed_cache_bytes(tmp.path()).unwrap(), 4);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_canonical_cache_symlink() {
        let tmp = TempDir::new().unwrap();
        let external = tmp.path().join("external");
        std::fs::create_dir_all(&external).unwrap();
        std::os::unix::fs::symlink(&external, tmp.path().join("target")).unwrap();

        let error = managed_cache_bytes(tmp.path()).unwrap_err();
        assert!(error.to_string().contains("must not be a symlink"));
    }

    #[test]
    fn removes_only_canonical_managed_cache_dirs() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("target")).unwrap();
        std::fs::write(tmp.path().join("review.md"), "evidence").unwrap();
        remove_managed_cache_dirs(tmp.path()).unwrap();
        assert!(!tmp.path().join("target").exists());
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("review.md")).unwrap(),
            "evidence"
        );
    }
}
