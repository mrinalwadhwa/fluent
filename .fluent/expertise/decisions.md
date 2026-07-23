# Decisions

Architectural and design decisions that are intentional and should not be flagged by reviewers.

---

## capture-brief Phase 3 keeps cognitive science inline

The capture-brief skill includes cognitive science principles (anchoring bias, framing effects, etc.) directly in the skill content rather than referencing an external expertise file. This is intentional: agents are more likely to read and apply material that appears inline within the skill they are following than to follow a reference to a separate file.

---

## Skills are bundled in the binary and materialized on demand

Skills live in the `skills/` source directory with `references/` symlinks to expertise files. At build time, `build.rs` walks the tree, dereferences symlinks, and generates a `BUNDLED_SKILL_FILES` constant. At runtime, `materialize_skill()` writes the bundled content to disk with atomic writes. Review skills materialize to `.fluent/work/skills/` for reviewers; the `fluent` interactive skill installs to `~/.claude/skills/` via `fluent skills`. Skills reference `references/X.md` in their SKILL.md, never `expertise/X.md` directly.

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

One shared predicate — `Attempt::learning_advancement_readiness()`, surfaced through `WorkItem::attempt_learning_advancement()` and `MergeCandidate::validate_advancement()` — gates every advancement boundary: Attempt readiness (`MergeCandidateReady`), Merge Candidate validation, and the land route. A candidate may advance only once its Attempt's Learner has SUCCEEDED; any other state (absent, `InProgress`, `HandoffPending`, or failed whether relaunchable or not) blocks with one durable reason, `WorkModelError::AttemptLearningNotSucceeded`. This deliberately replaces the earlier "land, then retry the Learner post-land" behavior, which let failed or prepared learning reach `MergeCandidateReady` and land. Do not re-add a boundary that advances over a non-succeeded Learner, and do not fold the readiness check into the structural `MergeCandidate::validate` — it is kept separate so a candidate persisted *before* its Learner runs stays valid (the candidate is created before `run_learner_step`).

Because landing now requires a succeeded Learner, the post-land handoff-only Learner *retry* path (`work_attempt_loop`, guarded by `learner_is_handoff_only` on a Merged candidate) is a recovery/legacy path, not the normal flow: a candidate reaches Merged only with a succeeded Learner, so a Merged candidate with a pending Learner comes from a legacy landing or an interrupted post-land handoff, not fresh advancement. `validate_advancement` therefore exempts Merged/Failed candidates so idempotent post-land follow-up processing still resumes. A pre-land Learner block is surfaced as the dedicated `WorkAttemptRunOutcome::LearnerNotReady`, not `FollowUpRecoveryPending` (whose CLI text hard-codes "is merged" and only fits a genuinely landed candidate). Do not flag the two outcomes as redundant.
