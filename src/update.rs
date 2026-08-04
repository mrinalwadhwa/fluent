use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_RELEASE_REPO: &str = "mrinalwadhwa/fluent";
const DEFAULT_UPDATE_ENDPOINT: &str = "https://fluent.computer/latest";
const CHECK_INTERVAL_SECS: u64 = 86400;
const CURL_CONNECT_TIMEOUT: u32 = 5;
const CURL_MAX_TIME_CHECK: u32 = 10;
const CURL_MAX_TIME_DOWNLOAD: u32 = 300;
const CACHE_FILE_NAME: &str = "update-check.json";

fn release_repo() -> String {
    std::env::var("FLUENT_RELEASE_REPO").unwrap_or_else(|_| DEFAULT_RELEASE_REPO.to_string())
}

fn release_url() -> String {
    if let Ok(base) = std::env::var("FLUENT_API_BASE") {
        let repo = release_repo();
        format!("{base}/repos/{repo}/releases/latest")
    } else {
        DEFAULT_UPDATE_ENDPOINT.to_string()
    }
}

fn user_agent() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let os = std::env::consts::OS;
    format!("fluent/{version} ({os})")
}

fn config_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".config/fluent")
    } else {
        PathBuf::from("/tmp/fluent-config")
    }
}

fn cache_path() -> PathBuf {
    std::env::var("FLUENT_UPDATE_CACHE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| config_dir().join(CACHE_FILE_NAME))
}

fn target_triple() -> String {
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "macos" => format!("{arch}-apple-darwin"),
        "linux" => format!("{arch}-unknown-linux-gnu"),
        other => format!("{arch}-{other}"),
    }
}

fn binary_path() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("FLUENT_BINARY_PATH") {
        return Ok(PathBuf::from(path));
    }
    std::env::current_exe().context("Failed to determine current binary path")
}

// -------------------------------------------------------------------------
// Version comparison
// -------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.strip_prefix('v').unwrap_or(s);
        let parts: Vec<&str> = s.splitn(3, '.').collect();
        if parts.len() != 3 {
            return None;
        }
        if parts[2].contains('-') {
            return None;
        }
        Some(Version {
            major: parts[0].parse().ok()?,
            minor: parts[1].parse().ok()?,
            patch: parts[2].parse().ok()?,
        })
    }

    pub fn current() -> Self {
        Version::parse(env!("CARGO_PKG_VERSION")).expect("CARGO_PKG_VERSION is valid semver")
    }

    pub fn is_behind(&self, latest: &Version) -> bool {
        (latest.major, latest.minor, latest.patch) > (self.major, self.minor, self.patch)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

// -------------------------------------------------------------------------
// GitHub release query
// -------------------------------------------------------------------------

#[derive(Debug)]
struct LatestRelease {
    version: Version,
    asset_name: String,
    asset_url: Option<String>,
    checksum_url: Option<String>,
}

fn query_latest_release() -> Result<LatestRelease> {
    let url = release_url();
    let triple = target_triple();
    let asset_name = format!("fluent-{triple}");

    let ua = user_agent();
    let output = Command::new("curl")
        .args([
            "--silent",
            "--fail",
            "--location",
            "--connect-timeout",
            &CURL_CONNECT_TIMEOUT.to_string(),
            "--max-time",
            &CURL_MAX_TIME_CHECK.to_string(),
            "-H",
            &format!("User-Agent: {ua}"),
            &url,
        ])
        .output()
        .context("Failed to run curl")?;

    if !output.status.success() {
        bail!(
            "Failed to query release source (exit {})",
            output.status.code().unwrap_or(-1)
        );
    }

    let body =
        String::from_utf8(output.stdout).context("Release API response is not valid UTF-8")?;
    let json: serde_json::Value =
        serde_json::from_str(&body).context("Failed to parse release API response")?;

    let tag = json["tag_name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Release response missing tag_name"))?
        .to_string();

    let version = Version::parse(&tag)
        .ok_or_else(|| anyhow::anyhow!("Release tag {tag:?} is not valid semver"))?;

    let assets = json["assets"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Release response missing assets array"))?;

    let asset_url = assets
        .iter()
        .find(|a| a["name"].as_str() == Some(&asset_name))
        .and_then(|a| a["browser_download_url"].as_str())
        .map(str::to_string);

    let checksum_name = format!("{asset_name}.sha256");
    let checksum_url = assets
        .iter()
        .find(|a| a["name"].as_str() == Some(&checksum_name))
        .and_then(|a| a["browser_download_url"].as_str())
        .map(str::to_string);

    Ok(LatestRelease {
        version,
        asset_name,
        asset_url,
        checksum_url,
    })
}

// -------------------------------------------------------------------------
// Download and checksum
// -------------------------------------------------------------------------

fn download_file(url: &str, dest: &Path, max_time: u32) -> Result<()> {
    let status = Command::new("curl")
        .args([
            "--silent",
            "--fail",
            "--location",
            "--connect-timeout",
            &CURL_CONNECT_TIMEOUT.to_string(),
            "--max-time",
            &max_time.to_string(),
            "--output",
            &dest.to_string_lossy(),
            url,
        ])
        .status()
        .context("Failed to run curl for download")?;

    if !status.success() {
        bail!("Download failed (exit {})", status.code().unwrap_or(-1));
    }
    Ok(())
}

fn expected_checksum(contents: &str, asset_name: &str) -> Result<String> {
    let mut fields = contents.split_whitespace();
    let digest = fields
        .next()
        .ok_or_else(|| anyhow::anyhow!("Release checksum is empty"))?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("Release checksum is not a SHA-256 digest");
    }
    if let Some(name) = fields.next() {
        let name = name.strip_prefix('*').unwrap_or(name);
        if name != asset_name {
            bail!("Release checksum names {name:?}, expected asset {asset_name:?}");
        }
    }
    if fields.next().is_some() {
        bail!("Release checksum contains unexpected fields");
    }
    Ok(digest.to_ascii_lowercase())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)
        .with_context(|| format!("Failed to open downloaded binary {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("Failed to read downloaded binary {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn verify_checksum(binary: &Path, checksum: &Path, asset_name: &str) -> Result<()> {
    let checksum_contents = fs::read_to_string(checksum)
        .with_context(|| format!("Failed to read release checksum {}", checksum.display()))?;
    let expected = expected_checksum(&checksum_contents, asset_name)?;
    let actual = sha256_file(binary)?;
    if actual != expected {
        bail!("Release checksum mismatch for {asset_name}");
    }
    Ok(())
}

// -------------------------------------------------------------------------
// Atomic self-replace
// -------------------------------------------------------------------------

fn atomic_replace(src: &Path, dest: &Path) -> Result<()> {
    fs::set_permissions(src, fs::Permissions::from_mode(0o755))
        .context("Failed to set executable permission on downloaded binary")?;

    fs::rename(src, dest).context("Failed to atomically replace binary")?;
    Ok(())
}

// -------------------------------------------------------------------------
// Update check cache
// -------------------------------------------------------------------------

#[derive(serde::Serialize, serde::Deserialize)]
struct UpdateCache {
    checked_at: u64,
    latest_version: String,
}

fn read_cache() -> Option<UpdateCache> {
    let path = cache_path();
    let content = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

fn write_cache(latest_version: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let cache = UpdateCache {
        checked_at: now,
        latest_version: latest_version.to_string(),
    };
    let path = cache_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&path, serde_json::to_string(&cache).unwrap_or_default());
}

fn cache_is_fresh(cache: &UpdateCache) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    now.saturating_sub(cache.checked_at) < CHECK_INTERVAL_SECS
}

// -------------------------------------------------------------------------
// Nudge (update check on invocation)
// -------------------------------------------------------------------------

pub fn maybe_nudge() {
    if std::env::var("FLUENT_NO_UPDATE_CHECK").is_ok() {
        return;
    }

    let current = Version::current();

    if let Some(cache) = read_cache() {
        if cache_is_fresh(&cache) {
            if let Some(latest) = Version::parse(&cache.latest_version) {
                if current.is_behind(&latest) {
                    print_nudge(&latest);
                }
            }
            return;
        }
    }

    let release = match query_latest_release() {
        Ok(r) => r,
        Err(_) => return,
    };

    write_cache(&release.version.to_string());

    if current.is_behind(&release.version) {
        print_nudge(&release.version);
    }
}

fn print_nudge(latest: &Version) {
    eprintln!("A newer fluent is available ({latest}) — run `fluent update`");
}

// -------------------------------------------------------------------------
// Perform update
// -------------------------------------------------------------------------

pub fn perform_update() -> Result<()> {
    let bin = binary_path()?;
    let current = Version::current();

    eprintln!("Checking for updates...");

    let release = query_latest_release().context("Failed to reach the release source")?;

    write_cache(&release.version.to_string());

    if !current.is_behind(&release.version) {
        eprintln!("fluent is up to date ({})", current);
        return Ok(());
    }

    eprintln!("Updating fluent {} → {} ...", current, release.version);
    let asset_url = release
        .asset_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Release has no asset named {:?}", release.asset_name))?;
    let checksum_name = format!("{}.sha256", release.asset_name);
    let checksum_url = release
        .checksum_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Release has no asset named {checksum_name:?}"))?;

    let parent = bin
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Binary path has no parent directory"))?;
    let binary_tmp = tempfile::Builder::new()
        .prefix(".fluent-update-")
        .tempfile_in(parent)
        .context("Failed to create a temporary binary beside the installation")?;
    let checksum_tmp = tempfile::Builder::new()
        .prefix(".fluent-checksum-")
        .tempfile_in(parent)
        .context("Failed to create a temporary checksum beside the installation")?;
    let tmp_path = binary_tmp.path().to_path_buf();
    let checksum_tmp_path = checksum_tmp.path().to_path_buf();

    let download_result = (|| -> Result<()> {
        download_file(asset_url, &tmp_path, CURL_MAX_TIME_DOWNLOAD)
            .context("Failed to download the release binary")?;

        download_file(checksum_url, &checksum_tmp_path, CURL_MAX_TIME_DOWNLOAD)
            .context("Failed to download the release checksum")?;

        verify_checksum(&tmp_path, &checksum_tmp_path, &release.asset_name)
            .context("Failed to verify the release checksum")?;

        atomic_replace(&tmp_path, &bin)?;
        Ok(())
    })();

    if let Err(e) = &download_result {
        return Err(anyhow::anyhow!("{:#}", e));
    }

    rematerialize_skills(&bin);

    eprintln!("Updated to fluent {}", release.version);
    Ok(())
}

fn rematerialize_skills(new_binary: &Path) {
    let result = Command::new(new_binary).args(["skills", "add"]).status();

    match result {
        Ok(status) if status.success() => {}
        Ok(status) => {
            eprintln!(
                "warning: skills re-materialization exited with {}",
                status.code().unwrap_or(-1)
            );
        }
        Err(e) => {
            eprintln!("warning: failed to re-materialize skills: {e}");
        }
    }
}

// -------------------------------------------------------------------------
// Tests
// -------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parse_plain_semver() {
        let v = Version::parse("1.2.3").unwrap();
        assert_eq!((v.major, v.minor, v.patch), (1, 2, 3));
    }

    #[test]
    fn version_parse_with_v_prefix() {
        let v = Version::parse("v0.5.10").unwrap();
        assert_eq!((v.major, v.minor, v.patch), (0, 5, 10));
    }

    #[test]
    fn version_parse_rejects_prerelease() {
        assert!(Version::parse("1.0.0-rc1").is_none());
    }

    #[test]
    fn version_parse_rejects_two_parts() {
        assert!(Version::parse("1.0").is_none());
    }

    #[test]
    fn version_parse_rejects_non_numeric() {
        assert!(Version::parse("a.b.c").is_none());
    }

    #[test]
    fn version_is_behind_when_latest_is_greater() {
        let current = Version::parse("0.1.0").unwrap();
        let latest = Version::parse("0.2.0").unwrap();
        assert!(current.is_behind(&latest));
    }

    #[test]
    fn version_is_not_behind_when_equal() {
        let current = Version::parse("0.1.0").unwrap();
        let latest = Version::parse("0.1.0").unwrap();
        assert!(!current.is_behind(&latest));
    }

    #[test]
    fn version_is_not_behind_when_ahead() {
        let current = Version::parse("1.0.0").unwrap();
        let latest = Version::parse("0.9.0").unwrap();
        assert!(!current.is_behind(&latest));
    }

    #[test]
    fn version_comparison_is_component_wise() {
        let v1 = Version::parse("0.1.9").unwrap();
        let v2 = Version::parse("0.2.0").unwrap();
        assert!(v1.is_behind(&v2));

        let v3 = Version::parse("0.9.9").unwrap();
        let v4 = Version::parse("1.0.0").unwrap();
        assert!(v3.is_behind(&v4));
    }

    #[test]
    fn release_url_defaults_to_update_endpoint() {
        let url = release_url();
        assert_eq!(url, DEFAULT_UPDATE_ENDPOINT);
    }

    #[test]
    fn release_url_uses_github_format_when_api_base_set() {
        unsafe {
            std::env::set_var("FLUENT_API_BASE", "https://api.example.com");
            std::env::set_var("FLUENT_RELEASE_REPO", "test/repo");
        }
        let url = release_url();
        unsafe {
            std::env::remove_var("FLUENT_API_BASE");
            std::env::remove_var("FLUENT_RELEASE_REPO");
        }
        assert_eq!(
            url,
            "https://api.example.com/repos/test/repo/releases/latest"
        );
    }

    #[test]
    fn user_agent_includes_version_and_os() {
        let ua = user_agent();
        let version = env!("CARGO_PKG_VERSION");
        let os = std::env::consts::OS;
        assert_eq!(ua, format!("fluent/{version} ({os})"));
    }

    #[test]
    fn version_current_parses_cargo_version() {
        let v = Version::current();
        assert_eq!(v.to_string(), env!("CARGO_PKG_VERSION"));
    }
}
