2026-06-05 — Fargate Codex support is intentionally not implemented
yet. The Fargate path is still Claude-specific: container image,
entrypoint, auth token injection, and session assumptions all target
Claude Code. Codex support likely needs a container image update,
Codex authentication/config strategy, runtime selection in the task
environment, and tests for launch, session loop, upload/download, and
review artifacts. Until then, `factory run --runtime fargate --coder
codex` should fail clearly instead of starting a run that breaks
halfway through.

→ Resolved: Resolved by Work Item fargate-codex-support at 438d834. Fargate base image now ships both claude-code and codex; entrypoint dispatches on FACTORY_CODER; host-side task launch passes the appropriate auth env var per coder.
