---
name: verified-cleanup-for-bounded-host-probes
description: A timed host-side probe may report timeout only after its owned process group has been verifiably swept
metadata:
  type: gotcha
---

Host-side helper probes are not covered by a long-lived coder supervisor, so a
deadline must own the process group it launches and use the same identity-safe
group cleanup boundary as managed coder processes. On expiry, attempt to
terminate and reap the owned group, including descendants. Report a timeout only
when that sweep is positively verified; if cleanup is unconfirmed, return a
distinct cleanup failure and retain its diagnostic rather than claiming the tree
is gone.

Do not continue a credential-dependent recovery flow after a probe timeout,
nonzero exit, or unconfirmed cleanup. Reread credentials only after a successful
zero exit, preserve the typed failure through the coder retry boundary, and let
the existing authentication recovery pause handle it without consuming another
Writer round. Tests need both a forced unconfirmed-sweep case and a real
Attempt-route timeout case. Related: [[capture-pump-terminate-descendants-before-eof]],
[[terminal-coder-errors-bypass-retry-budget]], and
[[route-tests-drive-real-launch-wiring]].
