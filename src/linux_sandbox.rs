//! Confine coders and Tester commands on Linux with a Landlock ruleset.
//!
//! Landlock unions every rule matching a path, so a nested rule only ever
//! widens what an ancestor granted — there is no equivalent of Seatbelt's
//! first-match-wins ordering where a `deny` beats a broader `allow`. Every
//! carve-out ("$HOME is readable except `~/.ssh`") is therefore enumerated into
//! sibling grants while rendering, by [`grant_excluding`]. Adding a narrower
//! rule to subtract access would silently do nothing.
//!
//! Landlock does not mediate `stat`, so Seatbelt's "metadata yes, contents no"
//! pairs collapse to granting nothing: path traversal keeps working and the
//! contents stay unreadable.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::coder::CoderKind;

pub const POLICY_VERSION: u32 = 1;

/// Read-only system hierarchies. `/` itself is never granted: that would hand
/// back every `$HOME` secret the rendered home rules exist to withhold.
const SYSTEM_READ: &[&str] = &[
    "/bin", "/sbin", "/lib", "/lib64", "/lib32", "/libx32", "/usr", "/etc", "/opt", "/nix", "/proc",
    "/sys", "/var", "/run", "/snap", "/srv",
];

/// Writable system hierarchies, each more specific than a `SYSTEM_READ` entry
/// so the union raises them from read-only to read-write.
pub(crate) const SYSTEM_WRITE: &[&str] = &["/tmp", "/var/tmp", "/dev/shm"];

/// Device nodes a coder writes: terminals for pty allocation, the null/zero
/// sinks, and the entropy sources. The rest of `/dev` stays read-only.
///
/// `/dev/fd`, `/dev/stdin`, `/dev/stdout`, and `/dev/stderr` are absent on
/// purpose. They are symlinks into `/proc/self/fd`, which the kernel refuses as
/// a rule target with `EBADFD`, and a rule would buy nothing: Landlock mediates
/// opening a path, not writing through a descriptor that is already open.
const DEVICE_WRITE: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/random",
    "/dev/urandom",
    "/dev/tty",
    "/dev/ptmx",
    "/dev/pts",
];

/// Home-relative paths withheld from the coder. Nested entries force `$HOME`
/// and the intervening directories to be enumerated rather than granted whole.
const HOME_SECRETS: &[&str] = &[
    ".ssh",
    ".aws",
    ".azure",
    ".gnupg",
    ".kube",
    ".docker",
    ".password-store",
    ".terraform.d",
    ".gem/credentials",
    ".pypirc",
    ".netrc",
    ".npmrc",
    ".git-credentials",
    ".vault-token",
    ".credentials",
    ".secrets",
    ".keys",
    ".env",
    ".envrc",
    ".bash_history",
    ".zsh_history",
    ".bashrc",
    ".zshrc",
    ".profile",
    ".bash_profile",
    ".zprofile",
    ".zshenv",
    ".cargo/credentials.toml",
    ".config/gh",
    ".config/gcloud",
    ".config/op",
    ".config/1Password",
    ".config/fish",
    ".config/configstore",
    ".config/heroku",
    ".config/stripe",
    ".config/vercel",
    ".config/netlify",
    ".config/firebase",
    ".config/google-chrome",
    ".config/chromium",
    ".config/BraveSoftware",
    ".config/microsoft-edge",
    ".local/share/keyrings",
    ".mozilla",
    ".thunderbird",
];

/// Readable despite living under the withheld `~/.ssh`. The FIDO2 key handle is
/// useless without the hardware token, and git needs the rest to verify and
/// sign commits.
const SSH_READABLE: &[&str] = &[
    ".ssh/known_hosts",
    ".ssh/allowed_signers",
    ".ssh/id_ed25519_sk",
    ".ssh/id_ed25519_sk.pub",
];

/// Home-relative paths a coder writes to keep sessions and local state.
const HOME_WRITE: &[&str] = &[".local/state", ".agent-browser"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Access {
    /// List a directory. File contents stay unreadable, which is how a
    /// directory is made discoverable without leaking what is in it.
    List,
    /// Read and execute.
    Read,
    /// Read, execute, create, modify, and delete.
    ReadWrite,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Rule {
    pub path: PathBuf,
    pub access: Access,
    /// Fail the launch when the path is missing, rather than dropping the rule.
    /// Set for paths the caller named; the rendered system and home boilerplate
    /// describes a generic host and is expected to be partly absent.
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub version: u32,
    pub rules: Vec<Rule>,
    /// Confine signals and abstract unix sockets to the sandbox, mirroring
    /// Seatbelt's `(allow signal (target same-sandbox))`. Needs Landlock ABI 6.
    pub scope_to_sandbox: bool,
}

/// What the caller wants confined, before it is expanded into Landlock rules.
pub struct PolicyRequest<'a> {
    pub home: &'a Path,
    pub writable_roots: &'a [PathBuf],
    pub readable_roots: &'a [PathBuf],
    pub denied_write_roots: &'a [PathBuf],
    pub coder_kind: Option<CoderKind>,
    /// Codex's private worker home. When it differs from the interactive home,
    /// the interactive one joins the withheld set.
    pub codex_home: Option<&'a Path>,
    /// Grant the shared temp trees. A handoff-only policy clears this: a
    /// writable `/tmp` is a channel around whatever `denied_write_roots`
    /// withholds.
    pub grant_shared_temp: bool,
}

pub fn render(request: &PolicyRequest<'_>) -> Result<Policy> {
    if request.writable_roots.is_empty() {
        bail!("At least one writable sandbox root is required");
    }

    let withheld = withheld_paths(request);
    let mut rules = Vec::new();
    push_system_rules(request, &withheld, &mut rules)?;
    push_home_rules(request, &withheld, &mut rules)?;
    push_coder_rules(request, &mut rules);
    push_root_rules(request, &mut rules)?;

    Ok(Policy {
        version: POLICY_VERSION,
        rules: merge_rules(rules),
        scope_to_sandbox: true,
    })
}

/// Paths no rule may grant, whichever hierarchy they happen to fall under.
fn withheld_paths(request: &PolicyRequest<'_>) -> BTreeSet<PathBuf> {
    let mut withheld: BTreeSet<PathBuf> = HOME_SECRETS
        .iter()
        .map(|relative| request.home.join(relative))
        .collect();

    // An autonomous Codex worker gets a staged home. Withholding the
    // interactive one keeps its hooks, sessions, and credentials out of reach.
    if let Some(worker_home) = request.codex_home {
        let source_home = crate::codex_worker::effective_source_home();
        if worker_home != source_home {
            withheld.insert(source_home);
        }
    }
    withheld
}

/// A system hierarchy is carved around the withheld paths too. Landlock unions
/// rules, so a home under a granted tree — `/tmp` for a scratch checkout, say —
/// would otherwise hand back every secret the home rules withhold.
fn push_system_rules(
    request: &PolicyRequest<'_>,
    withheld: &BTreeSet<PathBuf>,
    rules: &mut Vec<Rule>,
) -> Result<()> {
    for path in SYSTEM_READ {
        grant_excluding(Path::new(path), Access::Read, withheld, rules)?;
    }
    rules.push(Rule::optional("/dev", Access::Read));
    for path in DEVICE_WRITE {
        rules.push(Rule::optional(path, Access::ReadWrite));
    }
    if request.grant_shared_temp {
        let mut unwritable = withheld.clone();
        unwritable.extend(request.denied_write_roots.iter().cloned());
        for path in SYSTEM_WRITE {
            grant_excluding(Path::new(path), Access::ReadWrite, &unwritable, rules)?;
        }
        if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
            grant_excluding(
                Path::new(&runtime_dir),
                Access::ReadWrite,
                &unwritable,
                rules,
            )?;
        }
    }
    Ok(())
}

fn push_home_rules(
    request: &PolicyRequest<'_>,
    withheld: &BTreeSet<PathBuf>,
    rules: &mut Vec<Rule>,
) -> Result<()> {
    grant_excluding(request.home, Access::Read, withheld, rules)?;

    for relative in SSH_READABLE {
        rules.push(Rule::optional(request.home.join(relative), Access::Read));
    }
    for relative in HOME_WRITE {
        rules.push(Rule::optional(request.home.join(relative), Access::ReadWrite));
    }
    Ok(())
}

fn push_coder_rules(request: &PolicyRequest<'_>, rules: &mut Vec<Rule>) {
    let home = request.home;
    match request.coder_kind {
        Some(CoderKind::Claude) => {
            for relative in [".claude", ".claude.json", ".claude.json.lock"] {
                rules.push(Rule::optional(home.join(relative), Access::ReadWrite));
            }
            rules.push(Rule::optional(
                home.join("Workspace/skills"),
                Access::ReadWrite,
            ));
        }
        Some(CoderKind::Codex) => {
            let codex_home = request
                .codex_home
                .map(Path::to_path_buf)
                .unwrap_or_else(|| home.join(".codex"));
            rules.push(Rule::optional(codex_home, Access::ReadWrite));
        }
        Some(CoderKind::Pi) => {
            rules.push(Rule::optional(home.join(".pi"), Access::Read));
            rules.push(Rule::optional(home.join(".pi/agent"), Access::ReadWrite));
        }
        None => {}
    }
}

fn push_root_rules(request: &PolicyRequest<'_>, rules: &mut Vec<Rule>) -> Result<()> {
    let denied: BTreeSet<PathBuf> = request.denied_write_roots.iter().cloned().collect();

    for root in request.writable_roots {
        let mut granted = Vec::new();
        grant_excluding(root, Access::ReadWrite, &denied, &mut granted)?;
        for rule in &mut granted {
            rule.required = true;
        }
        rules.append(&mut granted);
    }

    // A denied write root keeps the read access every other rule grants it;
    // only the write half is withheld.
    for root in request.denied_write_roots {
        rules.push(Rule::optional(root, Access::Read));
    }
    for root in request.readable_roots {
        rules.push(Rule::required(root, Access::Read));
    }
    Ok(())
}

/// Grant `access` over `root` while withholding `exclusions`.
///
/// A subtree containing an exclusion is replaced by grants on the siblings that
/// lead nowhere near it, walking down only as far as an exclusion forces. An
/// enumerated directory keeps [`Access::List`] so `readdir` on it still works —
/// without that, carving `~/.ssh` out of `$HOME` would also stop anything from
/// listing `$HOME`. Listing is what Seatbelt's paired metadata grants allow
/// too: a withheld directory stays discoverable, its file contents do not.
///
/// Enumeration reads the filesystem, so a directory created after rendering
/// falls outside every rule and the sandbox fails closed on it. Exclusions are
/// applied whether or not they exist yet, so a secret directory created later
/// is withheld rather than inherited from its parent.
fn grant_excluding(
    root: &Path,
    access: Access,
    exclusions: &BTreeSet<PathBuf>,
    out: &mut Vec<Rule>,
) -> Result<()> {
    if exclusions.contains(root) {
        return Ok(());
    }
    let encloses_exclusion = exclusions
        .iter()
        .any(|excluded| excluded.starts_with(root) && excluded != root);
    if !encloses_exclusion {
        out.push(Rule::optional(root, access));
        return Ok(());
    }

    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        // A hierarchy this host does not have cannot contain an exclusion, so
        // the plain rule is safe; `apply` drops it when the path is absent.
        Err(_) if !root.exists() => {
            out.push(Rule::optional(root, access));
            return Ok(());
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!("enumerating {} to withhold nested paths", root.display())
            });
        }
    };
    out.push(Rule::optional(root, Access::List));
    for entry in entries {
        let entry = entry.with_context(|| format!("reading an entry of {}", root.display()))?;
        grant_excluding(&entry.path(), access, exclusions, out)?;
    }
    Ok(())
}

/// Collapse duplicate paths to their widest access. Landlock would union them
/// anyway; folding here keeps the rendered policy readable.
fn merge_rules(rules: Vec<Rule>) -> Vec<Rule> {
    let mut merged: std::collections::BTreeMap<PathBuf, Rule> = std::collections::BTreeMap::new();
    for rule in rules {
        merged
            .entry(rule.path.clone())
            .and_modify(|existing| {
                existing.access = existing.access.max(rule.access);
                existing.required |= rule.required;
            })
            .or_insert(rule);
    }
    merged.into_values().collect()
}

impl Rule {
    fn optional(path: impl Into<PathBuf>, access: Access) -> Self {
        Self {
            path: path.into(),
            access,
            required: false,
        }
    }

    fn required(path: impl Into<PathBuf>, access: Access) -> Self {
        Self {
            path: path.into(),
            access,
            required: true,
        }
    }
}

pub fn serialize(policy: &Policy) -> Result<String> {
    serde_json::to_string_pretty(policy).context("serializing the Landlock policy")
}

pub fn load(path: &Path) -> Result<Policy> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading the Landlock policy at {}", path.display()))?;
    let policy: Policy = serde_json::from_str(&raw)
        .with_context(|| format!("parsing the Landlock policy at {}", path.display()))?;
    if policy.version != POLICY_VERSION {
        bail!(
            "Landlock policy version {} is not supported (this build reads version {POLICY_VERSION})",
            policy.version
        );
    }
    Ok(policy)
}

#[cfg(target_os = "linux")]
mod enforce {
    use super::{Access, Policy};
    use anyhow::{Context, Result, bail};
    use landlock::{
        ABI, Access as _, AccessFs, BitFlags, CompatLevel, Compatible, PathBeneath, PathFd,
        Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus, Scope,
    };

    /// Landlock revision this policy is written against. Best-effort
    /// compatibility drops the rights an older kernel does not know, so the same
    /// policy still enforces what that kernel can.
    const TARGET_ABI: ABI = ABI::V5;

    fn access_flags(access: Access) -> BitFlags<AccessFs> {
        match access {
            Access::List => AccessFs::ReadDir.into(),
            Access::Read => AccessFs::from_read(TARGET_ABI),
            Access::ReadWrite => AccessFs::from_all(TARGET_ABI),
        }
    }

    pub fn apply(policy: &Policy) -> Result<()> {
        let mut attributes = Ruleset::default()
            .set_compatibility(CompatLevel::BestEffort)
            .handle_access(AccessFs::from_all(TARGET_ABI))
            .context("declaring the Landlock filesystem rights to enforce")?;
        if policy.scope_to_sandbox {
            attributes = attributes
                .scope(Scope::Signal | Scope::AbstractUnixSocket)
                .context("scoping signals and abstract sockets to the sandbox")?;
        }
        let mut ruleset = attributes.create().context("creating the Landlock ruleset")?;

        for rule in &policy.rules {
            let fd = match PathFd::new(&rule.path) {
                Ok(fd) => fd,
                Err(error) => {
                    if rule.required {
                        return Err(error).with_context(|| {
                            format!("opening required sandbox path {}", rule.path.display())
                        });
                    }
                    continue;
                }
            };
            ruleset = ruleset
                .add_rule(PathBeneath::new(fd, access_flags(rule.access)))
                .with_context(|| format!("adding a rule for {}", rule.path.display()))?;
        }

        let status = ruleset
            .restrict_self()
            .context("applying the Landlock ruleset to this process")?;

        // Best-effort compatibility degrades quietly by design, and an
        // unenforced ruleset would run the coder unconfined while every log line
        // still said "sandboxed".
        if status.ruleset == RulesetStatus::NotEnforced {
            bail!(
                "the kernel enforced no part of the Landlock ruleset; \
                 Landlock needs Linux 5.13 or newer with the LSM enabled"
            );
        }
        Ok(())
    }

    /// Report whether this kernel enforces Landlock at all.
    ///
    /// The probe demands ABI 1 outright instead of asking best-effort, which
    /// reports success on a kernel that would enforce nothing: Landlock is
    /// commonly compiled in but left out of the boot `lsm=` list, and there the
    /// syscall fails with `EOPNOTSUPP` while best-effort degrades to silence.
    pub fn is_available() -> bool {
        Ruleset::default()
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(AccessFs::from_all(ABI::V1))
            .and_then(|ruleset| ruleset.create())
            .is_ok()
    }
}

#[cfg(not(target_os = "linux"))]
mod enforce {
    use super::Policy;
    use anyhow::{Result, bail};

    pub fn apply(_policy: &Policy) -> Result<()> {
        bail!("Landlock confinement is available only on Linux")
    }

    pub fn is_available() -> bool {
        false
    }
}

pub use enforce::is_available;

/// Confine this process to `policy`, then replace it with `program`.
///
/// Landlock restrictions survive `execve`, so confining here and execing is what
/// makes the coder itself sandboxed. Applying the ruleset in a `pre_exec` hook
/// of the parent would have to allocate between `fork` and `exec`, so Fluent
/// re-executes itself as the launcher instead.
pub fn exec_confined(policy_path: &Path, program: &Path, args: &[String]) -> Result<()> {
    let policy = load(policy_path)?;
    enforce::apply(&policy)?;

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = std::process::Command::new(program).args(args).exec();
        Err(error).with_context(|| format!("executing {} inside the sandbox", program.display()))
    }
    #[cfg(not(unix))]
    {
        let _ = (program, args);
        bail!("the sandbox launcher requires a Unix host")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Build fixtures outside the granted system hierarchies.
    ///
    /// The platform temp directory sits inside one on both hosts — `/tmp` on
    /// Linux, `/var/folders` on macOS — so a fixture there is enumerated as
    /// part of a system rule, and assertions about that rule would be reading
    /// the fixture's own effect instead.
    fn fixture_home() -> TempDir {
        match std::env::var_os("HOME") {
            Some(home) => TempDir::new_in(home).expect("creating a fixture under HOME"),
            None => TempDir::new().unwrap(),
        }
    }

    fn request<'a>(home: &'a Path, roots: &'a [PathBuf]) -> PolicyRequest<'a> {
        PolicyRequest {
            home,
            writable_roots: roots,
            readable_roots: &[],
            denied_write_roots: &[],
            coder_kind: Some(CoderKind::Claude),
            codex_home: None,
            grant_shared_temp: true,
        }
    }

    fn access_for(policy: &Policy, path: &Path) -> Option<Access> {
        policy
            .rules
            .iter()
            .find(|rule| rule.path == path)
            .map(|rule| rule.access)
    }

    #[test]
    fn rendering_requires_a_writable_root() {
        let home = fixture_home();
        let error = render(&request(home.path(), &[])).unwrap_err();
        assert!(error.to_string().contains("writable sandbox root"));
    }

    #[test]
    fn home_secrets_are_withheld_by_enumerating_their_siblings() {
        let home = fixture_home();
        std::fs::create_dir(home.path().join(".ssh")).unwrap();
        std::fs::create_dir(home.path().join("code")).unwrap();
        let roots = vec![home.path().join("code")];

        let policy = render(&request(home.path(), &roots)).unwrap();

        assert_eq!(
            access_for(&policy, home.path()),
            Some(Access::List),
            "{policy:?}"
        );
        assert_eq!(
            access_for(&policy, &home.path().join(".ssh")),
            None,
            "{policy:?}"
        );
        assert_eq!(
            access_for(&policy, &home.path().join("code")),
            Some(Access::ReadWrite),
            "{policy:?}"
        );
    }

    #[test]
    fn signing_material_stays_readable_inside_the_withheld_ssh_directory() {
        let home = fixture_home();
        std::fs::create_dir(home.path().join(".ssh")).unwrap();
        let roots = vec![home.path().to_path_buf()];

        let policy = render(&request(home.path(), &roots)).unwrap();

        assert_eq!(
            access_for(&policy, &home.path().join(".ssh/known_hosts")),
            Some(Access::Read),
            "{policy:?}"
        );
    }

    #[test]
    fn an_enumerated_root_stays_listable() {
        let home = fixture_home();
        std::fs::create_dir(home.path().join("code")).unwrap();
        let roots = vec![home.path().join("code")];

        let policy = render(&request(home.path(), &roots)).unwrap();

        assert_eq!(
            access_for(&policy, home.path()),
            Some(Access::List),
            "{policy:?}"
        );
    }

    #[test]
    fn a_denied_write_root_keeps_read_and_loses_write() {
        let home = fixture_home();
        let workspace = home.path().join("workspace");
        let candidate = workspace.join("candidate");
        std::fs::create_dir_all(&candidate).unwrap();
        std::fs::create_dir(workspace.join("artifacts")).unwrap();
        let roots = vec![workspace.clone()];
        let denied = vec![candidate.clone()];

        let policy = render(&PolicyRequest {
            denied_write_roots: &denied,
            grant_shared_temp: false,
            ..request(home.path(), &roots)
        })
        .unwrap();

        assert_eq!(access_for(&policy, &candidate), Some(Access::Read));
        assert_eq!(
            access_for(&policy, &workspace.join("artifacts")),
            Some(Access::ReadWrite)
        );
        // The enclosing root keeps whatever read access the home rules gave it;
        // what the denial has to remove is the write half.
        assert_ne!(
            access_for(&policy, &workspace),
            Some(Access::ReadWrite),
            "{policy:?}"
        );
    }

    #[test]
    fn a_handoff_policy_withholds_the_shared_temp_trees() {
        let home = fixture_home();
        let workspace = home.path().join("workspace");
        let candidate = workspace.join("candidate");
        std::fs::create_dir_all(&candidate).unwrap();
        let roots = vec![workspace];
        let denied = vec![candidate];

        let policy = render(&PolicyRequest {
            denied_write_roots: &denied,
            grant_shared_temp: false,
            ..request(home.path(), &roots)
        })
        .unwrap();

        assert_eq!(access_for(&policy, Path::new("/tmp")), None, "{policy:?}");
        assert_eq!(access_for(&policy, Path::new("/var/tmp")), None, "{policy:?}");
    }

    #[test]
    fn a_writable_root_is_required_so_a_missing_one_fails_the_launch() {
        let home = fixture_home();
        let root = home.path().join("code");
        std::fs::create_dir(&root).unwrap();
        let roots = vec![root.clone()];

        let policy = render(&request(home.path(), &roots)).unwrap();

        let rule = policy.rules.iter().find(|rule| rule.path == root).unwrap();
        assert!(rule.required, "{rule:?}");
    }

    #[test]
    fn system_hierarchies_are_optional_so_a_minimal_host_still_launches() {
        let home = fixture_home();
        let roots = vec![home.path().to_path_buf()];

        let policy = render(&request(home.path(), &roots)).unwrap();

        let snap = policy
            .rules
            .iter()
            .find(|rule| rule.path == Path::new("/snap"))
            .unwrap();
        assert!(!snap.required, "{snap:?}");
    }

    #[test]
    fn writable_system_paths_outrank_the_read_only_hierarchy_containing_them() {
        let home = fixture_home();
        let roots = vec![home.path().to_path_buf()];

        let policy = render(&request(home.path(), &roots)).unwrap();

        assert_eq!(
            access_for(&policy, Path::new("/var")),
            Some(Access::Read),
            "{policy:?}"
        );
        assert_eq!(
            access_for(&policy, Path::new("/var/tmp")),
            Some(Access::ReadWrite),
            "{policy:?}"
        );
    }

    #[test]
    fn the_filesystem_root_is_never_granted() {
        let home = fixture_home();
        let roots = vec![home.path().to_path_buf()];

        let policy = render(&request(home.path(), &roots)).unwrap();

        assert_eq!(access_for(&policy, Path::new("/")), None, "{policy:?}");
    }

    #[test]
    fn each_coder_gets_only_its_own_configuration() {
        let home = fixture_home();
        let roots = vec![home.path().to_path_buf()];

        let claude = render(&PolicyRequest {
            coder_kind: Some(CoderKind::Claude),
            ..request(home.path(), &roots)
        })
        .unwrap();
        let pi = render(&PolicyRequest {
            coder_kind: Some(CoderKind::Pi),
            ..request(home.path(), &roots)
        })
        .unwrap();

        assert_eq!(
            access_for(&claude, &home.path().join(".claude")),
            Some(Access::ReadWrite)
        );
        assert_eq!(access_for(&pi, &home.path().join(".claude")), None);
        assert_eq!(
            access_for(&pi, &home.path().join(".pi/agent")),
            Some(Access::ReadWrite)
        );
    }

    #[test]
    fn a_policy_round_trips_through_its_serialized_form() {
        let home = fixture_home();
        let roots = vec![home.path().to_path_buf()];
        let policy = render(&request(home.path(), &roots)).unwrap();

        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), serialize(&policy).unwrap()).unwrap();

        assert_eq!(load(file.path()).unwrap(), policy);
    }

    #[test]
    fn a_policy_from_a_future_version_is_refused_rather_than_partly_applied() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            file.path(),
            r#"{"version": 99, "rules": [], "scope_to_sandbox": true}"#,
        )
        .unwrap();

        let error = load(file.path()).unwrap_err();
        assert!(error.to_string().contains("version 99"), "{error}");
    }
}
