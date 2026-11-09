# Observe descendant cleanup through socket EOF

## Context

Use this when a timeout test must prove that cleanup stops a descendant holding
an inherited descriptor. A delayed marker can fire before a busy host reaches
cleanup, and an empty read buffer returns zero without proving EOF.

## Mechanism

Pass one end of a Unix socket pair to a real descendant. Have that descendant
send a readiness byte, then block reading the socket. Close the leader's copies
and let the probe command release its copy before observing cleanup. Readiness
must come from the descendant, rather than from its parent before spawning it.

Set a bounded read timeout before launching the probe and retain it through the
EOF observation. On macOS, changing socket options after cleanup closes the peer
can fail with `EINVAL`; that failure prevents the test from observing EOF.

After production cleanup returns, read into an explicit one-byte buffer and
require EOF. Capture that result before fixture teardown can release or kill a
survivor. A Drop guard shuts down the socket on assertion failures. A negative
control kills only the leader and requires the same read to time out; retain
that leader unreaped so fallback cleanup can safely sweep its process group.

## Example

`src/credential.rs::tests::observe_refresh_probe_tree_cleanup` supports the real
cleanup test, a delayed timeout poll, and a leader-only negative control. The
control fails if the observer uses an empty buffer; the real cleanup assertion
fails if only the leader is killed. Both failures occur before fallback cleanup
can hide a surviving descendant.
