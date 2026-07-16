---
name: shell-tests-invisible-to-compiler
description: Shell behavior tests query JSON via jq and are not caught by the Rust compiler when serialized field names change
metadata:
  type: gotcha
---

The `tests/behaviors/` directory contains shell scripts that exercise the CLI binary and inspect its JSON output via jq. When a serialized field name changes (e.g., renaming `review_state` to `merge_review_state` on `MergeCandidate`), `cargo build` and Rust integration tests catch all Rust-side usages, but shell scripts using `.merge_candidates[0].review_state` in jq queries break silently — they return `null` instead of failing loudly.

After any rename of a serde-serialized field, grep the shell test directories for the old field name:

```bash
grep -r 'old_field_name' tests/behaviors/
```

The test reviewer treats broken shell tests as a blocking finding equivalent to broken Rust tests.

Related: [[backward-compatible-serde-fields]], [[behaviors-test-citation-sync]]
