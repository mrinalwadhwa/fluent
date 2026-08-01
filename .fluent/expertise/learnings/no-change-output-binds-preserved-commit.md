---
name: no-change-output-binds-preserved-commit
description: A follow-up Writer's no-change output must carry fresh Task-owned evidence and bind base_commit and commit to the same preserved candidate
metadata:
  type: architecture
---

A follow-up Writer may complete without a new commit only by proving that it
preserved the previous completed Writer commit. In the durable `TaskOutput`,
`commit` continues to name the candidate consumed by Tester and Reviewers, while
`no_change` records the typed verification evidence. A no-change output is valid
only when `base_commit` exists and equals `commit`; retaining `no_change` while a
later phase rewrites `commit` creates a contradictory identity and must fail
aggregate Work-model validation.

The evidence belongs to the Write Task that produced it. Its declaration path
must resolve to that Task's artifact area, its schema version must be supported,
and its reason and passing verification commands must be non-empty. The executor
accepts it only for a follow-up Writer whose workspace is clean and whose HEAD
still equals the preceding completed Writer commit. A Writer that advances HEAD
uses ordinary committed output and must not retain the no-change marker.

Treat the declaration as launch-scoped authority, not reusable state. Remove
only that declaration before every coder launch, including retries, and propagate
cleanup errors. Otherwise a failed launch can leave evidence that incorrectly
authorizes a later launch which performed no verification.

Cover the contract at both persistence and execution boundaries: model/storage
tests reject changed-commit, missing-base, wrong-kind, wrong-path, and malformed
evidence shapes, while a real Attempt-route test proves the unchanged candidate
SHA reaches Tester and every Reviewer. Keep the rendered follow-up Writer prompt
free of unconditional commit requirements so its instructions agree with the
host-enforced path.

Related: [[backward-compatible-serde-fields]],
[[mode-specific-prompts-replace-conflicting-base-instructions]],
[[route-tests-drive-real-launch-wiring]].
