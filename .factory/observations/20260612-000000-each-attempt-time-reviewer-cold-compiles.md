2026-06-12 — Each Attempt-time reviewer cold-compiles project build
artifacts in its own sandbox, costing 2–10 min per reviewer for Rust
projects (10m for the behaviors reviewer on `optional-attempt-merge-candidate-ids`
because it built `target/release` from scratch). The writer task
already produces a usable build cache in the candidate workspace
(599 MB of `target/debug` in our test), but two design choices prevent
reviewers from benefiting from it:

1. The reviewer prompt's `reviewer_writable_outputs_guidance` text
   tells reviewers to redirect Cargo commands away from the
   candidate's `target/` to a per-reviewer artifact directory. The
   prompt conflates write isolation (correct: reviewers must not
   modify the candidate) with read isolation (overreach: existing
   build outputs are safe to consume).
2. Even if reviewers DID write to the candidate's `target/`, parallel
   Cargo invocations would serialize on `target/debug/.cargo-lock`,
   defeating the reviewer parallelism we get from per-reviewer
   sandboxes. So sharing target/ as a writable resource is not
   tenable; each reviewer needs its own writable build cache.

The fix has two layers:

**Prompt guidance refinement** — tell reviewers they may READ the
candidate's existing build outputs freely (binaries, compiled
artifacts, installed dependencies), but must NOT write to the
candidate. New builds redirect to the reviewer's artifact directory.
For the immediate Rust case: behaviors and similar reviewers should
invoke `<candidate>/target/debug/<binary>` directly instead of
running `cargo build` against an empty `CARGO_TARGET_DIR`.

**Built-in auto-prep for popular toolchains** — Factory detects the
project type from marker files in the candidate workspace and copies
the canonical build directories into each reviewer's artifact area
before launching the reviewer. Reviewers point their toolchain at the
copy and warm-start incremental builds. Initial registry:

| Toolchain | Marker          | Dirs copied                                |
| --------- | --------------- | ------------------------------------------ |
| Rust      | `Cargo.toml`    | `target`                                   |
| Node      | `package.json`  | `node_modules`, `dist`, `.next`, `build`   |
| Maven     | `pom.xml`       | `target`                                   |
| Gradle    | `build.gradle`  | `build`, `.gradle`                         |

Go is intentionally omitted (build cache is content-addressed and
location-independent). Python is intentionally omitted (venvs hardcode
absolute paths and don't survive a copy reliably).

Copy implementation should try reflink (`cp -c` on macOS, `cp
--reflink` on Linux filesystems that support it) first, then hardlink
(`cp -l`), then a deep copy as last resort. With reflinks the copy is
effectively zero-cost and zero-extra-disk until something gets
modified.

A `.factory/hooks/prepare-pre-review` hook overrides the
auto-detection when present. The hook gets `FACTORY_REVIEWER_ARTIFACT_DIR`
in its env and CWD = candidate workspace; the project does whatever
it needs. No marker matched and no hook present → no prep, same as
today.

Implementation notes for the Work Item:

- Cargo fingerprints reference source paths, not target paths, so a
  copied `target/` warm-starts incremental builds correctly when the
  reviewer points `CARGO_TARGET_DIR` at the copy.
- The toolchain registry is a `&'static [Toolchain]` constant — adding
  a new entry is a one-row change with no new abstractions.
- The reviewer prompt's `reviewer_writable_outputs_guidance` becomes
  "the writer's outputs are pre-populated at
  `$FACTORY_REVIEWER_ARTIFACT_DIR/<dirname>/`; use them for
  incremental builds."

Estimated wall-clock impact on the
`optional-attempt-merge-candidate-ids` Work Item if this had been
live: behaviors reviewer ~10 min → ~2–3 min; architecture/tests
~2–3 min → ~30s; total attempt time ~17m 32s → ~9–10m.

This becomes its own Work Item after `optional-attempt-merge-candidate-ids` lands.
