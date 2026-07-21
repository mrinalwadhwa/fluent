# Inject a stage failure by obstructing its deterministic output path

## Context

Use when a binary test must prove that a specific stage of an idempotent,
journaled pipeline fails while earlier stages succeed and an outer operation
(like a land) stays intact — for example the post-land follow-up materialization
stages (Observation → Work → queue). Because each stage writes to a
deterministic path, you can fail exactly one stage by pre-creating an
obstruction at that path before running the flow, without any fault-injection
hooks in production code.

## Mechanism

Each stage fails when its output path is already occupied by content it refuses
to overwrite:

- **Observation stage** — pre-write a body-only file at the deterministic
  Observation id (`followup-<work>-<candidate>-<fu>.md`). `ensure_provenance_observation`
  refuses to overwrite a file lacking provenance frontmatter.
- **Work stage** — pre-write unreadable JSON (`{ not json`) at the derived Work
  Item path (`.fluent/work/items/<derived-id>.json`). `read_work_item` returns a
  parse error (not `NotFound`), so `create_work_item` never runs.
- **Queue stage** — `fs::create_dir_all` a directory at the ledger path
  (`.fluent/work/queue/<derived-id>.json`). `read_ledger`/`write_ledger` cannot
  read or write a path that is a directory.

To then test resume/idempotency, remove the obstruction and re-run the flow; the
journal resumes at the previously failed stage and produces each effect once.
Pair with a rebase-failing mock (a mock that `exit 1`s on any "Rebase the
candidate branch" prompt) to prove an already-merged re-land never rebases or
re-merges.

## Example

```rust
// Force the Work stage to fail on an execute-mode corrective land.
let items_dir = main_dir.join(".fluent/work/items");
fs::create_dir_all(&items_dir).unwrap();
fs::write(items_dir.join(format!("{DERIVED_FU1}.json")), "{ not json").unwrap();

land_work_1(&main_dir, &bin_dir, true);

assert!(is_merged(&main_dir), "a promotion failure does not undo the land");
assert_eq!(follow_up_failure_stage(&main_dir), "work");
assert!(main_dir.join(format!(".fluent/observations/{OBS_FU1}.md")).exists()); // earlier stage done
```
