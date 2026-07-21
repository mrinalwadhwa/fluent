# Give scheduler/attempt tests per-run-unique Work Item ids

## Title

Derive Work Item ids from the unique temp directory so attempt worktrees
never collide across test runs.

## Context

Any binary test that drives a real Attempt — `fluent attempt run`, or a
`fluent scheduler run` that launches Attempts — creates candidate and
reviewer worktrees as **siblings of the project directory**:
`initial_candidate_workspace_path` returns `../work-<len>-<id>-<attempt>`.
Under a `TempDir` project at `$TMPDIR/.tmpXXXX/`, that resolves to
`$TMPDIR/work-<len>-<id>-<attempt>` — a path in the **shared** temp root,
not inside the per-test `TempDir`.

Two consequences bite tests that use fixed ids (`wi-a`, `wi-conc-0`):

- Reruns and parallel `nextest` cases reuse the same worktree path and
  fail with `Workspace ... belongs to a different git repository`.
- A scheduler killed with SIGKILL (to end a test quickly) orphans its
  worktrees in `$TMPDIR`, poisoning the next run.

The failure is intermittent and easy to misread as a scheduler bug or a
timing/concurrency flake, because a clean `$TMPDIR` makes it pass once.

## Mechanism

- Build ids from the `TempDir`'s unique file name so each run's worktree
  paths are unique: filter the name to ASCII alphanumerics and prefix it
  (`format!("wc{token}{n}")`). Ids only forbid `/`, `\`, empty, `.`, `..`.
- Remove this run's sibling worktrees at the end (best effort): scan the
  project's parent for `work-*` entries containing the token and
  `remove_dir_all` them, so a hard-killed scheduler leaves no orphans.
- To observe a stable concurrent set (e.g. "capacity is four"), gate the
  mock writer on a release file it polls for, instead of a fixed `sleep`.
  The claimed Attempts hold their slots until released, so the count
  reaches capacity deterministically even under heavy parallel load;
  release and kill the scheduler once the count is observed.

## Example

```rust
fn run_token(tmp: &TempDir) -> String {
    tmp.path().file_name().and_then(|n| n.to_str()).unwrap_or("run")
        .chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>()
        .to_lowercase()
}

fn remove_sibling_worktrees(project: &Path, token: &str) {
    if let Some(parent) = project.parent() {
        for entry in fs::read_dir(parent).into_iter().flatten().flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("work-") && name.contains(token) {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
}

let token = run_token(&tmp);
let id = format!("wc{token}0"); // unique per run → unique worktree path
// ... run scheduler, observe, then:
remove_sibling_worktrees(project, &token);
```
