# Recover a host-evidence finding

Use exactly one recovery path. Attach host-only evidence when the candidate is
correct and a concrete external condition needs another host run. Classify only
a paused candidate-bound recovery when the operator authorizes a source
correction: a new recovery must have an actionable `evidence-needed` review,
while a legacy paused recovery may contain an unqualified marker. For a new
review, a single malformed `Disposition: evidence-needed` marker follows the
ordinary `code-change` Writer path; missing, duplicate, or unknown disposition
markers remain an uncertain pause. The legacy paused-recovery exception keeps
an unqualified marker eligible for its saved recovery. An older Plan-row
recovery may also retain `review_state: not-reviewed` after its failed targeted
Tests Review; Fluent admits that shape only after rechecking its immutable
Plan-row binding and records the failed review before it schedules the
corrective Writer. These paths are not
interchangeable: attachment keeps the candidate waiting, while classification
schedules a Writer on the existing Attempt.

## Contents

- [Recover mutable Writer assignment context](#recover-mutable-writer-assignment-context)
- [Recover a committed Writer with a malformed matrix](#recover-a-committed-writer-with-a-malformed-matrix)
- [Retry an unchanged Writer after invalid completion](#retry-an-unchanged-writer-after-invalid-completion)
- [Recover an output-free failed Writer](#recover-an-output-free-failed-writer)
- [Recover a usage-limit-interrupted Writer](#recover-a-usage-limit-interrupted-writer)
- [Diagnose a reported process leak](#diagnose-a-reported-process-leak)
- [Attach host-only evidence](#attach-host-only-evidence)
- [Attach proof for a carried finding](#attach-proof-for-a-carried-finding)
- [Declare when a command result applies](#declare-when-a-command-result-applies)
- [Run one focused Tests check](#run-one-focused-tests-check)
- [Classify a candidate-bound source correction](#classify-a-candidate-bound-source-correction)
- [Approve responsibility for a saved mixed Plan row](#approve-responsibility-for-a-saved-mixed-plan-row)
- [Recover an interrupted Writer awaiting a host check](#recover-an-interrupted-writer-awaiting-a-host-check)

## Attach host-only evidence

For an exact failed canonical Tester command, use the required-check selector:

```sh
fluent attempt evidence attach <work-item> <attempt> \
  --candidate <exact-commit> --required-check <check-id> \
  --evidence-file <host-result.json>
```

The check ID is `tester:<tester.yaml-sha256>:<zero-based-command-index>`, using
the digest recorded in that Tester's `canonical-run.json` without its `sha256:`
prefix. The evidence must name the exact command and candidate-relative working
directory (`.` for canonical commands). This attachment requires no failed
ordinary Review, failed-command recheck, or new Writer. Resume the Attempt to
run its targeted Tests Review. An attached result grants no pass until review
accepts it; accepted proof credits that occurrence only. Fluent retains the
failed result and reuses other valid results from the complete canonical run.
A diagnostic command cannot replace the required command. Creating this result
still requires the existing authorization to run on its host.

Use this path when the candidate does not need a source change and either a
failed review asks for proof that a trusted host can run or the final candidate
gate asks for one command-shaped `Test:` reference that canonical Tester did not
run. Create one JSON file on that host, for example:

```json
{"schema_version":1,"producer":"release-host","check":"fluent tester check","working_directory":"/repo","result":"pass","run_at":"2026-08-03T17:59:47Z","output":"...captured output..."}
```

This passing snapshot belongs to host-only attachment. Passing evidence
follows the ordinary host-only attachment path and cannot authorize a source
correction.

Inspect `fluent work-item show <work-item-id>` for the current Attempt,
candidate SHA, and failed `review.md` artifact paths. Then attach the exact
proof and each finding it addresses:

```sh
fluent attempt evidence attach <work-item-id> <attempt-id> \
  --candidate <candidate-sha> --evidence-file host-evidence.json \
  --review-artifact .fluent/work/artifacts/.../review.md
```

For a candidate-gate pause, run the exact command in `needs-user.md` against
the unchanged candidate and pass the rejected `candidate-gate.json` path as the
single `--review-artifact`. Set the evidence document's `check` to that exact
command and `working_directory` to `.`. Fluent accepts this only when that gate
has one missing complete command and no unrelated failure.

If Writer completion pauses because a required Plan row is an authorized-host
check, use the `--plan-row plan:step-N` attachment route named in the handoff:

```sh
fluent attempt evidence attach <work-item-id> <attempt-id> \
  --candidate <candidate-sha> --evidence-file host-evidence.json \
  --plan-row plan:step-N
```

The evidence `check` must exactly equal the one command Fluent resolves from the
Plan row. Its `working_directory` must exactly match the stored value shown in
the handoff. For example, for a Plan command `bash tests/host.sh` with
`working_directory` set to `.`, run the check from the candidate checkout root
and record that relative value, not an absolute host checkout path:

```json
{"schema_version":1,"producer":"release-host","check":"bash tests/host.sh","working_directory":".","result":"pass","run_at":"2026-08-03T17:59:47Z","output":"...captured output..."}
```

The row may contain a direct command reference, an inline code command, or one
command embedded in its Deliverable or Verification prose.
Fluent keeps the approved planning text unchanged; if the row contains no
command or more than one command-shaped reference, it remains a human-recovery
pause instead of guessing. Fluent binds the result to the unchanged Writer
candidate and source completion matrix, records an audited binding, and
schedules one targeted Tests Review. Passing evidence can complete that
host-owned row after the targeted Review and continue to the canonical gates;
failing evidence remains a reviewed correction and does not make the row
complete. Retrying the same command is idempotent. This route does not ask the
Writer to create a source commit for host-only work.

Fluent snapshots the exact document, binds it to that candidate, and reruns only
the named reviewers. Do not edit the candidate or invent a commit. Inspect the
Work Item again for the next action.

When ordinary Tests Review follows targeted acceptance, Fluent supplies the
currently applicable proof and targeted decision with their exact source paths.
Its current-acceptance summary names the candidate and the accepted Plan row,
requested check, carried finding, or canonical command occurrence. The historical
Writer matrix remains unchanged, including pending host-owned rows. Reviewers
should read that matrix as the Writer's claim and use the supplied host sources
to assess the separate accepted check.

Fluent rechecks selected proof before direct or queued reviewer launch. If a
source changes, disappears, or expires, inspect the reported binding and current
handoff before supplying matching evidence through the same public attachment
route. A passing targeted decision alone does not complete ordinary reviews,
the final canonical Tester, or Learning.

A command that Fluent could not launch because its declared platform prerequisite
did not match remains unresolved. Repair that environment or submit exact proof
from an authorized host. A command that ran and exited nonzero without a proven
cause needs diagnosis before source correction. Text mentioning Docker or another
dependency does not establish why execution failed. Other declared commands still
finish, and their valid passing results remain available for recovery.

A preserved pre-manifest canonical Tester can use the same attachment command
when its completed Task, exact candidate, full ordered results, and committed
checker files prove a default code check. Fluent saves a separate reconstruction
audit during attachment and preserves all original bytes. An identical retry
continues a partial publication without new review or operation identities.
Missing historical environment, freshness, or comparison proof remains a blocker;
current values cannot supply facts about a past run. Status never publishes a
reconstruction or changes the saved evidence.

Read `fluent work-item show <work-item>` again before continuing. Its
`evidence-recovery` reason and `next-action` describe the current check and
eligible operation. Missing or changed proof can withhold resume; foreground
execution, queue admission, scheduler claim and Review launch each revalidate it.
Repeating an eligible attachment reuses its recovery and Review Task. Repeating
queue resume while its dispatch is active reuses that dispatch. A later stop
requires explicit resume authority; an earlier queued dispatch cannot clear it.
Remove an unclaimed stale dispatch with `fluent queue remove <work-item>` before
issuing the newly authorized `fluent queue resume <work-item> --attempt <attempt>`.

## Attach proof for a carried finding

A carried finding is an unresolved Tests finding retained in the latest Writer's
immutable corrective assignment. After source corrections, its original Review
may still name candidate A while the completed Writer names B. When this is the
only remaining Writer obligation, inspect `fluent work-item show <work-item>` for
the exact finding ID, current candidate, command, and relative working directory.

After the operator separately authorizes and runs that check against B, attach
the schema-version-1 result using its exact command and working directory:

```sh
fluent attempt evidence attach <work-item> <attempt> \
  --candidate <current-candidate-sha> --finding <assigned-finding-id> \
  --evidence-file <host-result.json>
fluent attempt run <work-item> <attempt>
```

Use `--finding` alone among the attachment selectors. Fluent requires a completed
latest Writer, `needs-user/writer-completion`, a clean registered managed candidate,
settled Task and Learner owners, no landing owner or Merge Candidate, and complete
remaining Writer rows. The original assigned Tests Review must request exactly
one `Evidence-Command` and one `Evidence-Working-Directory`; it need not contain
an evidence-targeted disposition marker. Declared applicability still applies.
Status guidance and attachment do not authorize or execute that host command.

The attachment schedules one Tests Review against B. The original Review remains
bound to A, and the original assignment, completion matrices, and Writer output
remain unchanged. Passing proof and a passing targeted Review satisfy only the
selected finding in current completion. Failed proof, a failed Review, or an
uncertain Review grants no completion. Ordinary reviews, final Tester, candidate
gates, and Learning remain required. A targeted source-correction Review creates
a fresh Writer assignment against B. This exception requires explicit selection;
it does not automatically reassign findings or ownership based on prose.

Repeat the same attachment to recover an interrupted publication or reuse a
pending Review. Changed input cannot replace an active recovery. After an
`evidence-needed` Review or expiry of intact live-state proof, attach fresh proof
with the same finding and candidate once execution owners settle. Fluent retains
failed or expired proof and completed Reviews. An old attachment or queued
dispatch cannot clear a later cancellation; use a fresh explicit resume after
settlement. All resume and review routes recheck the binding, and changed saved
proof, source Review, matrix, candidate, or accepted Review bytes block progress.

Older executables reject finding-bound records and transactions. Continue with
the compatible executable; a record upgrade or an absent binding supplies no
finding acceptance.

## Declare when a command result applies

Keep applicability beside its command. A required host Plan row can add an
`Applicability` column containing JSON. An authorized Tests Review can add one
`Evidence-Applicability: <JSON>` line. A `.fluent/tester.yaml` command entry uses:

```yaml
applicability:
  environment: {os: linux}
  comparison_refs: [main]
  freshness: {kind: candidate-code}
```

Host evidence for that command includes the declared observations and the actual
comparison commits used during execution:

```json
{"environment":{"os":"linux"},"comparison_commits":{"main":"<full-commit>"}}
```

These fields supplement the schema-1 evidence document above. Record actual
observations; a current ref value cannot establish what an older run compared.
Fluent records only declared local `os` and `architecture` facts automatically.
Other facts need verifiable host evidence under the existing review rules.

Code checks have no age-only expiry, including code checks executed on an
external host. Checks about current external state must instead declare
`freshness: {kind: live-state, max_age_seconds: 3600}` (the JSON spelling uses
quoted keys and values). Such evidence expires exactly at `run_at` plus that
interval. Missing, unsupported, or expired policies do not imply unlimited
validity. No-change Writer history preserves acceptance only while the exact
candidate, configuration, command, comparison, environment, and freshness still
match. Declaring a policy grants no permission to execute on another host.

## Run one focused Tests check

Use this public path when a failed Tests Review asks for one exact
`Evidence-Command` and no host evidence document exists yet:

```sh
fluent attempt evidence run <work-item-id> <attempt-id> \
  --review-artifact .fluent/work/artifacts/.../review.md
```

This invocation authorizes the exact command and persists one candidate-bound
verification Task. It returns before execution. Follow its printed commands:

```sh
fluent task run <work-item-id> <attempt-id> <verification-task-id>
fluent attempt run <work-item-id> <attempt-id>
```

The worker reads the candidate, writes only private scratch, and keeps bounded
`stdout.log` and `stderr.log` in its Task artifact directory. It exclusively
publishes `receipt.json`, binding the candidate, command, working directory,
checker digest, result, timestamps, exit status, and log digests. The Attempt
loop validates the receipt and uses the ordinary evidence attachment path to
start only the targeted Tests Reviewer. It does not hold a project-wide lock
while the check runs.

If validation fails, inspect the named fixture, receipt, or digest mismatch and
restore the exact authorized inputs or worker-produced bytes before repeating
`fluent attempt run`. A changed candidate or checker requires a fresh
authorization against the current failed review. Do not edit a result to claim
a pass. A canceled verification requires explicit Attempt resume and writes its
retry artifacts separately. Automatic receipt ingestion cannot clear a durable
stop. Validated manual `attach` remains an explicit recovery action for a
settled interrupted Attempt; an invalid attachment leaves it stopped.

Existing schema-version-1 manual evidence documents remain accepted. Historical
Tasks omit the optional verification context; historical Attempts omit the
optional cancellation record. For a legacy review without structured request
lines, also provide its exact `--command` and `--working-directory`. Use `run`
when Fluent must perform the focused check; use `attach` when an authorized host
has already produced the JSON evidence document. Use `classify` only after a
failed snapshot and targeted review establish that the operator authorizes a
source correction.

## Classify a candidate-bound source correction

For a new evidence-targeted review, `evidence-needed` is valid only when the
review names exactly one non-empty `External-Condition` and exactly one
non-empty `Additional-Proof` that would resolve it. A single
`Disposition: evidence-needed` marker with missing, empty, or otherwise
malformed qualification follows the ordinary `code-change` Writer path. Missing,
duplicate, or unknown `Disposition` markers remain an uncertain pause. A legacy
paused recovery with an unqualified marker preserves the only documented
exception to that new-review qualification rule.
When the saved targeted review names the actionable condition above and the
operator authorizes a source correction, classify that correction on the
existing paused Attempt instead of attaching the same proof:

```sh
fluent attempt evidence classify <work-item-id> <attempt-id> \
  --candidate <candidate-sha> \
  --review-artifact .fluent/work/artifacts/.../review.md \
  --evidence-digest <attached-evidence-sha256> \
  --resolution 'The candidate-bound failure requires a source correction.'
```

`--evidence-digest` remains required for every `classify` invocation, including
legacy recovery. Legacy compatibility may derive only the missing persisted
target review digest from the exact bound Review bytes; it never derives the
supplied evidence digest or permits that argument to be omitted.

Source-correction classification requires an immutable candidate-bound evidence
snapshot whose `result` is `fail`. Passing evidence follows the ordinary
host-only attachment path and cannot authorize a source correction.

Use the failed targeted Tests Review path and evidence digest shown by
`fluent work-item show`. Fluent accepts this command only for the saved,
complete actionable `evidence-needed` review or a legacy paused recovery with
the unqualified marker; a new unqualified review is already on the ordinary
Writer path. The candidate, clean worktree, and matching Attempt frontier must
remain unchanged. A new classification must target the current paused
recovery; a later `Reviewing` or `NeedsEvidence` recovery rejects an older
classification and its Writer continuation. Exact replay returns the stored
result and safely reconciles its audit. Exact classification replay is
idempotent only while the saved classified Writer remains resumable. The
supported resumable states are `Planned`, `Executing`, or `NeedsUser` with no
output, and `Complete` with output. A `Failed` or `Superseded` classified Writer
is not replayable. In either state, repeating `classify` is rejected without
mutation. For a `Complete` Writer with output, replay retains the original
source, review, and evidence bindings but validates that saved output's
workspace is the current clean candidate before returning the stored result. For
a `Failed` Writer, classification grants no retry authority. Repeated
`classify` still rejects a failed Writer, including after usage-limit approval.
Only the dedicated `attempt recover-usage-limit` approval followed by a separate
explicit resume can retry that same Task; unrelated failed Writers remain
ineligible. Inspect the failed Task and follow the
[usage-limit recovery procedure](#recover-a-usage-limit-interrupted-writer).
For a `Superseded` Writer, continue with its replacement and do not repeat the
classification.
Fluent records the authorization,
explicit `code-change` disposition, and audit artifact, then schedules one
corrective Writer with the failed review, immutable evidence snapshot, and
audit input; resume it with `fluent queue resume <work-item-id> --attempt
<attempt-id>`. While the classified Writer remains resumable, repeating the
exact command returns the same Writer and audit identifiers without creating
another record; stale or different inputs are rejected without mutation. A
passing targeted review still requires all exact-commit evidence and Learning
before landing.

## Approve responsibility for a saved mixed Plan row

Reading status or running `work-item upgrade` does not adopt ownership. Prepare
an explicit JSON proposal for the original required row. Its parts must join
back to the original State reached text, byte for byte, in order. For an original
row `plan:step-2` saying `Fix source. Check host. Review. Test. Learn.`, use:

```json
{"splits":[{"row_id":"plan:step-2","parts":[
  {"requirement":"Fix source.","responsibility":"writer","completion_from":"plan:step-1"},
  {"requirement":" Check host.","responsibility":"host"},
  {"requirement":" Review.","responsibility":"review"},
  {"requirement":" Test.","responsibility":"tester"},
  {"requirement":" Learn.","responsibility":"learner"}
]}]}
```

`completion_from` is optional. Use it only when that source row already completed
an identical Writer obligation with concrete passing verification. Omit it to
leave the Writer part pending. Host and later-stage parts always start pending.
Every part retains the original source-row link and verification references;
the approval audit preserves the complete original contract and matrix digest.

```sh
fluent attempt ownership preview work-id attempt-1 --proposal-file split.json > preview.json
fluent attempt ownership approve work-id attempt-1 --preview-file preview.json --resolution 'Approve the displayed responsibility split'
```

The first command is read-only. Inspect its exact candidate, source evidence,
and proposed parts before approving. Approval rechecks the current Attempt,
clean candidate, planning inputs, source matrix bytes, and stopped Task owners
under the mutation lock. A stale preview requires a fresh preview and decision.
An identical eligible approval returns its original audit without duplicate
Tasks or evidence. Older readers reject the new record schema before mutation.

Approval assigns responsibility; it does not authorize the host command, retry
a hard-failed Writer, or declare any host check passed. The existing Test-reference
and Approach revision commands retain their separate meanings.

## Recover an interrupted Writer awaiting a host check

After approval, `fluent work-item show work-id` names the exact candidate and
host Plan row. An output-free canceled continuation can yield to that check
only when its Task lease and execution owner have stopped, its input names the
preserved source matrix, all Writer obligations are complete, and the managed
candidate remains clean at the saved commit. Fluent supersedes only that
continuation; earlier completed Tasks and failed evidence remain history.

After the operator authorizes and runs the displayed command, attach its fresh
result and resume the same Attempt:

```sh
fluent attempt evidence attach work-id attempt-1 --candidate <SHA> --plan-row plan:step-2:part-2 --evidence-file host-result.json
fluent queue resume work-id --attempt attempt-1
fluent scheduler run
```

For foreground execution, use `fluent attempt run work-id attempt-1` after
attachment. Resuming before attachment returns the exact host handoff. A failed
check never completes the host obligation. Passing evidence must pass targeted
Tests Review before ordinary review, the canonical final Tester, and Learning
can establish readiness.

An identical attachment reuses its pending targeted review. An identical active
queue resume reuses its dispatch. A foreground retry while its reviewer is live
refuses to advance the same Task. All paths recheck the bound candidate and
evidence; a changed attachment, source matrix, or dirty candidate requires
inspection before recovery can continue. Scheduler claim and Review launch
repeat those checks, including when a queue entry predates the changed evidence.

If live-state proof expires before Fluent consumes its targeted Review, submit
fresh proof after execution owners settle. Use the same candidate and original
selector: `--required-check` for a canonical check, `--plan-row` for a host Plan
row, `--finding` for a carried finding, or `--review-artifact` for a
reviewer-requested check. Status reports that
exact attachment command when the original bindings remain valid. This also
applies when the targeted Review completed before the proof expired.

Fluent preserves the expired attachment and completed Review artifacts,
supersedes any idle review reservation, and creates a new targeted Tests Review.
Repeating the fresh attachment reuses that recovery. An explicit fresh attachment
can resume a settled cancellation; changed sources, dirty candidates, live owners,
or invalid fresh proof cannot replace the pending recovery.

Reviewer-requested checks remain required even when their commands are absent
from the canonical Tester. If completed proof expires before readiness, renew
it through the original `--review-artifact` selector; a later passing Review
does not erase the request. Changed source bindings require repair before Fluent
can accept fresh proof.


## Recover a usage-limit-interrupted Writer

Use this dedicated path for a failed corrective Codex Writer paused at
`round-cap`, with its original assignment, settled ownership, and surviving
final Fluent-captured quota stream. Inspect the Work Item, failed execution,
partial HEAD, index, and unfinished source first. With operator authorization:

```sh
fluent attempt recover-usage-limit <work-item> <attempt> --resolution "<decision>"
```

Fluent reads evidence through the saved Task; there is no arbitrary transcript
file override. It requires the supported complete native terminal sequence and
matching quota errors. A phrase in model text, a provider-session rollout, a
terminal status without settled leases, or an unrelated failure is insufficient.
The surviving stream cannot prove every earlier launch or the origin of every
partial edit. The recorded approval explicitly acknowledges that evidence limit.

Inspect the printed approval ID, partial commit, and immutable audit. Approval
leaves the Attempt paused and launches or queues nothing. The audit binds both
the last completed Writer output and actual partial HEAD, index, working-file
bytes and modes, deleted and untracked source, assignment, saved provider/model/
effort, prior evidence, and execution identity. Do not reset, stash, commit, or
otherwise change partial work to make recovery eligible. A change after approval
invalidates its binding rather than silently broadening permission.

Resume separately:

```sh
fluent attempt run <work-item> <attempt>
# Or request the same recovery through the queue:
fluent queue resume <work-item> --attempt <attempt>
```

Every entry point revalidates before launch. The retry keeps the same Writer Task
and Attempt without another round, preserves the original assignment and failed
execution as history, and writes fresh artifacts. Its sandbox grants no writable
ancestor of retained failure/Review evidence or host approval. Keep sandboxing
enabled and do not switch the saved coder settings to work around capacity.
Provider readiness is a launch prerequisite; it does not promise future capacity.

If the first approved launch pauses at `host-sandbox` before reservation,
inspect the current Work show or status guidance. After the host readiness
problem is repaired, use the same explicit Attempt or queue resume command.
Fluent accepts the existing approval only when its native preparation and
preflight records prove that no launch occurred and all approved inputs still
match. It keeps the same Writer and round. A missing proof, later stop, or
conflicting execution record requires inspection; changing a pause label or
manually resetting the Task does not establish eligibility.

Repeating matching approval or an interrupted publication reuses the same
operation. An already running or finished retry does not start again. A lost
launch acknowledgement or uncertain ownership requires diagnosis; it grants no
second launch. A later cancellation or terminal failure invalidates earlier
restart authority. A new supported quota interruption needs its own explicit
approval; cancellation retains its normal explicit recovery requirements.

Approval provides no verification credit. Complete the Writer's current-candidate
checks and all five ordinary review domains, required host proof and targeted
Tests Review, the canonical final Tester with process-leak rejection, and
Learning. Targeted evidence review supplies no ordinary coverage. A normal
hermetic route test or unavailable sandbox probe does not establish host
confinement. Keep the test workspace outside the private worker temporary tree
and require production Codex `login status` using the prepared launcher and
private home inside the strict profile. Check denied reads of unrelated temporary
and operator-home content. Also require an offline Rust test that compiles and
runs inside the production retry profile using installed Cargo/rustc and the
production launch environment. Confirm that installed tools remain read-only, `CARGO_HOME` uses
the private dependency cache, and temporary tool state stays in private scratch.
Retain positive workspace, fresh-artifact, and Git operations and denied writes
to installed tools, operator state, shared temporary paths, failed execution
evidence, prior reviews, and the approval audit. For support Work declaring the
real-host check, the exact command is:

```sh
FLUENT_USAGE_LIMIT_REQUIRE_REAL_CONFINEMENT=1 cargo nextest run --features test-support --test binary usage_limit_writer_recovery_real_confinement
```

The assigned host runs it against the exact candidate and attaches the captured
result through the Plan-row route above. Failure to run the real probe is a
failure under this command. A Writer must leave that host row pending.

## Diagnose a reported process leak

Start with the original native report and full stdout/stderr for each affected
run. Retain its command, harness version, concurrency settings, working directory,
run time, and affected test identities. Record the candidate commit and any
dirty-source evidence captured at that time; a later checkout cannot establish
the earlier working tree. Keep absent provenance explicitly unknown.

Before another Writer continuation, check that its supplied input paths actually
include those reports and any operator evidence named by the Plan. If they do
not, ask the coordinator for readable preserved copies and leave the diagnosis
row pending. A later canonical result with empty process-leak lists cannot
replace the missing historical report.

For a reproduction, retain the original concurrency and process-leak rejection.
Use the native focused selector, prepare its required pinned readers, and capture
complete native output. Check recorded execution owners, leaders, process groups,
and leases before fixture cleanup. If the report identifies a still-live PID,
an authorized host can capture its process identity, working directory, and
standard descriptors with these bounded commands, substituting that observed PID:

```sh
ps -p <pid> -o pid=,ppid=,pgid=,etime=,stat=,command=
lsof -a -p <pid> -d cwd,0,1,2 -Fn
```

Capture the observation time and command failures too. A reused PID or a process
that has already exited cannot establish the original process's identity; do not
invent missing PIDs or reconstruct an earlier inventory from a later run. If the
Writer cannot inspect processes, hand off the exact observed selectors and the
needed evidence to the host while keeping the diagnosis incomplete.

Close the finding only when retained evidence supports the cause and any source
defect has a verified correction. A clean rerun proves that run's result only.
Focused diagnostics do not replace required host proof, ordinary reviews, or the
complete concurrent final Tester with process-leak rejection enabled.

For the quota-recovery selector, the repository supplies a read-only macOS
observer at `tests/fixtures/collect-quota-process-evidence.py`. Start it while the
focused nextest launch is alive. It follows observed descendants across
reparenting, excludes unrelated processes, and records their process identities
and open descriptors. It does not start tests, signal processes, or clean up
fixtures. For example, from the settled candidate checkout on the authorized host:

```sh
set -eu
: "${CARGO_HOME:?Use the assigned private dependency cache}"
mkdir -p tests/output
quota_evidence=$(mktemp -d "$PWD/tests/output/quota-process-evidence.XXXXXX")
FLUENT_USAGE_LIMIT_OLD_BIN="$(bash tests/fixtures/build-usage-limit-old-reader.sh)"
export FLUENT_USAGE_LIMIT_OLD_BIN
{
  git rev-parse HEAD
  git status --porcelain=v1
  cargo nextest --version
  rustc --version
} > "$quota_evidence/context.txt"
printf '%s\n' 'NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1 cargo nextest run --offline --features test-support --test binary usage_limit_writer_ --no-fail-fast --message-format libtest-json' > "$quota_evidence/command.txt"
(
  set +e
  NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1 cargo nextest run --offline \
    --features test-support --test binary usage_limit_writer_ \
    --no-fail-fast --message-format libtest-json \
    > "$quota_evidence/stdout.jsonl" 2> "$quota_evidence/stderr.log"
  printf '%s\n' "$?" > "$quota_evidence/nextest-exit-status"
) &
quota_run_pid=$!
printf '%s\n' "$quota_run_pid" > "$quota_evidence/launch-pid"
python3 tests/fixtures/collect-quota-process-evidence.py \
  --pid "$quota_run_pid" --seconds 90 --output "$quota_evidence/processes"
```

Preserve the exact launch command with that directory. The observer stops when
all observed identities disappear or its deadline expires; each host probe has
a three-second timeout. It returns 2 for denied or incomplete observations and
for a deadline with processes still observed. On failure, inspect the retained
data and the native launch rather than interpreting the observer status as a
test result. The background launch records its own exit status separately.

An observer exit of 0 describes only its samples: children that exit or reparent
between samples, PID reuse within the start-time clock's one-second resolution,
and descriptor changes between probes remain possible gaps. Correlate timestamps
and pipe identities with the native LEAK event before attributing a cause. The
collector requests each descriptor's access mode as well as its type and pipe
peer; older records without an access-mode field do not establish that mode.
Compare a warned test's stdout/stderr with extra descriptors in sibling test
leaders in the same sample. A sibling can inherit another test's capture pipe
during concurrent runner launches, keeping its output open after it exits.
Before changing Fluent's worker cleanup, test that boundary with a small fixture
that observes its initial descriptors and launches no children. Preserve the
fixture, exact runner identity, native failures and positive controls. Such an
upstream runner diagnosis requires a toolchain repair that preserves concurrent
execution and leak rejection; closing unknown descriptors in Fluent tests or
waiving native LEAK results would conceal the faulty launch boundary. Keep
the entire quota selector concurrent; do not narrow reproduction to only the
first historically affected tests or change the leak timeout to make it pass.
The collector's hermetic regression runs with
`bash tests/behaviors/operations/test-quota-process-evidence.sh`; it does not
constitute host process evidence.

## Recover mutable Writer assignment context

New corrective assignments retain snapshots of host-owned progress and handoff
inputs. An older completed Writer, or its output-free completion retry, may still bind the live file. If permitted
edits make that binding stale, inspect `fluent status` or `fluent work-item show
<work-item>` for the exact candidate, Task, assignment digest, and input path.
These inspections do not publish snapshots or authorize recovery.

Supply a regular file containing the original bytes matching the recorded digest,
using a canonical path with no symlink components. Use the command printed by
status, replacing only the original-file and resolution placeholders:

```sh
fluent attempt recover-assignment-context <work-item> <attempt> \
  --task <assigned-writer> --candidate <exact-commit> \
  --assignment-digest <saved-assignment-digest> --input <assigned-context-path> \
  --original-file <original-bytes-file> --resolution "<decision>"
```

Recovery retains a snapshot and an audited before/after assignment. It preserves
current progress, the current handoff, completed Tasks, findings, and their
original evidence. It does not resume the Attempt or grant completion, host
verification, ordinary reviews, Tester, or Learner credit. Missing original bytes,
changed immutable evidence, stale identities, unsafe paths, and live ownership
remain blockers. Context recovery alone cannot retry a Writer or reset its
completion matrix.

An identical request reuses its operation and snapshot, including after an
interrupted publication. A later cancellation remains in force. If another
mutable input is stale, inspect status again and recover that input using its
newly reported assignment digest. For a completed Writer, separately run `fluent attempt run
<work-item> <attempt>` or `fluent queue resume <work-item> --attempt <attempt>`.
The native lifecycle requires any outstanding host evidence before advancement.

## Recover a committed Writer with a malformed matrix

When a completed Writer committed a candidate but left a malformed completion
matrix, inspect `fluent work-item show <work-item>` or `fluent status` for the
Writer-completion pause and the offered recovery action. Preserve the original
matrix bytes and completed source Task; do not rewrite host-owned rows or replace
the matrix with invented passing evidence. This route requires the immutable
completion contract and retained source transcript, output, assignment, and
matrix. The candidate must still be clean at the recorded commit, all execution
owners must be settled, and the Attempt must remain uncanceled and within its
Writer recovery boundary. A missing matrix, changed source evidence or assignment,
or another pause kind is not this recovery case. A valid matrix qualifies only
for the separate stalled-Writer case below or unfinished no-change work.

After inspecting the saved evidence, authorize the correction with a concrete
resolution, then explicitly resume the same Attempt:

```sh
fluent attempt repair-writer-completion <work-item> <attempt> --resolution "<approved correction>"
fluent attempt run <work-item> <attempt>
```

For scheduler-owned Work, use the queue to resume after authorization:

```sh
fluent attempt repair-writer-completion <work-item> <attempt> --resolution "<approved correction>"
fluent queue resume <work-item> --attempt <attempt>
```

Authorization binds the source matrix bytes by SHA-256 digest; the original
matrix remains a retained source Task artifact. The audit also records the
contract, assignment, candidate, and decision. It schedules a bound corrective
Writer without rerunning or changing the completed source Task; approval alone
does not execute the Writer.
The correction starts with a fresh matrix from the immutable contract and may
submit a verified no-change result if the candidate already satisfies the
requirements. Repeated eligible approval and resume reuse the recorded
recovery and Task identities. A later cancellation or changed candidate,
assignment, contract, or source evidence invalidates stale authority. Host
evidence, targeted Tests Review, ordinary review, final Tester, and Learner
remain required after the correction.

## Recover a stalled committed Writer

When Fluent pauses after two Writer continuations made no completion progress,
inspect the pending Writer rows and the retained Writer history with
`fluent work-item show <work-item>`. Status offers `repair-writer-completion`
only when the latest committed Writer has a valid incomplete matrix and the
saved runs prove unchanged obligations and completion counts across both
continuations. Preserve the matrices, transcripts, source Tasks, and earlier
recovery audits.

Authorize one corrective Writer with a concrete decision:

```sh
fluent attempt repair-writer-completion <work-item> <attempt> --resolution "<approved retry>"
fluent attempt run <work-item> <attempt>
```

For queued Work, use `fluent queue resume <work-item> --attempt <attempt>` after
the repair command. Approval binds the exact clean candidate, contract, source
matrix, assignment, transcript, and output; it schedules but does not execute
the corrective Writer. Repeating the same decision reuses that Task and audit.
Changed evidence, live execution owners, regressions, and unrelated pauses
remain ineligible. Pending external checks retain their separate gates.

## Continue a no-change Writer with unfinished Plan rows

When a completed no-change Writer is paused with pending Writer-owned rows in
its completion matrix, inspect `fluent work-item show <work-item>` and the
source matrix. An explicit decision schedules one corrective Writer on the
same Attempt:

```sh
fluent attempt repair-writer-completion <work-item> <attempt> --resolution "<approved correction>"
fluent attempt run <work-item> <attempt>
```

For a queued Attempt, use `fluent queue resume <work-item> --attempt
<attempt>` after the repair command. The repair records the decision and
source evidence, while resume runs the Writer. A prior approval that reserved
no successor remains historical; use a new, distinct resolution to bind the
current source and create the successor. Repeating that new decision keeps the
same Task. Pending host, review, Tester, and Learner rows do not authorize a
Writer. Once Writer work is complete, attach host evidence through the action
shown by `work-item show` before resuming review.

## Retry an unchanged Writer after invalid completion

An unchanged candidate can pause at its first invalid Writer completion
handoff, or after a previously approved completion retry fails before provider
launch. Inspect `fluent status` or `fluent work-item show <work-item>` and the
saved Task artifacts. If the assignment context is current and Fluent offers
completion repair, authorize that same Task's retry directly, then resume it
separately:

```sh
fluent attempt repair-writer-completion <work-item> <attempt> --resolution "<retry decision>"
fluent attempt run <work-item> <attempt>
```

For scheduler-owned Work, use `fluent queue resume <work-item> --attempt
<attempt>` after approval. The first invalid handoff must belong to a settled
Writer with no output and an unchanged, clean candidate; its saved matrix or
no-change declaration must be invalid. A saved pre-launch failure must have
the earlier completion-recovery assignment and a native execution record
proving that its current generation did not launch. A well-formed receipt from
an earlier generation may remain in the Task area; a receipt for the current
generation or an invalid receipt blocks recovery. Both cases require the same
candidate, valid assignment and retained source evidence, settled owners, and an
uncanceled Attempt. A generic failed Task, changed inputs, or uncertain launch
outcome does not qualify. The approval keeps the original failure files, Task
snapshot, assignment, and accepted evidence, and gives the retry fresh output
files. It does not grant passing credit from the invalid handoff. Repeating the
same approval reuses its Task and cannot override a later stop; the retry still
needs the ordinary host, review, Tester, and Learner gates.

Use `recover-assignment-context` first only when the assignment context itself
is stale and Fluent offers that action. Inspect status again after repairing
that context; context repair alone never authorizes a retry.

## Recover an output-free failed Writer

For an output-free failed Writer whose assignment context is stale, recovery
requires the original saved assignment, its completion-recovery authority, the
same clean candidate and workspace, and a native execution record proving
setup stopped before provider launch. Every owner
must be settled. A later Writer may remain only as an unstarted reservation with
no artifacts. Log messages, a missing candidate, or a quota pause alone do not
prove this case.

After recovering its context, inspect status again. Separately authorize one
retry of that exact Task, then explicitly resume:

```sh
fluent attempt repair-writer-completion <work-item> <attempt> --resolution "<retry decision>"
fluent attempt run <work-item> <attempt>
```

For scheduler-owned Work, use the queue to resume after authorization:

```sh
fluent attempt repair-writer-completion <work-item> <attempt> --resolution "<retry decision>"
fluent queue resume <work-item> --attempt <attempt>
```

Authorization preserves the original assignment and findings, snapshots the
failed Task, and selects a fresh execution artifact directory for the same Task.
It supersedes only an exact unstarted later reservation. Failed Tester evidence,
accepted carried evidence, and the earlier completion audit remain intact. The
retry does not extend round limits or authorize any further retry. Cancellation,
changed evidence, a different candidate, or live ownership invalidates pending
authority. After a started retry stops, inspect its new failure; repeating the old
authorization cannot launch it again. Ordinary reviews, the canonical final
Tester, and Learner still govern completion. The required host confinement check
must run through the exact production profile; a nested sandbox skip is no proof.
