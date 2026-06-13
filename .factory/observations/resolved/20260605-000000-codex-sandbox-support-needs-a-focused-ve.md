2026-06-05 — Codex sandbox support needs a focused verification run.
The implementation should verify Codex auth/config access, JSON
transcript output, worktree-limited writes, no sibling writes, and
credential handling under the Factory Seatbelt wrapper.
→ Resolved: 77aeddd, 11d0313, d50b2c3 (Codex runs inside the Factory
Seatbelt profile, uses a Codex-specific profile layer, disables Codex's
inner sandbox under Factory control, and receives a file-based CA bundle
when needed)
