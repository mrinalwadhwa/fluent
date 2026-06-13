2026-05-11 — Three of four reviewers printed results to stdout but
didn't write the review artifact file during the latest review run.
The verdict check defaulted to pass.
→ Resolved: run_single_reviewer now cds to the project root derived
from the run dir before launching claude. Reviewers were writing
artifacts at relative paths that resolved to the original project
root instead of the worktree.
