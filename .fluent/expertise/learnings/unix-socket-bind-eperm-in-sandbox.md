---
name: unix-socket-bind-eperm-in-sandbox
description: The execution test sandbox returns EPERM on Unix socket bind; socket-level tests must export fake types for downstream non-sandboxed consumers
metadata:
  type: testing
---

The execution sandbox returns `EPERM` on `UnixListener::bind` for every Unix
socket path. Any test that requires an actual Unix socket health exchange or
`bind`/`accept` call cannot run inside the sandbox and must be marked
`Untestable:` in `progress.md` with the justification "Unix socket `bind`
returns EPERM in the execution sandbox."

The approved workaround is to:
1. Implement the full protocol (`FakeSocketListener`, request/response framing)
   in the production source so it is code-reviewable.
2. Export the fake types (`pub struct FakeSocketListener`) so downstream tests
   running outside the sandbox can exercise the socket exchange.

This lets the reviewer confirm protocol correctness by inspection while
non-sandboxed integration or release tests can drive the real path.
