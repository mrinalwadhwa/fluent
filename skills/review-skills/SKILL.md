---
name: review-skills
description: Reviews Agent Skills. Use when auditing skills in a codebase, checking a new or edited SKILL.md, or verifying that a skill follows the Agent Skills spec.
---

## Purpose

Decide whether each skill under review is fit to ship. Identify improvements that would make a skill more discoverable, more efficient, or more likely to succeed at its purpose.

## Scope

The invoking layer decides which skills to review: the changed skills for a diff-scoped review, or all skills for a full-codebase audit.

For each skill in scope, assess it as a whole: the SKILL.md, its supporting files under `references/`, `scripts/`, and `assets/`, and how the skill is invoked in its codebase. Skills can be invoked by name from a prompt template, through a hook, or by agent auto-activation.

When an agent picks among skills based on their descriptions, an overlap can cause it to pick the wrong one. Compare each in-scope skill's description against sibling skills invoked the same way, including siblings that aren't part of this review.

## Method

1. Read `references/skills.md` for skill-writing standards and `references/documentation.md` for writing quality.

2. For each skill under review:
   - Read the SKILL.md and files under `references/`, `scripts/`, and `assets/`.
   - Understand how the skill is invoked in its codebase — by name from a prompt template, through a hook, or by agent auto-activation.
   - Compare descriptions against siblings invoked the same way.
   - Identify improvements using the standards in `references/skills.md` (skill-writing) and `references/documentation.md` (writing quality).

3. For each improvement, decide if it blocks shipping. Missing or invalid frontmatter, broken references, or instructions that contradict the invoking layer's specifications (output format, error handling) typically block. Lighter versions — a vague but present description, load triggers that could be sharper, duplication that's aligned rather than conflicting — and style issues typically don't.
