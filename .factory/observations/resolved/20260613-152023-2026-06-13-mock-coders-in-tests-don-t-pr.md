2026-06-13 — Mock coders in tests don't produce
`behavior-tests-results.json`, so 24 binary tests fail in every
Work Item review since the BehaviorTests Task contract landed.

Concrete pattern (visible in reviewer artifacts across multiple
Work Items today): tests that exercise the Attempt loop end-to-end
fail with messages like "BehaviorTests Task completed without
writing behavior-tests-results.json". The mock coders only know
how to produce write Task commits and review Task review.md
artifacts; they have no fixture for the BehaviorTests Task's
expected output.

Every reviewer has to assess these 24 failures and conclude they
are pre-existing infrastructure issues, not introduced by the
candidate. That assessment cost compounds; the failures also
mask real regressions that a clean test suite would reveal.

Fix shape: extend the mock coder framework (existing
`tests/mock-claude`, `tests/mock-aws-cli`, etc.) so that when
invoked for a BehaviorTests Task, the mock produces a valid
`behavior-tests-results.json` fixture in the artifact directory.
The fixture can be a minimal JSON with summary counts of 0 and
an empty behaviors array — just enough to satisfy the existence
check and the schema parser.

This is small, well-scoped, and unblocks a clean test signal
for every subsequent Work Item.

→ Resolved: Resolved by Work Item mock-coder-behavior-tests-fixture at 1277db8. Factory sets FACTORY_TASK_KIND=behavior-tests in extra_env when invoking the Coder for a BehaviorTests Task; write_mock_claude injects a prelude that detects the env var and writes a minimal-valid behavior-tests-results.json.
