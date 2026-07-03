Factory should set non-interactive git editor defaults in the Coder sandbox environment.

Today the rebase Coder (and any other task that shells out to git) can trigger interactive editor prompts — `git commit`, `git rebase -i` with `reword`, `git rebase --continue` on a commit with a broken message, etc. Coder CLIs run non-interactively, so any editor prompt hangs the session.

Fix: set env vars in the Coder invocation for tasks that run git — most importantly the rebase Coder. Candidates:
- GIT_EDITOR — used for commit-message edits (commit, amend, reword during rebase).
- GIT_SEQUENCE_EDITOR — used for the `git rebase -i` todo list.

The naive value `true` skips the editor but keeps the previous message / accepts the default sequence, which makes reword a silent no-op. Better: a small Factory helper that either (a) refuses editor operations and returns a clear error the Coder can act on, or (b) passes through a pre-written message from an env-var or file.

Prompt-side workaround already landed (rebase-user.md tells the Coder to avoid `reword` in `-i` mode and use `git commit --amend -m` post-rebase for top-commit message changes). But that's a Coder-observed workaround, not a robust default. The env-level fix would remove the hang risk across all Coders and all Factory tasks that touch git.

Scope: touches src/work_merge_executor.rs (rebase Coder invocation) and likely src/work_task_executor.rs (write/review Coder invocations). Consider also whether the same protection is needed for check-pre-merge and fix-pre-merge hooks.
