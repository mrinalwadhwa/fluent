2026-06-07 — Fix skill review findings: `review-behaviors` should not
tell reviewers to read `plan.md` unless the allowed-read boundary
explicitly includes it, and `design-approach` should use
`references/...` for expertise files instead of direct `expertise/...`
paths.
→ Resolved: 6168a98, 2a95f3a (review-behaviors guidance now matches its
visibility boundary, design-approach uses skill-local expertise
references, the design-approach skill packages all references advertised
by its index, and focused behavior tests cover both contracts)
