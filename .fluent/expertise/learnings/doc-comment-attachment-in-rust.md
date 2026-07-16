---
name: doc-comment-attachment-in-rust
description: Inserting a function between a doc comment and its target silently re-attaches the comment to the wrong item
metadata:
  type: gotcha
---

Rust `///` doc comments attach to the immediately following item. When inserting a new function above an existing function, placing it between the doc comment block and the documented function silently moves the doc comment onto the new function. The documented function loses its documentation with no compiler warning.

The documentation reviewer checks doc-comment attachment and will block when a comment describes behavior that does not match the item it is attached to.

When inserting a new function near an existing documented function, place the new function above the doc comment block, not between the comment and its target.
