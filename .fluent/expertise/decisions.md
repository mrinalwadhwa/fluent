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

## The transcript pump's console renderer and config are process-wide

The `transcript_pump` module renders console previews through a single
process-wide bounded renderer (`console_preview_sink`, a `OnceLock`) and reads
its thresholds from a process-wide installed config (`install_config` /
`active_config`, a `Mutex`). This is deliberate, not a hidden global smell.
`Coder::run`'s signature is kept stable for non-transcript callers, so the pump
cannot take per-call config through the trait; the executor resolves the layered
thresholds once per project (`install_transcript_pump_config`) and installs them
before launching a coder. One renderer for the whole process is the point: a
blocked console must not accumulate one stuck thread per Task, and previews are
dropped (`try_send`) rather than backpressuring capture. The renderer thread is
never joined so a blocked stderr cannot keep the process alive at shutdown. Do
not "fix" this by threading config through `Coder::run` or by spawning a renderer
per pump.

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
