You are capturing learnings from a completed code review run. Your job: read the review findings and the change that was made, identify any durable project-level learnings, and merge them into the project's expertise model.

## Inputs

### Review findings

The following review artifacts contain the findings from this run's review rounds:

{{review_artifact_paths}}

Read each file. Focus on concrete observations reviewers made about the project — conventions, patterns, gotchas, architectural decisions — not on the specifics of the change under review.

### Change produced

Run the following command to see the change this run produced:

```sh
{{diff_command}}
```

### Existing learnings

The current learnings directory is at `{{learnings_dir}}`.
{{#if has_learnings_index}}The current index is at `{{learnings_index_path}}`.{{/if}}

Read the existing files before writing, so you merge rather than duplicate.

## What to capture

Record only durable, project-level learnings — facts or principles that would help a future writer or reviewer orient to this project. Examples:

- A convention the reviewers enforced (naming, error handling, test structure)
- An architectural constraint or decision the review surfaced
- A gotcha or non-obvious pattern in this codebase

Do NOT record:

- One-off details of this specific change (variable names, line numbers, the particular bug fixed)
- Generic programming advice that applies to any project
- Anything already captured in the existing learnings files

## Output format

Write one file per learning under `{{learnings_dir}}`. Each file uses this frontmatter format:

```markdown
---
name: short-kebab-case-slug
description: one-line summary used to decide relevance in future conversations
metadata:
  type: convention | gotcha | architecture | testing
---

Body text describing the learning. Link related learnings with [[slug]].
```

If a learning file already exists that covers the same topic, update it rather than creating a new file. If an existing learning is wrong or stale based on what you see in the review, update or remove it.

Maintain `{{learnings_index_path}}` — a flat list of learning files with one-line descriptions:

```markdown
# Learnings Index

- [slug](slug.md) — one-line description
```

If the learnings index file does not exist, create it. If it exists, add or update entries. Keep it sorted alphabetically.

If `{{expertise_index_path}}` does not already contain a row pointing to the learnings folder, add one:

```
| learnings/INDEX.md | Durable learnings captured from review runs | When orienting to project conventions or checking known gotchas |
```

## After writing

If you identified any learnings and wrote or updated files, commit with the message "Capture learnings from review run". Do not commit anything else.

If you found no durable learnings worth recording, do not commit. Do not create empty files or placeholder entries.
