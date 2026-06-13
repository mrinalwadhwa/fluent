2026-06-11 — Attempt IDs, Merge Candidate IDs, and write/review Task
IDs being external input may be more friction than benefit. Today
every CLI surface that creates or operates on these entities takes
the ID as an argument: `factory work attempt <work-id> <attempt-id>`,
`factory work merge <work-id> <merge-candidate-id>`, etc. Task IDs
within an Attempt are already auto-generated (`attempt-N-write-K`,
`attempt-N-review-K-role`) and the user never types them — so the
right pattern is already in the codebase, just not extended one level
up to Attempts and Merge Candidates. Most users type sequential
defaults (`attempt-1`, `attempt-2`) which is exactly what auto-gen
would produce. Sketch of the change: make Attempt/Candidate IDs
optional, default to next-free-integer suffix when omitted, keep them
positional when explicitly supplied (scriptability + recovery still
work). Work Item IDs should stay user-provided because they often
mirror external ticket/doc IDs and lose traceability under auto-gen.
Small, self-contained change — ~20 LOC of CLI default-handling and
~10 test updates per surface.
