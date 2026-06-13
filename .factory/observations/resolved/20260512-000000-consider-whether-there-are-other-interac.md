2026-05-12 — Consider whether there are other interactive git operations
that could block headless agents beyond commit signing (merge conflict
resolution, gpg passphrase prompts, interactive rebase).
→ Resolved: git-non-interactive-defaults Work Item. All git invocations
now route through src/git.rs which sets GIT_EDITOR=true,
GIT_SEQUENCE_EDITOR=true, GIT_TERMINAL_PROMPT=0, -c commit.gpgsign=false,
and -c core.editor=true. Regression-guard test enforces zero direct
Command::new("git") call sites in src/ outside the wrapper.
