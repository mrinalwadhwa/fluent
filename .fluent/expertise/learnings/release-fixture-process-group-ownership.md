---
name: release-fixture-process-group-ownership
description: Every long-lived release fixture owns, terminates, and reaps its complete process group on every exit path
metadata:
  type: testing
---

A release fixture that starts a long-lived Fluent or provider command must own
the command's process group, not only its leader PID. The owner must remain
responsible after a normal wait or output collection, and its unconditional
cleanup path must terminate, escalate when necessary, and verify reaping of the
whole group. This cleanup must also run during assertion failure, timeout, and
unwinding.

Use the shared fixture owner for Rust launches rather than raw `Command::spawn`
calls followed by test-body cleanup. In shell behavior tests, install the cleanup
trap in the same subshell that starts the background child; a parent-scope trap
cannot own a child PID stored only in the case subshell.

Test both a leader with a surviving descendant and at least one real
launch-capable fixture path. Related: [[capture-pump-terminate-descendants-before-eof]]
and [[verified-cleanup-for-bounded-host-probes]].
