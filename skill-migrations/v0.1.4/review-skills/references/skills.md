# How to write a skill

## Contents

- What are Agent Skills?
  - Directory structure
  - Frontmatter
- Progressive disclosure
- Read the specification and canonical guides
- Read the writing-quality guide
- Understand how the skill is invoked first
- SKILL.md vs reference material
- Bundle only what SKILL.md uses
- For interactive skills, ask questions users can answer easily
  - Avoid unlabeled options
  - Avoid compound decisions
  - Avoid cascading alternatives

## What are Agent Skills?

A skill is a reusable procedure. It lives in a directory with a `SKILL.md` file: YAML frontmatter with a name and description, plus a markdown body with the instructions. It may also bundle supporting references, scripts, and assets. The agent knows what skills are available from their names and descriptions, and activates a skill when it recognizes a matching situation.

### Directory structure

```
skill-name/
├── SKILL.md              # required — YAML frontmatter and Markdown instructions
├── scripts/              # optional — executable code
├── references/           # optional — reference material
└── assets/               # optional — templates and other static resources
```

### Frontmatter

Required fields:
- `name` — lowercase with hyphens, matches the directory name, max 64 characters.
- `description` — what the skill does and when to use it. Max 1024 characters. Include keywords that help agents recognize matching situations.

```yaml
---
name: review-<role>
description: <one-line description of what the skill reviews>. Use when <trigger 1>, <trigger 2>, or <trigger 3>.
---
```

## Progressive disclosure

The agent loads a skill in stages, pulling in more content only as a task calls for it. This keeps the agent's context window lean while giving it deep knowledge on demand.

1. **Advertise (~100 tokens per skill).** The agent knows every available skill's name and description from the start.
2. **Activate (< 5000 tokens).** When a task matches a skill's description, the agent loads the full `SKILL.md` body.
3. **Read references (as needed).** The agent reads files under `references/` and `assets/` only when the skill's instructions call for them.
4. **Run scripts (as needed).** The agent runs code from `scripts/` only when the skill's instructions call for it.

Keep `SKILL.md` under 500 lines. If it grows past that, move detailed content to `references/` and point at it from the main body with clear "load when X" instructions.

## Read the specification and canonical guides

Start with these:

- [Specification](https://agentskills.io/specification) — the complete format (directory, frontmatter, progressive disclosure).
- [Best practices for skill creators](https://agentskills.io/skill-creation/best-practices) — scoping and calibration.
- [Optimizing skill descriptions](https://agentskills.io/skill-creation/optimizing-descriptions) — reliable triggering.
- [Using scripts in skills](https://agentskills.io/skill-creation/using-scripts) — bundling executable scripts.
- [Evaluating skill output quality](https://agentskills.io/skill-creation/evaluating-skills) — eval-driven iteration.
- [Anthropic engineering blog: Agent Skills](https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills) — background on the format.
- [Anthropic's skill-creator](https://github.com/anthropics/skills/tree/main/skills/skill-creator) — meta-skill for creating skills.

If you cannot browse URLs directly, fetch them with `curl`:

```
curl -Ls https://agentskills.io/specification.md
curl -Ls https://agentskills.io/skill-creation/best-practices.md
```

## Read the writing-quality guide

`documentation.md` — writing-quality standards that apply to SKILL.md prose.

The remaining sections cover what we've learned beyond these sources.

## Understand how the skill is invoked first

Different codebases invoke skills in different ways. Some rely on the auto-activation model described in progressive disclosure — the agent matches a skill to the situation itself. Others invoke skills directly, by name from a prompt template, or through a hook.

For a skill invoked from a prompt template or a hook, read the invoking layer end-to-end before writing. Anything it already specifies — output format, verdict rules, error handling — should not appear in the skill. Copies drift apart. The skill should cover what the invoking layer doesn't.

For a skill activated by description matching, the description carries the burden. Make it specific about what the skill does and when to use it, and check that it doesn't overlap with sibling skills invoked the same way.

## SKILL.md vs reference material

The SKILL.md body loads on every activation. Content under `references/` loads only when the body points at it, via an explicit "read X when Y" trigger. Put procedures in the body — what to do, in what order. Put reference material in `references/` — standards, patterns, detail for specific situations.

## Bundle only what SKILL.md uses

When a skill bundles files under `references/`, `scripts/`, or `assets/`, verify that the SKILL.md's instructions actually tell the agent when to read each reference, run each script, or use each asset. Unused files inflate the skill without adding value.

## For interactive skills, ask questions users can answer easily

An interactive skill drives a back-and-forth conversation with the user. When it asks for input, design the question so the answer is short and about one decision.

Two good patterns:
- Labeled multi-choice with a lean: "(a) X, (b) Y, (c) Z. I'd lean (a). Which?" — the user picks with a letter.
- Yes/no with implicit default: "Ready to apply X?" — the user picks with "yes."

### Avoid unlabeled options

Forces the user to describe their choice:

> Should the authentication run before every request to guarantee freshness, or should we use a token cache with a 5-minute TTL for performance?

Reword with labels: "(a) Auth every request, (b) 5-minute token cache. Which?"

### Avoid compound decisions

Packs two questions into one turn:

> Should we use PostgreSQL, and should we set up read replicas from day one?

Break into sequential turns: ask about the database, then the replicas.

### Avoid cascading alternatives

Agent keeps adding options mid-turn:

> Should we use PostgreSQL? Or maybe MongoDB would be better? Or would SQLite work for our scale?

Restructure with labels: "(a) PostgreSQL, (b) MongoDB, (c) SQLite. I'd lean (a). Which?"
