use std::path::{Path, PathBuf};

/// Resolve runtime content the Factory binary reads directly.
///
/// The implemented bundled categories are prompts and sandbox profiles.
/// Skills and expertise stay in repository or installed skill layouts
/// and are read by agents, not by this resolver.
///
/// The resolution chain:
/// 1. Project-local: `<project_root>/.factory/<relative_path>`
/// 2. User config: `~/.config/factory/<relative_path>`
/// 3. Bundled defaults (compiled into the binary)
pub struct ContentResolver {
    project_root: Option<PathBuf>,
    user_config: PathBuf,
}

impl ContentResolver {
    pub fn new(project_root: Option<&Path>) -> Self {
        let user_config = dirs_config_path();
        Self {
            project_root: project_root.map(|p| p.to_path_buf()),
            user_config,
        }
    }

    /// Resolve a file by checking the resolution chain.
    /// Returns the path to the first match, or None if only bundled content exists.
    pub fn resolve_path(&self, relative: &str) -> Option<PathBuf> {
        // 1. Project-local
        if let Some(ref root) = self.project_root {
            let path = root.join(".factory").join(relative);
            if path.exists() {
                return Some(path);
            }
        }

        // 2. User config
        let path = self.user_config.join(relative);
        if path.exists() {
            return Some(path);
        }

        // 3. Bundled — caller should use bundled_* functions
        None
    }

    /// Resolve content as a string, falling back to bundled defaults.
    pub fn resolve_content(&self, relative: &str) -> Option<String> {
        // Check filesystem first
        if let Some(path) = self.resolve_path(relative) {
            return std::fs::read_to_string(&path).ok();
        }

        // Fall back to bundled content
        bundled_content(relative)
    }
}

fn dirs_config_path() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".config/factory")
    } else {
        PathBuf::from("/tmp/factory-config")
    }
}

/// Bundled runtime content compiled into the binary.
pub fn bundled_content(relative: &str) -> Option<String> {
    // Prompts
    match relative {
        "prompts/author.md" => Some(include_str!("../prompts/author.md").to_string()),
        "prompts/review-architecture.md" => {
            Some(include_str!("../prompts/review-architecture.md").to_string())
        }
        "prompts/review-behaviors.md" => {
            Some(include_str!("../prompts/review-behaviors.md").to_string())
        }
        "prompts/review-documentation.md" => {
            Some(include_str!("../prompts/review-documentation.md").to_string())
        }
        "prompts/review-skills.md" => Some(include_str!("../prompts/review-skills.md").to_string()),
        "prompts/review-tests.md" => Some(include_str!("../prompts/review-tests.md").to_string()),
        // Sandbox profiles
        "sandbox/common.sb" => Some(include_str!("../scripts/assets/common.sb").to_string()),
        "sandbox/claude-code.sb" => {
            Some(include_str!("../scripts/assets/claude-code.sb").to_string())
        }
        "sandbox/codex.sb" => Some(include_str!("../scripts/assets/codex.sb").to_string()),
        _ => None,
    }
}

/// Extract a named section from a prompt file.
/// Sections are delimited by `[section-name]` markers.
pub fn prompt_section(content: &str, section: &str) -> String {
    let marker = format!("[{section}]");
    let mut in_section = false;
    let mut result = String::new();

    for line in content.lines() {
        if line.starts_with('[') && line.ends_with(']') {
            in_section = line == marker;
            continue;
        }
        if in_section {
            result.push_str(line);
            result.push('\n');
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_prompt_section_extract() {
        let content = "\
[system]
You are a reviewer.
Check things.

[full-codebase]
Review the whole thing.

[run-scoped]
Review run {{RUN_ID}}.
";
        assert_eq!(
            prompt_section(content, "system").trim(),
            "You are a reviewer.\nCheck things."
        );
        assert_eq!(
            prompt_section(content, "full-codebase").trim(),
            "Review the whole thing."
        );
        assert_eq!(
            prompt_section(content, "run-scoped").trim(),
            "Review run {{RUN_ID}}."
        );
    }

    #[test]
    fn test_prompt_section_missing() {
        let content = "[system]\nHello\n";
        assert_eq!(prompt_section(content, "nonexistent"), "");
    }

    #[test]
    fn test_content_resolver_project_local() {
        let tmp = TempDir::new().unwrap();
        let factory_dir = tmp.path().join(".factory/prompts");
        std::fs::create_dir_all(&factory_dir).unwrap();
        std::fs::write(factory_dir.join("author.md"), "custom prompt").unwrap();

        let resolver = ContentResolver::new(Some(tmp.path()));
        let path = resolver.resolve_path("prompts/author.md");
        assert!(path.is_some());
        let content = std::fs::read_to_string(path.unwrap()).unwrap();
        assert_eq!(content, "custom prompt");
    }

    #[test]
    fn test_content_resolver_user_config() {
        let tmp = TempDir::new().unwrap();
        let user_config = tmp.path().join("config");
        std::fs::create_dir_all(user_config.join("prompts")).unwrap();
        std::fs::write(user_config.join("prompts/author.md"), "user prompt").unwrap();

        let resolver = ContentResolver {
            project_root: None,
            user_config: user_config.clone(),
        };
        let path = resolver.resolve_path("prompts/author.md");
        assert!(path.is_some());
        let content = std::fs::read_to_string(path.unwrap()).unwrap();
        assert_eq!(content, "user prompt");
    }

    #[test]
    fn test_content_resolver_project_overrides_user_config() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("project");
        let user_config = tmp.path().join("config");

        // Set up both project-local and user-config files
        std::fs::create_dir_all(project.join(".factory/prompts")).unwrap();
        std::fs::write(project.join(".factory/prompts/author.md"), "project prompt").unwrap();
        std::fs::create_dir_all(user_config.join("prompts")).unwrap();
        std::fs::write(user_config.join("prompts/author.md"), "user prompt").unwrap();

        let resolver = ContentResolver {
            project_root: Some(project),
            user_config,
        };
        let content = resolver.resolve_content("prompts/author.md").unwrap();
        assert_eq!(content, "project prompt");
    }

    #[test]
    fn test_content_resolver_bundled_fallback() {
        let resolver = ContentResolver::new(None);
        let content = resolver.resolve_content("prompts/author.md");
        assert!(content.is_some());
        assert!(content.unwrap().contains("Status file contract"));
    }

    #[test]
    fn test_bundled_content_prompts() {
        assert!(bundled_content("prompts/author.md").is_some());
        assert!(bundled_content("prompts/review-architecture.md").is_some());
        assert!(bundled_content("prompts/review-behaviors.md").is_some());
        assert!(bundled_content("prompts/review-documentation.md").is_some());
        assert!(bundled_content("prompts/review-skills.md").is_some());
        assert!(bundled_content("prompts/review-tests.md").is_some());
    }

    #[test]
    fn test_bundled_content_sandbox() {
        assert!(bundled_content("sandbox/common.sb").is_some());
        assert!(bundled_content("sandbox/claude-code.sb").is_some());
        assert!(bundled_content("sandbox/codex.sb").is_some());
    }

    #[test]
    fn test_bundled_content_does_not_include_agent_managed_content() {
        assert!(bundled_content("skills/build-in-the-factory/SKILL.md").is_none());
        assert!(bundled_content("expertise/architecture.md").is_none());
        assert!(bundled_content(".factory/expertise/testing.md").is_none());
    }

    #[test]
    fn test_bundled_content_missing() {
        assert!(bundled_content("nonexistent").is_none());
    }
}
