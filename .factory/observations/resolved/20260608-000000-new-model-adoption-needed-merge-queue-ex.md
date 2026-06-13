2026-06-08 — New model adoption needed merge queue execution after
passed Attempt reviews created durable Merge Candidates. Merge Candidates
should become the path to `main`: validate provenance, rebase/update the
candidate, run configured checks, run required merge-time reviewers,
fast-forward land, record merge state and artifacts, and clean managed
workspaces.
→ Resolved: `9852155` added `factory work merge <work-item-id>
<merge-candidate-id>`, durable Merge Candidate merge state, merge-time
check and review artifacts, idempotent already-landed handling, rebase
and target-move protection, failure recording, workspace cleanup, and
behavior/binary/model coverage.
