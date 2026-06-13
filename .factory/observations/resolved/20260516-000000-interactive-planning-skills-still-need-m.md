2026-05-16 — Interactive planning skills still need more scenario
coverage. `capture-brief` has multiple scenarios, and
`define-behaviors` now has an initial run-summary scenario. The remaining
gap is focused coverage for `design-approach` and `plan-execution`, plus
deeper define-behaviors cases that verify final artifact quality instead
of only conversation structure. These skills drive the planning phase, so
scenario tests should simulate the interview flow and verify outputs.
→ Resolved: added `format-check-behaviors`, `format-check-approach`, and
`format-check-plan` scenarios, updated the behavior coverage map, and
taught `tests/test-skill` to write planning skill artifacts as
`behaviors.diff.md`, `approach.md`, and `plan.md` instead of always
using `brief.md`.
