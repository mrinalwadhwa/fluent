---
name: config-fails-open-only-for-diagnostics
description: Layered config resolution fails closed by default; only diagnostic-only knobs whose failure cannot corrupt correctness may fail open, and that must be justified in the docstring
metadata:
  type: convention
---

Layered configuration resolution in `config.rs` (`resolve_leaf`,
`FollowUpConfigError`) is deliberately **fail-closed**: a malformed configured
value names the config path and offending key and propagates an error rather
than silently substituting a lower-precedence or default value. Callers are
expected to abort rather than run on a mis-resolved value.

The one sanctioned exception is a threshold that is **purely diagnostic** — one
whose failure cannot corrupt a correctness-critical path. The transcript-pump
thresholds are the exemplar: `install_transcript_pump_config` discards a
`resolve_transcript_pump_config` error and keeps the built-in defaults, because
canonical byte capture never depends on those knobs (they only bound console
previews and pace status flushes). A malformed threshold must not abort a coder
launch.

When you add a config path, default it to fail-closed. Fail open **only** when
the value is diagnostic-only, and say so explicitly in the installing function's
docstring so a reviewer does not read it as an accidental break of the
fail-closed convention. Reviewers treat an unexplained fail-open path as a
finding. Prefer also recording the divergence per
[[record-divergence-in-decisions-md]].
