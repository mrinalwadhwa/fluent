---
name: provider-outage-evidence-fails-closed
description: Pause an Attempt as provider-unavailable only when every preserved provider retry transcript and the terminal transcript prove no model progress
metadata:
  type: architecture
---

A provider rate-limit exit is not, by itself, enough to declare a resumable
provider outage: a coder may already have produced tokens or used tools before
the rate limit. Classify `provider-unavailable` only from the canonical captured
transcripts, and fail closed for unknown providers, malformed records, missing
retry phases, progress records, or records after the terminal rate-limit event.

The evidence must cover every immutable transcript preserved by the provider
retry helper and the final live transcript. Each phase may contain only the
provider's recognized launch prelude followed by its structured terminal
rate-limit event. This prevents a generic retry or a resume from repeating a
coder run that may have made side effects.

When the proof succeeds, keep the typed failure distinct from ordinary coder
errors: bypass the outer retry budget, pause only the affected Task as
`NeedsUser`, and let the normal resumable-pause path replan that Task while
preserving completed peers. This complements
[[terminal-coder-errors-bypass-retry-budget]] and
[[route-tests-drive-real-launch-wiring]].
