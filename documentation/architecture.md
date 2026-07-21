# Architecture

Workflow and execution system for autonomous coding agents. Manages work
from intent capture through execution and review across multiple sessions.

## System overview

```
┌─────────────────────────────────────────────────┐
│  Skills                                         │
│  fluent (interactive lifecycle),                │
│  review-documentation, review-behaviors,        │
│  review-architecture, review-skills,            │
│  review-tests                                   │
│  Portable procedures any agent follows          │
├─────────────────────────────────────────────────┤
│  fluent skill                                  │
│  Teaches agents the full workflow               │
├─────────────────────────────────────────────────┤
│  Fluent command                                │
│  fluent <noun> <verb> / status / dashboard      │
│  fluent update / fargate teardown / init        │
│  fluent keep-awake on / off / status           │
│  Deterministic, operational                     │
└─────────────────────────────────────────────────┘
```

Skills describe procedures. They don't know about sandboxes, sessions,
or runtimes. The fluent command handles the operational envelope:
sandbox setup, credential injection, session continuity, worktree
creation, and remote execution. The fluent skill bridges the two — an
agent reads it and can drive the entire workflow.

## Workflow

```
Brief → Behaviors → Approach → Plan → Execute → Review → Land
(interactive)                         (autonomous)
```

Interactive stages happen in the agent's session with the user present.
The agent follows skills directly.

Autonomous stages don't need the user. The Work model is the delegated
execution path.

## Core work model

Fluent's execution lifecycle uses these durable nouns: Work Item,
Attempt, Task, Workspace, and Merge Candidate. This model is documented
and represented in Rust so scheduling, status, review, and merge paths
use the same vocabulary.

`fluent task run <work-item-id> <attempt-id> <task-id>` executes a
stored write or review Task through the selected coder, `fluent
attempt run <work-item-id> <attempt-id>` advances an Attempt through safe
write and review transitions, and `fluent merge-candidate land <work-item-id>
<merge-candidate-id>` executes a stored Merge Candidate.

`fluent work-item create <id> --title <title>` exposes the first Work Item
intake surface. It writes Work Item metadata under
`.fluent/work/items/` and leaves Attempt, Task, and Merge Candidate
collections empty. It does not schedule work.
Callers may attach approved planning context directly to the Work Item with
`--planning-context <text>`, `--planning-context-file <path>`, or
separate `--brief-file`, `--behaviors-file`, `--approach-file`, and
`--plan-file` inputs. Fluent stores that context as optional
`WorkItem.planning_context` so `fluent work-item show <id>` exposes the
brief, behaviors, approach, and plan that write Tasks use. Planning
skills treat this Work Item planning context as the normal handoff to
delegated Work execution. Callers may also pass explicit prompt text with `--instructions <text>` or
`--instructions-file <path>`; Fluent stores that text as optional
`WorkItem.instructions` and gives it precedence over derived planning
context when it creates write Task instructions. `fluent attempt create
<work-item-id> <attempt-id>` creates the first operational transition
from intake: it appends a planned Attempt with one initial `write` Task.
The Task declares role `author`, copies explicit Work Item instructions
or derives instructions from Work Item planning context into optional
`Task.instructions`, and declares one writable workspace reference at
`../work-<work-item-id-byte-len>-<work-item-id>-<attempt-id>`.
`fluent task run` creates or reuses that writable workspace as a
sibling git worktree beside the source checkout, runs the coder there,
and completes the Task only after the workspace is clean and contains a
new commit produced after Fluent bound the workspace for that Task run.
The bridge stores workspace paths relative to the source root for
portability, resolves them through the source checkout parent at
execution time, and rejects writable Task workspace paths outside the
expected managed sibling workspace before it creates or binds a
worktree.
`fluent review <work-item-id> <attempt-id>` appends planned
`review` Tasks for the default reviewer set after a completed write Task
exists. Each review Task reads the candidate workspace, carries review
context copied from the write output, and writes only under
`.fluent/work/artifacts/<work-item-id>/<attempt-id>/<task-id>/`. The
review context names the candidate workspace, source branch, and
candidate commit and includes a shell-quoted `git -C <workspace> diff
<range>` command so a reviewer can inspect the scoped diff without
rediscovering the author Task. Running a review Task requires
`review.md` in that artifact area; the Task can complete even when that
artifact says `Verdict: fail` or `Verdict: uncertain` because verdict
acceptance belongs to later review or merge policy.
`fluent review codebase <work-item-id> <attempt-id>` appends a
review-only Attempt for full-codebase review of the current source
checkout. Review-only Attempts contain review Tasks only, read the source
checkout through workspace id `source` at path `.`, and write artifacts
under `.fluent/work/artifacts/<work-item-id>/<attempt-id>/<task-id>/`.
The review Task executor
treats the source checkout as a guarded readable workspace: the reviewer
sandbox gets the source checkout as a read-only root and the managed
artifact area as its writable root. For no-sandbox or failed-reviewer
paths, the guard verifies that source HEAD and source files stayed
unchanged and that only the Task artifact area changed under `.fluent/`.
If a reviewer changes source HEAD, the guard resets HEAD before failing
the Task. If a reviewer changes source files or protected `.fluent/`
state outside the Task artifact area, the guard restores protected
checkout state before failing the Task. This restorative guard applies
only to interactive `ReviewOnly` Attempts (e.g. `fluent review
codebase`).

Post-merge review Attempts use `AttemptKind::PostMergeReview` and a
non-restoring `PostMergeSourceGuard`. This guard verifies that the
source HEAD still matches the merged commit on completion but does not
snapshot or restore worktree changes or `.fluent/` file contents.
This allows Fluent and the user to write new state concurrently while
a background post-merge review is in flight. If the source HEAD moves
during the review (e.g. another merge lands), the guard fails the
review Tasks with a clear error and does not attempt restoration.

When a review round includes a candidate workspace, Fluent creates a
`TaskKind::Tester` Task alongside the reviewer Tasks. The Tester is a
deterministic subcommand (`fluent tester run`) that reads
`.fluent/tester.yaml` from the candidate workspace, runs each declared
test command sequentially, invokes `.fluent/extract-tester-results` to
normalize the raw output into per-test entries, and writes
`tester-results.json` to its artifact directory. Every reviewer Task
declares a `depends_on` reference to the Tester Task; the Attempt
scheduler blocks all reviewers until the Tester completes. The Tester
does not spawn a Coder process or write a transcript — it is a
deterministic subprocess, not an LLM agent.

The `behaviors.md` format supports two markers on EARS statements:
- `Test:` — names a test that verifies the behavior.
- `Untestable:` — marks a behavior as genuinely untestable with a reason.

Before launching each review Task, Fluent pre-populates the reviewer's
artifact directory with the candidate's build outputs so reviewers start
with a warm build cache. Fluent detects the project toolchain from
marker files (`Cargo.toml`, `package.json`, `pom.xml`, `build.gradle`)
and copies the canonical build directories using reflink, hardlink, or
deep copy in that order. A `.fluent/hooks/prepare-pre-review` hook
overrides the built-in detection when present. Review-only and
post-merge review Attempts skip this step because they review the source
checkout, not a candidate with writer-produced build outputs.

`fluent attempt run <work-item-id> <attempt-id>` is the first
Attempt-level orchestration path. It advances one Attempt by running the
next planned write Task serially through the Task executor, or by running
planned review Tasks in parallel with concurrency limited to
`FLUENT_MAX_PARALLEL_REVIEWERS` (default 5, minimum 1). Review-only
Attempts run review Tasks serially because their reviewers share a source
checkout. When a write or review Task coder errors, the Task executor
retries up to `FLUENT_MAX_TASK_RETRIES` (default 2) times before marking
the Task failed and pausing the Attempt at `needs-user`, except for auth
rejections (401), which skip retries and escalate immediately. Tester
Task errors follow the same retry-then-pause policy but do not check for
auth rejections because the tester invokes test harnesses, not a coder.
Each failed Task writes a per-task handoff file
(`needs-user-{task_id}.md`) so concurrent review failures preserve
independent context. The loop reloads stored state before deciding the
next transition. After the
initial write output completes it plans review Tasks for the full Work
reviewer set. After a follow-up write output completes it derives the
next review roles from that Task's failed review input artifacts; when
it cannot derive at least one role, it falls back to the full Work
reviewer set. After a completed review round it interprets review
artifacts with the review subsystem verdict parser and checks the
round's `tester-results.json` for test failures — only failures the
Work Item introduced (tests failing now that were not failing in the
pre-write baseline) block the Merge Candidate path; pre-existing
failures pass through. When no baseline is available the gate falls
back to blocking on any failure (fail-open: a missing or unparseable
results file does not block). All pass with no introduced tester
failures marks the Attempt review state as passed, completes
the Attempt, and creates or returns one durable Merge Candidate for
later merge execution. The Merge
Candidate records the source candidate workspace, target workspace,
source branch, target branch, candidate commit, and its own pending
review state. When the same-invocation auto-continue budget permits
another follow-up write Task, any fail records a `PlannedFollowup`
outcome, creates a planned follow-up write Task with the failed review
artifacts as Task inputs, and copies explicit Work Item instructions
into that follow-up Task, or derives those instructions from stored Work
Item planning context when explicit instructions are absent. One command
invocation may advance at most two follow-up write Tasks, including
already-planned follow-up write Tasks that existed before the command
started and follow-up Tasks planned by the command. The same invocation
then continues through each budgeted follow-up write Task, targeted
review planning, and targeted review execution until the Attempt reaches
an existing terminal boundary such as merge-candidate-ready, needs-user,
review-only complete, review-only failed, failed task, or executing-task
state. If another failed review round would require a third
same-invocation follow-up write Task, the Attempt loop marks the Attempt
`needs-user`, writes a handoff that names the failed review artifacts
and says the auto-continue budget was exhausted, and stops without
creating the over-budget Task. When no review artifact fails, uncertain
or missing verdicts mark the Attempt `needs-user` with a handoff under
`.fluent/work/artifacts/<work-item-id>/<attempt-id>/`.
For review-only Attempts, all pass marks the Attempt complete with review
state `passed` and does not create a Merge Candidate. Any fail marks the
Attempt failed with review state `failed` and does not create a follow-up
write Task. Uncertain verdicts without failures mark the Attempt
`needs-user` and write the same Work handoff artifact.
`fluent work-item list` and `fluent work-item show <id>` expose the same durable
Work Item model for inspection. These commands use `.fluent/work/items/`
through the Rust storage model and validate stored objects.
`fluent status` and `fluent dashboard` use Work Items as the default
operator surface. They read Work Items through `work_status.rs`, which
reduces stored Work Items to operator-facing rows. That boundary chooses
the latest Attempt, the active or waiting Task, the matching Merge
Candidate, and a short action label. It returns valid rows and per-file
read errors together so one bad Work Item file does not hide the rest of
the queue.
Write Task prompt generation reads `Task.instructions` from durable Work
state and includes non-empty instructions in the coder prompt. A Task
receives those instructions from explicit Work Item instructions first,
or from rendered Work Item planning context when explicit instructions
are absent. Extra arguments passed after `--` remain coder flags only;
Fluent does not append them as additional prompt text.
`fluent merge-candidate show <work-item-id> <merge-candidate-id>` prints
one stored Merge Candidate as pretty JSON. This command only reads the
boundary object. `fluent merge-candidate land <work-item-id> <merge-candidate-id>`
executes a Merge Candidate that still needs to land: it invokes an agent
to rebase the candidate workspace against the target branch, regenerates
post-rebase provenance, runs configured pre-merge checks in the candidate
workspace, runs the required reviewer set with merge-time context, then
fast-forwards the target branch to the updated candidate head.

The rebase step is recorded as a `TaskKind::Rebase` Task on the Attempt,
with its own artifact directory and prompt log. The agent runs
`git rebase <target>` inside the candidate workspace, resolving trivial
conflicts inline (additive doc edits, observation files, append-only
state). If the agent cannot resolve a conflict, it writes a diagnostic
to `give-up.md` in its artifact directory and exits non-zero; the Merge
Candidate transitions to `needs-user` with the diagnostic attached. When
the rebase succeeds, Fluent regenerates provenance: it updates every
completed Write Task's `output.base_commit` to the rebased target commit and
`output.commit` to the candidate tip, the Attempt's
`artifacts[*].path` entries, and the Merge Candidate's
`candidate_commit` to the new candidate-tip SHA. Per-task SHA fidelity
is intentionally lossy; per-task contribution remains visible through the
Attempt's Task list, the per-Task prompt logs, and the per-Task artifact
directories. The agent may squash, reorder, reword, or drop redundant
commits during rebase as long as all pre-rebase content changes are
preserved. No project hooks run during the rebase step; `fix-pre-merge`
continues to own post-rebase cleanup. Re-running `fluent merge-candidate land`
after a failed rebase creates a new rebase Task (with an incremented
suffix) and operates on the candidate workspace in its current state.

Merge-time review prepares one detached reviewer
worktree per role at the post-rebase candidate commit and runs those
roles in parallel. Each reviewer worktree lives at a sibling path
`../review-<work-item-id-bytelen>-<work-item-id>-<attempt-id>-<reviewer>`
relative to the project root, not nested under `.fluent/work/artifacts/`.
Each reviewer sees its dedicated reviewer worktree as
the candidate workspace and receives only its own writable artifact
directory. Merge-time reviewers receive the exact
`.fluent/work/artifacts/<work-item-id>/<attempt-id>/<candidate-id>/merge/reviews/<role>/review.md`
artifact path for their output and the absolute filesystem path the
reviewer must write. They also receive a shell-quoted `git -C
<workspace> diff <target-commit>..<candidate-commit>` command and a
merge-check status note; Fluent does not ask them to inspect
merge-check artifact paths from the reviewer sandbox. When Fluent
builds the Work merge reviewer system prompt, it uses the prompt's
`[work-system]` section when one exists and falls back to the raw
`[system]` section otherwise.
Fluent then points the reviewer at the absolute candidate
workspace skill path when that skill exists; if the candidate does not
contain that skill file, the prompt tells the reviewer to apply the
reviewer role directly. If the candidate workspace contains
`.fluent/expertise/decisions.md`, the prompt names that absolute path so
reviewers do not resolve decisions relative to their artifact directory.
Reviewers treat the candidate workspace as read-only and write only merge
review artifacts; scratch tests, suggested patches, and proposed
documentation edits belong in those artifacts, not in the candidate
workspace. Fluent sets `CARGO_TARGET_DIR` in each reviewer's environment
to a path under that reviewer's artifact directory, so build outputs go to
`.fluent/work/artifacts/<work-item-id>/<attempt-id>/<candidate-id>/merge/reviews/<role>/target/`
without reviewer cooperation. The reviewer sandbox grants read access to
the whole `.fluent/work/artifacts/<work-item-id>/<attempt-id>/` subtree
so merge-check and prior-review artifacts are readable. After reviewers exit, merge execution checks each reviewer
worktree for staged, unstaged, untracked, and ignored file changes,
including changes under `.fluent`, and fails before merging if any
reviewer dirtied its isolated candidate. It writes one combined review
state after all reviewer jobs finish and cleans up reviewer worktrees
after successful merge or failed review handling. After it records the
merged state, it removes the managed candidate worktree — unless the
Attempt's Learner run failed and is still retryable, in which case it
retains the workspace so a post-land handoff-only Learner retry has a
chance to run against. If cleanup
fails after the target branch has merged, merge execution prints a
warning and leaves the merged Merge Candidate state intact. Running the
command again
for a Merge Candidate that already has merge status `merged` and a stored
`merged_commit` succeeds idempotently and reports the stored commit
without resolving workspaces, rerunning checks, rerunning reviewers, or
moving the target branch. Merge artifacts live under
`.fluent/work/artifacts/<work-item-id>/<attempt-id>/<candidate-id>/merge/`,
and the stored Merge Candidate records whether execution is pending,
executing, failed, needs-user, or merged.

Once a Merge Candidate reaches merge status `merged`, merge execution
materializes the Attempt's successful learner handoff into the local
Observation backlog. It does this only after the merge is durable, so
nothing materializes before merge. It normalizes the verified handoff
into a source-neutral batch stamped with the authoritative origin (Work
Item, Attempt, Merge Candidate, and merged commit), records a versioned
`PendingPostLandOperationV1` and a resumable journal under
`.fluent/work/follow-ups/land-<digest>/`, where the digest identifies the Work
Item and Merge Candidate, and replays
that operation. Operation, Observation, and derived Work identities use hashes
of canonical component arrays, so delimiters and filename normalization cannot
collapse distinct origins or follow-ups. Recording and replay hold the same
per-operation lock while
they read or replace durable state. A repeated recording must match both the
stored operation identity and its verified batch, so a conflicting retry
cannot overwrite the accepted handoff. Replay creates exactly one provenance-bearing Observation
per follow-up, keyed by a deterministic id, so a land retry, recovery, or
journal replay reuses the same Observation rather than duplicating it; an
empty handoff records as processed without a placeholder; and a resolved
Observation is never reopened. System-generated Observations carry a
reserved YAML frontmatter block naming the follow-up and its origin.

After materializing each Observation, replay classifies the follow-up
through a source-neutral corrective host gate. A follow-up is corrective
only when it is marked corrective, carries a complete corrective context
and expected result, leaves no unresolved decision, and cites a trusted
authority — a behavior statement in `documentation/behaviors.md`, an
instruction in a tracked `AGENTS.md`, or a committed `.fluent/expertise/`
entry. The gate rejects non-normal paths and committed symlinks, reads the
authority from the regular-file blob at the operation's immutable landed
commit, and requires the corrective requirement to equal the non-empty
digest-matched anchor. Corrective proposals carry structured normalized target
paths; an `AGENTS.md` authority must be the closest applicable ancestor for
every target, including when a nested instruction overrides a root instruction.
Later `HEAD` movement, working-tree edits, and untracked files cannot authorize
or invalidate corrective Work. Any incomplete,
unsupported, unresolved, stale, or mis-namespaced context downgrades the
follow-up to Observation-only. When a follow-up first
validates as corrective, replay freezes the resolved follow-up policy
(mode, lineage limit, automatic priority, and configuration provenance)
into its journal receipt before creating any Work, so a retry reuses the
frozen decision even after configuration changes. It then derives one
corrective Work Item keyed by a deterministic id and linked to the
Observation and its originating lineage root. Reusing that id requires the
stored Work to match the complete expected provenance, corrective context,
lineage, authorization, and enqueue intent; unrelated Work is rejected. The
derived Work also retains the accepted expected result, trusted authority,
supporting evidence, unresolved-decision set, follow-up source, and learning
summary. Fluent includes this audit package in corrective task instructions,
so the Work stays executable and inspectable after cleanup removes its origin
handoff and post-land journal. In `propose` mode the Work
stays proposed with no queue entry; in `execute` mode it is authorized
automatically and enqueued on the regular Work Queue while lineage budget
remains, and stays proposed once the budget is exhausted. Concurrent
processors of the same operation serialize on an operation lock. Every
automatic promotion and human authorization also takes a lock keyed by the
root lineage before it counts or records charges, so different operations
cannot overspend one lineage. The lock order is follow-up operation, root
lineage, Work Item, then queue. Callers release the lineage and Work locks before
touching the queue; replay retains its outer operation lock through the queue
stage so another processor cannot observe a partially completed journal.
The Observation, Work Item, lineage charge, and queue entry each converge
exactly once.

`fluent work-item authorize <work-item-id>` transitions a proposed Work
Item to execution-ready under human authority and reconciles its
regular-queue dispatch. It holds a per-Work-Item lock across the read,
the transition, and the write, persisting the authorization, the lineage
charge — charged once even above the automatic descendant limit, so a
human can override an exhausted budget — and a durable enqueue intent in
one locked mutation before releasing the lock and touching the queue. If
the process crashes between the model write and the queue write, a repeat
authorization reconciles the missing dispatch from the durable intent.
Repeated authorization preserves the existing authorization, lineage
charge, and any active, terminal, or canceled queue disposition without
duplicating or reviving it. Authorizing execution never authorizes
landing.

Follow-up processing never undoes a successful land: a failure leaves the
merge intact and the persisted operation replayable, and re-running
`fluent merge-candidate land` on the merged candidate resumes it without
resolving workspaces, rebasing, or moving the target branch. A malformed
or origin-mismatched handoff, or a failure at any effect stage
(Observation, Work, or queue), keeps merge status `merged`, reports the
merged commit as successful, and records a retryable follow-up-processing
failure on the candidate naming the first incomplete stage and a next
action. The resumable journal preserves completed stages so a retry
produces each Observation, Work Item, lineage charge, and queue entry at
most once; a completed resume clears the recorded failure.
Land and post-land `attempt run` both call this same durable failure boundary.
If Learning already succeeded, `attempt run` resumes materialization
idempotently instead of rerunning the Learner; this closes the crash window
between persisting the handoff and recording or completing its operation.

`fluent cleanup` retains an origin — its Work Item, Attempt, Merge
Candidate, worktree, and managed artifacts — while its landed-learning
recovery is still live: a landed Attempt with a failed or legacy-missing
Learner record, or a landed candidate with a recorded follow-up failure, an
incomplete post-land journal, or a pending imported operation. Land retains
the candidate worktree for the first two cases. The shared recovery boundary
removes it after Learning and materialization complete, then clears the
recovery failure; a workspace-cleanup failure remains retryable. Cleanup may
then reap the origin. Derived Observations and Work Items stay inspectable with
self-contained corrective context and provenance identifiers even after
optional origin artifacts are gone.

A Learner run that failed before its candidate landed recovers through
`fluent attempt run`, which retries only the Learner. When the candidate
has already merged, that retry runs in handoff-only mode: it serializes
against land on the land lock, denies expertise writes, and discards any
commit it makes, so the merged commit and target branch stay unchanged.
The command persists its resolved coder mapping through a fresh field-level
Work-model mutation under that same lock; it never writes a whole Work Item
snapshot captured before the serialization boundary.
After taking the lock, retry re-reads the Attempt and candidate and skips the
coder when another retry has already persisted success. It resets the retained
candidate worktree and index to the candidate's stored `merged_commit` before
launch, so an interrupted unrecorded Learner commit cannot become the next
retry's baseline. The Learner diff uses the persisted accepted base. For a
legacy merged TaskOutput without that field, recovery reads the retained
candidate's rebase reflog to recover the exact target tip; this includes every
accepted candidate and merge-fix commit while excluding target-only history.
Only repositories whose reflog predates that sequence fall back to compatible
first-parent reconstruction.
Its sandbox exposes the shared Git directory read-only, strips the common
profile's broad temporary-tree write grants, and explicitly denies candidate
and shared-Git writes. This remains effective when managed worktrees themselves
live below macOS's temporary tree. After the coder exits, Fluent compares
protected refs plus the target checkout's HEAD, complete raw index, and
non-Fluent status. It restores refs with compare-and-swap, restores captured
index bytes and only changed checkout paths, and reapplies pre-existing staged,
unstaged, untracked, and index-flag state instead of globally resetting target.
Candidate commits, staged files, unstaged files, and untracked files are
collected as denied paths, then the candidate branch, index, and worktree reset
to the merged commit. Raw candidate HEAD and index metadata restore before
inspection, so cleanup still runs after hostile corruption or a nonzero coder
exit.
Land takes that lock before it reads or mutates candidate state, resolves
workspaces, or checks cleanliness, and holds it through merge finalization and
follow-up recovery. A retry therefore cannot make land observe its transient
index or worktree state.
Durable knowledge it could not write to expertise is recorded as a
non-corrective follow-up, which materializes as an Observation only. The
recovered handoff is then materialized immediately under the same
land-gated, idempotent rules the land hook uses.

Merge execution auto-continues through failed merge-time review
rounds within a same-invocation budget. The merge loop iterates
rebase → checks → reviews. If reviewers return fail (and not a
reviewer launch panic), Fluent invokes the same Coder used at
Attempt time against the candidate workspace, passing the failed
merge review artifact paths as input, asking the coder to address
the findings and commit. After the follow-up writer commits, the
loop restarts at rebase. One `fluent merge-candidate land` invocation may
advance at most `MAX_MERGE_FOLLOWUP_WRITES_PER_INVOCATION = 2`
follow-up write cycles. If a third round would be needed, Fluent
marks the Merge Candidate `needs-user`, writes a handoff under
`.fluent/work/artifacts/<work-item-id>/<attempt-id>/<candidate-id>/merge/needs-user.md`
naming the failed review artifact paths, and bails. Reviewer launch
panics or non-verdict errors are never retried — they fail the
merge immediately.

Coder runs whose transcript contains a session-limit or rate-limit
marker are not treated as Task failures. The Coder wrapper parses
structured rate-limit events from the transcript to determine when to
retry:

- Claude Code emits `{"type":"rate_limit_event","retry_after":N,...}`
  with a `retry_after` (seconds), `retry_after_ms` (milliseconds),
  or `reset_at` (ISO-8601 timestamp) field.
- Codex emits `{"type":"error","code":"rate_limit","retry_after":N,...}`
  with a `retry_after` (seconds) or `reset_at` field.

When a structured event carries parseable timing, the wrapper
computes a wait: `retry_at + jitter` where jitter is drawn uniformly
from `[0, FLUENT_RATE_LIMIT_JITTER_MAX_SECONDS]` (default 30).
When no structured timing is available, the wrapper falls back to
`FLUENT_RATE_LIMIT_RETRY_AFTER_SECS` (default 1800 seconds) plus
jitter — matching previous behavior on unstructured transcripts.

Per-run jitter uses the process PID and nanosecond timestamp to
produce independent values across concurrent Fluent runs, preventing
thundering-herd retries. The jitter function accepts the maximum as a
parameter internally (`rate_limit_jitter_with_max`) for testability;
the public `rate_limit_jitter()` reads the env var at call time.

The wrapper retries the same coder invocation up to two more times
before propagating the exit code. A `RateLimitState` tracker fires
macOS notifications (`osascript`) on state transitions: once on
entering rate-limit state (naming the reason and expected resume
time) and once on leaving (after the first successful invocation
following a pause). Repeated retries within the same pause do not
fire additional enter-state notifications.

Author and reviewer Tasks inherit this
behavior without further plumbing.

| Env var | Default | Purpose |
|---|---|---|
| `FLUENT_RATE_LIMIT_RETRY_AFTER_SECS` | 1800 | Fallback wait when no structured timing is available |
| `FLUENT_RATE_LIMIT_JITTER_MAX_SECONDS` | 30 | Maximum per-run jitter added to the retry wait |

`claude_auth.rs` detects Claude auth token expiry before and during
coder invocations, failing the Task with a clear recovery message
instead of letting the coder fail mid-Task with a cold 401. Two layers:

- **Prevention.** `ensure_not_expired()` reads the macOS Keychain entry
  under service `Claude Code-credentials`, parses the `claudeAiOauth`
  object, and checks `expiresAt` against a 5-minute margin. If the
  token is expired or about to expire, it returns `AuthError::Expired`
  and the caller bails with an error message naming `claude /login` as
  the recovery action. When the keychain entry is missing, malformed,
  or has no `refreshToken` (API-key path), the check returns `Ok(())`
  and the coder launches as before. `SandboxedClaudeCode::run` and
  `BareClaudeCode::run` call this before every launch;
  `CodexCode::run` does not.

- **Recovery.** `classify_transcript_401()` walks the transcript JSONL
  for `result` events with `api_error_status == 401`. When the most
  recent `result` event is a 401, it returns `AuthError::Rejected`. The
  retrying loop in `run_with_transcript_retrying` checks this before
  rate-limit detection so 401 wins when both match. The caller bails
  with the same recovery message.

Automatic OAuth refresh is explicitly out of scope. The coder returns
a typed `AuthError` (via `anyhow::Error::new`) instead of a plain
string `bail!`, so callers can recover the type via
`downcast_ref::<AuthError>()`. Both task retry loops in the task
executor recognize `AuthError`, skip retries entirely, and escalate
immediately to `needs-user` with the auth-specific handoff message
from `AuthError::user_message()`. Other coder errors still retry up
to `FLUENT_MAX_TASK_RETRIES`. A follow-up Work Item will tackle
OAuth refresh once the correct request format for the Claude.ai
subscription OAuth endpoint is known.

`fluent cleanup` owns the terminal Work model cleanup lifecycle. It
defaults to a dry run and only mutates state with `--apply`. A Work Item
is eligible when every Attempt is terminal, every Task in those Attempts
is terminal, and every Merge Candidate is terminal. Operators can also
run `fluent work-item abandon <work-item-id> [--reason <text>]` to mark a
stale Work Item as intentionally abandoned; cleanup treats that marker as
terminal only when no Attempt is executing or reviewing, no Task is
executing, and no Merge Candidate is reviewing or merging. Cleanup removes
the stored Work Item, referenced managed Work artifacts, managed candidate
worktrees, Work task branches, and stranded sibling reviewer worktrees
left behind by killed merges. Managed artifact references must be
relative paths made only of normal path components and must resolve under
`.fluent/work/artifacts/`; cleanup ignores absolute paths and parent
escapes. It skips Work Items with active Attempts, Tasks, or Merge
Candidates, and it only removes candidate worktrees that match Fluent's
managed sibling path and are registered git worktrees.

| Concept | Meaning |
|---|---|
| Work Item | Planned Fluent work. Planning operates on work items. |
| Attempt | One execution history branch under a work item. Attempts are visible state and history, but they are usually not their own queue. |
| Task | Schedulable unit of work. Task kinds stay generic: `write`, `review`, `merge`, `report`, `learn`, `probe`, and `tester`. Roles carry prompt and domain behavior. |
| Workspace | Fluent-managed filesystem/git context. A task may read many workspaces and write at most one. |
| Merge Candidate | Candidate result prepared for merge. Its review state is separate from attempt review state. |

When artifacts or tests need to exchange a standalone task definition,
use the serialized `Task` shape from `fluent::work_model` and call
`Task::validate` after parsing. This shape is an exchange contract for
the core model.

```json
{
  "id": "attempt-1-write-1",
  "kind": "write",
  "role": "author",
  "instructions": "Preserve coder flags as args.\nKeep prompt content in Work state.",
  "work_item_id": "work-1",
  "attempt_id": "attempt-1",
  "workspace_access": {
    "reads": [],
    "writes": [
      {
        "id": "candidate",
        "path": "../work-6-work-1-attempt-1"
      }
    ]
  },
  "artifact_area": {
    "path": ".fluent/work/artifacts/work-1/attempt-1/attempt-1-write-1"
  },
  "status": "complete",
  "output": {
    "workspace_id": "candidate",
    "workspace_path": "../work-6-work-1-attempt-1",
    "source_branch": "main",
    "commit": "0123456789abcdef"
  }
}
```

The `kind` field accepts `write`, `review`, `merge`, `report`,
`learn`, `probe`, or `tester`. The value `behavior-tests` is accepted
on read for backward compatibility with stored state but is not created
by current code. `workspace_access.reads` may list any number of
workspaces. `workspace_access.writes` may be empty or contain one
workspace. A `review` task must keep `writes` empty; reviewers write
findings and notes under a required `artifact_area`.

Write Tasks carry an `artifact_area` under
`.fluent/work/artifacts/<work-item-id>/<attempt-id>/<task-id>/`,
matching the review convention. The writer's Coder persists
`transcript.jsonl` into this directory during execution. The writer's
sandbox grants write access to both the candidate workspace and the
artifact directory. Review Tasks also persist `transcript.jsonl`
alongside their primary artifact (`review.md`). Tester Tasks write
`tester-results.json`, per-command log files, and `commands.json` to
their artifact directory but do not write a transcript. Reviewer
sandboxes intentionally exclude writer artifact directories and other
reviewers' artifact directories to preserve independent verification.

### progress.md

Each Attempt with a plan.md records a `progress.md` file at
`.fluent/work/artifacts/<work-item-id>/<attempt-id>/progress.md`,
gitignored alongside other artifacts. It holds two sections:

- ## Checklist — one `- [ ]` or `- [x]` line per plan.md step,
  in plan.md order
- ## Notes — `### Step N` subsections with Done / Note / Next
  lines for each completed step

The writer reads progress.md at the start of every step (so
context compaction doesn't lose state), picks the first unchecked
item, commits code, then updates progress.md outside git.
Reviewers receive progress.md as an input artifact and may
cross-check plan-step coverage.

Write Tasks may include optional `instructions` copied from explicit Work
Item instructions or derived from Work Item planning context. JSON omits
`instructions` when the Task has no rich execution context.

`status` tracks Task lifecycle state: `planned`, `executing`,
`complete`, `failed`, or `needs-user`. Planned Tasks omit the field in
JSON. Completed write Tasks include `output`, which records the writable
workspace id and path, the source branch resolved from the project root,
the immutable base commit captured before the coder ran, and the commit that
contains the Task output. A post-land Learner uses the stored base and merged
candidate commit for its complete-change diff, so advancing the source branch
cannot turn the retry prompt into an empty diff.
Planned review Tasks include `review_context`, copied from that write
output, with the candidate workspace id and path, source branch, and
candidate commit. Follow-up write Tasks include `input_artifacts` when
reviewers fail an Attempt; each entry names the producing review Task
and the artifact path, such as
`.fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md`.
The Attempt loop uses those producer task ids to choose the reviewers
for the next follow-up review round. When Fluent plans that targeted
round, each review Task receives the matching prior failed review
artifact for its role in `input_artifacts`, so the executor can prompt
the reviewer to verify the follow-up against the concrete findings and
grant sandboxed read access to that artifact.
JSON omits `input_artifacts` when the list is empty. Incomplete Tasks do
not carry output. Attempt
completion is derived from its Tasks; a complete Attempt must not contain
unfinished Tasks, and completing one Task does not by itself complete an
Attempt that still has unfinished Tasks. Completed review Tasks record
artifact references under the Attempt and do not set `output`; their
markdown verdict remains data, not Task execution status.

Review tasks are read-only with respect to candidate workspaces,
including managed candidate workspaces beside the source checkout.
Fluent grants sandboxed review Tasks read-only access to candidate
worktrees and their shared git metadata so standard commands such as
`git diff <source>..<candidate>` work from the candidate workspace. They
may write task artifacts, such as findings or scratch notes, but concrete
reviewer fixes become follow-up `write` tasks. Sandboxed delegated tasks
produce learning artifacts in task artifact areas; Fluent can later
ingest those artifacts into project-local expertise after review.
Work-model behavior reviewers receive the Work Item behavior increment
from `WorkItem.planning_context.behaviors` in the prompt when one exists,
or an explicit statement that no behavior increment was provided.

Project-local `.fluent/expertise/*` is durable Fluent memory that
belongs in normal repository history. `.fluent/observations/` is a
local working backlog that stays ignored and is not committed.
Observations are stored as one file per entry under
`.fluent/observations/<id>.md` (open) and
`.fluent/observations/resolved/<id>.md` (resolved). The `fluent
observations` CLI surface manages the lifecycle: `add` records a new
observation, `resolve` appends resolution context and moves the file,
`list` prints the open queue, and `show` prints a single entry. Manual
observations are body-only Markdown; a system-generated observation adds
a reserved YAML frontmatter block carrying its provenance, which `list`
skips when it derives the summary line. The per-file layout prevents
write collisions when parallel sessions add or resolve observations
concurrently. A one-shot `migrate` command
converted the prior monolithic `observations.md` and
`observations-resolved.md` into the per-file layout.

Durable work model state lives under `.fluent/work/`. Keeping learning
and planning separate from transient session state lets them land through
the same reviewed workflow as code without treating session state as
project knowledge.

Managed candidate worktrees do not live under `.fluent/work/`. Fluent
keeps Work Item JSON, review artifacts, merge artifacts, and operator
state in the source checkout, but it creates candidate git worktrees as
sibling directories beside the source checkout. Stored workspace
references remain relative to the source root, for example
`../work-6-work-1-attempt-1`, and include a Work Item ID byte-length
prefix, Work Item ID, and Attempt ID so valid hyphenated IDs remain
globally distinct.

The Work storage contract is:

```
.fluent/work/
  items/
    <work-item-id>.json
  attempts/
    <work-item-id>/
      <attempt-id>.json
  tasks/
    <work-item-id>/
      <attempt-id>/
        <task-id>.json
  merge-candidates/
    <work-item-id>/
      <merge-candidate-id>.json
  artifacts/
    <work-item-id>/
      <attempt-id>/
        <task-id>/
        <merge-candidate-id>/merge/
```

Each file in `items/` stores Work Item metadata and planning context:
the Work Item id, title, optional explicit instructions, optional
brief/behaviors/approach/plan context, and optional abandonment marker
with the operator-provided reason. Attempts live in
`attempts/<work-item-id>/<attempt-id>.json`, Tasks live in
`tasks/<work-item-id>/<attempt-id>/<task-id>.json`, and Merge Candidates
live in `merge-candidates/<work-item-id>/<merge-candidate-id>.json`.
`WorkModelStore` assembles those split records into the public
`WorkItem` shape from `fluent::work_model` for `fluent work-item show`,
status, dashboard, task execution, review, merge, and cleanup.
If an item file contains nested Attempts, Tasks, or Merge Candidates,
Fluent ignores those nested operational collections and exposes only
records from the split Attempt, Task, and Merge Candidate collections.
Attempt records carry `kind`, which serializes as `write` or
`review-only`; older records and omitted values default to `write`.
Write attempts omit `kind` on new writes, and review-only attempts
persist `kind: "review-only"` so the attempt loop can interpret review
verdicts as terminal output instead of post-author feedback. Attempt
records also carry an internal `order` field so the assembled public
Work Item preserves append order after Fluent reloads split records.
Task records carry the same internal `order` field; Fluent sorts split
Task files by that persisted order before exposing `Attempt.tasks`.
Fluent writes Work Item metadata to `items/` and operational records to
the split Attempt, Task, and Merge Candidate collections.
Tasks carry optional `created_at`, `started_at`, and `completed_at`
timestamps (ISO 8601 / RFC 3339, UTC). Attempts carry `created_at` and
`completed_at`. Merge Candidates carry all three. Fluent populates
`created_at` at construction, `started_at` at the first transition out
of the initial state, and `completed_at` at terminal transitions.
Helper functions `mark_task_started`, `set_task_terminal`,
`set_attempt_terminal`, `mark_merge_candidate_started`, and
`set_merge_candidate_terminal` in `work_model.rs` centralize timestamp
assignment so every transition site uses the same format. Existing
JSON files that lack the fields deserialize with `None` values; keys
with `None` values are omitted on write.
When `WorkModelStore` reads stored Work state, it normalizes older
artifact references that still use
`.fluent/work/artifacts/<attempt-id>/...` into the current
`.fluent/work/artifacts/<work-item-id>/<attempt-id>/...` form before it
validates the assembled Work Item. If the older path exists on disk and
the namespaced path does not, the store moves that artifact directory or
file into the Work Item namespace during the read.

Tasks store their workspace access under `workspace_access.reads` and
`workspace_access.writes`. Workspace references stay inside task
`workspace_access.reads` and `workspace_access.writes` and point to
managed sibling worktrees for candidate execution. Merge Candidates store
the reviewed source candidate workspace and target workspace directly as
boundary data derived from the passed Attempt's latest completed write
Task. Fluent does not keep a standalone workspace registry in this
contract. Merge Candidates use the public `MergeCandidate` shape and have
their own candidate collection.

Code that reads `.fluent/work/` state must parse records into the public
Rust model and validate every assembled Attempt, Task, and Merge
Candidate before using the object.
The merge executor is the only recovery reader in this contract: it may
load a Work Item that fails merge-execution preconditions so it can mark
the affected Merge Candidate failed, but it must validate the candidate
before it updates a workspace or target branch. Failed Merge Candidates
still must preserve the boundary data derived from the latest completed
write Task; the failed state only records why merge execution stopped.
The `WorkItem.id` inside each item file must match the file stem, so
`.fluent/work/items/work-1.json` must contain `"id": "work-1"`.
Attempt, Task, and Merge Candidate ids must match their file stems.
Work item IDs, Attempt IDs, Task IDs, and Merge Candidate IDs must not be
empty, `.`, `..`, or contain `/` or `\`, because Fluent uses each ID as
one path component under the split collections. Each stored Attempt must
set `work_item_id` to the containing `WorkItem.id` and store its append
position in `order`. Each stored Task must set `work_item_id` to the
containing `WorkItem.id`, and must set `attempt_id` to the containing
Attempt id even though the public Task shape allows `attempt_id` to be
absent before a task joins an Attempt.
Invalid JSON, ID mismatches, invalid object IDs, and model validation
failures must report the file path or object that failed. Code that
writes Work state must use deterministic pretty JSON and must not write
invalid model state.

## Pre-merge hooks

Projects wire pre-merge verification through the hooks system. A
single executable at `.fluent/hooks/check-pre-merge` gates merging:
exit 0 lets it proceed, non-zero stops before any destructive step.
A sibling executable at `.fluent/hooks/fix-pre-merge` (optional)
is invoked when `check-pre-merge` fails. Fluent requires the
worktree to be clean before running the fix hook, commits any
changes the fix produces outside `.fluent/`, reruns
`check-pre-merge`, reruns reviewers after the fix commit, and
continues only when the required checks and reviews pass. The
project decides what each script invokes — `cargo fmt`,
`make ci`, `pre-commit run --all-files`, anything else.

Example:

```sh
#!/bin/sh
# .fluent/hooks/check-pre-merge
set -e
cargo fmt --all -- --check
```

```sh
#!/bin/sh
# .fluent/hooks/fix-pre-merge
cargo fmt --all
```

## Version Metadata

`fluent version` prints a single line:

```sh
fluent 0.1.0 abc1234
```

The first field is the literal `fluent` command name. The second field
is the package version from `Cargo.toml`. The third field is the short
Git commit captured by `build.rs` when Cargo builds the binary. If Git
is unavailable at build time, Fluent prints
`unknown` in the commit field.

## Self-update

`fluent update` replaces the binary with the latest release from the
configured release source, verifies the download against a published
SHA-256 checksum, and re-materializes skills by invoking the new
binary. The updater queries
`{api_base}/repos/{owner}/{repo}/releases/latest` via curl, downloads
the platform asset `fluent-{target-triple}` and its `.sha256` file,
verifies the checksum, and atomically replaces the binary via POSIX
rename. After replacement, the updater shells out to the new binary's
`skills` command so skills are always in sync with the binary.

On every command except `fluent update`, Fluent runs a cached update
check: it queries the release source at most once per 24 hours, caches
the result at `~/.config/fluent/update-check.json`, and prints a nudge
to stderr when the current version is behind. The check never downloads
or replaces the binary. Setting `FLUENT_NO_UPDATE_CHECK` suppresses
both the query and the nudge. Offline check failures are silent.

| Env var | Default | Purpose |
|---|---|---|
| `FLUENT_NO_UPDATE_CHECK` | (unset) | Suppress update check and nudge when set |
| `FLUENT_RELEASE_REPO` | `mrinalwadhwa/fluent` | GitHub `owner/repo` for release queries |
| `FLUENT_API_BASE` | `https://api.github.com` | GitHub API base URL |
| `FLUENT_BINARY_PATH` | `current_exe()` | Override binary path (testing) |
| `FLUENT_UPDATE_CACHE_PATH` | `~/.config/fluent/update-check.json` | Override cache file path (testing) |

## Agents

### Coder selection

Fluent supports three Coders: Claude Code, OpenAI Codex, and Pi.
Claude is the default. Select a different Coder with `--coder codex`,
`--coder pi`, or `FLUENT_CODER=<coder>`.

Each Attempt stores a per-Task-kind **coder mapping** that determines
which Coder and model run each Task kind (write, review). Tester Tasks
bypass the coder mapping entirely — they run as a deterministic
subprocess without a Coder. Configure with per-Task-kind flags like
`--write-coder pi --write-model qwen3-30b-a3b --review-coder claude`.
When no per-Task-kind flag is set, `--coder` or `FLUENT_CODER` sets
all Task kinds to the same Coder. The resolved mapping is stored on
the Attempt record so different Attempts of the same Work Item can
use different Coder configurations.

Claude sessions use `claude -p --append-system-prompt` with stream-json
output. Sandboxed Claude sessions run inside the macOS Seatbelt profile
that Fluent renders for the worktree plus the source repository's
common git directory. The worktree root lets the agent edit project
files; the common git directory lets linked worktrees update branch,
index, and worktree metadata without granting write access to unrelated
sibling worktrees.

Codex sessions use `codex --ask-for-approval never exec --json --cd <worktree>`
and receive the fluent system prompt prepended to the session prompt
because the Codex CLI has no Claude-style append-system-prompt flag.
`--ask-for-approval` is a top-level Codex option and must appear before
`exec`. Sandboxed local Codex sessions are wrapped by Fluent's macOS
Seatbelt profile with the same writable roots as Claude: the
worktree and source repository common git directory. Fluent passes
`--dangerously-bypass-approvals-and-sandbox` to Codex in this mode so
Codex does not apply its own sandbox or pause for approvals inside the
Fluent sandbox. Fluent also sets `SSL_CERT_FILE` for sandboxed Codex
using a file-based CA bundle so Codex can connect without Keychain IPC.
`FLUENT_CODEX_CA_BUNDLE` overrides the detected bundle path and any
caller-provided `SSL_CERT_FILE`. In bare mode, Codex also runs with
`--dangerously-bypass-approvals-and-sandbox`, but without
`sandbox-exec`.

Pi sessions use `pi -p <prompt> --append-system-prompt <file> --mode json
--thinking off --provider local-openai --model <model>`. Pi talks to a
local vllm-mlx server (default `127.0.0.1:8000`) configured via
`~/.pi/extensions/local-vllm.js`. Fluent does not manage the vllm-mlx
server lifecycle — the user starts it externally. The default model is
`qwen3-30b-a3b`, overridden by `FLUENT_PI_MODEL`. Sandboxed Pi sessions
run inside a Seatbelt profile with read access to `~/.pi/` and write
access to `~/.pi/agent/`. Pi is local-only and cannot run on
Fargate.

Fargate supports both Claude and Codex. The base image
(`infrastructure/run/Dockerfile`) installs both `@anthropic-ai/claude-code`
and `@openai/codex` via npm so `claude` and `codex` are both on the
`PATH`. The entrypoint dispatches on `FLUENT_CODER` (default `claude`),
validates the coder-specific auth env var, and passes `--coder $CODER` to
the fluent binary.

Coder-specific auth for Fargate:

| Coder | Auth env var | Source on host | In-container setup |
|-------|-------------|----------------|-------------------|
| Claude | `CLAUDE_CODE_OAUTH_TOKEN` | macOS Keychain / env | Passed as-is |
| Codex | `CODEX_AUTH_JSON` | `~/.codex/auth.json` | Written to `${HOME}/.codex/auth.json` (mode 0600) |

The host-side launcher (`fargate.rs::coder_task_overrides`) reads the
appropriate auth source, validates it, and passes it as an ECS task
override. For Codex, the host-side check and the entrypoint both
validate `auth_mode == "chatgpt"` to preserve ChatGPT subscription
billing. The entrypoint also unsets `OPENAI_API_KEY` and rejects any
`~/.codex/config.toml` containing `preferred_auth_method = "apikey"`.

The Fargate run image builds the Rust Fluent binary during the Docker
image build and copies it to `/usr/local/bin/fluent`; the task
entrypoint dispatches to `fluent attempt run` or
`fluent merge-candidate land` depending on the mode.

Sandboxed local Claude runs refresh Claude OAuth credentials outside the
sandbox at session boundaries. Sandboxed local Codex runs do not use that
Claude refresh hook.

### Author

Implements code. Follows the plan. Pauses when genuinely uncertain rather
than drifting.

### Reviewers

Evaluate the author's output. Five reviewers run in parallel, each
following its own skill:

- Documentation (code-aware) — reads code and docs, checks accuracy,
  writing quality, and completeness.
- Behaviors (user-facing) — observes behavior only, cannot see code.
  Evaluates the system from the outside, as a user would.
- Architecture (code-aware) — reads code and architectural expertise,
  evaluates structural decisions against principles.
- Skills (code-aware) — reads skill files and checks them against
  `references/skills.md` for structure, quality, and spec compliance.
- Tests (code-aware) — reads tests and evaluates coverage, isolation,
  structure, and adherence to testing principles.

Review verdicts: **pass** / **uncertain** (ask user) / **fail** (send
back to author with findings).

When the author receives findings from multiple reviewers, it weighs
each finding according to the reviewer's domain expertise. When reviewers
disagree, the one with relevant expertise for that finding takes priority.
The author escalates to `needs-user` only when genuinely stuck.

## Runtimes

### Local

The fluent command runs Tasks on the local machine. Claude and Codex
run inside a macOS Seatbelt sandbox rendered by Fluent. Fluent renders
each sandbox from `common.sb` plus the selected coder's profile layer:
`claude-code.sb` for Claude Code and `codex.sb` for Codex. Claude uses
the Claude token refresh hook at session boundaries; Codex does not.

### Local (bare)

`--no-sandbox` runs without Seatbelt sandboxing, Codex sandboxing, or
credential refresh. A git worktree is still created when the directory
is a git repo. Used on platforms without local sandbox support or when
the agent is already isolated by other means. Claude runs with
`--dangerously-skip-permissions`; Codex runs with
`--dangerously-bypass-approvals-and-sandbox`.

### Fargate

Single-container model on AWS ECS Fargate.

The `fargate_bootstrap.rs` module deploys infrastructure and builds
images just-in-time on the first `--runtime fargate` invocation
(and again whenever inputs change). The Dockerfile at
`infrastructure/run/Dockerfile` compiles the Rust Fluent binary in
a builder stage and copies it into the task image at
`/usr/local/bin/fluent`, so task startup only transfers the
workspace and invokes the binary.

```
Local machine                    Fargate task
─────────────                    ────────────
1. upload project workspace → S3
2. start ECS task ───────────►
                                 3. pull workspace from S3
                                 4. fluent attempt run
                                    --no-sandbox --coder $CODER
                                    <work-item> <attempt>
                                 5. Fluent launches coder
                                 6. ...hours pass...
                                 7. upload workspace → S3
```

#### IAM permissions (minimal)

| Permission | Scope | Purpose |
|---|---|---|
| `s3:GetObject` | `work/*`, `work-merge/*` | Pull input workspace |
| `s3:PutObject` | `work/*`, `work-merge/*` | Upload completed workspace |
| `s3:*` Deny | Outside the allowed prefixes | Explicit deny on everything else |
| `ssmmessages:*` | `*` | Accept incoming ECS Exec sessions |

Six actions total. No ECS, IAM, STS, or other AWS permissions. The
container can be connected to (ECS Exec) but cannot connect out to other
containers via SSM. `work/` covers Work Attempt artifacts and
`work-merge/` covers Merge Candidate artifacts.

#### Infrastructure (CloudFormation)

- 1 ECR repository (`fluent/run`)
- 1 ECS cluster
- 1 task definition (1 vCPU, 2 GB RAM, 30 GB ephemeral storage)
- 1 S3 bucket (30-day lifecycle)
- 1 IAM task role (6 actions)
- 1 IAM execution role (ECR pull + logs)
- 1 security group (egress only)
- CloudWatch log group (optional, infra debugging)

No EFS. Fargate ephemeral storage is sufficient for a single container.

#### Worktree layout

The Fargate path uses a worktrees-root layout that matches the
local layout (project root + sibling candidate/review worktrees,
all under a single parent directory).

Container layout:

```
/worktrees/
├── ${FLUENT_PROJECT_NAME}/              project root
├── work-<bytelen>-<id>-<attempt>/        candidate worktree
└── review-<bytelen>-<id>-<attempt>-...   review worktrees
```

`FLUENT_PROJECT_NAME` is the basename of the local project root
(e.g. `main`) passed as a task environment override. The container's
`WORKSPACE` then resolves to `/worktrees/${FLUENT_PROJECT_NAME}`, so
Fluent's `initial_candidate_workspace_path = "../<name>"` naturally
lands siblings at `/worktrees/work-...` and `/worktrees/review-...`
beside the project root.

Local layout mirrors this:

```
<project_root>/..  (e.g. /Users/mrinal/Workspace/fluent/)
├── main/                                project root
├── work-<bytelen>-<id>-<attempt>/
└── review-<bytelen>-<id>-<attempt>-...
```

Tarball format (symmetric input and output): one top-level entry per
worktree, no wrapper directories.

| Direction | Tar `-C` directory | Contents |
|-----------|--------------------|----------|
| Local → S3 (input) | `<project_root>/..` | the project basename only |
| S3 → container (input) | `/worktrees` | files restored under `/worktrees/<project>/` |
| Container → S3 (output) | `/worktrees` | project + any sibling worktrees |
| S3 → local (output) | `<project_root>/..` | project overwritten and siblings restored |

Commands:

```
fluent attempt run   --runtime fargate <work-item-id> <attempt-id>
fluent attempt watch                   <work-item-id> <attempt-id>
fluent attempt pull                    <work-item-id> <attempt-id>
fluent attempt stop                    <work-item-id> <attempt-id>

fluent merge-candidate land  --runtime fargate <work-item-id> <candidate-id>
fluent merge-candidate watch                   <work-item-id> <candidate-id>
fluent merge-candidate pull                    <work-item-id> <candidate-id>
fluent merge-candidate stop                    <work-item-id> <candidate-id>
```

The local launcher uploads the project workspace to
`s3://<bucket>/work/<work-item-id>/<attempt-id>/workspace-in.tar` (or
`work-merge/<work-item-id>/<candidate-id>/workspace-in.tar`), launches
the ECS task with `FLUENT_WORK_ITEM_ID`, `FLUENT_PROJECT_NAME`, and
either `FLUENT_WORK_ATTEMPT_ID` or `FLUENT_WORK_MERGE_CANDIDATE_ID`,
and records the task ARN under
`.fluent/work/runtime/{attempts,merges}/<id>/.../fargate-task-arn`.

`watch` polls `aws ecs describe-tasks` until `lastStatus=STOPPED`,
printing transitions and the final `stopCode`/`stoppedReason`.

`stop` reads the recorded task ARN and calls `aws ecs stop-task`. The
call is idempotent: an already-stopped or absent task returns Ok.

After changes to `entrypoint.sh`, the base image's Dockerfile, or
the Fluent binary, the next `--runtime fargate` invocation detects
the input change via the hash recorded in
`~/.config/fluent/fargate.state.json` and rebuilds + pushes the
base image automatically. A rebuilt base also triggers a rebuild
of any project image that FROMs it. The `FLUENT_FARGATE_FORCE_REBUILD`
environment variable forces the chain regardless of cached state.

#### Just-in-time bootstrap

`src/fargate_bootstrap.rs::ensure_setup` is called before every
Fargate launch. It is idempotent: on first use it discovers the
default VPC and subnets, deploys the CloudFormation stack named
`fluent`, reads stack outputs (cluster ARN, task-definition ARN,
ECR repository URI, S3 bucket, security group), authenticates
Docker with ECR, builds the Fluent base image from the embedded
`infrastructure/run/Dockerfile`, pushes it, and writes everything to
`~/.config/fluent/fargate.state.json`. The state file records
the deployed region, stack output values, a hash of the base image
inputs, and per-project hashes of `.fluent/Dockerfile`. On later
invocations Fluent recomputes the hashes and only rebuilds when
they change. The Fluent source tree must be locatable: either
`FLUENT_SOURCE_ROOT` is set explicitly, or Fluent walks up from
the project root looking for a directory that contains both
`Cargo.toml` and `infrastructure/run/Dockerfile`.

#### Per-project images

Fluent publishes a thin base image (`fluent` binary +
`claude-code` + `codex` + minimum runtime) and each project extends it with
whatever toolchains its merge checks require through
`.fluent/Dockerfile`.

**Tag scheme.** Both base and project images live in the same ECR
repository. The base image is tagged
`fluent-base-<fluent-version>` (e.g. `fluent-base-0.1.0`)
using `env!("CARGO_PKG_VERSION")`. The project image is tagged
`project-<sha256-first-12-hex>` where the hash is the SHA-256 of
`.fluent/Dockerfile` (e.g. `project-a3f2b8c9d4e1`).

**Auto-stub.** When `fluent fargate ensure-setup` runs and
`.fluent/Dockerfile` does not exist, Fluent creates a stub
containing `ARG FLUENT_BASE_URI` and
`FROM ${FLUENT_BASE_URI}` with a comment explaining how to
extend it. The stub is left uncommitted for the user to inspect
and version-control.

**Build-arg portability.** Project Dockerfiles use
`ARG FLUENT_BASE_URI` instead of a literal ECR URI. Fluent
passes `--build-arg FLUENT_BASE_URI=<resolved-uri>` when
invoking `docker build`. This keeps the file content (and
therefore its SHA-256 tag) stable across developers who push
to different ECR registries.

**ECR skip-if-exists.** Before building either image, Fluent
calls `aws ecr describe-images --image-ids imageTag=<tag>`. If
the tag exists, the build and push are skipped. The local state
file hash check remains as a short-circuit when state is intact;
the ECR check covers the state-wiped case.

**Task definition revision.** After a successful project image
push, Fluent registers a new ECS task definition revision that
updates the container image URI to the new project tag. The
`run-task` call uses the task definition family name (without
revision number) so AWS auto-resolves to the latest active
revision.

This repo ships `.fluent/Dockerfile` that extends the Fluent
base with the Rust toolchain via rustup so `cargo fmt --check`,
`cargo test`, and `cargo clippy` execute successfully under the
merge-check hook on Fargate.

#### Teardown

`fluent fargate teardown` removes the Fargate infrastructure
deployed by `ensure_setup`. It reads
`~/.config/fluent/fargate.state.json` to learn the region, ECR
repository, and S3 bucket, then deletes the ECR repository (unless
`--keep-ecr`) including all base image tags (`fluent-base-*`) and
project image tags (`project-*`), empties and deletes the S3 bucket
(unless `--keep-s3`), deletes the CloudFormation stack and waits for
deletion to complete, and removes the state file so the next
`--runtime fargate` invocation re-bootstraps from scratch. Each
destructive step checks for existence before deleting: a missing
stack, absent ECR repository, or absent S3 bucket is treated as
success. When no state file exists and no stack is present, the
command exits zero with a message saying nothing needed teardown.

## Credential management

### Local runtime

| Credential | Source | Method |
|---|---|---|
| Claude OAuth | macOS Keychain | Extract, pass as env var. Refresh via unsandboxed `claude -p "ok" --max-turns 1` at session boundaries. |
| AWS | SSO profile | `aws configure export-credentials` resolves to STS temps, passed as env vars. |
| Brave Search | macOS Keychain | Extract, pass as env var. |

Sandbox profile unchanged — credentials injected via env vars, never by
opening filesystem access.

### Fargate runtime

Claude OAuth token passed as env var at task launch. Short-lived; multi-hour
runs will outlive it. Future: WIF (Workload Identity Federation) for
automatic token refresh using the task's IAM identity.

## Repository structure

```
fluent/main/
  CLAUDE.md
  build.rs                   ← emits build-time metadata
  Cargo.toml                 ← Rust crate definition
  Cargo.lock
  src/
    main.rs                  ← CLI dispatch (clap)
    lib.rs                   ← public API for tests
    auto_merge.rs            ← Auto-merge watcher for merge-ready Work Items
    tester.rs                ← Tester subcommand (deterministic test runner)
    claude_auth.rs           ← Claude OAuth authentication
    cleanup.rs               ← Cleanup of terminal Work state
    cli.rs                   ← CLI argument types
    coder.rs                 ← Coder trait + Claude/Codex implementations
    content.rs               ← Runtime content resolution (project → user → bundled)
    credential.rs            ← Keychain credential injection
    dashboard.rs             ← Live TUI for Work Item activity
    fargate.rs               ← Fargate launch, watch, stop, and pull for Work execution
    fargate_bootstrap.rs     ← JIT Fargate setup (CFN, base + project image builds)
    follow_up.rs             ← Land-gated follow-up materialization: pending operation, journal, corrective host gate, derived Work intake
    git.rs                   ← Git command wrapper
    hooks.rs                 ← Project hook execution (.fluent/hooks/<name>)
    keep_awake.rs            ← macOS idle-sleep prevention toggle
    lineage_lock.rs          ← Root-lineage serialization for corrective Work charges
    notify.rs                ← Push notification delivery
    observations.rs          ← Observation CRUD and lifecycle
    os.rs                    ← Seatbelt sandbox rendering, prerequisites
    plan.rs                  ← Parse plan.md into groups and steps
    post_merge_review.rs     ← Post-merge review orchestration
    prep.rs                  ← Pre-flight workspace preparation
    queue.rs                 ← Per-Work-Item dispatch ledger (history + one active dispatch)
    review.rs                ← Review verdict parsing and state
    review_diff_command.rs   ← Review diff CLI subcommand
    scheduler.rs             ← Elected coordinator running a capacity-limited worker pool
    transcript.rs            ← Parse stream-json transcripts incrementally
    update.rs                ← Self-update and update-check nudge
    usage.rs                 ← Per-turn usage row logging and summary cache
    version.rs               ← Version command output format
    work_attempt_loop.rs     ← Advance one Work model Attempt
    work_merge_executor.rs   ← Execute Work Merge Candidates
    work_model.rs            ← Core Work Item / Attempt / Task model
    work_status.rs           ← Summarize Work Items for status and dashboard
    work_task_executor.rs    ← Execute Work Tasks
    worktree.rs              ← Git worktree helpers (signing, repo detection)
  documentation/
    architecture.md          ← this file
    behaviors.md             ← behavioral statements (EARS)
  expertise/                 ← fluent-level (applies to all projects)
    architecture.md
    documentation.md
    shell-scripts.md
    skills.md
    terminal-ui.md
    tests.md
  .fluent/
    Dockerfile               ← per-project Fargate image (Rust toolchain)
    tester.yaml              ← test command declarations for the Tester subcommand
    extract-tester-results   ← normalize raw test output into per-test JSON
    observations/             ← per-file observation queue (local, not tracked)
      <id>.md                ← open observations
      resolved/
        <id>.md              ← resolved observations
    expertise/               ← project-level learnings (tracked)
    hooks/                   ← project hook scripts (tracked)
      check-pre-merge
      fix-pre-merge
    work/                    ← Work model durable state (not tracked)
  prompts/                   ← agent system prompts
    review-architecture.md
    review-behaviors.md
    review-documentation.md
    review-skills.md
    review-tests.md
    work-author.md           ← Work model author agent prompt
    work-rebase.md           ← Work model rebase agent prompt
  sandboxes/                 ← Seatbelt profile templates
      common.sb              ← Shared Seatbelt profile template
      claude-code.sb         ← Claude-specific Seatbelt profile layer
      codex.sb               ← Codex-specific Seatbelt profile layer
  skills/
    fluent/SKILL.md
    fluent/references/               ← stage procedures and expertise (dereferenced at build time)
    review-architecture/SKILL.md
    review-behaviors/SKILL.md
    review-documentation/SKILL.md
    review-skills/SKILL.md
    review-tests/SKILL.md
  scripts/
    release.sh               ← Build, checksum, and publish a GitHub release
  infrastructure/
    cloudformation.yaml
    run/
      Dockerfile
      entrypoint.sh
  tests/
    behaviors/
      operations/            ← behavioral tests for the Rust binary
      skills/                ← scenario cards for test-skill
      README.md              ← behavior-to-test mapping
    lib/
      log.rs                 ← LoggedCommand wrapper for Rust tests
      run_test.sh            ← shared run_test helper for shell tests
    output/                  ← per-case logs (git-ignored, created on run)
```

### Per-test log output

Both the Rust binary suite (`tests/binary.rs` via `LoggedCommand`)
and the shell behavior suite (`tests/behaviors/` via
`tests/lib/run_test.sh`) write per-case stdout and stderr to
`tests/output/` on every run. Rust tests produce
`tests/output/<test-name>.log`; shell tests produce
`tests/output/<test-file>/<case>.log`. Failed cases append their
absolute log path to `tests/output/.failed` and print a tail
summary at the end of the run. Set `FLUENT_TESTS_SKIP_LOG=1` to
bypass log writing.

## Active module responsibilities

Several modules own operational policy that would otherwise blur across
the CLI, Work model, and git helpers.

### Project hooks

`hooks.rs` is the project-hook execution surface. Fluent invokes
executable scripts at `.fluent/hooks/<name>` at known lifecycle
events. The naming convention encodes both the action and the
phase: `check-pre-<phase>` are gates (non-zero exit blocks the
phase), `fix-pre-<phase>` are autofixes (run when the matching
`check-pre-<phase>` failed), and `post-<phase>` are notifications
(non-zero exit is logged but does not block). The `<phase>` suffix
aligns with existing Fluent state vocabulary (`land`,
`attempt-failed`, `merge-needs-user`, `write`, `review`).

Each hook receives Fluent context as environment variables
(`FLUENT_HOOK`, `FLUENT_WORK_ITEM_ID`, `FLUENT_ATTEMPT_ID`,
`FLUENT_TASK_ID`, `FLUENT_MERGE_CANDIDATE_ID`,
`FLUENT_CANDIDATE_COMMIT`, `FLUENT_ARTIFACT_DIR`) and runs with
the candidate workspace as its working directory. Stdout and
stderr are captured to `<log_dir>/<hook-name>.log` so failures stay
inspectable after the fact. Hooks that are missing or not
executable are silently skipped — no central registry, no
configuration file, the filesystem is the manifest.

### Tester

`tester.rs` implements the deterministic Tester subcommand. Tester
replaces the LLM-driven BehaviorTests Task with a subcommand that
reads `.fluent/tester.yaml`, runs each declared command sequentially,
invokes `.fluent/extract-tester-results` to normalize per-test
output, and assembles `tester-results.json` — the canonical artifact
all reviewers consume.

Three files form the Tester contract:

- `.fluent/tester.yaml` — project's declaration of the test commands
  Tester runs. Each entry has a `command` (shell string) and a
  `test_harness` (parser identifier for the extractor).
- `.fluent/extract-tester-results` — project's executable that
  normalizes raw command output into a per-test JSON array on stdout.
  Receives the artifact directory as its single argument, reads
  `commands.json` and per-command log files, emits structured results.
- `tester-results.json` — canonical artifact with `commands`, `tests`,
  `summary`, and `error` fields. All five reviewers receive it via
  `input_artifacts`.

These contract files live at the top of `.fluent/`, not under
`.fluent/hooks/`. Top-level `.fluent/` entries are required
schema-bound contracts that Fluent depends on structurally (like
`tester.yaml` and `extract-tester-results`). `.fluent/hooks/` holds
optional auxiliary lifecycle gates that Fluent invokes when present
but does not require.

When either contract file is missing in a candidate workspace, the
writer Task's prompt includes a bootstrap section instructing the
writer to author and commit it. Tester soft-fails when contract files
are missing or malformed, producing a `tester-results.json` with an
`error` field; the Task still completes successfully so the review
loop can surface the problem and the next writer round can fix it.

### Merging

`work_merge_executor.rs` owns the merge policy for Work Merge
Candidates. It calls the `check-pre-merge` hook (if present)
against the candidate worktree and proceeds with merging only
after the hook exits 0. If `check-pre-merge` fails and a
`fix-pre-merge` hook is also present, the executor requires a
clean worktree outside `.fluent/`, runs the fix hook, commits
any changes outside `.fluent/`, reruns reviewers, reruns
`check-pre-merge`, and lands only when the recheck and reviewers
pass.

`worktree.rs` provides git worktree helpers: disabling commit
signing in a worktree, detecting git repositories, and resolving
the common git directory.

### Auto-merge watcher

`auto_merge.rs` is a long-lived polling process that watches Work
Items for merge-ready Merge Candidates and fires
`work_merge_executor::merge_candidate` automatically. Two modes:
single Work Item (`fluent auto-merge <id>`) or all Work Items
(`fluent auto-merge --all`). The watcher polls every 30
seconds (configurable via `--poll-seconds`).

A candidate is merge-ready when the latest Attempt has
`status == complete` and `review_state == passed`, the candidate
has `review_state == passed` and `merge_state.status == pending`,
and `merge_state.auto_merge_skipped` is not `true`.

The `auto_merge_skipped` field on `MergeCandidateMergeState` is an
`Option<bool>` that persists skip state across watcher restarts.
When a merge fails for non-authentication reasons, the watcher sets
this field to `true` so the candidate is not retried. Authentication
failures (401, token expiry) leave the field unset so the watcher
retries after re-authentication.

The watcher is merge-only: it does not invoke attempt progression,
task execution, or any other phase. It respects `--coder` and
`--no-sandbox` flags, resolving them through `CoderKind::resolve`
like other agent-invoking commands. Signal handling uses `ctrlc`
with the `termination` feature to catch SIGINT and SIGTERM.
In-progress merges run to completion before the watcher exits.

### Keep-awake toggle

`keep_awake.rs` prevents macOS idle sleep via a user-controlled
`caffeinate -i` toggle. Four subcommands: `on`, `off`, `status`,
`uninstall`. macOS-only; exits non-zero on other platforms.

Process management uses a wrapper shell script at
`~/.config/fluent/keep-awake-caffeinate` that starts `caffeinate -i`
in the background and handles signal forwarding via `trap`. The
wrapper script path serves as the `pgrep -f` sentinel for process
discovery — no pidfile needed.

A LaunchAgent plist at
`~/Library/LaunchAgents/com.fluent.keep-awake.plist` persists the
toggle state across reboots. `KeepAlive` and `RunAtLoad` are both
`true` when on, both `false` when off. `on` bootstraps the
LaunchAgent via `launchctl bootstrap gui/$UID`; `off` unloads it
via `launchctl bootout` and rewrites the plist with both flags
disabled. `uninstall` removes the plist and wrapper script entirely.

### Git wrapper

`git.rs` is the single entry point for all git subprocess invocations
in Fluent. Every git command passes through `build_command`, which
sets non-interactive defaults so headless agents never block on an
editor, passphrase, or credential prompt:

- Environment: `GIT_EDITOR=true`, `GIT_SEQUENCE_EDITOR=true`,
  `GIT_TERMINAL_PROMPT=0`
- Config overrides: `-c commit.gpgsign=false`, `-c core.editor=true`

Three public functions cover the call-site patterns in the codebase:
`run` (check exit status), `run_stdout` (return trimmed stdout), and
`run_raw` (return raw `Output` for caller inspection). On failure,
`run` and `run_stdout` surface the subcommand, exit code, working
directory, and captured stderr so failures are debuggable without
re-running.

All three public functions retry git lock errors transparently.
When a git invocation exits non-zero with stderr matching a known
lock-error pattern (`could not lock`, `lock failed`,
`: File exists` against a `.lock`/`index`/`HEAD` path, or
`Resource temporarily unavailable` against a `.lock` path), the
wrapper sleeps with exponential backoff and retries the command.
Backoff starts at 20ms and doubles each attempt, capping at 320ms
after the 5th retry, with ±25% random jitter per sleep.
Up to 8 attempts total (~1.5s max wall clock) before bailing.
Successful retries are invisible to callers; budget exhaustion
emits one stderr line and then returns the same error the wrapper
produces for any other non-zero exit.

Constants: `LOCK_RETRY_MAX_ATTEMPTS = 8`,
`LOCK_RETRY_BASE_MS = 20`, `LOCK_RETRY_CAP_MS = 320`.

A regression-guard test (`no_direct_git_command_in_src` in
`tests/binary.rs`) scans `src/` for `Command::new("git")` outside
`src/git.rs` and fails if any are found.

External coder processes (Claude, Codex) run git outside this wrapper.
`worktree::disable_commit_signing` sets persistent repo-level
`commit.gpgsign=false` in candidate worktrees for those processes.

### Cleanup

`cleanup.rs` owns cleanup of terminal Work model state. It selects Work Items only after every Attempt,
Task, and Merge Candidate is terminal, or after an operator explicitly
marks the Work Item abandoned with no executing or reviewing Attempts,
no executing Tasks, no reviewing Merge Candidates, and no executing
Merge Candidate merges. Applying cleanup removes the Work Item metadata
JSON, split Attempt records, split Task records, split Merge Candidate
records, referenced managed Work artifact files or directories, managed
candidate worktrees, and Work task branches. Managed artifact references
must be relative paths made only of normal path components and must
resolve under `.fluent/work/artifacts/`; cleanup ignores absolute paths
and parent escapes. Managed Work worktrees are resolved with the same
expected workspace path rules used by Work task and merge execution, and
registered worktrees are removed through `git worktree remove --force`.
Missing worktree paths and unregistered
directories are reported without deleting arbitrary filesystem paths.
After planning stored Work Item cleanup, cleanup scans the top level of
`.fluent/work/artifacts/` for directories whose names do not match any
stored Work Item JSON under `.fluent/work/items/`. Dry runs report those
orphan Work artifact roots, and `--apply` removes only those top-level
artifact directories. File entries under `.fluent/work/artifacts/` and
artifact roots for stored Work Items are ignored by orphan cleanup.

Cleanup resolves source Fluent state even when invoked from a
worktree by finding the registered worktree that points back to the
current checkout.

### Model selection environment

`coder.rs` owns model-selection environment variables. Claude uses
`FLUENT_CLAUDE_MODEL` first, falls back to `FLUENT_MODEL`, then uses
the built-in default `claude-opus-4-6`. Codex uses
`FLUENT_CODEX_MODEL` when set; otherwise Fluent leaves Codex model
selection to the Codex CLI default. `FLUENT_CODER` selects the default
coder when the CLI does not pass `--coder`.

`FLUENT_CODEX_CA_BUNDLE` is not a model selector, but it lives beside
Codex launch configuration: for sandboxed Codex runs it overrides the
CA bundle path that Fluent sets as `SSL_CERT_FILE`.

## Skills, expertise, and documentation

Three types of content serve different purposes. Procedures live in
`skills/` as step-by-step instructions an agent follows (following the
Agent Skills spec). Reference material for decision-making — principles,
patterns, conventions — lives in `expertise/` at the fluent level and
in `.fluent/expertise/` at the project level. System documentation
(`architecture.md`, `behaviors.md`) describes what IS: structure,
behaviors, and contracts.

Observations captured during usage become Work Items that build or
improve things. Patterns observed across Work Items accumulate as
project expertise in `.fluent/expertise/`.

## Content resolution

`ContentResolver` resolves runtime content that the Fluent binary reads
while executing commands. The implemented runtime content categories are
prompts under `prompts/` and sandbox profiles under `sandbox/`.

Runtime content uses a three-tier search chain. First match wins, no
merging:

1. **Project-local**: `<project>/.fluent/<relative_path>`
2. **User config**: `~/.config/fluent/<relative_path>`
3. **Bundled defaults**: compiled into the binary at build time

For example, a project can override the work-author prompt with
`<project>/.fluent/prompts/work-author.md`, or a user can set a
personal default at `~/.config/fluent/prompts/work-author.md`.

Skills and expertise are outside this resolver boundary. Agents read
skills from the repository or installed skill locations, and read
expertise from `expertise/`, `.fluent/expertise/`, or skill
`references/` directories. Fluent does not currently bundle or resolve
skills and expertise through `ContentResolver`.

## Scheduler

The scheduler drives unattended Work Item execution. It layers a
durable dispatch ledger, an elected per-project coordinator that fills
configured capacity with a worker pool, and usage logging. Later slices
add cost estimation, capacity-aware deferrals, coder switching, and
calibration.

### Usage logging

Each Coder invocation (write and review tasks) appends
per-turn token usage rows to `~/.config/fluent/usage/usage.jsonl`. Rows
follow a fixed JSONL schema: `ts`, `coder`, `work_item_id`, `attempt_id`,
`task_id`, `model`, `input_tokens`, `output_tokens`,
`cached_input_tokens`, and `reasoning_output_tokens` (Codex only, omitted
for Claude).

After appending rows, the system recomputes
`~/.config/fluent/usage/summary.json` with per-coder totals
(`input_tokens + output_tokens` per row) for 5-hour and 7-day sliding
windows. Rows outside the window are excluded from the respective spent
calculations. The summary exists for quick queries; future calibration
slices populate remaining-estimate fields.

Usage logging is best-effort: parse or I/O failures print a warning
to stderr and do not fail the Task.

### Dispatch ledger

Each Work Item owns one dispatch ledger at
`.fluent/work/queue/<work-item-id>.json`. A ledger holds an ordered
list of dispatches (oldest first): terminal history plus at most one
active dispatch, always the last entry. Each dispatch carries
`work_item_id`, `queued_at` (RFC 3339 UTC), `priority` (numeric,
higher = sooner, default 0), `status`, a monotonic `generation` bumped
on every state mutation, an optional `bound_attempt_id`, and a block
reason when blocked. The status vocabulary is `queued`, `claimed`,
`running`, `candidate-ready`, `failed`, `needs-user`, `canceled`, and
`blocked`; `queued`/`claimed`/`running` are active and the rest are
terminal.

A legacy single-entry queue file loads as the ledger's first dispatch,
preserving its priority, queue time, and outcome, mapping the old `done`
status to `candidate-ready`. Migration is lazy and creates no new
Attempt.

`fluent queue add <work-item-id> [--priority N]` creates one `queued`
dispatch for an execution-ready, lifecycle-eligible Work Item that has
no active dispatch, keeping any earlier terminal history. It exits
non-zero for an unknown, proposed, or abandoned Work Item, or one whose
Attempt is suspended at `needs-user` or whose Merge Candidate is pending
land. Repeating `add` while a dispatch is already active preserves its
queue time and changes priority only when an explicit `--priority` is
given.

Automatic promotion enqueues through `ensure_dispatch`, which reconciles
or creates one dispatch without reviving terminal history — a replayed
promotion, materialization, or enqueue reuses the existing disposition
rather than restoring canceled or terminal Work or charging lineage
again.

`fluent queue list` prints each Work Item's active dispatch sorted by
priority descending, then `queued_at` ascending, showing priority,
queue time, execution status, and Work Item id. `fluent queue remove`
records a `canceled` disposition on an unclaimed dispatch — the entry
leaves the active queue but the cancellation survives replayed automatic
promotion. Removal is rejected while a dispatch has a live claim, and
errors when the Work Item has no active dispatch.

Malformed ledger files are skipped with a warning and preserved
on disk for operator inspection.

### Local scheduler coordinator

`fluent scheduler run` elects a single coordinator per project using a
lifetime `flock` lease. A start that finds another live coordinator
reports reuse and returns successfully without claiming Work; when
several starts race, exactly one wins and the rest reuse it.

The elected coordinator runs an in-process worker pool
(`thread::scope`). On each poll it fills free capacity up to the project's
configured limit (`ResolvedSchedulerConfig.max_local_concurrency`,
default 4), claiming eligible dispatches by priority descending and
oldest queue time first. Running Work is never interrupted. Capacity
counts every dispatch claimed or running with a live bound-Attempt lease,
including still-live leases left by an earlier coordinator; Attempts
started directly or on an explicitly selected Fargate runtime do not
count, and reviewer Tasks nested inside a scheduled Attempt occupy one
slot while the reviewer-parallelism limit applies independently.

When it claims a `queued` dispatch, the coordinator durably binds
exactly one Attempt (`bound_attempt_id`) under a generation-checked
update before launching, then transitions the dispatch from `claimed`
to `running`. A `DispatchToken` carries the Work Item, dispatch id,
generation, bound Attempt id, and priority so a launch or reconcile can
confirm it still acts on the dispatch it claimed. Each worker holds a
whole-Attempt lease for the life of the dispatch, and drives execution
via `fluent attempt run <id> <attempt-id> --no-sandbox` (sandbox
disabled for unattended runs).

Before each poll, `recover_and_reconcile` reads model state first and
then applies a generation-checked queue update, so recovery never nests
the queue lock with Work, lineage, candidate, or follow-up locks. A
stale claim whose bound Attempt is nonterminal and not executing resumes
that same Attempt; a stale claim whose bound Attempt became terminal
reconciles the dispatch from that outcome without rerunning it; an
interrupted claim with no bound Attempt returns to `queued` and creates
at most one Attempt on the next claim.

When an Attempt terminates, the coordinator reconciles the dispatch and
releases its claim: a passing Merge Candidate sets `candidate-ready` and
leaves the candidate pending, `failed` and `needs-user` set the matching
state, an unclaimed Work Item that became abandoned is set `canceled`
with no Attempt, and a missing, malformed, or non-executable reference is
set `blocked` with the reason. Other in-flight dispatches are unaffected.
The scheduler never invokes merge logic — landing remains the job of an
explicit land command or the separately authorized `fluent auto-merge
--all`, which runs as a sibling process.

On SIGTERM or SIGINT the coordinator stops claiming new Work, preserves
unclaimed Work as `queued`, and drains live children — letting them
finish and recording their outcomes — before exiting. With no live
Attempts it exits without waiting for the polling interval.

When queued Work exists but no live coordinator can claim it, the Work
stays `queued` and `fluent status` reports that execution is stopped,
naming `fluent scheduler run` as the start or recovery action.
