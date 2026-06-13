2026-06-09 — New model adoption still had reviewer prompts and merge
review prompts that spoke in legacy `.factory/runs` terms even when
Factory was executing Work review Tasks and Merge Candidate reviews.
→ Resolved: `201e8a5` added Work-native `[work-system]` sections to the
bundled reviewer prompts, taught Work review Task prompts to name Work
artifact paths and artifact-local writable output locations, taught
merge-time reviewer prompts to prefer `[work-system]` with legacy
fallback, and documented the Work review prompt contract in architecture
and behavior docs.
