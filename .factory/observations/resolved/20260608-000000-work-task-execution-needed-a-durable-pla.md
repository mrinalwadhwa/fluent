2026-06-08 — Work task execution needed a durable place for the rich
brief, behavior expectations, approach, and plan that should guide coder
execution. Passing that material as extra CLI args to
`factory work attempt run` was the wrong boundary because extra args are
coder flags and Codex treats additional positional text as invalid prompt
input.
→ Resolved: 03051d8, 0790846, 79444f4 (`factory work create` accepts
inline or file-backed instructions, stores them on the Work Item,
copies them onto initial and follow-up write Tasks, includes non-empty
`Task.instructions` in write prompts, and preserves extra args as coder
options)
