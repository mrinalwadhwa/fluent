# Test native display pressure with independent input

## Context

Use this pattern when changing a native protocol relay that shares a WebSocket
between input reads and display writes. Pausing a reader briefly and then
draining everything does not test input starvation during a partial write.

## Mechanism

Keep the actual production WebSocket relay and replace only its peers with
socket producers and consumers. Constrain the socket send buffer and cap the
consumer's reads. Space reads farther apart than the per-write timeout but
closer than the idle limit, and retain enough bytes to exceed the former total
deadline. Signal observed byte consumption before sending concurrent input.
Assert that the provider receives that input before the first display message
finishes, then compare complete messages in FIFO order and canonical evidence
with their assigned generation, conversation, turn, and input sequence.

For teardown tests, wait for the payload-free `send-pending` trace before
closing a peer or the owned relay socket. Check canonical completion separately
from display delivery and use a bounded result channel to detect a stuck join.
Keep no-progress and permanent-close cases separate from a progressing reader.

## Example

`assert_codex_display_progress` and `assert_codex_display_settlement` in
`tests/native_agent_tasks.rs` drive these boundaries. These hermetic peers do
not replace the installed-provider host check or the canonical SSH journey.
