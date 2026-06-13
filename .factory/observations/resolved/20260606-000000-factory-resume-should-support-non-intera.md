2026-06-06 — `factory resume` should support non-interactive automation
or provide a separate headless resume path. During run curation,
`factory resume 20260606-run-curation --coder codex` failed with
`stdin is not a terminal`, while `factory run --run-id
20260606-run-curation --coder codex` could continue the run. Automation
should not have to know that distinction, and a resume path should be
usable from scripts, agents, or other non-TTY orchestrators when the
intent is to restart the session loop rather than attach interactively.
→ Resolved: bd82a58, a2f8d84, e057ae7, c757421, 53077d6 (headless
resume restarts selected or implicit resumable runs, rejects parallel
parent runs, and documents the selection behavior)
