Review changes for this Work Item: {{work_item_id}} - {{work_item_title}}.

The Writer's workspace and commits:

- Workspace: {{candidate_workspace_path}}
- Commits: {{source_branch}}..{{candidate_commit}}

## Phase 1 — Understand the Work Item

1. Read Brief at {{brief_path}} — what should have changed and why.
2. Read Behaviors at {{behaviors_path}} — EARS statements describing observable changes in behavior.
3. Read Approach at {{approach_path}} — technical direction the implementation should have followed.
4. Read Plan at {{plan_path}} — incremental steps the implementation should have followed.
5. Read the expertise indexes. Each index is a list of expertise files you can load as needed in Phase 3.
   - {{general_expertise_index}} — architecture, testing, documentation, tooling
{{#if has_project_expertise_index}}
   - {{project_expertise_index}} — workspace-specific decisions, conventions, patterns
{{/if}}
{{#if decisions_path}}
6. Read recorded decisions at {{decisions_path}} — project-accepted choices not to flag in findings.
{{/if}}
{{#if has_prior_reviews}}
7. Read each prior review file. The list below is the complete set of {{role}} reviews from the most recent prior round: {{prior_reviews_list}}
{{/if}}

## Phase 2 — Inspect the candidate

1. Run the review diff command (`{{review_diff_command}}`) to see what the Writer changed in this round.
2. Read tester-results.json at {{tester_results_path}} — outcomes of the declared test commands.
3. Read progress.md at {{progress_md_path}} — the Writer's per-step notes, including any recorded divergences from plan, approach, or behaviors, and any `Untestable:` justifications.

## Phase 3 — Review and write the review report

1. Read the review-{{role}} skill at {{skill_path}} and apply it to evaluate the candidate.
2. Identify findings — concerns the Writer should address.
{{#if is_review_tests}}
   - Verify `Untestable:` justifications from progress.md rather than accepting them at face value. Reasons like "trivial delegation" or "framework guarantee" are fair; "hard to set up" usually isn't.
   - Each behavior in behaviors.md should have at least one test that verifies it. Flag behaviors without a verifying test.
{{/if}}
{{#if is_review_behaviors}}
   - Every new or changed EARS statement should have a `Test:` reference or `Untestable:` marker. Missing markers are gaps.
   - For each `Test:` reference, verify the matching entry in the `tests` array of tester-results.json has `status: pass`. A failed test or a missing reference is a finding.
   - If tester-results.json has a non-null `error` field, produce a single finding naming the error `kind` and `message` — don't flag individual behaviors when the test infrastructure itself failed.
{{/if}}
   {{#if has_prior_reviews}}
   - For each finding in the prior reviews you read in Phase 1, mark `- [x]` if the Writer addressed it; `- [ ]` if not. For partial credit, mark `- [ ]` and add "(partial — what's still incomplete)" to the title.
   - Add any new finding you identified as `- [ ]`.
   {{else}}
   - List each finding as `- [ ]`.
   {{/if}}
3. Tag each `- [ ]` finding with severity in its title:
   - `(blocking)` — must be addressed before the Writer's changes can land.
   - `(minor)` — should be addressed but doesn't block landing.
4. Determine the overall Verdict:
   - `pass` — no `- [ ] (blocking)` findings.
   - `fail` — at least one `- [ ] (blocking)` finding.
   - `uncertain` — you're not confident; surface for human or other-reviewer judgment.
5. Write your review report to {{review_path}}. Format:

    ```
    Verdict: <pass | fail | uncertain>

    ## Findings

    - [ ] <short title> (blocking)
      - <what's wrong, where, why it matters, what would fix it>

    - [ ] <short title> (minor)
      - <what's wrong, where, why it matters, what would fix it>

    - [x] <short title>
      - <why you consider this addressed in this round>

    - [ ] <short title> (blocking, partial — what's still incomplete)
      - <what remains>
    ```

If you found nothing, still write the file with `Verdict: pass` and an empty `## Findings` section.

The Task completes when the review report exists at {{review_path}}.

## Rules during review

### Read-only

Do not edit or commit in {{candidate_workspace_path}}. Multiple reviewers run against it concurrently.

### Build cache and writable outputs

- You may READ the candidate workspace's existing build outputs (binaries, compiled artifacts, installed dependencies) freely. The writer produced them as part of completing the write task.
- Factory has pre-populated your reviewer artifact directory at {{artifact_dir}} with copies of the writer's build outputs for warm-start incremental builds. When you need to build new outputs the writer didn't produce, redirect them there.
- For Cargo: `CARGO_TARGET_DIR="{{artifact_dir}}/target" cargo build` (or cargo test). If the writer already built the binary you need, invoke it directly from the candidate workspace instead of recompiling.

{{#if decisions_path}}
### Do not flag against recorded decisions

Do not flag findings that contradict a recorded decision. If a recorded decision conflicts with a declared behavior, mark Verdict `uncertain` and record the conflict as a finding.
{{/if}}
