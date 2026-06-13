2026-06-09 — Work write Task prompts still carry a legacy run status-file
contract. During `work-planning-bridge-cleanup`, a Work follow-up author
was told to write `.factory/runs/[run-id]/status`, and the candidate
ended up with `.factory/runs/attempt-1-write/status = complete`.
Work write Tasks should be Work-native: task completion should mean a
clean committed workspace plus durable Task/Attempt state, not delegated
authors writing legacy run status files.
→ Resolved: `42577d2` clarified the Work write Task no-change
completion prompt, and `3b9d0aa` added `prompts/work-author.md` plus
Work task executor wiring so Work write Tasks no longer receive the
legacy run status/handoff author prompt. Focused Rust and shell behavior
tests now assert that Work write prompts mention the Factory Work model,
warn that no committed Task output fails, and exclude legacy
`.factory/runs` status and `handoff.md` instructions.
