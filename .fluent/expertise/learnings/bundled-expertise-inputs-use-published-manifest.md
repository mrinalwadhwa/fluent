---
name: bundled-expertise-inputs-use-published-manifest
description: Collect bundled worker expertise from its published file manifest so concurrent atomic writes cannot expose temporary inputs
metadata:
  type: gotcha
---

Parallel Tasks materialize bundled expertise under `.fluent/work/expertise`
through atomic replacement. A recursive scan of that directory can discover
another Task's temporary file before publication. If the collector declares
that path as a worker input, the file can disappear before sandbox preflight
opens it and prevent an otherwise valid Task from starting.

Collect bundled expertise through `content::GENERAL_EXPERTISE_FILES` in
`declared_worker_input_files`, rather than enumerating every directory entry.
Keep this fixed bundle manifest separate from the recursive collection of
project-owned expertise and planning files.

`declared_inputs_exclude_unpublished_general_expertise` in
[`src/work_task_executor.rs`](../../../src/work_task_executor.rs) holds a real
staging file open during collection, then removes it and verifies that every
declared bundled input remains readable. This exercises the overlap without
depending on thread timing.

Related: [[atomic-write-replace-through-utility]],
[[publish-managed-inputs-with-model-record]].
