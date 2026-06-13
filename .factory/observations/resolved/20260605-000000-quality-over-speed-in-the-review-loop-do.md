2026-06-05 — Quality over speed in the review loop. Don't optimize
review time at the expense of thoroughness. Scoped review prompts
should provide context (previous verdict, what changed) to help
reviewers focus, not to reduce their coverage. A reviewer that
passed last round should still re-evaluate fully if the changes
could affect its domain. The goal is better-informed reviewers,
not faster ones. Reviewers should always view what the author
says with skepticism — the author's explanation of what changed
is context, not evidence. The reviewer verifies independently.

→ Resolved: Design-philosophy note, not a Work Item candidate. The current narrowing approach (full reviewer set in round 1, round 2+ narrows to previously-failing reviewers, post-merge review as safety net) is the explicit tradeoff. If the tradeoff ever needs revisiting it can be discussed then.
