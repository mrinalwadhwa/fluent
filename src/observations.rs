use anyhow::{Result, bail};
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Read};
use std::path::Path;

pub enum MatchResult {
    Unique(String),
    None,
    Ambiguous(Vec<String>),
}

/// Marks the kind of a system-generated Observation in its reserved frontmatter.
pub const LEARNER_FOLLOW_UP_KIND: &str = "learner-follow-up";

/// Reserved YAML frontmatter carried by a system-generated Observation. It
/// records the provenance an inspector needs to trace a materialized follow-up
/// back to its origin without consulting any other artifact. Manual, body-only
/// Observations carry no frontmatter and stay valid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub struct ObservationFrontmatter {
    /// Names the generating subsystem, e.g. `learner-follow-up`. Present marks
    /// the Observation as system-generated.
    #[serde(rename = "fluent-observation", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The follow-up within its landed batch that this Observation materializes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up_id: Option<String>,
    /// The originating Work Item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    /// The originating Attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    /// The originating Merge Candidate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_candidate_id: Option<String>,
    /// The commit the originating Merge Candidate landed as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_commit: Option<String>,
    /// The derived Work Item promoted from this Observation, once one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_work_item_id: Option<String>,
}

/// Outcome of ensuring a provenance-bearing Observation exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsureOutcome {
    /// A new open Observation was written.
    Created,
    /// An open Observation with matching provenance already existed.
    ReusedOpen,
    /// A resolved Observation already existed and was left resolved.
    ReusedResolved,
}

impl EnsureOutcome {
    pub fn created(&self) -> bool {
        matches!(self, Self::Created)
    }
}

pub fn slugify(text: &str) -> String {
    let lowered = text.to_lowercase();
    let mut result = String::new();
    let mut prev_dash = true;
    for c in lowered.chars() {
        if c.is_ascii_alphanumeric() {
            result.push(c);
            prev_dash = false;
        } else if !prev_dash {
            result.push('-');
            prev_dash = true;
        }
    }
    while result.ends_with('-') {
        result.pop();
    }
    if result.len() > 40 {
        result.truncate(40);
        while result.ends_with('-') {
            result.pop();
        }
    }
    result
}

pub fn generate_id(content: &str) -> String {
    let timestamp = Local::now().format("%Y%m%d-%H%M%S").to_string();
    let first_line = content.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let slug = slugify(first_line);
    if slug.is_empty() {
        timestamp
    } else {
        format!("{timestamp}-{slug}")
    }
}

pub fn resolve_collision(base_id: &str, dir: &Path) -> String {
    if !dir.join(format!("{base_id}.md")).exists() {
        return base_id.to_string();
    }
    let mut counter = 2;
    loop {
        let candidate = format!("{base_id}-{counter}");
        if !dir.join(format!("{candidate}.md")).exists() {
            return candidate;
        }
        counter += 1;
    }
}

pub fn expand_prefix(prefix: &str, dir: &Path) -> Result<MatchResult> {
    if !dir.is_dir() {
        return Ok(MatchResult::None);
    }
    let mut matches = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if let Some(id) = name_str.strip_suffix(".md") {
            if id.starts_with(prefix) {
                matches.push(id.to_string());
            }
        }
    }
    match matches.len() {
        0 => Ok(MatchResult::None),
        1 => Ok(MatchResult::Unique(matches.into_iter().next().unwrap())),
        _ => {
            matches.sort();
            Ok(MatchResult::Ambiguous(matches))
        }
    }
}

fn read_content_or_stdin(content: Option<String>) -> Result<String> {
    match content {
        Some(c) => {
            if c.trim().is_empty() {
                bail!("Observation content is empty");
            }
            Ok(c)
        }
        None => {
            let mut buf = String::new();
            io::stdin().read_to_string(&mut buf)?;
            if buf.trim().is_empty() {
                bail!("No content provided on stdin");
            }
            Ok(buf)
        }
    }
}

pub fn add(project_root: &Path, content: Option<String>) -> Result<()> {
    let content = read_content_or_stdin(content)?;
    let obs_dir = project_root.join(".fluent/observations");
    fs::create_dir_all(&obs_dir)?;

    let base_id = generate_id(&content);
    let id = resolve_collision(&base_id, &obs_dir);

    let mut body = content;
    if !body.ends_with('\n') {
        body.push('\n');
    }
    fs::write(obs_dir.join(format!("{id}.md")), &body)?;

    println!("{id}");
    Ok(())
}

pub fn resolve(project_root: &Path, id_prefix: &str, resolution: Option<String>) -> Result<()> {
    let obs_dir = project_root.join(".fluent/observations");
    let resolved_dir = obs_dir.join("resolved");

    let id = match expand_prefix(id_prefix, &obs_dir)? {
        MatchResult::Unique(id) => id,
        MatchResult::None => bail!("No open observation matching {id_prefix:?}"),
        MatchResult::Ambiguous(matches) => {
            let list = matches.join("\n  ");
            bail!(
                "Ambiguous prefix {id_prefix:?} matches multiple observations:\n  {list}\nSpecify a longer prefix to disambiguate."
            );
        }
    };

    let resolution = read_content_or_stdin(resolution)?;

    let source = obs_dir.join(format!("{id}.md"));
    let existing = fs::read_to_string(&source)?;

    let mut combined = existing;
    if !combined.ends_with('\n') {
        combined.push('\n');
    }
    combined.push_str(&format!("\n\u{2192} Resolved: {resolution}\n"));

    fs::create_dir_all(&resolved_dir)?;
    fs::write(resolved_dir.join(format!("{id}.md")), &combined)?;
    fs::remove_file(&source)?;

    println!("{id}");
    Ok(())
}

pub fn list(project_root: &Path) -> Result<()> {
    let obs_dir = project_root.join(".fluent/observations");
    if !obs_dir.is_dir() {
        return Ok(());
    }

    let mut entries: Vec<(String, String)> = Vec::new();
    for entry in fs::read_dir(&obs_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if let Some(id) = name_str.strip_suffix(".md") {
            let content = fs::read_to_string(entry.path())?;
            entries.push((id.to_string(), display_summary(&content)));
        }
    }

    entries.sort_by(|a, b| a.0.cmp(&b.0));

    for (id, first_line) in entries {
        println!("{id}  {first_line}");
    }

    Ok(())
}

pub fn show(project_root: &Path, id_prefix: &str) -> Result<()> {
    let obs_dir = project_root.join(".fluent/observations");
    let resolved_dir = obs_dir.join("resolved");

    match expand_prefix(id_prefix, &obs_dir)? {
        MatchResult::Unique(id) => {
            let content = fs::read_to_string(obs_dir.join(format!("{id}.md")))?;
            print!("{content}");
            return Ok(());
        }
        MatchResult::Ambiguous(matches) => {
            let list = matches.join("\n  ");
            bail!(
                "Ambiguous prefix {id_prefix:?} matches multiple observations:\n  {list}\nSpecify a longer prefix to disambiguate."
            );
        }
        MatchResult::None => {}
    }

    match expand_prefix(id_prefix, &resolved_dir)? {
        MatchResult::Unique(id) => {
            let content = fs::read_to_string(resolved_dir.join(format!("{id}.md")))?;
            print!("{content}");
            Ok(())
        }
        MatchResult::Ambiguous(matches) => {
            let list = matches.join("\n  ");
            bail!(
                "Ambiguous prefix {id_prefix:?} matches multiple resolved observations:\n  {list}\nSpecify a longer prefix to disambiguate."
            );
        }
        MatchResult::None => {
            bail!("No observation matching {id_prefix:?}");
        }
    }
}

// --- Provenance-bearing system Observations ---

/// Split a leading YAML frontmatter block from an Observation's content. A block
/// is `---\n...\n---\n` at the very start; anything else is treated as a
/// body-only Observation with no frontmatter, so legacy and manual Observations
/// stay valid.
pub fn split_frontmatter(content: &str) -> (Option<ObservationFrontmatter>, &str) {
    let Some(rest) = content.strip_prefix("---\n") else {
        return (None, content);
    };
    let Some(end) = rest.find("\n---\n") else {
        // An unterminated fence is not frontmatter; leave the content untouched.
        return (None, content);
    };
    let yaml = &rest[..end];
    let body = &rest[end + "\n---\n".len()..];
    match serde_yaml::from_str::<ObservationFrontmatter>(yaml) {
        Ok(frontmatter) => (Some(frontmatter), body),
        Err(_) => (None, content),
    }
}

/// Render an Observation as a reserved YAML frontmatter block followed by its
/// human-readable body.
fn render_with_frontmatter(frontmatter: &ObservationFrontmatter, body: &str) -> Result<String> {
    let yaml = serde_yaml::to_string(frontmatter)?;
    let mut out = String::from("---\n");
    out.push_str(&yaml);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("---\n\n");
    out.push_str(body.trim_end());
    out.push('\n');
    Ok(out)
}

/// The display summary of an Observation: its first non-empty body line with any
/// frontmatter block skipped.
pub fn display_summary(content: &str) -> String {
    let (_, body) = split_frontmatter(content);
    body.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .to_string()
}

/// Idempotently ensure exactly one Observation with the given id and provenance.
///
/// A matching open Observation is reused; a resolved Observation is left resolved
/// and never reopened; a body-only Observation at the id, or one whose stored
/// provenance conflicts with `frontmatter`, is rejected so replay converges on a
/// single authoritative record rather than overwriting another.
pub fn ensure_provenance_observation(
    project_root: &Path,
    id: &str,
    frontmatter: &ObservationFrontmatter,
    body: &str,
) -> Result<EnsureOutcome> {
    let obs_dir = project_root.join(".fluent/observations");
    let resolved_dir = obs_dir.join("resolved");

    let resolved_path = resolved_dir.join(format!("{id}.md"));
    if resolved_path.exists() {
        verify_provenance_matches(&resolved_path, frontmatter)?;
        return Ok(EnsureOutcome::ReusedResolved);
    }

    let open_path = obs_dir.join(format!("{id}.md"));
    if open_path.exists() {
        verify_provenance_matches(&open_path, frontmatter)?;
        return Ok(EnsureOutcome::ReusedOpen);
    }

    fs::create_dir_all(&obs_dir)?;
    let content = render_with_frontmatter(frontmatter, body)?;
    crate::atomic_write::atomic_write(&open_path, content.as_bytes())?;
    Ok(EnsureOutcome::Created)
}

/// Verify that an open or resolved provenance Observation exists without
/// creating or reopening it.
pub fn provenance_observation_exists(
    project_root: &Path,
    id: &str,
    frontmatter: &ObservationFrontmatter,
) -> Result<bool> {
    let obs_dir = project_root.join(".fluent/observations");
    let open = obs_dir.join(format!("{id}.md"));
    let resolved = obs_dir.join("resolved").join(format!("{id}.md"));
    match (open.exists(), resolved.exists()) {
        (true, true) => bail!("Observation {id} exists as both open and resolved"),
        (true, false) => {
            verify_provenance_matches(&open, frontmatter)?;
            Ok(true)
        }
        (false, true) => {
            verify_provenance_matches(&resolved, frontmatter)?;
            Ok(true)
        }
        (false, false) => Ok(false),
    }
}

/// Reject an existing Observation whose stored provenance does not match the
/// provenance a replay expects. A body-only Observation at a reserved id is a
/// conflict too: the id is claimed by an unrelated record.
fn verify_provenance_matches(
    path: &Path,
    expected: &ObservationFrontmatter,
) -> Result<()> {
    let existing = fs::read_to_string(path)?;
    let (stored, _) = split_frontmatter(&existing);
    let Some(stored) = stored else {
        bail!(
            "Observation {} exists without provenance frontmatter; refusing to overwrite it",
            path.display()
        );
    };
    if &stored != expected {
        bail!(
            "Observation {} has provenance conflicting with the follow-up being materialized",
            path.display()
        );
    }
    Ok(())
}

// --- Migration ---

struct ObservationBlock {
    date_compact: String,
    title_slug: String,
    body: String,
}

fn is_date_header(line: &str) -> bool {
    let bytes = line.as_bytes();
    if bytes.len() < 14 {
        return false;
    }
    bytes[0..4].iter().all(|b| b.is_ascii_digit())
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(|b| b.is_ascii_digit())
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(|b| b.is_ascii_digit())
        && line[10..].starts_with(" \u{2014} ")
}

fn extract_date_and_title(line: &str) -> (String, String) {
    let date_str = &line[..10];
    let date_compact = date_str.replace('-', "");
    let title = line[10..]
        .strip_prefix(" \u{2014} ")
        .unwrap_or("")
        .to_string();
    (date_compact, title)
}

fn parse_observation_blocks(content: &str) -> Vec<ObservationBlock> {
    let lines: Vec<&str> = content.lines().collect();
    let mut blocks = Vec::new();
    let mut current_start: Option<usize> = None;
    let mut current_date = String::new();
    let mut current_title = String::new();

    for (i, line) in lines.iter().enumerate() {
        if is_date_header(line) {
            if let Some(start) = current_start {
                let body = lines[start..i].join("\n");
                let body = body.trim_end().to_string();
                blocks.push(ObservationBlock {
                    date_compact: current_date.clone(),
                    title_slug: slugify(&current_title),
                    body: format!("{body}\n"),
                });
            }
            let (date, title) = extract_date_and_title(line);
            current_date = date;
            current_title = title;
            current_start = Some(i);
        }
    }

    if let Some(start) = current_start {
        let body = lines[start..].join("\n");
        let body = body.trim_end().to_string();
        blocks.push(ObservationBlock {
            date_compact: current_date,
            title_slug: slugify(&current_title),
            body: format!("{body}\n"),
        });
    }

    blocks
}

pub fn migrate(project_root: &Path) -> Result<()> {
    let obs_file = project_root.join(".fluent/observations.md");
    let resolved_file = project_root.join(".fluent/observations-resolved.md");

    if !obs_file.exists() && !resolved_file.exists() {
        println!("Nothing to migrate (no monolithic observation files found)");
        return Ok(());
    }

    let obs_dir = project_root.join(".fluent/observations");
    let resolved_dir = obs_dir.join("resolved");
    fs::create_dir_all(&obs_dir)?;
    fs::create_dir_all(&resolved_dir)?;

    if obs_file.exists() {
        let content = fs::read_to_string(&obs_file)?;
        let blocks = parse_observation_blocks(&content);
        for block in blocks {
            let base_id = if block.title_slug.is_empty() {
                format!("{}-000000", block.date_compact)
            } else {
                format!("{}-000000-{}", block.date_compact, block.title_slug)
            };
            let id = resolve_collision(&base_id, &obs_dir);
            fs::write(obs_dir.join(format!("{id}.md")), &block.body)?;
        }
        fs::remove_file(&obs_file)?;
    }

    if resolved_file.exists() {
        let content = fs::read_to_string(&resolved_file)?;
        let blocks = parse_observation_blocks(&content);
        for block in blocks {
            let base_id = if block.title_slug.is_empty() {
                format!("{}-000000", block.date_compact)
            } else {
                format!("{}-000000-{}", block.date_compact, block.title_slug)
            };
            let id = resolve_collision(&base_id, &resolved_dir);
            fs::write(resolved_dir.join(format!("{id}.md")), &block.body)?;
        }
        fs::remove_file(&resolved_file)?;
    }

    println!("Migrated observations to per-file layout");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn slugify_lowercases_and_replaces_non_alnum() {
        assert_eq!(slugify("Hello World!"), "hello-world");
    }

    #[test]
    fn slugify_collapses_runs() {
        assert_eq!(slugify("foo---bar___baz"), "foo-bar-baz");
    }

    #[test]
    fn slugify_trims_leading_trailing() {
        assert_eq!(slugify("  --hello-- "), "hello");
    }

    #[test]
    fn slugify_truncates_at_40() {
        let long = "a".repeat(50);
        assert_eq!(slugify(&long).len(), 40);
    }

    #[test]
    fn slugify_no_trailing_dash_after_truncation() {
        let input = format!("{} more stuff", "a".repeat(39));
        let result = slugify(&input);
        assert!(!result.ends_with('-'));
        assert!(result.len() <= 40);
    }

    #[test]
    fn slugify_empty_input() {
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("   "), "");
        assert_eq!(slugify("---"), "");
    }

    #[test]
    fn generate_id_includes_timestamp_and_slug() {
        let id = generate_id("Test observation");
        assert!(id.contains("-test-observation"), "id={id}");
        assert!(id.len() > 15);
    }

    #[test]
    fn generate_id_timestamp_only_when_no_alnum() {
        let id = generate_id("---");
        let parts: Vec<&str> = id.splitn(3, '-').collect();
        assert_eq!(parts.len(), 2, "id should be YYYYMMDD-HHMMSS only: {id}");
    }

    #[test]
    fn resolve_collision_no_conflict() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(resolve_collision("test-id", tmp.path()), "test-id");
    }

    #[test]
    fn resolve_collision_sequential_suffixes() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("test-id.md"), "a").unwrap();
        assert_eq!(resolve_collision("test-id", tmp.path()), "test-id-2");

        fs::write(tmp.path().join("test-id-2.md"), "b").unwrap();
        assert_eq!(resolve_collision("test-id", tmp.path()), "test-id-3");

        fs::write(tmp.path().join("test-id-3.md"), "c").unwrap();
        assert_eq!(resolve_collision("test-id", tmp.path()), "test-id-4");
    }

    #[test]
    fn expand_prefix_unique_match() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("20260612-000000-hello.md"), "x").unwrap();
        match expand_prefix("20260612", tmp.path()).unwrap() {
            MatchResult::Unique(id) => assert_eq!(id, "20260612-000000-hello"),
            _ => panic!("expected Unique"),
        }
    }

    #[test]
    fn expand_prefix_no_match() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("20260612-000000-hello.md"), "x").unwrap();
        assert!(matches!(
            expand_prefix("20260611", tmp.path()).unwrap(),
            MatchResult::None
        ));
    }

    #[test]
    fn expand_prefix_ambiguous() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("20260612-000000-hello.md"), "x").unwrap();
        fs::write(tmp.path().join("20260612-000000-world.md"), "y").unwrap();
        match expand_prefix("20260612", tmp.path()).unwrap() {
            MatchResult::Ambiguous(ids) => {
                assert_eq!(ids.len(), 2);
                assert!(ids.contains(&"20260612-000000-hello".to_string()));
                assert!(ids.contains(&"20260612-000000-world".to_string()));
            }
            _ => panic!("expected Ambiguous"),
        }
    }

    #[test]
    fn expand_prefix_nonexistent_dir() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("missing");
        assert!(matches!(
            expand_prefix("test", &missing).unwrap(),
            MatchResult::None
        ));
    }

    #[test]
    fn parse_blocks_splits_on_date_headers() {
        let content = "# Header\n\nSome intro text.\n\n---\n\n\
            2026-06-12 \u{2014} First observation\nMore details.\n\n\
            2026-06-12 \u{2014} Second observation\nDifferent content.\n";
        let blocks = parse_observation_blocks(content);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].date_compact, "20260612");
        assert_eq!(blocks[0].title_slug, slugify("First observation"));
        assert!(blocks[0].body.starts_with("2026-06-12"));
        assert!(blocks[0].body.contains("More details."));
        assert_eq!(blocks[1].date_compact, "20260612");
        assert!(blocks[1].body.contains("Different content."));
    }

    #[test]
    fn parse_blocks_preserves_multiline_content() {
        let content = "2026-06-11 \u{2014} Multi-paragraph observation\n\n\
            Second paragraph with details.\n\n\
            Third paragraph.\n";
        let blocks = parse_observation_blocks(content);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].body.contains("Second paragraph with details."));
        assert!(blocks[0].body.contains("Third paragraph."));
    }

    #[test]
    fn parse_blocks_empty_content() {
        let blocks = parse_observation_blocks("# Just a header\n\nNo observations here.\n");
        assert!(blocks.is_empty());
    }

    #[test]
    fn migrate_splits_and_removes_monolithic() {
        let tmp = TempDir::new().unwrap();
        let fluent = tmp.path().join(".fluent");
        fs::create_dir_all(&fluent).unwrap();

        fs::write(
            fluent.join("observations.md"),
            "# Observations\n\n---\n\n\
             2026-06-12 \u{2014} Test entry\nContent here.\n",
        )
        .unwrap();
        fs::write(
            fluent.join("observations-resolved.md"),
            "# Resolved\n\n---\n\n\
             2026-06-11 \u{2014} Old entry\n\u{2192} Resolved: done.\n",
        )
        .unwrap();

        migrate(tmp.path()).unwrap();

        assert!(!fluent.join("observations.md").exists());
        assert!(!fluent.join("observations-resolved.md").exists());
        assert!(fluent.join("observations").is_dir());
        assert!(fluent.join("observations/resolved").is_dir());

        let open_count = fs::read_dir(fluent.join("observations"))
            .unwrap()
            .filter(|e| e.as_ref().unwrap().file_type().unwrap().is_file())
            .count();
        assert_eq!(open_count, 1);

        let resolved_count = fs::read_dir(fluent.join("observations/resolved"))
            .unwrap()
            .filter(|e| e.as_ref().unwrap().file_type().unwrap().is_file())
            .count();
        assert_eq!(resolved_count, 1);
    }

    #[test]
    fn migrate_idempotent() {
        let tmp = TempDir::new().unwrap();
        let fluent = tmp.path().join(".fluent");
        fs::create_dir_all(&fluent).unwrap();

        fs::write(
            fluent.join("observations.md"),
            "# Observations\n\n---\n\n\
             2026-06-12 \u{2014} Test entry\nContent here.\n",
        )
        .unwrap();

        migrate(tmp.path()).unwrap();
        migrate(tmp.path()).unwrap();

        let open_count = fs::read_dir(fluent.join("observations"))
            .unwrap()
            .filter(|e| e.as_ref().unwrap().file_type().unwrap().is_file())
            .count();
        assert_eq!(open_count, 1);
    }

    #[test]
    fn migrate_collision_suffixes() {
        let tmp = TempDir::new().unwrap();
        let fluent = tmp.path().join(".fluent");
        fs::create_dir_all(&fluent).unwrap();

        fs::write(
            fluent.join("observations.md"),
            "# Observations\n\n---\n\n\
             2026-06-12 \u{2014} Same title\nFirst body.\n\n\
             2026-06-12 \u{2014} Same title\nSecond body.\n",
        )
        .unwrap();

        migrate(tmp.path()).unwrap();

        let obs_dir = fluent.join("observations");
        let mut files: Vec<String> = fs::read_dir(&obs_dir)
            .unwrap()
            .filter_map(|e| {
                let e = e.ok()?;
                if e.file_type().ok()?.is_file() {
                    Some(e.file_name().to_string_lossy().to_string())
                } else {
                    None
                }
            })
            .collect();
        files.sort();

        assert_eq!(files.len(), 2);
        assert!(files.contains(&"20260612-000000-same-title.md".to_string()));
        assert!(files.contains(&"20260612-000000-same-title-2.md".to_string()));
    }

    fn sample_frontmatter() -> ObservationFrontmatter {
        ObservationFrontmatter {
            kind: Some(LEARNER_FOLLOW_UP_KIND.to_string()),
            follow_up_id: Some("fu-1".to_string()),
            work_item_id: Some("work-1".to_string()),
            attempt_id: Some("attempt-1".to_string()),
            merge_candidate_id: Some("attempt-1-merge-candidate".to_string()),
            merged_commit: Some("abc123".to_string()),
            derived_work_item_id: None,
        }
    }

    #[test]
    fn frontmatter_round_trips_through_render_and_split() {
        let fm = sample_frontmatter();
        let content = render_with_frontmatter(&fm, "# Follow-up\n\nBody text.").unwrap();
        let (parsed, body) = split_frontmatter(&content);
        assert_eq!(parsed.as_ref(), Some(&fm));
        assert!(body.contains("Body text."));
    }

    #[test]
    fn body_only_content_has_no_frontmatter() {
        let (parsed, body) = split_frontmatter("2026-06-12 \u{2014} Manual note\nDetails.\n");
        assert!(parsed.is_none());
        assert!(body.starts_with("2026-06-12"));
    }

    #[test]
    fn unterminated_fence_is_not_frontmatter() {
        let (parsed, body) = split_frontmatter("---\nnot really frontmatter\n");
        assert!(parsed.is_none());
        assert!(body.starts_with("---"));
    }

    #[test]
    fn display_summary_skips_frontmatter() {
        let content =
            render_with_frontmatter(&sample_frontmatter(), "# Restore the retry cap").unwrap();
        assert_eq!(display_summary(&content), "# Restore the retry cap");
    }

    #[test]
    fn ensure_creates_then_reuses_open_observation() {
        let tmp = TempDir::new().unwrap();
        let fm = sample_frontmatter();

        let first =
            ensure_provenance_observation(tmp.path(), "followup-fu-1", &fm, "# Body").unwrap();
        assert_eq!(first, EnsureOutcome::Created);

        let path = tmp.path().join(".fluent/observations/followup-fu-1.md");
        assert!(path.exists());
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("merged-commit: abc123"));

        // A second ensure with the same provenance reuses the record.
        let second =
            ensure_provenance_observation(tmp.path(), "followup-fu-1", &fm, "# Body").unwrap();
        assert_eq!(second, EnsureOutcome::ReusedOpen);

        let count = fs::read_dir(tmp.path().join(".fluent/observations"))
            .unwrap()
            .filter(|e| e.as_ref().unwrap().file_type().unwrap().is_file())
            .count();
        assert_eq!(count, 1, "ensure must not duplicate the Observation");
    }

    #[test]
    fn ensure_leaves_resolved_observation_resolved() {
        let tmp = TempDir::new().unwrap();
        let fm = sample_frontmatter();
        ensure_provenance_observation(tmp.path(), "followup-fu-1", &fm, "# Body").unwrap();

        // Simulate the operator resolving it.
        resolve(tmp.path(), "followup-fu-1", Some("handled".to_string())).unwrap();
        assert!(
            tmp.path()
                .join(".fluent/observations/resolved/followup-fu-1.md")
                .exists()
        );

        // A replay must not reopen it.
        let outcome =
            ensure_provenance_observation(tmp.path(), "followup-fu-1", &fm, "# Body").unwrap();
        assert_eq!(outcome, EnsureOutcome::ReusedResolved);
        assert!(
            !tmp.path()
                .join(".fluent/observations/followup-fu-1.md")
                .exists(),
            "a resolved Observation is never reopened"
        );
    }

    #[test]
    fn ensure_rejects_conflicting_provenance() {
        let tmp = TempDir::new().unwrap();
        let fm = sample_frontmatter();
        ensure_provenance_observation(tmp.path(), "followup-fu-1", &fm, "# Body").unwrap();

        let mut other = fm.clone();
        other.merged_commit = Some("different-commit".to_string());
        let err = ensure_provenance_observation(tmp.path(), "followup-fu-1", &other, "# Body")
            .unwrap_err();
        assert!(
            err.to_string().contains("conflicting"),
            "conflicting provenance must be rejected: {err}"
        );
    }

    #[test]
    fn ensure_rejects_body_only_observation_at_reserved_id() {
        let tmp = TempDir::new().unwrap();
        let obs_dir = tmp.path().join(".fluent/observations");
        fs::create_dir_all(&obs_dir).unwrap();
        fs::write(obs_dir.join("followup-fu-1.md"), "a manual note\n").unwrap();

        let err =
            ensure_provenance_observation(tmp.path(), "followup-fu-1", &sample_frontmatter(), "# B")
                .unwrap_err();
        assert!(err.to_string().contains("without provenance frontmatter"));
    }
}
