2026-06-05 — Local run filesystem sandboxing should allow exactly the
run worktree plus the source repository's common git directory, not the
entire workspace parent. The sandbox should let agents commit from linked
worktrees without exposing unrelated sibling worktrees.
→ Resolved: bf2f323, 77aeddd, 11d0313 (local sandbox roots were narrowed
and Codex/Claude sandbox profiles render coder-specific writable roots)
