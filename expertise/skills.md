# How to write a skill

How to write skills that agents follow well. Based on the Agent
Skills spec (agentskills.io) and patterns learned from building
factory skills.

## What a skill is

A skill is a procedure an agent follows — a reusable approach to a
class of problems. It lives in a directory with a SKILL.md file
containing YAML frontmatter and a markdown body.

**The test:** if the content describes steps to follow or an approach
to take, it's a skill. If it describes standards to check against or
principles to consider, it's expertise. Skills tell agents what to
do. Expertise informs the choices they make while doing it.

## Directory structure

```
skill-name/
  SKILL.md              # required — metadata + instructions
  scripts/              # optional — executable code
  references/           # optional — supplementary detail
  assets/               # optional — templates, resources
```

## Frontmatter

Required fields:
- `name` — lowercase with hyphens, matches directory name, max 64
  characters
- `description` — what the skill does and when to use it, max 1024
  characters. Include keywords that help agents identify relevant
  tasks.

```yaml
---
name: review-documentation
description: >
  Code-aware documentation reviewer. Reads the codebase alongside
  documentation, checks accuracy, writing quality, and completeness.
  Produces a verdict and findings artifact.
---
```

## Grounding in real expertise

A common pitfall: asking an LLM to generate a skill from general
knowledge. The result is vague ("handle errors appropriately,"
"follow best practices"). Skills should be grounded in real
experience.

Good source material:
- A real task completed in conversation with an agent — extract the
  steps that worked and the corrections you made
- Internal documentation, runbooks, style guides
- Code review comments and issue trackers (captures recurring
  concerns)
- Version control history, especially patches and fixes (reveals
  patterns through what actually changed)
- Real failure cases and their resolutions

The key is project-specific material, not generic references.

## Spending context wisely

The full SKILL.md loads into the agent's context when the skill
activates. Every token competes for attention with conversation
history, system context, and other active skills.

### Add what the agent lacks, omit what it knows

Focus on what the agent wouldn't know without the skill:
project-specific conventions, non-obvious edge cases, specific
tools or APIs. Don't explain HTTP, git, or common programming
concepts.

Ask about each piece of content: "Would the agent get this wrong
without this instruction?" If no, cut it. If unsure, test it.

### Size and progressive disclosure

Keep SKILL.md under 500 lines. Move detailed reference material
to `references/` with explicit load triggers:

```markdown
Read references/api-errors.md if the API returns a non-200 status.
```

Not a generic "see references/ for details." The agent needs to
know *when* to load each file.

### Design coherent units

A skill should encapsulate a coherent unit of work that composes
well with other skills. Too narrow: multiple skills load for one
task, risking overhead and conflicting instructions. Too broad:
hard to activate precisely.

## Calibrating control

Not every part of a skill needs the same level of prescriptiveness.

### Match specificity to fragility

**Give freedom** when multiple approaches are valid. Explaining
*why* is more effective than rigid directives — an agent that
understands the purpose makes better context-dependent decisions:

```markdown
## Code review
1. Check database queries for SQL injection
2. Verify authentication on every endpoint
3. Look for race conditions in concurrent paths
4. Confirm error messages don't leak internals
```

**Be prescriptive** when operations are fragile or a specific
sequence matters:

```markdown
## Database migration
Run exactly this sequence:
  python scripts/migrate.py --verify --backup
Do not modify the command or add flags.
```

Most skills have a mix. Calibrate each part independently.

### Procedures over declarations

A skill should teach how to approach a class of problems, not what
to produce for a specific instance. The approach should generalize
even when individual details are specific.

### Defaults over menus

When multiple tools or approaches could work, pick a default and
mention alternatives briefly. Don't present them as equal options —
the agent will struggle to choose.

## Patterns for effective instructions

Use the patterns that fit the task. Not every skill needs all of
them.

### Interactive conversation

When a skill involves a user, drive the conversation in small
pieces. Each area or decision gets its own turn. Don't produce a
document and ask for approval.

This applies through the entire conversation — a common failure
mode is starting with small pieces and dumping everything remaining
at the end. One question at a time. Let each answer land before
moving on.

This pattern is appropriate for skills like capturing briefs,
defining behaviors, designing approaches, and planning. It is not
appropriate for autonomous skills like reviewers or code processors.

### Gotchas

The highest-value content in many skills — concrete corrections to
mistakes the agent will make without being told:

```markdown
## Gotchas
- The users table uses soft deletes — queries must include
  WHERE deleted_at IS NULL
- User ID is user_id in the database, uid in auth, and
  accountId in billing. Same value, three names.
- The /health endpoint returns 200 even if the database is down.
  Use /ready for full health checks.
```

Keep gotchas in SKILL.md where the agent reads them before
encountering the situation. When an agent makes a mistake you
correct, add it to the gotchas section.

### Output templates

When the agent must produce output in a specific format, provide a
template. Agents pattern-match well against concrete structures —
this is more reliable than describing the format in prose.

Short templates can live inline. Longer templates, or templates
only needed in some cases, go in `assets/` and are referenced
from SKILL.md.

### Checklists

An explicit checklist helps the agent track progress and avoid
skipping steps, especially when steps have dependencies:

```markdown
## Processing workflow
- [ ] Step 1: Analyze input (run scripts/analyze.py)
- [ ] Step 2: Create mapping (edit mapping.json)
- [ ] Step 3: Validate (run scripts/validate.py)
- [ ] Step 4: Execute (run scripts/process.py)
- [ ] Step 5: Verify output (run scripts/verify.py)
```

### Validation loops

Instruct the agent to validate its own work before moving on:
do the work, run a validator, fix issues, repeat until it passes.

```markdown
1. Make your edits
2. Run validation: python scripts/validate.py output/
3. If validation fails, fix the issues and re-validate
4. Only proceed when validation passes
```

### Plan-validate-execute

For batch or destructive operations, have the agent create a plan,
validate it against a source of truth, then execute. The validation
step catches errors before they become expensive:

```markdown
1. Extract fields: scripts/analyze.py input.pdf → fields.json
2. Create values.json mapping each field to its value
3. Validate: scripts/validate.py fields.json values.json
4. If validation fails, revise and re-validate
5. Execute: scripts/process.py input.pdf values.json output.pdf
```

### Bundling scripts

When the agent independently reinvents the same logic across runs —
parsing a format, validating output, building charts — write a
tested script and bundle it in `scripts/`. One tested implementation
beats many improvised ones.

## Iteration

The first draft of a skill usually needs refinement.

### Refine with real execution

Run the skill against real tasks. Read agent execution traces, not
just final outputs. Common causes of poor execution:
- Instructions too vague — the agent tries several approaches
  before finding one that works
- Instructions that don't apply to the current task — the agent
  follows them anyway
- Too many options without a clear default

### Gotchas grow from experience

Every time an agent makes a mistake the skill should have prevented,
add a gotcha. Over time, the gotchas section becomes the most
valuable part of the skill.

### Test systematically

For a structured approach to iteration, write test cases with
expected outcomes and grade the results. The factory's test-skill
harness simulates skill conversations for interactive skills.
Autonomous skills can be tested by running them against known
inputs and checking outputs.
