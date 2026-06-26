# Comparing structured data

## Context

When asserting that a structured artifact (JSON, YAML, TOML, a serialized model) is unchanged or matches an expected value. These formats have structure independent of serialization details: whitespace, field order, trailing newlines, indentation.

## Mechanism

Parse both sides into their structured form and compare the parsed values. Don't compare the serialized text directly. Two semantically equal documents can differ in whitespace, field order, or trailing punctuation depending on which serializer wrote them. Comparing as strings makes the test brittle to the writer, not to the data.

For "this file should be unchanged" assertions, capture the file as text before the action, then after the action parse both and compare as parsed values.

## Example

```rust
// Good — compares parsed values, robust to serializer differences
let before_text = fs::read_to_string(&path).unwrap();
run_thing_that_should_not_modify(&path);
let parsed_before: Value = serde_json::from_str(&before_text).unwrap();
let parsed_after: Value =
    serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
assert_eq!(parsed_after, parsed_before);

// Bad — fragile to field-order or whitespace differences
assert_eq!(fs::read_to_string(&path).unwrap(), before_text);
```
