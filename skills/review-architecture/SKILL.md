---
name: review-architecture
description: >
  Code-aware architecture reviewer. Reads the codebase and evaluates
  structural decisions against architectural expertise. Checks at
  whatever scale is relevant — from code organization to system topology.
  Produces a verdict and findings.
---

# Review Architecture

Review code by reading it alongside the architectural expertise.
Evaluate structural decisions at whatever scale is relevant to the
changes: function organization, module boundaries, component
interactions, system topology. Produce findings the author can act on.

This reviewer is code-aware — it reads both source code and
architecture documentation.

---

## How to run this skill

### Phase 1 — Read the inputs and load expertise

Read the architectural expertise:
- `expertise/architecture.md` — core principles, viewpoints,
  anti-patterns

If the codebase uses a specific language, check for language-specific
expertise. Read `expertise/{language}.md` if it exists (e.g.,
`expertise/shell-scripts.md`).

Read the system context:
- `documentation/architecture.md` — how the system is built today

Check how the review was triggered:

**Run-scoped (default):** Use the git diff of the worktree against
the source branch to identify what changed. Read the run's brief
and approach.md to understand what the run was trying to do.

The diff identifies where to look. The review happens in full
context — read the changed code and everything it depends on.

**Full-codebase:** Read all significant code and evaluate the overall
structure.

### Phase 2 — Identify relevant viewpoints

Not every viewpoint applies to every change. Based on what the code
does, determine which viewpoints to apply:

- **Functional** — component responsibilities, collaboration. Apply
  when new components are added or responsibilities shift.
- **Development** — code organization, dependencies, module structure.
  Apply to most code changes.
- **Information** — data flow, data ownership. Apply when data models
  or data paths change.
- **Deployment** — infrastructure, runtime configuration. Apply when
  deployment or infrastructure changes.
- **Operational** — monitoring, debugging, error handling. Apply when
  error paths or observability change.

Most run-scoped reviews need the development viewpoint. Full-codebase
reviews should consider all viewpoints.

### Phase 3 — Evaluate against principles

For each relevant viewpoint, evaluate the code against the
architectural expertise:

**Simplicity:** Is the solution as simple as it can be? Is there
complexity that isn't justified by a concrete benefit?

**Separation of concerns:** Does each component focus on one thing?
Are concerns mixed?

**Modularity:** Are modules independently understandable and
testable? Can they be changed without rippling?

**Boundaries:** Are boundaries explicit? Are contracts clear between
components?

**Vocabulary:** Do code, architecture docs, behaviors, tests, and user
conversation use the same terms for the same concepts? Is a term being
introduced that conflicts with the system's domain model?

**Coupling:** Is there unnecessary coupling? Check for the
shared-utils trap, deep import paths, circular dependencies.

**Anti-patterns:** Check for god objects, circular dependencies,
leaky abstractions, premature abstractions, big ball of mud patterns.

For each finding, record:
- The location in the code
- Which principle it relates to
- Why it matters — what problem it causes or will cause
- Severity — is this blocking or advisory?

Treat vocabulary findings as architectural findings when inconsistent
terms obscure component boundaries, domain concepts, or contracts. Make
them advisory unless the mismatch would cause real ambiguity for users,
authors, or future reviewers.

### Phase 4 — Check architecture documentation

Compare what the code does with what `documentation/architecture.md`
describes. Report gaps:
- Components that exist in code but not in docs
- Structural changes that docs don't reflect
- Architectural decisions that should be recorded

### Phase 5 — Produce verdict and findings

Write the review artifact to the exact path named in the prompt. For
legacy run reviews, that path is usually
`.factory/runs/[run-id]/reviews/review-architecture.md`.

Do not create legacy run review artifacts during Work-model reviews.

Determine the verdict:
- **pass** — no findings that warrant changes
- **fail** — findings the author should address before completion
- **uncertain** — findings that could go either way, need the user's
  judgment

Format:

```markdown
# Architecture Review

Reviewer: review-architecture
Verdict: [pass | fail | uncertain]

## Findings

### [Viewpoint: Development]

1. [file:location] — [principle: coupling]
   [What the code does and why it's a concern]
   Severity: [blocking | advisory]

### [Viewpoint: Functional]

2. [file:location] — [principle: separation of concerns]
   [What the code does and why it's a concern]
   Severity: [blocking | advisory]

### Documentation gaps

3. [What exists in code but not in architecture.md]
```

Each finding should have enough context for the author to understand
the concern and decide how to address it.

---

## Rules

- **Read the expertise.** Evaluate against the principles in
  `expertise/architecture.md`, not your own assumptions.
  The expertise captures the project's architectural values.
- **Findings, not rewrites.** Report what concerns you and why.
  The author determines the fix.
- **Scale to the change.** A small function refactoring doesn't need
  a system-level review. A new service boundary does. Match the depth
  of review to the scope of the change.
- **Viewpoints are lenses, not checklists.** Apply the viewpoints
  that are relevant. Don't force every viewpoint on every review.
- **Nudge vocabulary consistency.** Check for domain terms that drift
  across code, documentation, behaviors, tests, and dashboard copy.
  Report meaningful drift, not harmless wording differences.
- **Severity matters.** A circular dependency that prevents deployment
  is blocking. A function that could be slightly simpler is advisory.
  Lead with the findings that matter most.
- **Context, not just diff.** The diff tells you where to look.
  Evaluate in the context of the full codebase and the system's
  architecture.
- **Overlapping findings are fine.** If the documentation reviewer
  also noticed a gap in architecture.md, report it anyway. Redundant
  detection is better than missed problems.
