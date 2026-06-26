# Asserting on generated output

## Context

When the test subject renders a template, formats a message, or otherwise produces text by combining data with a fixed template — prompts, codegen output, error messages, log lines, reports. The output has two parts: the static prose from the template, and the values the renderer substituted in.

## Mechanism

Assert on what the renderer produced from the data, not on the static prose:

- The substituted values appear in the output (data flow worked).
- No unrendered placeholder syntax remains (`{{`, `${`, `<%`, etc.).

Asserting on the static prose tests that the prose hasn't changed — a property that has no value once you're iterating on the prose. Every wording tweak breaks the test without catching a real bug.

For "did it render at all" smoke coverage, one renderer-level unit test that feeds known inputs and checks both conditions above is enough. Downstream tests should verify the data flow that populated those substitutions, not the wording around them.

## Example

```rust
// Good — verifies the renderer wired data into the output
assert!(prompt.contains(&user_id));
assert!(!prompt.contains("{{"));

// Bad — verifies the prose hasn't been edited
assert!(prompt.contains("You are an analyst"));
```
