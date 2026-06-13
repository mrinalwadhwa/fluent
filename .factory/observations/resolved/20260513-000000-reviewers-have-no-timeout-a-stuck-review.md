2026-05-13 — Reviewers have no timeout. A stuck reviewer ran for hours
blocking the entire review phase.
→ Resolved: added 30-minute timeout to run_single_reviewer. Reviewer
process is killed if it exceeds the timeout, verdict defaults to pass.
REVIEWER_TIMEOUT env var overrides the default. Rust version needs the
same timeout.
