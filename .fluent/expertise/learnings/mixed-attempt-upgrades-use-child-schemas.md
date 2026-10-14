---
name: mixed-attempt-upgrades-use-child-schemas
description: Compare each split Attempt record with the schema its own authority requires when upgrading a Work Item
metadata:
  type: gotcha
---

Fluent stores a Work Item and its Attempts in separate records. A new authority
in one Attempt can raise the Work Item's record schema while other Attempts
keep the predecessor schema. During `work-item upgrade`, derive each Attempt's
target schema from that Attempt's authority. Using the Work Item's target for
every child makes an already-current mixed Work Item appear to need another
upgrade on every call.

Check the upgrade report as well as the saved records: a mixed-Attempt fixture
should name only children that change, and a second upgrade should report
`already_current` without publishing another operation.

Related: [[backward-compatible-serde-fields]],
[[old-reader-fences-use-recognized-wire-fields]].
