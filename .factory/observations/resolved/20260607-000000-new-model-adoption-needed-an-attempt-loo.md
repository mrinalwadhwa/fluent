2026-06-07 — New model adoption needed an Attempt loop after Work Item,
Attempt, write Task, and review Task primitives existed. The loop needed
to drive one Attempt through planned write/review Tasks, create
follow-up write Tasks from failed review artifacts, move uncertain or
missing verdicts to `needs-user`, and stop before Merge Candidate
creation.
→ Resolved: `2cba3a2` and `afb28cf` added
`factory work attempt run <work-item-id> <attempt-id>`, review verdict
interpretation for Attempt rounds, follow-up write Task creation with
usable input artifacts, managed review artifact path validation,
`needs-user` handoffs, documentation, and behavior/binary coverage. The
remaining new-model adoption work starts at Merge Candidate creation and
merge queue execution.
