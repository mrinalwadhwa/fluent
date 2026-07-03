Rebase prompt now depends on the B1 sandbox fix (writable artifact_dir).

After the R2 clarity fix (prompts/rebase-user.md), the Coder is instructed to run `git diff {{target_branch}}..HEAD > {{artifact_dir}}/pre-rebase.diff` as Phase 1 step 1 on every rebase — not just on give-up.

Today this fails under sandbox because artifact_dir isn't in the writable roots (see B1 observation 20260702-163158-sandbox-blocks-the-rebase-coder-from-wri). Merges invoked with --no-sandbox work; sandboxed merges will now fail at Phase 1 step 1.

Priority: elevate B1 to a real blocker rather than a latent one. Fixing B1 is now a prerequisite for the current rebase prompt to work under sandbox.
