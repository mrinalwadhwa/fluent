2026-06-09 — Planning skills still treated legacy run files as the
normal handoff in places, even after Work Items gained durable planning
context. Capture, behavior definition, approach design, and execution
planning should distinguish active pre-Work-Item planning conversation
artifacts from durable Work Item planning context and use legacy
`.factory/runs` planning files only for fallback or recovery.
→ Resolved: c10bd34. Planning skills now describe approved planning
drafts as the pre-Work-Item handoff, `factory work create` stores those
drafts as durable Work Item planning context, and legacy run planning
files are documented as fallback or recovery state. Architecture and
behavior docs plus `test-planning-skills-work-context.sh` cover the
boundary.
