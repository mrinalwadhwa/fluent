# Observe a sandboxed handoff-only coder through its durable transcript

## Context

Use when a binary test drives an effectively sandboxed post-land Learner (a
handoff-only retry) and needs to observe what the coder mock did — how many
times it ran, the prompt it received, the commit it saw, or that it reached a
serialized window. The handoff-only Seatbelt profile denies every shared macOS
temp tree (`/private/tmp` and `/private/var/folders`) and the live project
roots, so a mock cannot record observability in a `tmp.path()` counter file the
way an unsandboxed mock can. The only writable surfaces are the disposable
isolated clone and a private scratch, neither of which the test can read after
the run.

## Mechanism

The host captures the coder's stdout line-by-line into a durable transcript on
the managed Learner surface (`.../learner/transcript.jsonl`), written outside
the sandbox. `run_learner` preserves any prior run's transcript as an immutable
`transcript.run<N>.jsonl` sibling before the next run truncates the live path,
so every run on the Attempt leaves its own record.

So: have the mock **print** its observability to stdout instead of writing a
shared-temp file, and read it back by concatenating every `transcript*.jsonl`
in the learner dir.

- **Run count** — the mock echoes a marker (`LEARNER_RUN`); count the marker
  across all preserved and live transcripts.
- **Prompt / observed commit** — the mock echoes the prompt and
  `git rev-parse HEAD`; grep the concatenated transcripts for the substring.
- **Serialized window / release** — the mock *reads* a shared-temp release file
  (reads under `/private/var/folders` are allowed) but *announces* its window on
  stdout; the test polls the transcript for the marker, then writes the release.

Reads of shared temp still work inside the sandbox, so a one-shot guard file
(`touch` pre-land while unsandboxed, `[ -f ]` post-land) is fine — only writes
from the sandboxed run must move to stdout. Emit markers from the mock with
`echo`/`printf` (not a here-document: bash's heredoc temp file needs a writable
`$TMPDIR`, which the private scratch supplies, but `printf '%s\n' '<json>'` for
the draft avoids the dependency entirely).

## Example

```rust
// In the mock (bash): record observability on stdout, not shared temp.
//   echo "LEARNER_RUN"
//   printf 'LEARNER_HEAD %s\n' "$(git rev-parse HEAD)"
//   printf '%s\n' '{"learning_summary":"...","follow_ups":[]}' > "$DRAFT"

// In the test: read it back from the managed transcript surface.
fn learner_transcripts(main_dir: &Path) -> String { /* cat transcript*.jsonl */ }
fn learner_run_count(main_dir: &Path) -> usize {
    learner_transcripts(main_dir).lines().filter(|l| *l == "LEARNER_RUN").count()
}

assert_eq!(learner_run_count(&main_dir), 2, "pre-land failure + post-land retry");
assert!(learner_transcripts(&main_dir).contains(&format!("LEARNER_HEAD {merged}")));
```
