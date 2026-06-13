2026-06-08 — New model adoption needed operator visibility for Work
Items, Attempts, Tasks, Merge Candidates, merge state, read errors, and
needs-user/actionable state. `factory status` and `factory dashboard`
still centered legacy Runs, so the Work model required manual JSON
inspection.
→ Resolved: `1630e30`, `11fa927`, `25cb457`, `a80d021`, and `605475d`
added `work_status.rs`, Work Item output in `factory status`, a dashboard
Work Items view, polling refresh, actionable/error counts, invalid Work
Item read-error reporting, needs-user visibility, architecture and
behavior docs, and behavior/binary/unit coverage.
