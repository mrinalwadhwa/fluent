---
name: fluent
description: Use Fluent to review, build, fix, or improve software. Invoke when a user asks to install or initialize Fluent; capture an Observation; define a slice; create or refine a Brief, Behavior Specification, Technical Approach, Implementation Plan, or Work Item; run, queue, inspect, resume, or recover an Attempt; review a codebase through Fluent; manage or land a Merge Candidate; capture project Expertise; or configure Fluent's agents, scheduler, sandboxes, or remote execution.
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
