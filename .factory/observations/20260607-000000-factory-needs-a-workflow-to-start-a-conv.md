2026-06-07 — Factory needs a workflow to start a conversation-focused
Codex coordinator with the right operational permissions up front. The
coordinating instance should be able to create Factory runs, resume
them, install rebuilt binaries, and perform normal local orchestration
without repeatedly asking the human for permission after the initial
trust decision. This is distinct from loosening permissions for delegated
run agents: delegated runs should still execute inside their intended
sandbox/runtime boundaries. The missing workflow is a trusted,
conversation-facing launcher or profile for the human-agent planning
loop, so the coordinator can use Factory effectively while run execution
remains isolated.
