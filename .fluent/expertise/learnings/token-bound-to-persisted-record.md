---
name: token-bound-to-persisted-record
description: Identity tokens returned by dispatch operations must be constructed from persisted record fields, not from caller-supplied request fields
metadata:
  type: convention
---

When a dispatch or submit operation returns a token that identifies the durable
record (e.g., a `DispatchToken` containing `id`, `work_item_id`, `attempt_id`),
construct the token from the fields read back from the persisted on-disk state,
never from the caller-supplied request struct.

Even when the operation already verified that the caller's fields match the
persisted record exactly, binding the returned token to the freshly-read
persisted values ensures the token is provably tied to what is durable — not
to what the caller claimed. A caller can supply structurally matching but
independently sourced values; the token must attest to the committed state.

This pattern also makes the code self-documenting: readers see that the token
derives from `persisted.*`, not `request.*`, and can immediately infer the
authenticity guarantee without reading the verification logic above.

Related: [[registry-read-modify-write-needs-flock]]
