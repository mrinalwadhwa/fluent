# Codebase Overview

Fluent is an autonomous software factory that orchestrates coding agents (Claude Code, Codex, Pi) through a structured lifecycle: brief, behaviors, approach, plan, write with focused verification, parallel review, one final Tester, Learner, and merge. It runs agents inside macOS Seatbelt sandboxes for file-access isolation and manages work in git worktrees so the main branch stays clean. For code-producing Work, reviewers inspect the Writer candidate before the final deterministic Tester runs the complete declared suite; review-only Work retains its Tester-first contract. A Merge Candidate becomes ready only after the applicable reviews, final Tester, and Learner succeed.

## Entry points

- `src/main.rs` — CLI entry point; parses args via clap, dispatches to `cmd_*` handler functions
- `src/cli.rs` — clap-derive CLI definition; all subcommands and flags
- `src/lib.rs` — public module declarations; every `src/*.rs` module is re-exported here

## Major components

| Area | Files | Purpose |
|------|-------|---------|
| Work model | `src/work_model.rs` | Core data structures (WorkItem, Attempt, Task, MergeCandidate) and JSON-file storage |
| Attempt loop | `src/work_attempt_loop.rs` | Drive code-producing Attempts through write → review → final Tester → Learner, including corrective rounds |
| Task execution | `src/work_task_executor.rs` | Execute Writer, Reviewer, Tester, and Learner Tasks; Tester is a deterministic subprocess and does not spawn a coder |
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

cargo test                      # Rust unit and integration tests under libtest
fluent tester check             # complete project-owned suite from .fluent/tester.yaml
```

The canonical Tester configuration uses `cargo nextest` for Rust tests and then runs every operation and skill script under `tests/behaviors/`. Writers should use narrow harness-native selectors for feedback and leave the complete configured suite to Fluent's final Tester unless they are explicitly validating the Tester boundary. Per-case output goes to `tests/output/` (gitignored).

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
