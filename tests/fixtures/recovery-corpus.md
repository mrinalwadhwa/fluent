# Recovery replay corpus

This small, provider-free corpus records the recurring Fluent recovery failures
that motivated the shared pause records, recovery-operation records, and
explicit model upgrades. It intentionally contains synthetic identifiers and
relative placeholders rather than project checkouts, host paths, credentials,
or provider transcripts.

`src/recovery_replay.rs` loads the versioned JSON and replays every Attempt case
through the same candidate-bound operation validator used by public recovery
commands. Each replay verifies that:

- the saved command names the Work Item and Attempt;
- the current candidate is accepted;
- repeating the exact operation returns the original record without duplication;
- a stale candidate is rejected before mutation; and
- the original Task history and operation record survive serialization.

These are model validation and serialization checks. The corpus does not parse
or dispatch its saved command strings. Executed CLI coverage lives separately
in `tests/binary.rs`: `legacy_ownership_*` drives preview and approval, and
`interrupted_plan_host_evidence_recovers_same_attempt` plus
`host_plan_recovery_public_commands_are_idempotent` drive cancellation,
attachment, foreground resume, queue resume, and scheduler execution against
isolated Git repositories with controlled external providers.

`combined-evidence-recovery.json` preserves a sanitized historical shape: Plan
proof and Review, a failed complete canonical run, a no-change Writer, and a
later passing targeted Plan Review without a failed-command recheck. The binary
fixture hydrates the historical Writer into an isolated candidate and uses real
attachment, Tester and Review commands to bind machine-local identities. Once
recovery starts, it changes no model or artifact bytes to remove blockers.
`combined_evidence_recovery_resumes_to_ready_candidate_from_saved_history`
asserts that the later Review cannot hide the original canonical run.

`combined_evidence_recovery_resumes_to_ready_candidate` generates its Work Item,
initial Writer, host attachment, canonical failure, reviewed correction and
no-change Writer through public commands. Current behavior retains the accepted
Plan proof, so it needs no redundant Plan attachment or second Plan Review.
Both sequences finish through production targeted Review and Learning. The
current sequence cancels after Review settlement, repeats queue resume, and
uses isolated production dispatch to finish the same Attempt. Assertions retain
candidate identity and exact result, matrix and Review bytes, and compare run
markers to exclude repeated canonical commands. Provider doubles supply external
coder output; they do not replace admission, acceptance or readiness decisions.

The `approach-revision-round-cap` case records the supported new shape: a
failed-review `round-cap` pause, an explicit replacement-Approach command,
bound authorization and input digest, and preserved extension, Approach,
settled-review, and corrective-Writer history. Its replay uses the same generic
candidate-bound operation checks without launching or dispatching the command;
the public binary tests cover the real extension, revision, retry, and resume
routes.

The `unchanged-writer-invalid-completion-retry` case records a failed
preparation after an earlier approved completion repair. The retained launch
receipt belongs to the earlier generation; the current execution generation
has no matching receipt. The binary regression drives public approval and
resume, and checks that the old receipt remains byte-identical.

`writer-assignment-mismatch` records the saved shape addressed by this repair:
the completed corrective Writer changed its candidate, while its original
finding assignment and source evidence remained the obligations to reevaluate.
The offline replay checks only candidate binding, exact operation replay, stale
candidate rejection, serialization, and logical history preservation. The
production paths are covered separately by
`corrective_writer_assignment_binds_source_evidence`,
`corrective_writer_assignment_survives_candidate_change`,
`corrected_candidate_requires_fresh_verification`,
`corrected_candidate_reaches_reviews_before_final_tester`,
`corrective_writer_assignment_rejects_invalid_authority`,
`saved_writer_assignment_recovers_through_public_resume`,
`saved_writer_assignment_resume_rejects_unsafe_state`,
`saved_writer_assignment_recovery_preserves_history`, and
`saved_writer_assignment_resume_is_idempotent` in `tests/binary.rs`; those tests
exercise assignment publication, fresh verification, and foreground or queued
resume against isolated repositories. The corpus does not execute its command
string or replace that production coverage.

`usage-limit-interrupted-writer` records a failed corrective Writer with a
surviving final native quota stream, saved assignment, and partial committed and
uncommitted source. `candidate-commit` remains the completed source Writer's
output; the input digest represents the separate typed approval, including the
partial HEAD and source state. There is no attached host-evidence digest. The
shared replay checks operation identity and retained history only. The
`usage_limit_writer_` tests execute dedicated approval and separate resume,
rejection, publication interruption, queue/claim, confinement, and old-reader
compatibility through real public routes. No fixture copies live Work, leases,
credentials, or provider sessions. Approval grants no later gate credit.

`usage-limit-preflight-pause` preserves the approved retry whose private Codex
readiness failed before reservation. It names the unchanged source, assignment,
approval, preparation, and native Task/Attempt transitions without copying live
operational state. The corpus verifies operation binding and retained logical
history; it does not prove host confinement or interpret a pause label as launch
authority. The `usage_limit_writer_preflight_pause_` binary tests exercise native
preflight failure, foreground and queued resume, scheduler claim, direct Task and
pre-spawn checks, rejected proof, later stops/failures, and read-only guidance.

Workflow cases document public reconciliation commands whose state lives outside
an Attempt, such as an already-landed Merge Candidate. Their command and history
are still checked for safe, machine-independent shape.

Add a new case when a recovery failure reaches a user-facing handoff. Keep the
description short, use synthetic commit IDs, and update the schema only when
the replay meaning changes. This corpus is not a replacement for the focused
module tests that exercise each recovery's Git and artifact checks; it is the
stable cross-recovery regression set that catches missing or stale next actions.
