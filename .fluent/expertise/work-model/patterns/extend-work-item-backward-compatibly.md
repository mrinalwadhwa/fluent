# Extend the WorkItem model with a backward-compatible field

## Context

Use when adding a field to `WorkItem` (or another split-stored model type in
`src/work_model.rs`). `WorkItem` is stored split: its top-level fields live in a
separate `WorkItemRecord`, and it is constructed by dozens of struct literals
across `src/` and `tests/`. A naive field addition breaks every literal and can
strand previously persisted Work by changing how old JSON deserializes.

## Mechanism

1. Add the field to both `WorkItem` and `WorkItemRecord`, and copy it in both
   directions of the `From` conversions (`From<&WorkItem> for WorkItemRecord`
   and `From<WorkItemRecord> for WorkItem`). Forgetting one direction silently
   drops the field on read or write.
2. Give the field `#[serde(default)]` (or `default, skip_serializing_if = ...`)
   so legacy JSON without it still deserializes. Choose the default so old Work
   keeps working — e.g. absent authorization means execution-ready, not
   proposed.
3. To keep legacy files byte-identical (a deterministic-JSON storage test pins
   the minimal `{id, title}` shape), `skip_serializing_if` the field's default
   value. Serialize it only when it carries non-default meaning.
4. Add `impl Default for WorkItem` and convert existing struct literals with
   functional update: append `..Default::default()` before each literal's
   closing brace. This fills only the new fields and leaves the explicitly set
   ones untouched. For ~dozens of literals, drive the insertion with a
   brace-matching script (skip strings/`//` comments so `format!("{}")` braces
   do not miscount), then `cargo fmt`.
5. Exercise a persisted old-format fixture through both the model
   (`WorkItem::from(record)`) and a `work-item show` binary test to prove legacy
   Work is neither stranded nor mutated.

## Example

```rust
// WorkItem and WorkItemRecord both gain the field:
#[serde(default, skip_serializing_if = "ExecutionAuthorization::is_unattributed_ready")]
pub authorization: ExecutionAuthorization,

impl Default for ExecutionAuthorization {
    fn default() -> Self { Self::ExecutionReady { authority: None } }
}

// Legacy JSON with no `authorization` reads as execution-ready:
let item = WorkItem::from(from_json::<WorkItemRecord>(r#"{"id":"w","title":"t"}"#)?);
assert!(item.authorization.is_execution_ready());
```
