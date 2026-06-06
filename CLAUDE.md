# Instructions for Coding Agents

These instructions define how Coding Agents should assist with this project.

## Factory workflow

Use the factory to build the factory. For non-trivial code,
documentation, skill, expertise, or behavior changes, follow the
`build-in-the-factory` skill and go through the full run lifecycle:
brief, behaviors, approach, plan, execution, review, and land.

Do not implement substantial product/code changes directly on `main`.
Use Factory runs for delegated build work that needs isolation,
reviewers, and landing.

Conversation agents may edit Factory planning and memory state directly
when they are collaborating with the user in the discussion loop:
observations, briefs, behavior drafts, approaches, plans, lightweight
curation, and similar durable notes. These edits are part of shaping
work, not delegated run execution.

Do not meddle with live run execution state directly: run branches,
worktrees, statuses, session artifacts, child-run metadata, and landing
state belong to the run system. Modify them only during explicit
recovery with the user.

Keep `main` available as a stable integration branch for runs to rebase
from and merge into. If conversation-state edits could overlap with
active runs or parent landing, make them on a lightweight discussion
branch or worktree and land them separately instead of dirtying `main`.
Use `.factory/observations.md` to record future work and lessons.

## Commit messages

### Subject line
- Use imperative mood and active voice
- Start with a verb: "Add", "Fix", "Update", "Remove", "Refactor"
- Keep under 50 characters
- Capitalize the first letter
- Do not end with a period
- Describe the change, not the process that led to it
  - Good: "Fix sandbox worktree binding"
  - Bad: "Run review and fix issues found"
  - Bad: "Address reviewer findings"
- Use "Improve" over "Fix" when the change enhances something
  that was working but could be better. "Fix" implies it was broken.

### Body
- Separate from the subject with a blank line
- Use bullet points (- prefix) for listing changes
- Wrap at 72 characters
- Explain what changed and why, not how
- Do not reference the process: no "from review run," "based on
  reviewer feedback," or "as part of run X"

### Prohibited
- Do not add Co-Authored-By trailers
- Do not reference run IDs, review artifacts, or factory internals
- Do not include counts or statistics: "fix 12 issues," "remove
  3,000 lines," "update 47 files." The diff shows the numbers.
  The message describes the change.

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
