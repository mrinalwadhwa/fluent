---
name: commit-body-bullet-point-convention
description: Commit bodies that list multiple changes must use "- prefix" bullet points, not prose paragraphs
metadata:
  type: convention
---

CLAUDE.md specifies that commit bodies use `- prefix` bullet points when listing
multiple changes. Prose paragraphs describing a sequence of changes fail this
convention even when the prose is accurate and clear.

Each bullet should correspond to one logical change; the body is not a narrative
of what was wrong but a structured list of what was modified or added. Combined
with the 50-character subject-line cap, this keeps commits scannable and
reviewable.

Related: [[commit-subject-line-fifty-char-limit]]
