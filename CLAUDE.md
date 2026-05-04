# Instructions for Coding Agents

These instructions define how Coding Agents should assist with this project.

## Commit messages

- Use imperative mood and active voice
- Start the subject line with a verb: "Add", "Fix", "Update", "Remove", "Refactor"
- Keep the subject line under 50 characters
- Capitalize the first letter of the subject line
- Do not end the subject line with a period
- Separate the subject from the body with a blank line
- Wrap the body at 72 characters
- Use the body to explain what and why, not how
- Focus on the change itself, not the process of making it
- Write as if completing the sentence: "If applied, this commit will..."
- Do not add yourself as a co-author using Co-Authored-By trailers

## Linear history

Maintain a linear commit history — never create merge commits.

- Rebase feature branches onto main before merging: `git rebase main`
- Fast-forward merge only: `git merge --ff-only <branch>`
- If the fast-forward fails, rebase the branch again and retry

## Documentation

- Don't create too many summary documents and markdown files.

### Use active verb forms

When writing comments, docstrings, commit messages, or documentation, prefer **active verb phrases** over
**nominalized noun phrases**.

Active verbs are clearer, more direct, and easier to scan. They specify who or what performs the action
and reduce ambiguity.

#### Examples

| Avoid (nominalized) | Prefer (active) |
|----------------------|-----------------|
| User authentication handling | Authenticate users |
| WebSocket connection management | Manage WebSocket connections |
| Error logging and reporting | Log and report errors |
| Data validation | Validate data |
| Cache invalidation | Invalidate cache |
| Request processing | Process requests |

#### Prefer "to + verb" over "for + gerund"

When describing what a module or function does, prefer infinitive phrases:

| Avoid | Prefer |
|-------|--------|
| "provides functions for extracting audio" | "provides functions to extract audio" |
| "for downloading and uploading files" | "to download and upload files" |
| "for managing webhooks" | "to manage webhooks" |

#### Where this applies

- Function and method docstrings
- TODO comments
- Commit messages
- PR descriptions
- README sections
- Inline comments explaining intent

#### Exceptions

Nominalized forms are acceptable for class names (`ConnectionManager`, `RequestHandler`), module names,
category labels in structured docs, and domain terms where the noun form is canonical.

#### Quick test

If you can ask "Who does what?" and rewrite to answer that question with a subject and verb, use the active form.

## Interactive commands

Some git commands open an interactive pager that blocks terminal execution. Always pipe output to prevent blocking:

```bash
# Use these patterns to avoid blocking
git log --oneline -10 | cat
git diff | cat
git diff --stat | cat
git show | cat
git branch -a | cat

# Or use --no-pager flag
git --no-pager log --oneline -10
git --no-pager diff
```
