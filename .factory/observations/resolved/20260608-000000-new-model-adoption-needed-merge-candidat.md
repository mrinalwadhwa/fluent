2026-06-08 — New model adoption needed Merge Candidate creation after
Attempt reviews passed. A passed Attempt should create or return one
durable candidate result, record the reviewed source workspace, target
workspace, branch provenance, and candidate commit, expose candidate
inspection through the Work CLI, and still stop before merge queue
execution.
→ Resolved: `fc5b54a`, `208dde2`, and `4862b23` added durable
`MergeCandidate` storage on Work Items, `factory work merge-candidate`
inspection, Attempt-loop candidate creation after passed reviews,
idempotent reruns, one-candidate-per-Attempt validation, documentation,
and behavior/binary/model coverage. The remaining new-model adoption work
starts at merge queue execution.
