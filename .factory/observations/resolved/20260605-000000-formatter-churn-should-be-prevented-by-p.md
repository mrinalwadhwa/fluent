2026-06-05 — Formatter churn should be prevented by process, not cleaned
up after the fact. Factory should run the repo's formatter consistently
before merge so formatting diffs are deliberate and reviewer-visible.
→ Resolved: 42531ff (Factory supports configurable pre-land checks with
autofix commands)
