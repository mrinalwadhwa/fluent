# OpenCode ↔ Fluent integration hardening

**Status**: Implemented, 2026-11-08. WP1–WP3 and
WP4–WP8 landed; WP7 uses the clean-tree alternative instead of a working-tree
snapshot. Derived from a full session
using Fluent 0.6.0 (modified) with OpenCode on a monorepo: eight Work Items
planned and executed, several of which failed in recoverable but costly ways.

## Why

The session built a product roadmap through Fluent. The workflow produced good,
reviewed code, but the OpenCode integration had three blockers and a set of
friction points that cost multiple Work Item restarts and repeated
`--no-sandbox` workarounds. This plan sequences the fixes for the next session.

## Evidence from the session

- macOS, Python 3.14, a `uv`-managed Python package inside a monorepo. The Tester
  command was `cd <pkg> && uv run pytest tests -v --junitxml=...`.
- The Tester/worker sandbox blocked `uv`: `uv run pytest` exited `2` in 27–56 ms
  inside Fluent while the same command passed outside. Every
  `tester check`, `attempt run`, and `merge-candidate land` needed
  `--no-sandbox`.
- An `uncertain` architecture review → `revise-approach` → the corrective writer
  committed → `attempt run`/`queue resume` rejected “approved Approach source
  completion evidence changed”. A second `revise-approach` also rejected, and
  `repair-writer-completion` said “Attempt has no completed Writer candidate.”
  Only a new Work Item recovered.
- Landing one Work Item while another writer was running moved `main` and the
  active writer reported “Writer changed source checkout … changed product
  paths {the other Work Item's files}”. The task was invalidated; a writer retry
  recovered it.
- `.fluent/` had no `tester.yaml`/`extract-tester-results`; `review codebase`
  failed with “Tester is not structurally ready.” The default `shell-harness`
  extractor reports one result per command, while the `Test:` references are
  per-test; a custom JUnit extractor was needed.
- The `.opencode/skills/*` installation was counted as “uncommitted changes
  outside .fluent/”.
- `.fluent/config.yaml` said `opencode`, but the review-only Attempt resolved to
  `claude`; build Attempts used `opencode`.
- `merge-candidate land` re-ran the Tester in the sandbox and failed with exit
  `2`; `--no-sandbox` was needed separately.
- An `attempt run` aborted mid-write; the Attempt stayed `executing`.
  Recovery needed `cancel` → `queue resume` → a background `attempt run`, and
  the queue resume reported “no scheduler is running.”

## Work packages

### WP1 — Sandbox package-manager access (blocker)

**Symptom**: `uv` (and likely Cargo/pip) fails inside the Tester/worker sandbox.

**Where**: `src/tester.rs` sandbox profile/render
(`prepare_sandbox_profile`, `render_sandbox_profile`,
`run_with_sandbox_profile`), `src/coder.rs`, `src/work_task_executor.rs`.

**Fix**: detect the project's package manager; grant the cache/toolchain paths
or set a writable `UV_CACHE_DIR`/`CARGO_HOME`/`PIP_CACHE_DIR` under a scratch
dir. If network is denied, fail with a specific message and the `--no-sandbox`
remedy, not a command exit 2. Let `merge-candidate land` inherit the Attempt's
sandbox choice, or expose `tester.sandbox` in the project config.

**Tests**: a temp `uv` project; a Tester command that shells out to `uv run`;
assert a clear sandbox error when the cache is denied.

**Acceptance**: `fluent tester check`, `attempt run`, and
`merge-candidate land` pass without `--no-sandbox` on a `uv` project, or fail
with the specific remediation message.

### WP2 — Re-anchor `revise-approach` after its corrective writer (blocker)

**Symptom**: the revision's source-completion guard rejects the corrective
writer's own commit, leaving the Attempt unrecoverable.

**Where**: `src/planning_approach_recovery.rs:318–333` (the source-completion
comparison), `:489`/`:832` (“no completed Writer candidate”),
`src/host_evidence.rs`, `src/writer_completion_recovery.rs`.

**Fix**: bind the revision to the source completion it was approved against,
then record the corrective writer's new completion as the revision's output;
allow a re-record at the new frontier. Do not require a completed writer that
the state no longer exposes.

**Tests**: a regression test for
`uncertain architecture review → revise-approach → corrective writer →
attempt run → reviews → land`; assert the resume is accepted and the candidate
lands.

### WP3 — Concurrent-landing fence (blocker)

**Symptom**: landing one Work Item while another writer runs moves `main` and
invalidates the active writer.

**Where**: `src/work_task_executor.rs:9391–9409`
(`ensure_source_product_unchanged`); `src/work_merge_executor.rs`,
`src/scheduler.rs`.

**Fix**: distinguish “the writer changed the source” from “another Work Item
landed.” Compare the worktree HEAD separately; serialize landings against active
Attempts (a landing fence/lock); the error should name the other landing, not
the writer.

**Tests**: two Work Items, one running while the other lands; assert the running
writer is not invalidated, or that the landing is refused until it finishes.

### WP4 — Tester bootstrap and per-test extractors (friction)

**Symptom**: no Tester on first init; the default extractor is command-level; the
named `Test:` references do not resolve.

**Where**: `src/setup.rs`, `src/tester.rs`, `src/verification_reference.rs`,
`src/candidate_gate.rs`.

**Fix**: first-time init generates a starter Tester (a detected command or a
no-op) and documents the `Test:`/JUnit contract; ship pytest/JUnit and
cargo-nextest extractors; at Tester check time, compare the declared references
with the extractor IDs and warn when they cannot match.

**Tests**: a new project init with a pytest suite; a `Test:` reference resolves;
a command-level extractor warns.

### WP5 — OpenCode-managed paths (friction)

**Symptom**: `.opencode/skills/*` is counted as a source change.

**Where**: `ensure_no_non_fluent_worktree_changes`
(`src/work_task_executor.rs:9412–9437`).

**Fix**: treat `.opencode` as Fluent-managed when it contains
`.fluent-managed.json`; or install into a gitignored path; or prompt to commit
at init.

**Tests**: an install with `.opencode` untracked does not trip the guard.

### WP6 — Coder resolution transparency (friction)

**Symptom**: the config says `opencode`, but review-only resolves to `claude`.

**Where**: `src/coder.rs`, `src/config.rs`.

**Fix**: print the resolution source (project/user/default); warn on mismatch;
use the same resolver for review-only.

**Tests**: a config with `opencode`; the resolved plan names the source; a
review-only Attempt uses the same coder.

### WP7 — Review-only working-tree snapshot (friction)

**Symptom**: `review codebase --from-working-tree` fails on a pre-existing
Fluent install and untracked docs.

**Where**: `src/review_only_worktree.rs`.

**Fix**: snapshot the working tree into the review worktree, or require a clean
tree with an explicit message; exclude Fluent-managed paths.

**Tests**: untracked docs plus `.opencode`; review-only starts; no source
mutation.

### WP8 — Interruption and landing UX (friction)

**Symptom**: an aborted Attempt stays `executing`; recovery needs
cancel/resume; `queue resume` needs a scheduler; landing re-runs the sandboxed
Tester.

**Where**: `src/work_attempt_loop.rs`, `src/cli.rs`, `src/tester.rs`.

**Fix**: a `--detach`/background mode; a clearer interruption message; a local
`queue resume` without the scheduler; landing inherits the Tester sandbox
config.

**Tests**: interrupt an Attempt; resume; land; assert the messages and the
sandbox inheritance.

## Sequence

1. WP1 and WP8's landing inheritance — unblocks every run.
2. WP2 — unblocks the uncertain-review path.
3. WP3 — unblocks parallel Work Items.
4. WP4 — unblocks new projects.
5. WP5, WP6, WP7.
6. WP8 remainder.

## Acceptance for the plan

- A new `uv` project can run `fluent tester check`, `attempt run`, and
  `merge-candidate land` without `--no-sandbox` (or with a specific remediation
  if network is denied).
- An `uncertain` architecture review can be resolved by `revise-approach`, the
  corrective writer can complete, and the Attempt can land without a new Work
  Item.
- Two Work Items can run in parallel; landing one does not invalidate the
  other's writer.
- A new project gets a starter Tester and a per-test extractor.
- `.opencode` installs are not treated as source changes.
- The resolved coder is visible and consistent.

## Non-goals

- Redesigning the Work model.
- Changing the Local Preview's human landing gate.
- Remote/Fargate execution changes.

## Open questions

- Is the sandbox denial intentional (network/cache), and should the fix be a
  configurable allowlist rather than an automatic cache grant?
- Is the source-completion guard protecting against a real corruption case, and
  what is the minimal re-anchor?
- Should parallel landings be serialized by the scheduler or by a repository
  lock?

## Reproduction notes

- Fluent 0.6.0 (modified), macOS, `uv`, Python 3.14.
- The project was a monorepo with a C99 profiler and a Python package; the
  Tester command was `cd <pkg> && uv run pytest tests -v --junitxml=...`.
- The exact failures: sandbox exit 2; the revise-approach error; the
  source-checkout guard; the Tester bootstrap; the extractor mismatch; the
  `.opencode` guard; the coder mismatch; the interruption; the landing sandbox.

## Sources

- Session evidence from the eight Work Items run with this integration.
- Fluent code paths named above.
