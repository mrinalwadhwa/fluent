# Reuse verified historical readers

## Context

Use this pattern when compatibility tests run pinned older executables from several Fluent worktrees.

## Mechanism

Hash the pinned source archive, lockfile, toolchain, target, Cargo configuration, and build options into one identity. Build with a private target, then publish only the executable and its provenance under a per-identity lock. Check the identity, digest, and executable mode on every use. Keep fixture-local identity files, binary paths, and source proof files that existing tests inspect. Grant the Tester sandbox access to the exact project cache; use isolated preparation where that grant is unavailable.

## Example

`tests/fixtures/prepare-historical-reader.py` prepares the pinned readers used by `build-*-old-reader.sh`. `tests/behaviors/operations/test-historical-reader-cache.sh` varies identity inputs, damages cached bytes and provenance, and checks concurrent publication.
