use std::fmt;
use std::path::{Path, PathBuf};

include!(concat!(env!("OUT_DIR"), "/bundled_skills.rs"));

/// Resolve runtime content the Fluent binary reads directly.
///
/// The resolution chain:
/// 1. Project-local: `<project_root>/.fluent/<relative_path>`
/// 2. User config: `~/.config/fluent/<relative_path>`
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
            let path = root.join(".fluent").join(relative);
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
        PathBuf::from(home).join(".config/fluent")
    } else {
        PathBuf::from("/tmp/fluent-config")
    }
}

/// General expertise files bundled with the binary. File names relative to the `expertise/` directory.
pub const GENERAL_EXPERTISE_FILES: &[&str] = &[
    "INDEX.md",
    "README.md",
    "architecture.md",
    "documentation.md",
    "pdf.md",
    "shell-scripts.md",
    "skills.md",
    "terminal-ui.md",
    "tests.md",
    "youtube.md",
];

/// Return the content of a bundled skill file.
/// `relative` is the path within the skills tree (e.g. `review-tests/SKILL.md`).
pub fn bundled_skill_content(relative: &str) -> Option<&'static str> {
    BUNDLED_SKILL_FILES
        .iter()
        .find(|(path, _)| *path == relative)
        .map(|(_, content)| *content)
}

/// Return all bundled skill file entries whose path starts with `prefix`.
pub fn bundled_skill_files_under(prefix: &str) -> Vec<(&'static str, &'static str)> {
    BUNDLED_SKILL_FILES
        .iter()
        .filter(|(path, _)| path.starts_with(prefix))
        .copied()
        .collect()
}

/// Return the list of skill directory names embedded in the binary.
pub fn bundled_skill_names() -> Vec<&'static str> {
    let mut names: Vec<&str> = BUNDLED_SKILL_FILES
        .iter()
        .filter_map(|(path, _)| path.split('/').next())
        .collect();
    names.dedup();
    names
}

/// Bundled runtime content compiled into the binary.
pub fn bundled_content(relative: &str) -> Option<String> {
    // Prompts
    match relative {
        "prompts/write-system.md" => Some(include_str!("../prompts/write-system.md").to_string()),
        "prompts/write-user.md" => Some(include_str!("../prompts/write-user.md").to_string()),
        "prompts/review-system.md" => Some(include_str!("../prompts/review-system.md").to_string()),
        "prompts/review-user.md" => Some(include_str!("../prompts/review-user.md").to_string()),
        "prompts/review-only-system.md" => {
            Some(include_str!("../prompts/review-only-system.md").to_string())
        }
        "prompts/review-only-user.md" => {
            Some(include_str!("../prompts/review-only-user.md").to_string())
        }
        "prompts/rebase-system.md" => Some(include_str!("../prompts/rebase-system.md").to_string()),
        "prompts/rebase-user.md" => Some(include_str!("../prompts/rebase-user.md").to_string()),
        // Sandbox profiles
        "sandbox/common.sb" => Some(include_str!("../sandboxes/common.sb").to_string()),
        "sandbox/claude-code.sb" => Some(include_str!("../sandboxes/claude-code.sb").to_string()),
        "sandbox/codex.sb" => Some(include_str!("../sandboxes/codex.sb").to_string()),
        "sandbox/pi.sb" => Some(include_str!("../sandboxes/pi.sb").to_string()),
        // General expertise
        "expertise/INDEX.md" => Some(include_str!("../expertise/INDEX.md").to_string()),
        "expertise/README.md" => Some(include_str!("../expertise/README.md").to_string()),
        "expertise/architecture.md" => {
            Some(include_str!("../expertise/architecture.md").to_string())
        }
        "expertise/documentation.md" => {
            Some(include_str!("../expertise/documentation.md").to_string())
        }
        "expertise/pdf.md" => Some(include_str!("../expertise/pdf.md").to_string()),
        "expertise/shell-scripts.md" => {
            Some(include_str!("../expertise/shell-scripts.md").to_string())
        }
        "expertise/skills.md" => Some(include_str!("../expertise/skills.md").to_string()),
        "expertise/terminal-ui.md" => Some(include_str!("../expertise/terminal-ui.md").to_string()),
        "expertise/tests.md" => Some(include_str!("../expertise/tests.md").to_string()),
        "expertise/youtube.md" => Some(include_str!("../expertise/youtube.md").to_string()),
        _ => None,
    }
}

/// Render a template with `{{name}}` substitutions and `{{#if name}}...{{else}}...{{/if}}` blocks.
///
/// Syntax:
/// - `{{name}}` substitutes the value of `name` from `ctx`. A missing name is an error.
/// - `{{#if name}}body{{/if}}` renders `body` when `name` is present in `ctx` and non-empty.
/// - `{{#if name}}body{{else}}otherwise{{/if}}` adds an else branch.
/// - `{{{{` in the template renders as a literal `{{` in the output.
/// - When a `{{#if}}`, `{{else}}`, or `{{/if}}` tag is the only non-whitespace content
///   on its line, the tag's entire line — including the trailing newline — is consumed.
///   Variable tags `{{name}}` never strip surrounding whitespace.
///
/// Constraints:
/// - Nested `{{#if}}` blocks are not supported. A nested block is an error.
/// - Tags must close on the same line they open. A `{{` without a `}}` before the next
///   newline is an unclosed-tag error.
pub fn render_template(template: &str, ctx: &[(&str, &str)]) -> Result<String, TemplateError> {
    let tokens = tokenize(template)?;
    validate(template, &tokens)?;
    let mut out = String::with_capacity(template.len());
    render_tokens(template, &tokens, ctx, &mut out)?;
    Ok(out)
}

/// Errors from `render_template`.
#[derive(Debug, PartialEq, Eq)]
pub enum TemplateError {
    UnclosedTag {
        line: usize,
        col: usize,
    },
    UnclosedIf {
        line: usize,
        col: usize,
        name: String,
    },
    UnmatchedEndIf {
        line: usize,
        col: usize,
    },
    UnmatchedElse {
        line: usize,
        col: usize,
    },
    UnknownVariable {
        line: usize,
        col: usize,
        name: String,
        available: Vec<String>,
    },
    EmptyTag {
        line: usize,
        col: usize,
    },
    NestedIf {
        line: usize,
        col: usize,
    },
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnclosedTag { line, col } => write!(
                f,
                "template error at line {line}, col {col}: tag opened with {{{{ but not closed on the same line"
            ),
            Self::UnclosedIf { line, col, name } => write!(
                f,
                "template error at line {line}, col {col}: {{{{#if {name}}}}} block was never closed"
            ),
            Self::UnmatchedEndIf { line, col } => write!(
                f,
                "template error at line {line}, col {col}: {{{{/if}}}} without a matching {{{{#if}}}}"
            ),
            Self::UnmatchedElse { line, col } => write!(
                f,
                "template error at line {line}, col {col}: {{{{else}}}} outside a {{{{#if}}}} block"
            ),
            Self::UnknownVariable {
                line,
                col,
                name,
                available,
            } => write!(
                f,
                "template error at line {line}, col {col}: unknown variable {{{{{name}}}}}. Available: {}",
                available.join(", ")
            ),
            Self::EmptyTag { line, col } => {
                write!(f, "template error at line {line}, col {col}: empty tag")
            }
            Self::NestedIf { line, col } => write!(
                f,
                "template error at line {line}, col {col}: nested {{{{#if}}}} blocks are not supported"
            ),
        }
    }
}

impl std::error::Error for TemplateError {}

#[derive(Debug)]
enum Token<'a> {
    Literal(String),
    Variable { name: &'a str, offset: usize },
    IfStart { name: &'a str, offset: usize },
    Else { offset: usize },
    EndIf { offset: usize },
}

fn tokenize(template: &str) -> Result<Vec<Token<'_>>, TemplateError> {
    let mut tokens: Vec<Token<'_>> = Vec::new();
    let mut pending_literal = String::new();
    let mut cursor = 0;
    let bytes = template.as_bytes();

    while cursor < template.len() {
        let remaining = &template[cursor..];
        let Some(rel) = remaining.find("{{") else {
            pending_literal.push_str(remaining);
            break;
        };
        let tag_start = cursor + rel;
        pending_literal.push_str(&template[cursor..tag_start]);

        // Brace-doubling escape: {{{{ in source -> {{ in output.
        if template[tag_start + 2..].starts_with("{{") {
            pending_literal.push_str("{{");
            cursor = tag_start + 4;
            continue;
        }

        // Find the closing `}}` on the same line as the opening `{{`.
        let after_open = &template[tag_start + 2..];
        let line_text = after_open
            .split_once('\n')
            .map(|(line, _)| line)
            .unwrap_or(after_open);
        let Some(close_rel) = line_text.find("}}") else {
            let (line, col) = line_col(template, tag_start);
            return Err(TemplateError::UnclosedTag { line, col });
        };
        let content = line_text[..close_rel].trim();
        let tag_end = tag_start + 2 + close_rel + 2;

        if content.is_empty() {
            let (line, col) = line_col(template, tag_start);
            return Err(TemplateError::EmptyTag { line, col });
        }

        let block_kind = classify_tag(content);
        let new_token = match block_kind {
            TagKind::Variable(name) => Token::Variable {
                name,
                offset: tag_start,
            },
            TagKind::IfStart(name) => {
                if name.is_empty() {
                    let (line, col) = line_col(template, tag_start);
                    return Err(TemplateError::EmptyTag { line, col });
                }
                Token::IfStart {
                    name,
                    offset: tag_start,
                }
            }
            TagKind::Else => Token::Else { offset: tag_start },
            TagKind::EndIf => Token::EndIf { offset: tag_start },
            TagKind::Invalid => {
                // `#if` with no name, or some other malformed `#`-prefixed tag.
                let (line, col) = line_col(template, tag_start);
                return Err(TemplateError::EmptyTag { line, col });
            }
        };

        let is_block_tag = matches!(
            new_token,
            Token::IfStart { .. } | Token::Else { .. } | Token::EndIf { .. }
        );

        // Standalone-tag whitespace rule applies only to block tags.
        let mut consume_to = tag_end;
        if is_block_tag {
            let leading_ws_start = literal_trailing_ws_start(&pending_literal);
            let trailing_consume = trailing_line_consume(bytes, tag_end);
            let leading_is_standalone = pending_literal[leading_ws_start..]
                .chars()
                .all(|c| c == ' ' || c == '\t');
            let prev_char_is_newline_or_start =
                leading_ws_start == 0 || pending_literal.as_bytes()[leading_ws_start - 1] == b'\n';
            let trailing_is_standalone = trailing_consume.is_some();
            if leading_is_standalone && prev_char_is_newline_or_start && trailing_is_standalone {
                pending_literal.truncate(leading_ws_start);
                consume_to = trailing_consume.unwrap();
            }
        }

        if !pending_literal.is_empty() {
            tokens.push(Token::Literal(std::mem::take(&mut pending_literal)));
        }
        tokens.push(new_token);
        cursor = consume_to;
    }

    if !pending_literal.is_empty() {
        tokens.push(Token::Literal(pending_literal));
    }
    Ok(tokens)
}

enum TagKind<'a> {
    Variable(&'a str),
    IfStart(&'a str),
    Else,
    EndIf,
    Invalid,
}

fn classify_tag(content: &str) -> TagKind<'_> {
    if content == "else" {
        return TagKind::Else;
    }
    if content == "/if" {
        return TagKind::EndIf;
    }
    if let Some(rest) = content.strip_prefix("#if") {
        let name = rest.trim_start();
        if name.is_empty() || rest == name {
            // `#if` with no whitespace before name, or `#if` with nothing after.
            return TagKind::Invalid;
        }
        return TagKind::IfStart(name);
    }
    if content.starts_with('#') || content.starts_with('/') {
        return TagKind::Invalid;
    }
    TagKind::Variable(content)
}

/// Byte index in `s` at which trailing run of ASCII spaces/tabs begins.
fn literal_trailing_ws_start(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut i = bytes.len();
    while i > 0 {
        match bytes[i - 1] {
            b' ' | b'\t' => i -= 1,
            _ => break,
        }
    }
    i
}

/// If the run of bytes at `start..` is "[ \t]*\n" or "[ \t]*$", return the index
/// one past the consumed run. Otherwise return None (tag is not standalone on the right).
fn trailing_line_consume(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    if i == bytes.len() {
        Some(i)
    } else if bytes[i] == b'\n' {
        Some(i + 1)
    } else {
        None
    }
}

/// Validate that `{{#if}}` / `{{else}}` / `{{/if}}` tags are matched and not nested.
fn validate(template: &str, tokens: &[Token<'_>]) -> Result<(), TemplateError> {
    let mut open_if: Option<(&str, usize)> = None;
    let mut saw_else = false;
    for token in tokens {
        match token {
            Token::IfStart { name, offset } => {
                if open_if.is_some() {
                    let (line, col) = line_col(template, *offset);
                    return Err(TemplateError::NestedIf { line, col });
                }
                open_if = Some((name, *offset));
                saw_else = false;
            }
            Token::Else { offset } => {
                if open_if.is_none() {
                    let (line, col) = line_col(template, *offset);
                    return Err(TemplateError::UnmatchedElse { line, col });
                }
                if saw_else {
                    let (line, col) = line_col(template, *offset);
                    return Err(TemplateError::UnmatchedElse { line, col });
                }
                saw_else = true;
            }
            Token::EndIf { offset } => {
                if open_if.is_none() {
                    let (line, col) = line_col(template, *offset);
                    return Err(TemplateError::UnmatchedEndIf { line, col });
                }
                open_if = None;
                saw_else = false;
            }
            _ => {}
        }
    }
    if let Some((name, offset)) = open_if {
        let (line, col) = line_col(template, offset);
        return Err(TemplateError::UnclosedIf {
            line,
            col,
            name: name.to_string(),
        });
    }
    Ok(())
}

fn render_tokens(
    template: &str,
    tokens: &[Token<'_>],
    ctx: &[(&str, &str)],
    out: &mut String,
) -> Result<(), TemplateError> {
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            Token::Literal(s) => {
                out.push_str(s);
                i += 1;
            }
            Token::Variable { name, offset } => match lookup(ctx, name) {
                Some(v) => {
                    out.push_str(v);
                    i += 1;
                }
                None => {
                    let (line, col) = line_col(template, *offset);
                    return Err(TemplateError::UnknownVariable {
                        line,
                        col,
                        name: (*name).to_string(),
                        available: ctx.iter().map(|(k, _)| (*k).to_string()).collect(),
                    });
                }
            },
            Token::IfStart { name, .. } => {
                let truthy = lookup(ctx, name).map(|v| !v.is_empty()).unwrap_or(false);
                let (else_idx, endif_idx) = find_else_endif(tokens, i);
                let endif_idx = endif_idx.expect("validate ensures EndIf");
                if truthy {
                    let body_end = else_idx.unwrap_or(endif_idx);
                    render_tokens(template, &tokens[i + 1..body_end], ctx, out)?;
                } else if let Some(e) = else_idx {
                    render_tokens(template, &tokens[e + 1..endif_idx], ctx, out)?;
                }
                i = endif_idx + 1;
            }
            Token::Else { .. } | Token::EndIf { .. } => {
                unreachable!("Else/EndIf consumed by IfStart branch");
            }
        }
    }
    Ok(())
}

fn lookup<'a>(ctx: &'a [(&str, &str)], name: &str) -> Option<&'a str> {
    ctx.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
}

/// Given an IfStart at `start_idx`, find the matching Else (if any) and EndIf
/// at the same nesting level. Since nesting is forbidden, both can be located
/// by linear scan returning the first match.
fn find_else_endif(tokens: &[Token<'_>], start_idx: usize) -> (Option<usize>, Option<usize>) {
    let mut else_idx = None;
    for (i, token) in tokens.iter().enumerate().skip(start_idx + 1) {
        match token {
            Token::Else { .. } if else_idx.is_none() => else_idx = Some(i),
            Token::EndIf { .. } => return (else_idx, Some(i)),
            _ => {}
        }
    }
    (else_idx, None)
}

/// Convert a byte offset into the original template into (line, col), 1-indexed.
fn line_col(template: &str, offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 1;
    for (i, ch) in template.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
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

[detail]
Check item {{ITEM_ID}}.
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
            prompt_section(content, "detail").trim(),
            "Check item {{ITEM_ID}}."
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
        let fluent_dir = tmp.path().join(".fluent/prompts");
        std::fs::create_dir_all(&fluent_dir).unwrap();
        std::fs::write(fluent_dir.join("write-system.md"), "custom prompt").unwrap();

        let resolver = ContentResolver::new(Some(tmp.path()));
        let path = resolver.resolve_path("prompts/write-system.md");
        assert!(path.is_some());
        let content = std::fs::read_to_string(path.unwrap()).unwrap();
        assert_eq!(content, "custom prompt");
    }

    #[test]
    fn test_content_resolver_user_config() {
        let tmp = TempDir::new().unwrap();
        let user_config = tmp.path().join("config");
        std::fs::create_dir_all(user_config.join("prompts")).unwrap();
        std::fs::write(user_config.join("prompts/write-system.md"), "user prompt").unwrap();

        let resolver = ContentResolver {
            project_root: None,
            user_config: user_config.clone(),
        };
        let path = resolver.resolve_path("prompts/write-system.md");
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
        std::fs::create_dir_all(project.join(".fluent/prompts")).unwrap();
        std::fs::write(
            project.join(".fluent/prompts/write-system.md"),
            "project prompt",
        )
        .unwrap();
        std::fs::create_dir_all(user_config.join("prompts")).unwrap();
        std::fs::write(user_config.join("prompts/write-system.md"), "user prompt").unwrap();

        let resolver = ContentResolver {
            project_root: Some(project),
            user_config,
        };
        let content = resolver.resolve_content("prompts/write-system.md").unwrap();
        assert_eq!(content, "project prompt");
    }

    #[test]
    fn test_content_resolver_bundled_fallback() {
        let resolver = ContentResolver::new(None);
        let content = resolver.resolve_content("prompts/write-system.md");
        assert!(content.is_some());
        assert!(content.unwrap().contains("Fluent Writer"));
    }

    #[test]
    fn test_bundled_content_prompts() {
        assert!(bundled_content("prompts/write-system.md").is_some());
        assert!(bundled_content("prompts/write-user.md").is_some());
        assert!(bundled_content("prompts/review-system.md").is_some());
        assert!(bundled_content("prompts/review-user.md").is_some());
        assert!(bundled_content("prompts/rebase-system.md").is_some());
        assert!(bundled_content("prompts/rebase-user.md").is_some());
    }

    #[test]
    fn bundled_write_system_prompt_avoids_legacy_run_state_contract() {
        let content = bundled_content("prompts/write-system.md").unwrap();

        assert!(content.contains("Fluent Writer"));
        assert!(!content.contains("Status file contract"));
        assert!(!content.contains(".fluent/runs/"));
        assert!(!content.contains("handoff.md"));
    }

    #[test]
    fn test_bundled_content_sandbox() {
        assert!(bundled_content("sandbox/common.sb").is_some());
        assert!(bundled_content("sandbox/claude-code.sb").is_some());
        assert!(bundled_content("sandbox/codex.sb").is_some());
    }

    #[test]
    fn test_bundled_content_does_not_include_agent_managed_content() {
        // Skills are bundled through BUNDLED_SKILL_FILES, not through bundled_content.
        assert!(bundled_content("skills/fluent/SKILL.md").is_none());
        assert!(bundled_content(".fluent/expertise/testing.md").is_none());
    }

    #[test]
    fn bundled_skill_files_include_review_skills() {
        for role in &[
            "architecture",
            "behaviors",
            "documentation",
            "skills",
            "tests",
        ] {
            let skill_path = format!("review-{role}/SKILL.md");
            assert!(
                bundled_skill_content(&skill_path).is_some(),
                "expected bundled skill content for {skill_path}"
            );
        }
    }

    #[test]
    fn bundled_skill_files_dereference_symlinks() {
        let content = bundled_skill_content("review-architecture/references/architecture.md");
        assert!(
            content.is_some(),
            "review-architecture/references/architecture.md should be bundled"
        );
        let body = content.unwrap();
        assert!(
            !body.is_empty(),
            "dereferenced reference should have content"
        );
    }

    #[test]
    fn bundled_skill_names_lists_all_skills() {
        let names = bundled_skill_names();
        assert!(
            names.contains(&"review-tests"),
            "should contain review-tests"
        );
        assert!(names.contains(&"fluent"), "should contain fluent");
    }

    #[test]
    fn bundled_skill_files_under_returns_matching_entries() {
        let entries = bundled_skill_files_under("review-tests/");
        assert!(
            entries.len() >= 2,
            "review-tests should have SKILL.md and at least one reference"
        );
        assert!(
            entries.iter().any(|(p, _)| *p == "review-tests/SKILL.md"),
            "should contain SKILL.md"
        );
    }

    #[test]
    fn test_bundled_content_expertise() {
        for name in GENERAL_EXPERTISE_FILES {
            let key = format!("expertise/{name}");
            assert!(
                bundled_content(&key).is_some(),
                "expected bundled content for {key}"
            );
        }
    }

    #[test]
    fn test_bundled_content_missing() {
        assert!(bundled_content("nonexistent").is_none());
    }

    // ---- render_template tests ----

    #[test]
    fn render_no_tags_is_identity() {
        let out = render_template("Hello world.\nNo tags here.", &[]).unwrap();
        assert_eq!(out, "Hello world.\nNo tags here.");
    }

    #[test]
    fn render_simple_substitution() {
        let out = render_template("Hello {{name}}.", &[("name", "Alice")]).unwrap();
        assert_eq!(out, "Hello Alice.");
    }

    #[test]
    fn render_multiple_substitutions() {
        let out = render_template(
            "{{greeting}}, {{name}}!",
            &[("greeting", "Hi"), ("name", "Bob")],
        )
        .unwrap();
        assert_eq!(out, "Hi, Bob!");
    }

    #[test]
    fn render_substitution_with_empty_value() {
        let out = render_template("[{{x}}]", &[("x", "")]).unwrap();
        assert_eq!(out, "[]");
    }

    #[test]
    fn render_missing_variable_errors_with_available_list() {
        let err = render_template("Hello {{name}}.", &[("greeting", "Hi")]).unwrap_err();
        match err {
            TemplateError::UnknownVariable {
                name, available, ..
            } => {
                assert_eq!(name, "name");
                assert_eq!(available, vec!["greeting".to_string()]);
            }
            other => panic!("expected UnknownVariable, got {other:?}"),
        }
    }

    #[test]
    fn render_empty_tag_errors() {
        let err = render_template("{{}}", &[]).unwrap_err();
        assert!(matches!(err, TemplateError::EmptyTag { .. }));
    }

    #[test]
    fn render_whitespace_only_tag_errors() {
        let err = render_template("{{   }}", &[]).unwrap_err();
        assert!(matches!(err, TemplateError::EmptyTag { .. }));
    }

    #[test]
    fn render_unclosed_tag_errors_with_line_col() {
        let err = render_template("line 1\nline 2 {{name no close\nline 3", &[]).unwrap_err();
        match err {
            TemplateError::UnclosedTag { line, col } => {
                assert_eq!(line, 2);
                assert_eq!(col, 8);
            }
            other => panic!("expected UnclosedTag, got {other:?}"),
        }
    }

    #[test]
    fn render_if_truthy_renders_body() {
        let out = render_template("a{{#if x}}b{{/if}}c", &[("x", "v")]).unwrap();
        assert_eq!(out, "abc");
    }

    #[test]
    fn render_if_empty_value_skips_body() {
        let out = render_template("a{{#if x}}b{{/if}}c", &[("x", "")]).unwrap();
        assert_eq!(out, "ac");
    }

    #[test]
    fn render_if_missing_key_skips_body() {
        let out = render_template("a{{#if x}}b{{/if}}c", &[]).unwrap();
        assert_eq!(out, "ac");
    }

    #[test]
    fn render_if_truthy_with_else_skips_else_branch() {
        let out = render_template("a{{#if x}}B{{else}}E{{/if}}c", &[("x", "v")]).unwrap();
        assert_eq!(out, "aBc");
    }

    #[test]
    fn render_if_falsy_with_else_renders_else_branch() {
        let out = render_template("a{{#if x}}B{{else}}E{{/if}}c", &[("x", "")]).unwrap();
        assert_eq!(out, "aEc");
    }

    #[test]
    fn render_unmatched_endif_errors() {
        let err = render_template("body {{/if}}", &[]).unwrap_err();
        assert!(matches!(err, TemplateError::UnmatchedEndIf { .. }));
    }

    #[test]
    fn render_unmatched_else_errors() {
        let err = render_template("body {{else}} more", &[]).unwrap_err();
        assert!(matches!(err, TemplateError::UnmatchedElse { .. }));
    }

    #[test]
    fn render_unclosed_if_errors() {
        let err = render_template("a{{#if x}}body", &[("x", "v")]).unwrap_err();
        match err {
            TemplateError::UnclosedIf { name, .. } => assert_eq!(name, "x"),
            other => panic!("expected UnclosedIf, got {other:?}"),
        }
    }

    #[test]
    fn render_nested_if_errors() {
        let err = render_template(
            "a{{#if x}}{{#if y}}b{{/if}}{{/if}}",
            &[("x", "v"), ("y", "v")],
        )
        .unwrap_err();
        assert!(matches!(err, TemplateError::NestedIf { .. }));
    }

    #[test]
    fn render_brace_doubling_escapes_to_literal_braces() {
        let out = render_template("use {{{{name}} as a literal", &[]).unwrap();
        assert_eq!(out, "use {{name}} as a literal");
    }

    #[test]
    fn render_brace_doubling_around_substitution() {
        let out = render_template("literal {{{{ then {{name}}", &[("name", "value")]).unwrap();
        assert_eq!(out, "literal {{ then value");
    }

    #[test]
    fn render_standalone_if_consumes_whole_line() {
        // {{#if foo}} on its own line, {{/if}} on its own line.
        // Body line is kept; surrounding tag lines vanish.
        let template = "before\n{{#if foo}}\nbody\n{{/if}}\nafter\n";
        let out = render_template(template, &[("foo", "v")]).unwrap();
        assert_eq!(out, "before\nbody\nafter\n");
    }

    #[test]
    fn render_standalone_if_falsy_consumes_whole_block() {
        let template = "before\n{{#if foo}}\nbody\n{{/if}}\nafter\n";
        let out = render_template(template, &[("foo", "")]).unwrap();
        assert_eq!(out, "before\nafter\n");
    }

    #[test]
    fn render_standalone_else_branches_render_cleanly() {
        let template = "a\n{{#if foo}}\nyes\n{{else}}\nno\n{{/if}}\nz\n";
        let out_true = render_template(template, &[("foo", "v")]).unwrap();
        assert_eq!(out_true, "a\nyes\nz\n");
        let out_false = render_template(template, &[("foo", "")]).unwrap();
        assert_eq!(out_false, "a\nno\nz\n");
    }

    #[test]
    fn render_standalone_tag_with_indent_strips_indent() {
        let template = "x\n  {{#if a}}\nbody\n  {{/if}}\ny\n";
        let out = render_template(template, &[("a", "v")]).unwrap();
        assert_eq!(out, "x\nbody\ny\n");
    }

    #[test]
    fn render_inline_block_tag_keeps_surrounding_text() {
        // {{#if x}} not on its own line — surrounding text preserved.
        let out = render_template("a{{#if x}}B{{/if}}c", &[("x", "v")]).unwrap();
        assert_eq!(out, "aBc");
    }

    #[test]
    fn render_variable_tag_never_strips_whitespace() {
        // Even if {{name}} appears alone on a line in source, it should NOT
        // consume its surrounding newlines — only block tags get that treatment.
        let template = "before\n{{name}}\nafter\n";
        let out = render_template(template, &[("name", "MID")]).unwrap();
        assert_eq!(out, "before\nMID\nafter\n");
    }

    #[test]
    fn render_consecutive_substitutions_work() {
        let out =
            render_template("{{a}}{{b}}{{c}}", &[("a", "1"), ("b", "2"), ("c", "3")]).unwrap();
        assert_eq!(out, "123");
    }

    #[test]
    fn render_value_containing_braces_is_not_re_parsed() {
        // If a substituted value contains `{{`, it must appear verbatim in output
        // and NOT trigger further tag parsing.
        let out = render_template("value is {{x}}", &[("x", "literal {{not_a_tag}}")]).unwrap();
        assert_eq!(out, "value is literal {{not_a_tag}}");
    }

    #[test]
    fn render_if_with_dashes_in_name() {
        let out =
            render_template("{{#if review_only}}A{{/if}}", &[("review_only", "yes")]).unwrap();
        assert_eq!(out, "A");
    }
}
