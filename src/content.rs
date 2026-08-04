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
        "prompts/write-continuation-user.md" => {
            Some(include_str!("../prompts/write-continuation-user.md").to_string())
        }
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
        "prompts/seed-system.md" => Some(include_str!("../prompts/seed-system.md").to_string()),
        "prompts/seed-user.md" => Some(include_str!("../prompts/seed-user.md").to_string()),
        "prompts/learner-system.md" => {
            Some(include_str!("../prompts/learner-system.md").to_string())
        }
        "prompts/learner-user.md" => Some(include_str!("../prompts/learner-user.md").to_string()),
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

    fn assert_ordered(content: &str, expected: &[&str]) {
        let mut offset = 0;
        for needle in expected {
            let relative = content[offset..]
                .find(needle)
                .unwrap_or_else(|| panic!("missing ordered text: {needle:?}"));
            offset += relative + needle.len();
        }
    }

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
        assert!(bundled_content("prompts/write-continuation-user.md").is_some());
        assert!(bundled_content("prompts/review-system.md").is_some());
        assert!(bundled_content("prompts/review-user.md").is_some());
        assert!(bundled_content("prompts/rebase-system.md").is_some());
        assert!(bundled_content("prompts/rebase-user.md").is_some());
        assert!(bundled_content("prompts/seed-system.md").is_some());
        assert!(bundled_content("prompts/seed-user.md").is_some());
        assert!(bundled_content("prompts/learner-system.md").is_some());
        assert!(bundled_content("prompts/learner-user.md").is_some());
    }

    #[test]
    fn bundled_content_resolves_seed_and_learner_system_prompts() {
        let seed = bundled_content("prompts/seed-system.md").unwrap();
        assert!(
            !seed.contains("Fluent Writer"),
            "seed system prompt must not reuse writer identity"
        );

        let learner = bundled_content("prompts/learner-system.md").unwrap();
        assert!(
            !learner.contains("Fluent Writer"),
            "learner system prompt must not reuse writer identity"
        );
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
    fn bundled_fluent_skill_is_full_not_shim() {
        let entries = bundled_skill_files_under("fluent/");
        let skill_md = entries
            .iter()
            .find(|(p, _)| *p == "fluent/SKILL.md")
            .expect("bundled fluent skill must have SKILL.md");
        assert!(
            !skill_md.1.contains("fluent-shim: true"),
            "bundled fluent/SKILL.md must be the full skill, not the shim"
        );
        assert!(
            entries
                .iter()
                .any(|(p, _)| p.starts_with("fluent/references/")),
            "bundled fluent skill must include references"
        );
    }

    #[test]
    fn bundled_fluent_skill_uses_public_description() {
        const DESCRIPTION: &str = "description: Operate Fluent, a self-improving software factory. Use when a user wants to review, build, fix, or improve software with Fluent. Invoke when they ask to install or initialize Fluent; capture an Observation; define a slice; create or refine a Brief, Behavior Specification, Technical Approach, Implementation Plan, or Work Item; run, queue, inspect, resume, or recover an Attempt; review a codebase through Fluent; manage or land a Merge Candidate; capture project Expertise; or configure Fluent's agents, scheduler, sandboxes, or remote execution.";
        let skill = bundled_skill_content("fluent/SKILL.md")
            .expect("bundled fluent skill must have SKILL.md");
        assert!(
            skill.lines().any(|line| line == DESCRIPTION),
            "bundled fluent skill must use the approved public description"
        );
    }

    #[test]
    fn bundled_fluent_skill_documents_local_preview_boundary() {
        let skill = bundled_skill_content("fluent/SKILL.md")
            .expect("bundled fluent skill must have SKILL.md");
        let preview = skill
            .split_once("## Local Preview")
            .and_then(|(_, rest)| rest.split_once("## First-time project setup"))
            .map(|(section, _)| section)
            .expect("the bundled skill must have a bounded Local Preview section");
        assert!(
            preview.contains("locally in the foreground"),
            "must describe local foreground Attempts"
        );
        assert_ordered(
            preview,
            &[
                "proposed follow-up Work",
                "fluent work-item authorize",
                "authorizes and enqueues",
                "fluent scheduler run",
                "successful Learning",
                "ready Merge Candidate",
                "inspected and landed by a human",
                "off by default",
            ],
        );
        assert!(
            preview.contains("Authorization does not run an Attempt")
                && preview.contains("never authorizes landing"),
            "authorization must not be confused with execution or landing"
        );
        assert!(
            preview.contains("The scheduler never lands a candidate")
                && preview.contains("after successful Learning it stops at")
                && preview.contains("a ready Merge Candidate"),
            "the scheduler must stop at a ready candidate only after Learning succeeds"
        );
        assert!(
            preview.contains("--post-merge-review") && preview.contains("positive per-land"),
            "must describe positive per-land post-merge review that is off by default"
        );
    }

    #[test]
    fn bundled_fluent_skill_excludes_automatic_landing() {
        let skill = bundled_skill_content("fluent/SKILL.md")
            .expect("bundled fluent skill must have SKILL.md");
        assert!(
            skill.contains(
                "`fluent auto-merge`, automatic scheduler lifecycle, automatic landing, and"
            ) && skill.contains("Fargate are outside the Local Preview"),
            "must explicitly exclude auto-merge, automatic landing, and Fargate"
        );
        assert!(
            !skill.contains("policy allows autonomous merging"),
            "must not permit policy-based autonomous landing"
        );
        assert!(
            !skill.contains("autonomous execute → review → land"),
            "must not present landing as an autonomous lifecycle stage"
        );
        assert!(
            skill.contains("Only after the user explicitly accepts that candidate"),
            "must require explicit acceptance before landing"
        );
    }

    #[test]
    fn bundled_fluent_skill_offers_coder_profiles_before_init() {
        let skill = bundled_skill_content("fluent/SKILL.md")
            .expect("bundled fluent skill must have SKILL.md");
        let setup = skill
            .split_once("## First-time project setup")
            .map(|(_, section)| section)
            .expect("the bundled skill must have first-time setup guidance");
        assert_ordered(
            setup,
            &[
                "When `.fluent/` does not exist",
                "Before running `fluent init`",
                "(a) propose",
                "(b) execute",
                "Which coder profile should Fluent save",
                "After the user completes both choices, run one configured command",
                "fluent init --coder-profile codex-balanced --follow-up-mode propose",
            ],
        );
        assert!(setup.contains("recommended"));
    }

    #[test]
    fn bundled_fluent_skill_offers_curated_and_custom_profiles() {
        let skill = bundled_skill_content("fluent/SKILL.md")
            .expect("bundled fluent skill must have SKILL.md");
        let setup = skill
            .split_once("## First-time project setup")
            .map(|(_, section)| section)
            .expect("the bundled skill must have first-time setup guidance");
        assert!(
            setup.contains("(a) codex-balanced")
                && setup.contains("(b) codex-stronger")
                && setup.contains("(c) custom")
        );
    }

    #[test]
    fn bundled_fluent_skill_names_complete_curated_profiles() {
        let skill = bundled_skill_content("fluent/SKILL.md").unwrap();
        let setup = skill.split_once("## First-time project setup").unwrap().1;
        assert!(setup.contains("Codex, gpt-5.6-terra, medium effort for the writer,\n       reviewers, and behavior-test coder"));
        assert!(setup.contains("Codex, gpt-5.6-sol, medium effort for the writer,\n       reviewers, and behavior-test coder"));
    }

    #[test]
    fn bundled_fluent_skill_explains_roles_before_custom_profile() {
        let skill = bundled_skill_content("fluent/SKILL.md").unwrap();
        let setup = skill.split_once("## First-time project setup").unwrap().1;
        assert_ordered(
            setup,
            &[
                "If the user chooses `custom`, explain",
                "the writer\n   implements the change",
                "Ask separately for the\n   coder, model, and effort for each role",
            ],
        );
    }

    #[test]
    fn bundled_fluent_skill_orders_profile_preflight_before_init() {
        let skill = bundled_skill_content("fluent/SKILL.md").unwrap();
        let setup = skill.split_once("## First-time project setup").unwrap().1;
        assert_ordered(
            setup,
            &[
                "After the user completes both choices, run one configured command",
                "The command preflights each distinct provider before it initializes the\n   project",
            ],
        );
    }

    #[test]
    fn bundled_fluent_skill_offers_preflight_retry_or_reselection() {
        let skill = bundled_skill_content("fluent/SKILL.md").unwrap();
        assert!(skill.contains("offer the user a retry or a different profile"));
    }

    #[test]
    fn bundled_fluent_skill_bounds_provider_preflight_claim() {
        let skill = bundled_skill_content("fluent/SKILL.md").unwrap();
        assert!(skill.contains("does not promise\n   future provider capacity"));
    }

    #[test]
    fn bundled_fluent_skill_reports_saved_profile_and_attempt_boundary() {
        let skill = bundled_skill_content("fluent/SKILL.md").unwrap();
        assert!(
            skill.contains("show the saved coder, model, and effort")
                && skill.contains("every new Attempt stores\n   the effective mapping unless the user supplies explicit Attempt overrides"),
            "setup must explain saved profile and Attempt freezing"
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

    #[test]
    fn bundled_plan_execution_selects_no_expertise_for_common_sha_work() {
        // The bundled plan-execution skill teaches operators to select
        // `--learner-mode no-expertise` when the contract requires one reviewed
        // Writer SHA to remain unchanged through Learner.
        let skill = bundled_skill_content("fluent/references/plan-execution.md")
            .expect("bundled plan-execution skill must be present");
        assert!(
            skill.contains("--learner-mode no-expertise"),
            "the skill must show the no-expertise create flag"
        );
        assert!(
            skill.contains("remain unchanged through") && skill.contains("reviewed Writer SHA"),
            "the skill must name the one-reviewed-SHA condition"
        );
        assert!(
            skill.contains("trusted macOS host"),
            "the skill must note that no-expertise Work is local-only"
        );
    }

    #[test]
    fn bundled_plan_execution_preserves_capture_by_default() {
        // Ordinary Work omits `--learner-mode` and uses the default capture policy.
        let skill = bundled_skill_content("fluent/references/plan-execution.md")
            .expect("bundled plan-execution skill must be present");
        assert!(
            skill.contains("omit `--learner-mode`") && skill.contains("default to `capture`"),
            "the skill must default ordinary Work to capture"
        );
        assert!(
            skill.contains("Add `--learner-mode no-expertise` only when"),
            "no-expertise must be the exception, not the default"
        );
    }

    #[test]
    fn bundled_plan_execution_selects_mode_before_creating() {
        // The mode decision is an irreversible input to `fluent work-item create`,
        // so the skill must teach selecting the mode before it gives any instruction
        // to create. A later conditional cannot undo a default create already run.
        let skill = bundled_skill_content("fluent/references/plan-execution.md")
            .expect("bundled plan-execution skill must be present");
        let select_at = skill
            .find("Select the Learner mode")
            .expect("the skill must have a Learner-mode selection step");
        let first_create_at = skill
            .find("fluent work-item create")
            .expect("the skill must show a create command");
        assert!(
            select_at < first_create_at,
            "the Learner-mode decision must precede the first create command"
        );
        // Both create branches are present and each retains its correct flag: the
        // capture branch is introduced before the no-expertise branch, and the
        // no-expertise command carries the flag.
        assert_ordered(
            skill,
            &[
                "Select the Learner mode",
                "If you selected capture",
                "If you selected no-expertise",
                "--learner-mode no-expertise",
            ],
        );
        assert!(
            skill.contains("mutually exclusive"),
            "the two create commands must be presented as mutually exclusive branches"
        );
    }

    #[test]
    fn living_behaviors_cover_no_expertise_prompt_and_create_order_contracts() {
        // B11d: the living behavior documentation carries the shared production prompt
        // construction contract, the failure-before-coder-construction contract, and
        // the per-Work-Item mode-selection-before-creation contract with its mutually
        // exclusive capture and no-expertise create forms.
        let behaviors = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/documentation/behaviors.md"
        ))
        .expect("documentation/behaviors.md must be present");

        assert!(
            behaviors.contains("one shared production builder that both the")
                && behaviors.contains("initial audit and the bounded")
                && behaviors
                    .contains("schema-repair invocation in capture, no-expertise, and post-land"),
            "living behaviors must describe shared production prompt construction for every mode and invocation"
        );
        assert!(
            behaviors.contains("before constructing or launching the")
                && behaviors
                    .contains("no coder is ever built or launched from a mis-rendered prompt"),
            "living behaviors must describe returning the prompt error before coder construction"
        );
        assert!(
            behaviors.contains("select the Learner mode before any create command")
                && behaviors.contains("mutually exclusive branches")
                && behaviors.contains("fixes the mode irreversibly"),
            "living behaviors must describe selecting the mode before creation with mutually exclusive create forms"
        );
    }

    #[test]
    fn no_expertise_docs_state_residual_out_of_band_git_race() {
        // B4m: architecture states the model-lock transaction makes supported
        // persisted-state transitions atomic but cannot serialize an arbitrary process
        // that edits candidate Git after the last Git read; closing that residual race
        // would need a universal candidate-workspace lock, out of scope here.
        let architecture = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/documentation/architecture.md"
        ))
        .expect("documentation/architecture.md must be present");

        assert!(
            architecture.contains("supported persisted-state") && architecture.contains("atomic"),
            "architecture must distinguish atomic supported persisted-state transitions"
        );
        assert!(
            architecture.contains("edits candidate Git")
                && architecture.contains("after")
                && architecture.contains("residual race"),
            "architecture must name the residual out-of-band candidate-Git race after the last read"
        );
        assert!(
            architecture.contains("universal lock shared by every candidate-workspace Git writer")
                && architecture.contains("outside this release correction"),
            "architecture must state the universal candidate-workspace Git lock is out of scope"
        );
    }

    /// The first ```` ```sh ```` fenced block that appears after `marker` in `text`.
    fn sh_block_after<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
        let start = text.find(marker)?;
        let after = &text[start..];
        let fence = after.find("```sh")?;
        let body = &after[fence + "```sh".len()..];
        let end = body.find("```")?;
        Some(&body[..end])
    }

    /// Tokenize a fenced shell command, treating line-continuation backslashes as
    /// separators rather than tokens, so a flag and its value are adjacent tokens.
    fn shell_tokens(block: &str) -> Vec<String> {
        block
            .replace('\\', " ")
            .split_whitespace()
            .map(str::to_string)
            .collect()
    }

    /// Every value token immediately following a `--learner-mode` flag token.
    fn learner_mode_values(tokens: &[String]) -> Vec<String> {
        tokens
            .iter()
            .enumerate()
            .filter(|(_, token)| token.as_str() == "--learner-mode")
            .map(|(index, _)| tokens.get(index + 1).cloned().unwrap_or_default())
            .collect()
    }

    #[test]
    fn bundled_plan_execution_create_blocks_require_exact_learner_mode_tokens() {
        // B11f: parse the fenced create commands into tokens so the value following the
        // single `--learner-mode` flag is EXACTLY `no-expertise`. A prefix or suffix
        // such as `no-expertise-invalid` fails. The capture command carries no
        // `--learner-mode` token.
        let skill = bundled_skill_content("fluent/references/plan-execution.md")
            .expect("bundled plan-execution skill must be present");

        let capture = sh_block_after(&skill, "If you selected capture")
            .expect("the skill must show a capture create block");
        let no_expertise = sh_block_after(&skill, "If you selected no-expertise")
            .expect("the skill must show a no-expertise create block");

        assert_eq!(
            capture.matches("fluent work-item create").count(),
            1,
            "the capture block must hold exactly one create command"
        );
        assert_eq!(
            no_expertise.matches("fluent work-item create").count(),
            1,
            "the no-expertise block must hold exactly one create command"
        );

        let capture_tokens = shell_tokens(capture);
        assert!(
            capture_tokens.iter().all(|token| token != "--learner-mode"),
            "the capture create command must carry no --learner-mode token"
        );

        let no_expertise_tokens = shell_tokens(no_expertise);
        let values = learner_mode_values(&no_expertise_tokens);
        assert_eq!(
            values,
            vec!["no-expertise".to_string()],
            "the no-expertise command must carry exactly one --learner-mode flag whose value is \
             exactly `no-expertise`"
        );

        // Exact token comparison rejects a near-match value that a substring check
        // (`contains(\"--learner-mode no-expertise\")`) would wrongly accept.
        let near_miss =
            shell_tokens("fluent work-item create x --learner-mode no-expertise-invalid");
        assert_ne!(
            learner_mode_values(&near_miss),
            vec!["no-expertise".to_string()],
            "a prefix or suffix such as `no-expertise-invalid` must fail the exact-token check"
        );
    }

    #[test]
    fn no_expertise_docs_describe_lock_held_publication_transaction() {
        // B4t: architecture distinguishes the first lock-held InProgress -> HandoffPending
        // preparation from the second lock-held final identity check, handoff
        // publication, and terminal settlement; describes the reviewed-identity
        // transition guard and both transaction-journal recovery outcomes; and retains
        // only out-of-band candidate Git mutation after the final Git read as the
        // residual race.
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/documentation/architecture.md"
        ))
        .expect("documentation/architecture.md must be present");
        // Collapse hard-wrap whitespace so phrase checks are insensitive to line breaks.
        let architecture = raw.split_whitespace().collect::<Vec<_>>().join(" ");

        assert!(
            architecture.contains("first lock-held phase")
                && architecture.contains("advances Learning `InProgress` to `HandoffPending`"),
            "architecture must describe the first lock-held InProgress -> HandoffPending preparation"
        );
        assert!(
            architecture.contains("second lock-held phase")
                && architecture
                    .contains("publishes the canonical handoff while the model lock is still held")
                && architecture.contains("settles the same aggregate to `Succeeded`"),
            "architecture must describe the second lock-held final check, publication, and settlement"
        );
        assert!(
            architecture.contains("share one model-mutation boundary")
                && architecture
                    .contains("no supported concurrent Work-model transition can interleave"),
            "architecture must state the final transaction is one uninterruptible boundary"
        );
        assert!(
            architecture.contains("reviewed-identity transition guard"),
            "architecture must describe the reviewed-identity transition guard"
        );
        assert!(
            architecture.contains("two transaction-journal outcomes")
                && architecture.contains("reruns only the Learner")
                && architecture.contains("finishes that exact journal to `Succeeded`"),
            "architecture must describe both safe transaction-journal recovery outcomes"
        );
        assert!(
            architecture.contains("edits candidate Git") && architecture.contains("residual race"),
            "architecture must retain only out-of-band candidate Git mutation as the residual race"
        );
    }

    /// Read a repo-relative living document and collapse hard-wrap whitespace so
    /// phrase checks are insensitive to line breaks.
    fn read_living_doc(relative: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{relative} must be present: {e}"));
        raw.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Extract the text of a living document between two stable section anchors so a
    /// contract check binds to one bounded section: unrelated text elsewhere cannot
    /// satisfy a positive claim and a contradictory sentence elsewhere cannot coexist.
    /// Fails if either anchor is absent, ambiguously duplicated, or out of order.
    fn bounded_section<'a>(doc: &'a str, start_anchor: &str, end_anchor: &str) -> &'a str {
        let starts: Vec<usize> = doc
            .match_indices(start_anchor)
            .map(|(idx, _)| idx)
            .collect();
        assert_eq!(
            starts.len(),
            1,
            "start anchor {start_anchor:?} must appear exactly once (found {})",
            starts.len()
        );
        let ends: Vec<usize> = doc.match_indices(end_anchor).map(|(idx, _)| idx).collect();
        assert_eq!(
            ends.len(),
            1,
            "end anchor {end_anchor:?} must appear exactly once (found {})",
            ends.len()
        );
        let start = starts[0];
        let end = ends[0];
        assert!(
            start < end,
            "start anchor {start_anchor:?} must precede end anchor {end_anchor:?}"
        );
        &doc[start..end]
    }

    #[test]
    fn living_no_expertise_contract_names_two_phase_publication_and_recovery() {
        // B4ar: the living no-expertise contract is enforced per bounded document
        // section, so unrelated text elsewhere cannot satisfy a positive claim and a
        // contradictory sentence elsewhere cannot coexist. The bounded no-expertise
        // section of BOTH documentation/behaviors.md and documentation/architecture.md
        // independently names the two lock-held publication phases, the frozen
        // reviewed-identity guard and its InProgress/relaunchable rules, both
        // transaction-journal recovery outcomes, and the Generic-vs-typed publisher
        // classification — and each slice rejects the stale blanket claim that every
        // publication failure is Generic.
        let architecture = read_living_doc("documentation/architecture.md");
        let behaviors = read_living_doc("documentation/behaviors.md");
        let decisions = read_living_doc(".fluent/expertise/decisions.md");
        let reserved =
            read_living_doc(".fluent/expertise/learnings/reserved-phase-terminal-finalizer.md");
        let field_finalizer = read_living_doc(
            ".fluent/expertise/learnings/fresh-field-level-finalizer-preserves-concurrent-state.md",
        );
        let route_tests =
            read_living_doc(".fluent/expertise/learnings/route-tests-drive-real-launch-wiring.md");
        let mode_prompts = read_living_doc(
            ".fluent/expertise/learnings/mode-specific-prompts-replace-conflicting-base-instructions.md",
        );

        // Bound each living document to its no-expertise section. Extraction fails if
        // either anchor is missing, ambiguously duplicated, or out of order — no
        // whole-document fallback remains.
        let behaviors_slice = bounded_section(
            &behaviors,
            "## Selectable pre-land Learner policy",
            "## Corrective classification and Work authorization",
        );
        let architecture_slice = bounded_section(
            &architecture,
            "A `no-expertise` pre-land Learner instead",
            "A relaunchable Learner run that failed before its candidate landed recovers",
        );

        // The two-phase publication, recovery, and classification contract is required
        // INSIDE each bounded slice independently.
        for (name, slice) in [
            ("behaviors", behaviors_slice),
            ("architecture", architecture_slice),
        ] {
            // 1. First lock-held phase: fresh identity/cleanliness recheck and the
            //    InProgress -> HandoffPending (or typed Failed) transition.
            assert!(
                slice.contains("`prepare_no_expertise_handoff`"),
                "{name} slice must name the first lock-held publication phase"
            );
            assert!(
                slice.contains("`InProgress` to `HandoffPending`"),
                "{name} slice must describe the prepared InProgress -> HandoffPending transition"
            );
            // 2. Separately lock-held second phase: final identity check, canonical
            //    publication, and the HandoffPending -> Succeeded (or typed Failed) result.
            assert!(
                slice.contains("`publish_no_expertise_handoff`"),
                "{name} slice must name the second lock-held publication phase"
            );
            assert!(
                slice.contains("`Succeeded`"),
                "{name} slice must describe settling the published handoff to Succeeded"
            );
            // 3. Frozen reviewed identity only in HandoffPending/Succeeded, InProgress
            //    changes detectable, relaunchable Failed repairable.
            assert!(
                slice.contains("reviewed-identity transition guard"),
                "{name} slice must name the frozen reviewed-identity transition guard"
            );
            assert!(
                slice.contains("`InProgress`") && slice.contains("repairable"),
                "{name} slice must keep InProgress changes detectable and relaunchable Failed \
                 repairable"
            );
            // 4 & 5. Both transaction-journal recovery outcomes: rerun only the Learner
            //    when no terminal journal is durable, and finish the exact durable journal
            //    to Succeeded without rerunning.
            assert!(
                slice.contains("transaction-journal outcomes"),
                "{name} slice must describe both transaction-journal recovery outcomes"
            );
            assert!(
                slice.contains("without rerunning"),
                "{name} slice must recover the durable terminal journal without rerunning the Learner"
            );
            // 6. Integrity/cleanliness/candidate-mutation classify Generic, while
            //    publisher failures preserve their cause-derived classification.
            assert!(
                slice.contains("`EvidencePending`") && slice.contains("`TranscriptPump`"),
                "{name} slice must name the typed publisher-failure classifications"
            );
            assert!(
                slice.contains("not every handoff-publication failure is `Generic`"),
                "{name} slice must state not every handoff-publication failure is Generic"
            );
            // Negative: reject any stale blanket claim that a publication failure is
            // Generic; the only allowed form is the corrected "not every ... is Generic".
            for (idx, _) in slice.match_indices("handoff-publication failure is `Generic`") {
                assert!(
                    slice[..idx].ends_with("not every "),
                    "{name} slice carries a stale blanket 'every handoff-publication failure is \
                     `Generic`' claim; only the corrected 'not every ...' form is allowed"
                );
            }
        }

        // Architecture slice: the exact-SHA land preconditions, shared scheduling
        // coordinator, capture-only rebase/fix qualification, and HostPreparation seam.
        assert!(
            architecture_slice.contains("identity-preserving exact-SHA route")
                && architecture_slice.contains("skips rebase and provenance regeneration")
                && architecture_slice.contains("never `fix-pre-merge`"),
            "architecture slice must describe the exact-SHA route that skips rebase and never \
             runs fix-pre-merge"
        );
        assert!(
            architecture_slice.contains("Removing that disposable worktree is a land precondition"),
            "architecture slice must state disposable-worktree removal is a land precondition"
        );
        assert!(
            architecture_slice
                .contains("the same target-worktree cleanliness policy capture landing uses")
                && architecture_slice.contains(
                    "before any side effect and again immediately before the fast-forward"
                ),
            "architecture slice must state the target cleanliness policy before side effects and \
             before the fast-forward"
        );
        assert!(
            architecture_slice.contains("shared follow-up-processing and scheduling coordinator")
                && architecture_slice.contains(
                    "schedules the optional post-merge review only after the landed follow-up \
                     result is durably recorded",
                ),
            "architecture slice must describe the shared scheduling coordinator's durability gate"
        );
        assert!(
            architecture_slice.contains("`HostPreparation`")
                && architecture_slice.contains("`HostPreparation::Production`"),
            "architecture slice must name the HostPreparation launch seam"
        );

        // Behaviors slice: the exact-SHA land route without fix-pre-merge, and B14's
        // total already-Merged rule — an absent OR divergent merged_commit fails closed.
        assert!(
            behaviors_slice.contains("identity-preserving exact-SHA route")
                && behaviors_slice.contains("never `fix-pre-merge`"),
            "behaviors slice must describe the exact-SHA land route without fix-pre-merge"
        );
        assert!(
            behaviors_slice.contains("`merged_commit` is absent or differs"),
            "behaviors slice B14 must fail closed when an already-Merged no-expertise \
             merged_commit is absent or divergent"
        );

        // Decisions and single-topic learnings still name the implemented publication
        // function and carry no superseded identifiers or claims.
        assert!(
            decisions.contains("`publish_no_expertise_handoff`"),
            "decisions must name publish_no_expertise_handoff"
        );
        for (name, doc) in [
            ("architecture slice", architecture_slice),
            ("decisions", decisions.as_str()),
            ("reserved-phase learning", reserved.as_str()),
            ("fresh-field-level learning", field_finalizer.as_str()),
        ] {
            assert!(
                !doc.contains("settle_no_expertise_publication"),
                "{name} must not carry the superseded settle_no_expertise_publication identifier"
            );
        }
        assert!(
            route_tests.contains("`HostPreparation`"),
            "the launch-route learning must name the HostPreparation seam"
        );
        assert!(
            !mode_prompts.contains("satisfied with fake executables"),
            "the mode-prompts learning must not claim the host prereq is satisfied with fake \
             executables"
        );
        assert!(
            mode_prompts.contains("injected `HostPreparation` seam"),
            "the mode-prompts learning must describe the injected HostPreparation seam"
        );
    }
}
