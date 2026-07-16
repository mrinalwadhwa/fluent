---
name: backward-compatible-serde-fields
description: Persisted Work model field additions and renames must preserve backward compatibility with existing on-disk JSON
metadata:
  type: convention
---

The Work model is persisted as JSON files under `.fluent/work/items/`. Existing stored files will not have new fields and may use old field names.

**Adding a new optional field** to a persisted struct (`Attempt`, `WorkItem`, `Task`, `MergeCandidate`, or their storage record counterparts):

1. Use `Option<T>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`
2. Add a round-trip test that writes and reads back through `WorkModelStore`
3. Add a test that verifies the field is omitted from serialized JSON when `None`

The `#[serde(default)]` ensures old JSON without the field deserializes to `None`. The `skip_serializing_if` keeps the JSON clean and avoids polluting existing records with null values.

**Renaming an existing field** on a persisted struct:

1. Add `#[serde(alias = "old_name")]` to the renamed field so existing on-disk JSON with the old key still deserializes
2. Add a test that deserializes JSON containing the old key and asserts the value lands in the renamed field

The alias approach preserves read compatibility without requiring a migration. New writes use the new key. The enum variant names are typically left unchanged so serialized values are unaffected.

The architecture reviewer checks both patterns explicitly for any field change on a persisted struct.

Related: [[inject-side-effects-for-testability]], [[shell-tests-invisible-to-compiler]]
