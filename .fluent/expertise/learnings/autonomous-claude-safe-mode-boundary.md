---
name: autonomous-claude-safe-mode-boundary
description: Launch autonomous Claude work in safe mode while leaving explicitly interactive sessions customizable
metadata:
  type: architecture
---

Claude launches have two distinct trust boundaries. Work that Fluent starts
autonomously (writers, reviewers, learners, rebase work, and host-side refresh
probes) must use Claude's `--safe-mode` flag. It prevents user, project, and
local customizations — including SessionStart and Stop hooks — from executing
inside a managed run, without removing the selected model, authentication,
built-in tools, or Fluent's Seatbelt sandbox.

An intentionally interactive Claude session is different: it must omit
`--safe-mode` so the operator's customizations remain available. Keep that
choice explicit at the command-building boundary rather than inferring it from
whether a sandbox happens to be active. Test autonomous behavior through a real
launch route with a fake Claude executable that makes a missing flag observable;
constructor-only argument assertions do not prove the production route carries
the boundary. Related: [[route-tests-drive-real-launch-wiring]].
