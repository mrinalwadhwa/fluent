2026-06-13 — Anthropic OAuth tokens expire silently within long
Factory sessions, causing the next agent invocation to fail with
401 instead of pausing or refreshing.

Concrete incident: behavior-tests-task Attempt-1 finished reviews,
sat for 6+ hours waiting for manual merge, and when factory work
merge was finally invoked the agentic rebase Task's first request
hit 401 Invalid authentication credentials. The merge failed cold
and required the user to /login in the Claude Code session before
retrying. The same 401 pattern happened earlier in the day during
the parallel-three test.

The CLAUDE_CODE_OAUTH_TOKEN keychain-managed token has a finite
lifetime. Factory's session loop doesn't refresh it; the Coder
abstraction just shells out to claude / codex and expects the
authentication to work.

Possible improvements (all separate from the merge auto-trigger
work):

- Pre-flight check before any agent invocation: claude doctor
  --quick (or equivalent) verifies the token is still valid, fails
  the Task fast with a clearer error if not.
- Periodic refresh: a background heartbeat that triggers a no-op
  authenticated request every N minutes to keep the token live.
  Risks hiding real auth failures behind retry-on-401.
- Surface needs-user with a specific reason: "authentication
  expired" → user runs /login → factory work merge retry resumes
  from where it failed.

Related: the merge auto-trigger work (separate observation) makes
this harder, not easier, because the auto-trigger would fire
during the same long-lived session and hit the same expired token.

Probably needs to be addressed as part of any long-session
automation work.
