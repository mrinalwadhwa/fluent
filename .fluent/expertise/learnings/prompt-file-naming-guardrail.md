---
name: prompt-file-naming-guardrail
description: Adding or renaming prompt files under prompts/ requires updating the no_legacy_prompt_files_in_prompts_dir allowlist test
metadata:
  type: gotcha
---

`tests/binary.rs::no_legacy_prompt_files_in_prompts_dir` enforces the naming convention for files under `prompts/`. It asserts that every prompt file matches one of an allowlisted set of prefixes (e.g. `["write-", "review-", "rebase-", "seed-", "learner-"]`). When you add a prompt with a new prefix, or rename an existing family (e.g. `capture-*.md` → `learner-*.md`), you must update this allowlist in the same commit, or the whole `cargo nextest` suite goes red with `Unexpected prompt file: <name>`.

The rename is only complete when the guardrail tracks it: add the new prefix and drop any retired one, so the convention artifact matches the shipped state. Also refresh the assertion's failure message — it hardcodes the allowed prefixes in prose (it once still said "Only work-* and review-* prompts should exist" long after more prefixes were added).

This is the same class of defect as a stale test citation: a convention-guarding test that no longer matches the convention it guards. It is a mechanical completion of a rename, not a design choice, so reviewers treat it as blocking rather than deferring it.

Related: [[behaviors-test-citation-sync]], [[shell-tests-invisible-to-compiler]]
