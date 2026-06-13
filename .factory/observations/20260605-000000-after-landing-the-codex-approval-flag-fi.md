2026-06-05 — After landing the Codex approval-flag fix, installed smoke
run `20260605-codex-installed-smoke-3` verified the fixed command shape.
The installed Factory binary launched installed Codex without the
`unexpected argument '--ask-for-approval'` parser error; invoking Codex
directly with the old bad order still reproduces that parser error.
When Codex was launched from inside this tool's outer sandbox, it then
failed with `failed to initialize in-process app-server client:
Operation not permitted`, including when called directly with the
correct argument order. Treat that as an environment/sandbox interaction
separate from Factory's flag placement. Earlier failed smoke
`20260605-codex-installed-smoke-2` also exposed a status propagation
gap: the worktree run status was `failed`, while the source run
directory still showed `planned` because failed worktree artifacts were
not copied back.
