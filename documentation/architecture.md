# Architecture

The [workflow foundation](../tests/formal/workflow/README.md) records the bounded whole-workflow model, first public-command trace, and the pending connections to the detailed scheduler protocols.

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

`fluent skills add --agent <agents> --project|--global` creates selected
agent-facing roots. The agent target chooses where the interactive Fluent skill
is available; it is independent of the Writer and reviewer coder profile saved
by `fluent init`. Each installed bundle records a `.fluent-managed.json`
sidecar with its schema, agent, scope, skill name, complete file inventory, and
content digest. A self-consistent sidecar grants Fluent authority to update that
same identity across releases, even when its earlier digest is unknown to the
running binary. Any malformed or mismatched sidecar, changed file, missing
file, or unlisted file withdraws that authority: Fluent preserves the directory
and reports its path with cleanup guidance. Exact allowlisted pre-sidecar bundles
and sidecar-free Fluent-marked shims remain bounded migration targets. The shim
scan covers existing project `.agents/skills`, `.claude/skills`, and
`.codex/skills` roots plus their supported global counterparts. A bare
`fluent skills add` materializes the fixed handoff copy and updates only those
existing managed installations and Fluent-marked bootstraps; it does not create
otherwise absent roots. An explicit agent and scope operate only on the selected
install targets. When a shim has a matching
`skills` CLI lock entry, Fluent removes only that entry before adoption and
restores it if bundle installation fails. Invalid or foreign lock entries stop
the install before any mutation. This releases the external updater before
Fluent records its own sidecar, preventing a later `npx skills update` from
restoring the bootstrap.
The fixed data copy used by the bootstrap shim is host-owned handoff data, not
a managed agent-facing installation. `fluent init` does not install or refresh
interactive-agent skills, while `fluent update` uses the safe bare refresh after
it replaces the binary.

## Workflow

```
Brief → Behaviors → Approach → Plan → Execute → Review → Learner → Ready Merge Candidate
(interactive)                         (delegated)
                                                                    ↓
                                                              Inspect → Land
                                                                 (human)
```

Interactive stages happen in the agent's session with the user present.
The agent follows skills directly.

After a code-producing Attempt's reviews pass, the Learner runs and records
possible follow-ups for materialization after land. Whether it may capture
expertise is governed by a durable Work Item policy, its Learner mode, chosen
before execution and defaulted to `capture`: a `capture` Learner may refine and
commit durable project expertise, while a `no-expertise` Learner audits the
reviewed change from an isolated snapshot and produces only a handoff, keeping
the reviewed Writer commit exact through Learner for a release gate that requires
one unchanged SHA. Only a successful Learner run makes the pending Merge
Candidate ready. A relaunchable Learner failure leaves the candidate non-ready
and unlandable; `fluent attempt run` retries only the Learner in the same mode.
A Merge Candidate's `created_at` records its pre-Learner identity allocation.
When Learning succeeds, one fresh Work-model mutation stamps its `ready_at` and
the code-producing Attempt's `completed_at` with the same instant. This keeps
crash recovery anchored to the early candidate identity while end-to-end Attempt
timing includes Learning. Legacy candidates omit `ready_at` until a later
readiness check backfills it.
A failure after the coder ran but before its host evidence became durable is
non-relaunchable: status reports
`learner-blocked`, and human evidence recovery is required before advancement.

Delegated execution can run without the user until it produces a ready Merge
Candidate, stops at a Learner failure, or pauses at `needs-user`. The Work model
is that delegated execution path. In the Local Preview, a human inspects and
lands every ready candidate.

Interactive planning has an optional conversation-scoped delegation path. After
the request is grounded, Fluent offers once that the user may explicitly ask it
to use its judgment through the rest of planning. `Keep going`, silence, saved
drafts, and execution autonomy do not activate this path. When active, Fluent
uses repository evidence and stage recommendations to write provisional Brief,
Behaviors, Approach, and Plan artifacts without intermediate approval gates. It
records assumptions, alternatives, and tradeoffs rather than treating its choices
as approved.

The delegation contract keeps human control at two boundaries. A behavior or
scope change, consequential or difficult-to-reverse tradeoff, recorded-decision
conflict, missing input or access, decision outside planning, or exceptional
policy stops for one focused answer. At the last applicable planning artifact,
Fluent presents the complete planning set, proposed Work Items and Learner modes,
release criteria, and decisions it made, then asks once for approval. Revisions
flow into every affected downstream artifact before the set is shown again.
Delegation is not persisted in the Work model and expires at this confirmation.
It does not authorize Work Item creation before confirmation, exceptional scope,
Attempt execution, or landing. If the user revokes delegation sooner, Fluent
returns to the earliest provisional artifact and restores the ordinary ordered
confirmation gates; revocation itself approves nothing.

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
context when it creates write Task instructions.
Callers may repeat `--input-artifact <path>` to approve exact existing files
under `.fluent/work/artifacts/` or `.fluent/work/progress/` as execution
inputs. Intake canonicalizes every source to reject symlink escapes, reads all
sources before changing Work state, and installs byte-for-byte snapshots under
`.fluent/work/artifacts/<work-item-id>/inputs/`. Each stored identity records
the canonical project-relative source, managed snapshot, and `sha256:` digest.
Any validation, snapshot installation, or Work Item storage failure removes
the new snapshot tree and leaves no runnable Work Item. Work Items created
without inputs retain the legacy storage shape.

`fluent attempt create <work-item-id> <attempt-id>` creates the first operational transition
from intake: it appends a planned Attempt with one initial `write` Task.
Attempt creation resolves `terminal.mode` from user and project configuration,
then applies `--terminal-mode headless|native` when supplied. It stores the
resolved mode on the Attempt so a retry or continuation cannot pick up a later
configuration change. Missing legacy fields mean `headless`. Until a native
provider adapter is available, agent Tasks on a native Attempt stop before
launch and report the missing adapter.
The Codex relay records original app-server notifications in JSONL records
marked `fluent-native-agent-event` with schema version 1, execution generation,
conversation and turn IDs, and admitted input sequence. The transcript feed and
usage reader recognize these records alongside existing headless transcripts.
Writer continuation reads the assigned provider conversation ID from native
evidence; a later terminal generation or unrelated notification cannot change it.
The Task declares role `author`, copies explicit Work Item instructions
or derives instructions from Work Item planning context into optional
`Task.instructions`, and declares one writable workspace reference at
`../work-<work-item-id-byte-len>-<work-item-id>-<attempt-id>`.
`fluent task run` creates or reuses that writable workspace as a
sibling git worktree beside the source checkout, runs the coder there,
and normally completes the Task only after the workspace is clean and contains
a new commit produced after Fluent bound the workspace for that Task run. A
follow-up Writer may instead preserve the previous completed Writer commit. It
must leave the workspace clean at that exact commit and write a fresh
`no-change.json` in its Task artifact area with schema version 1, a reason, and
one or more passing command entries. When the correction inputs contain failed
final-Tester command logs, the declaration must additionally bind every attached
candidate gate, structured Tester result, stdout log, and stderr log by exact
project-relative path and SHA-256 digest. The host recomputes those digests and
rejects omissions, extras, duplicates, or stale bytes before any reviewer or
Tester reruns. Other corrective no-change paths retain the schema-version-1
contract without this conditional field. Fluent removes only that declaration
before each coder launch, including retries, so evidence from an earlier launch
cannot authorize completion. Initial Writers and follow-up Writers without valid
fresh evidence still require a new commit. When a successful coder run has
neither a commit nor valid fresh no-change evidence, Fluent pauses the same
Writer Task at the Writer-completion boundary when the workspace is clean, and
writes the validation diagnostic into its needs-user handoff. Explicit recovery
replans that Task instead of converting
the evidence error into a terminal Attempt.

Digest-bound inspection admits the Writer's no-change claim but does not make
that claim independent evidence that the failed command now works. Before such
an unchanged candidate can re-enter review, the Attempt loop plans a serial
Tester Task with role `failed-command-recheck`. Its only input names the prior
host-owned `tester-results.json`, and its dependency names that completed source
Tester. The executor derives the exact nonzero command indexes from that result,
requires the same indexes, commands, and harnesses in the candidate's current
`.fluent/tester.yaml`, and runs only that immutable subset through the ordinary
Tester sandbox. That sandbox grants read-only access to the candidate's common
Git directory, including a linked worktree's external metadata, and appends an
explicit write denial for that directory after its broader candidate grants.
Tester commands can therefore resolve candidate identity without gaining a Git
mutation path. It persists `failed-command-recheck.json`, structured results,
and command logs in the Task artifact area. This Task does not participate in
the parallel review phase and does not seed the landed-baseline cache.

`TaskOutput.commit` names the current candidate for both paths, while
`TaskOutput.base_commit` keeps the commit a no-change Writer verified. Those
identities initially match. A no-change output also stores typed `no_change`
evidence with the declaration's project-relative path, reason, and verification
entries. If capture Learning later adds a canonical expertise commit, the host
keeps `base_commit`, advances `commit`, and stores a typed
`learner_canonicalization` record whose `from_commit` and `to_commit` match those
two fields. Validation rejects unexplained, reversed, partial, or mismatched
divergence and accepts the record only for a capture Attempt with durable Learning
state that proves Fluent accepted the canonical result: `HandoffPending`,
`Succeeded`, or `Failed` with the typed `HandoffPublication` failure stage.
Ordinary coder, transcript-pump, and evidence-persistence failures do not account
for divergence. Existing TaskOutput and AttemptLearning JSON records without the
optional fields keep their legacy shape. New code-producing Attempts stamp an
immutable completion contract from approved behavior statements and their existing
verification markers, Approach choices and execution guidance, and required Plan
rows and Verification cells. Before each Writer launch the host materializes a
bounded task-owned `writer-completion.json` matrix from that contract and an
unchecked `## Required completion` projection of Writer-owned Plan rows. Wrapped
execution-guidance bullets retain every continuation line until the next bullet
or section. Physical line breaks therefore cannot remove an approved constraint
from the matrix.
For each required Plan row, the matrix also carries a bounded structured command
record when one can be identified: the exact invocation, candidate-relative
working directory, leading environment assignments, and the `plan:<step>` source
identity. The row's state and verification prose stay separate. Host evidence
and later Plan checks use this record instead of searching the prose again.
Matrices from older Attempts omit the record and go through one explicit
compatibility parser; an ambiguous legacy row remains paused for correction.
Planning claims from the prior Writer's authoritative completion matrix carry
forward, including approved ownership mappings and retained repair lineage.
The shared resolver validates the approval and its source evidence before using
those claims; original matrices remain history. Open findings from the
current review artifacts
become pending rows identified by stable role-scoped finding identities. The
Writer may fill only completion state, implementation evidence, passing focused
commands, and finding-to-constraint mappings for Writer-owned obligations.
External rows remain pending with empty evidence arrays. Findings may cite the exact
Behavior, Approach, or required Plan row IDs that the host enumerates in the
Writer prompt. It does not edit the host-owned
required-progress projection or choose a second set of test-selector names.

Corrective Writer Tasks also carry a host-owned
`writer_correction_assignment` on the protected Task record. Fluent publishes
the versioned assignment before launch and binds it to the Work Item, Attempt,
Writer Task, source Writer Task, source candidate commit, assigned finding
identities and requirement text, and the source Task and evidence artifacts
with their SHA-256 digests. Completion validation reads those findings from the
assignment, so a corrected candidate's current verification assessment cannot
erase or replace the obligations assigned to that Writer. The assignment is
not current verification: the corrected candidate still needs its own
candidate-bound Tester evidence and all later review, Tester, Learning, and
landing gates.

A Writer continuation created for the same incomplete completion frontier carries
the source Writer's assignment and does not recapture findings from current
verification. A later corrective Writer created from new Review or Tester
evidence captures a fresh assignment bound to that correction Task, source Task,
candidate, findings, and evidence.

For a legacy saved Writer without this field, `recovery_contracts` performs a
read-only reconstruction only for the narrow paused Writer-completion shape.
It verifies the preceding completed source Writer and candidate, the complete
source Tester or other correction evidence, the completion matrix, a clean
unchanged recorded candidate, and settled Task ownership before returning an
assignment. Foreground and queue recovery publish a
`LegacyReconstructed` assignment through the existing Work-model transaction;
status and show remain read-only. Missing historical applicability or accepted
proof, changed evidence, mixed sources, live owners, or contradictory records
remain a specific `needs-user` blocker rather than an empty finding set.

After each completed Writer, the Attempt loop validates the latest Task's matrix
before it creates any Tester or reviewer Task. Incomplete Writer-owned rows or
applicable findings create a continuation only when the no-output, regression,
stagnation, and structural pause rules permit it. Once Writer work is complete,
a required host check pauses with the exact candidate, action, and evidence
handoff. Passing evidence must complete targeted Tests Review before ordinary
review and the separate final Tester and Learner gates. Missing or malformed matrices,
immutable-field edits, unknown constraint references, and pending rows that carry
completion claims pause for repair. Once Writer-owned Plan completion is valid,
the host replaces only the required-completion section with exact Writer-owned
requirement text and evidence links to the corresponding matrix rows. It preserves
separate progress sections and verifies its own projection before planning review.
A disagreement between the Plan and matrix contracts pauses rather than selecting
one authority. The valid matrix is attached to every reviewer as an advisory
coverage map; reviewers still inspect the candidate, and the final Tester remains
the authoritative complete-suite gate. Attempts created before the matrix contract
retain their Writer-owned required-progress gate and review path. Fluent records
each host-derived Writer outcome, candidate commit, progress and matrix completion
counts, provider session identity, and whether the run was initial, pre-review
continuation, or post-review corrective work.

When a completed Writer committed a candidate but left an unreadable completion
matrix, status offers `fluent attempt repair-writer-completion <work-item-id>
<attempt-id> --resolution "<decision>"` while the candidate is clean and Task
owners are settled. The command binds the source matrix bytes by SHA-256 digest
in its audit; the original matrix remains a retained source Task artifact. It
records the requirement contract, assignment, candidate, and explicit decision,
then schedules one corrective Writer in the same Attempt. The completed source
Task and its artifacts
remain history. Run `fluent queue resume <work-item-id> --attempt <attempt-id>`
separately to execute the correction. The new Writer receives the failed matrix
as an input and starts a fresh matrix from the immutable contract; it may submit
a verified no-change result when the source candidate already satisfies the
requirements. Host evidence, targeted Tests Review, ordinary review, final
Tester, and Learner remain separate gates. Repeated authorization reuses the
recorded Task and operation; changed source evidence, contract, assignment,
candidate, or ownership rejects stale authority.

The same action can recover a committed Writer stopped by the repeated-progress
guard. Admission requires a parseable matrix with intact immutable rows,
valid completed claims, and pending Writer-owned work. Partial claims on
pending rows are ignored only for eligibility; the original matrix is retained.
Saved runs must prove two continuations with unchanged obligations and
completion counts. The latest run must be blocked and match the completed
source Task and candidate. An earlier continuation may be a verified no-change
run when its declaration and unchanged commit match the saved Writer chain.
Recovery retains the stop rule and all source evidence,
binds the matrix, contract, assignment, transcript, and output to the decision,
and schedules one corrective Writer through the existing recovery path. A
regressed matrix or an unrelated completion pause does not grant this retry.

The same public command repairs an unchanged Writer whose completion or
no-change handoff contains an invalid result, including a saved failed retry
that stopped during preparation before its provider launched. Approval retains
the original failure files and Task snapshot, binds their bytes and the
host-owned write baseline, and gives the same Task a fresh artifact directory.
The current candidate comes from the latest completed Writer and clean managed
worktree; a carried correction assignment keeps its earlier source candidate
and finding lineage. Accepted claims in a prior valid Writer matrix may carry
into the fresh matrix, but invalid result text grants no credit. Run `fluent
attempt run <work-item-id> <attempt-id>` or `fluent queue resume <work-item-id>
--attempt <attempt-id>` after approval. Resume, direct Task admission,
reservation, and provider spawn recheck the bound evidence and allow one launch.
A later cancellation or ambiguous launch record blocks reuse. The retry does
not satisfy host checks, reviews, the final Tester, or Learning.

`ordinary_review_tasks` in `recovery_contracts::review_coverage` establishes
ordinary coverage from completed Review Tasks, their candidate contexts, and
their saved verdicts. Attempt advancement and completed Approach-recovery resume
share this read-only selection. A later ordinary verdict replaces the earlier
verdict for its domain while preserving both reports. Completed recovery checks
all ordinary domains and separately revalidates accepted evidence through the
Plan and canonical verification contracts.
Targeted evidence reviews retain their narrow scope and supply no ordinary
coverage. Before final Tester or Learner, all five built-in domains need passing
ordinary coverage. A missing role is scheduled even when the only preceding
reviews were targeted host checks.

At each completed Writer, the host compares its candidate with the immediately
preceding completed Writer commit. The Git name-status delta includes both sides
of copies and renames. Project-owned path domains map that delta to reviewers;
only ordinary passes with valid contexts and unchanged domains survive every
intervening delta. A later revert does not restore an invalidated pass. Unknown
or unsafe paths, ambiguous prior review contexts, unreadable commits, invalid
workspaces, or unclassifiable deltas invalidate every reviewer. The next review
round includes failed, changed, and missing ordinary roles in built-in order.

A replacement review must name its actual Writer candidate. A mismatched current
review reopens that role, and a matching replacement supersedes it without
changing historical Tasks. Saved resumes apply the same coverage check before
running a planned final Tester or entering Learning, retracting an unready
pending Merge Candidate through the existing correction transition when needed.
Accepted Learning remains intact on repeated resume. Capture Learner changes
under `.fluent/expertise/` are excluded from pre-Learner domain invalidation: the
Learner mutation boundary restricts those changes, and the deterministic
candidate gate validates the resulting expertise commit before handoff
publication.

Writers with a persisted base commit also cross a deterministic admission gate
before matrix reconciliation or review. The host verifies the exact candidate
and clean workspace, runs `git diff --check`, rejects merge commits, invalid
identities, and empty subjects, and resolves approved behavior references
against committed test files and native test identifiers. Commit-body presence,
line wrapping, literal `\n` text, attribution trailers, and other repository
style conventions remain project-owned and may be enforced by
`check-pre-merge`. Factory's hook reports missing bodies, long body lines, and
literal `\n` text as advice. Prohibited attribution and other project-check
failures remain blocking. Literal `\n` advice alone sends the same candidate
directly to review without a corrective Writer, rewrite authorization, or pause.
Fluent stores the results under the Attempt's `candidate-gates/` artifacts,
separate from the Writer's private artifact directory. When the gate
first rejects a candidate, Fluent adds its canonical reference exactly once to
the Attempt artifact index before it creates a corrective Writer with the same
reference as an input. A first candidate defect becomes the sole gate input to
that corrective Writer. Failed check diagnostics are also promoted into the
Writer's authoritative current findings, so the correction does not depend on
enumerating the gate artifact's parent directory or retaining an unbounded path
in the context index. A repeated rejection pauses. Explicit recovery preserves
the Attempt, workspace, and provider session while admitting one correction at
a time. It also narrowly repairs an older binary's terminal failed-correction
shape by replanning only the same output-free Writer whose attached gate was
produced by an earlier completed Writer; unrelated failed Attempts stay
terminal. Unreadable Git or Tester evidence and a mutating check hook pause
immediately.

Ordinary foreground and queued resume also include a narrow compatibility
transition for Attempts that Factory's retired presentation checks paused. The
transition accepts only the exact preceding clean candidate and a stopped,
output-free corrective Writer. Its source-bound candidate gate and canonical
hook log must contain only the former missing-body or body-line-length
findings, and the Writer's complete input set must equal the Work Item's
preserved inputs plus that gate. The exact gate input on the correction Task is
the legacy provenance record. Current Attempts also index that reference in the
Attempt aggregate; the compatibility path accepts an older first-rejection
projection where this duplicate aggregate entry is absent. During the audited
reclassification mutation, Fluent adds the missing aggregate reference together
with the receipt and correction-Task supersession. It preserves the failed gate
and log, and the separate receipt binds the gate digest, candidate tree, and
project-policy commit before Fluent reuses the candidate and continues to
review. Live or
output-producing Writers, dirty or changed candidates, extra or missing
Writer inputs, duplicate gates, noncanonical or reassigned paths, ambiguous
aggregate entries, changed evidence, and mixed, provenance, or unknown findings
fail closed. Because the candidate's historical policy can omit a finding,
recovery also scans every candidate commit independently and rejects any
prohibited attribution trailer that is absent from the stored hook log.

Recovery replay validates the receipt's recorded policy commit separately from
the current policy marker and hook. The recorded commit must remain an ancestor
of the current project branch, and the current checkout must still expose the
advisory marker and invoke the same hook. This lets an unrelated descendant
project commit retain a completed transition, including a receipt written
before the Work-model mutation was bound, without weakening the exact
classification of legacy evidence. Re-entry then completes or reuses the same
candidate transition instead of rewriting candidate history or duplicating
Tasks.

The metadata-recovery parsers and receipts still recognize literal `\n`
failures persisted by older Fluent versions. These historical artifacts use
the existing exact compatibility classifier; the active candidate gate never
produces a literal-newline failure. New literal `\n` advice cannot authorize
metadata recovery or corrective work.

A metadata recovery is eligible when `commit-metadata` is the only failed
check, or when `project-check` is the one additional failure and is derived
from those findings. In the derived form, the gate entry must record exactly
one nonzero `check-pre-merge exited N` diagnostic. Its canonical
`hooks/check-pre-merge.log` must be non-empty and contain only nonblank
hook-finding lines that normalize to the gate's commit-metadata findings,
including the exact body-line length finding for a commit whose historical
metadata finding is literal-line-breaks. The exit diagnostic belongs to the
gate entry; the non-empty hook log supplies the findings. The protected base must remain an
ancestor.

An operator authorizes this recovery with:

```text
fluent attempt repair-candidate-metadata <work-item-id> <attempt-id> --resolution "<approved rewrite>"
```

The transition records the exact gate, rejected candidate, derived hook
evidence, protected base, candidate tree, and human decision before scheduling
one corrective Writer. After authorization, run this exact command:

```text
fluent queue resume <work-item-id> --attempt <attempt-id>
```

This resumes the same Attempt and continues through the ordinary candidate
gate, Tester, review, and Learner lifecycle.

The same command also covers a narrower Writer-completion case. If the latest
Writer has a verified no-change result and its completion matrix has exactly
one pending Documentation finding for a historical commit-body line-length
defect, Fluent binds the recovery to that review, the current candidate, and
the Writer's passing gate. The operator can authorize the same
`repair-candidate-metadata` command even when the ordinary Writer-round budget
is exhausted. Fluent records the review digest and finding identity, derives
the historical commit's protected parent from Git, and schedules one metadata
repair Writer with the review and gate artifacts as inputs. A changed review,
candidate, worktree, or finding is rejected before any state is changed.

For historical evidence, a generic `body line N exceeds 72 characters` finding
must resolve to a gate commit, and the exact overlong commit message line must
contain literal `\n` text. A finding for an uncovered commit, an ordinary
overlong line, any additional non-exit project-check diagnostic, diagnostic present only in the
gate structure, unrelated or unparseable hook line, noncanonical path, or
symlinked log remains rejected before Fluent writes recovery authority.

A corrective gate reads the preceding host-owned gate and checks the complete
replacement from the original base; it never treats the rejected candidate as a
new trusted base. Corrective history remains append-only except for one bounded
case: when commit metadata was the preceding gate's only failure, or when
`project-check` was the one additional failure and its canonical stored hook
log is non-empty and contains only matching metadata findings, including
`commit <id> body line <positive integer> exceeds 72 characters` for a historical
literal-line-breaks finding, while its gate entry records exactly one nonzero
hook exit. The Writer may rewrite those commits only after the host
revalidates the prior gate, proves that both candidates descend from its
original base, and requires identical Git trees, runs every gate check again
over the replacement, and records both commit identities, the shared tree, and
the prior artifact path. A project-check-only failure is never eligible. A
generic overlong line must prove the corresponding literal `\n` text in the
commit message; a changed tree, another prior failure, changed author identity,
changed linear
suffix shape, malformed or unproven hook evidence, or an ordinary non-ancestor
candidate blocks before review. Committer timestamps may change during the
authorized rewrite, but the protected suffix's per-commit trees and authors
remain fixed.

Before that Writer launches, Fluent writes the immediately preceding candidate
and a host-owned lineage mode into the generated execution context and renders
both into the correction prompt. The default mode is `append-only`: removing
unwanted content still requires a new correction commit, and the preceding
candidate must remain an ancestor. Fluent renders
`tree-identical-metadata-rewrite` when the latest validated rejected gate that
names the current candidate has exactly the commit-metadata failure, or exactly
that failure plus a project check proven by its canonical stored hook log,
matching the same gate-covered commit and its required body-line finding;
older gate artifacts remain readable history but cannot override that current
frontier. The mode tells the Writer to change metadata without changing the tree
and supplies the exact protected base that must remain an ancestor. These
instructions reduce avoidable correction failures; the deterministic candidate
gate remains the enforcement boundary and independently verifies the resulting
history.

If the Writer nevertheless replaces that protected base, and this is the only
blocking check, the gate offers the public `fluent attempt
repair-candidate-ancestry` transition. The host revalidates the current gate,
its prior metadata-only gate, the clean registered worktree, both candidate
trees, and the protected ancestry. It preserves the rejected replacement under
a recovery ref, records durable recovery intent, then creates one host-owned
Writer result whose unchanged tree has the protected base as its sole parent
and whose message comes from the already metadata-valid source candidate.
The intent record makes retries safe before or after the branch advances. The
same Attempt is rebound through an exact `queue resume` dispatch and then
returns through the ordinary candidate gate and review lifecycle; ordinary
`queue add` is never used because it can allocate a fresh Attempt. If older
guidance already admitted one later Attempt, the public repair retry supersedes
it only after local cancellation has left one interrupted, output-free initial
Writer. A durable receipt preserves that admission as history and authorizes
the earlier repaired Attempt; any preexisting output, additional task, or
different pause fails closed. Unrelated candidate drift remains ineligible.
If the interrupted Writer settles after its audited supersession, its task,
output commit, and branch remain durable history. A later repair can carry that
supersession forward only by validating the earlier receipt and exact
single-task admission; the late output never becomes the recovered candidate.
If a later Writer again rewrites history while making no tree change, Fluent
accepts a second repair only when the new protected base descends from the
earlier recovered commit and every other deterministic gate check passed. The
new durable repair preserves that substantive base as its parent. Writer and
review prompts identify the latest protected base as immutable: product content
remains reviewable, but commit-metadata findings apply only after that boundary.
An attached review, Tester, or candidate-gate correction artifact selects this
route even when the preceding Writer run was recorded as a pre-review
continuation; corrective evidence takes precedence over that older outcome.
The same artifact classification records the resulting Writer run as
corrective for status and scope-calibration metrics. Progress and
completion-matrix evidence alone continue to identify a pre-review
continuation.

The explicit `repair-candidate-ancestry --restore-interrupted` mode lives in
`candidate_ancestry_recovery::interrupted`. It restores the latest completed
Writer's exact commit after a source-identical rewrite removed its ancestry.
Eligibility requires a current Interrupted Attempt with active settled
cancellation, one final output-free Writer, its immutable correction assignment,
and a validated completion-recovery receipt naming that source. Failed historical
reviews remain failed. The source and candidate branches must be distinct local
branches in the same repository, and the candidate worktree must be registered
and clean. Ordinary append-only candidates are ineligible.

The assignment's original source can precede the candidate being restored. Each
continuation must carry its preceding completed Writer's exact assignment apart
from the receiving Task identity and proven freezing of recognized legacy
context. `recovery_contracts` validates that chain, including the original input
digest, exact snapshot and provenance paths and bytes, and any operation-bound
context-recovery audit. A retained snapshot can prove the predecessor's original
mutable context after live progress changes; immutable inputs and finding
evidence still require their original bytes. The owner validates the original
source independently of the receipt's latest completed Writer binding.
Restoration never rewrites immutable assignments to match its target.

The owner holds the repository land lock, Task and execution identity leases,
Learner and dispatch leases, and the fresh WorkModelStore aggregate lock. Lease
paths and Task artifact areas must retain their managed identity without aliases.
An immutable schema-2 intent binds both commits, the tree, protected base, branch
refs, canonical workspace and repository, original aggregate digest, exact
evidence paths and bytes, and the operator's resolution. This aggregate digest
also binds cancellation, source outputs, assignments, and every Task value.
The intent records the original assignment source separately from the restoration
source. Existing schema-1 intents retain exact replay for their original narrower
shape, in which those sources were identical; replay preserves their bytes.
`publish_recovery_snapshot` durably writes that bounded intent before Git effects.
The owner retains the rejected commit under its exact recovery ref before using
a compare-and-swap to replace the candidate branch.

A prepared Git transaction verifies and holds the worktree's symbolic `HEAD`;
a separate prepared transaction in the common repository locks the source,
retained, and candidate refs with `no-deref` and expected commit IDs. While those
locks are held, the owner checks that each ref remains direct before committing
the candidate update. `no-deref` alone would still permit replacing a symbolic
ref itself. A substituted symbolic ref therefore aborts restoration without
changing it or its referent, and no recovery operation is published. Git aborts
uncommitted transactions when their input closes, including after a coordinator
exit; normal cleanup also reaps each child. This requires Git's `symref-verify`
capability, checked before intent publication.
The owner rereads protected evidence before the candidate update, then publishes
only an audit reference and recovery operation through the aggregate transaction.
Retries normalize only that operation and audit when comparing the original
aggregate. An identical intent reconciles interrupted publication or returns the
same operation; any intervening binding change blocks it. Restoration leaves
Tasks, assignments, completion matrices, progress, reviews, and cancellation
unchanged. A separate explicit resume retries the same unfinished Writer through
ordinary execution and candidate gates.

The Writer-phase gate invokes the project's existing `check-pre-merge` hook from
a temporary shared clone checked out at the candidate commit with detached
HEAD. The hook can inspect committed files and the commit range, but cannot
depend on the live Writer workspace path, branch name, uncommitted files, or
changes that persist after the check. Fluent checks the live candidate's HEAD
and worktree status separately after assessment, then publishes that assessed
gate result at final handoff. This keeps formatting and architecture rules project-owned;
for example, this repository's hook rejects the dashboard `app` importing
`render`, which would recreate the forbidden `app -> render -> app` dependency.
The gate snapshots the detached checkout's HEAD and worktree status around the
hook, so a check may report but never repair. Other hook phases retain their
own execution contexts. The later landing check remains an independent final
safeguard.

After the final Tester, the host runs a final-Tester candidate gate before it
creates a Merge Candidate or launches the Learner. The gate matches each `Test:`
reference to its passing structured result. `verification_reference.rs` parses
declared meanings and adapts supported native identities. Rust
identities retain any supplied source path, binary, and module qualification.
Supported forms are bare or qualified names, `path.rs (name)`, `path.rs::name`,
and nextest's `binary$name` or `binary name`, including qualified names inside
source wrappers. Nextest binary IDs use a separate grammar: `package`,
`package::target`, or `package::kind/target`, such as `my-crate::bin/tool`.
The gate preserves the complete binary ID, so failed or skipped binary targets
still participate in ambiguity checks. A bare name can match a qualified name
with the same final
identifier; two qualified names must agree in full, as must any paths or
binaries supplied on both sides. The cheap pre-review source check looks for
the function identifier because Rust declares modules separately. The final
evidence check retains the full reference; it does not rewrite references or
Tester artifacts or maintain an alias registry.

Python references support `path.py::Class.test_method` and
`path.py (Class.test_method)`, as well as a short `test_method` selector.
Selector components use ASCII identifier syntax. The pre-review gate scans
committed text for a direct `def` or `async def` in the named top-level class.
It masks comments and strings, respects indentation, and handles decorated and
multiline declarations. It does not resolve inherited or dynamically assigned
methods, nested or conditional class definitions, or duplicate declarations of
the referenced class or method. This check never imports or executes candidate
Python; the final Tester must still report passing evidence.

Python evidence accepts `Class.test_method`, `module.Class.test_method`,
`test_method (module.Class)`, and `test_method (module.Class.test_method)`,
plus `path.py::Class.test_method` and `path.py (Class.test_method)` wrappers.
Short method identities remain supported when unambiguous. The gate retains
every supplied path, module, and class qualifier, so a result from another
supplied class cannot satisfy the reference through its method name alone.

For legacy `.sh` references, the exact
committed script path identifies the test at both gates. A parenthetical such as
`tests/lifecycle.sh (help documents drain lifecycle)` describes coverage; it
need not appear in source or result text. The structured result may use a
project-specific harness display label, but must still name the exact path and
pass. Applicable reviewers assess whether the script covers the stated behavior.

Explicit `Test:` objects distinguish whole-script, named-test, and command
requirements. `whole-script` requires `path`; `named-test` requires `path` and
native `name`, with an optional Rust `binary`; `command` requires the exact
`command`, with an optional zero-based `command_index`. Every kind accepts
non-identity `description` metadata. Unknown fields, malformed values, and
contradictory qualifiers or recognized identities from another native format
cannot establish a pass. Unqualified native names and free-form display labels
remain supported when they do not contradict the structured identity. A named
shell result must use the exact `path::name`; a whole-script pass cannot satisfy
it. An explicit command must resolve to a committed canonical entry, and duplicate
command text requires an index. Configured legacy command references use the same occurrence,
applicability, and comparison checks. Reviewed legacy proof may satisfy a command
outside the canonical configuration only when the current run proves that it is
not a configured command. See the [planning examples](../skills/fluent.full/references/define-behaviors.md#rules).

`verification_assessment.rs` evaluates references and canonical checks below the
gate, recovery, executor, and status consumers. It reuses `verification_policy`
and `check_applicability`, with no calls to orchestration or execution modules.
Each reference ID binds the Work Item, Attempt, approved requirement row,
reference occurrence, and original-text digest. Diagnostic wording and ordering
do not create obligations. The read-only result separates satisfaction, cause,
owner, next action, and source bindings. Status uses nonmutating snapshots and
bounded managed-file reads. An assessment records what was valid at its explicit
evaluation time; execution boundaries must revalidate current applicability.
A behavior with neither a `Test:` reference nor an `Untestable:` reason retains
one unsatisfied row keyed to its approved requirement. Its missing meaning needs
diagnosis before verification can proceed, including before the first Tester.
Before a changed candidate has its first Tester, Writer requirements, context,
and status use its pending assessment and retain earlier Tester failures only
as history.
The shared assessment also attributes suite failures to the retained pre-write
baseline. Matching pre-existing failures do not become Writer obligations; an
unrelated review or newly introduced failure retains its correction route.
Baseline attribution never substitutes for passing explicit-reference evidence.
Returned integrity and binding blockers prevent final advancement even when
native results pass, or the Attempt has no completion contract. Pending reviews
and testing may still run to collect evidence. Diagnostic handoffs identify the
candidate, Tester task, expected result path, specific error, and recorded logs.

The gate builds its Rust and Python evidence views before selecting a passing result.
Failed and skipped tests still count when resolving a short name. Matching
representations must agree on supplied qualifiers and all report a pass;
an underspecified result cannot bridge conflicting paths, binaries, modules, or classes.
An explicit reference can distinguish otherwise colliding identities. Missing,
unrelated, ambiguous, or non-passing evidence leaves the reference unsatisfied,
and stale or unbound evidence blocks the gate. Diagnostics retain the unresolved
path and selector and show relevant observed identities with their statuses.
These checks consume the canonical final Tester output; they add no test run
before review. Required coverage, applicable reviews, the complete final Tester,
and a successful Learner remain mandatory.

Missing or uninterpretable evidence pauses for diagnosis and verification
recovery. Only established unresolved candidate defects authorize a corrective
Writer, including when reopening a saved gate. When the only missing
evidence is one complete command-shaped `Test:` reference that did not run in
canonical Tester, Fluent pauses before another Writer. It binds
host-run evidence to the exact rejected gate and candidate, then asks a targeted
Tests Reviewer to accept or reject that evidence. A passing review credits only
the exact command when the candidate gate runs again. Multiple missing commands,
another failing gate check, candidate drift, and selector-only gaps remain
ineligible for this route. When missing evidence
coincides with failed Tester commands, the gate records every failed command,
its exit code, and its durable stdout and stderr paths so diagnosis can establish
whether a candidate change is needed. An authorized correction Task
attaches the gate, the exact structured Tester result, and only the stdout and
stderr files for nonzero commands. Those files receive individual read-only
sandbox grants; their parent directories and sibling Tester artifacts remain
unreadable. Fluent accepts only the Tester's deterministic `commands/<index>`
log names, so persisted result content cannot choose a broader sandbox path. The
corrective prompt names every current artifact with its untruncated path and
requires the Writer to inspect the failed logs and candidate-owned
`.fluent/tester.yaml` before treating a harness, path, or cache failure as
external. If the Writer concludes that no candidate change is needed, its fresh
declaration must prove access to every attached final-Tester evidence file with
the current SHA-256 digest; the host validates that exact set before it can
launch another review wave. A legacy recovery recognizes only a generic Learner
failure whose retained gate proves this exact
frozen-evidence gap; it retracts the pending candidate that never passed Learner
advancement and rechecks the cause in the same Attempt. A gap alone pauses for
diagnosis without adding a Writer. Other Learner
failures retain their existing recovery disposition.

A capture Learner runs a separate gate after canonicalizing expertise but before
publishing its handoff. That gate checks the Learner-owned delta while reusing
the already-admitted final Tester evidence. Rejection rolls the Learner
transaction back to the reviewed Writer
commit and leaves Learning failed and retryable. A no-expertise Learner checks the
unchanged reviewed candidate before it publishes its handoff.

Codex
Writers omit `--ephemeral` and subsequent pre-review or reviewed-correction
rounds resume with `codex exec resume <thread-id>`;
because that subcommand does not accept `exec`'s `--cd` option, Fluent sets the
subprocess current directory to the candidate workspace instead. Claude Writers
run with an Attempt-scoped managed `HOME`; their sandbox denies the operator's
persistent `.claude/projects` tree, so session and memory writes remain inside
the Work artifact boundary. They resume with `--resume <session-id>`. A missing
or provider-mismatched identity
starts a fresh persistent session. Codex Writer Tasks copy only their generated
`sessions/` rollout tree into the Task artifact after each launch and restore it
into the next launch's temporary worker home. Authentication, configuration,
hooks, and cache are not included. Older Attempts without that snapshot fall
back to a fresh persistent session rather than attempting an unusable resume.

Every autonomous Writer and final Tester receives one Fluent-owned Cargo
dependency cache under
`.fluent/work/cache/writers/<work-item-id>/<attempt-id>/`. Its private
`cargo-home/` is shared by initial, continuation, and corrective Writer Tasks
and the final Tester in that exact Attempt, but never by another Attempt. Cargo
build output remains in the candidate workspace so later Writer rounds, the
final Tester, and the candidate-keyed reviewer cache retain the existing
warm-output path.
Before the first launch, the host admits only `registry/` and `git/` from its
canonical Cargo home, using copy-on-write or a deep copy so private writes can
never mutate the source. Fluent does not copy Cargo credentials, configuration,
or binaries, and Writer and Tester sandboxes grant the private cache without
granting the operator's Cargo home. The cache is rebuildable execution state
rather than a durable artifact. Existing managed-path components and copied dependency
symlinks are checked for escapes, and cache reclamation preserves only
Work Item/Attempt pairs still referenced by nonterminal Work.

Pre-review continuation and reviewed-correction prompts contain only the paths
needed for current work, including a generated `execution-context.json`. The
versioned context records candidate/base commits, changed files, unresolved
steps, current findings deduplicated by role and stable identity, passing Tester
evidence, executed commands, and historical artifact paths. When verification
is available, new contexts use schema 3: `verification` contains unresolved
reference and command rows, counts of satisfied rows, and a path and SHA-256
digest for the complete `verification-assessment.json` beside the context.
The complete assessment retains passing rows for audit. Saved schema-2 contexts
with inline verification rows remain readable; rewriting one preserves its
schema and verification shape. Contexts without verification use schema 1.
The context's collections and serialized size are bounded. Fluent may omit
advisory commands, evidence summaries, or changed-file rows and records those
counts, but it never silently omits an unresolved step, current finding,
verification failure, or input-artifact path. Fluent
preserves historical artifact paths exactly, including paths longer than the
240-byte prose truncation threshold, because they identify evidence. If the
authoritative core cannot fit within the 128 KiB context limit, Fluent rejects
it before writing `execution-context.json` or launching the Writer.
Reviewed-correction prompts are capped at 32 KiB. Full historical artifacts
remain unchanged for audit and are opened by path only when needed. Review
prompts use the same generated context instead of reinjecting prior review
bodies. Corrective Writers save complete command output under their task
artifact's `commands/` directory and inspect concise summaries or relevant
failure excerpts in the model conversation.

The generated context includes the shared current verification assessment.
Canonical checks retain their configuration digest and command occurrence;
native failures retain their qualified identity. The Writer matrix receives
only unresolved candidate defects from that assessment. Missing or unknown
historical proof stays blocking diagnostic context, and applicable accepted host
proof keeps its external owner. Diagnostic lines and historical command failures
do not each become a new Writer row. After the Writer returns a corrected candidate, the tests reviewer alone
receives that structured result and its failed-command logs. If an unchanged
Writer follows the failed Tester, correction artifacts resolve the exact
candidate-bound Tester result by path rather than assuming a newer Tester must
follow that Writer. If an unchanged candidate first passes a failed-command
recheck, the recheck result replaces the older failure for review; later
unrelated correction rounds do not inherit stale Tester evidence.

A targeted Tests Review may accept host proof for one canonical command.
Native failures belong to that command only when its harness is unique in the
configuration; repeated harnesses require an exact synthetic command identity.
The assessment applies accepted coverage to named references in a copy of the
Tester results and retains the original artifact. Landing may carry this proof
across an expertise-only descendant commit after revalidating the original
failures, command identities, source artifacts, and passing Review. Changed
executable paths or newly failing tests block that allowance.

Every Writer persists `writer-context-usage.json` beside its transcript. The
sidecar records prompt kind and bytes, execution-context and transcript bytes,
input, cached-input, uncached-input, and output tokens, plus warnings when cached
replay, uncached input, or transcript size crosses the host-owned budget. It
does not truncate the audit transcript or fail completed model work.

Two consecutive Writer continuations, including after review, that create commits
without completing additional work against the same obligations pause for user
inspection. Matrix comparisons use stable requirement and finding identities;
new findings reset the comparison. Legacy progress compares counts only against
the same finding identities. A no-change incomplete Writer
also pauses. A malformed, unknown, duplicate, rewritten, or regressed contract
pauses for user repair. Only a reconciled candidate reaches
reviewer planning.
Those Tasks use the Writer output that exists before Learning; after Learning,
landing consumes the current `TaskOutput.commit`.
Before Fluent binds a missing initial Writer workspace, its read-only
worktree preflight checks the source checkout for staged, unstaged, and
untracked changes outside `.fluent/`. If it finds any, it reports Git's
porcelain paths and stops before Task reservation, branch or worktree
creation, baseline persistence, and coder launch. The Attempt and Task
remain planned so the user can commit or revert the source changes and
retry the same command. Changes only under `.fluent/` retain the existing
exception. Once the candidate workspace exists, recovery uses that
authoritative workspace and does not repeat this source-checkout gate.
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
`review.md` in that artifact area. A passing report additionally requires
`review-inspection.json`, which binds the Review Task role and exact candidate
commit to at least one canonical repository-relative tracked file and explains
what that role checked. The host verifies each path through Git and requires a
successful Codex command event, Claude tool-result pair, or Pi tool-execution
pair that identifies the exact candidate and declared file in the same event.
The event may name the joined absolute file path, keep the workspace and
repository-relative path separate, or pair the exact candidate commit with the
repository-relative path as Git `commit:path` inspection does. Workspace-only,
commit-only, path-only, wrong-candidate, cross-event, provider console, and
failed-tool evidence does not count; a later successful tool result can supply
the required evidence. Fluent records both artifacts before
completing a passing Review Task. A `fail` or `uncertain` report still completes
with `review.md` alone
because verdict acceptance belongs to later review or merge policy.
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

When a reconciled code-producing round has a candidate workspace, Fluent creates
the reviewer Tasks immediately and makes the Writer transcript readable to them.
The Writer's final message reports its focused, harness-native checks as advisory
evidence. Reviewers inspect the code and tests in parallel; the tests reviewer may
run one named missing check, but no reviewer reruns the complete suite.

Only after every scheduled reviewer passes does Fluent create one
`TaskKind::Tester` Task for that review round. The Tester is a deterministic
subcommand (`fluent tester run`) that reads
`.fluent/tester.yaml` from the candidate workspace, runs each declared
test command sequentially, invokes `.fluent/extract-tester-results` to
normalize the raw output into per-test entries, and writes
`tester-results.json` to its artifact directory. This final canonical gate is
bound to the exact reviewed candidate and runs once per candidate that reaches
it. Established introduced assertion or harness defects retain their corrective
Writer or harness-repair routes. Verified unmet prerequisites require environment
recovery; unexplained failures pause for diagnosis without another Writer.
Missing or unreadable proof cannot establish a pass. Preserve the result and logs,
and follow the [host evidence recovery](#host-evidence-recovery) handoff when an
outstanding check needs exact authorized proof. Applicable passing results advance
to Learning. The Tester does not spawn a Coder process or write a transcript — it
is a deterministic subprocess, not an LLM agent.

Each Work Task Tester launch is wrapped in a host-owned tracked-state guard. The
guard admits only the expected candidate commit with a clean tracked index and
worktree. After the launch, including a launch that reports test or harness
failure, Fluent records any tracked status in `candidate-mutation.json`, runs
`git restore --staged --worktree -- .`, verifies the original commit and clean
tracked state, and only then interprets or retries the Tester outcome. Command
results and logs live in the Task artifact outside the candidate and therefore
survive restoration. The guard deliberately does not run `git clean`: untracked
and ignored build outputs remain governed by the existing candidate gate and
project ignore rules. A preexisting tracked change or a changed `HEAD` is not
attributed to the Tester and fails closed without restoration.

An explicit resume can also recover a tracked mutation left by a completed
Tester from an older Fluent build. This compatibility path is intentionally
narrow: the persisted final-Tester gate must be blocked only with a parseable
dirty candidate status, the latest completed Tester and Writer must still name
the same commit, `HEAD` must match, the current tracked status must byte-match
the gate after Git's output trimming, and the gate must contain no untracked
entry. Fluent then writes the same mutation evidence, restores tracked state,
and rechecks the unchanged candidate. Any mismatch leaves the workspace alone.

Review-only Attempts keep their existing Tester-first contract because no Writer
produces focused candidate evidence for them.

The pre-write baseline remains part of each Attempt's evidence. Before the
initial Writer, Fluent resolves the exact source commit's baseline and copies it
into the Attempt artifact. Corrective Writers reuse that same Attempt-local copy
even though no final Tester has been planned yet.

Every final Tester writes a host-owned baseline-seed manifest after binding its
results to the exact reviewed commit. The manifest records the source commit,
Tester configuration and extractor digests, Fluent build, platform,
architecture, sandbox mode, and artifact and result digests. When that exact
commit lands under the same boundary, Fluent atomically promotes the final
Tester artifact into the project's durable landed-baseline cache. Known test
failures remain useful baseline evidence; a harness error does not.

Capture-mode landing verifies the exact candidate after rebase and any
`fix-pre-merge` commit, before fast-forward. It validates the final Tester seed
without publishing it; if the seed does not match, it runs the guarded Tester
against the final candidate tip. It compares those results with a trusted
baseline for the target commit captured at the start of landing. A missing
target baseline requires a run in an exact detached checkout; a cached baseline
avoids that run. Harness errors, process leaks, unexplained failed commands,
and newly failing test IDs block landing. Unchanged target failures remain
accepted even when the Attempt-start baseline passed.

After fast-forward, Fluent promotes the exact pre-land artifact into the durable
landed-baseline cache. It does not immediately repeat a successful verification.
Legacy landed candidates and the identity-only `no-expertise` route retain the
post-land fallback: when no exact seed is available, a detached checkout captures
the merged commit's baseline. A failure after durable fast-forward warns and
remains retryable; it never undoes the merge.

Direct and scheduler-launched Attempts first check the durable cache using the
same immutable identity. A file lease serializes publication; consumers validate
the manifest and artifact digests before copying the complete evidence into
their own Attempt artifact with digest-bearing provenance. Scheduler-launched
Attempts still receive one opaque baseline-session id. When no landed entry
exists, matching Attempts share one run within that scheduler lifetime. The
scheduler deletes only its temporary session cache after workers drain and
preserves durable landed entries. Harness errors stay Attempt-local, while
missing or changed cached results fail closed before reuse. Suite-health checks
always read the Attempt-local copy.

The scheduler launches each Attempt with the ordinary sandbox default. Its
baseline-session id is host coordination data: Fluent consumes it while resolving
the pre-write cache, then removes it from every configured Tester command and
result extractor. Nested Fluent processes created by the repository test suite
therefore cannot join, publish to, or recursively capture the outer scheduler's
baseline session.

Xcode's embedded SwiftPM manifest sandbox cannot apply inside Fluent's
restrictive outer Tester Seatbelt profile. Fluent does not weaken that profile
automatically: even a minimally restricted outer profile can block the inner
`sandbox_apply`, while permitting arbitrary child profiles would let a project
command escape the outer policy. A production occurrence is therefore persisted
as a `tester-harness` pause with a structured Tester no-sandbox recovery and an
exact recovery handoff. Status and Work Item show derive the same command from
that durable recovery. If the user trusts the candidate, `fluent queue resume
<work-item-id> --attempt <attempt-id> --no-sandbox` records one dispatch-scoped
approval. The scheduler translates it
to an internal Tester-only Attempt capability, which is validated against the
single `NeedsUser` Tester Task before the Attempt is reopened and consumed only
when that Task launches. Completed Writers are not rerun, and later Writers,
reviewers, Testers, and the Learner retain the ordinary sandbox policy. The
scheduler must itself run from a host boundary that permits the Xcode child
sandbox; an enclosing agent sandbox still applies.

For a Tester that runs as a Work Task, Fluent reads and validates its
configuration without creating paths, then durably reserves the Task before it
creates the Task artifact area or any guarded process-settlement directory.
That ordering means a peer-terminal transition rejects the start without leaving
filesystem artifacts. After reservation, Fluent creates and canonicalizes each
guarded directory before rendering and preflighting the sandbox that grants it
to the command. Standalone `fluent tester check` has no Work Task reservation;
it creates its temporary artifact as part of its own command boundary. That
host-owned setup does not initialize the project, create `.fluent/.gitignore`,
or edit `AGENTS.md`; only a project-defined Tester command can intentionally
write inside the candidate workspace.

`fluent tester check` first validates the project's Tester structure and then
runs the same sandboxed command and normalization boundary used by a Tester
Task. It reports ordinary normalized failures as test-suite failures. Because
the standalone check uses a temporary artifact and creates no Attempt, operators
repair its configuration, extraction, sandbox, execution, or result-persistence
problem and rerun `fluent tester check`. The same failures from a production
Tester Task pause its existing Attempt as a Tester harness error. An
environmental repair can resume that exact Tester while completed Writer work
remains complete because the reviewed candidate stays frozen. A repair to
candidate-owned `tester.yaml` or the extractor instead uses `fluent attempt
repair-tester <work-item-id> <attempt-id> --resolution "<repair needed>"`.
That explicit transition records the operator's decision and the failed Tester
evidence, preserves the original candidate and failed Tester as history, and
plans one `tester-harness-repair` Writer on the existing candidate workspace.
The host admits the Writer only from the exact clean reviewed commit and accepts
only a clean descendant whose net change is confined to `.fluent/tester.yaml`
and `.fluent/extract-tester-results`, both still regular files. Fresh reviews
and the final Tester then bind to the repair Writer's commit. Repeating the same
repair request or scheduler resume reuses its existing durable record; ordinary
candidate drift still fails closed.

The repair Writer receives the immutable failed Tester artifacts as diagnostic
context, but neither the completion matrix nor the generated execution context
translates their ordinary command failures into product obligations. The
operator's recorded repair decision is its authority, and its accepted Git
surface remains the two Tester files. If the Writer nevertheless commits outside
that surface, Fluent records
the rejected commit in the Writer artifact area and restores the managed
candidate to the approved commit before pausing. A retry may normalize a clean,
linear rejected descendant produced by an older Fluent build only after the
Task start is durably reserved. It never cleans a dirty workspace, accepts a
non-descendant, or moves the candidate during read-only preflight.

The `behaviors.md` format supports two markers on EARS statements:
- `Test:` — names a test that verifies the behavior.
- `Untestable:` — marks a behavior as genuinely untestable with a reason.

The final Tester produces host-owned `tester-results.json` and binds it once to
the exact candidate commit. It is the authoritative canonical-suite gate used by
the Attempt loop and Learner. Code reviewers run before it and use the Writer's
focused verification only as advisory evidence. Review-only reviewers consume
their Tester-first result as default executable evidence.

Status and `work-item show` expose cycle-cost measurements alongside lifecycle
state: review rounds, summed duration of completed Tasks, known provider wait,
active duration after subtracting that wait, input/output tokens, repeated stable
findings, artifact bytes, and pre-review continuation cycles that avoided
premature Tester and review work. Task settlement updates only that Task's
contribution to one per-Work-Item sidecar under `.fluent/work/metrics/`;
read-only status and show load only that bounded file and never recursively walk
artifact trees. Live Task state may read the Task's two bounded provider timing
documents directly to distinguish `provider-wait` from ordinary execution.
After an upgrade or manual artifact repair,
`fluent work-item rebuild-metrics [id]` reconstructs the complete sidecar
explicitly.

That focused check uses one project cache keyed by the candidate commit under
`.fluent/work/cache/reviewers/`; reviewer artifact areas never receive private
build trees. Admission is serialized by a project lock and accounts for the
shared caches against `reviewer-cache.max-project-gib` and
`reviewer-cache.min-free-gib`. Invalid configuration, failed accounting,
unavailable capacity, or an exceeded limit starts the reviewer cold without
pausing the Attempt. Toolchain detection and reflink, hardlink, then deep-copy
fallbacks remain unchanged. A custom `prepare-pre-review` hook still writes
through its artifact contract, but admitted canonical cache directories are
relocated to the shared candidate cache while diagnostic hook output remains in
the artifact. Caches remain while any nonterminal Work references their commit
and retire when no such Work remains. Each settled review records artifact byte
usage and warns about oversized artifacts or a private managed cache.
Review-only and post-merge review Attempts do not provision caches from the
source checkout.

`fluent attempt run <work-item-id> <attempt-id>` is the first
Attempt-level orchestration path. It advances one Attempt by running the
next planned write Task serially through the Task executor, or by running
planned review Tasks in parallel with concurrency limited to
`FLUENT_MAX_PARALLEL_REVIEWERS` (default 5, minimum 1). Review-only
Attempts run review Tasks serially because their reviewers share a source
checkout. Before a Writer or reviewer reserves its Task, and before a Learner
records an in-progress run, it acquires one provider-scoped user lease and one
project lease. The user lease bounds all Fluent processes and projects that use
the same provider; the project lease can impose a lower local ceiling. Waiting
leaves the Task planned or the Learner unreserved. Learner capacity waiting
rechecks active cancellation even while its reviewed Attempt stays Complete;
it exits and releases the Learner lease so cancellation can settle while provider
slots remain occupied. The provider lease spans the complete logical role run, including provider recovery, generic Task retries, and Learner
schema repairs, and drops on success, failure, cancellation, or process exit.
Different providers use independent pools, so unavailable Claude capacity does
not serialize Codex work.

Set the shared ceiling only in `~/.config/fluent/config.yaml` under
`providers.concurrency.default` or a provider key such as `codex`. A project may
lower its own use under `providers.project-concurrency.default` or a provider
key in `.fluent/config.yaml`; project configuration cannot raise the shared
ceiling. Both limits default to 5.

When a write or review Task coder returns an otherwise unclassified error, the
Task executor retries up to `FLUENT_MAX_TASK_RETRIES` (default 2) times before
marking the Task failed and pausing the Attempt at `needs-user`. Authentication
and rate-limit recovery belong exclusively to the coder layer: one credential
refresh or two bounded rate-limit retries occur inside the same logical run.
Claude no-progress recovery also belongs to the coder layer. A transcript-enabled
autonomous Claude launch has a project-layered 60-second deadline until model or
tool progress, two automatic relaunches, and a 5-second initial exponential
backoff. The transcript pump accepts only complete `system/init` and
`system/api_retry` records as pre-progress evidence. Any assistant, tool,
malformed, unknown, or deadline-partial record disables automatic relaunch for
that launch. On a safe timeout, the supervisor terminates and reaps the full
process group, the retry loop preserves the phase, and the same logical Task
continues without consuming a Writer round. If termination, reaping, or the
process-group sweep cannot be confirmed, Fluent does not relaunch and pauses the
same Task with the cleanup diagnostic. The configuration keys are
`provider-recovery.claude.no-progress-timeout-seconds`, `automatic-retries`, and
`retry-backoff-seconds`; zero retries disables automatic recovery. A malformed
layer disables recovery rather than substituting a retry policy.

```yaml
provider-recovery:
  claude:
    no-progress-timeout-seconds: 60
    automatic-retries: 2
    retry-backoff-seconds: 5
```

Typed auth failures, typed rate-limit exhaustion, and typed Claude no-progress
exhaustion bypass the generic Task retry loop, preventing nested retry
multiplication. Tester Task errors follow the same retry-then-pause policy but
do not check for auth rejections because the tester invokes test harnesses, not
a coder.
Each failed Task writes a per-task handoff file
(`needs-user-{task_id}.md`) so concurrent review failures preserve independent
context. When that Task owns the Attempt's current pause, Fluent also replaces
the managed `needs-user.md` atomically and reports recovery only after that write
succeeds. The loop reloads stored state before deciding the next transition.

`execution.rs` publishes a `launching` execution identity beside the Task
transcript before the Task becomes `executing`. Task and Learner reservations
bind that record to the owning project, Work Item, and Attempt. The shared spawn
boundary takes the Work model lock before the execution lock, checks the durable
Attempt stop, and holds both through child creation and identity publication.
It releases both before waiting for the worker or settling a failed launch.
Thus a reserved peer cannot start while cancellation is still settling another
worker, even if its own execution record has not yet been marked `canceled`.
The process tree holds an inherited `execution-owner.lock`. The parent first
stores a unique launch generation in `execution.json` with state `launching`
and zero leader and group fields. Before it enters the requested program, the
child publishes `execution-receipt` with that generation and its PID after
checking that its process group equals its PID. Recovery derives the group from
that validated PID. The parent then promotes the record to
`running` with the same identity. If the parent dies before promotion, recovery
matches the receipt to the generation and requires the inherited identity lock
to remain held; it can then cancel the child and its descendants. A held lock
keeps even a `settled` record live until ownership is released. A missing,
partial, or mismatched receipt cannot authorize signaling or a replacement
launch. The child retains the Work model lock through entry into the requested
program, so a durable stop cannot pass the final launch boundary after parent
death. Legacy version 1 records remain readable with their established
ownership rules. Version 2 records require the generation and receipt, and
older binaries reject their schema before mutation. The receipt and stop
boundaries are covered by `tests/execution_handoff.rs` tests
`launch_handoff_publishes_identity_before_payload`,
`launch_handoff_recovers_child_after_owner_crash`,
`launch_handoff_settled_record_preserves_owned_descendant`, and
`launch_handoff_cancel_serializes_with_payload_start`. Nested
Tester commands belong to their Task, and Learner executions register their
identities before entering `in-progress`. `run_task` installs a scoped,
thread-local `TaskOwner` so synchronous nested commands use the Task's transcript
and reserved identity. New threads do not inherit that scope: moving a nested
command to another thread requires explicit owner propagation before launch.
Command transitions and heartbeat updates preserve the Attempt binding. Legacy
records without it remain readable; new Task and Learner reservations always
publish it before execution.
Cancellation marks even a naturally `settled` child record as `canceled`, so the
next command or extractor in the same Task cannot launch. Only a new reservation
admitted by the Work model after explicit resume can clear that stop.

`fluent attempt cancel <work-item-id> <attempt-id>` first persists an optional
Attempt cancellation record under the Work model lock. It also records an
`interrupted` pause unless the Attempt owns a persisted pending Merge Candidate.
Task reservation, follow-up creation, completion, Learner capture, and readiness
reject advancement while that stop is active. Cancellation then validates each
owned identity before signaling, waits for process trees and execution leases to
settle, and records settlement. Repeating the same settled cancellation changes
no history. Explicit `attempt run` captures the current cancellation before
initialization, then validates that same request under the Work model lock before
recording resume and replanning unfinished work. Scheduler children carry their
dispatch ID and use only the cancellation authorized by that dispatch's queue
admission. A delayed launch cannot authorize a newer stop during startup;
completed Tasks remain complete, and canceled verification retries use new
artifact directories. Historical Attempts without a cancellation record keep
their existing semantics.
An Attempt with a reviewed pending Merge Candidate stays `complete`, with no
`interrupted` pause kind. Its active typed cancellation record blocks execution
and advancement, even if Learner previously succeeded. The structural candidate
invariant retains only its existing host-sandbox pause exception. Matching
explicit resume consumes the stop after settlement, preserves Tasks, the
candidate, and its original readiness timestamp, and resumes unfinished Learning.
The storage guard admits the Complete-state stop only while preserving that
pending candidate exactly; Task, Learning, and readiness advancement stay blocked.
Natural completion observes the leader without reaping it, sweeps its process
group, and then reaps exactly once. On macOS, `killpg` returns `EPERM` when an
exited, still-waitable leader is the group's only remaining member; Fluent maps
only that observed-exited case to already gone. A live same-user descendant
remains signalable, so the group signal succeeds and retains the ordinary
descendant-sweep path.

At a legitimate failed-correction Writer-round cap, an operator may run:

```text
fluent attempt extend <work-item-id> <attempt-id> --additional-write-rounds N --resolution "<authorization>"
```

where N is one through three. Under the land lock, Fluent verifies a clean registered
candidate worktree, exact candidate commit, exhausted current cap, and the exact
SHA-256-bound failed correction bytes. The correction frontier includes failed
review output and, for a rejected final-Tester gate, the gate, structured Tester
result, and attached failed-command logs. A rejected gate owns the Tester
frontier whenever it exists. If a no-change Writer follows it, Fluent resolves
the gate's exact Tester by candidate-bound result and gate path instead of
requiring the Tester to follow that Writer. A final Tester with a structured
failure or harness error stops before that gate; in that no-gate path, Fluent
instead accepts only the latest completed final Tester after the latest Writer,
requires its result to name that Writer's exact candidate commit, and binds the
result plus the exact logs for its failed commands. Passing, stale, missing, malformed, or
candidate-mismatched Tester evidence cannot authorize an extension. The
extension path advertised by either round-cap handoff is therefore executable
without approving mutable or unreadable evidence. The append-only approval does
no code work. Resume revalidates that frontier before reopening the same
Attempt; any changed candidate or correction content fails closed.

`Attempt::effective_write_round_cap` uses the greater of the approved cap plus
subsequent evidence allowances and the materialized Writer count. Structural
validation in `src/work_model.rs` applies the same rule between extensions:
the next `old_cap` must equal the greater of the preceding `new_cap` plus the
recorded evidence delta and the proven Writer count at approval. For example,
a preceding cap of 16 with no new evidence allowance can support an extension
from 18 to 19 when 18 Writers already existed at approval. Later Writers cannot
explain that historical gap. Increment bounds, checked arithmetic, extension
identities, evidence monotonicity, paired approval fields, and human-resolution
digests continue to apply.

Current extensions record their configured base cap and pre-approval Writer
count in `authorized_base_cap` and `pre_extension_write_rounds`. When a legacy
entry lacks both fields and needs a Writer-count floor to explain a gap, the
validator parses saved Writer creation times and the approval time as instants.
It requires an unambiguous creation order, excludes Writers created after
approval, and binds the last pre-approval Writer to a completed output matching
the approved candidate. That Writer must have completed between its creation
and approval. Missing or invalid timestamps, tied or contradictory creation
order, creation at the approval instant, and candidate mismatches reject the
entry with its extension identity. Partial explicit fields cannot use legacy
inference. Contiguous legacy chains need no timestamp reconstruction.

This proof uses only saved model values. Status, Work Item list/show, and cleanup
preview preserve history without loading historical review files or publishing
a migration. Cleanup still applies its terminal-state and workspace-safety
rules, so a readable paused Work Item remains unfinished Work. Recovery retains
its independent authorization, exact review-byte, candidate-worktree,
cancellation, and live-owner checks. Failed-review Approach revision still
requires explicit bound approval fields; a derived legacy count supplies no
such authorization.

A failed-review `round-cap` may also authorize an Approach revision, but only
after a separately recorded bounded extension leaves an unused Writer round.
The extension binds the exact candidate and failed-review bytes. Then submit the
replacement Approach with:

```text
fluent attempt revise-approach <work-item-id> <attempt-id> --approach-file <path> --resolution "<authorization>"
```

The Approach revision additionally binds the authorization, prior and
replacement Approach, and every supporting review identity and digest.
Architecture may pass when another settled review failed. The command first
journals the immutable revision audit as a managed file in the same Work Item
aggregate transaction that publishes its reference with the revised planning,
completion contract, recovery binding, and exactly one corrective Writer. The
transaction does not launch that Writer. Before transaction application, no
audit sidecar exists; during interrupted application, a pending transaction may
expose its managed bytes, but the aggregate reference and exact managed-file
publication together establish authority. A matching retry reuses those exact
bytes, while an unrelated file remains inert and conflicting bytes fail closed.
Foreground and queued resume use the same shared admission checks, while exact
retries reuse the existing publication and changed or partial authority fails
closed.

An evidence-targeted `code-change` result supplies one scoped Writer allowance
because that recovery transition plans the Writer even when the ordinary cap is
exhausted. The effective cap adds only `code-change` recovery records whose
targeted review Task is an input to a Writer Task. Each explicit extension
snapshots how many of those allowances its `new_cap` already includes, so
repeated evidence corrections advance the frontier once each and later
configuration changes cannot rewrite an approved cap. Reviewing,
`evidence-needed`, and completed recovery records add no Writer capacity.

After the
initial write output completes it plans review Tasks for the full Work
reviewer set. After a follow-up write output completes it derives the
next review roles from that Task's failed review input artifacts; when
it cannot derive at least one role, it falls back to the full Work
reviewer set. After a completed reviewer round it interprets review
artifacts with the review subsystem verdict parser. A failed or uncertain
review returns directly to a Writer without spending a canonical Tester run.
When a corrective Writer returns an accepted no-change declaration over failed
final-Tester command evidence, Fluent first runs the failed-command recheck
described above. An established command, structured-test, or process-settlement
defect returns the recheck manifest, result, and relevant logs to the next Writer;
the existing write-round budget decides whether to continue or pause at round
cap. A verified unmet prerequisite requires environment recovery. An unexplained
nonzero exit pauses for diagnosis without another Writer; preserve the manifest,
result, and logs to establish the cause. Follow the status-provided command in
[host evidence recovery](#host-evidence-recovery) when exact authorized proof can
satisfy the outstanding check. A passing subset admits the normal Reviewer round
but never replaces the complete post-review Tester.
When all reviews pass, Fluent plans one final Tester and checks its
`tester-results.json` — only failures the
Work Item introduced (tests failing now that were not failing in the
pre-write baseline) block the Merge Candidate path; pre-existing
failures pass through. When no baseline is available the gate falls
back to blocking on any failure. A missing or unparseable final result
fails closed. All reviews and the final Tester passing with no introduced tester
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
`.fluent/work/artifacts/<work-item-id>/<attempt-id>/`. An explicit direct or
queued resume reopens only the roles whose latest verdict remains uncertain.
`lifecycle_recovery` verifies that every Task has settled, each latest review
artifact is readable and bound to the latest Writer commit, the candidate
worktree is clean at that commit, no latest verdict is `fail`, and no Merge
Candidate exists. The Attempt loop repeats the same inspection inside the Work
Item mutation, appends the targeted review round, and clears the pause. Passing
roles remain complete and continue to count when the targeted roles finish.
Changed or dirty candidates, missing evidence, active Tasks, and stale review
contexts fail without a model write.
For review-only Attempts, all pass marks the Attempt complete with review
state `passed` and does not create a Merge Candidate. Any fail marks the
Attempt failed with review state `failed` and does not create a follow-up
write Task. Uncertain verdicts without failures mark the Attempt
`needs-user` and write the same Work handoff artifact.

### Recover a usage-limit-interrupted Writer

`fluent attempt recover-usage-limit <work-item> <attempt> --resolution "<decision>"`
records an explicit approval while leaving the Attempt paused. A separate
`fluent attempt run <work-item> <attempt>` or
`fluent queue resume <work-item> --attempt <attempt>` requests execution. The
ordinary failed-Writer guard and existing provider-pause grammars still apply.
This route supports a failed corrective Codex Writer at `round-cap`, with a
preserved correction assignment and settled execution ownership.

`provider_evidence::codex_usage_limit_failure` accepts the complete surviving
four-event native stream: `thread.started`, `turn.started`, `error`, then
`turn.failed`. The two terminal errors must carry the same supported usage-limit
message. Missing, additional, malformed, contradictory, or model-authored events
are rejected. Fluent resolves the transcript from the failed Task's managed
artifact area and verifies its path, file type, execution record, and ownership
leases. A terminal status or old timestamp alone does not establish settlement.
The final stream cannot reconstruct earlier overwritten launches or establish
the origin of every partial edit. The immutable approval records that limitation;
provider-session rollouts grant no failure or completion authority.

`usage_limit_contracts` owns read-only inspection below lifecycle, queue, status,
persistence, and execution. `lifecycle_recovery` exposes the shared resume
predicate to foreground and queue consumers. `usage_limit_recovery` publishes approval, prepares
the retry, and records launch identity through the existing Work-model mutation
and execution locks. Approval uses an append-only recovery operation and a typed
schema-1 audit bound by its immutable input digest. The generic operation's
`candidate_commit` still names the last completed Writer output; its
`evidence_digest` remains reserved for attached host evidence. The audit binds
the actual partial HEAD separately, together with the Git directory and branch,
raw index and stage entries, working-file bytes and modes, deletions, untracked
source, original failed Task, prior Tasks and evidence, saved coder settings,
correction assignment, cancellation state, and round extensions. Fixed untracked
exclusions cover `target`, `.git`, `.fluent/work`, `.scratch`, `node_modules`,
`.cargo-home`, and `tests/output`; tracked files remain bound even there. The
snapshot preserves source symlink leaves by binding their type, permissions,
and literal target bytes without following their targets, including external or
dangling targets. It rejects symlink ancestors, symlinked managed evidence,
other nonregular files, unmerged stages, unsupported path encoding, files over
64 MiB, and unstable observations.

When a matrix-bound completion recovery later hits quota, the completion
contract keeps its original source candidate and evidence bindings. Before
launch, `usage_limit_contracts::validated_prelaunch_approval` validates the exact
prepared Task, unused approval, retained failure evidence, and approved partial
workspace. The completion contract accepts that partial workspace only when
the quota approval also names its original source candidate and workspace.
Without this separate authority, the initial corrective Writer still requires
the original clean candidate. Neither contract rewrites the other's audit.

Approval neither queues nor launches work. Foreground resume, queue admission,
scheduler claim, direct Task reservation, and the immediate pre-spawn boundary
revalidate authority. Status uses the same read-only inspector and suppresses
stale guidance. Resume preserves the same Task and Attempt, assignment, provider,
model, effort, and round accounting. The Writer can change its source after
launch, but receives no credit for historical passing checks. Fresh execution
artifacts live in `usage-limit-retries/<approval-id>`; retained failure and Review
artifacts and the host audit stay outside writable roots. The scoped Codex
profile grants the specific worktree, Git metadata, progress, fresh artifacts,
and private worker/cache roots without broad shared-temporary-directory writes.
Installed Cargo, rustc, rustup, and their resolved toolchain paths receive read
and executable-map access; installed tools remain read-only. The retry also reads
macOS's selected-developer links, including `/var/select/developer_dir` and its
`/private/var` spelling, and their resolved directory so `xcrun` can locate the
SDK and linker. Those additions grant no writes or reads of sibling files.
Git resolves configuration origins and include paths in the retained Writer
worktree, including worktree-specific `includeIf` settings. The retry admits
only existing regular configuration files and their resolved symlink targets;
active empty includes also receive read access. Include queries report inactive
declarations too, so Fluent asks Git to evaluate each condition against a
temporary marker in the prepared private worker home before granting its target.
It removes the marker after the query and grants no reads for inactive targets.
Git expands home and installation
prefixes, and relative includes resolve beside their declaring file. Fluent
does not copy configuration values or grant their parent directories, and
operator configuration remains read-only.
`CARGO_HOME` names the private dependency cache, and `TMPDIR` and
`CFFIXED_USER_HOME` name private scratch directories within that cache.
Compiled workspace programs can execute.
These allowances grant no writes to the operator's Cargo/rustup state or retained
failed execution, Review, and approval artifacts.

A prepared retry can also pause before reservation when the private Codex
sandbox readiness check fails. `usage_limit_contracts::validate_preflight_pause`
requires the existing host-owned transition history: explicit preparation under
this approval, followed by the Task executor's Planned-to-NeedsUser transition
and its adjacent Attempt host-sandbox pause. It checks the unchanged prepared
Task, no recorded start or launch marker, no execution/transcript/supervision
evidence, settled ownership, and all original approval bindings. Missing or
conflicting history rejects recovery; a pause label or diagnostic is insufficient.
`usage_limit_recovery::resume` reopens that same Task under the Work lock without
another approval or Writer round. Direct Task execution cannot reopen the pause.
This derives eligibility from existing records and introduces no durable marker,
schema, or independent recovery authority; older approval-aware coordinators
continue to refuse the NeedsUser Task rather than discard an unknown field.

Strict Codex readiness runs `login status` through the prepared production
launcher from its admitted private home. Rendering includes that exact home's
ancestor metadata so canonicalization works when the workspace is outside the
worker temporary tree. Ancestors receive no content or write grant, and unrelated
temporary and operator-home contents remain inaccessible.

Immutable audit publication can precede a journal. Matching retries reuse it;
conflicting bytes fail. The transaction lists new approval/preparation operation
IDs and retains the existing preparation ID when it reopens a paused Task.
Both transitions revalidate approval bindings before publication, including
when a maintenance read or format upgrade recovers the journal. An interrupted
reservation can resume only when locks are free, recorded processes have exited, execution
is still an unlaunched setup, and no host-owned launch marker exists. A marker
means launch may have happened, so a lost acknowledgement cannot authorize another
worker. A later stop or terminal failure invalidates earlier restart authority;
a new supported quota failure requires another explicit approval.

`tests/binary.rs` exercises real public recovery/resume, queue and scheduler
routes with isolated historical fixtures and provider doubles. The normal
`usage_limit_writer_recovery_real_confinement` test places its repository outside
the worker temporary tree. Its mandatory host form prepares the installed Codex
launcher with existing authentication and runs production sandboxed readiness
without synthetic overrides. It also requires an offline Rust test that compiles
and runs inside the production retry profile with the actual launch environment
and installed toolchain. It also checks source and fresh
artifact writes, Git source operations, denied reads of unrelated temporary and
operator-home content, and denied writes to installed tools,
operator state, shared temporary paths, and retained failure, Review, and audit
artifacts. Only a successful real host probe establishes enforcement; a nested
sandbox skip supplies no host proof. Its mandatory host form sets
`FLUENT_USAGE_LIMIT_REQUIRE_REAL_CONFINEMENT=1` and fails when the probe cannot
run. The assigned host attaches exact-candidate proof through the required Plan
row for targeted Tests Review. Current-candidate completion, all five ordinary
review domains, the canonical final Tester with process-leak rejection, and
Learning remain separate required gates.

Compatibility tests use the actual pre-change executable from
`5c670fdd9eec7eda22e93aee4ea619f1c60f7a70`. They verify its full source archive and
binary digests, published outer and inner schema refusals, and a journal
interrupted by the public approval command. Current code completes matching
publication once. The canonical Tester prepares the old reader; equivalent
focused preparation is:

```sh
set -eu
FLUENT_USAGE_LIMIT_OLD_BIN="$(bash tests/fixtures/build-usage-limit-old-reader.sh)"
export FLUENT_USAGE_LIMIT_OLD_BIN
cargo nextest run --offline --features test-support --test binary usage_limit_writer_compatibility_
```

The helper uses the pinned lockfile, a separate target directory, and the supplied
private `CARGO_HOME`. Missing or mismatched preparation fails the tests.

### Host evidence recovery

`fluent attempt evidence attach --finding <finding-id>` hands off one assigned
Tests finding after source corrections leave only its host check pending. The
source Review may name candidate A while the latest completed Writer names B.
Fluent verifies A through the immutable assignment, then binds the supplied
schema-1 evidence to B, the exact structured command, relative working directory,
and declared applicability. This route requires a clean registered managed
candidate and settled owners at `needs-user/writer-completion`; attachment
creates one targeted Tests Review and executes no host command.

`EvidenceRecovery.carried_finding` stores a typed `CarriedFindingEvidence`.
Its schema-1 binding preserves Work, Attempt, current Writer, candidate, assigned
finding, original assignment and Review, source completion matrix, contract,
command, applicability, and proof digests. An immutable content-addressed artifact
stores the same binding under `carried-finding-evidence/`. The new
`host-evidence-carried-finding-attachment` operation namespace leaves earlier
selector identities unchanged.

`src/recovery_contracts/carried_finding.rs` validates source authority, targeted
Review inputs, acceptance, and current completion. Queue admission and claim,
foreground resume, direct Review launch, and status consume that neutral boundary.
A passing proof plus a passing targeted Review supplies an in-memory completion
claim for the selected finding. Its saved Review digest remains mandatory.
Historical assignments, Review bytes, matrices, and Writer output remain intact.
After revalidating accepted finding evidence, shared recovery assessment continues
with later required checks and active recoveries for the same candidate. Finding
acceptance cannot hide their blockers, expiry, or next attachment command.
`verification_assessment` excludes this receipt from generic command coverage;
ordinary reviewers, the canonical Tester, candidate gates, and Learning still run.
The targeted Review reads only declared binding, source Review, proof, and source
completion files through the existing worker read boundary.

Exact attachment retries reuse the saved operation and Review Task. Before
recovering an interrupted split publication, the store checks that the pending
target matches that exact operation and proof, repeating validation under its
model lock. Changed input cannot recover or replace an active operation. Fresh
proof may replace intact expired proof or an unchanged `evidence-needed` Review
after owners settle, retaining the old proof and completed Reviews. A later stop
survives an old attachment or dispatch; fresh explicit resume remains required.
A targeted `code-change` Review captures a new Writer assignment against B.

Canonical Tester failures also accept exact host proof through
`fluent attempt evidence attach --required-check <id>`. A selector uses
`tester:<tester.yaml-sha256>:<zero-based-command-index>` so repeated command
entries remain separate requirements. A canonical execution writes
`canonical-run.json` before running commands. That manifest records the full
command list, configuration and extractor digests, and whether the run covers
the complete configured list.

The existing Attempt evidence-recovery record stores a typed `required_check`
with the project Work Item, Attempt, candidate, exact command, relative working
directory, comparison commit, original Tester Task, and immutable manifest,
result, and log references. Attachment creates a targeted Tests Review without
requiring a failed ordinary Review or recheck. Passing review records its digest
and acceptance before further advancement. `check_applicability` resolves declared comparison inputs below both Tester and
recovery. `verification_assessment` evaluates the complete original result plus
applicable reviewed replacements; it does not
rewrite the failed source result. Missing commands, unattributed structured
failures, invalid extraction, and unresolved process leaks still block readiness.

`CheckApplicability` declares bounded environment facts, comparison refs, and a
freshness rule beside each command. Required Plan rows may add an `Applicability`
column with a JSON object; review requests use one `Evidence-Applicability` JSON
line; Tester command entries use `applicability`. The default is candidate-code,
which has no age-only expiry. Live-state requires positive `max_age_seconds` and
expires at that boundary. Unsupported or missing required facts block acceptance.
Mutable comparison names resolve when execution or authorized host attachment
captures the binding; current ref values cannot establish historical values.

The shared pending-evidence assessment checks canonical, Plan, carried-finding,
and reviewer-requested proof for expiry while revalidating its original source and
candidate. Read-only status reports the original selector for fresh proof only
after execution owners settle. Explicit attachment retires the expired recovery
and idle review reservation under the land and model locks, preserving snapshots
and completed Reviews. A completed Review whose proof expired before consumption
cannot credit the check; fresh proof requires a new targeted Tests Review.

Canonical manifests and managed verification receipts record only declared facts
the executor can observe (`os` and `architecture`) and resolved comparison commits.
Unknown environment facts remain unproven until applicable host evidence supplies
them. Receipt ingestion preserves recorded observations. The neutral validator
rechecks those bindings for acceptance; a passing exit alone does not establish
the declared environment. Plan proof overlays the current completion matrix only
when its row identity, command, requirements, and candidate match the preserved
source. Settlement records the passing Plan Review digest; later overlays check
that digest and the Review context before crediting completion. Policy-bearing
Plan receipts require this digest, while preserved default-code receipts retain
the older verdict-based compatibility path. A no-change Writer does not alter
that original source or Review.

When a saved passing Plan Review still has an unsettled receipt for an older
candidate, explicit fresh attachment can settle that receipt against its original
bindings. This preserves completed history while requiring new proof for the
current candidate; changed evidence or previously recorded Review bytes still
block attachment.

Tester command results keep `failure_cause` separate from exit status. An observed
mismatch in a declared `os` or `architecture` prerequisite prevents launch and
records the required and observed values with the failed result. The executor
continues the other commands. Unknown environment facts do not imply such a
cause. The shared assessment validates the recorded observation against the
manifest before explaining an environment handoff. Unexplained nonzero exits
pause for diagnosis; structured test failures and identified harness errors keep
their existing correction routes. Log phrases alone grant no correction authority.

Plan evidence retains its original Writer and matrix as provenance. Its newest
admitted result governs the row: unresolved or malformed later proof cannot
revive an older pass. During combined recovery, the loop validates the existing
complete canonical run against the unchanged candidate and committed checker
inputs before considering a new Tester. This includes a Plan Review that settled
after a historical no-change Writer.

Required-check admission and status use the shared verification assessment to revalidate the
same source digests, candidate, policy and targeted Review bindings. Foreground
resume, queue admission, scheduler claim and direct Review Task launch each use
that evaluator before execution. Read-only status reports the outstanding check
and the currently eligible operation; missing proof suppresses generic resume.
Attachment repeats validation under the model lock. Immutable publication may
precede reservation; retries reuse content-addressed snapshots and recorded
recovery, Review Task and operation identities. Queue claim also compares its
resume authority with the current durable stop, so an earlier dispatch cannot
claim an Attempt canceled after admission.

The bounded legacy adapter supports a completed, exact-candidate canonical
Tester without `canonical-run.json` when its ordered result commands account
for the committed configuration and every command uses default candidate-code
applicability. It does not infer historical environment observations, expiry,
or mutable comparison refs. Read-only assessment derives that view in memory.
Authorized attachment publishes a derived manifest and `audit.json` beneath
`evidence-reconstructions/<tester-task>/`, then indexes their immutable references
in the existing Attempt. The audit names the original result, Task context,
candidate, and committed configuration and extractor digests. Original result,
Review, matrix, and host-document bytes remain untouched. Publication retries
reuse the same paths and require identical bytes, including a partial publication
that stopped before review reservation. Acceptance checks the referenced proof
as well as the passing targeted Review.

Corrective assignment capture recognizes only `required-progress` inputs at the
exact Work/Attempt progress, required-progress handoff, and Writer-completion
handoff paths. It publishes
original bytes and separately bound source provenance under `assignment-context/`.
The source Task keeps its original references; the assignment validates the
snapshot and provenance digests. Other source inputs and finding evidence keep
their existing immutable checks. A continuation also freezes a carried legacy
binding while its original bytes still match. This is the only permitted
assignment representation change in a continuation: Work, Attempt, original
source Task and candidate, findings, and immutable evidence remain unchanged.
Interrupted restoration accepts that exact proof without rewriting historical
assignments, clearing cancellation, or granting completion credit.

`attempt recover-assignment-context` accepts an exact candidate, assigned Writer,
assignment digest, assigned context path, and a file containing proven original
bytes. `recovery_contracts` validates their meaning and settled ownership;
`assignment_context_recovery` publishes under the Work-model aggregate lock.
A deterministic request path locates the immutable before/after audit. The
recovery operation binds the complete audit digest, so replay can retain its
original identity after a later stop without accepting changed audit bytes.
The store permits only that audited context substitution and preserves all
other assignment and Task fields. Recovery never overwrites current progress,
launches a Task, or supplies verification credit. Status and Work Item, Attempt,
and Task show use observation reads, including while a transaction is pending.
Context-recovery journals retain the authorized base aggregate, bound by the
existing base-child digest. Replay validates the original audited substitution
and requires each published child record to match its base or target version.
The matching recovery command can reconcile after any child write under the
storage lock; status and show continue to withhold partially published state.
All new journals use transaction schema 10 to fence each predecessor, including
ordinary writes of combined authority. Current readers still accept the source
versions' schema-6 journals and schema-7 context journals with their base proof.

Context recovery also admits a bounded output-free failure: the original Writer
has a validated completion-recovery authority and native pre-launch record, its
source candidate remains clean, and all ownership is settled. Its schema-2 audit
pins the failed Task, any unstarted later reservation, and retained failure files.
The neutral `recovery_contracts::context_retry` module validates a separate retry
authorization; `writer_completion_recovery::context_retry` publishes it. A stable
per-Task audit prevents conflicting authorization after an interrupted write.
The same Task retains its assignment and receives a fresh artifact directory.
Explicit resume, Task reservation, and provider spawn revalidate the same proof;
spawn consumes a launch marker under the model lock. Context-retry journals retain
the base aggregate and accept only exact base or target children during replay.
Later journals that retain this authority also retain the base, so removing the
publication list cannot hide a new retry operation. Readers require its base
digest and exact operation difference before any generic journal reconciliation.
Neither context repair nor retry approval changes verification credit. Observation
commands derive context recovery, completion repair, then resume in that order.
An `InvalidCompletion` retry source uses the same audited publication and launch
checks but requires the original malformed handoff and host-owned baseline. It
does not acquire assignment-context repair authority. Its recovery operations
carry a result discriminator that the pinned predecessor cannot deserialize,
even if older outer Work and Attempt envelopes are restored. New authority uses
Work and Attempt record schema 13, writer envelope schema 11, and transaction
schema 13; ordinary records retain their predecessor formats.
For a failed preparation, the execution record must still name a launch that
never spawned a child. An earlier child may have left a receipt in the same Task
area. Fluent accepts that receipt only when it is well formed and names a
different generation; the current generation's receipt or an ambiguous receipt
blocks recovery. The retry audit binds the retained receipt bytes with the other
failure files and prepares fresh outputs for the same Task.

Context recovery selects the latest completed Writer even when completed
targeted Reviews follow it; every Task must still be settled. If carried-finding
evidence binds that Writer's original assignment, keep the binding unchanged.
The shared recovery contract follows only complete, operation-bound context
audits from that assignment to its recovered snapshot. It revalidates exact
source, proof, Review and matrix bytes before retaining acceptance. Recovering
context supplies no new completion credit and does not launch another Task.

Work and Attempt record schema 10, writer envelope schema 9, completion contract
and matrix version 2, and transaction schema 10 fence executables that predate
explicit verification meaning, carried-finding and usage-limit retry authority, and frozen assignment
context. Writer correction assignments use schema 2 and still read schema 1;
candidate gates use schema 5; execution context with a verification assessment
uses schema 2. Current readers accept supported earlier records and journals. The combined
compatibility tests pin the landed preflight, assignment, and carried-evidence
predecessors by complete source archive and executable SHA-256. Each predecessor
authors its own published and interrupted recovery fixtures; the current reader
must preserve their authority through public upgrade and exact recovery replay.
All three must refuse combined published records and both kinds of pending
transaction, with the root envelope lowered only to exercise the inner fences.
The installation integration also pins main, output-free retry, and accepted-proof
source builds. Records use schema 10, writer envelopes use schema 9, and journals
use schema 10 because a repair predecessor could otherwise rewrite combined
authority and discard main's managed revision files. The combined regression
checks each outer and inner fence, preserves predecessor-authored records through
current upgrade, and recovers a real interrupted revision only with the current
coordinator. The compatibility tests invoke pinned pre-change executables against
both published and genuinely pending state and check exact bytes. The pinned-reader
regression preserves required-check policy, source and reconstruction references,
accepted Review digests, and corrective Writer assignments through public current
writes. It makes a Task record directory unwritable to interrupt a real public
mutation after journal publication, verifies that the old executables cannot
recover it, and then lets the current executable recover without losing
authority or changing audit bytes.
`tests/fixtures/build-carried-finding-old-reader.sh` builds the production reader
at `b5d8bf02b016a3977a08fc713e47f525b895aed0` with locked offline dependencies and
an isolated target directory. Its source and executable hashes are checked before
public mutation tests. The canonical Tester prepares it before nextest; equivalent
focused setup from the candidate root is:

```sh
set -eu
FLUENT_CARRIED_FINDING_OLD_BIN="$(bash tests/fixtures/build-carried-finding-old-reader.sh)"
export FLUENT_CARRIED_FINDING_OLD_BIN
cargo nextest run --offline --features test-support --test binary -E 'test(carried_finding_evidence_compatibility_)'
```

The finding tests interrupt the real public attachment after child records are
written and before the root is committed. The fixture-specific interruption
exists only with `test-support`. Independent envelope, root record, Attempt, and
transaction checks preserve binding authority and all stored bytes on refusal.
The current executable then recovers the same operation without duplicate Tasks.

`tests/fixtures/build-verification-old-reader.sh` builds source
`0b769cb4681aade324f0cee351832a94f8e72687` with locked, offline dependencies in a
separate target directory. The canonical Tester requires that binary and verifies
its source commit and executable SHA-256. Published records and real interrupted
transactions exercise both outer and otherwise-hidden inner schema fences.

`tests/fixtures/build-round-cap-approach-revision-stable-old-reader.sh` builds
stable source `bd59cf35308bf30fa5cbabc05ff5cf69cebfff5a`, while
`tests/fixtures/build-round-cap-approach-revision-assignment-old-reader.sh`
builds assignment-era source `5c670fdd9eec7eda22e93aee4ea619f1c60f7a70`.
Both use locked, offline dependencies in separate target directories; the
canonical Tester verifies each source commit and executable SHA-256. Published
records and real interrupted transactions exercise both outer and otherwise-
hidden inner schema fences.

When one missing focused result prevents a Tests Reviewer from deciding, the
review may emit one `Evidence-Command` and one candidate-relative
`Evidence-Working-Directory`, either as plain lines or Markdown list entries.
Fluent pauses before planning another Writer and reports this explicit recovery
action:

```sh
fluent attempt evidence run <work-item> <attempt> \
  --review-artifact <path>
```

The operator invocation authorizes that exact model-proposed command. Fluent
accepts one direct, single-line harness invocation, binds it to the latest
failed Tests Review and unchanged Writer commit, and persists one idempotent
`verification` Task with an optional typed `verification_context`. The context
binds the command, candidate-relative working directory, timeout, sandbox and
checker digests, and failed review digest. Historical Tasks omit this field.
The CLI reports `fluent task run <work-item> <attempt> <task>` followed by
`fluent attempt run <work-item> <attempt>`. Task execution uses the ordinary
reservation, Task lease, process ownership, and settlement boundaries, under a
host-owned Seatbelt profile. Planning and receipt attachment hold only short
coordination locks; command execution holds no project-global lock. Candidate source is read-only. `HOME`, Core Foundation's
fixed user home, `TMPDIR`, Cargo, Clang, and Swift build/cache paths point into
an already-created private run directory available as
`$FLUENT_EVIDENCE_SCRATCH`; this keeps Foundation-backed macOS caches private
even when a tool resolves the account home without reading `HOME`. The profile
may read installed user toolchains but explicitly denies Claude, Codex, and
agent session trees. It may also read the candidate's linked-worktree Git
common directory, while denying writes to both the candidate and that metadata,
so Git-aware verification can inspect the unchanged commit. The child
environment is rebuilt from a small toolchain
allowlist, so provider credentials and enclosing-agent variables do not reach
the check. Each run owns a process group and a
bounded deadline; timeout advances only after Fluent confirms cleanup. Fluent
drains stdout and stderr into separate logs capped at 384 KiB each, retaining
those files in the Task artifact directory. `verification.rs` exclusively
publishes `receipt.json`, binding Work Item, Attempt, Task, candidate, command,
working directory, checker digest, result, timestamps, exit status, and artifact
digests. The Attempt loop accepts receipts up to 16 KiB, validates every binding
and log digest, then attaches a compact schema-version-1 evidence summary with
the receipt reference and digest. Detailed logs stay outside the summary.
Missing, malformed, stale, or inconsistent inputs block acceptance; restoring
the authorized inputs and exact worker-produced result permits another validation
attempt without changing the candidate. Automatic ingestion cannot resume a
canceled Attempt.
Legacy reviews without structured request lines require the operator to supply
both `--command` and `--working-directory`; a structured request cannot be
overridden with different values.

An operator can attach trusted host-run proof to an unchanged candidate:

```sh
fluent attempt evidence attach <work-item> <attempt> \
  --candidate <sha> \
  --evidence-file <path> \
  --review-artifact <path>...
```

The evidence file is a
schema-version-1 JSON document containing `producer`, `check`,
`working_directory`, `result` (`pass` or `fail`), RFC3339 `run_at`, and captured
`output`. Fluent validates and snapshots its exact bytes under the Attempt's
managed artifacts, binds its digest to the named candidate commit, and plans
only the named failed reviewer roles. Attachment never changes the candidate or
approved planning inputs, and it cannot make a candidate landable. Existing
schema-version-1 manual documents remain compatible. Validated manual attachment
can explicitly resume a settled interrupted Attempt; invalid input leaves the
stop intact. Passing
evidence follows the ordinary host-only attachment path and cannot authorize a
source correction.

Evidence-targeted reviewers receive the immutable snapshot and the prior
decision artifact. This is normally their failed review; after an unexplained
failed-command recheck at the Writer-round cap, it is a content-addressed
binding of the source Tester result, recheck manifest, command, logs, and exact
candidate. A command-shaped Test reference rejected by the final candidate gate
uses the same attachment command with `--review-artifact <candidate-gate.json>`.
Fluent snapshots the rejected gate inside a content-addressed binding so a
later gate run cannot erase the decision that authorized the host evidence.
A focused `Evidence-Command` uses `fluent attempt evidence run`; use `attach`
when the host already has the schema-version-1 JSON document, and use
`classify` only when a failed result and targeted review authorize a source
correction.
A passing result replaces only that role's result for the same
candidate; `Verdict: fail` must include `Disposition: evidence-needed` or
`Disposition: code-change`. A new `evidence-needed` result is actionable only
when it names exactly one non-empty `External-Condition` and exactly one
non-empty `Additional-Proof`; a single marker with malformed qualification
follows the ordinary source-correction (`code-change`) path and plans a Writer.
Missing, duplicate, or unknown `Disposition` markers remain an uncertain pause.
For a complete
candidate-bound failure, an operator may authorize that correction on a paused
Attempt with:

```sh
fluent attempt evidence classify <work-item> <attempt> \
  --candidate <sha> \
  --review-artifact .fluent/work/artifacts/.../review.md \
  --evidence-digest <snapshot-digest> \
  --resolution '<source-correction decision>'
```

Source-correction classification requires an immutable candidate-bound evidence
snapshot whose `result` is `fail`. For a current actionable recovery, the
command requires the saved review digest and disposition. A legacy paused
recovery may omit that digest and contain only the unqualified `Disposition:
evidence-needed` line; Fluent derives and persists the digest during this
one-time migration. This includes the older Plan-row shape that retained
`review_state: not-reviewed` after its failed targeted Tests Review; Fluent
admits it only after verifying the immutable Plan-row binding and records the
failure as reviewed before scheduling the corrective Writer. The legacy form is
the only place an unqualified marker is classified: an unqualified new review
never pauses for this command and already follows the ordinary Writer path.
Both classification forms require the exact
candidate and evidence snapshot, current clean worktree, and complete targeted
Tests Review frontier. A new classification must target the current paused
recovery; a later `Reviewing` or `NeedsEvidence` recovery rejects the older
classification and its continuation.
Exact classification replay is idempotent only while the saved classified
Writer remains resumable. The supported resumable states are `Planned`,
`Executing`, or `NeedsUser` with no output, and `Complete` with output. A
`Failed` or `Superseded` classified Writer is not replayable. In either state,
repeating `classify` is rejected without mutation. For a `Complete` Writer with
output, replay retains the original source, review, and evidence bindings but
validates that saved output's workspace as the current clean candidate before
returning the stored result. For a `Failed` Writer, classification grants no
retry authority. Repeated `classify` still rejects a failed Writer, including
after usage-limit approval. Only the dedicated `attempt recover-usage-limit`
approval followed by a separate explicit resume can retry that same Task;
unrelated failed Writers remain ineligible. For a `Superseded` Writer, continue
with its replacement and do not repeat the classification. Fluent
persists the authorization and operation identity with an explicit
`code-change` disposition, then schedules one corrective Writer with the failed
review, evidence snapshot, and audit artifact; the candidate and prior records
remain unchanged. The Work Item mutation commits the classification and its
audit reference before the audit bytes are published; a retry deterministically
reconciles a missing audit and rejects conflicting bytes. The next action is
`fluent queue resume <work-item> --attempt <attempt>`. While the classified
Writer remains resumable, repeating the exact review, evidence, and
authorization request returns the persisted Writer and audit result without
duplicates; stale or different requests fail before mutation. A passing targeted review
still requires all exact-commit evidence and Learning before landing. The
ordinary `code-change` disposition closes evidence recovery and follows the
normal Writer, Tester, and reviewer path.

Classification admission and Writer-completion checks live in
`src/recovery_contracts.rs`, a neutral boundary consumed by the queue, executor,
and host-evidence modules. Keeping these checks shared prevents a reciprocal
executor/host-evidence dependency while ensuring direct and queued continuations
enforce the same candidate-bound contract.
The queue and executor therefore depend on this boundary without importing the
host-evidence implementation.

For a classified source correction, the executor admits the exact
`classification.json` audit as a correction input. The prompt identifies source
correction as the current action, includes the authorized instructions, and
names the audit that retains the earlier evidence and candidate binding.
`recovery_contracts` checks the audit bytes again when the prompt is built
after reservation. The sandbox grants that file without granting its parent
directory. Missing, changed, or oversized required input stops launch.

If that code-change disposition plans a corrective Writer and the operator
interrupts it before it produces output, replacement evidence remains eligible
only against that exact evidence Review and unchanged clean candidate. Accepting
the replacement marks the output-free Writer `superseded`, retains the failed
evidence and review as history, clears the interruption, and plans a new targeted
Review. Ordinary interrupted Writers, changed candidates, different review
inputs, and live task owners remain ineligible.

For a failed-command recheck, `fluent attempt evidence attach` accepts
`--recheck-artifact` instead of `--review-artifact`. Fluent accepts a passing
document only when its `check` names the exact failed command from the
candidate root, and revalidates every bound byte before Learning. If an
authorized host can run only a related direct command, it may attach a failing
document as diagnostic evidence. Fluent records that command separately from
the immutable recheck command and sends it to a targeted Tests Review; it can
never satisfy the recheck or make the candidate landable by itself. When this
narrow route is not eligible, the immutable recheck artifacts remain a valid
frontier for an explicit Writer extension.

If the Writer-completion gate pauses because a required Plan row is an
authorized-host check, the row is a different evidence frontier: completing it
produces host proof, not a source commit. The handoff names the exact
`plan:<step>` row, resolved command, unchanged candidate, and Writer completion
matrix. The resolver preserves the approved Plan text and accepts either a
direct command reference, an inline code command, or one unambiguous command
embedded in the row's Deliverable or Verification prose. If those fields name
zero or multiple executable commands, Fluent remains paused for an explicit
human decision rather than guessing.
After running that command on the authorized host, create the same schema-
version-1 evidence document and attach it with:

```sh
fluent attempt evidence attach <work-item> <attempt> \
  --candidate <sha> \
  --evidence-file <path> \
  --plan-row plan:step-<n>
```

The external transition records the evidence snapshot, source-matrix digest,
Writer identity, candidate, row, command, and operator reason. A passing
attachment materializes an immutable completion-matrix overlay and schedules
one targeted Tests Review; a failing attachment preserves the failed result
for that Review and cannot complete the Plan row. An identical retry returns
the existing recovery. Once the targeted Review passes, the normal Attempt
loop credits the host-owned row, projects required progress, and continues to
the canonical Tester and Learner without inventing a Writer commit or changing
the approved Plan.

`fluent work-item list` and `fluent work-item show <id>` expose the same durable
Work Item model for inspection. These commands use `.fluent/work/items/`
through the Rust storage model and validate stored objects.
Once a validated Merge Candidate records `Merged` and its merged commit,
`work_status` presents retained Tester evidence as history. It selects the
Merge Candidate's final landing Tester artifact when Learning or rebase
changed the tested commit, or the Attempt Tester artifact when it already
names the merged commit. `verification_assessment` validates the selected
artifact against the exact merged SHA, committed Tester configuration, command
occurrences, logs, summary, and accepted target baseline failures. It also
owns the digest contract for accepted landing evidence and checks a retained
landing intent against that digest. These reads need no disposable source checkout.
A missing or inconsistent result produces an evidence-history diagnostic; it
does not reopen a Writer row or offer an Attempt resume command. Unmerged
candidates, including Complete Attempts that have not landed, continue through
current recovery and verification checks.
`work-item show` includes their current verification assessment, or a
verification diagnostic when that assessment cannot read its required proof.
Status, Work Item inspection, and dashboard assemble snapshots without
replaying pending Work transactions or changing retained evidence.
`fluent status` and `fluent dashboard` use Work Items as the default
operator surface. They read Work Items through `work_status.rs`, which
reduces stored Work Items to operator-facing rows. That boundary chooses
the latest Attempt, the active or waiting Task, the matching Merge
Candidate, and a short action label. A pending candidate is `merge-ready`
only when its Attempt passes the shared Learning advancement gate. A safely
relaunchable Learning state is `learner-not-ready`, whose next action reruns
the Attempt rather than offering land; a non-relaunchable evidence-pending
state is `learner-blocked`. The originating Attempt prints one inspection hint,
but later status and show commands emit no next action for that row, avoiding an
inspection self-loop and allowing other actionable Work to surface. The boundary
returns valid rows and per-file read errors together so one bad Work Item file
does not hide the rest of the queue.
`fluent dashboard` projects those rows into a read-only operator console. Its
snapshot keeps Current Work grouped as Needs you, Running, and Ready, with
Terminal Work available through the All Work view. The console stores selection
by Work Item ID, so a successful refresh preserves a visible selection across
reordering. It uses a wide list-and-detail layout at 100 columns, separate list
and detail panes from 60 columns, and a resize message below the minimum size.
The event loop refreshes the read-only snapshot every two seconds or on request,
retains and labels the last successful snapshot after a read failure, and draws
only when visible state changes. `dashboard.rs` owns terminal setup and the
event loop; `dashboard/snapshot.rs`, `dashboard/app.rs`, and
`dashboard/render.rs` respectively own projection, interaction state, and pure
Ratatui rendering. `dashboard/layout.rs` computes shared wrapping and scroll
bounds without depending on interaction state or rendering. The `e` control
opens a scrollable view of all Work read errors, including errors that overflow
the list. `e` or Esc returns to the preserved selection. Help wraps navigation
instructions and closes with `?` or Esc. Both controls leave Work files unchanged.
Next-action guidance consumes these rows directly. A `merge-ready` row renders
`merge-candidate show` and `merge-candidate land` with both its Work Item ID and
Merge Candidate ID. A ready `attempt run` outcome uses the command's Work Item
ID and the outcome's candidate ID for the same concrete commands.
Write Task prompt generation reads `Task.instructions` from durable Work
state and includes non-empty instructions in the coder prompt. A Task
receives those instructions from explicit Work Item instructions first,
or from rendered Work Item planning context when explicit instructions
are absent. Extra arguments passed after `--` remain coder flags only;
Fluent does not append them as additional prompt text.
`fluent merge-candidate show <work-item-id> <merge-candidate-id>` prints
one stored Merge Candidate as pretty JSON. This command only reads the
boundary object. `fluent merge-candidate land <work-item-id> <merge-candidate-id>`
executes a Merge Candidate that still needs to land: it locks landing and
resolves a trusted baseline for the exact current target, invokes an agent to
rebase the candidate, regenerates provenance, and runs configured pre-merge
checks. After any `fix-pre-merge` commit, it updates provenance to that final tip
and reuses exact final Tester evidence or runs the guarded Tester. Only accepted
results may reach the target-head recheck and fast-forward to the verified SHA.
This rebase-and-check path is the capture route; not every candidate rebases and not
every failed check runs `fix-pre-merge`. A fresh `no-expertise` `Succeeded`
candidate instead lands through the identity-preserving exact-SHA route described
in the no-expertise section below, which never rebases, never regenerates
provenance, and never runs `fix-pre-merge`.

The rebase step is recorded as a `TaskKind::Rebase` Task on the Attempt,
with its own artifact directory and prompt log. The agent runs
`git rebase <target>` inside the candidate workspace, resolving trivial
conflicts inline (additive doc edits, observation files, append-only
state). If the agent cannot resolve a conflict, it writes a diagnostic
to `give-up.md` in its artifact directory and exits non-zero; the Merge
Candidate transitions to `needs-user` with the diagnostic attached. When
the rebase succeeds, Fluent regenerates provenance: it updates the latest
completed Writer's `output.base_commit` to the rebased target commit and
`output.commit` to the candidate tip, that Writer's Attempt artifact reference,
and the Merge Candidate's `candidate_commit` to the new candidate-tip SHA.
Earlier Writer outputs and their matching artifacts retain the commits and
no-change evidence each Writer actually produced. The agent may squash, reorder, reword, or drop redundant
commits during rebase as long as all pre-rebase content changes are
preserved. No project hooks run during the rebase step; `fix-pre-merge`
continues to own post-rebase cleanup. Re-running `fluent merge-candidate land`
after a failed rebase creates a new rebase Task (with an incremented
suffix) and operates on the candidate workspace in its current state.

Landing creates no Reviewer or Attempt Tester Tasks. Its host-owned verification
uses the existing Tester sandbox, Attempt dependency cache, candidate guard, and
failure comparison rules. It prepares that cache under the project cache lock
without reclaiming other caches or reading unrelated Work Items. Task launches
and cleanup retain responsibility for reclamation, so a malformed follow-up
record can report its error after landing without blocking verification.
The guard restores tracked index and worktree changes
after preserving test evidence; it rejects HEAD changes or failed restoration.
The executor rechecks candidate cleanliness and the target head before merging.
A failed verification records merge status `failed`, the exact Tester artifact,
and a repair instruction while retaining the candidate workspace.

Each landing verification uses a separate directory under
`.fluent/work/artifacts/<work-item-id>/<attempt-id>/<candidate-id>/merge/verification-*/`.
The `target-baseline` and `tester` directories contain their Tester results and
command logs. `merge_state.check_artifacts` references these results, or the
original final Tester artifact when reused. Retries preserve earlier evidence.
The completed Attempt's review, Tester, and Learning records remain intact;
landing only adds its existing Rebase Task and updates candidate provenance.

Before either landing route changes the target, Fluent stores `LandingPending`
with a `LandingIntent` in the Work journal. The intent binds the Work Item,
Attempt, Merge Candidate, operation ID, Learner mode, repository and branch,
target worktree, target tip before landing, verified candidate tip, accepted
check and review artifacts, and authorization. Capture records its final tip
after any admitted `fix-pre-merge` change and provenance update;
`no-expertise` records the frozen reviewed Writer SHA. The target fast-forward
then uses that exact intent. A retry with an unchanged target revalidates the
intent, cancellation, evidence, worktree cleanliness, and target tip before
applying the same effect. A retry after Git advanced recognizes the intent's
verified commit in target history, including when later commits followed it,
and publishes the original merged result through the Work journal. It does
not repeat rebase, hooks, or Tester work. An already-applied legacy candidate
without intent cannot gain landing authority from Git ancestry alone. Older
writers reject the new durable state and record version. Use
`fluent merge-candidate land <work-item-id> <merge-candidate-id>` to complete
publication; status and show only inspect it.

After recording the merged state, Fluent removes the managed candidate worktree — unless the
Attempt's Learner run failed and is still retryable, in which case it
retains the workspace so a post-land handoff-only Learner retry has a
chance to run against the merged candidate. If cleanup
fails after the target branch has merged, merge execution prints a
warning and leaves the merged Merge Candidate state intact. Running the
command again
for a Merge Candidate that already has merge status `merged` and a stored
`merged_commit` succeeds idempotently and reports the stored commit
without rebasing, rerunning pre-merge checks, or moving the target branch.
It may finish pending baseline preparation and follow-up recovery.
Merge artifacts live under
`.fluent/work/artifacts/<work-item-id>/<attempt-id>/<candidate-id>/merge/`,
and the stored Merge Candidate records whether execution is pending,
executing, landing-pending, failed, needs-user, or merged.

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
collapse distinct origins or follow-ups. When Fluent finds a V1 operation that
predates hashed identities, recording, replay, cleanup, and failure reporting
reuse its legacy operation path and its already-established Observation and
Work ids. Discovery inventories operation, batch, journal, Observation, and
derived-Work evidence under one landed-origin lock, so it can reconstruct a
missing legacy operation header without creating duplicate effects. Fluent
fails closed on malformed evidence or when both legacy and hashed identities
exist for the same landed origin. Recording and replay hold the same
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
digest-matched anchor. Corrective proposals carry non-empty relative target
paths whose raw spelling already matches their canonical normal components;
absolute paths, parent/current-directory components, repeated separators, and
trailing separators do not pass as lexical aliases, and filesystem path inputs
containing NUL or control characters are rejected before Git or filesystem
access. An `AGENTS.md` authority
must be the closest applicable ancestor for every target, including when a
nested instruction overrides a root instruction.
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
lineage root and limit, audit context, and enqueue intent; unrelated Work is
rejected. Replay and cleanup revalidate that immutable identity even after the
journal records the derived Work. For pre-audit V1 Work records, Fluent verifies
the legacy immutable fields and adds only the missing audit package; it preserves
mutable lifecycle state such as human authorization and its resulting lineage
charge. The
derived Work also retains the accepted expected result, trusted authority,
supporting evidence, unresolved-decision set, follow-up source, and learning
summary. Fluent includes this audit package in corrective task instructions,
so the Work stays executable and inspectable after cleanup removes its origin
Work records and handoff. Cleanup retains the post-land journal as durable
replay and completion evidence. Writer and reviewer prompts render those
instructions. Tester Task records retain them for inspection, while the Tester
runner executes the commands declared in `tester.yaml` without consuming a
prompt. In `propose` mode the Work
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
between persisting the handoff and recording or completing its operation. When
that boundary reports incomplete recovery, `attempt run` emits a
`FollowUpRecoveryPending` outcome instead of claiming the merged candidate is
ready, names the persisted stage and `merge-candidate land` retry action, and
returns a non-zero command status.

After that landed handoff recovery result is durable — complete or incomplete —
an ordered post-land coordinator in `merge-candidate land` evaluates the optional
post-merge review. Ordinary land, and `fluent auto-merge` without its positive
option, keep it off: the land stops after handoff processing and appends, logs,
and spawns nothing. Only when the operator passes `--post-merge-review` does the
coordinator append a post-merge review queue entry and spawn the detached runner,
under the existing debounce, singleton-lease, and corrective-depth rules, and
only on a clean fresh land. An idempotent re-land resumes handoff recovery but
never schedules a new review, and an incomplete handoff result stays retryable
without suppressing an explicitly requested review. The `--no-post-merge-review`
spelling remains the compatibility form of the default, and passing both
spellings is rejected before any durable mutation. A `--runtime fargate` land
translates only an affirmative request, carrying `FLUENT_POST_MERGE_REVIEW=1` to
the runner entrypoint, which passes `--post-merge-review` to the inner land;
Fargate remains compatibility plumbing outside the Local Preview claim.

The Local Preview is Fluent's supervised first-release path: local foreground
Attempts and proposed follow-up Work by default. `fluent work-item authorize`
authorizes and enqueues proposed Work but does not execute it. A human
separately runs `fluent scheduler run`; the scheduler may produce a pending
Merge Candidate but never lands one. Every candidate requires human inspection
after it becomes ready, and only a ready candidate can land. Post-merge review
is off by default and available only as a
positive per-land opt-in. `fluent auto-merge`, automatic scheduler lifecycle,
automatic landing, and Fargate are outside the Local Preview.

Work intake applies two host-owned expansion guards before that execution path.
The planning guard reuses the completion-contract parsers to count approved
behaviors and required Plan rows, then compares the larger count with the layered
`planning.scope-limit` setting (default 12). This avoids asking planners for a
second set of scope labels. Prose, optional rows, and TBD rows do not affect the
result. Intake rejects over-reference planning context before creating the Work
Item or input snapshots. It reports both counts, the configuration source, and
the arithmetic minimum number of policy-sized groups; that minimum is not a
claim about the optimal number of Work Items. `--authorize-large-scope` preserves
an explicit authorization in the Work model.

The same intake boundary reports project-local calibration evidence without
changing the configured reference. It compares the proposed breadth with
successful code Attempts between half and twice that size that passed review and
Learning under the current assessment version. Five nearby Attempts are required
before Fluent reports a one-pass rate plus median reviewed Writer rounds and
recorded active task time. Pre-review Writer continuations do not count as
reviewed corrections. Legacy, distant, paused, failed-review, and failed-Learning
records do not influence the nearby sample; unreadable Work Item records are
omitted with an explicit count. If the eligible records lack task timestamps,
the duration median is reported as unavailable. Otherwise the report names how
many eligible Attempts contributed timing data. Planning guidance uses this
evidence when choosing independently landable Work Items and keeps the
combined-scope override available for work that must remain atomic.

Release Work has a second immutable intake boundary: `--release` requires one or
more `--release-criterion id=statement` arguments before the Work Item exists.
The stored release contract retains those accepted criteria and append-only
finding classifications. `work-item classify-finding` records an unmapped
finding as `proposed-follow-up`; it records `release-blocker` only when
`--blocker-for` names an accepted criterion. Status reports criterion, blocker,
and proposed-follow-up totals alongside the existing review-round, duration,
token, repeated-finding, and artifact-byte metrics. This keeps later discoveries
visible without silently expanding release acceptance.

For an uninitialized project, the bundled full skill detects the missing
`.fluent/` directory and asks separately for a follow-up mode and coder profile.
The curated profiles configure two roles: writer and reviewer.
`codex-balanced` uses Codex `gpt-5.6-terra` at medium effort for both;
`codex-stronger` substitutes `gpt-5.6-sol`; and `claude-balanced` uses Claude's
provider-default model at medium effort. `codex-write-claude-review` and
`claude-write-codex-review` use the balanced settings for the named providers.
Explicit `--write-*` and `--review-*` values overlay individual curated fields.
A custom profile requires the coder, model, and effort for both roles. The skill
invokes one configured `fluent init` command only after every choice is complete.

Configured init validates the complete argument combination, confirms the Git
repository root, and asks Git whether its effective ignore rules hide any
required durable Fluent path. `setup.rs` owns the durable path contract;
`git.rs` obtains source, line, pattern, and path records from `git check-ignore`
without reproducing Git's repository, nested, info-exclude, global, or negation
precedence. A conflict fails before the host-sandbox check, provider access, or
filesystem mutation. The diagnostic recommends Fluent's generated
`.fluent/.gitignore`, which ignores runtime state while keeping Expertise,
hooks, the project Dockerfile, and Tester contract files trackable.

After that gate, configured init preflights the enclosing host and every
distinct selected provider before it creates `.fluent/`, installs skills, seeds
instructions, or writes configuration. The shared admission boundary checks
command availability and local authentication without model inference. In
addition, configured init requires one bounded live response from every
distinct selected Claude model. That tool-free canary verifies that the exact
model can answer at setup time; it neither reserves nor guarantees future
provider capacity. Codex configured init remains authentication-only. The
configuration writer parses the existing YAML, updates only the two role
mappings and (for execute) `follow-up.mode`, then atomically replaces the file.
It preserves unrelated keys and never leaves a partial role mapping. `propose`
adds no follow-up mapping. Coder configuration and persisted coder mappings
reject the retired `behavior-tests` key instead of discarding or translating
it. Bare `fluent init` remains compatible with existing non-interactive callers
and therefore retains the legacy implicit-default path.

Execute mode authorizes and queues corrective Work; it still requires a
separately started scheduler and human candidate landing.
Initialization creates or updates the managed Fluent block in `AGENTS.md` and
`CLAUDE.md` without committing it. It skips byte-identical rewrites, then checks
the resulting Git state and names each instruction file the user must commit or
revert before an Attempt. This handoff and the initial Writer preflight enforce
the same rule: candidate worktrees start from committed Git state.

`fluent cleanup` takes the same land lock as land and post-land recovery, then
re-reads each candidate before applying its plan. It retains an origin — its
Work Item, Attempt, Merge Candidate, worktree, and managed artifacts — while
landed-learning recovery is still live. This includes a failed or
legacy-missing Learner record, successful Learning with no recorded operation,
a recorded follow-up failure, and any missing or incomplete operation evidence.
A top-level completed bit is not enough: the follow-up module verifies the
operation and batch identity and digests, journal identity, one complete receipt
per follow-up, provenance Observation, derived Work, and required queue
dispatch. Land retains the candidate worktree while recovery needs it. The
shared recovery boundary removes it after Learning and materialization complete,
then clears the recovery failure; cleanup also prunes a stale Git worktree
registration when its checkout directory has already disappeared. Derived
Observations and Work Items stay inspectable with self-contained corrective
context and provenance identifiers after optional origin artifacts are gone.
After a successful land, guidance names the global `fluent cleanup` command
without an identifier. Cleanup remains a dry run unless the operator passes
`--apply`.

Fluent stores each Work Item as a top-level record plus split Attempt, Task, and
Merge Candidate records. Lifecycle preparation and writes share one per-Work-Item
model lock. Before changing split records, a writer records the complete target
snapshot in a durable transaction; it publishes child records first and the
new top-level storage revision last. Lifecycle preparation recovers interrupted
transactions, including creations. Status and dashboard instead use
`WorkModelStore::snapshot_work_items` to assemble published records without
model locks, recovery, or artifact migration. They keep readable items visible
and report pending host-owned recovery and item-local failures. Each snapshot
checks the root revision and journal before and after assembling children,
retrying changed publication markers at most three times. New journals record
a digest of the base Attempt, Task, and Merge Candidate values. During a pending
transaction, a reader compares stored children with that base digest or, after
the target root has published, with the journal's target children. It never
substitutes journal contents for stored records. An interrupted child update or
a legacy pending journal without enough proof produces an item-local unavailable
diagnostic until host-owned recovery completes. Only a missing journal or child
collection counts as absent; lookup, read, and metadata errors make that item
unavailable instead of producing a partial aggregate. Dashboard
derives its latest transition in memory from that same published snapshot;
it does not persist a cursor. This prevents mixed-generation reads and makes a failed or crashed
write retryable without allowing a stale snapshot to overwrite its successor.
Storage-revision exhaustion fails closed. Artifact migration runs only at lifecycle writes
and explicit upgrade. `fluent work-item upgrade` records converted source and
destination paths before moving artifacts and publishes the rewritten references
through the aggregate transaction, including when no record schema changes.

A Work Item's Learner mode selects how its pre-land Learner runs. The mode is a
serde-defaulted `LearnerMode` on the Work Item — `capture` or `no-expertise`.
The default `capture` value is omitted from the split record; writes still stamp
`schema_version` and `model_writer_version`. The command
`fluent work-item create --learner-mode` sets the policy once, and every Attempt,
retry, and host-side Fargate preflight reads it. The
orchestration resolves a crate-private execution mode from the stored policy and
the candidate's merge state: an unmerged `capture` policy runs the capture path
below, an unmerged `no-expertise` policy runs the isolated no-expertise path, and
any merged candidate always runs post-land handoff-only regardless of the stored
policy. The public `LearnerRunInputs { handoff_only: bool }` surface stays
source-compatible and never expresses no-expertise, which is reachable only
through the production adapter.

A `capture` pre-land Learner runs inside one host-owned Git transaction over the
managed candidate workspace, so a successful run leaves exactly one committed,
expertise-only result and a clean workspace, and any rejection restores the
baseline. The transaction first verifies the workspace `HEAD` equals the Learner
baseline with a clean index and worktree; a dirty entry launches no coder, moves
no pointer, and settles relaunchable failed Learning. It stays open across the
initial coder, every bounded schema repair, and final draft validation.

Right after each coder invocation returns — before its result is inspected,
its draft published, or another invocation launched — the host pins that
return's `HEAD`, reachable per-commit paths, and staged, unstaged, and
untracked paths into a cumulative ledger a later reset cannot erase. At
finalization it classifies the union of every retained snapshot plus the final
pre-normalization state using raw NUL-delimited paths, so a newline-bearing
name stays one path and a non-UTF-8 name is rejected rather than decoded
lossily. A snapshot whose `HEAD` is not an unambiguous linear descendant of the
baseline is unaccountable, and any accounted path outside
`.fluent/expertise/` — including one a later commit or repair reverted or reset
away — rejects the whole result.

When every accounted path is expertise, the host moves `HEAD` and the index back
to the baseline while retaining the final worktree, stages the complete
`.fluent/expertise/` delta including deletions, and authors at most one commit
with subject `Update expertise` whose sole parent is the baseline; a result
with no net expertise delta keeps the baseline without an empty commit. The
Write output and Merge Candidate pointers move only after the canonical result
is verified exactly clean. When a no-change Writer produced that output, the same
in-memory aggregate preserves its verified `base_commit` and records the exact
Learner commit transition before the finalizer first persists `HandoffPending`.
The handoff is written last. Any pre-acceptance
failure runs one restoring rollback that hard-resets and cleans back to the
proven baseline; an obstructed rollback composes a distinct restoration
diagnostic under the primary without changing its typed kind or relaunchability.
Every ordinary completed-call rejection settles terminal failed Learning and
withholds Merge Candidate readiness. If canonical-handoff publication fails
after a clean successor is verified, that successor is retained as both
pointers while Learning stays relaunchable `Generic` failed with no handoff
reference; a canonicalized no-change output retains its original verified commit
and typed transition in that recovery state.

A `no-expertise` pre-land Learner instead runs from an isolated snapshot of the
reviewed accepted change, baselined on the reviewed Writer commit rather than the
live workspace, and reuses the same host-enforced confinement as post-land
recovery: it forces the trusted Seatbelt boundary even when the caller passed
`--no-sandbox`, grants writes only to its managed handoff staging surface and a
per-launch private temporary directory, and denies writes to project expertise,
the candidate workspace, the live checkout, shared Git metadata, and shared
temporary roots.

Because it must keep the reviewed commit exact, the host owns a
pointer-identity gate around the run. Before launching the coder it requires
candidate `HEAD`, the Merge Candidate commit, the latest Write-output commit, and
every Tester and built-in reviewer context in the final passing review round to
name the reviewed Writer SHA with a clean worktree, reading only that final round
and treating earlier corrective rounds as historical evidence; a failure launches
no coder. It pins every coder invocation's return into a ledger that rejects any
candidate Git mutation — a source or expertise-only commit, a staged, unstaged,
or untracked change, or one a later invocation reverted — rather than
normalizing it.

Unlike capture, which moves its canonical pointers through the host-owned Git
transaction and one whole-aggregate finalizer, a `no-expertise` Learner settles
every terminal transition through a fresh, lock-held field-level mutation that
re-reads the current aggregate, accepts only this runner's exact Learning
frontier, and changes only the Learning record. It settles across two lock-held
phases. The first lock-held phase runs after the coder returns: it repeats the
identity and cleanliness check against the freshly re-read pointers and
final-round contexts and, in the same atomic step, advances Learning `InProgress`
to `HandoffPending` — a durable, crash-observable preparation — or settles it
`Failed`. The second lock-held phase is one transaction that re-finds the Attempt,
requires its exact `HandoffPending` frontier and landing eligibility, re-runs the
full final identity check against the current aggregate and candidate Git, and
only then publishes the canonical handoff while the model lock is still held and
settles the same aggregate to `Succeeded` (with the digest-bearing handoff
reference) or a typed `Failed`. Because the final check, publication, and
settlement share one model-mutation boundary, no supported concurrent Work-model
transition can interleave between them. Because each transition reads and writes
the same current aggregate, a concurrent Work-model change during the coder run
neither strands Learning `InProgress` on a stale write nor is overwritten by a
pre-run snapshot, and the Attempt call sites skip their post-Learner
whole-aggregate write for this mode. The first phase is
`prepare_no_expertise_handoff` and the second is `publish_no_expertise_handoff`;
capture-mode landing keeps its whole-aggregate finalizer.

Serialization alone is insufficient, so while a `no-expertise` Attempt's Learning
is `HandoffPending` or `Succeeded` a central reviewed-identity transition guard
rejects any supported write that would move its frozen reviewed tuple — the latest
completed Write task or commit, the selected Merge Candidate id or commit, a
final-round Tester or reviewer context, or the no-expertise policy — or persist a
`merged_commit` other than that frozen reviewed Writer SHA, so a pointer update
queued behind the publication lock cannot stale the settled handoff after the lock
is released. A change while Learning is `InProgress` stays detectable by the
postflight, and a relaunchable `Failed` record stays repairable.

If the canonical handoff is written but the final Work-model transaction fails,
recovery honestly reads one of two transaction-journal outcomes. When no terminal
journal became durable, the previously durable `HandoffPending` frontier stands,
recovery treats any unreferenced canonical file as untrusted, reruns only the
Learner, revalidates the current reviewed identity, and atomically replaces that
file with a newly validated handoff only after the new final check passes. When
the terminal journal became durable before model application or journal removal
finished, the next supported recovery read finishes that exact journal to
`Succeeded` with its exact digest-bearing handoff reference, without rerunning the
Learner. The failing call itself reports no readiness in either case.

The model-lock transaction makes every *supported persisted-state* transition
atomic: the pass/fail decision and the Learning write read and write the same
lock-held aggregate, so no supported concurrent Work-model change can interleave
between them. That guarantee covers persisted state, not candidate Git. The
postflight's final read of candidate `HEAD` and the worktree is a point-in-time
observation, so an arbitrary out-of-band process that edits candidate Git *after*
that last read is a residual race the model lock cannot serialize. Eliminating it
would require a universal lock shared by every candidate-workspace Git writer, which
is outside this release correction and remains separate work.

A fresh, unmerged `no-expertise` `Succeeded` candidate lands through an
identity-preserving exact-SHA route rather than the capture rebase. It skips
rebase and provenance regeneration, requires the live candidate clean at the
frozen reviewed Writer SHA and the target head an ancestor of it, and runs
`check-pre-merge` — never `fix-pre-merge` — only in a disposable exact-SHA
worktree before fast-forwarding the target to exactly that SHA. Removing that
disposable worktree is a land precondition: a passing check whose worktree cannot
be removed fails the land before the second precondition check or any target Git
mutation, retaining at most the isolated worktree for cleanup. The route enforces
the same target-worktree cleanliness policy capture landing uses, both at the
initial preflight before any side effect and again immediately before the
fast-forward. Both the capture and exact-SHA routes feed their landed outcome
through one shared follow-up-processing and scheduling coordinator
(`finish_fresh_land_with`), which schedules the optional post-merge review only
after the landed follow-up result is durably recorded and schedules nothing when
that persistence is unknown, always returning the successful landed outcome
unchanged.

A `no-expertise` launch prepares its sandboxed host through the injected
`HostPreparation` seam. Production always runs the fixed
`HostPreparation::Production` prerequisite, credential, and signing steps; a
hermetic launch-route test replaces them with the `#[cfg(test)]`-only
`HostPreparation::Injected` variant to assert the same launch invariant in both
host branches without real host side effects.

An identity or cleanliness contradiction, a candidate Git mutation, or an untyped
handoff-publication failure fails closed with relaunchable `Generic` Learning,
while a typed publisher failure instead preserves the `LearningFailureKind`
derived from its concrete cause — `EvidencePending` when the Learner coder already
ran (a supervision-sidecar fault that is non-relaunchable and takes precedence) and
`TranscriptPump` for a transcript-pump publication fault — so not every
handoff-publication failure is `Generic`. Every such settlement leaves every live
and persisted candidate pointer unchanged and withholds readiness; it never
overwrites the live candidate, so a pointer an identity check found already off
the reviewed Writer SHA is preserved as evidence rather than repaired back onto
it. The handoff carries no expertise references, and missing durable knowledge is
proposed only as non-corrective, Observation-eligible follow-up material. A `no-expertise` Attempt is local-only:
`fluent attempt run --runtime fargate` refuses it on the host before any launch
side effect — credential access, bootstrap, image or Docker/AWS commands,
upload, ECS submission, or runtime-state writes — and directs the operator to a
trusted macOS host, and a local host that cannot enforce the boundary fails
closed without downgrading the policy.

A relaunchable Learner run that failed before its candidate landed recovers
through `fluent attempt run`, which retries only the Learner. An
`EvidencePending` failure is different: because its coder already ran, a later
Attempt run preserves the non-relaunchable record and launches no coder while
human recovery remains required. When a relaunchable failure's candidate has
already merged, the retry runs in handoff-only mode: it serializes against land
on the land lock, denies expertise writes, and discards any commit it makes, so
the merged commit and target branch stay unchanged.
When the command carries explicit coder, model, or effort flags, it persists the
changed coder mapping through a fresh field-level Work-model mutation under that
land lock; a flagless recovery leaves the mapping unchanged. It never writes a
whole Work Item snapshot captured before the serialization boundary.
After taking the lock, retry re-reads the Attempt and candidate and skips the
coder when another retry has already persisted success. It never repairs or
resets the retained candidate before isolation, so malformed or concurrently
changed live Git state cannot be overwritten. The Learner diff uses the
persisted accepted base. For a
legacy merged TaskOutput without that field, recovery reads the retained
candidate's rebase reflog only when it contains one intact session whose start
names the recorded target branch and whose finish returns to the retained
candidate ref. Both endpoints must be ancestors of the stored merged commit,
which includes accepted rebase rewrites and merge-fix commits while excluding
target-only history. Missing, partial, expired, or repeated sessions fail
closed; recovery never guesses the base by counting commits.
Before invoking the coder, Fluent bundles the merged repository into a
disposable directory, clones that bundle without hard links, removes the
remote, and deletes clone reflogs. It creates a sibling temporary project for
the draft. The prompt, diff command, current directory, environment, draft
path, and readable Git metadata name only isolated paths. The live target
checkout, refs, index, dirty files, retained candidate, and shared Git directory
stay outside the Learner's writable surface. A handoff-only run always invokes
the absolute system Seatbelt launcher, even when the caller uses
`--no-sandbox`; if the host cannot enforce that profile, the command records
failed Learning and returns a recovery-pending failure instead of reporting a
ready candidate. Before entering the sandbox — the last trusted host boundary —
Fluent hydrates the selected coder's supported credentials from the host
credential store, which the sandbox itself denies. The profile exposes isolated
Git data read-only, strips the shared `/private/tmp` and per-user
`/private/var/folders` temporary-tree write grants on every effectively
sandboxed branch, and explicitly denies every live root while allowing only the
coder's staging directory. In place of the shared temp trees, each coder launch
receives a distinct, mode-0700 private temporary directory created beneath its
staging scope and held alive for the launch. Claude 2.1.220's Bash tool creates
command state under `/private/tmp/claude-<uid>/<workspace-key>/` even when
`TMPDIR` points at that private directory, so a Claude Learner additionally gets
write access to only its current workspace key. Sibling workspace keys and the
rest of `/private/tmp` remain denied. Fluent launches the coder in its
own process group and kills surviving descendants before it imports the draft.
It then opens the draft without following the final component, requires one
bounded regular inode with no hardlink aliases, and writes through a retained
project-directory handle after rejecting symlinked ancestors. Review and Tester
artifacts cross the isolation boundary under the same regular-file, identity,
confinement, and size rules; a declared missing artifact fails the run.
Candidate commits, staged files, unstaged files, and untracked files are
collected as denied paths inside the disposable clone, which is removed after
the host stamps the handoff.

Each Learner invocation allocates a collision-safe, host-owned run directory
under `.fluent/work/artifacts/<work-item-id>/<attempt-id>/learner/runs/run-<N>`,
its index taken from on-disk state so a lost or absent in-memory Learning record
can never reuse an earlier run identity. That directory holds the host-written
run evidence — the coder transcript, the immutable submitted-draft snapshot, any
recorded normalizations, and, on a rejection, the full validation error. It is a
non-writable sibling of the coder's staging directory, so the coder cannot
replace, delete, or truncate its own run evidence. A refreshing auth retry
preserves the pre-refresh transcript phase with a create-new write that
propagates failures, never an overwrite-capable copy.

Before it stamps a handoff, the host normalizes the untrusted draft toward
Observation-only, only ever moving a follow-up away from corrective authority:
malformed optional artifact evidence is dropped, and malformed, incomplete, or
unsupported corrective metadata downgrades the follow-up to a plain Observation,
each change recorded on the run surface. A single malformed optional field
therefore cannot reject the whole draft, and no normalization can manufacture
executable Work. If the normalized draft still fails schema validation and the
configured repair budget (project-over-user layering, default two) remains,
Fluent re-invokes the coder with the rejected draft and exact error as a bounded
schema repair — not a fresh audit — under a fresh run identity, and accepts the
corrected draft only when it preserves every prior follow-up id and its
non-schema content. The prompt keeps the artifact-evidence array empty until the
handoff transport can publish referenced evidence artifacts.

The current release keeps the pre-land and post-land schema-repair loops
separate. They share the draft-validation protocol and repair budget,
but enforce different Git boundaries. Pre-land must capture every coder
return in the Learner Git transaction before inspecting the result or
publishing its draft, and it cannot attach expertise references until the
host has normalized the canonical result. Post-land runs in a disposable
handoff-only clone, cannot change landed state, and stamps the expertise
references already produced by confinement. A shared loop would have to
hide these ordering requirements behind callbacks. Fluent accepts the
duplicated repair plumbing for now; unifying it should be a separate
refactor with tests that prove both Git boundaries.

Land takes that lock before it reads or mutates candidate state, resolves
workspaces, or checks cleanliness, and holds it through merge finalization and
follow-up recovery. A retry therefore cannot make land observe its transient
index or worktree state.
Durable knowledge it could not write to expertise is recorded as a
non-corrective follow-up, which materializes as an Observation only. The
recovered handoff is then materialized immediately under the same
land-gated, idempotent rules the land hook uses.

Landing follows one deterministic verification sequence after rebase. A failed
check or Tester run stops before fast-forward and leaves evidence on the Merge
Candidate. After repair, a later land invocation starts from the retained
candidate. It does not run a merge-time Writer or Reviewer loop.

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
before returning a typed rate-limit-exhaustion error. Task orchestration never
retries that typed result. A `RateLimitState` tracker fires
macOS notifications (`osascript`) on state transitions: once on
entering rate-limit state (naming the reason and expected resume
time) and once on leaving (after the first successful invocation
following a pause). Repeated retries within the same pause do not
fire additional enter-state notifications.

Provider wait has two observation points because two processes own retries.
Fluent's wrapper writes each outer rate-limit sleep to `provider-timing.json` in
the role artifact directory. While a provider child remains alive, the transcript
pump recognizes Claude Code `system/api_retry` records and writes their declared
delay and observed elapsed wait to `transcript-pump.json`. The pump persists an
expected resume time as soon as it sees the record, so status can show
`provider-wait` while no further bytes arrive. The next provider record or pump
completion closes the interval, capped at the provider's declared delay. These
documents are diagnostics, not retry authority. Before an authentication or
rate-limit relaunch replaces pump status, Fluent adds the completed phase's pump
timing to the role-owned sidecar. The recorded phase number prevents a resumed
handoff from counting the same wait twice. Malformed or unknown records do not
change execution.

The same preserved-phase mechanism records safe Claude no-progress timeouts.
Its backoff is included in the role-owned provider timing sidecar, so status and
duration reports do not misclassify that wait as active model work.
After the configured relaunch budget is exhausted, Task orchestration records
the affected Task and Attempt as `needs-user` / `provider-unavailable`. The
handoff names `fluent attempt run` as the manual same-Attempt recovery. Fluent
never switches providers automatically.

Author and reviewer Tasks inherit this
behavior without further plumbing.

`provider_evidence.rs` owns the fail-closed evidence boundary after bounded
provider retries exhaust. Current Task execution requires every numbered retry
transcript and the terminal transcript to contain only the provider's accepted
launch prelude and structured rate-limit terminal record. A missing phase,
malformed or unknown event, model output, or tool activity prevents the typed
`provider-unavailable` pause.

The Attempt loop has a separate compatibility path for historical scheduler
Attempts paused at `round-cap`. It accepts only the preserved single-file Claude
grammar: one initialization event, ten ordered and session-consistent
`api_retry` events, the exact synthetic zero-token connection-refused assistant
record, and the matching zero-cost error result. It requires every failed Task
to be a Review Task and rejects any existing `review.md`. The parser compares
the complete allow-listed event structure after normalizing only paths,
identifiers, and timing metadata; changed terminal semantics or any token,
thinking, tool, permission, model-usage, cost, or usage evidence fails closed.
When all failed reviews match, one lock-held Work-model mutation changes only
those Tasks and the pause kind before the normal same-Attempt resume path runs.
Current Task execution never calls this compatibility parser, so a newly
produced single-file transcript remains insufficient provider evidence.

| Env var | Default | Purpose |
|---|---|---|
| `FLUENT_RATE_LIMIT_RETRY_AFTER_SECS` | 1800 | Fallback wait when no structured timing is available |
| `FLUENT_RATE_LIMIT_JITTER_MAX_SECONDS` | 30 | Maximum per-run jitter added to the retry wait |

Autonomous Claude workers run with Claude's `--safe-mode` flag. This disables
user, project, and local customizations, including SessionStart and Stop hooks,
while preserving the selected model, subscription authentication, built-in
tools, and Fluent's Seatbelt sandbox. Intentionally interactive Claude sessions
do not receive that flag and retain user customizations.

Captured provider launches do not inherit Fluent's scheduler stderr descriptor.
Fluent pre-opens `provider-stderr.log` beside the task transcript and gives that
task-owned descriptor to the provider. This preserves diagnostics while keeping
Claude/Bun startup independent of whether the scheduler's own log path is inside
the worker's Seatbelt roots. Learner profiles grant read access to the exact
host-owned run-evidence directory so Claude can inspect that descriptor, while
keeping the directory outside the Learner's writable `staging` subtree. Optional
autonomous calls with no transcript use stable null device streams. A Claude/Bun
`fstat` denial while constructing an inherited standard stream is classified as
a resumable `host-sandbox` launch failure rather than a Writer round cap.

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

When that expiry check or a transcript 401 requires a refresh,
`credential::refresh_credentials()` runs a minimal host-side Claude probe in
safe mode. The same bounded process runner supports configured init's live
Claude canary. Each probe owns a process group; on deadline expiry, Fluent
attempts to terminate and reap the owned group, including its descendants. A
verified sweep returns a timeout, while an unconfirmed sweep returns a cleanup
failure that retains its diagnostic. Fluent captures command output in
temporary files so a full pipe cannot deadlock the probe, reads a bounded prefix
after failure, and redacts credential-shaped values. The refresh probe uses
`FLUENT_CLAUDE_REFRESH_DEADLINE_SECS` and rereads the Keychain only after a
successful zero exit. The configured-init canary uses
`FLUENT_CLAUDE_READINESS_DEADLINE_SECS`; both deadlines default to 30 seconds
and must be positive.

`claude_diagnostics.rs` classifies readiness and refresh failures as
authentication, host credential boundary, rate limit, network, provider
availability, timeout, or unknown. Each category has a stable
`FLUENT_CLAUDE_*` code and a specific repair. Only authentication evidence
names `claude /login`. A configured `fluent init` failure tells the user to
retry the same initialization command; a Task failure records a resumable
same-Attempt pause and names `fluent attempt run`. This keeps a provider outage
or an enclosing sandbox's denied Keychain access from masquerading as a bad
login. Readiness treats valid `claude auth status --json` output as the
authoritative status even when the command exits nonzero because Claude can
return a logged-out JSON status with exit code 1. For a logged-out status,
Fluent probes the Claude Code Keychain entry to distinguish an inaccessible
host credential boundary from an actual logout. Before the Claude readiness
probe, it runs the same host Keychain-to-environment bridge used by
managed Claude workers. On macOS, the bridge derives the login Keychain from the
effective Unix account record instead of the launch's `HOME`, so an isolated
worker home does not turn a healthy host login into a false logged-out result.

The coder returns a typed `AuthError` (via `anyhow::Error::new`) instead of a plain
string `bail!`, so callers can recover the type via
`downcast_ref::<AuthError>()`. Both task retry loops in the task
executor recognize `AuthError`, skip retries entirely, and escalate
immediately to `needs-user` with the auth-specific handoff message
from `AuthError::user_message()`. Typed rate-limit exhaustion follows the same
no-outer-retry rule; other coder errors still retry up to
`FLUENT_MAX_TASK_RETRIES`.

The first Writer starts from the committed candidate base without a separate
project-model generation step. If `.fluent/expertise/INDEX.md` already exists
in that committed state, the Writer reads it; otherwise the Writer orients from
the project and general expertise directly. A Writer may change project
expertise when that change belongs to its planned work. After reviews pass, the
Learner may also create or update project expertise. Fluent does not generate a
separate orientation commit before the Writer, so orientation cannot expand the
Work Item's pre-review candidate scope.

`fluent cleanup` owns the terminal Work model cleanup lifecycle. It
defaults to a dry run and only mutates state with `--apply`. A Work Item
passes the lifecycle check when every Attempt is terminal, every Task in
those Attempts is terminal, and every Merge Candidate is terminal. Operators
can also run `fluent work-item abandon <work-item-id> [--reason <text>]` to mark a
stale Work Item as intentionally abandoned; cleanup treats that marker as
terminal only when no Attempt is executing or reviewing, no Task is
executing, and no Merge Candidate is reviewing or merging. Cleanup also
requires candidate worktrees to be clean and every existing Work branch and
candidate HEAD to be provably integrated into its recorded target branch.
It retains the whole Work Item if any owned source remains unmerged, dirty,
or unverifiable;
see [Cleanup](#cleanup) for the source checks. Cleanup removes
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
| Task | Schedulable unit of work. Task kinds stay generic: `write`, `review`, `merge`, `report`, `learn`, `probe`, `tester`, and `verification`. Roles carry prompt and domain behavior. |
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
`learn`, `probe`, or `tester`. Any other value is rejected.
`workspace_access.reads` may list any number of
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
their artifact directory but do not write a transcript. Reviewer sandboxes
intentionally exclude writer artifact directories and other reviewers'
artifact directories to preserve independent verification. They grant each
exact input file named in the reviewer prompt, including the Writer transcript,
completion matrix, and progress record. An exact-file grant does not open its
producer directory or unnamed siblings. Before rendering either a Writer or
Reviewer profile, Fluent canonicalizes each declared exact-file input. A
symlink adds only its resolved regular-file target when that target remains
under the canonical project root. Missing targets, cycles, directories, and
targets outside the project fail closed; Fluent never substitutes a parent
directory grant.

### Paused Attempt records

Every persisted `needs-user` Attempt may carry one versioned pause record. The
record is the shared source for the blocker, the candidate and Task involved,
the evidence or decision still needed, and either one exact public recovery
command or an explicit explanation that no safe command is currently eligible.
The Work-model store refreshes this record whenever it writes a Work Item and
clears it when the Attempt resumes or reaches another terminal state. Older
Attempts without the field remain readable and use the existing compatibility
derivation until their next write.

`fluent status`, `work-item show`, resume errors, and needs-user handoffs use the
same saved record. Before a command is shown, Fluent checks that the Attempt's
pause kind, candidate, and blocking Task still match the record; if they do not,
the old command is suppressed and a current operation is derived instead. This
keeps a handoff from directing an operator to a transition that the current
Attempt would reject.

### Public recovery operation records

Every public recovery that changes a paused Attempt also records one append-only
recovery operation on that Attempt. The record names the operation, Work Item,
Attempt, candidate, relevant Task or evidence, authorization text, and a digest
of the immutable inputs. The recovery checks those identities before it mutates
the Work Item, then performs its specialized artifact and Git checks inside the
same model transaction. It records the resulting Attempt status and the next
action after the change.

The operation identity is stable for the complete request. Repeating the exact
request returns the saved result and does not add another Task, evidence
attachment, candidate, or audit record. A changed candidate, Task, evidence
digest, workspace, or authorization gets a clear rejection before mutation.
Existing recovery artifacts remain the detailed evidence; this shared record
connects them to the public operation and gives status and later recovery code a
single history to inspect. The field is optional when reading older Attempts so
old model records remain readable.

### Offline recovery replay corpus

The repository keeps a small versioned corpus at
`tests/fixtures/recovery-corpus.json`. Its cases are sanitized descriptions of
real recovery failures: they use synthetic Work Item and commit IDs and never
copy a checkout, provider transcript, host path, or credential. Each Attempt
case is replayed without launching a coder. The replay creates the minimum
candidate-bound model, checks the exact operation through the shared validator,
repeats it to verify idempotence, rejects a stale candidate, and verifies that
the original Task history and operation record survive serialization. Workflow
cases also check their public reconciliation command and preserved history.

This corpus complements, rather than replaces, focused recovery tests. Focused
tests cover each recovery's Git and artifact rules; the corpus checks the common
operator promise across all of them: a saved failure has a usable command,
stale input is rejected, history is retained, and the same Attempt can be
continued without provider access.

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
`WorkModelStore::create_work_item_with_inputs` owns input intake and Work Item
publication under the same per-item `model.lock`. It validates and reads the
approved managed sources before taking the lock, then installs snapshots and
authors the recoverable Work-model transaction while holding it. A competing
creator cannot install or remove that Work Item's snapshot tree. If storage
fails before it authors a transaction, the creator removes only its unpublished
tree. If the item record or recovery journal became durable, it retains the
snapshots so transaction recovery cannot publish references to missing inputs.
Every Writer and reviewer Task also copies the Work Item's preserved input
references into `input_artifacts`. Task construction deduplicates those
references against Attempt-generated inputs. Reviewer-role selection continues
to derive roles only from completed review Task producer ids, so the Work Item
producer marker cannot widen a targeted review round. Prompt rendering lists
preserved inputs separately from prior reviews. The executor resolves the
snapshots through the existing managed-input preflight and grants their
isolated `inputs/` parent read-only; it does not grant the broader
`.fluent/work` tree or any Attempt artifact sibling.
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

Each Work Item root record stores `model_writer_version` as a validated writer
envelope: `{"format":"fluent-work-item-writer","schema_version":9,"version":"0.5.0"}`.
The public in-memory value remains an optional package-version string. Current
Fluent also accepts legacy strings without rewriting them during observation.
The envelope occupies an existing field that older readers deserialize as
`Option<String>`, so pinned stable and assignment-era readers reject current
records before they can rewrite split records or recover their journals. Current
Fluent similarly rejects the unsupported intervening envelope and transaction
schemas rather than guessing their assignment-aware meaning. Adding an unknown
top-level schema field alone cannot fence a deserializer that ignores unknown
fields.
The offline check in `tests/behaviors/operations/test-work-model-old-reader-fence.sh`
builds the pinned v0.3.0 release source, checks its source and executable hashes,
and compares the entire Work tree before and after attempted mutation, with and
without a pending journal. `PauseKind` uses a string-preserving
decoder: known values become typed variants, while an unrecognized value remains
an `Unknown(String)` and serializes back to the exact same string. Assembly and
validation therefore remain available to read-only status, list, and show
commands. Those surfaces display the unknown value and an upgrade warning; they
also warn when the recorded writer semantic version is newer than the installed
binary. Mutation compatibility is a separate gate, checked before Task or
Attempt execution, landing, locked model reducers, ordinary model writes, and
cleanup eligibility. It rejects an unknown pause or newer writer version before
candidate, artifact, Git, or model effects, preventing an older binary from
silently normalizing state it does not understand.

The split Work Item and Attempt records also carry a storage schema version.
Records from before that marker read as legacy version 0, but a newer unknown
version is rejected rather than guessed. `fluent work-item upgrade [id]` is the
explicit conversion boundary: it verifies the stored versions and writer
version, records the source and destination in an immutable audit file under
`artifacts/<work-item-id>/model-upgrades/`, and rewrites the Work Item, Attempts,
Tasks, and Merge Candidates through the normal transaction. Running it again
after success is a no-op. The command accepts an optional ID; without one it
processes each stored Work Item independently, so a future-format record is
reported instead of being silently changed.

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
      inputs/
        <ordinal>-<source-filename>
      <attempt-id>/
        <task-id>/
        <merge-candidate-id>/merge/
```

Each file in `items/` stores Work Item metadata and planning context:
the Work Item id, title, optional explicit instructions, optional
brief/behaviors/approach/plan context, and optional abandonment marker
with the operator-provided reason. It also stores optional immutable input
identities; legacy records omit the field and deserialize to an empty input
list. Attempts live in
`attempts/<work-item-id>/<attempt-id>.json`, Tasks live in
`tasks/<work-item-id>/<attempt-id>/<task-id>.json`, and Merge Candidates
live in `merge-candidates/<work-item-id>/<merge-candidate-id>.json`.
`WorkModelStore` assembles those split records into the public
`WorkItem` shape from `fluent::work_model` for `fluent work-item show`,
status, dashboard, task execution, review, merge, and cleanup.
Each assembled Work Item carries the storage revision observed in its top-level
record. Aggregate writes take a per-item `model.lock`, compare that revision,
and increment it before replacing metadata or split records. A stale writer
fails before it can revert a concurrent field change or prune a newly added
Attempt, Task, or Merge Candidate. Legacy top-level records omit the revision
and read as revision zero.
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
When `WorkModelStore` reads stored Work state, it retains older artifact
references such as `.fluent/work/artifacts/<attempt-id>/...`. Status and
dashboard resolve these stored references at their legacy locations.
Lifecycle writes and explicit `fluent work-item upgrade` normalize the
references into the current
`.fluent/work/artifacts/<work-item-id>/<attempt-id>/...` form. If the older
path exists on disk and the namespaced path does not, these mutating
boundaries move that artifact directory or file into the Work Item
namespace. Explicit upgrade records the source and destination paths in
its audit before moving artifacts.

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
`check-pre-merge`, and verifies the final commit through the Tester. Landing
continues only when the checks and exact Tester comparison pass. The
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

`scripts/release.sh` runs on either supported macOS architecture and builds
both `aarch64-apple-darwin` and `x86_64-apple-darwin`. The release host must
have both Rust targets installed with `rustup target add
aarch64-apple-darwin x86_64-apple-darwin`. The script verifies each staged
binary's Mach-O architecture with `lipo`, applies and verifies an ad-hoc
signature, then computes a separate SHA-256 file. It calls `gh release create`
once with both binaries and both checksums, so a build, architecture, signature,
checksum, or release-gate failure stops before publication.

Publication also requires a clean `origin/main` commit, a new tag, and committed
`documentation/releases/<tag>.md` notes. Formatting, compilation, and the exact
configured Tester commands must pass. The script passes the committed notes to
GitHub with `--notes-file`. `tools/install.sh` maps Apple Silicon to
`aarch64-apple-darwin` and Intel to `x86_64-apple-darwin`, downloads that
architecture's binary and checksum, and applies the checksum gate before
replacing an existing installation.

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

## Clean-room first-run smoke

`scripts/first-run-smoke.sh` is an operator-run release gate that exercises a
candidate binary from first-use skill handoff through scheduler execution,
Learner completion, Merge Candidate inspection, and explicit landing inside a
fresh repository and isolated home. `prepare` seeds the exact marked bootstrap
and external lock shape produced by the public skill installer; the separate
live release matrix runs the networked `npx skills add` and coding-agent
conversation themselves. The automated shell suite drives the harness with
local stateful doubles, while a release operator uses the same phases with the
candidate binary and real providers.

The harness runs in three explicit, resumable phases. Every path is quoted, so
the smoke root may live anywhere, including a path containing spaces:

```sh
# 1. Prepare a self-contained smoke root (isolated home, Git repository,
#    deterministic failing fixture, skill bootstrap, planning inputs,
#    schema-v2 manifest, and evidence). Select the provider mix explicitly.
scripts/first-run-smoke.sh prepare /path/to/smoke-root \
  --coder-profile codex-balanced \
  --codex-home /path/to/authenticated/codex-home

# 2. Install/select Fluent, hand off the bootstrap, initialize, queue the Work
#    Item, run the scheduler, and stop at a ready Merge Candidate.
scripts/first-run-smoke.sh run /path/to/smoke-root

# 3. After you inspect and accept the candidate, land it, verify the fixture
#    test and a clean target, and record the merged commit.
scripts/first-run-smoke.sh land /path/to/smoke-root
```

Choose any empty (or not-yet-existing) directory as the smoke root; `prepare`
refuses a nonempty directory unless it already holds a compatible harness
manifest or an incomplete-prepare marker left by an earlier interrupted
`prepare`, so both re-running `prepare` on a finished root and resuming one that
failed mid-seed are safe. Schema-v1 manifests are rejected rather than migrated
because this operator harness was not released with a compatibility contract.
A completed root also rejects a different requested coder profile instead of
silently changing the recorded provider matrix row.
A relative `--installer` or `--binary` override is
resolved to an absolute path during `prepare`, so `run` selects the same
boundary even when invoked from a different working directory. The harness
creates and preserves its isolated `HOME`, repository at
`<root>/project/main`, selected binary, phase logs under
`<root>/harness/logs`, and evidence under `<root>/evidence` beneath the root.
An operator-supplied Codex authentication home remains outside the root and is
only referenced; the harness never copies it there. The harness never deletes
the root, on success or failure.

**Coder profile and authentication.** `prepare` requires one curated profile:
`codex-balanced`, `codex-stronger`, `claude-balanced`,
`codex-write-claude-review`, or `claude-write-codex-review`. It records the
choice in the manifest, and `run` passes that exact value to configured
initialization. For a profile that uses Codex, pass an existing
authenticated Codex home with `prepare --codex-home`. The harness records that
absolute path in its manifest and sets it as `CODEX_HOME` for Fluent commands;
it does not copy credentials into the smoke root, logs, evidence, or fixture
repository. Claude credentials continue through Fluent's host-owned Keychain
path. The smoke `HOME` remains isolated under the root.

**Skill and scheduler boundaries.** Before initialization, `run` invokes
`fluent skills add` exactly as the bootstrap instructs. It requires the marked
project skill to become a sidecar-managed full skill, Fluent's lock entry to be
removed, unrelated lock values to remain, and no unselected Codex or Claude
Code project skill root to be created. It then initializes with
`propose` follow-up mode, writes the fixture Tester configuration and executable
extractor, and commits the complete first-use state before creating the Work
Item and planned Attempt. The fixture extractor maps `./check.sh` to the stable
`first-run-smoke/greeting` id, so the seeded failure and corrected pass compare
as two observations of the same test. It returns an empty array for Tester's
empty structural probe and rejects other command shapes.
The harness submits that Attempt with `fluent queue add`, starts `fluent
scheduler run` as a child, and polls `fluent queue show` for the Work Item's
latest structured dispatch. It terminates and reaps the child after a terminal
dispatch or harness interruption.
`candidate-ready` still requires an independent `attempt show` check for a
complete Attempt and succeeded Learner; the scheduler never lands the
candidate.

**Install boundary.** By default `prepare` records the public installer at
`https://fluent.computer/install`, and `run` invokes it under the isolated
home; it needs `curl` on `PATH` and network access to fluent.computer. For
pre-release validation, override the boundary without changing the phase
contract:

```sh
scripts/first-run-smoke.sh prepare /path/to/smoke-root \
  --coder-profile claude-balanced --installer ./tools/install.sh
scripts/first-run-smoke.sh prepare /path/to/smoke-root \
  --coder-profile codex-write-claude-review --binary ./target/release/fluent
```

**Failure and resume.** If any phase fails, the harness exits non-zero,
preserves the whole root, and prints the failed phase, its log path under
`<root>/harness/logs`, and the exact command that resumes from the last safe
phase. Authentication, provider-capacity, `needs-user`, and non-ready
candidates are preserved nonterminal states: read the named log, resolve the
cause, and re-run the printed resume command against the same root. A resumed
phase appends to its existing log rather than truncating it, so the failed
command that first stopped the phase stays in the evidence. Re-running `land`
after a successful merge is safe: the phase inspects the durable candidate state
and skips the merge when it has already happened, finishing only the outstanding
verification. Landing is gated on a succeeded Learner, so `run` stops with a
truthful nonterminal state rather than handing off a candidate whose Learner has
not yet succeeded. When the scheduler records `needs-user`, a later `run`
issues `fluent queue resume` for the manifest's exact Attempt before starting
the scheduler again; it never creates a replacement Attempt or replays
completed initialization.

## Agents

### Coder selection

Fluent supports three Coders: Claude Code, OpenAI Codex, and Pi.
Claude is the creation-time default. When creating an Attempt, select a
different Coder with `--coder codex`, `--coder pi`, or
`FLUENT_CODER=<coder>`.

Each Attempt stores a writer/reviewer **coder mapping** that determines
which Coder and model run those agent roles. Tester Tasks run as a deterministic
subprocess without a Coder. Configure with per-role flags like
`--write-coder pi --write-model qwen3-30b-a3b --review-coder claude`.
At Attempt creation, per-role and global CLI flags take precedence over
environment, project config, user config, and coder defaults. Fluent stores the
resolved mapping on the Attempt record so different Attempts of the same Work
Item can use different Coder configurations.

Running or resuming an existing Attempt or one of its Tasks starts from that
stored mapping. A flagless `attempt run` or `task run` does not resolve config or
environment again and performs no mapping write. Explicit run flags overlay only
their named fields; Fluent persists a changed mapping through a fresh
field-level Work-model mutation before execution. Learner-only recovery reads
the Writer entry from the same persisted Attempt mapping.

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
`PATH`. The entrypoint dispatches on `FLUENT_CODER` (default `claude`) and
uses that selection only to validate and prepare coder-specific credentials.
An Attempt run receives no coder flag and consumes the mapping persisted in
its uploaded Work Item.

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

Worker execution defaults to headless. Native Attempts run assigned Tasks in
managed terminal sessions: Native Codex connects its TUI through Fluent's
private app-server relay, Native Claude uses its TUI with lifecycle hooks, and
Native Pi uses its TUI with Fluent's owned extension. Each native worker
invocation uses one foreground tmux server.
`terminal_sessions` publishes a versioned descriptor under the source's
host-owned `.fluent/work/terminal-sessions/` tree only after the server and
session start. The descriptor names the source, Work Item, Attempt, Task, and
execution generation; attachment rechecks the child-owned execution receipt
and live owner lock. A private socket directory has an exact Seatbelt grant in
the worker's profile. The server starts under that profile, so its pane
commands inherit the worker's workspace permissions. An attachment has no
authority to start a worker.

`fluent terminal attach` observes by default. Control uses a process-held
lock beside the host-owned descriptor. The writable tmux client directly inherits
the Control lock descriptor, so Control remains occupied if the Fluent CLI exits
while tmux is attached. The client releases the lock when it exits. A second
controller may still observe, and detaching releases only the client lease.

Managed windows use manual sizing from creation. tmux's `ignore-size` flag
alone still lets an observer determine the window size when no ordinary client
is attached. Observe keeps that flag and read-only input, while only Control
calls the runtime's validated resize operation. Embedded Control supplies its
viewport dimensions; full-screen Control polls the inherited terminal's size
without reading its input. tmux's status row is disabled so the worker receives
the entire viewport. Detaching Control leaves the last worker size intact,
even when observers remain attached.

Embedded key input quotes tmux's Ctrl-B prefix so one native Ctrl-B reaches
the worker and a following key cannot invoke a tmux command. Managed sessions
disable timing-based paste detection so rapid native keys still obey that
quoting; bracketed paste retains its explicit boundaries. Full-screen clients
keep tmux's ordinary Ctrl-B, d detach sequence.

Cancellation inventories tmux
pane descendants, the captured pane process sessions, processes holding a
private per-generation file handle, and processes with that generation's
private Seatbelt socket grant before stopping the server. The policy check
compares the private path with an ungranted sibling path to exclude unrestricted
processes. The policy grant identifies children even after they close the
handle, clear their environment, start a new session, or are reparented.
After cancellation settles, it retires the generation's socket and resource
receipt. If it cannot verify settlement, cancellation fails
without claiming successful cleanup. The separate host acceptance check must
prove this behavior on a real macOS Seatbelt and tmux runtime.

The fluent command runs Tasks on the local machine. Claude and Codex
run inside a macOS Seatbelt sandbox rendered by Fluent. Fluent renders
each sandbox from `common.sb` plus the selected coder's profile layer:
`claude-code.sb` for Claude Code and `codex.sb` for Codex. Claude uses
the Claude token refresh hook at session boundaries; Codex does not.

An enclosing Codex or Claude Code process may already confine Fluent's shell
command. macOS Seatbelt then rejects Fluent's nested production profile before
a worker launches. `os::preflight_host_execution` applies the production common
profile to `/usr/bin/true` as a side-effect-free capability probe. Configured
initialization runs it after validating the repository root but before provider
authentication or `.fluent/` mutation. A local Attempt runs it after resolving
its local runtime but before provider readiness or Work-state mutation. A newly
elected scheduler runs it before baseline preparation, polling, or queue claims;
a process that merely finds and reuses a live coordinator performs no new probe.

The typed failure starts with `FLUENT_HOST_SANDBOX_BLOCKED`. Its recovery keeps
the two security boundaries distinct: the host agent approves only the same
Fluent command outside its enclosing sandbox, while Fluent continues to apply
its own worker sandbox. Recovery never substitutes Fluent's `--no-sandbox`
option and never asks the user to weaken the host agent globally. This ordering
also prevents an enclosing sandbox's denied Keychain access from being
misclassified as a Claude login failure during configured initialization.

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
                                    --no-sandbox
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
| Claude OAuth | macOS Keychain | Extract and pass as an env var. Refresh with a safe-mode host probe at session boundaries; it has a configurable deadline and rereads Keychain only after a successful exit. |
| AWS | SSO profile | `aws configure export-credentials` resolves to STS temps, passed as env vars. |
| Brave Search | macOS Keychain | Extract, pass as env var. |

Sandbox profile unchanged — credentials injected via env vars, never by
opening filesystem access.

Test-support builds treat `FLUENT_TEST_HERMETIC_PROVIDERS` as a host credential
boundary. They skip Keychain injection, Claude expiry checks, and credential
refresh so nested release fixtures reach only their configured provider doubles.
Credential-specific tests may set `FLUENT_TEST_ALLOW_CREDENTIAL_FIXTURES` while
putting fixture-owned `claude` and `security` executables first on `PATH`; this
re-enables the host credential code paths only for those explicit fixtures. It
does not allow environment credentials into a provider child; that separate
allowlist remains `FLUENT_TEST_FIXTURE_PROVIDER_CREDENTIAL`.

Autonomous Claude Writers use an Attempt-managed `HOME` so interactive Claude
configuration and session history do not enter the worker boundary. That
managed home is retained even when the scheduler starts an Attempt with
`--no-sandbox`; in that route Fluent still extracts the supported Claude
credential on the host and passes it through the inherited environment. A
structured `authentication_failed` transcript is a typed authentication pause,
not a generic coder failure, so it does not consume repeated Writer launches.
Configured Claude readiness also runs `claude --version` after authentication
succeeds. Versions older than 2.1.211 produce a non-blocking warning because
that release contains Claude Code's parallel session token-renewal fix. Missing
or unparseable version output does not turn successful readiness into failure.
Before configured init mutates the project, it then invokes each distinct
selected Claude model once with safe mode, no tools, low effort, JSON output,
and no session persistence. The response must be a successful result containing
the exact readiness marker. Fluent accepts both Claude's single-result JSON and
the current event-array JSON when its terminal event is that exact successful
result. The probe uses the host credential bridge and the bounded process-group
runner described above. A failure leaves a new project uninitialized and
reports the matching redacted diagnostic category. This configured-init canary
is deliberately not part of the shared runtime readiness boundary: autonomous
phases retain the cheaper authentication-only preflight before reservation and
perform useful inference only in their assigned task.

### Fargate runtime

Claude OAuth token passed as env var at task launch. Short-lived; multi-hour
runs will outlive it. Future: WIF (Workload Identity Federation) for
automatic token refresh using the task's IAM identity.

### Controlled Codex workers

Autonomous Codex launches cross a provider-specific boundary in
`codex_worker`. The boundary resolves the effective interactive `CODEX_HOME`,
selects one absolute `codex` command from `PATH`, and derives its bounded
package read closure. For an official npm Node wrapper, the boundary verifies
that the platform package identity and version match the wrapper package and
retains its packaged native executable for autonomous model work. A missing or
mismatched native package fails preparation. Standalone native commands remain
their own execution target. Fluent stages only supported authentication into a
mode-private temporary home and runs `codex login status` through the selected
command before useful work starts. The preflight runs from the system temp
directory instead of inheriting an arbitrary project directory that the
production profile may not grant. The launch keeps that prepared capability
alive through sandbox construction and process completion, then removes the
temporary home by RAII cleanup.

The same pre-reservation boundary resolves `codex.skill-roots` from user and
project configuration. User configuration may name absolute external roots;
project configuration may name only roots contained by the canonical project.
Fluent rejects missing roots, root or bundle symlinks, non-directories, and
duplicate top-level skill names. It copies the validated directories into
`$CODEX_HOME/skills` before launch, so Codex discovers a stable snapshot and
never receives read access to the source root or the rest of the operator's
home. An absent setting keeps the private skill catalog empty.

Every autonomous command disables hooks and ignores user configuration.
Reviewer, Learner, and rebase commands use ephemeral sessions. Writer commands
use persistent sessions inside the temporary worker home so an approved
pre-review continuation can retain its rollout; Fluent copies only the
`sessions/` tree into the Writer Task artifact and restores it into the next
temporary worker home. The Seatbelt profile receives the staged home as its
only writable Codex-state root plus read-only access to the resolved launcher's
package closure; it does not receive the interactive home or the launcher's
enclosing operator home. Writer, Reviewer, Learner, and rebase routes execute
the retained autonomous execution target instead of searching `PATH` again.
This avoids adding the npm Node shim's process boundary inside Seatbelt when a
host tool supplies an isolated `HOME`. Interactive Codex deliberately remains
outside this boundary and retains the selected command, user configuration,
hooks, and home access.

Writers, Learners, and rebase workers run from a candidate worktree, where
Codex can discover the applicable `AGENTS.md` normally. Reviewers run from a
writable artifact directory, so their profile adds the project-root
`AGENTS.md` as one exact read-only file. When that file is a symlink to an
in-project regular file such as `CLAUDE.md`, the profile grants both the
declared path and the exact canonical target so macOS Seatbelt can follow the
link. The same canonicalization boundary applies to declared Writer and
Reviewer input artifacts. It rejects targets outside the project and does not
grant the project root, a parent directory, or adjacent files.

Writer and Reviewer routes preflight before reserving their planned Tasks. An
authentication failure pauses the existing Attempt for `codex login` recovery,
so it does not consume a Writer round. Learner and rebase routes preflight
before they record their respective in-progress work. Resuming with `fluent
attempt run` reopens the authentication-paused Task using the Attempt's stored
coder mapping.

The Reviewer preflight also opens every existing readable root under the
rendered production profile. It batches files through `cat` and directories
through a one-level `ls`, so a successful preflight uses at most two processes.
A failed batch checks paths individually only to identify the denied path.
Reading file bytes and directory entries avoids the false confidence of a
metadata-only `stat`.
The gate prevents an expensive model call from discovering that an advertised
input is unreadable. Reviewer prompts tell agents to open supplied absolute
paths directly because parent directories can remain intentionally
inaccessible.

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
    candidate_gate.rs        ← Host-owned candidate admission and evidence
    verification_reference.rs ← Declared reference and native result identities
    verification_assessment.rs ← Read-only current requirements and bound proof
    tester.rs                ← Tester subcommand (deterministic test runner)
    claude_auth.rs           ← Claude OAuth authentication
    cleanup.rs               ← Cleanup of terminal Work state
    cli.rs                   ← CLI argument types
    coder.rs                 ← Coder trait + Claude/Codex implementations
    content.rs               ← Runtime content resolution (project → user → bundled)
    credential.rs            ← Keychain credential injection
    dashboard.rs             ← Operator-console entry point and terminal session
    dashboard/               ← Snapshot projection, interaction state, layout, rendering
    execution.rs             ← Register, supervise, and cancel local process ownership
    fargate.rs               ← Fargate launch, watch, stop, and pull for Work execution
    fargate_bootstrap.rs     ← JIT Fargate setup (CFN, base + project image builds)
    follow_up.rs             ← Land-gated follow-up materialization: pending operation, journal, corrective host gate, derived Work intake
    git.rs                   ← Git commands, ignore decisions, and source-cleanliness helpers
    guidance.rs              ← Render concrete next-action commands
    hooks.rs                 ← Project hook execution (.fluent/hooks/<name>)
    keep_awake.rs            ← macOS idle-sleep prevention toggle
    lifecycle_recovery.rs    ← Exact classifiers for supported persisted lifecycle migrations
    lineage_lock.rs          ← Root-lineage serialization for corrective Work charges
    notify.rs                ← Push notification delivery
    observations.rs          ← Observation CRUD and lifecycle
    os.rs                    ← Seatbelt sandbox rendering, prerequisites
    plan.rs                  ← Parse plan.md into groups and steps
    post_merge_review.rs     ← Post-merge review orchestration
    prep.rs                  ← Pre-flight workspace preparation
    provider_evidence.rs     ← Classify current and legacy provider outage evidence
    queue.rs                 ← Per-Work-Item dispatch ledger (history + one active dispatch)
    review.rs                ← Review verdict parsing and state
    review_diff_command.rs   ← Review diff CLI subcommand
    scheduler.rs             ← Elected coordinator running a capacity-limited worker pool
    tester_harness_recovery.rs ← Audited project-owned Tester repair transition
    transcript.rs            ← Parse stream-json transcripts incrementally
    update.rs                ← Self-update and update-check nudge
    usage.rs                 ← Per-turn usage row logging and summary cache
    usage_limit_contracts.rs ← Inspect immutable quota approval and retry eligibility
    usage_limit_recovery.rs  ← Publish quota approval and reserve the same Writer retry
    verification.rs          ← Execute approved verification Tasks and validate receipts
    verification_policy.rs   ← Share checker identity and bounded-log policy
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
    first-run-smoke.sh       ← Operator-run clean-room first-run release gate
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

### Public verification

`host_evidence.rs` validates operator authorization and persists verification
Tasks. `verification.rs` executes their confined checks and validates immutable
receipts using the execution and attachment primitives in `host_evidence.rs`.
Both modules consume checker hashing, sandbox identity, and log-size limits from
`verification_policy.rs`; that shared contract depends on neither caller.
`work_task_executor.rs` owns reservation and Task settlement;
`work_attempt_loop.rs` accepts the receipt and advances targeted evidence review.
`execution.rs` serializes child launch with cancellation across coder and
non-coder workers. These modules reuse Work model, lease, sandbox, and evidence
attachment boundaries; they introduce no second execution ledger.

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
`FLUENT_CANDIDATE_COMMIT`, `FLUENT_BASE_COMMIT`,
`FLUENT_ARTIFACT_DIR`) and runs with the candidate workspace as its
working directory. `FLUENT_BASE_COMMIT` identifies the lower bound of
the candidate range that the gate is checking. Stdout and stderr are
captured to `<log_dir>/<hook-name>.log` so failures stay inspectable
after the fact. Hooks that are missing or not
executable are silently skipped — no central registry, no
configuration file, the filesystem is the manifest.

### Tester

`tester.rs` implements the deterministic Tester subcommand. It reads
`.fluent/tester.yaml`, runs each declared command sequentially,
invokes `.fluent/extract-tester-results` to normalize per-test
output, and assembles `tester-results.json` — the canonical artifact
all reviewers consume.

Three files form the Tester contract:

- `.fluent/tester.yaml` — project's declaration of the test commands
  Tester runs. Each entry has a `command` (shell string), a
  `test_harness` (parser identifier for the extractor), and may set
  `reject_process_leaks: true`. For a guarded command, Tester creates a
  private fixture directory below its artifact area before it renders the
  sandbox, exposes that directory as `TMPDIR` and
  `FLUENT_RELEASE_TEST_ROOTS`, and collects process evidence in its host
  process before and after the command. The command never supplies that
  inventory. Tester reports unavailable collection as a harness error and
  records any surviving scoped Fluent, Claude, Codex, or Pi process with its
  PID, kind, and directory in the command's settlement evidence.
  `src/process_settlement.rs` resolves selected processes with `lsof`. Only
  exit 1 with empty stdout and stderr triggers a PID-specific `ps` state
  check. Exited processes and zombies are omitted; a live process gets at
  most three immediate cwd probes. Permission errors, malformed output,
  and persistent live processes without a cwd make the inventory unavailable.
  This keeps transient process rows from discarding usable baseline results
  while preserving the pre/post leak comparison.
- `.fluent/extract-tester-results` — project's executable that
  normalizes raw command output into a per-test JSON array on stdout.
  Receives the artifact directory as its single argument, reads
  `commands.json` and per-command log files, emits structured results.
- `tester-results.json` — canonical artifact with `commands`, `tests`,
  `summary`, and `error` fields. A guarded command may also carry optional
  `process_settlement` evidence without replacing its output or exit-status
  record. All five reviewers receive it via `input_artifacts`.

Tester preserves each raw native test `id` and status and adds a normalized
typed `identity` where supported. Producers may supply that identity directly,
but it must retain every qualifier already present in the raw ID; omitted or
conflicting qualifiers invalidate the producer identity. For shell
results, `{"kind":"whole-script","path":"tests/check.sh"}` differs from
`{"kind":"named-test","path":"tests/check.sh","name":"stops"}`.
Rust and Python identities preserve binary, module, class, source path, and native
name where supplied. Harness display text cannot grant a passing identity or
author a canonical command occurrence. The host continues to bind commands,
configuration, extractor, result provenance, and accepted review evidence.

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
any changes outside `.fluent/`, and reruns `check-pre-merge`. It then verifies
the final candidate tip against the exact target baseline with the Tester before
fast-forward. Successful exact evidence is promoted after the merge.

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

The watcher selects a candidate as merge-ready when the latest Attempt has
`status == complete`, `review_state == passed`, and Learning in the `Succeeded`
state, the candidate has `merge_state.status == pending`, and
`merge_state.auto_merge_skipped` is not `true`. Selection does not require the
Merge Candidate's `merge_review_state` to have passed; the land command owns
pre-merge checks and exact candidate Tester verification after selection.

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

The wrappers cover checked commands, trimmed or raw output, and byte input on
stdin. The crate-private `ignored_paths` uses the same command boundary and parses
`git check-ignore -z -v --no-index --stdin` into effective positive ignore
matches. Git also emits the winning negation in verbose mode; the parser drops
those records because they prove the path is trackable. On failure, the checked
wrappers surface the subcommand, exit code, working directory, and captured
stderr so failures are debuggable without re-running.

`run`, `run_stdout`, and `run_raw` retry git lock errors transparently.
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
cleanup inspects every Work branch and registered candidate HEAD against the
target branch saved by its Writer output or Merge Candidate. A missing or
conflicting target, unmerged commit, dirty worktree, failed Git probe, or
existing unregistered candidate directory retains the whole Work Item.
Cleanup reports the affected source and reason in both preview and apply.
The read-only inventory precedes every removal. Apply repeats the source
assessment while holding the dependency, land, and Work model locks, removes
clean registered worktrees without force, and deletes Work branches only at
their assessed tips. It removes artifacts and model records after Git assets.
Registered worktrees whose directories are missing can be removed with force
after their saved HEAD is proved integrated. Unregistered directories are
retained without deleting arbitrary filesystem paths.
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
selection to the Codex CLI default. When Fluent creates an Attempt,
`FLUENT_CODER` selects the default coder if the CLI does not pass `--coder`.
It does not reselect the coder mapping when Fluent runs an existing Attempt.

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
The initial seed describes the workspace immediately before the first
Writer starts. Fluent passes that source commit to the first Writer and
instructs the Writer to reconcile seeded current-state claims that its
changes invalidate before review.

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

Codex usage extraction accepts the legacy internal `event_msg/token_count`
record and the public `turn.completed.usage` record emitted by current Codex
CLI versions. If a transcript contains both shapes, Fluent uses the public
completion record to avoid counting one turn twice. When that record omits the
model, Fluent reads the model from the Task's sibling `coder-info.json`.

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
on every state mutation, an optional `bound_attempt_id`, an admission
`intent`, an optional prior-dispatch link for resumes, and a block reason when
blocked. The status vocabulary is `queued`, `claimed`,
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
current Attempt is suspended at `needs-user` or whose Merge Candidate is
pending land. An older suspended Attempt does not block a newer planned
Attempt; the scheduler binds that planned Attempt instead of allocating
another one. Repeating `add` while a dispatch is already active preserves
its queue time and changes priority only when an explicit `--priority` is
given.

`fluent queue resume <work-item-id> --attempt <attempt-id> [--priority N]`
is the explicit scheduler recovery boundary for a supported current
`needs-user` Attempt. It never revives or rewrites the prior terminal dispatch.
Instead it appends a queued dispatch with `intent: "resume-attempt"`, binds the
exact Attempt before claim, and records `resume_of_dispatch_id` when a prior
needs-user dispatch exists. Repeating the command while that same resume is
active is idempotent; a later needs-user outcome requires a new explicit
authorization. Mismatched Attempts, unsupported pause kinds, unrelated active
dispatches, and contradictory terminal history fail without queue mutation.
An earlier canceled dispatch remains terminal history and does not authorize or
block a new explicit resume. The new dispatch binds the exact currently eligible
Attempt without linking itself to the canceled dispatch, whose binding may name
an older Attempt.
For a canceled Attempt, admission records `resume_cancellation_requested_at`
without clearing the stop. The Attempt loop compares it with the current
cancellation under the Work model lock before reopening. Ordinary and legacy
dispatches without this field cannot resume a cancellation; repeating an active
resume cannot update its authority to cover a later stop.
For an uncertain-review pause, queue admission calls the same read-only
`lifecycle_recovery` inspection as foreground execution before it appends the
resume dispatch. The scheduler later launches the bound Attempt through the
ordinary Attempt loop, which revalidates the frontier before reopening only the
uncertain roles.

A foreground `fluent attempt run` does not claim a queue dispatch. After the
run returns, Fluent checks the latest dispatch bound to that exact Attempt and
reconciles it from the Attempt's durable result: a completed or failed Attempt
settles the dispatch, while a reopened planned Attempt with no executing Task
returns a prior `needs-user` dispatch to `queued`. A live `claimed` or
`running` dispatch is left to its scheduler worker. The update names the
dispatch generation that was read, so a concurrent resume or queue mutation is
left untouched rather than being overwritten by the foreground command.

A pending evidence-targeted Review is also resumed through that exact Attempt
binding, even when its preceding dispatch is `blocked`; this includes the brief
frontier where the Review Task is complete but its verdict has not yet been
consumed by the Attempt loop. An executing Review remains ineligible so a
second dispatch cannot duplicate live work. Ordinary `queue add` rejects this
frontier so it cannot allocate a replacement Attempt. Fluent also
reconciles the narrow historical case where an older scheduler already created
a later initial Writer: every later Attempt must contain exactly one interrupted
output-free Writer, have no live task lease, and own a clean worktree still at
project `HEAD`. The resume records immutable reconciliation evidence, marks
those Writers `superseded`, and binds the dispatch to the older evidence Review.
A valid reconciliation remains the Attempt-selection authority after that
Review settles, so later explicit resumes of the same recovered Attempt do not
fall back to the superseded later Attempt merely because the frontier changed.
A dirty, advanced, live, multi-task, or output-bearing later Attempt fails
closed. Queue history and superseded Attempts remain available for audit.

A relaunchable failed pre-land Learner is also an explicit scheduler recovery
frontier even though its review-passed Attempt is complete and its prior
dispatch is `blocked`, not `needs-user`. Queue admission and claim share a
read-only `lifecycle_recovery` inspection. It requires every Task to be Complete
or Superseded, exactly one pending and unreviewed Merge Candidate, a clean
candidate workspace at the recorded commit, and no live Learner lease. The Work
model validates retained Superseded history; active or incomplete Tasks block
the retry. The resume keeps the prior blocked dispatch as history and launches
the same Attempt. The ordinary Attempt loop then performs its own checks and retries
only the Learner; it does not repeat the Writer, Tester, or reviewers. A changed,
dirty, reviewed, landed, or ambiguous candidate fails closed, as does a
candidate with a live Learner run.

The terminal-history incident is covered by the
`learner_terminal_history_queue_`, `learner_terminal_history_claim_`,
`learner_terminal_history_foreground_`, `learner_terminal_history_scheduler_`,
and `learner_terminal_history_preserves_` tests in `tests/binary.rs`. They check
admission, claim-time drift, both retry routes, and rejection without replacing
the stored Task or candidate evidence.

Legacy dispatches omit the new fields and deserialize as ordinary run intent.

For the narrow Xcode/SwiftPM recovery, the same command accepts `--no-sandbox`
only while the exact current Attempt is paused at `tester-harness`. Its new
dispatch records `tester_no_sandbox: true`; ordinary and legacy dispatches omit
that default-false field. Admission and claim both revalidate the pause and
intent, and a repeated resume must match the stored approval. The terminal
dispatch retains the value for audit, while any later pause requires a new
explicit resume. This flag never maps to the broad public `attempt run
--no-sandbox` policy.

Automatic promotion enqueues through `ensure_dispatch`, which reconciles
or creates one origin-bound dispatch without reviving terminal history. A
receipt-loss replay recognizes its own durable dispatch by operation id even
after Work lifecycle advances, and reports that it created no new dispatch.
An active, canceled, or terminal dispatch from another origin fails closed;
it cannot satisfy the automatic operation's completion proof. Automatic
dispatch and ordinary claim paths apply the same lifecycle checks as explicit
queueing, so proposed, abandoned, suspended, or land-pending Work cannot enter
or resume execution through replay. Resume claims use the Attempt loop's shared
pause predicate and require the persisted intent and Attempt binding, so a bare
bound id cannot impersonate user authorization.

Queue creation and claim acquire the Work Item's aggregate model lock before
the project queue lock. They read lifecycle state while holding that Work lock
and retain it through the ledger mutation, so abandonment or another Work
lifecycle change cannot land between eligibility and dispatch state. No path
acquires a Work-model lock while already holding the queue lock.

`fluent queue list` prints each Work Item's active dispatch sorted by
priority descending, then `queued_at` ascending, showing priority,
queue time, execution status, and Work Item id. `fluent queue show
<work-item-id>` prints the latest dispatch as JSON regardless of whether it is
active or terminal, preserving fields such as `bound_attempt_id` for exact
handoffs. It fails when the Work Item has no dispatch history. `fluent queue
remove` records a `canceled` disposition on an unclaimed dispatch — the entry
leaves the active queue but the cancellation survives replayed automatic
promotion. Removal is rejected while a dispatch has a live claim, and
errors when the Work Item has no active dispatch.

Malformed ledger files are skipped with a warning and preserved
on disk for operator inspection.

### Local scheduler coordinator

The shared scheduler service elects one owner across registered projects with
`~/.config/fluent/scheduler/service.lock`. Socket health reports the protocol
generation. Before a start can reuse the service, health and `service.json`
metadata must agree on the current lease's instance, running state, version,
and executable SHA-256.
Both startup entry paths check these fields, including after a managed start
answers health. A different build requires `fluent scheduler stop --all`,
followed by an explicit `fluent scheduler run` with the new build. A reachable
service whose identity cannot be verified is preserved for manual inspection.

`fluent scheduler stop --all` addresses the current service instance. It
durably records the request, stops new claims across every registered project,
rejects new socket submissions without persisting them, and waits for owned
workers and result reconciliation. It publishes an instance-specific stop
result before releasing the socket and lease. Repeated requests for that
instance replay its result; an old request cannot drain a replacement. A
crash before the result leaves an interrupted drain to inspect and retry. If
the old service lacks verifiable drain control, stop its original supervised
process, wait for active work to settle, then run the new build. Ordinary
`fluent scheduler stop` remains scoped to one project.

`fluent scheduler run` elects a single coordinator per project using a
lifetime `flock` lease. Immediately after election, the coordinator atomically
writes `coordinator.json` beside the lease. This schema-versioned record contains
a random instance id, display-only process id, start time, `running` or
`draining` state, and the immutable Fluent build identity: version plus the
SHA-256 of the current executable. The locked file contains the same instance
id, so a reader accepts metadata only when it belongs to the current lease.
Contending starts wait through the bounded lease-to-metadata publication window.
A start reuses a live coordinator only when it is running and both build fields
match, and names the reused build. A draining coordinator tells the caller to
wait before starting again. A different build is rejected with the exact
`fluent scheduler stop` then `fluent scheduler run` recovery. Missing,
malformed, unsupported, or lease-mismatched metadata on a live lease fails
closed; the process id is never a signaling authority.

`fluent scheduler status` reads the lease and metadata without changing either.
`fluent scheduler stop` reads the live metadata, atomically writes a stop request
bound to that instance id, and revalidates the instance after publication. The
coordinator recognizes only its own request. A replacement therefore ignores a
request left by an earlier instance, and a stop command reports a replacement
race instead of claiming it stopped the project. There is no automatic restart
and Fluent never sends a signal to the metadata's process id.

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
via `fluent attempt run <id> <attempt-id>`.

If the bound Attempt does not exist yet, `ensure_bound_attempt` resolves coder
inputs from user configuration, the human-facing project's
`.fluent/config.yaml`, and supported `FLUENT_*` environment overrides through
the same creation-time path as `fluent attempt create`. It snapshots the fully
resolved writer and reviewer mapping while adding the Attempt, before the coder
plan is printed or a baseline starts. If the Attempt already exists, including
for a resume dispatch, this step returns without reading current coder
configuration or changing the persisted mapping.

For a resume dispatch, the Attempt binding is already durable at enqueue time.
Claim verifies that the persisted binding still names the current supported
needs-user Attempt and then launches that same Attempt; the one migration
exception accepts an older complete Attempt only when its retained generic
Learner failure, pending unreviewed candidate, completion contract, final Tester
results, and rejected Learner gate jointly prove the former missing
test-reference defect. The durable Learning run counter identifies that gate;
empty later directories preallocated for bounded retries are ignored, while any
later directory containing evidence blocks the migration. That exact recovery
may follow the older false `candidate-ready` dispatch or its newly classified
`blocked` equivalent.
`ensure_bound_attempt` therefore finds the same Attempt instead of allocating
another one. Coordinator restart leaves an unclaimed resume queued. A resumed
Attempt that pauses again terminalizes only the new dispatch at `needs-user`,
preserving every earlier dispatch and requiring another explicit resume command.

The shared scheduler service reads its registered checkouts on each poll. For
each checkout it owns, it calls `recover_and_reconcile` before claiming fresh
work, then starts a tracked worker for each claim before trying another claim.
An error in one checkout's configuration, queue, coordinator, or worker is
reported with its project and available dispatch identity while the service
continues to later checkouts. Registry errors still stop the poll because the
service cannot establish its project set. Automatic recovery retries on a later
configured poll; it does not create a new pause or infer an Attempt outcome
from a worker error. Reaping joins only finished workers and reports their
failures. On a shutdown signal, the service joins remaining workers and returns
their collected failures to its caller. If another exit path drops the service,
the destructor joins remaining workers and logs their failures.
The host sandbox preflight is a service-wide capability gate: its typed failure
stops the service before baseline preparation, queue claims, or provider launch.

Before each poll, `recover_and_reconcile` reads model state first and
then applies a generation-checked queue update, so recovery never nests
the queue lock with Work, lineage, candidate, or follow-up locks. A
stale claim whose bound Attempt is nonterminal and not executing resumes
that same Attempt; a stale claim whose bound Attempt became terminal
reconciles the dispatch from that outcome without rerunning it; an
interrupted claim with no bound Attempt returns to `queued` and creates
at most one Attempt on the next claim.
The same generation check is used after a foreground `attempt run`, so an
already-replaced queue dispatch remains the authority for its own recovery.

When an Attempt terminates, the coordinator reconciles the dispatch and
releases its claim. It sets `candidate-ready` only when the Attempt's pending
Merge Candidate passes the shared Learner advancement gate. `failed` and
`needs-user` mirror those Attempt states; a complete Attempt with a
Learner-unapproved candidate is `blocked`. An unclaimed Work Item that became
abandoned is `canceled` with no Attempt, and a missing, malformed, or
non-executable reference is `blocked` with the reason. Other in-flight
dispatches are unaffected.
The scheduler never invokes merge logic — landing remains the job of an
explicit land command or the separately authorized `fluent auto-merge
--all`, which runs as a sibling process.

On an instance-bound stop request, SIGTERM, or SIGINT the coordinator changes
its metadata to `draining`, stops claiming new Work, preserves unclaimed Work as
`queued`, and drains live children — letting them finish and recording their
outcomes — before exiting. It then removes metadata and its matching stop
request while it still owns the lease and releases the lease. With no live
Attempts it exits without waiting for the polling interval.

When queued Work exists but no live coordinator can claim it, the Work
stays `queued` and `fluent status` reports that execution is stopped,
naming `fluent scheduler run` as the start or recovery action.

### Assign required actions to existing roles

The Plan's `Responsibility` column freezes `writer`, `host`, `review`, `tester`,
or `learner` on each required completion-contract row. Behavior implementation,
Approach constraints, and corrective findings remain Writer work. The planning
agent splits mixed rows before approval; command text never grants host access.
Legacy Plans without the column retain their prior meaning.

The full completion contract preserves every obligation and verification
reference. Required progress projects Writer rows; external rows remain pending
read-only context in the Writer matrix. The host rejects edited responsibility
and external completion claims before admitting a candidate. Work record schema
2 prevents older readers from ignoring ownership when mutating records.

`recovery_contracts` validates the source matrix and unchanged clean candidate.
The shared Plan acceptance reader overlays only accepted candidate-bound evidence
in memory after targeted Tests Review passes. The Attempt loop pauses before
another Writer when only a host check remains, and checks required host evidence
before advancing past reviews. Required reviewers, the canonical final Tester, and a successful
Learner retain their existing gates.

Ordinary Tests Review also receives currently accepted host proof through
`recovery_contracts::review_evidence`. The reader preserves the distinct Plan,
reviewer-requested, carried-finding, and canonical-command acceptance rules.
Planning pins a derived summary and the exact original proof, binding, source
matrix or command artifacts, and targeted Review decision in the Task's existing
artifact references. This summary conveys acceptance; it does not create another
acceptance authority or rewrite the pending host rows in the Writer matrix.

The executor presents a bounded current-acceptance summary with the candidate and
check or finding scope. Full sources remain readable through the existing exact
regular-file resolver, without additional directory reads or evidence writes.
Historical requests added as proof provenance do not assign their findings again.
Direct and queued launches revalidate the selected bindings before invoking the
reviewer. Changed, missing, expired, or otherwise inapplicable proof blocks that
selection; it cannot become current acceptance merely because an earlier summary
advertised it. Ordinary domain reviews, final Tester, and Learning still require
their own results.

`ownership_recovery` exposes `attempt ownership preview` and `approve`. A preview
binds a proposed exact partition to the original contract, current Attempt,
clean candidate, Task history, planning inputs, and source completion bytes.
`OwnershipProposal::apply` in `work_model` preserves original source links and
verification references, and permits reuse only of an identical completed Writer
obligation. It never transfers a Writer claim to a host or later stage.

Approval revalidates through `recovery_contracts` under the Work mutation and
land locks. Immutable sidecars retain the original contract and audit; a new
matrix supplies the approved mapping without rewriting historical matrices or
Tasks. The aggregate transaction adopts that mapping and records one recovery
operation. Record syntax upgrades and status reads do not invoke adoption.
See the saved mixed-row example in
`skills/fluent.full/references/host-evidence-recovery.md` for the public commands.

Test-reference recovery revises the Attempt's adopted contract and reads its
shared authoritative completion matrix. It changes only selected behavior
references, preserving split Plan identities, responsibility, commands, progress,
and completion claims. The neutral boundary replays recorded reference approvals
over the ownership contract and verifies each revised matrix digest. Exact retries
revalidate both approval histories before returning their recorded result.
Accepted host evidence retains its original source bytes and targeted Review;
a later reference matrix can reuse it only for the same Writer candidate and an
identical Plan row in the validated history. Each new revision binds its source
contract, matrix path, and source digest, including ordinary Task-produced
matrices and Plans with explicit ownership from intake. Recovery follows these
recorded sources instead of requiring an ownership-adoption artifact.
The matrix-source resolver follows reference-revision audits, which alone create
replacement matrices. The shared history validator decodes Approach approvals
under their own schema before evidence consumption, so interleaved approvals
retain the accepted host check for the unchanged candidate.

Approach recovery has two audited forms. The original
`UncertainArchitecture` form retains its existing uncertain-review and narrowly
supported historical-replay semantics. The `FailedReviewRoundCap` form applies
only at a failed-review round cap after a matching unused extension; its audit
binds the exact candidate, extension, Architecture-pass and failed-review
identities, review bytes and digests, prior and replacement Approach, and
authorization. Both forms apply their authorized delta to the Attempt's current
contract, retaining every non-Approach row, including adopted Plan splits and
earlier Test-reference revisions. The neutral boundary replays both kinds of
planning revision in order before source selection, exact retry, or corrective
Writer materialization. The revision command journals the immutable audit as a
managed file in that same aggregate transaction before it publishes the file's
reference with the planning, completion contract, recovery binding, and one
corrective Writer. Before transaction application, no audit sidecar exists;
during recoverable application, a pending transaction may expose the managed
bytes, but the file alone never launches execution or establishes authority. An
interrupted retry recovers the exact journaled bytes, while an unreferenced file
is ignored as an accepted revision and conflicting bytes fail closed. Host
evidence keeps its original source contract and bytes, while changed Approach
rows remain pending until the corrective Writer completes them. A changed audit
or source digest prevents replay without rewriting preserved history.
The shared evidence validator checks an active receipt against its recorded
Writer and candidate. Once every targeted Plan review passes, Attempt execution
settles that receipt before advancing a correction; queue admission checks the
same facts without mutating history, and a later attachment revalidates inside
its transaction. A new Writer candidate still requires its own host evidence.

Writer stagnation compares consecutive completion matrices by stable obligation
identity and completed rows. Repeated corrective reviews do not exempt a Writer
from the two-continuation guard. New findings reset the comparison; replacing an
old finding does not regress an unchanged completed Plan row. Legacy progress
without a matrix retains its counts and compares the finding identities. A new
commit alone never counts as additional completion.

An interrupted initial Writer or a continuation with unfinished Writer obligations
uses ordinary Task retry. Host-resume admission first recognizes a completed
Writer followed by an output-free continuation with complete Writer obligations;
only that shape must satisfy the host-only candidate and evidence prerequisites.

`writer_completion_recovery::validated_writer_context` admits a recovery input
only after checking its producer, exact path, schema-1 bytes, resolution digest,
source Writer, owning Attempt, recorded recovery operation, and generated Task
instructions. `work_task_executor` uses these validated references for exact
readable-file grants and appends the complete instructions after selecting the
initial, reviewed-correction, or pre-review-continuation prompt. Invalid or
oversized authority stops launch. Recovery inputs do not broaden the general
JSON filter or grant access to their parent directories.

Writer-completion repair selects the currently paused Writer before treating a
retained recovery audit as a replay. If the current source is a later completed
Writer, the recovery owner validates earlier attached audits and their completed
replacement Tasks as history. It then requires a distinct resolution and writes
a new audit, operation, and corrective Task for the later source. An approval
for the earlier incident cannot bind the later candidate. The public status
action uses the same eligibility and prior-history checks. Before a corrective
Writer starts, the recovery contract rechecks its exact source matrix, contract,
assignment, transcript, output, and operation binding.

`recovery_contracts::pending_writer_completion_rows` reads the authoritative
matrix through the existing completion and immutable-assignment validation.
The generated `execution-context.json` preserves pending Writer rows and their
assigned finding identities even when progress is checked or current verification
passes. External responsibilities remain pending and uncredited. This core is
bounded by rejecting excess rather than truncating obligations. Every recovered
prompt identifies the exact no-change declaration path and requires fresh
verification under the existing completion policy. Continuations use the same
validated lineage classifier as corrective prompts: preserve the candidate's
ancestry by default; a current host-validated metadata exception supplies its
exact protected base. Historical gate text cannot create that exception.

For a stopped, output-free interrupted continuation, `lifecycle_recovery`
recognizes the narrow Task sequence and passes it to `recovery_contracts` to verify the managed
candidate, approved host row, source completion, and both owner mechanisms.
Attachment supersedes only that continuation and uses the existing targeted
Tests Review transition. Shared checks validate the original ownership approval
and source bytes, attachment digests, exact command, and candidate before
attachment, foreground resume, queue resume, scheduler claim, and Review launch.
The canceled continuation must name the preserved pre-adoption source from the
approval, including validated repair provenance when that source was repaired.
The neutral boundary does not call lifecycle recognition or its executor callers.
An active queue retry revalidates evidence before returning its original dispatch.
Later gates retain their existing order, including capture-mode Learning.
