2026-06-11 — Tests should never be run without saving their output to a
durable file. When a shell behavior test fails inside a chain of dozens
of cases, the only way to read the failing case's full stdout/stderr is
to rerun the whole test — slow and wasteful. The harness should write
each test's output to a per-test artifact (e.g.,
`tests/output/<test-name>/<case>.log`) so post-failure inspection does
not require a rerun. Apply this to the Rust binary suite as well: capture
full stdout/stderr for every `tests/binary.rs` case to disk on first run.

→ Resolved: Resolved by Work Item tests-auto-save-output. Rust binary suite (via LoggedCommand wrapping factory_cmd) and shell behavior tests (via shared run_test in tests/lib/run_test.sh) now write per-case stdout+stderr to tests/output/<file>/<case>.log on every run. Failed-case summary prints absolute log paths plus the last 20 lines inline.
