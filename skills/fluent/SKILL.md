---
name: fluent
description: Operate the fluent workflow to build software autonomously over extended periods. Interactive stages (brief, behaviors, approach, plan) run with the user. Autonomous execution loops writer → tester → parallel reviewers → Learner. A Merge Candidate becomes ready only after the Learner succeeds; retryable Learner failures resume with `fluent attempt run`, while non-relaunchable evidence failures stay blocked for human recovery. When a decision needs a human, Fluent sets `needs-user` and pauses, then resumes once the user resolves it.
fluent-shim: true
---

# Fluent (bootstrap shim)

This is a bootstrap shim. It installs the `fluent` binary if missing, then
materializes the full fluent skill from the binary so the skill always matches
the installed version.

## Step 1 — Install the binary if missing

```sh
fluent --version
```

If `fluent` is not found, install it:

```sh
curl -fsSL fluent.computer/install | sh
```

The installer puts `fluent` in `~/.local/bin`. If `fluent --version` still
fails after installation, use the full path `~/.local/bin/fluent` for all
subsequent commands, and tell the user to add `~/.local/bin` to their `PATH`.

## Step 2 — Materialize the full skill

Run the following to install the full fluent skill from the binary:

```sh
fluent skills add
```

## Step 3 — Continue with the full skill

Read the full fluent skill from the data directory the binary wrote to:

```
~/.local/share/fluent/skills/fluent/SKILL.md
```

Read that file now with the Read tool (expand `~` to the user's home directory).
Follow its instructions from the beginning as if this shim had not been loaded.
The full skill replaces this shim — do not return to these instructions.
