---
name: terminal-coder-errors-bypass-retry-budget
description: The coder retry loop gates on a single should_retry_coder_error predicate; typed errors that may have left invisible side effects must be classified terminal there, never retried
metadata:
  type: convention
---

The write and review coder loops in `work_task_executor.rs` do not retry every
failure. They gate the retry `while` on a single predicate,
`should_retry_coder_error`, which excludes classes of typed error that are
**terminal for the Task**. Two such classes exist today: auth failures
(`is_auth_error`, `claude_auth::AuthError`) and transcript-pump infrastructure
failures (`is_transcript_pump_error`, `transcript_pump::TranscriptPumpError`).

The principle: a failure must be terminal (not retried) when re-running the
coder could repeat side effects the first run already produced. A capture
failure can surface only after the coder has already mutated the workspace
invisibly; spinning it again through the generic retry budget would redo that
work. Auth failures are terminal because retrying cannot succeed.

When you introduce a new typed failure from inside a coder run, decide whether
re-running is safe. If it is not, add a classifier and extend
`should_retry_coder_error` rather than letting the failure fall through to the
generic `is_err()` retry path. Surface the failure as a distinct typed error
(implement `std::error::Error`) so the classifier can `downcast_ref` it — a
stringly-typed error cannot be distinguished from a retryable coder exit.
