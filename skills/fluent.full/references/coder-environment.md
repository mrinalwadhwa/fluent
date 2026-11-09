# Configure autonomous coder environments

Read this guide when an autonomous role needs additional skills or when you
need to inspect the managed home used by a Claude Writer or an OpenCode role.

## Configure Codex skill roots

Configure explicit skill roots for autonomous Codex roles. Put absolute external
roots in user configuration (for example, `~/.config/fluent/config.yaml`).
Project configuration may use only relative roots that stay inside the project;
it must not name external roots. The external-root form is:

```yaml
codex:
  skill-roots:
    - /absolute/path/to/skills
```

Project-relative roots resolve from the project root before Fluent checks their
containment (for example, `project-skills` means `<project>/project-skills`, not
a path relative to the caller's current directory). A project root cannot use
`..` to escape the project.

Fluent validates and snapshots each root into the private worker home before
reserving the role. Symlinks and duplicate top-level names fail closed, and the
worker cannot access the rest of the operator's home.

## Inspect the Claude Writer home

Autonomous Claude Writers use an Attempt-scoped managed home. Their sandbox
blocks the operator's persistent Claude project and memory tree; do not copy
state from `~/.claude/projects` into a Work Item manually.

## Inspect the OpenCode role home

Autonomous OpenCode roles require OpenCode 2 or newer and run with a private
`HOME` under the Attempt's artifacts for a Writer, or under the role's artifact
or handoff directory for a reviewer, Learner, or rebase. Fluent copies the
operator's `~/.local/share/opencode/auth.json` and
`~/.config/opencode/opencode.json` or `.jsonc` into that home. Plugins, custom
agents, commands, and global instructions are not copied.

Do not point an autonomous role at the operator's live OpenCode directories.
The private home also isolates each role's session database. If parallel
reviewers contend on one database, isolate the role's data directory, or use
OpenCode's `OPENCODE_DB` override where the installed release supports it,
rather than sharing the operator's state.
