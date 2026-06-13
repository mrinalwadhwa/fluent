2026-06-09 — The first attempt to run independent peer Work Items in
parallel exposed a Work artifact namespace bug. Two Work Items
(`cleanup-empty-work-artifact-dirs` and `build-skill-work-default`) both
used `attempt-1` and review task ids such as
`attempt-1-review-documentation`, so reviewers from both items wrote to
the same `.factory/work/artifacts/attempt-1/...` paths. One review
artifact was overwritten with findings for the other Work Item. The
author commits were intact on separate branches, but review state was
not trustworthy. Before using peer Work Items in parallel, Work artifact
paths need to include the Work Item id, or another globally unique run
namespace, so attempt/task ids only need to be unique within a Work Item.
→ Resolved: `af7d61f` added `work_artifact_path(work_item_id, attempt_id,
artifact)` and routed task, attempt, and merge artifact construction
through it so new artifacts live under
`.factory/work/artifacts/<work-item-id>/<attempt-id>/<artifact>`.
`WorkModelStore` normalizes legacy `attempt-only` paths at the storage
boundary on read, and `1088f6e` documented the migration. Tests
`review_artifact_paths_include_work_item_namespace` and
`store_migrates_legacy_work_artifact_paths_on_read` lock in the new
layout and the migration.
