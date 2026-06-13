2026-05-12 — The system prompts (FACTORY_SYSTEM_PROMPT, reviewer prompts)
are embedded in the factory shell script.
→ Resolved: extracted to prompts/ directory. Author prompt in
prompts/author.md. Reviewer prompts in prompts/review-{name}.md with
[system], [full-codebase], [run-scoped] sections. Reviewer loop in
run_reviews collapsed from 5 blocks to a single loop. PROMPTS_DIR
overridable for FACTORY_LIB sourcing.
