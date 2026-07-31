---
name: commit-subject-line-fifty-char-limit
description: Commit subject lines must be 50 characters or fewer per project convention
metadata:
  type: convention
---

The project's stated commit-message convention caps subject lines at 50
characters. Descriptive module names and test lists easily push subjects past
this limit; the documentation reviewer counts characters and flags violations
even when they are otherwise clear.

Enforce this during drafting, not after: a subject like "Add scheduler service
foundation module with identity and registry" (62 chars) must be shortened to
"Add scheduler service foundation module" or similar before committing.
