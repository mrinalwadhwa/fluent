---
name: no-change-completion-retry-binds-original-handoff
description: Retry an unchanged Writer's invalid handoff from its retained failure and host-owned baseline, without granting verification credit
metadata:
  type: architecture
---

An unchanged Writer with an invalid completion matrix or no-change handoff has
its own completion-repair authority. An output-free Task alone does not identify
that authority. The host requires the original malformed handoff, the
host-owned Writer baseline, the latest clean completed candidate, and any
carried correction assignment's source lineage. A retry that failed during
preparation also needs a native execution record proving that its provider did
not launch; an ambiguous launch cannot be approved again.

Approval retains the failure files and Task snapshot, then prepares fresh
outputs for the same Task. It carries only claims from earlier valid completion
evidence; malformed result text grants no passing credit. Approval and resume
are separate actions, and the resumed Writer still needs the ordinary host,
review, Tester, and Learner gates. Keep this route distinct from
assignment-context repair, which applies when the assignment context itself is
stale.

Related: [[writer-completion-repairs-are-incident-scoped]],
[[recovery-recognition-precedes-evidence-validation]],
[[old-reader-fences-use-recognized-wire-fields]].
