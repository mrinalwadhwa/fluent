2026-06-08 — Factory should teach agents to turn internal behavior and
architecture artifacts into more human-readable, polished public
documentation. Current behavior docs and `behaviors.diff.md` files are
valuable as precise contracts, but they often read like internal test
scaffolding: dense EARS statements, implementation nouns, and long test
reference lists. That is useful for reviewers and automation, but it is
not the same as documentation that helps a human understand the product.

The skills and expertise should make this split explicit:
- `define-behaviors` should continue producing precise, testable
  behavior contracts.
- `write-documentation` should translate those contracts into concise
  user-facing prose, grouping related behaviors into readable workflows
  and explaining the user-visible meaning instead of mirroring every
  contract statement.
- `review-documentation` should check not only accuracy and coverage but
  also whether public-facing docs read like polished documentation rather
  than a restated behavior test matrix.
- Writing expertise should give concrete examples of converting EARS
  statements and architecture notes into public docs while preserving
  vocabulary consistency and test traceability.
