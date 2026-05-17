# Decisions

Architectural and design decisions that are intentional and should not
be flagged by reviewers.

---

## capture-brief Phase 3 keeps cognitive science inline

The capture-brief skill includes cognitive science principles (anchoring
bias, framing effects, etc.) directly in the skill content rather than
referencing an external expertise file. This is intentional: agents are
more likely to read and apply material that appears inline within the
skill they are following than to follow a reference to a separate file.

---

## Skills use references/ symlinks for distribution

Review skills reference expertise via symlinks in their `references/`
directory (e.g., `references/architecture.md` → `../../expertise/architecture.md`).
This makes skills self-contained after installation via skills.sh, which
dereferences symlinks on copy. During development the symlinks resolve
locally; after install the files are inlined. Skills reference
`references/X.md` in their SKILL.md, never `expertise/X.md` directly.
