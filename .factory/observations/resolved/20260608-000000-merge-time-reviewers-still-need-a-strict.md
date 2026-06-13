2026-06-08 — Merge-time reviewers still need a stricter Work-native,
read-only contract. During the `work-planning-artifacts` merge candidate,
the merge-time behavior reviewer received legacy `.factory/runs/...`
instructions even though the Work merge artifact path was
`.factory/work/artifacts/...`, then created useful scratch behavior tests
and documentation edits inside the candidate workspace. The merge landed
only the committed candidate and cleanup removed the transient worktree,
so those scratch edits did not land. This reinforces the redesigned
model: merge-time reviews should write only review artifacts, prompts
should use Work-native paths, and useful scratch tests or suggested edits
should become follow-up write Tasks instead of candidate mutations.
→ Resolved: fc382c1, ee9b549, ea96319, 6d4fce1, and 2715773 made
merge-time reviewers Work-native and read-only at the merge boundary.
Reviewer prompts now use Work artifact paths, absolute candidate skill
and decision paths, and read-only candidate guidance. Merge execution now
detects staged, unstaged, untracked, and ignored candidate workspace
mutations after each reviewer, records failed merge review state before
landing, and keeps reviewers writing artifacts instead of changing the
candidate.
