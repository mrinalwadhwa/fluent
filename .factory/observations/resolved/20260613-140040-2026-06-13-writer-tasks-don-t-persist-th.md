2026-06-13 — Writer Tasks don't persist their agent transcripts.
Inspecting recent merged Work Items shows
`Task.artifact_area: null` on every `write` Task and no
`attempt-N-write-N/` subdirectory under
`.factory/work/artifacts/<wi>/<attempt>/`. Only the resulting
commit SHA + workspace path survive in `Task.output`.

Consequence: no audit trail of what the writer agent actually did.
Adjacent observations that depend on writer transcripts (e.g.,
the just-paused expertise-usage tracking observation, future
reviewer-eval-framework work, learning capture from authoring
sessions) can't be addressed without first persisting these
transcripts.

Reviewer Tasks and the BehaviorTests Task already get artifact
directories and persist their transcripts. Write Tasks are the
exception. The asymmetry isn't intentional — review Tasks need
to write a `review.md` (so they need a writable artifact dir
anyway), but write Tasks write directly to the candidate workspace
(so the dir wasn't strictly needed). Adding one purely for
transcript persistence is a small change.

Concrete Work Item shape:
- Allocate `.factory/work/artifacts/<wi>/<attempt>/<task-id>/`
  for write Tasks (same convention as review/behavior-tests).
- Set `Task.artifact_area` to that path on creation.
- Direct the Coder to write transcript.jsonl into that path.
- Sandbox: writable artifact dir; candidate workspace remains
  writable as today.
- Downstream: any future "what did the writer read/do?"
  analysis can grep the transcript without needing live process
  access.

→ Resolved: Resolved by Work Item persist-writer-transcripts at 0fdee98. Write Tasks now allocate artifact directories and the Coder persists transcript.jsonl into them; reviewer sandboxes intentionally exclude these dirs to preserve independent verification.
