2026-06-09 — Work merge reviewers sometimes cannot read merge check or
prior artifact directories even when prompts mention them. During
`work-planning-bridge-cleanup`, multiple merge reviewers saw
`Operation not permitted` while trying to inspect merge check or review
artifact paths. Work merge review should either grant reviewers access
to the referenced artifacts or avoid prompting reviewers to inspect paths
they cannot read.
→ Resolved by the combination of slice-3 and earlier role-matched-input
artifact work. Slice 3 removed Attempt-time merge-time reviewers
entirely. The current two reviewer surfaces both handle artifact access
correctly: post-merge reviewers run with `no_sandbox: true`
(`src/post_merge_review.rs:297,397,415`) so they have unrestricted
reads, and Attempt-time follow-up reviewers' sandbox readable roots are
extended via `input_artifact_readable_roots`
(`src/work_task_executor.rs:1058`) for prompt-named input artifact
paths produced by `review_input_artifacts_by_role`
(`src/work_model.rs:2607`). No code change needed.
