# Decisions

Architectural and design decisions that are intentional and should not be flagged by reviewers.

---

## Preserve embedded input through tmux

Keep embedded prefix quoting coupled to the managed server's default bindings
and disabled timing-based paste detection, while retaining native full-screen
detach. The [prefix contract](learnings/embedded-input-preserves-tmux-prefix.md)
records the mechanism and its worker-byte and detach regressions.

---

## Revoke attachment access before cleanup

Publish attachment revocation before cleanup waits for Control, while retaining
execution ownership until settlement is verified. The
[revocation contract](learnings/terminal-revocation-precedes-cleanup.md) records
the ordering and cancellation/retry regressions.

---

## Preserve manual sizing for managed terminals

Managed tmux windows use manual sizing from creation because `ignore-size`
alone lets observers resize a window when no ordinary client is attached.
Only the runtime's validated Control owner may resize the worker. Embedded
Control supplies its viewport; full-screen Control polls terminal dimensions.
Keep the status row disabled and retain the last controlled size after detach,
including when observers remain. Future attachment clients must preserve this
invariant. See [runtime terminal ownership](../../documentation/architecture.md)
and `observer_cannot_type_or_resize_worker` and
`fullscreen_resize_requires_control` in the
[terminal regressions](../../tests/dashboard_terminal.rs).

---

## Keep separate budgets for Codex display responses

The [Codex relay](../../src/native_agent/codex.rs) retains one FIFO with
separate queued allowances: 12 MiB for ordinary frames and 64 MiB for large
responses, plus one in-flight message bounded to 64 MiB. Valid UI catalog
responses can exceed the ordinary allowance and overlap before display drains.
Charge overlapping large responses against their aggregate byte allowance;
keep ordinary events in wire order. Exceeding either budget disconnects the
display without blocking independent provider evidence or assigned-turn
completion. Combining the budgets or allowing only one large response would
restore premature display disconnects.

`display_spool_delivers_overlapping_large_responses_in_order` and
`display_spool_bounds_oversized_responses_separately` in the relay cover FIFO
order and separate bounds.
`codex_relay_bounds_plugin_responses_without_losing_turn_evidence` in the
[relay journeys](../../tests/native_agent_tasks.rs) covers evidence and
completion after display overload.

---

## Keep the old dashboard snapshot as an internal compatibility path

The CLI opens the project grouped dashboard. The older status group snapshot
remains inside `dashboard::snapshot` and its app and render branches so existing
dashboard tests and internal snapshot callers continue to work while the project
view is adopted. New operator behavior and terminal integration belong to the
project view. Retire the old branch after its remaining tests and callers move
to project snapshots.

---

## Inventory terminal descendants by owned handle and Seatbelt grant

Terminal cancellation inventories pane descendants and process sessions, then
checks the private owner handle and the per-generation macOS Seatbelt socket
grant. The handle finds processes that retain the inherited descriptor after
leaving the pane's process tree. A child can close that descriptor, clear its
environment, change session, or be reparented, so the grant provides the
remaining ownership signal. Compare the grant against an ungranted existing
path to exclude unrestricted processes; a missing probe path gives unreliable
results. If the process or policy inventory cannot be verified, settlement
fails instead of claiming cleanup. This depends on the worker's private socket
grant remaining unique to that invocation and on macOS exposing its policy to
the host; changes to either require a new ownership proof.

---

## Keep joint child settlement as a separate workflow model

`tests/formal/workflow/JointSettlement.tla` models the crash boundary where
service worker accounting reaches zero while an owned child receipt remains
unsettled. Publication still requires child settlement, including after a
second crash. This separate mapping is the accepted scope for the current
workflow check. Integrated assurance must connect live `ServiceLifecycle`
state and production child records in one trace; the separate model does not
establish that production connection. See
`tests/formal/workflow/connections.md` for the remaining scenario and control.

---

## Keep compact execution contexts linked to complete verification

New contexts with verification use schema 3. They keep unresolved reference and
command rows and satisfied-row counts inline, and bind the complete assessment
in a sibling `verification-assessment.json` by path and SHA-256 digest. This
keeps the Writer context within its size limit while retaining all passing rows
for audit. A saved schema-2 context with inline verification remains readable
and keeps that shape if rewritten. Contexts without verification use schema 1.

---

## Bind library references without rejecting unrelated workspace members

Cargo metadata comes from the live candidate workspace, so the root manifest
must match the candidate commit. A member selected for a `cargo test --lib`
reference must also have a committed manifest and bound Rust sources. A dirty
manifest in another member disqualifies the reference when committed Rust files
under that member, or its committed explicit library source and literal
inclusions, contain the referenced declaration: live metadata could otherwise
hide an ambiguous library. Fluent allows a dirty member with no such committed
declaration so unrelated member edits do not discard valid library evidence.
This text scan can reject an ambiguous looking declaration conservatively;
native package and target identities still determine which result may pass.

---

## Writer handoff hooks run in a detached private candidate copy

Active preflight and final handoff run the Writer-phase project hook against a
temporary shared clone checked out at the candidate commit with detached HEAD.
This keeps a hook from changing the live Writer worktree, branch, or commit while
both checks apply the same policy. Hooks can inspect the candidate's committed
files and commit range, but must not rely on the live worktree path, branch name,
uncommitted files, or mutations that persist after the check. The candidate
gate records the hook outcome, and Fluent checks the live candidate again after
the assessment because the Writer may continue editing during preflight.

## Bind the Writer baseline outside writable artifacts

A host-owned `write-baseline` file beside the Task record is authoritative for
the Writer's starting commit. Publish it before the writable artifact sidecar. If
setup stops between those writes, resume from the host binding instead of the
workspace HEAD. If a correction Task has an older sidecar but no host binding,
restore the binding only when its immutable correction assignment names that
exact source candidate commit. A different or unreadable binding must fail
closed; the Writer cannot establish a new baseline from its own artifacts.

---

## capture-brief Phase 3 keeps cognitive science inline

The capture-brief skill includes cognitive science principles (anchoring bias, framing effects, etc.) directly in the skill content rather than referencing an external expertise file. This is intentional: agents are more likely to read and apply material that appears inline within the skill they are following than to follow a reference to a separate file.

---

## Skills are bundled in the binary and materialized on demand

Skills live under `skills/`; a `<name>.full/` directory overrides a same-named shim when the binary is bundled. Review-skill references are symlinks to authoritative expertise where practical, while the full interactive Fluent bundle carries its stage procedures and selected references directly. At build time, `build.rs` walks the selected trees, dereferences symlinks, and generates `BUNDLED_SKILL_FILES`. At runtime, `materialize_skill()` writes review bundles under `.fluent/work/skills/`, and `fluent skills add` installs provenance-bound agent-facing bundles at supported Claude or Codex roots. Skill instructions reference `references/X.md`, never source-side `expertise/X.md` paths directly.

---

## Releases are ad-hoc signed only — no Developer ID signing or notarization

Release binaries carry only an ad-hoc signature (`codesign --sign -`), applied in `scripts/release.sh` before the checksum. This is deliberate, not an oversight: fluent ships over curl and npx, which do not set the macOS quarantine attribute, so Gatekeeper never runs on the installed binary and Developer ID signing plus notarization would be enforced by nothing. This matches how community CLIs distributed via curl/Homebrew ship (rustup, bun, deno, ripgrep). Ad-hoc signing is the actual macOS requirement — Apple Silicon refuses to execute an unsigned binary. Download safety comes from HTTPS plus the published SHA-256 checksum that `fluent update` verifies. Avoiding Developer ID signing also avoids managing signing secrets in CI. Revisit only if a browser-downloadable artifact (a quarantined `.pkg`/`.dmg`/`.zip`) is ever offered; until then, do not flag the absence of Developer ID signing or notarization.

---

## Learner run evidence is a non-writable sibling of coder staging, not a denied subpath

Host-owned Learner run evidence (transcript, submitted-draft snapshot, error, normalizations) lives under `.fluent/work/artifacts/<work>/<attempt>/learner/runs/run-<N>/`, while the coder writes only to a separate `staging` sibling inside that same run directory. It is deliberately *not* implemented by granting the whole `learner/` directory writable and then denying `learner/runs`. Seatbelt SBPL is last-match-wins, and the rendered profile places `(deny …)` rules ahead of the per-root `(allow file-write* (subpath …))` rules, so an allow on an ancestor subtree overrides a deny on a descendant. Host evidence must therefore live *outside* every granted writable subtree — a sibling of `staging`, not a denied child of a granted root. The run index is allocated from on-disk state (scan existing `run-<N>`, exclusive-create the next), never from the in-memory Learning record, so a lost or omitted record cannot reuse a run identity. Do not "simplify" this into a granted-parent-with-denied-child layout; it would silently let the sandboxed coder write its own run evidence. Related: [[sandbox-denials-track-template-grants]].

---

## The transcript pump's console sink is synchronous and terminal-only; config is per-capture and status has one coordinator

The `transcript_pump` module renders console previews through a single
process-wide sink (`console_preview_sink`, a reference to a plain zero-sized
`static ConsoleSink`). Its operator thresholds
are **not** process-global: they are resolved once per launch (`resolve_config`)
into an immutable `coder::TranscriptCapture` value that is threaded through
`Coder::run_captured` and retained across a launch's auth/rate-limit retry phases.
This replaced the earlier process-wide installed config (a `Mutex` of
`install_config` / `active_config`), under which a concurrent launch could
overwrite another capture's thresholds between resolution and pump spawn. The
public `TranscriptCapture::new(transcript_path, project_root)` constructor resolves
the config internally, so an external `Coder` never names the private config type.

Every persisted `transcript-pump.json` write for one capture is owned by a single
`StatusCoordinator` over an injectable `StatusStore`. It coalesces best-effort
periodic snapshots through a latest-only slot behind a capacity-one wake, processes
required Running and terminal statuses FIFO with acknowledgement (so a terminal
acknowledgement can never be followed by a persisted Running state), balances
every submission across written/coalesced/dropped/disconnected/write-failed
categories, and falls back from an unpersistable Complete to a Failed status. This
replaced the earlier split of a background `StatusWriter` plus a synchronous
`persist_status_sync`; do not reintroduce a second writer or a synchronous
side-channel write. The capture path and the status worker publish the immutable
first fault to a per-pump latch before terminal settlement, so a blocked or slow
status store can never hide a fault from coder supervision.

A coder launch's per-launch supervision diagnostic — most importantly a reaped
leader whose process group could not be *verifiably* swept (`killpg` returned
`EPERM`, or another non-`ESRCH` error) — is surfaced out of the supervisor through
the additive `Coder::run_captured_reported -> CoderRunCompletion`, which pairs the
terminal `Result<i32>` with a serializable `CoderSupervisionReport`. Built-in
coders override it; external coders and mocks use the default that wraps the legacy
`run_captured` with an empty report. Each role artifact boundary
(Writer/Reviewer/Learner/rebase) calls `finish_supervised_coder_run`, which
atomically persists a non-clean report as `coder-supervision.json` beside the
transcript and composes a sidecar-write obstruction as a typed, non-retryable
`SupervisionSidecarError` secondary without relaunching the coder. This is the
durable, non-blocking supervision channel: a group-sweep diagnostic is never
written to a possibly-saturated stderr and never dropped with the `ManagedChild`,
and `Drop` still never publishes a supervision outcome.

Preview delivery is **synchronous and best-effort**, deliberately not a
background renderer over a bounded queue. `PreviewSink::deliver` decides the fate
of the preview on the pump's own thread and returns whether it was delivered, so
drop accounting is exact at every status write (there is no in-flight queue to
settle before `Complete`).

For this landing the production sink **declines every preview** and counts it as
dropped (`dropped_console == records`). Live previews are deferred, not merely
disabled for redirected output:

- Mirroring previews into a redirected (non-terminal) stderr is the flood that
  first stalled Fluent, so a pipe or file sink is never written to.
- Writing to the terminal is no safer here. Even a nonblocking write to an
  independent `/dev/tty` consumes the terminal's remaining queue capacity, so the
  very next *blocking* control-plane write to fd 2 could stall on the space the
  preview just took; an independent file description does not reserve capacity for
  fd 2. Until every Fluent-owned stderr write moves behind one independently
  nonblocking console bus, declining is the safe contract.
- Never `dup(2)` and write blocking: the duplicate shares the same kernel pipe,
  so a later ordinary `eprintln!` would still block in the kernel even with no
  Rust stderr mutex held. Never set `O_NONBLOCK` on a dup of fd 2 either —
  file-status flags are shared.

Declining touches no descriptor and no Rust process-global stderr lock, so
capture is never backpressured and control-plane output never stalls behind the
console. The canonical transcript already holds every byte.

The pump may derive bounded provider-wait diagnostics from complete streamed
records, but those diagnostics never gate capture or execution. Claude Code's
`system/api_retry` record opens a wait interval capped by its `retry_delay_ms`;
the next record or terminal pump settlement closes it. Persist the retry count,
elapsed wait, and active expected resume time in the existing pump status so a
quiet provider is visible without scanning the transcript. Fluent-owned outer
rate-limit sleeps use the sibling role-owned `provider-timing.json` file. Work
metrics index both bounded documents at Task settlement. Before a relaunch
replaces pump status, Fluent adds that phase's completed pump timing to the
role-owned sidecar and records the absorbed phase number to prevent double
counting after resume. Read-only status never walks transcript trees.

Do not "fix" the declining sink by mirroring previews to any stderr or by
reintroducing a background renderer thread. Re-enabling live previews is a separate
change that must first move all Fluent-owned stderr writes behind one independently
nonblocking console bus. (Per-launch config now travels with each capture through
`run_captured`; that is the shipped design, not a thing to undo.)

---

## Transcript age and pump-status timestamps are diagnostics, never authority

`transcript-pump.json` records state, timestamps, and byte/record/drop counters
next to each transcript so an operator can tell a quiet coder from a blocked
console, a failed pump, or completed capture. It is explicitly not a liveness
lease or heartbeat. Executing-Task recovery decides liveness solely from the
process-held flock lease (`executing_task_is_live`), never from how old a
transcript or its status file is. Do not add a transcript-age watchdog or use
pump-status timestamps to reclaim or signal a Task; durable Task ownership is a
separate, dependent Work Item that consumes the pump's terminal signal.

---

## The Learner schema-repair prompt is built inline, not bundled

The bounded schema-repair prompt (`schema_repair_prompt` in `work_task_executor`) is constructed inline rather than added as a file under `prompts/`. It is a short, host-authored instruction that embeds the rejected draft and exact validation error, and it is never resolved through the project→user→bundled content layers the way `learner-user.md` is. Keeping it inline avoids expanding the `prompts/` bundling surface and its naming-guardrail allowlist for a prompt that has no per-project override story. Do not flag the absence of a `prompts/learner-schema-repair.md`.

---

## Advancement requires a succeeded Learner; the post-land handoff-only retry is recovery-only

One shared predicate — `Attempt::learning_advancement_readiness()`, surfaced through `WorkItem::attempt_learning_advancement()` and `MergeCandidate::validate_advancement()` — gates every advancement boundary: Attempt readiness (`MergeCandidateReady`), the scheduler's `candidate-ready` projection, Merge Candidate validation, and the land route. A candidate may advance only once its Attempt's Learner has SUCCEEDED; any other state (absent, `InProgress`, `HandoffPending`, or failed whether relaunchable or not) blocks with one durable reason, `WorkModelError::AttemptLearningNotSucceeded`. This deliberately replaces the earlier "land, then retry the Learner post-land" behavior, which let failed or prepared learning reach `MergeCandidateReady` and land. Do not re-add a boundary that advances over a non-succeeded Learner, and do not fold the readiness check into the structural `MergeCandidate::validate` — it is kept separate so a candidate persisted *before* its Learner runs stays valid (the candidate is created before `run_learner_step`).

Because landing now requires a succeeded Learner, the post-land handoff-only Learner *retry* path (`work_attempt_loop`, guarded by `learner_is_handoff_only` on a Merged candidate) is a recovery/legacy path, not the normal flow: a candidate reaches Merged only with a succeeded Learner, so a Merged candidate with a pending Learner comes from a legacy landing or an interrupted post-land handoff, not fresh advancement. `validate_advancement` therefore exempts Merged/Failed candidates so idempotent post-land follow-up processing still resumes. A pre-land Learner block is surfaced as the dedicated `WorkAttemptRunOutcome::LearnerNotReady`, not `FollowUpRecoveryPending` (whose CLI text hard-codes "is merged" and only fits a genuinely landed candidate). Do not flag the two outcomes as redundant.

### Post-land handoff-only recovery is distinct from selectable pre-land no-expertise

There are two confinement modes that both deny expertise and candidate Git writes, and they must not be conflated. Post-land **handoff-only** recovery is a fixed property of a *merged* candidate: it runs against the persisted merged commit and exists only to resume a landed Attempt's follow-up. Pre-land **no-expertise** is a *durable Work Item policy* (`WorkItem::learner_mode`, default `capture`) selected before execution — for release gates that require the latest reviewed Writer SHA to survive the final Tester, every review executed for that candidate, and Learner unchanged. A passing reviewer from an earlier candidate may remain effective only through Fluent's host-owned changed-domain invalidation proof; it is carried-forward review authority, not a false claim that its Task context names the later SHA. The no-expertise identity gate checks only the final passing round's newly executed Tester and reviewer contexts, while review admission separately proves that every required role is either current or validly carried forward. Both modes reuse the same isolated-snapshot and Seatbelt confinement primitives (`HandoffOnlyWorkspace`, forced sandbox, denied live roots), but `work_task_executor::LearnerExecutionMode` keeps them separate: `PostLandHandoffOnly` baselines on the merged commit, while `PreLandNoExpertise` baselines on the reviewed Writer SHA in the candidate worktree and rejects — never discards-and-records — any candidate Git mutation. The crate-private enum is resolved from `(learner_mode, candidate_merged)`; a merged candidate always runs post-land regardless of the stored policy. The public `LearnerRunInputs { handoff_only: bool }` stays source-compatible (`false` = capture, `true` = post-land) and never expresses no-expertise, which is reachable only through the production adapter. A no-expertise Fargate launch is refused on the host before any side effect, since Linux has no trusted Fluent write boundary and a stale image could ignore the stored field. Prompt text alone is not authority: the host enforces the boundary. Do not "unify" the two modes into one Boolean or run pre-land no-expertise in the live candidate workspace.

---

## The no-expertise pointer-identity postflight settles atomically and per-field, and its concurrency test contradicts the reviewed-SHA pair together

The pre-land no-expertise Learner settles every terminal transition through a fresh, lock-held `mutate_work_item` (`settle_no_expertise_learner` and its helpers `prepare_no_expertise_handoff`, `publish_no_expertise_handoff`, `settle_no_expertise_failure`), not through capture's whole-aggregate `finalize_learning`. Each mutation re-reads the current aggregate, accepts only this runner's exact Learning frontier (`learning_frontier_is`), evaluates the postflight against that `fresh` state, and changes only the Learning record. The Attempt call sites (`interpret_reviews`, the resume path) deliberately skip their post-Learner whole-aggregate `write_work_item` for this mode; re-writing a refreshed snapshot would reopen the stale-write window. This is intentional and must not be "simplified" back into a single whole-aggregate finalizer for no-expertise. See [[fresh-field-level-finalizer-preserves-concurrent-state]].

The aggregate invariant `MergeCandidate.candidate_commit == latest completed Write output.commit` (`MergeCandidate::validate`) couples those two identity pointers, so no valid persisted aggregate can hold them apart. The concurrency test (`pre_land_no_expertise_concurrent_pointer_contradiction_fails_closed`) therefore contradicts the reviewed SHA by moving the Write output and Merge Candidate together off the live candidate HEAD, and contradicts a final review context independently. Do not flag the test for "not contradicting the Merge Candidate on its own" — an independent Merge Candidate contradiction is unreachable through a valid concurrent mutation, and the gate's Merge Candidate check is defended by that validation invariant.

---

## Keep the host-sandbox-only candidate exception during cancellation

`MergeCandidate::validate` requires a candidate's Attempt to be `Complete` with
reviews `Passed`; a `Failed` merge remains separately exempt. Its only recovery
exception permits `NeedsUser` / `HostSandbox`, because the Learner's sandbox
preflight runs after Fluent persists the reviewed candidate. Do not broaden
this structural exception to `Interrupted` or other pauses.

When canceling an Attempt with a persisted pending Merge Candidate, retain
`Complete`, Passed reviews, the candidate, and its original readiness timestamp.
Write an active typed `AttemptCancellation` (`resumed_at` is absent) before
signaling the registered Learner execution. That record is the scheduling pause;
do not attach an `Interrupted` pause kind to this structurally complete Attempt.
Other local Attempts continue to use `NeedsUser` / `Interrupted` with their stop.

Structural validity remains separate from advancement. Both readiness stamping
and `WorkItem::attempt_learning_advancement` reject active cancellation even if
Learning previously succeeded. The storage guard permits the Complete-state stop
only when the existing pending candidate remains unchanged, while rejecting
new Task launches, Learning advancement, and stale candidate-readiness writes.

Explicit resume must match the current cancellation authority and wait for
settlement. It consumes the stop without reopening completed review or Writer
work or changing readiness history, then may relaunch unfinished Learning.
After resume the shared Learner gate still requires succeeded Learning. Keep
`Attempt::learning_advancement_readiness` as the proof of prior Learning when
validating an existing readiness timestamp; a stop must not make that history
unreadable. See [[advancement-requires-a-succeeded-learner]].

---

## Recovery contracts form a neutral dependency boundary

Candidate-bound recovery admission and Writer completion checks live in the
neutral `recovery_contracts` module. Host-evidence classification, queue
admission, and Writer reservation may depend on that boundary, while the
boundary does not depend on any of those callers. This keeps the immutable
candidate, workspace, review, and host-evidence binding contract shared without
introducing a reciprocal executor-to-host-evidence dependency.

Lifecycle recognition feeds candidate and evidence facts into this boundary.
The neutral module must not invoke `lifecycle_recovery`, which can itself depend
on executor services. Authoritative completion selection and ownership-proof
validation are shared here so host attachment and later Writer materialization
cannot choose different source matrices.

## Saved ownership approvals preserve exact obligations and identities

An ownership proposal partitions each original required Plan row into ordered
parts whose text joins back to the original requirement byte for byte. Child
identities append `:part-N` to the original row ID, and every part retains the
source-row provenance and verification references. This intentionally requires
explicit text boundaries: adopting responsibility cannot silently summarize,
drop, optionalize, or reinterpret approved work. Changing the partition or its
identities changes durable approval and completion identity and needs a fresh
approval; it is not a serialization cleanup.

`completion_from` may reuse concrete passing completion only from an identical
Writer obligation. Host, review, Tester, and Learner parts always remain pending
in Writer matrices and use their separate evidence gates. Matching text alone
does not grant completion without the saved approved source and verification.
Future Writer admission consumes the approved mapping while preserving original
matrices and completed Task history.

Work record schema 2 is the mutation guard for readers predating responsibility
assignments. A package version still labeled 0.3.0 cannot provide that guard:
an older reader could otherwise ignore the new fields and overwrite approved
meaning. Retain actual pinned-older-executable refusal coverage when changing
this schema. Reading legacy records or upgrading their syntax does not infer or
approve ownership; legacy rows retain Writer responsibility until an explicit
saved-ownership approval adopts a split.

## Source-correction recovery fails closed on Writer lifecycle

Source-correction classification requires an immutable candidate-bound evidence
snapshot whose `result` is `fail`. Passing evidence follows the ordinary
host-only attachment path and cannot authorize a source correction. Exact
classification replay is idempotent only while the saved classified Writer
remains resumable. The supported resumable states are `Planned`, `Executing`,
or `NeedsUser` with no output, and `Complete` with output. A `Failed` or
`Superseded` classified Writer is not replayable. In either state, repeating
`classify` is rejected without mutation. For a `Complete` Writer with output,
replay retains the original source, review, and evidence bindings but validates
that saved output's workspace as the current clean candidate before returning
the stored result. For a `Failed` Writer, classification grants no retry
authority. Repeated `classify` still rejects a failed Writer, including after
usage-limit approval. Only the dedicated `attempt recover-usage-limit` approval
followed by a separate explicit resume can retry that same Task; unrelated
failed Writers remain ineligible. For a `Superseded` Writer, continue with its
replacement and do not repeat the classification.

This fail-closed policy keeps replay and queue admission aligned: a hard-failed
Writer has no implicit retry and cannot create a second corrective Writer.
No ordinary resume command makes that recovery eligible again. At classified
recovery terminal completion, the host may supersede exactly one historical
failed output-free Writer that the classification replaced; it preserves that
Writer as immutable history and never grants a second Writer through replay.

---

## Dashboard observation owns no durable transition cursor

Status and dashboard must leave the Work tree unchanged. Dashboard refresh
therefore derives its latest-transition view in memory from the same verified
published Work snapshot as its rows. The Work transition ledger owns the durable
history; dashboard observation owns no cursor, acknowledgement, or persisted
projection. Restarting the dashboard reconstructs this view from that ledger.
Do not restore writes to `transition-projection.json` during observation. Other
consumers that deliver durable effects, such as Slack, keep their own cursor
ownership independently of the dashboard.

---

## Verify pending Work snapshots before observation

New Work transaction journals persist `base_children_digest` to prove that stored
Attempts, Tasks, and Merge Candidates still belong to the published base root.
Read-only observation checks stable root and journal values around child assembly.
While a journal remains, the stored children must match that base digest or, once
the target root publishes, the target children. An unreadable journal or child
collection cannot establish absence or consistency.

Keep the digest optional when decoding legacy journals so lifecycle mutations
can still recover them under the model lock. Observation withholds a legacy
pending aggregate when it lacks the required proof, reports a bounded diagnostic,
and leaves recovery to its owner. It must neither recover the journal nor use its
contents as a replacement snapshot. Preserve this compatibility distinction when
changing journal writers or readers; see
[Serialize aggregate Work writes](work-model/patterns/serialize-aggregate-work-writes.md).

---

## Share verification policy below admission and execution

`verification_policy` owns checker digests, sandbox-content identity, and the
bounded-log limit shared by `host_evidence` admission and `verification` workers.
It depends only on content resolution and hashing. Neither admission nor worker
execution belongs in this neutral contract. `verification` may reuse confined
execution and attachment primitives in `host_evidence`; `host_evidence` must not
depend back on `verification`. Both continue to use the shared `execution`
supervisor for process ownership and cancellation. This keeps policy consistent
without making admission and worker orchestration depend on each other.

---

## Keep nested commands under their Task's execution owner

`work_task_executor::run_task` installs the scoped `execution::TaskOwner` guard
for the Task's managed transcript. The guard stores that path in thread-local
state, restores the previous scope on drop, and makes nested synchronous
commands use the Task's execution record even when they supply another
transcript path. Baseline checks, Tester commands, and extractors therefore
retain the reserved Attempt owner and cannot escape its durable stop between
children. Heartbeats and command transitions preserve that owner binding.

Synchronous execution on the owning thread is part of this contract. A newly
spawned thread does **not** inherit `TaskOwner`. Any future move of nested command
execution to another thread must explicitly propagate the owning transcript and
install its scope there before launch. Keep each guard on the thread where it
was entered; retain the single-record launch and settlement discipline rather
than assigning concurrent children to the same record without an ownership
protocol. Passing a different transcript without that scope loses attribution.

Acquire the Work model lock **before** the execution lock. Check the shared
Attempt cancellation while holding the model lock, then retain both locks
through child creation and identity publication. Release both before waiting
for the child or cleaning up a failed launch. This order serializes reservation,
launch, and cancellation without making cancellation wait for worker completion;
a reserved peer observes the shared stop even before cancellation visits its
own execution record. Preserve it when adding nested or parallel work.

---

## Resolve Python source selectors without execution

The candidate gate intentionally uses a limited Rust text scanner to resolve
class-qualified Python selectors from committed declarations. It does not import
candidate Python or resolve inherited or dynamically assigned methods. Keep
this cheap source check separate from the final Tester's required passing
evidence. See [[python-test-selectors-use-static-class-ownership]] for the
supported declaration boundary and
[the architecture documentation](../../documentation/architecture.md)
for source and evidence matching.

---

## Assess verification below lifecycle consumers

`verification_reference` owns declared reference meaning and native identity
normalization. Structured producer identities must retain every qualifier in
the raw identity; missing or conflicting paths, binaries, modules, or classes
cannot weaken the match. `verification_assessment` owns the read-only current
requirements, canonical command occurrences, failure causes, proof bindings,
and accepted-evidence adapters. Candidate gating, Writer findings and context,
status, and recovery admission consume that shared result. Keep scheduling,
publication, and host execution in their existing lifecycle modules.

Missing current evidence does not authorize a corrective Writer. A changed
candidate may still run its ordinary reviews and canonical Tester, and a bound
pending evidence Review may run while unrelated references remain unresolved.
Known native failures and project-owned extractor defects retain correction;
unreadable results and host infrastructure errors require diagnosis. Accepted
legacy recheck proof covers its bound canonical command occurrence only, under
default candidate-code applicability and the original comparison context.
See `corrective_writer_views_preserve_current_proof_and_history` and
`changed_candidate_resumes_through_reviews_and_tester` in `tests/binary.rs`.

## Pin ordinary Review proof without creating acceptance authority

Save the accepted-proof selection in existing Task artifact references so an
ordinary Tests Review receives stable inputs. The content-addressed summary
pins those inputs; fresh source validation grants acceptance. Sources added as
historical proof provenance do not reassign their findings to the Review.
See [host evidence recovery](../../documentation/architecture.md#host-evidence-recovery)
and [[accepted-proof-selection-revalidates-original-sources]] for the contract.

## Canonical check identity includes configuration and position

Use `tester:<tester.yaml-sha256>:<zero-based-command-index>` to select a canonical
command. Command text alone cannot distinguish duplicate entries. Acceptance
also binds the exact candidate, checker inputs, immutable producer artifacts,
and targeted Tests Review; the selector alone never grants acceptance.
See [host evidence recovery](../../documentation/architecture.md#host-evidence-recovery)
and `canonical_tester_replacement_covers_attributed_failure` in `tests/binary.rs`.

## Reuse reviewed command proof only for unchanged inputs

`verification_assessment` attributes native failures to an accepted canonical
command only when its harness is unique in the ordered configuration. Repeated
harnesses require the exact synthetic command identity. Apply that coverage to
named references in a derived view; retain the original Tester result unchanged.

`tester_evidence` may carry reviewed proof from its original candidate to a
descendant only when every changed path is under `.fluent/expertise/`. Require
the same ordered commands, harnesses, configuration and extractor, only
originally covered failures, and identical resolved applicability, including
comparison commits.
Revalidate the original artifact bindings, targeted Tests Review, environment
requirements, and freshness. `work_merge_executor` includes the landing
`canonical-run.json` in the evidence digest; `verification_assessment` checks
that binding when inspecting history. Use the inputs recorded for that run,
not mutable refs resolved later. No original matrix, Tester result, or host
proof is rewritten to grant credit. See the
[architecture documentation](../../documentation/architecture.md#core-work-model).

## Legacy reconstruction proves only default candidate-code checks

Reconstruct an absent canonical manifest only from a completed candidate-bound
Tester whose ordered command results exactly match committed configuration.
The supported legacy shape has default candidate-code applicability. Never
invent historical environment facts, comparison values, or live-state expiry.
An authorized recovery publishes the deterministic audit and derived manifest
without changing the source result; status only derives the same view.
See [host evidence recovery](../../documentation/architecture.md#host-evidence-recovery)
and `legacy_evidence_reuses_proven_bindings_without_host_rerun` in `tests/binary.rs`.

## Evidence authority fences older record and transaction writers

Work and Attempt record schema 4 and transaction schema 3 prevent older
executables from removing explicit verification meaning, required-check
acceptance, or recovering a newer pending transaction. Writer envelopes,
completion contracts, and completion matrices use version 2; candidate gates
use schema 5, and execution contexts containing verification assessments use
schema 2. These extend the earlier record-3/transaction-2 authority fence.
Current readers retain supported older records and journals through bounded
compatibility adapters without inventing qualifiers or historical proof.
Compatibility must use the pinned pre-change executable, including an interrupted
real transaction, rather than model round trips alone.
See [host evidence recovery](../../documentation/architecture.md#host-evidence-recovery)
and `tests/behaviors/operations/test-evidence-recovery-old-reader.sh`.

## Settle older Plan Reviews only during fresh attachment

Only fresh explicit Plan evidence attachment may settle a completed Review for
an older candidate as history. Validate its original Writer, matrix, evidence,
and Review, including any saved Review digest. Ordinary resume and current
completion still require the current candidate and matching Plan requirement;
historical settlement cannot supply that candidate's fresh proof. See
[[host-evidence-settles-against-bound-writer]] and
[host evidence recovery](../../documentation/architecture.md#host-evidence-recovery).

## Limit legacy Plan Review fallback to default applicability

A preserved Plan receipt with default applicability may lack a saved Review
digest. Acceptance still requires the completed Tests Review's matching context
and passing verdict. Receipts with declared applicability require the saved
digest, and any present digest must match the Review bytes. Keep this compatibility
fallback bounded when changing Plan evidence overlays; do not extend it to
environment, comparison, or live-state policies. See
[host evidence recovery](../../documentation/architecture.md#host-evidence-recovery)
and `plan_evidence_review_is_accepted` in `src/recovery_contracts.rs`.

## Failed-review round-cap Approach revision remains an audited delta

The supported failed-review round-cap path requires the operator to authorize a
matching bounded extension first. `fluent attempt revise-approach` may then
accept a replacement Approach even when Architecture passed, but only when the
audit binds the exact candidate, extension, Architecture-pass and failed-review
identities, review bytes and digests, prior and replacement Approach, and
authorization. The extension remains a separate admission record; the revision
does not silently increase the cap.

Journal the audit as a managed file and publish its reference, revised planning,
Writer completion contract, recovery binding, and one corrective Writer through
the Work Item aggregate transaction.
Use the same validator for foreground and queue resume, and make exact retries
reuse the existing revision and Writer. Interrupted publication must recover
from the journal without exposing or combining partial authority. Preserve all
non-Approach requirements, ownership, reports, host evidence, review verdicts,
and completed Tasks; do not launch execution while recording the revision.

Keep the older `UncertainArchitecture` audit kind and historical replay
semantics unchanged. The installation integration supersedes the earlier
exclusion of assignment-aware formats: this coordinator now models their
corrective assignments, carried findings, and separately authorized retries
alongside the failed-review round-cap audit. Retain bounded adapters for those
modeled records and journals. Combined records use schema 10, writer envelopes
use schema 9, and transactions use schema 10. The output-free predecessor could
otherwise rewrite the combined aggregate, and neither repair predecessor models
managed Approach-revision files in pending journals. The current reader models
envelope schemas 1 through 9 and transaction schemas 1 through 10; each retained
recovery validator still checks the authority present in those older formats.
Pinned predecessor readers must refuse combined
authority in both published and pending states, and read-only legacy observation
must not recover or combine partial state.

---

## Preserve corrective Writer assignments independently of current verification

An operator may select one carried Tests finding with `attempt evidence attach
--finding` when the latest completed Writer has finished every other Writer row.
The typed binding on `EvidenceRecovery` preserves the original assignment and
Review at candidate A while binding new proof and one targeted Tests Review to
the current clean managed candidate B. The original Review needs one structured
check request, without an evidence-targeted disposition marker. Selection never
authorizes host execution or a general change of responsibility.

`recovery_contracts/carried_finding.rs` supplies the shared source validation and
accepted-finding overlay. Passing proof and a passing Review satisfy that finding
in memory; historical matrices and outputs retain their original claims. Keep
this acceptance out of `verification_assessment` command coverage and ordinary
review-domain credit. The remaining review, Tester, gate, and Learning obligations
still apply. Exact retries revalidate immutable bytes and the saved operation,
including under the model lock before interrupted publication recovery. Fresh
proof preserves expired or failed history; old actions cannot clear a later stop.
See `carried_finding_evidence_*` in `tests/binary.rs` and the host recovery guide.

A corrective Writer receives a bounded, immutable assignment on its host-owned
Task record before launch. `WriterCorrectionAssignment` binds the Work Item,
Attempt, Writer Task, source Writer Task, source candidate, finding identities,
requirement text, and the source Task and evidence artifacts with their
digests. `recovery_contracts` validates that record and supplies its findings
to completion checks; the Writer's matrix and generated context remain claims
or instructions, not assignment authority.

An incomplete completion frontier creates a pre-review continuation of the same
Writer session, so that Task carries the source assignment unchanged apart from
its Task identity and the proven context-freezing exception below. A later Writer
whose inputs contain new Review or Tester correction evidence starts a new
corrective round and captures a fresh assignment for that source Task,
candidate, findings, and evidence.

The corrected candidate still has a separate verification frontier. The source
candidate's Tester result and the assigned Writer rows do not satisfy fresh
candidate checks. Once the preserved assignment and other pre-review gates pass,
the Attempt may plan ordinary reviews before the complete final Tester; review
admission does not make the candidate verified or ready to land. Setup rollback
restores only the reservation-owned execution state and retains the published
assignment, while retries reuse the same record.

Older saved Writers may lack the field. Foreground and queued resume can publish
a `LegacyReconstructed` assignment only after the read-only inspector verifies
the exact completed source Writer, candidate, matrix, evidence, clean worktree,
and settled Task ownership. It does not trust the saved matrix or current
assessment to invent historical findings, and it does not reconstruct missing
applicability or accepted-proof facts. Status and show never publish this
assignment. Changed, mixed, contradictory, or live-owned histories remain
paused with a specific diagnosis.

Work and Attempt record schema 10, writer envelope schema 9, and transaction
schema 10 preserve carried-finding authority, corrective assignment snapshots,
and usage-limit approvals together. The three pinned predecessors share record
schema 7 but cannot each retain the other branches' authority; the combined
fence must exceed every predecessor's recognized limits.
The pinned b5d8bf02 reader rejects the new binding in published records and real
interrupted public transactions, with independent inner-fence checks. Earlier
record-6/envelope-4/transaction-5 fences introduced the assignment field. The pinned
older readers must refuse both published assignment records and genuinely
pending assignment transactions without changing protected bytes. See
`corrective_writer_assignment_survives_candidate_change`,
`saved_writer_assignment_recovers_through_public_resume`, and
`writer_assignment_old_reader_reaches_inner_fences`,
`writer_assignment_old_reader_refuses_pending_transactions` in
`tests/binary.rs`.


## Freeze mutable context and recover proven original bytes

Corrective assignment schema 2 freezes only recognized Work/Attempt progress
and host handoff inputs at assignment time. Bind each snapshot to its original
source reference and digest through immutable provenance. Keep source Tasks,
completion matrices, findings and other evidence unchanged; schema 1 remains
readable. Writer envelope schema 9 fences executables that predate these
snapshot semantics. Carried-finding bindings retain their original assignment
when context recovery freezes it later. Validate the complete operation-bound
audit chain before accepting that original identity; never rewrite the evidence
binding or infer acceptance from a snapshot alone.

A continuation may freeze a carried legacy context binding without changing its
original Work, Attempt, source Task, source candidate, findings, or immutable
evidence. `recovery_contracts` validates the exact snapshot path and original
digest, provenance path and bytes, and any recorded context-recovery audit.
Interrupted restoration uses that same proof to validate the predecessor after
live progress changes. Preserve every historical assignment; this exception
permits only the recognized context representation change and receiving Task
identity. It grants no completion credit or permission to resume a canceled
Attempt.

An explicit context recovery may replace one saved mutable binding only after
proving the original bytes, exact assignment, clean candidate and settled
ownership. It also admits an output-free Failed Writer whose saved completion
authority binds the latest completed source and whose native execution identity
proves its setup never launched a provider. At most one later unstarted Writer
reservation may remain, with no materialized artifacts. Context recovery keeps
both Tasks and all lifecycle statuses unchanged; it grants no retry authority. The deterministic request path locates the before/after
audit; the recovery operation separately binds the complete audit bytes. This
separation preserves request lookup after publication without allowing changed
audit fields to reuse an operation identity. The store permits only the audited
substitution and retains all other assignment and lifecycle authority.

New Work/Attempt records use schema 10, writer envelopes 9, and transactions 10
to fence readers that cannot retain the output-free failure audit.
Context-recovery journals use schema 10 and retain their authorized base aggregate
with its children digest. Replay validates the original audited change against
that base and accepts each stored child only if it matches the base or target.
This permits interrupted Attempt and Task publication without treating mixed
records as a fresh authorization. Other mutations also write journal
schema 10; readers retain the modeled predecessor journals through schema 9. Status and
show never reconcile; the exact matching recovery can do so under the storage
lock. Recovery neither launches work nor supplies resume,
Review, Tester, host-evidence or merge authority, and replay retains later stops.

See [the storage architecture](../../documentation/architecture.md) and
[the complete-audit publication pattern](persistence/patterns/bind-complete-recovery-audit.md).

## Keep saved Writer history aligned with stagnation recovery

The last three durable Writer runs and their completed Tasks establish admission
to recover a stalled committed Writer. Preserve their ordering and candidate
bindings: the latest run must be blocked, the last two runs must be continuations
or corrective work, and all three matrices must validate against the immutable
contract with identical obligation identities and completion counts. The latest
matrix must still contain pending Writer-owned work. A Writer-completion pause
label alone grants no retry authority.

Treat these saved fields as a recovery contract when changing the stop rule,
Writer-run recording, or matrix identity. Keep the guard and
[`validate_stalled_committed_writer`](../../src/writer_completion_recovery.rs)
in agreement, and retain the shared eligibility check for status and repair.
See the [completion recovery architecture](../../documentation/architecture.md#core-work-model).

## Require separate approval to retry a quota-failed Writer

The source-correction decision still refuses implicit retries of a failed Writer.
The dedicated `attempt recover-usage-limit` operation is a narrow exception: it
validates the surviving native Codex terminal stream, preserved correction
assignment, settled ownership, and exact partial source state before recording
operator approval. A separate resume retries that same Task once. The source
Writer output and partial HEAD remain distinct identities. Retained failure and
Review bytes and the host-owned audit stay outside the retry's writable roots.

Keep immutable inspection in `usage_limit_contracts` below its queue, lifecycle,
status, persistence, and executor consumers. Publication belongs to
`usage_limit_recovery`. Work records now use schema 10, the writer envelope 9, and
transactions 10 to keep pre-change executables from discarding retry authority.
The approval supplies no completion, host-evidence, review, Tester, or Learning
credit. A later cancellation or terminal failure cannot reuse it to restart.
See the [architecture](../../documentation/architecture.md#recover-a-usage-limit-interrupted-writer)
and the `usage_limit_writer_` public-route and compatibility tests.

When a quota retry retains completion-recovery instructions,
`usage_limit_contracts` reconstructs the original instructions through recorded
approvals, requiring each matching artifact area, Task identity, instruction
string, input list, and preparation operation to agree. The shared completion
contract validates those original instructions against completion authority
while delivering the full audited instructions to the Writer. Keep this rule
with those owners; consumers must not strip prose to infer authorization. See
[[composed-recovery-instructions-bind-each-preparation]].

## Preserve transition history as preflight recovery proof

Derive eligibility to reopen an approved Writer's pre-reservation host-sandbox
pause from existing append-only transitions. Keep the external preparation reason
`prepare approved usage-limit retry`, Task state spellings `failed`, `needsuser`,
and `planned`, and preparation time at or after this approval's PREPARE operation
completion time.
The latest Task transition must come from `TaskExecutor` and move `planned` to
`needsuser`. Its paired Attempt transition must come from `TaskExecutor`, move
`planned` or `executing` to `needs-user`, and carry reason `host-sandbox`. The
Attempt entry must follow preparation and immediately precede the Task entry
by sequence, with the same timestamp.

These details bind the pause to explicit preparation and one native executor
mutation. Preserve them when changing transition emission; treating the reason,
state spelling, ordering, or timestamps as display-only would break recovery
compatibility. Existing records suffice alongside the unchanged Task and approval
bindings, absent start/launch and execution evidence, and settled ownership.
Missing or conflicting proof rejects recovery. Do not add a second durable
marker or infer authority from the pause label alone.

See the [retry architecture](../../documentation/architecture.md#recover-a-usage-limit-interrupted-writer)
and [public-route regressions](../../tests/binary.rs):
`usage_limit_writer_preflight_pause_requires_compatible_history` covers the ledger
details, `usage_limit_writer_preflight_pause_rejects_unproven_state` rejects missing
or conflicting proof, and `usage_limit_writer_preflight_pause_resumes_same_task`
and `usage_limit_writer_preflight_pause_repeats_without_consuming_approval` retain
valid recovery without a new approval or Writer round.

## Derive ordinary review coverage from scoped Task history

Use completed Task contexts and verdicts to establish every ordinary review
domain. Targeted evidence acceptance supplies only its declared scope. Retain
ordinary passes only across unchanged domains at every intervening Writer
delta, and schedule missing coverage through native review planning before
final Tester or unfinished Learning. Keep historical Tasks unchanged instead of
adding a second durable coverage authority. See
[[ordinary-review-coverage-follows-writer-history]] and
[the Work model architecture](../../documentation/architecture.md#core-work-model).

## Separate installed tools from private Writer state

Scoped Writer retries resolve installed tools and exact Git configuration reads
in the executor, render read/executable-map grants and installed-tool write
denials in `os`, and keep mutable dependencies and scratch in `writer_cache`.
Let Git evaluate conditional includes in the retained Writer worktree before
granting their exact files; configuration parent directories receive no grant.
See [[scoped-writers-separate-tools-from-mutable-state]], the
[conditional-include pattern](security/patterns/evaluate-conditional-git-includes.md),
and [the retry architecture](../../documentation/architecture.md#recover-a-usage-limit-interrupted-writer).
---

## Authorize one retry after recovering failed setup context

A failed completion retry needs two separate authorities: an operation-bound
schema-2 context audit and a fresh completion-retry audit. The latter retains the
original failed Task and evidence, and binds an exact prepared version of that
same Task with a fresh artifact directory. Use a stable per-Task audit path;
a request-dependent path would permit conflicting approvals after an interrupted
audit write. Resume, Task reservation, and spawn validate the same neutral
contract. Record one start and one launch marker while holding the model lock.
Do not reuse quota, authentication, round-extension, or generic failure recovery
authority to admit this case.

When failed preparation reserved a new launch generation in a reused Task
area, a well-formed receipt for a different generation remains historical
evidence. Admit the completion retry only if the current launching record has
no leader or process group and the receipt cannot belong to its generation.
Reject a matching or malformed receipt because launch outcome remains
ambiguous. Preserve the earlier receipt and bind its bytes in the retry audit;
resume rejects a changed receipt. See
[[local-execution-identity-requires-receipt]].

Retain the base aggregate in retry journals and accept only exact base or target
children, while revalidating the pinned files before replay. The immediate
combined predecessor at `43022b29fa437ade211fef3fff66798c5d940ef9` must reject
record-10/envelope-9/transaction-10 state before mutation. Keep all earlier pinned
reader preparations in the canonical Tester. The real confinement command is a
separate host gate; routine profile assertions cannot supply its evidence.

## Hold reference identity through interrupted restoration

Restoration validates an inherited correction assignment against its original
source and exact continuation chain independently of the latest completed
Writer's saved candidate. It never rewrites the assignment to match that target.
The immutable intent records both identities; replay preserves earlier schema-1
intents only within their original same-source contract.

Hold the worktree's symbolic HEAD in a prepared Git transaction and update the
candidate through a separate prepared transaction in the common repository.
Git rejects combining HEAD verification with its referent's update because the
update also implicitly updates HEAD. The candidate transaction must use
`no-deref` with expected commit IDs for the source, retained, and candidate refs,
then verify that all remain direct while their locks are held. Only commit after
those checks pass. `no-deref` by itself can replace a symbolic ref and therefore
does not prove that the candidate's identity stayed unchanged.

Close transaction input on failure to abort without modifying substituted refs
or publishing a recovery operation. EOF also releases Git locks after coordinator
exit; normal cleanup reaps each child. Preserve the existing native ownership
locks, immutable intent, exact evidence checks, cancellation, and replay gates.
See [the prepared Git transaction pattern](persistence/patterns/hold-git-head-during-restoration.md)
and `candidate_ancestry_recovery::interrupted::PreparedRefs`.

## Keep Discord conversations and authority independent

The local Discord runner supports one configured private server and selected
text channels. Each public thread binds one checkout identity and one provider.
Owner state stores routing and ingress; authoritative accepted history, provider
sessions, pending turns, approvals, and delivery receipts live under the
checkout's `.fluent/work/discord/` tree. Publish the initial project snapshot
before the routing index. An indexed thread whose snapshot is missing fails
closed; it cannot become a new conversation. Failure reporting requires that
same snapshot and status reports restoration guidance without creating state.

Conclusive action rejections use a distinct error type so history recovery can
advance past them and the inbox can consume them. Save their input identities
in the project snapshot; later authority changes cannot make rejected messages
execute on redelivery. Keep actual processing failures and incomplete accepted
approval receipts pending for reconciliation.

`src/conversation.rs` owns the shared provider and Work contracts. Discord reuses
the existing provider implementations, scheduler client, and `WorkControl` from
the Slack module. Those implementation locations do not confer Slack identity:
Discord selects a separate runtime namespace, request prefix, and actor identity.
Keep Slack's persisted schemas and default `WorkControl::new` behavior stable.
Provider authentication and sessions use private state per Discord thread so a
conversation never implicitly inherits the owner's general provider session.

Each thread owns one deterministic Work Item, `discord-<thread-id>`. Approving
its Brief creates proposed Work; execution needs a separate approval after all
planning stages. This avoids ambiguous selection among multiple Work Items
inside one conversation and makes creation retries reconcile the same identity.

External approvals bind an action identity and observed Work fingerprint. A
Discord receipt records acceptance, not proof of effect. Planning and execution
validate the fingerprint under the Work model lock and journal an immutable
operation receipt with the effect. An existing receipt suppresses the effect
without overwriting later revisions. Resume validates both Work and queue state
under their existing locks, and saves the operation identity on the admitted
dispatch; replay cannot authorize a subsequent cancellation. Landing checks the
approved revision at the host admission boundary and continues through the
existing candidate, evidence, cancellation, and durable landing-intent checks.
Incomplete stale approvals fail closed and preserve newer pending controls.

Production Gateway orchestration accepts injected providers and bounded loopback
transports for deterministic tests. Admission, durable ingress, prompt REST
acknowledgement, sequence persistence, worker scheduling, and attribution remain
production code in those tests. Credentials stay in the host transport and never
enter durable inputs or provider requests.
