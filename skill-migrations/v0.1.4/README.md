# Public Fluent 0.1.4 migration fixture

This directory contains the exact complete skill directories written by public
Fluent release commit `f093d9f`, before managed-installation records existed.

`build.rs` hashes each skill directory independently and embeds only those
identities as migration authority. Do not edit the skill directories to update
this fixture: doing so changes which unmarked user installations Fluent can
replace. To verify or regenerate it, check out `f093d9f`, run its public
`fluent skills add` command in an empty HOME, and compare every resulting skill
directory with this fixture. Keep this provenance note outside the six skill
directories so it is not part of any migration digest.
