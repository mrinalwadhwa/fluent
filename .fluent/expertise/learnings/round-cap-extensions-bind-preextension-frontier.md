---
name: round-cap-extensions-bind-preextension-frontier
description: Validate round-cap chains against approval-time Writer counts while keeping legacy readability separate from recovery authority
metadata:
  type: architecture
---

A `RoundCapExtension` is an authorization record, not a recalculated cap. When
Fluent creates one, it records the configured base cap, materialized Writer
count, and evidence-backed allowances observed before the extension. Validate
the first extension's old cap against that independent frontier, then validate
the bounded increment, derived identity, evidence accounting, and continuity
with every preceding extension. Run the pure chain check at `Attempt` model
validation so unrelated Work-model writes cannot preserve malformed authority.

Between extensions, the old cap must equal the greater of the preceding cap
plus newly earned evidence allowances and the Writer count at approval. The
current Attempt's Writer count cannot explain an earlier gap: Tasks created
after approval contribute only to later decisions. Keep structural validation
consistent with the effective-cap calculation, including when Writers already
exceed the preceding approved cap.

Use the explicitly recorded pre-extension count when present. Infer a legacy
count only when both explicit approval fields are absent and the chain needs
that count to explain a gap. Compare saved timestamps as instants, require an
unambiguous Writer creation order, and count only Writers created before
approval. The last such Writer must have completed between its creation and
approval and produced the approved candidate. Reject incomplete or ambiguous
proof with the affected extension identified. Contiguous legacy chains need no
timestamp reconstruction; they may predate those Task fields.

Keep this compatibility proof inside pure model validation. Ordinary reads
must preserve saved history without opening historical review files or
publishing a migration. Recovery still applies its independent authorization,
review-byte, candidate-workspace, cancellation, and live-owner checks.

The failed-review Approach-revision path and historical planning replay need the
stricter bound-frontier check. Legacy extensions without the newer fields may
remain readable for compatibility, but they cannot authorize this recovery.
Do not trust a coherently edited old-cap, new-cap, identity, base-cap, and
pre-extension tuple; a public rejection test must tamper those fields together
and prove that neither the aggregate nor its artifact tree changes.

Related: [[audited-planning-recovery-binds-exact-frontier]],
[[backward-compatible-serde-fields]],
[[semantic-validation-tests-reach-target-branch]].
