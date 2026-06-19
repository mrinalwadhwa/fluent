Rebase the candidate branch onto `{{target_branch}}`.

If you cannot resolve a conflict, write your diagnostic to:
{{artifact_dir}}/give-up.md

Then run `git rebase --abort` and exit with a non-zero exit code.
