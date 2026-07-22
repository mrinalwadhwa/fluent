---
name: expertise-testing-patterns-are-copyable-templates
description: Files under .fluent/expertise/testing/patterns are copied verbatim by future test authors, so stale paths or removed mechanisms block review
metadata:
  type: convention
---

A file under `.fluent/expertise/testing/patterns/` documents a reusable test
technique that a future author is expected to copy verbatim. Reviewers treat an
inaccurate path, a removed mechanism, or a helper that no longer exists as a
**blocking** finding, not a cosmetic one: an agent following a stale note writes
a test that reads the wrong location and silently observes nothing, so the test
passes while verifying nothing.

When a change reworks the surface a pattern describes — a transcript layout, a
helper's read-back shape, a generated file name — update the matching pattern
file in the *same* change, and keep it consistent with the sibling prose in
`documentation/architecture.md` (they describe the same shipped system and a
reviewer will diff them against each other). A checkpoint commit that added a
pattern for a layout later superseded by an accepted rework is exactly the case
that slips through; re-check every pattern file the diff's code touches.

Related: [[keep-architecture-doc-in-sync]].
