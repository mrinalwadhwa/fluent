2026-06-13 — Conversation agents should frame questions so the
answer doesn't have to re-state the option. Labeled multi-option
questions ("(a) X, (b) Y, (c) Z — which?") are fine because the
user types a single letter. The bad pattern is "Do you want X or
do you want Y?" where the answer has to be a description of X or Y
— the user has to re-type or re-state the option's content rather
than picking a label.

Heuristic for the agent: either
- Give the option a label ((a), (b), 1., name, etc.) and ask the
  user to pick the label, OR
- Ask a yes/no question when one option is the obvious default and
  the alternative is "no, pick something else"

Avoid: "Do you want to <describe option A> or <describe option B>?"
The answer requires the user to type one of those descriptions or
explicitly negate them, which is much higher friction than a
single-letter pick.

Related to the existing
20260607-000000-factory-discussion-agents-should-frame-c
observation; that's the broader principle, this is one specific
failure mode and its fix.

→ Resolved: Resolved. The skill-side rule landed today in Work Item easy-to-answer-skill-rule at 2d68954 (inline rule in capture-brief, define-behaviors, design-approach, plan-execution skills). The conversation-agent side is codified in MEMORY.md feedback_easy_to_answer_questions.md.
