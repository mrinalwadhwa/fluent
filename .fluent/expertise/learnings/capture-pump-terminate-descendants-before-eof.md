---
name: capture-pump-terminate-descendants-before-eof
description: When a thread drains a child's piped stdout to EOF, terminate the whole process group before waiting for EOF — a backgrounded descendant can inherit the write end and hold the pipe open forever
metadata:
  type: gotcha
---

A stdout-draining pump (`transcript_pump`) only sees EOF when *every* writer of
the pipe's write end has closed it. The coder leader is not the only writer: a
backgrounded descendant it spawned inherits the same stdout descriptor, so
waiting for the pump to reach EOF right after the leader exits can deadlock — the
descendant keeps the write end open forever. The regression that pins this is
`transcript_capture_returns_when_descendant_holds_stdout_open`.

The enforced ordering in `CoderSupervisor` (`coder.rs`): when the leader exits,
`terminate_process_group` the whole group *first*, then `wait_terminal` on the
pump. Killing the descendants closes the last write end; the leader's already
buffered bytes are still in the pipe, so the pump drains them to EOF and
finishes. Never wait for pump EOF before terminating descendants.

The complementary discipline is that the child and pump are owned by one guard
(`CoderSupervisor`) whose `Drop` is the single structured-cleanup point: it
terminates and reaps the whole process group and settles the pump on every exit,
including a `?` early return (e.g. a pump failure while the coder is still alive
returns at once and lets Drop reap the live coder). This makes cleanup run on
every path without scattering it across each fallible step. When supervising a
child plus a stream-draining helper thread, own both in one Drop guard and always
kill descendants before waiting on the drain. Related:
[[atomic-task-start-reservation]], [[terminal-coder-errors-bypass-retry-budget]].
