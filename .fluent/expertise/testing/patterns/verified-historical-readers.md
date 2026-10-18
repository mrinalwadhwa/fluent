# Reuse verified historical readers

## Context

Use this pattern when compatibility tests run pinned older executables from several Fluent worktrees.

## Mechanism

Hash the pinned source archive, lockfile, toolchain, target, Cargo configuration, and build options into one identity. Build with a private target, then publish the executable and its provenance in a new version directory under a per-identity lock. Verify the selected version's identity, digest, and executable mode on every use. Atomically select a new version after repair, and retain old versions and fixture-local executable paths already returned to tests. Keep the fixture identity and source proof files that existing tests inspect. Grant the Tester sandbox access to the exact project cache; use isolated preparation where that grant is unavailable.

When Cargo selects an explicit target, read its executable from the target triple's `debug` directory, even if that triple equals the host. A Cargo configuration can also select a target; accept a nested executable only when the fresh private target directory contains exactly one matching reader.

## Example

`tests/fixtures/prepare-historical-reader.py` prepares the pinned readers used by `build-*-old-reader.sh`. `tests/behaviors/operations/test-historical-reader-cache.sh` varies identity inputs, damages cached bytes and provenance, and checks that an earlier returned path remains executable during concurrent repair.
