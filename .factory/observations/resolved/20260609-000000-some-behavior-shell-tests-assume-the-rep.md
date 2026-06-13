2026-06-09 — Some behavior shell tests assume the repository's default
`target/debug/factory` build output. During merge review for
`work-planning-bridge-cleanup`, `test-work-task-instructions.sh` was
awkward to run from a read-only candidate workspace because merge
reviewers are supposed to redirect Cargo output into artifact-local
directories. Behavior scripts should consistently support an explicit
Factory binary path or artifact-local build output so reviewers can run
them without writing into candidate workspaces.
→ Resolved: `1f69ab3` added `FACTORY_BIN_OVERRIDE` plumbing to selected
Work behavior scripts and added a mock-binary operation test. `8f69a10`
extended override coverage to operation scripts more broadly and aligned
behavior docs with the suite-wide contract. `90190f6` tightened
override behavior for `test-run-curation.sh` and confirmed the override
test surface. `test-work-task-instructions.sh` and the rest of the
behavior suite now read `FACTORY_BIN_OVERRIDE` with a `target/debug/factory`
default, so reviewers can point them at the artifact-local candidate
build.
