2026-05-10 — Full-codebase reviews should be runs, not a separate
command. The worktree isolation and history are valuable. But the
full brief → behaviors → approach → plan ceremony is heavy for what's
essentially "run all reviewers." Need a lightweight run path — a brief
that says "full review" should skip empty stages and go straight to
execution. Resolve this in the capture-brief or build-in-the-factory
skill.
→ Resolved: 26e2ada (review runs with mode=review skip to planned)
