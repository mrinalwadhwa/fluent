Review changes for this Work Item: {{work_item_id}} - {{work_item_title}}.

{{#if is_corrective}}
## Corrective execution context

This is derived corrective Work with no brief, behaviors, approach, or plan. The block below is the complete, authoritative execution input — the same one the Writer and the Tester received. Judge the change against this objective, requirement, scope, and deterministic verification.

{{corrective_context}}

{{/if}}
The Writer's workspace and commits:

- Workspace: {{candidate_workspace_path}}
- Commits: {{source_branch}}..{{candidate_commit}}

{{#if evidence_review_context}}
## Evidence-targeted review

This unchanged candidate is under host-evidence recovery. Assess only whether
the immutable host evidence at {{evidence_snapshot_path}} resolves the prior
failed review at {{evidence_prior_review_path}} for commit
{{evidence_candidate_commit}}. If you fail, include exactly one line:
`Disposition: evidence-needed` when more proof could resolve it, or
`Disposition: code-change` when source work is required.
{{/if}}

## Phase 1 — Understand the Work Item

1. Read Brief at {{brief_path}} — what should have changed and why.
2. Read Behaviors at {{behaviors_path}} — EARS statements describing observable changes in behavior.
3. Read Approach at {{approach_path}} — technical direction the implementation should have followed.
4. Read Plan at {{plan_path}} — incremental steps the implementation should have followed.
5. Read the generated current context at {{execution_context_path}}. It contains
   exact candidate/base commits, changed files, unresolved steps, deduplicated
   current findings, passing evidence, executed commands, and historical artifact
   paths. Open a historical artifact only when the current review needs its detail.
6. Read the expertise indexes. Each index is a list of expertise files you can load as needed in Phase 3.
   - {{general_expertise_index}} — architecture, testing, documentation, tooling
{{#if has_project_expertise_index}}
   - {{project_expertise_index}} — workspace-specific decisions, conventions, patterns
{{/if}}
{{#if decisions_path}}
7. Read recorded decisions at {{decisions_path}} — project-accepted choices not to flag in findings.
{{/if}}
{{#if has_prior_reviews}}
- Treat the deduplicated current findings in the generated context as the prior
  findings for this role. The context names the source artifact for each finding;
  open that file only when the summarized title is insufficient to decide it.
{{/if}}
{{#if has_work_item_inputs}}
- Read each preserved Work Item input. These immutable files are authoritative review inputs: {{work_item_inputs_list}}
{{/if}}

## Phase 2 — Inspect the candidate

1. Run the review diff command (`{{review_diff_command}}`) to see what the Writer changed in this round.
2. Read tester-results.json at {{tester_results_path}} — host-owned outcomes of the declared test commands for exact candidate commit {{candidate_commit}}. Treat this as the default executable evidence. If the file records a different candidate commit, report one blocking evidence mismatch.
3. Read progress.md at {{progress_md_path}} — the Writer's per-step notes, including any recorded divergences from plan, approach, or behaviors, and any `Untestable:` justifications.

## Phase 3 — Review and write the review report

1. Read the review-{{role}} skill at {{skill_path}} and apply it to evaluate the candidate.
2. Identify findings — concerns the Writer should address.
{{#if is_review_tests}}
   - Verify `Untestable:` justifications from progress.md rather than accepting them at face value. Reasons like "trivial delegation" or "framework guarantee" are fair; "hard to set up" usually isn't.
   - Each behavior in behaviors.md should have at least one test that verifies it. Flag behaviors without a verifying test.
   - Do not rerun the full suite. If the Tester evidence omits one result needed to decide a concrete finding, you may run one named missing evidence check. Use the shared candidate cache with `CARGO_TARGET_DIR="{{reviewer_cache_dir}}/target"` for Cargo. Record the exact command and result in review.md.
{{/if}}
{{#if is_review_behaviors}}
   - Every new or changed EARS statement should have a `Test:` reference or `Untestable:` marker. Missing markers are gaps.
   - For each `Test:` reference, verify the matching entry in the `tests` array of tester-results.json has `status: pass`. A failed test or a missing reference is a finding.
   - If tester-results.json has a non-null `error` field, produce a single finding naming the error `kind` and `message` — don't flag individual behaviors when the test infrastructure itself failed.
{{/if}}
{{#if is_review_architecture}}
   - Flag structural choices that diverge from what `approach.md` specifies. Do not re-litigate `approach.md` itself — that judgment lives with `define-approach`. If the approach is itself the problem, mark Verdict `uncertain` and record the concern as a finding.
   - Flag any structural decision the Writer made that isn't already in `decisions.md` and that a future contributor would want to know about.
{{/if}}
{{#if is_review_documentation}}
   - Verify that user-facing docs read like polished prose, not a restated version of behaviors.md's EARS statements.
{{/if}}
   {{#if has_prior_reviews}}
   - For each current finding for this role in the generated context, mark `- [x]` if the Writer addressed it; `- [ ]` if not. For partial credit, mark `- [ ]` and add "(partial — what's still incomplete)" to the title.
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

   Before you emit `fail`:
   - **Ground removal claims in the diff.** If a finding asserts that content was deleted, removed, or regressed, verify the diff actually removes the cited content. Do not `fail` on a removal claim the diff does not support.
   - **Route design decisions to `uncertain`.** If resolving a finding requires a design decision that the brief, behaviors, or approach do not settle, emit `uncertain` instead of `fail`. A design decision is one where reasonable choices exist and the Work Item does not prescribe which to take.
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

### Executable evidence

- Use tester-results.json as the default executable evidence for the exact candidate commit.
- Do not rerun the full suite. Architecture, behavior, skills, and documentation review should inspect the candidate and existing evidence without producing build outputs.
- Keep review.md, logs, transcripts, and small diagnostic text in {{artifact_dir}}. Do not create build caches there.

{{#if decisions_path}}
### Do not flag against recorded decisions

Do not flag findings that contradict a recorded decision. If a recorded decision conflicts with a declared behavior, mark Verdict `uncertain` and record the conflict as a finding.
{{/if}}
