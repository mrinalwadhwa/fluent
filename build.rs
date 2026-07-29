use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

fn main() {
    print_git_rerun_paths();

    let commit = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|stdout| stdout.trim().to_string())
        .filter(|commit| !commit.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=FLUENT_BUILD_COMMIT={commit}");

    generate_bundled_skills();
    generate_skill_migrations();
}

/// Embed exact historical bundle identities that may be adopted without a
/// managed-installation record.
fn generate_skill_migrations() {
    let migrations_dir = PathBuf::from("skill-migrations");
    println!("cargo:rerun-if-changed={}", migrations_dir.display());
    let fixture_dir = migrations_dir.join("v0.1.4");
    let mut entries = Vec::new();

    if fixture_dir.is_dir() {
        let mut skills = fs::read_dir(&fixture_dir)
            .expect("failed to read skill migration fixture")
            .collect::<Result<Vec<_>, _>>()
            .expect("failed to iterate skill migration fixture");
        skills.sort_by_key(|entry| entry.file_name());
        for skill in skills {
            if !skill
                .file_type()
                .expect("failed to inspect skill fixture")
                .is_dir()
            {
                continue;
            }
            let name = skill.file_name().to_string_lossy().to_string();
            let mut files = Vec::new();
            collect_migration_files(&skill.path(), &skill.path(), &mut files);
            assert!(
                !files.is_empty(),
                "migration fixture {name} must not be empty"
            );
            files.sort_by(|left, right| left.0.cmp(&right.0));
            let mut hasher = Sha256::new();
            for (path, bytes) in files {
                hasher.update(path.as_bytes());
                hasher.update([0]);
                hasher.update((bytes.len() as u64).to_be_bytes());
                hasher.update(bytes);
            }
            entries.push((name, format!("{:x}", hasher.finalize())));
        }
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let dest = out_dir.join("skill_migrations.rs");
    let mut code = String::from("/// Historical skill bundles eligible for exact migration.\n");
    code.push_str("pub const BUNDLED_SKILL_MIGRATION_DIGESTS: &[(&str, &str)] = &[\n");
    for (skill, digest) in entries {
        code.push_str("    (");
        write_rust_str_literal(&mut code, &skill);
        code.push_str(", ");
        write_rust_str_literal(&mut code, &digest);
        code.push_str("),\n");
    }
    code.push_str("];\n");
    fs::write(dest, code).expect("failed to write skill_migrations.rs");
}

fn collect_migration_files(root: &Path, dir: &Path, files: &mut Vec<(String, Vec<u8>)>) {
    let mut entries = fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", dir.display()))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| panic!("failed to iterate {}: {error}", dir.display()));
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .expect("migration entry remains below fixture")
            .to_string_lossy()
            .replace('\\', "/");
        let file_type = entry
            .file_type()
            .expect("failed to inspect migration entry");
        if file_type.is_dir() {
            collect_migration_files(root, &path, files);
        } else if file_type.is_file() {
            files.push((
                relative,
                fs::read(&path).expect("failed to read migration file"),
            ));
        } else {
            panic!(
                "migration fixture contains non-regular entry: {}",
                path.display()
            );
        }
    }
}

fn print_git_rerun_paths() {
    for args in [
        ["rev-parse", "--git-path", "HEAD"].as_slice(),
        ["rev-parse", "--git-path", "index"].as_slice(),
    ] {
        if let Some(path) = git_stdout(args) {
            println!("cargo:rerun-if-changed={path}");
        }
    }

    if let Some(head_ref) = git_stdout(["symbolic-ref", "-q", "HEAD"].as_slice())
        && let Some(path) = git_stdout(["rev-parse", "--git-path", &head_ref].as_slice())
    {
        println!("cargo:rerun-if-changed={path}");
    }
}

fn git_stdout(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|stdout| stdout.trim().to_string())
        .filter(|stdout| !stdout.is_empty())
}

fn generate_bundled_skills() {
    let skills_dir = PathBuf::from("skills");
    println!("cargo:rerun-if-changed=skills");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let dest = out_dir.join("bundled_skills.rs");

    let mut entries: Vec<(String, String)> = Vec::new();

    // Collect names that have a `.full/` override so we can skip their
    // plain directories (the shim) when bundling.
    let full_overrides = find_full_overrides(&skills_dir);

    if skills_dir.is_dir() {
        collect_skill_files(&skills_dir, &skills_dir, &full_overrides, &mut entries);
    }

    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut code = String::new();
    code.push_str("/// Bundled skill files generated by build.rs.\n");
    code.push_str("/// Each entry is (relative_path, file_content).\n");
    code.push_str("pub const BUNDLED_SKILL_FILES: &[(&str, &str)] = &[\n");
    for (rel_path, content) in &entries {
        code.push_str("    (");
        write_rust_str_literal(&mut code, rel_path);
        code.push_str(", ");
        write_rust_str_literal(&mut code, content);
        code.push_str("),\n");
    }
    code.push_str("];\n");

    fs::write(&dest, code).expect("failed to write bundled_skills.rs");
}

/// Scan `skills/` for `<name>.full/` directories. Returns the set of `name`
/// strings that have a `.full/` override.
fn find_full_overrides(skills_dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let entries = match fs::read_dir(skills_dir) {
        Ok(e) => e,
        Err(_) => return names,
    };
    for entry in entries.flatten() {
        let fname = entry.file_name();
        let fname = fname.to_string_lossy();
        if fname.ends_with(".full") && entry.path().is_dir() {
            names.push(fname.trim_end_matches(".full").to_string());
        }
    }
    names
}

fn collect_skill_files(
    base: &Path,
    dir: &Path,
    full_overrides: &[String],
    entries: &mut Vec<(String, String)>,
) {
    let mut dir_entries: Vec<fs::DirEntry> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", dir.display()))
        .collect::<Result<_, _>>()
        .unwrap_or_else(|e| panic!("failed to iterate {}: {e}", dir.display()));
    dir_entries.sort_by_key(|e| e.file_name());

    for entry in dir_entries {
        let path = entry.path();
        let metadata = fs::metadata(&path)
            .unwrap_or_else(|e| panic!("failed to read metadata for {}: {e}", path.display()));

        if metadata.is_dir() {
            // Skip a top-level `<name>/` directory when `<name>.full/` exists,
            // so the shim is never bundled.
            if dir == base {
                let dir_name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                if !dir_name.ends_with(".full") && full_overrides.iter().any(|n| *n == dir_name) {
                    continue;
                }
            }

            // A `.full/` directory maps its contents into `<name>/`:
            //   `<name>.full/<name>.md`       → `<name>/SKILL.md`
            //   `<name>.full/references/...`  → `<name>/references/...`
            if dir == base {
                let dir_name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                if let Some(skill_name) = dir_name.strip_suffix(".full") {
                    collect_full_override_files(skill_name, &path, entries);
                    continue;
                }
            }

            collect_skill_files(base, &path, full_overrides, entries);
        } else if metadata.is_file() {
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let content = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
            entries.push((rel, content));
        }
    }
}

/// Collect files from a `<name>.full/` directory, mapping them into
/// `<name>/` paths for bundling.
fn collect_full_override_files(
    skill_name: &str,
    full_dir: &Path,
    entries: &mut Vec<(String, String)>,
) {
    // Map `<name>.full/<name>.md` → `<name>/SKILL.md`
    let main_file = full_dir.join(format!("{skill_name}.md"));
    if main_file.is_file() {
        let content = fs::read_to_string(&main_file)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", main_file.display()));
        entries.push((format!("{skill_name}/SKILL.md"), content));
    }

    // Map `<name>.full/references/*` → `<name>/references/*`
    let refs_dir = full_dir.join("references");
    if refs_dir.is_dir() {
        collect_files_recursively(&refs_dir, &format!("{skill_name}/references"), entries);
    }
}

fn collect_files_recursively(dir: &Path, prefix: &str, entries: &mut Vec<(String, String)>) {
    let mut dir_entries: Vec<fs::DirEntry> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", dir.display()))
        .collect::<Result<_, _>>()
        .unwrap_or_else(|e| panic!("failed to iterate {}: {e}", dir.display()));
    dir_entries.sort_by_key(|e| e.file_name());

    for entry in dir_entries {
        let path = entry.path();
        let fname = path.file_name().unwrap_or_default().to_string_lossy();
        if path.is_dir() {
            collect_files_recursively(&path, &format!("{prefix}/{fname}"), entries);
        } else if path.is_file() {
            let content = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
            entries.push((format!("{prefix}/{fname}"), content));
        }
    }
}

fn write_rust_str_literal(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
}
