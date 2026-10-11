# Verify a published release predecessor

## Context

Use this when upgrade tests must prove compatibility with the bytes shipped in
a release. Building older source does not reproduce that artifact's identity,
and an installed CLI changes when the operator upgrades it.

## Mechanism

Keep the published URL, checksum, and version in one committed identity file.
Prepare a project-local ignored cache before tests, checking the checksum before
executing the version command. Verify both explicit offline sources and cache
hits. Download into private temporary files beside the cache and rename only
after verification, so concurrent preparation cannot publish partial bytes.

Pass the prepared path explicitly to a shared test consumer. Read and hash the
bytes once, write those verified bytes into the scenario's private directory,
then check the copy's version. Preserve the predecessor digest in formal traces.
Never fall back to the installed CLI or a newly compiled predecessor.

## Example

`tests/fixtures/prepare-released-predecessor.sh` and
`tests/lib/released_predecessor.rs` share `released-predecessor.json`.
`.fluent/tester.yaml` assigns `FLUENT_RELEASED_050_BIN` from the helper, then
exports it under `set -eu`. The helper's shell test executes the actual declared
setup with local download and Cargo doubles to prove setup failure stops the
suite. It substitutes a small executable's checksum only in an isolated copy
of the identity file; the Rust release scenarios still run the published asset.

See [focused release tests](../../../../documentation/releases/v0.6.0.md#run-focused-release-tests)
for preparation and offline-source commands.
