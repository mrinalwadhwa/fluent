---

## Prior reviews of this candidate

When the inputs to your review Task include a previous review of this
candidate by your role, treat it as another reviewer's findings, not
as your past self. Read it first.

For each finding in that previous review:
- Verify against the current candidate state whether the writer
  addressed the concern.
- If addressed, note it in your "Prior concerns addressed" section.
- If not addressed, carry it forward into your current findings.

Then evaluate the current candidate independently and add any new
findings. The writer may have addressed prior concerns while
introducing new ones — both pieces of information matter.

Use the `Progress:` field to summarize whether you observed any
movement on prior concerns: `yes`, `no`, `partial`, or `first-pass`
(when no prior review exists). This is independent of `Verdict:` — a
failing `Verdict:` can co-occur with `Progress: yes` when prior
concerns were addressed but new ones emerged.

---
name: review-documentation
description: >
  Code-aware documentation reviewer. Reads the codebase alongside
  documentation, checks accuracy, writing quality, and completeness.
  Produces a verdict and findings artifact.
---

# Review Documentation

Review documentation by reading it alongside the source code. Check
three things: does the documentation match the code (accuracy), does
it read like a person wrote it (writing quality), and does it cover
what it should (completeness). Produce a verdict and findings.

This reviewer is code-aware — it reads both documentation and source
code. It evaluates docs from the perspective of someone who knows how
the system works and checks whether the docs help readers understand
and use it.

---

## Build outputs and warm cache

Factory pre-populates your artifact directory with copies of the writer's
build outputs for warm-start incremental builds. Point your toolchain
at this directory for incremental builds; reading binaries the writer
built directly from the candidate workspace is also fine.

---

## How to run this skill

### Phase 1 — Determine scope

Check how the review was triggered:

**Run-scoped (default):** The review follows a run. Use the git diff
of the worktree against the source branch to identify what changed.
Read the run's brief, behaviors.diff.md, and approach.md to understand
what the run was supposed to do.

The diff identifies *where* to look. The review happens in full
context — read the changed files and everything they depend on: the
code they describe, the behaviors they reference, the architecture
they fit into.

**Full-codebase:** The review covers all documentation. Read every
documentation file and check it against the full codebase.

### Phase 2 — Check accuracy

Read each documentation file in scope. For each factual claim, check
it against the source code:

- File paths, commands, and status values referenced in docs — do they
  exist in the code?
- Workflows and procedures described in docs — do they match what the
  code actually does?
- Architecture descriptions — do they match the actual structure?
- Behavioral statements — do they match what the code implements?
- Configuration values, defaults, and options — are they current?

For each mismatch, record a finding with:
- The file and location of the inaccurate claim
- What the documentation says
- What the code actually does

### Phase 3 — Check writing quality

Read each documentation file in scope. Check against the guidance in
the writing expertise at `references/documentation.md`:

**Substance:**
- Does each paragraph teach the reader something? Flag empty paragraphs
  that describe importance without adding information.
- Is the writing stating facts or selling? Flag pitch voice: stock
  openers, amplifiers, adjective stacks, meta-commentary about the
  document's own structure.
- Are claims specific or vague? Flag vague claims that could be
  replaced with concrete examples, commands, numbers, or file paths.

**AI tells:**
- Scan for tier-1 AI vocabulary in figurative senses (delve, landscape,
  leverage, robust, seamless, pivotal, crucial, etc.)
- Scan for stock phrases (It's important to note, Furthermore, In
  conclusion, etc.)
- Check for the "not just X, it's Y" sentence pattern
- Check for self-posed questions answered immediately
- Check for meta-commentary ("In this section, we will explore...")
- Check for avoidance of plain "is" / "are"
- Check formatting: random bolding, inline-header lists, formulaic
  subheadings, uniform paragraph length

For each finding, record:
- The file, location, and the specific text
- Which quality issue it is (pitch voice, AI tell, vague claim, etc.)

### Phase 4 — Check vocabulary consistency

Compare terms used in documentation with terms used by the code,
behaviors, architecture docs, commands, tests, and dashboard copy.
Report meaningful drift:
- The same concept has multiple names that could confuse a reader
- A user-facing term conflicts with a code or behavior term
- A new term appears without a clear reason or definition
- A renamed concept leaves older terminology behind

Make vocabulary findings advisory unless the inconsistency would cause
users, authors, or reviewers to misunderstand behavior.

### Phase 5 — Check completeness

**Run-scoped:** Check whether code changes in the run are reflected in
the documentation. Look for:
- New components, commands, or behaviors added by the run that have no
  documentation
- Changed interfaces or workflows that the docs still describe the
  old way
- New status values, file formats, or conventions that aren't documented

**Full-codebase:** Check whether all significant components have
corresponding documentation. A component is significant if a user or
developer would need to understand it.

For each gap, record:
- What exists in the code but not in the docs
- Where in the docs it should be covered

### Phase 6 — Produce verdict and findings

Write the review artifact to the exact path named in the prompt.

Determine the verdict:
- **pass** — no findings that warrant changes
- **fail** — findings the author should address before completion
- **uncertain** — findings that could go either way, or conflicts
  with other reviewer findings that need the user's judgment

Format:

```markdown
# Documentation Review

Reviewer: review-documentation
Verdict: [pass | fail | uncertain]
Progress: [yes | no | partial | first-pass]

## Findings

### Accuracy

1. [file:location] — [what the doc says] vs [what the code does]

### Writing quality

2. [file:location] — [the text] — [which issue: pitch voice, AI tell,
   vague claim, etc.]

### Vocabulary consistency

3. [file:location] — [term drift and why it could confuse readers]

### Completeness

4. [what exists in code but not in docs] — [where it should be covered]
```

Each finding should have enough context for the author to act on it
without re-reading the entire review.

---

## Rules

- **Read the code.** Every accuracy finding must be grounded in what
  the code actually does, not what you think it should do. If you're
  not sure, don't report it as a finding.
- **Findings, not rewrites.** Report what's wrong and where. The author
  determines the fix.
- **Use the writing expertise as the reference.** The expertise at
  `references/documentation.md` defines what good documentation
  looks like. Check against it. Don't invent new rules.
- **Severity matters.** An inaccurate file path that would break
  someone's workflow is more important than a slightly AI-sounding
  sentence. Lead with the findings that matter most.
- **Context, not just diff.** The diff tells you where to look. The
  review happens in the context of the full codebase and the run's
  intent.
- **Don't over-report style.** A document with one "furthermore" is
  fine. A document with ten AI tells is a pattern worth reporting.
  Report patterns, not individual word choices unless they're egregious.
- **Nudge vocabulary consistency.** Report term drift when it affects
  reader understanding or conflicts with established project vocabulary.
  Do not turn harmless synonyms into churn.
