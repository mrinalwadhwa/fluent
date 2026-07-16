# Codebase Overview

Fluent is an autonomous software factory that orchestrates coding agents (Claude Code, Codex, Pi) to build software through a structured lifecycle: brief, behaviors, approach, plan, write, test, review, and merge. It runs agents inside macOS Seatbelt sandboxes for file-access isolation, manages work in git worktrees so the main branch stays clean, and drives an attempt loop where writers produce code, testers run declared test commands, and parallel reviewers (architecture, behaviors, documentation, skills, tests) evaluate the result. When all reviewers pass, a Merge Candidate is produced and can be landed onto main.

## Entry points

- `src/main.rs` — CLI entry point; parses args via clap, dispatches to `cmd_*` handler functions
- `src/cli.rs` — clap-derive CLI definition; all subcommands and flags
- `src/lib.rs` — public module declarations; every `src/*.rs` module is re-exported here

## Major components

| Area | Files | Purpose |
|------|-------|---------|
| Work model | `src/work_model.rs` | Core data structures (WorkItem, Attempt, Task, MergeCandidate) and JSON-file storage |
| Attempt loop | `src/work_attempt_loop.rs` | Drive an Attempt through write → test → review rounds |
| Task execution | `src/work_task_executor.rs` | Run a single Task (write, review, test, seed) by spawning a coder agent |
| Merge | `src/work_merge_executor.rs` | Rebase, squash, and fast-forward merge a Merge Candidate onto main |
| Sandbox | `src/os.rs`, `sandboxes/` | Render and apply macOS Seatbelt profiles for agent sandboxing |
| Coder abstraction | `src/coder.rs` | Launch Claude Code, Codex, or Pi with appropriate flags and env |
| Git operations | `src/git.rs` | Thin wrappers around `git` CLI commands |
| Worktrees | `src/worktree.rs` | Create and manage git worktrees for isolated work |
| Review | `src/review.rs` | Reviewer list, verdict parsing, outcome aggregation |
| Content resolution | `src/content.rs` | Resolve prompts and sandbox profiles from project → user config → bundled defaults |
| Skills | `skills/`, `build.rs` | Agent skills bundled into the binary at build time; materialized to disk at runtime |
| Prompts | `prompts/` | System and user prompts for write, review, seed, and rebase tasks |
| Tester | `src/tester.rs` | Run `.fluent/tester.yaml` commands and parse results |
| Queue / Scheduler | `src/queue.rs`, `src/scheduler.rs` | Priority queue and polling scheduler for sequential Work Item execution |
| Dashboard | `src/dashboard.rs` | Live TUI (ratatui) showing Work Item activity |
| Fargate | `src/fargate.rs`, `src/fargate_bootstrap.rs`, `infrastructure/` | Run attempts and merges on AWS Fargate |
| Observations | `src/observations.rs` | Per-file observation entries under `.fluent/observations/` |
| Cleanup | `src/cleanup.rs` | Remove stale worktrees, branches, and Work Item state |

## Key conventions

- **Rust 2024 edition** with `anyhow` for error propagation and `clap` derive for CLI parsing.
- **JSON-file persistence** — Work Items are stored as JSON under `.fluent/work/items/<id>.json`. No database.
- **Atomic writes** — `src/atomic_write.rs` writes to a temp file then renames, preventing partial reads.
- **File-based leasing** — `src/lease.rs` provides advisory locks for concurrent access.
- **Linear git history** — rebase only, no merge commits. `git merge --ff-only`.
- **Imperative commit messages** starting with a verb, no Co-Authored-By trailers.
- **Content bundling** — `build.rs` embeds skill files and sandbox profiles into the binary at compile time. Runtime resolution falls back: project `.fluent/` → `~/.config/fluent/` → bundled.
- **Naming** — snake_case throughout Rust code. CLI subcommands use kebab-case nouns (`work-item`, `merge-candidate`). Module names match their primary concept.

## Build and test

```sh
cargo build --release
install -m 0755 target/release/fluent /Users/mrinal/.local/bin/fluent

cargo test                      # all unit and integration tests
cargo test --test behaviors     # behavior tests only (tests/behaviors/)
```

Behavior tests (`tests/behaviors/`) are shell scripts that test the binary from the outside against EARS-style behavior statements. Per-case output goes to `tests/output/` (gitignored).

Integration tests in `tests/` use `assert_cmd` and `predicates` to test CLI behavior. Tests that touch shared git state use `serial_test` for isolation.

## Important dependencies

| Crate | Purpose |
|-------|---------|
| `clap` (derive) | CLI argument parsing |
| `anyhow` | Error handling with context |
| `serde` / `serde_json` / `serde_yaml` | Serialization for Work model (JSON) and tester config (YAML) |
| `ratatui` / `crossterm` | Terminal UI for the dashboard |
| `chrono` | Timestamps in Work model records |
| `sha2` | Content hashing for deduplication |
| `tempfile` | Temporary files for atomic writes and sandbox profiles |
| `rustix` | Low-level filesystem operations |
| `assert_cmd` / `predicates` | Integration test assertions (dev) |
| `serial_test` | Serialize tests that share mutable state (dev) |
