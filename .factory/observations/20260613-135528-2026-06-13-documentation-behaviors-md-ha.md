2026-06-13 — documentation/behaviors.md has accumulated EARS
statements that describe implementation details rather than
externally-observable behavior. The original contract was that
behaviors are user-observable; many recent additions describe
internal data structures, sandbox configurations, module
boundaries, and other things that would be better captured in
code (struct definitions, comments, tests) than in the behavior
contract.

The file's total length is becoming unwieldy and the
behavior-tests Task's reviewer has to evaluate a lot of
implementation-detail behaviors against tests that essentially
re-state the implementation.

Two needs:
1. A thorough audit of the current behavior set. For each EARS
   statement, decide:
   - Is this externally observable (CLI exit codes, file paths
     users see, output formats users read, etc.)? → Keep.
   - Is this an internal implementation detail (struct field,
     module boundary, sandbox configuration, code path
     selection)? → Move to code (as struct doc comments,
     module-level docs, or just delete because the test itself
     captures the intent).
2. Possible distinction between "external behaviors" (the
   user-facing contract) and "internal invariants" (developer-
   facing). If we keep both, they live in different files /
   sections with different review criteria. The current "every
   EARS needs a Test:" rule applies cleanly to external; for
   internal, code-level assertions / type signatures / unit
   tests may be enough.

The just-landed `behavior-tests-task` and
`easy-to-answer-skill-rule` Work Items both added EARS
statements that arguably skew toward implementation detail —
they may be useful candidates to inspect as part of the audit.

This Work Item should not be drafted until the
`delete-legacy-run-model` lands (which will significantly
reduce behaviors.md by removing legacy EARS) and we can see
the post-deletion state.
