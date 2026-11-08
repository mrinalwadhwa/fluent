use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Component, Path, PathBuf};

const SCHEMA_VERSION: u32 = 1;
pub(crate) const SIDECAR_NAME: &str = ".fluent-managed.json";
const SHIM_MARKER: &str = "fluent-shim: true";
const FLUENT_SOURCE: &str = "mrinalwadhwa/fluent";
const FLUENT_SKILL_PATH: &str = "skills/fluent/SKILL.md";
/// Exact digest of the bundled Fluent skill released immediately before
/// provenance sidecars were introduced. Keep this bounded: an unlisted copy
/// remains user-owned until the operator removes it.
const KNOWN_PRIOR_FLUENT_BUNDLE_DIGESTS: &[&str] =
    &["37076758c949e0701fa33a09e525aacd81021c4317c7f6f4212825b4f25982d0"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallOutcome {
    Installed,
    Updated,
    Current,
    ReplacedShim,
    ReplacedLegacy,
    Conflict,
}

#[derive(Debug, Serialize, Deserialize)]
struct ManagedSkill {
    schema_version: u32,
    fluent_version: String,
    agent: String,
    scope: String,
    skill: String,
    bundle_sha256: String,
    files: Vec<String>,
}

/// Report whether sidecar bytes claim Fluent ownership of the enclosing skill
/// tree. The source-change guard recognizes installs written by a different
/// Fluent version, so it checks only the stable identity fields rather than the
/// bundle digest and inventory.
pub(crate) fn sidecar_claims_ownership(bytes: &[u8]) -> bool {
    #[derive(Deserialize)]
    struct OwnershipClaim {
        schema_version: u32,
        agent: String,
        scope: String,
        skill: String,
    }
    match serde_json::from_slice::<OwnershipClaim>(bytes) {
        Ok(claim) => {
            claim.schema_version >= SCHEMA_VERSION
                && !claim.agent.is_empty()
                && !claim.scope.is_empty()
                && !claim.skill.is_empty()
        }
        Err(_) => false,
    }
}

/// A prepared handoff from the external `skills` CLI to Fluent ownership.
/// The original bytes allow the caller to restore the lock when installation
/// fails after the lock entry has been removed.
pub struct ExternalSkillLockHandoff {
    path: PathBuf,
    original: Vec<u8>,
    updated: Vec<u8>,
}

impl ExternalSkillLockHandoff {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn apply(&self) -> Result<()> {
        crate::atomic_write::atomic_write(&self.path, &self.updated).with_context(|| {
            format!(
                "Failed to update external skill lock {}",
                self.path.display()
            )
        })
    }

    pub fn restore(&self) -> Result<()> {
        crate::atomic_write::atomic_write(&self.path, &self.original).with_context(|| {
            format!(
                "Failed to restore external skill lock {}",
                self.path.display()
            )
        })
    }
}

/// Prepare removal of Fluent's bootstrap entry from a `skills` CLI lock.
/// Unrelated skills and top-level metadata remain in the parsed document.
pub fn prepare_external_skill_lock_handoff(
    lock_path: &Path,
) -> Result<Option<ExternalSkillLockHandoff>> {
    let original = match fs::read(lock_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("Failed to read external skill lock {}", lock_path.display())
            });
        }
    };
    let mut value: serde_json::Value = serde_json::from_slice(&original).with_context(|| {
        format!(
            "Invalid external skill lock {}; Fluent preserved the bootstrap shim",
            lock_path.display()
        )
    })?;
    let Some(skills_value) = value.get_mut("skills") else {
        return Ok(None);
    };
    let Some(skills) = skills_value.as_object_mut() else {
        anyhow::bail!(
            "Invalid external skill lock {}; Fluent preserved the bootstrap shim",
            lock_path.display()
        );
    };
    let Some(entry) = skills.get("fluent") else {
        return Ok(None);
    };
    let matches_fluent = entry.get("source").and_then(serde_json::Value::as_str)
        == Some(FLUENT_SOURCE)
        && entry.get("skillPath").and_then(serde_json::Value::as_str) == Some(FLUENT_SKILL_PATH);
    if !matches_fluent {
        anyhow::bail!(
            "external skill lock {} has a non-Fluent or unrecognized `fluent` entry; Fluent preserved the bootstrap shim",
            lock_path.display()
        );
    }
    skills.remove("fluent");
    let mut updated = serde_json::to_vec_pretty(&value)?;
    updated.push(b'\n');
    Ok(Some(ExternalSkillLockHandoff {
        path: lock_path.to_path_buf(),
        original,
        updated,
    }))
}

/// Install one bundled skill without replacing an installation Fluent cannot own.
pub fn install_bundled_skill(
    skill: &str,
    skills_dir: &Path,
    agent: &str,
    scope: &str,
) -> Result<InstallOutcome> {
    install_bundled_skill_with_legacy_digests(
        skill,
        skills_dir,
        agent,
        scope,
        KNOWN_PRIOR_FLUENT_BUNDLE_DIGESTS,
    )
}

/// Return whether Fluent owns a complete installation at this identity.
pub fn is_managed_bundled_skill(
    skill: &str,
    skills_dir: &Path,
    agent: &str,
    scope: &str,
) -> Result<bool> {
    is_valid_managed(&skills_dir.join(skill), skill, agent, scope)
}

fn install_bundled_skill_with_legacy_digests(
    skill: &str,
    skills_dir: &Path,
    agent: &str,
    scope: &str,
    legacy_fluent_bundle_digests: &[&str],
) -> Result<InstallOutcome> {
    let files = bundled_files(skill)?;
    let skill_dir = skills_dir.join(skill);

    let outcome = if !skill_dir.exists() {
        InstallOutcome::Installed
    } else if is_current_managed(&skill_dir, skill, agent, scope, &files)? {
        return Ok(InstallOutcome::Current);
    } else if is_valid_managed(&skill_dir, skill, agent, scope)? {
        fs::remove_dir_all(&skill_dir).with_context(|| {
            format!("Failed to replace managed skill at {}", skill_dir.display())
        })?;
        InstallOutcome::Updated
    } else if !has_sidecar(&skill_dir)?
        && is_known_prior_bundle(&skill_dir, skill, legacy_fluent_bundle_digests)?
    {
        fs::remove_dir_all(&skill_dir).with_context(|| {
            format!(
                "Failed to replace prior Fluent skill at {}",
                skill_dir.display()
            )
        })?;
        InstallOutcome::ReplacedLegacy
    } else if !has_sidecar(&skill_dir)? && skill == "fluent" && is_fluent_shim(&skill_dir) {
        fs::remove_dir_all(&skill_dir)
            .with_context(|| format!("Failed to replace Fluent shim at {}", skill_dir.display()))?;
        InstallOutcome::ReplacedShim
    } else {
        return Ok(InstallOutcome::Conflict);
    };

    write_bundle(&skill_dir, skill, agent, scope, &files)?;
    Ok(outcome)
}

fn bundled_files(skill: &str) -> Result<Vec<(&'static str, &'static str)>> {
    let prefix = format!("{skill}/");
    let files = crate::content::bundled_skill_files_under(&prefix);
    if files.is_empty() {
        anyhow::bail!("No bundled skill named {skill:?}");
    }
    Ok(files)
}

fn write_bundle(
    skill_dir: &Path,
    skill: &str,
    agent: &str,
    scope: &str,
    files: &[(&str, &str)],
) -> Result<()> {
    let root = skill_dir.parent().expect("skill directory has a parent");
    for (relative, content) in files {
        let relative = bundled_relative_path(skill, relative)?;
        let path = skill_dir.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create skill directory {}", parent.display())
            })?;
        }
        crate::atomic_write::atomic_write(&path, content.as_bytes())
            .with_context(|| format!("Failed to write skill at {}", path.display()))?;
    }

    let sidecar = ManagedSkill {
        schema_version: SCHEMA_VERSION,
        fluent_version: env!("CARGO_PKG_VERSION").to_string(),
        agent: agent.to_string(),
        scope: scope.to_string(),
        skill: skill.to_string(),
        bundle_sha256: bundled_digest(skill, files),
        files: files
            .iter()
            .map(|(path, _)| {
                bundled_relative_path(skill, path).map(|path| path.display().to_string())
            })
            .collect::<Result<Vec<_>>>()?,
    };
    let sidecar_path = skill_dir.join(SIDECAR_NAME);
    let bytes = serde_json::to_vec_pretty(&sidecar)?;
    crate::atomic_write::atomic_write(&sidecar_path, &bytes)
        .with_context(|| format!("Failed to write provenance at {}", sidecar_path.display()))?;
    debug_assert!(root.exists());
    Ok(())
}

fn is_current_managed(
    skill_dir: &Path,
    skill: &str,
    agent: &str,
    scope: &str,
    files: &[(&str, &str)],
) -> Result<bool> {
    let Some(sidecar) = read_valid_sidecar(skill_dir, skill, agent, scope)? else {
        return Ok(false);
    };
    if sidecar.bundle_sha256 != bundled_digest(skill, files)
        || sidecar.files.len() != files.len()
        || sidecar.files
            != files
                .iter()
                .map(|(path, _)| {
                    bundled_relative_path(skill, path).map(|path| path.display().to_string())
                })
                .collect::<Result<Vec<_>>>()?
    {
        return Ok(false);
    }
    Ok(files.iter().all(|(path, content)| {
        bundled_relative_path(skill, path)
            .ok()
            .and_then(|path| fs::read(skill_dir.join(path)).ok())
            .as_deref()
            == Some(content.as_bytes())
    }))
}

fn is_valid_managed(skill_dir: &Path, skill: &str, agent: &str, scope: &str) -> Result<bool> {
    Ok(read_valid_sidecar(skill_dir, skill, agent, scope)?.is_some())
}

/// Preserve any sidecar-bearing directory unless the sidecar validates its
/// ownership. Legacy migration applies only to installations that predate
/// provenance sidecars entirely.
fn has_sidecar(skill_dir: &Path) -> Result<bool> {
    match fs::symlink_metadata(skill_dir.join(SIDECAR_NAME)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error)
            .with_context(|| format!("Failed to inspect sidecar in {}", skill_dir.display())),
    }
}

fn is_known_prior_bundle(skill_dir: &Path, skill: &str, legacy_digests: &[&str]) -> Result<bool> {
    if skill != "fluent" {
        return Ok(false);
    }
    let mut files = Vec::new();
    if !collect_bundle_files(skill_dir, skill_dir, &mut files)? {
        return Ok(false);
    }
    if files.is_empty() {
        return Ok(false);
    }
    files.sort();
    let digest = digest_paths(&files, |path| fs::read(skill_dir.join(path)))?;
    Ok(legacy_digests.contains(&digest.as_str()))
}

fn collect_bundle_files(root: &Path, dir: &Path, files: &mut Vec<String>) -> Result<bool> {
    for entry in fs::read_dir(dir).with_context(|| format!("Failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .expect("bundle entry remains below root");
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if !collect_bundle_files(root, &path, files)? {
                return Ok(false);
            }
        } else if file_type.is_file() {
            let relative = relative.display().to_string();
            if relative != SIDECAR_NAME && is_safe_relative_path(&relative) {
                files.push(relative);
            }
        } else {
            return Ok(false);
        }
    }
    Ok(true)
}

fn read_valid_sidecar(
    skill_dir: &Path,
    skill: &str,
    agent: &str,
    scope: &str,
) -> Result<Option<ManagedSkill>> {
    let sidecar_path = skill_dir.join(SIDECAR_NAME);
    let bytes = match fs::read(&sidecar_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Failed to read {}", sidecar_path.display()));
        }
    };
    let sidecar = match serde_json::from_slice::<ManagedSkill>(&bytes) {
        Ok(sidecar) => sidecar,
        Err(_) => return Ok(None),
    };
    if sidecar.schema_version != SCHEMA_VERSION
        || sidecar.agent != agent
        || sidecar.scope != scope
        || sidecar.skill != skill
        || sidecar.files.is_empty()
        || sidecar
            .files
            .iter()
            .any(|path| !is_safe_relative_path(path))
        || !has_exact_managed_inventory(skill_dir, &sidecar.files)?
    {
        return Ok(None);
    }
    let digest = digest_paths(&sidecar.files, |path| fs::read(skill_dir.join(path)));
    let is_valid = digest.ok().as_deref() == Some(&sidecar.bundle_sha256);
    Ok(is_valid.then_some(sidecar))
}

/// Confirm that the sidecar accounts for every file and directory in a
/// managed skill. Any unlisted entry makes the directory user-owned.
fn has_exact_managed_inventory(skill_dir: &Path, sidecar_files: &[String]) -> Result<bool> {
    let mut expected_files = sidecar_files.to_vec();
    expected_files.sort();
    expected_files.dedup();
    if expected_files.len() != sidecar_files.len() {
        return Ok(false);
    }

    let mut actual_files = Vec::new();
    let mut actual_dirs = Vec::new();
    if !collect_managed_inventory(skill_dir, skill_dir, &mut actual_files, &mut actual_dirs)? {
        return Ok(false);
    }
    actual_files.sort();
    actual_dirs.sort();

    let mut expected_dirs = Vec::new();
    for file in &expected_files {
        let mut parent = Path::new(file).parent();
        while let Some(dir) = parent {
            if dir.as_os_str().is_empty() {
                break;
            }
            expected_dirs.push(dir.display().to_string());
            parent = dir.parent();
        }
    }
    expected_dirs.sort();
    expected_dirs.dedup();

    Ok(actual_files == expected_files && actual_dirs == expected_dirs)
}

fn collect_managed_inventory(
    root: &Path,
    dir: &Path,
    files: &mut Vec<String>,
    dirs: &mut Vec<String>,
) -> Result<bool> {
    for entry in fs::read_dir(dir).with_context(|| format!("Failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .expect("managed skill entry remains below root");
        let relative = relative.display().to_string();
        if !is_safe_relative_path(&relative) {
            return Ok(false);
        }
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            dirs.push(relative);
            if !collect_managed_inventory(root, &path, files, dirs)? {
                return Ok(false);
            }
        } else if file_type.is_file() {
            if relative != SIDECAR_NAME {
                files.push(relative);
            }
        } else {
            return Ok(false);
        }
    }
    Ok(true)
}

fn bundled_digest(skill: &str, files: &[(&str, &str)]) -> String {
    digest_paths(
        &files
            .iter()
            .map(|(path, _)| {
                bundled_relative_path(skill, path).map(|path| path.display().to_string())
            })
            .collect::<Result<Vec<_>>>()
            .expect("bundled paths are valid"),
        |path| {
            files
                .iter()
                .find(|(candidate, _)| {
                    bundled_relative_path(skill, candidate)
                        .map(|candidate| candidate == Path::new(path))
                        .unwrap_or(false)
                })
                .map(|(_, content)| content.as_bytes().to_vec())
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, path))
        },
    )
    .expect("bundled files are readable")
}

fn digest_paths<F>(paths: &[String], mut read: F) -> std::io::Result<String>
where
    F: FnMut(&str) -> std::io::Result<Vec<u8>>,
{
    let mut hasher = Sha256::new();
    for path in paths {
        let bytes = read(path)?;
        hasher.update(path.as_bytes());
        hasher.update([0]);
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn bundled_relative_path(skill: &str, bundled_path: &str) -> Result<PathBuf> {
    bundled_path
        .strip_prefix(&format!("{skill}/"))
        .filter(|path| is_safe_relative_path(path))
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("invalid bundled skill path {bundled_path:?}"))
}

fn is_safe_relative_path(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

pub fn is_fluent_shim(skill_dir: &Path) -> bool {
    fs::read_to_string(skill_dir.join("SKILL.md"))
        .map(|content| {
            let mut lines = content.lines();
            if lines.next().map(str::trim) != Some("---") {
                return false;
            }
            lines
                .take_while(|line| line.trim() != "---")
                .any(|line| line.trim() == SHIM_MARKER)
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn production_prior_bundle_digest_remains_allowlisted() {
        assert_eq!(
            KNOWN_PRIOR_FLUENT_BUNDLE_DIGESTS,
            &["37076758c949e0701fa33a09e525aacd81021c4317c7f6f4212825b4f25982d0"],
            "the production migration allowlist must retain the prior release digest"
        );
    }

    #[test]
    fn adopts_an_exact_allowlisted_bundle_and_rejects_a_near_match() {
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        let skill_dir = skills_dir.join("fluent");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "exact prior bundle").unwrap();
        let mut files = Vec::new();
        assert!(collect_bundle_files(&skill_dir, &skill_dir, &mut files).unwrap());
        files.sort();
        let digest = digest_paths(&files, |path| fs::read(skill_dir.join(path))).unwrap();

        let outcome = install_bundled_skill_with_legacy_digests(
            "fluent",
            &skills_dir,
            "claude",
            "global",
            &[&digest],
        )
        .unwrap();
        assert_eq!(outcome, InstallOutcome::ReplacedLegacy);
        assert!(skill_dir.join(".fluent-managed.json").is_file());
        assert_ne!(
            fs::read_to_string(skill_dir.join("SKILL.md")).unwrap(),
            "exact prior bundle"
        );

        let near_match_dir = skills_dir.join("near-match");
        fs::create_dir_all(&near_match_dir).unwrap();
        fs::write(near_match_dir.join("SKILL.md"), "near prior bundle").unwrap();
        assert!(
            !is_known_prior_bundle(&near_match_dir, "fluent", &[&digest]).unwrap(),
            "only the exact bounded digest can be adopted"
        );
    }

    #[test]
    fn does_not_adopt_allowlisted_bundle_when_sidecar_is_present() {
        for case in ["malformed", "identity-mismatched"] {
            let tmp = TempDir::new().unwrap();
            let skills_dir = tmp.path().join("skills");
            let skill_dir = skills_dir.join("fluent");
            fs::create_dir_all(&skill_dir).unwrap();
            fs::write(skill_dir.join("SKILL.md"), "exact prior bundle").unwrap();
            let files = vec!["SKILL.md".to_string()];
            let digest = digest_paths(&files, |path| fs::read(skill_dir.join(path))).unwrap();
            let sidecar = if case == "malformed" {
                b"not json\n".to_vec()
            } else {
                serde_json::to_vec(&ManagedSkill {
                    schema_version: SCHEMA_VERSION,
                    fluent_version: "earlier-release".to_string(),
                    agent: "codex".to_string(),
                    scope: "global".to_string(),
                    skill: "fluent".to_string(),
                    bundle_sha256: digest.clone(),
                    files: files.clone(),
                })
                .unwrap()
            };
            fs::write(skill_dir.join(SIDECAR_NAME), sidecar).unwrap();
            let before = fs::read_dir(&skill_dir)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    (entry.file_name(), fs::read(entry.path()).unwrap())
                })
                .collect::<Vec<_>>();

            let outcome = install_bundled_skill_with_legacy_digests(
                "fluent",
                &skills_dir,
                "claude",
                "global",
                &[&digest],
            )
            .unwrap();

            assert_eq!(outcome, InstallOutcome::Conflict, "{case} sidecar");
            let after = fs::read_dir(&skill_dir)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    (entry.file_name(), fs::read(entry.path()).unwrap())
                })
                .collect::<Vec<_>>();
            assert_eq!(after, before);
        }
    }

    #[test]
    fn external_lock_handoff_removes_only_the_fluent_entry() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("skills-lock.json");
        let original = serde_json::json!({
            "version": 1,
            "metadata": {"keep": true},
            "skills": {
                "fluent": {
                    "source": FLUENT_SOURCE,
                    "sourceType": "github",
                    "skillPath": FLUENT_SKILL_PATH,
                    "computedHash": "shim-hash"
                },
                "other": {
                    "source": "owner/other",
                    "skillPath": "skills/other/SKILL.md"
                }
            }
        });
        fs::write(&path, serde_json::to_vec_pretty(&original).unwrap()).unwrap();

        let handoff = prepare_external_skill_lock_handoff(&path)
            .unwrap()
            .expect("matching Fluent entry should prepare a handoff");
        handoff.apply().unwrap();

        let updated: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(updated["version"], original["version"]);
        assert_eq!(updated["metadata"], original["metadata"]);
        assert_eq!(updated["skills"]["other"], original["skills"]["other"]);
        assert!(updated["skills"].get("fluent").is_none());

        handoff.restore().unwrap();
        let restored: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn external_lock_handoff_preserves_unrecognized_or_invalid_locks() {
        for contents in [
            b"not json\n".as_slice(),
            br#"{"version":1,"skills":[]}"#,
            br#"{"version":1,"skills":{"fluent":{"source":"other/fluent","skillPath":"skills/fluent/SKILL.md"}}}"#,
        ] {
            let tmp = TempDir::new().unwrap();
            let path = tmp.path().join("skills-lock.json");
            fs::write(&path, contents).unwrap();
            let before = fs::read(&path).unwrap();

            assert!(prepare_external_skill_lock_handoff(&path).is_err());
            assert_eq!(fs::read(&path).unwrap(), before);
        }
    }

    #[test]
    fn fluent_shim_marker_must_be_in_frontmatter() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("fluent");
        fs::create_dir_all(&skill_dir).unwrap();

        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: fluent\n---\nDocumentation mentions fluent-shim: true.\n",
        )
        .unwrap();
        assert!(!is_fluent_shim(&skill_dir));

        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: fluent\nfluent-shim: true\n---\nBootstrap.\n",
        )
        .unwrap();
        assert!(is_fluent_shim(&skill_dir));
    }
}
