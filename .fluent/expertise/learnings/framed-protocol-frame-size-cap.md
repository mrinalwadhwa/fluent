---
name: framed-protocol-frame-size-cap
description: Framed protocol readers must cap the maximum frame size before allocating to prevent multi-GB allocations from a u32 length field
metadata:
  type: gotcha
---

A length-prefixed framed protocol that reads a `u32` from the wire and
immediately allocates `vec![0u8; len as usize]` is unsafe: a malformed or
misrouted message with `len = u32::MAX` will attempt a ~4 GB allocation.

Even when the socket is `0o600` (owner-only), the impact is still a
self-inflicted crash and sets a bad precedent for new request types added later.
The standard defensive practice in this codebase is to cap the frame size at a
protocol-appropriate limit before allocating (e.g. 64 KB for health messages).

Apply this cap on both sides:

- **Reader (`frame_read`)**: check `len > MAX_FRAME_SIZE` after the `u32::from_le_bytes` parse but before `vec![0u8; len as usize]`. On 64-bit targets the widening cast is lossless, but the check must come first.
- **Writer (`frame_write`)**: check `payload.len() > MAX_FRAME_SIZE` before the narrowing `payload.len() as u32` cast. A payload that passes the guard will fit in a `u32`; without the guard, a large payload silently truncates the length field and corrupts the framing.
