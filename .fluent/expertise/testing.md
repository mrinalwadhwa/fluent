# Testing in the fluent

The fluent has several types of tests:

**Behavioral tests** (`tests/behaviors/`) — verify the fluent delivers its specified behaviors. Written from EARS statements without seeing code. Test the system from the outside.

**Skill tests** (`tests/test-skill`) — simulate skill conversations between two agents. Test skill structure and flow.

Each type catches a different class of problems. Behavioral tests catch user-visible regressions. Skill tests catch conversation design issues.

## Where failure logs live

Every test run writes per-case stdout and stderr to `tests/output/`:

- Rust binary tests: `tests/output/<test-name>.log`
- Shell behavior tests: `tests/output/<test-file-name>/<case>.log`

The `tests/output/` directory is gitignored — it exists only on disk. After a failure, open the log directly instead of rerunning.

Set `FLUENT_TESTS_SKIP_LOG=1` to disable per-case log writing (useful for CI workers that manage their own artifact capture).
