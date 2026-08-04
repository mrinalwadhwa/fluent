Work Item: {{work_item_id}} - {{work_item_title}}

{{#if is_corrective}}
## Corrective execution context

This is derived corrective Work with no brief, behaviors, approach, or plan. The block below is the complete, authoritative execution input — the same one the Writer and the Tester received. Judge the change against this objective, requirement, scope, and deterministic verification.

{{corrective_context}}

{{/if}}
Review the codebase at {{candidate_workspace_path}}.

## Phase 1 — Understand the Work Item

1. Read Brief at {{brief_path}} — what to review and why.
2. Read the generated current context at {{execution_context_path}}. It contains
   exact candidate/base commits, changed files, unresolved steps, deduplicated
   current findings, passing evidence, executed commands, and historical artifact
   paths. Open a historical artifact only when the current review needs its detail.
3. Read the expertise indexes. Each index is a list of expertise files you can load as needed in Phase 3.
   - {{general_expertise_index}} — architecture, testing, documentation, tooling
{{#if has_project_expertise_index}}
   - {{project_expertise_index}} — workspace-specific decisions, conventions, patterns
{{/if}}
{{#if decisions_path}}
4. Read recorded decisions at {{decisions_path}} — project-accepted choices not to flag in findings.
{{/if}}
{{#if has_work_item_inputs}}

### Authoritative Work Item inputs

Read each preserved Work Item input. These immutable files are authoritative review inputs:
{{work_item_inputs_list}}
{{/if}}

## Phase 2 — Inspect the codebase

1. Use the Brief to decide what to look at.
{{#if review_diff_command}}
2. Run the review diff command (`{{review_diff_command}}`) to see the change that triggered this review.
{{else}}
2. Read the areas the Brief names first, then follow their dependencies outward.
{{/if}}

## Phase 3 — Review the codebase and write the report

1. Read the review-{{role}} skill at {{skill_path}} and apply it to evaluate the codebase.
2. Identify findings — concerns from your {{role}} review perspective.
   - List each finding as `- [ ]`.
3. Determine the overall Verdict:
   - `pass` — no findings.
   - `fail` — at least one finding.
   - `uncertain` — you're not confident; surface for human judgment.

   Before you emit `fail`:
   - **Ground removal claims in the diff.** If a finding asserts that content was deleted, removed, or regressed, verify the diff actually removes the cited content. Do not `fail` on a removal claim the diff does not support.
   - **Route design decisions to `uncertain`.** If resolving a finding requires a design decision that the brief does not settle, emit `uncertain` instead of `fail`. A design decision is one where reasonable choices exist and the review context does not prescribe which to take.
4. Write your review report to {{review_path}}. Format:

    ```
    Verdict: <pass | fail | uncertain>

    ## Findings

    - [ ] <short title>
      - <what's wrong, where, why it matters, how it could be addressed>
    ```

If you found nothing, still write the file with `Verdict: pass` and an empty `## Findings` section.

The Task completes when the review report exists at {{review_path}}.

## Rules during review

### Read-only

Do not edit or commit in {{candidate_workspace_path}}. Multiple reviewers run against it concurrently.

### Executable evidence

- Use existing exact-commit evidence before running another command.
- Do not rerun a full suite.
{{#if is_review_tests}}
- For a concrete finding that lacks one result, you may run one named missing evidence check. Review-only work does not provision a shared build cache; keep build outputs outside {{artifact_dir}}.
{{/if}}
- Keep review.md, logs, transcripts, and small diagnostic text in {{artifact_dir}}. Do not create build caches there.

{{#if decisions_path}}
### Do not flag against recorded decisions

Do not flag findings that contradict a recorded decision.
{{/if}}
