2026-06-05 — The author-reviewer loop can be faster without
skipping reviewers. All reviewers still run every round, but
with scoped prompts: reviewers that passed last round get "your
previous verdict was pass, these files changed, re-evaluate only
if relevant to your domain." Reviewers that failed get "here are
your findings, here's what the author changed, re-evaluate."
The factory can derive this from the diff and previous verdicts
without author input. The author's handoff explains what changed
and why, which naturally scopes the review.

→ Resolved: Design tradeoff captured. Current Factory chose narrowing (only previously-failing reviewers re-run, with the failed review.md as input_artifacts) over scope-and-rerun-all. The 'passed reviewers re-evaluate with scoped prompts' piece was the explicit tradeoff that speed won; post-merge review is the safety net. Same philosophy as the just-resolved quality-over-speed observation.
