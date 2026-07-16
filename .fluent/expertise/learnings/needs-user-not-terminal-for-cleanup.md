---
name: needs-user-not-terminal-for-cleanup
description: NeedsUser attempts are not terminal for cleanup — only Complete and Failed are reapable
metadata:
  type: gotcha
---

`cleanup.rs::attempt_is_terminal` treats only `Complete` and `Failed` as terminal states. `NeedsUser` is deliberately excluded so that paused attempts survive cleanup and can be resumed later.

The exception is abandoned work items: a `NeedsUser` attempt on an abandoned work item is still cleanable via the `work_item_has_no_active_execution` path. This is correct — explicit abandonment overrides the resume-safety guarantee.

When adding new attempt statuses or changing the lifecycle, verify that `attempt_is_terminal` still excludes any status that represents a resumable pause.
