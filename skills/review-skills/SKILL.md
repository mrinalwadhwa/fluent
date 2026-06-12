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
name: review-skills
description: >
  Code-aware skill reviewer. Reads skill files and checks them against
  skill writing principles for structure, quality, and adherence to the
  Agent Skills spec. Produces a verdict and findings.
---

# Review skills

Review skill files by reading them alongside the skill writing
expertise. Check whether skills are well-structured, procedural,
appropriately scoped, and follow the Agent Skills spec. Produce
findings the author can act on.

---

## How to run this skill

### Phase 1 — Read the inputs and load expertise

Read `references/skills.md` — the guidance for writing skills.

Check how the review was triggered:

**Run-scoped (default):** Use the git diff to identify which skill
files changed. Review those skills in full.

**Full-codebase:** Review all skills in the `skills/` directory.

### Phase 2 — Check spec compliance

For each skill in scope, check:

- **Frontmatter:** Does it have `name` and `description` fields?
  Does the name match the directory name? Is the name lowercase
  with hyphens only? Is the description under 1024 characters and
  specific enough to identify relevant tasks?
- **Size:** Is SKILL.md under 500 lines? If larger, is detailed
  material split into `references/` with explicit load triggers?
- **File structure:** Does the directory follow the expected layout
  (SKILL.md, optional scripts/, references/, assets/)?

### Phase 3 — Check content quality

For each skill in scope, check against the guidance in
`references/skills.md`:

- **Procedural, not reference material.** Does the skill describe
  steps to follow or an approach to take? If it describes standards
  or principles without a procedure, it should be expertise, not a
  skill.
- **Adds what the agent lacks.** Does the skill provide information
  the agent wouldn't know on its own? Flag content that explains
  obvious concepts (what HTTP is, how git works).
- **Calibrated control.** Is the skill prescriptive where operations
  are fragile and flexible where variation is OK? Flag skills that
  are uniformly rigid or uniformly vague.
- **Defaults over menus.** When multiple approaches are presented,
  is there a clear default? Flag skills that present equal options
  without guidance.
- **Gotchas.** Does the skill include concrete corrections for
  known mistakes? If the skill has been used and mistakes were
  observed, they should be captured here.

### Phase 4 — Check interactive skills

For skills that involve user conversation (capture-brief,
define-behaviors, design-approach, plan-execution):

- **Pacing.** Does the skill maintain one-area-at-a-time throughout
  the entire conversation? Flag skills that start with small pieces
  and dump content at the end.
- **One question at a time.** Does the skill instruct the agent to
  let each answer land before asking the next?

### Phase 5 — Check writing quality

Skills contain prose that agents read and follow. Apply the writing
standards from `references/documentation.md`:

- **AI tells.** Check for tier-1 AI vocabulary, stock phrases, and
  sentence patterns. Skills should read like a person wrote them.
- **Substance.** Does each section teach the agent something it
  needs? Flag filler paragraphs that describe importance without
  adding information.
- **Plain language.** Flag formal or inflated language where a
  simpler word would do. Skills should be direct.

Don't over-report style in skills — a few rough sentences in an
otherwise useful skill are less important than structural issues.
Focus on patterns, not individual word choices.

### Phase 6 — Check references

For each skill in scope:

- **Expertise references.** If the skill references expertise files
  (e.g., `expertise/architecture.md`), verify the referenced file
  exists and resolves correctly.
- **Internal references.** If the skill references its own
  `references/`, `scripts/`, or `assets/` files, verify they exist.
- **Load triggers.** If the skill references files in `references/`,
  does it tell the agent *when* to load them? Flag generic "see
  references/" without a trigger condition.

### Phase 7 — Produce verdict and findings

Write the review artifact to the exact path named in the prompt. For
legacy run reviews, that path is usually
`.factory/runs/[run-id]/reviews/review-skills.md`.

Do not create legacy run review artifacts during Work-model reviews.

Determine the verdict:
- **pass** — no findings that warrant changes
- **fail** — findings the author should address before completion
- **uncertain** — findings that need the user's judgment

Format:

```markdown
# Skill review

Reviewer: review-skills
Verdict: [pass | fail | uncertain]
Progress: [yes | no | partial | first-pass]

## Findings

### Spec compliance

1. [skill-name] — [what's wrong with frontmatter, size, or structure]

### Content quality

2. [skill-name] — [which guideline is violated and why it matters]

### Pacing (interactive skills)

3. [skill-name] — [where pacing breaks down]

### Writing quality

4. [skill-name] — [AI tell, filler, or inflated language]

### References

5. [skill-name] — [broken reference: path doesn't exist]
```

---

## Rules

- **Read the expertise.** Check against `references/skills.md`, not
  your own assumptions about what makes a good skill.
- **Findings, not rewrites.** Report what's wrong and where. The
  author determines the fix.
- **Severity matters.** A skill that's actually expertise (wrong
  format entirely) is more important than a missing gotcha. Lead
  with structural issues.
- **Broken references are blocking.** A skill that points to a
  file that doesn't exist will confuse the agent. Always report
  these.
- **Don't over-report.** A skill that's 510 lines doesn't need a
  finding about size. A skill that's 900 lines does.
