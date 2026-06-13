2026-06-07 — Running Factory Codex child sessions from inside a Codex
conversation still fails when the outer Codex session is launched with
restricted network/app-server permissions, even if the filesystem roots
allow sibling worktrees. `factory --no-sandbox resume
20260607-183819-attempt-intake --coder codex` bypassed Factory's
Seatbelt wrapper, but nested `codex exec` failed immediately with
`failed to initialize in-process app-server client: Operation not
permitted`. `codex doctor` in the same shell reported restricted
network, unreachable ChatGPT endpoints, and an idle app-server. This is
separate from worktree permissions: the conversation-hosted Codex agent
needs a launch mode that allows the delegated Codex runtime to initialize
and reach the model endpoint, or Factory needs a different Codex
execution surface for nested runs.
