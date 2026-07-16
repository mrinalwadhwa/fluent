---
name: backward-compatible-serde-fields
description: New optional fields on persisted Work model structs must use serde(default, skip_serializing_if) for backward compatibility
metadata:
  type: convention
---

The Work model is persisted as JSON files under `.fluent/work/items/`. Existing stored files will not have new fields. When adding an optional field to a persisted struct (`Attempt`, `WorkItem`, `Task`, `MergeCandidate`, or their storage record counterparts):

1. Use `Option<T>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`
2. Add a round-trip test that writes and reads back through `WorkModelStore`
3. Add a test that verifies the field is omitted from serialized JSON when `None`

The `#[serde(default)]` ensures old JSON without the field deserializes to `None`. The `skip_serializing_if` keeps the JSON clean and avoids polluting existing records with null values.

The architecture reviewer checks this explicitly for any new field on a persisted struct.

Related: [[inject-side-effects-for-testability]]
