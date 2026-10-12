# Observe a live protected profile

## Context

Use this when a host check must prove that a sandboxed command cannot change
the exact temporary profile Fluent launched. The profile exists only while the
command runs, so a path captured before launch cannot identify it.

## Mechanism

Start a host-side watcher before launching the command. Scan the protected
profile directory for a regular file containing the rule under test. Save its
bytes as evidence, then publish its path to a fixture file atomically. Make the
sandboxed command wait for that path, require the profile file to still exist,
and assert that a harmless write attempt fails. Bound the wait and settle the
watcher on every exit path. A passing path-text assertion alone does not prove
that the worker could not change the live file.

## Example

`verify-tester-process-table-permissions-host.sh` starts
`watch-tester-process-table-profile.py` before its approved Tester command.
The watcher selects the profile containing the `kern.proc.all` rule. The
command reads the published path and checks that `touch` cannot update that
existing profile while its Seatbelt profile is active.
