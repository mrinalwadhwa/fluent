---
name: attempt-terminal-mode-survives-config-changes
description: Resolve terminal mode at Attempt creation and preserve it across resume, keeping missing legacy fields headless regardless of project defaults
metadata:
  type: architecture
---

Fluent resolves terminal mode when creating an Attempt: an explicit
`--terminal-mode` wins over project configuration, then user configuration,
then the built-in headless default. The stored Attempt mode determines the
execution route through resume, continuation, and later Writer rounds. Changing
`terminal.mode` in project configuration affects new Attempts; it must not
retarget an existing Attempt.

Keep the project preference separate from the persisted-field default.
`Attempt` and its split storage record omit headless mode when serializing, and
missing fields deserialize as headless. A project configured for native mode
must therefore preserve both legacy records and an explicitly selected
headless Attempt. Changing the enum's default to match the project preference
would reinterpret those records.

The regression `attempt_retains_native_mode_across_resume` in
`tests/native_agent_tasks.rs` exercises saved native mode after a configuration
change, an explicit headless override under a native project default, legacy
field omission, and later Writer rounds through `WorkModelStore`.

Related: [[attempt-coder-mapping-is-runtime-authority]],
[[backward-compatible-serde-fields]].
