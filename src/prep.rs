use anyhow::{Context, Result};
use std::path::Path;
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
        if !src.is_dir() {
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
        std::fs::write(target_dir.join("debug/factory"), "binary").unwrap();

        let tc = Toolchain {
            name: "rust",
            marker_file: "Cargo.toml",
            dirs: &["target", "nonexistent"],
        };

        populate_reviewer_cache(&candidate, &artifact, &tc).unwrap();

        assert!(artifact.join("target/debug/factory").is_file());
        assert_eq!(
            std::fs::read_to_string(artifact.join("target/debug/factory")).unwrap(),
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
}
