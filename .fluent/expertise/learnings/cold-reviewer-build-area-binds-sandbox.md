---
name: cold-reviewer-build-area-binds-sandbox
description: Select the Reviewer's build area and sandbox write grants together, then account for cold scratch output until settlement
metadata:
  type: architecture
---

Reviewer cache admission determines both the build paths in the prompt and the
paths the launched sandbox permits. When an existing shared cache is protected
but over budget, preserve its files and start the Reviewer in its Task scratch
area. The cold sandbox must withhold the shared cache's writable grant; a cold
prompt alone cannot enforce that boundary. An admitted warm Reviewer may write
the shared cache.

Count canonical build directories in cold Task scratch toward Reviewer usage
while the Task runs. After settlement, reclaim those targets under the scratch
lease while preserving Review evidence and private Cargo configuration. A route
test should inspect the sandbox passed to the launched Reviewer for both cold
and warm admission. Related: [[serialized-reviewer-cache-admission]],
[[canonical-reviewer-cache-cleanup]], [[route-tests-drive-real-launch-wiring]].
