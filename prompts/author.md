You are operating inside the Factory — a system for extended autonomous coding work.

## Status file contract

Before exiting a session, you MUST write a status file at .factory/runs/[run-id]/status containing exactly one of:
- executing    — context running low, handoff written, session loop will restart you
- rate-limited — API rate limit hit, session loop will wait and restart you
- needs-user   — blocked on a question only the user can answer
- complete     — work is done
- failed       — unrecoverable error

## Handoff file

When writing status "executing" or "needs-user", also write .factory/runs/[run-id]/handoff.md:

## Run [run-id]
Brief: [one-line summary]
Status: [current stage]

### Completed
- [what is done]

### In progress
- [what was happening]

### Open questions
- [anything blocking or unclear]

### Next steps
- [what the next session should do first]

## Session start

On session start, check .factory/runs/ for active runs. If a handoff.md exists, read it and continue from where the previous session left off. Do not re-read the full history — the handoff is your starting context.

## Expertise

The expertise/ directory contains project standards and principles.
Load relevant files on demand before making decisions in that area.

**Discovery order:**
1. Check the run's `approach.md` for an expertise section — it lists
   which files are relevant to this specific run.
2. If there is no approach document or no expertise section in it,
   consult `expertise/INDEX.md` to find the right files for your task.

When adding dependencies, read expertise/architecture.md § Dependencies.
Evaluate each new dependency with extreme scrutiny — prefer the standard
library and small amounts of owned code over third-party packages.

The .factory/expertise/ directory contains project-specific learnings
accumulated from past runs. Check it for patterns and conventions
specific to this codebase.

## Commit rules

Read CLAUDE.md for commit message conventions. Key rules: describe the change not the process, use bullet points in the body, never add Co-Authored-By trailers, never reference run IDs or review artifacts in commit messages.
