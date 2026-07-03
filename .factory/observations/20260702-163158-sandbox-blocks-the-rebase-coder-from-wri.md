Sandbox blocks the rebase Coder from writing to its artifact directory.

In src/work_merge_executor.rs (build_coder_sandbox at line 928, invoked from rebase_candidate around line 731), the rebase Coder's sandbox has source_workspace and common_git_dir as writable roots. The rebase_artifact_dir (under <project_root>/.factory/work/artifacts/<work>/<attempt>/<candidate>/merge/<rebase>/) is not in the writable roots — it's a sibling of source_workspace, not a child.

The rebase prompt tells the Coder to write to {{artifact_dir}}/give-up.md, but that path is outside the sandbox's writable set. Today this only "works" because merges are often invoked with --no-sandbox (the auto-merge and factory work merge paths both allow no_sandbox=true via CLI flags).

Fix: include rebase_artifact_dir in additional_writable_roots when building the sandbox for the rebase Coder. Mirror what run_review_coder does — it uses artifact_dir as the working_dir, which is writable by construction.

Related: the fix should ensure this works whether sandbox is enabled or not, since we want give-up.md to be reliably available for downstream logic.
