---
name: review-architecture
description: Reviews the structural quality of code. Use when reviewing structural changes in a diff, checking a new or edited module boundary, or auditing the architecture of a codebase.
---

## Purpose

Decide whether the code's architecture is fit to ship. Identify improvements that would make the codebase simpler, more coherent, and easier to change.

## Scope

The invoking layer decides what's in scope. For a diff-scoped review, that's the structural choices in the diff. For a full-codebase audit, that's the codebase's overall structure. Even in a diff-scoped review, evaluate the changes in the context of the entire codebase — a boundary that looks fine in isolation can be wrong in context. Check each choice for structural quality and check the codebase as a whole for coherence.

## Method

1. Read the code under review to understand its structure — module boundaries, key abstractions, dependencies.

2. Read `references/architecture.md` for architectural principles. Read any related guides it points to for the code under review.

3. For each in-scope structural choice:
   - Evaluate against the principles in `references/architecture.md`.
   - Identify improvements.

4. Check for anti-patterns from `references/architecture.md`.

5. For each improvement, decide if it blocks shipping. Circular dependencies, god objects, and coupling that will block near-term work typically block. Small simplifications, naming preferences, and style choices typically don't.

The invoking layer may add checks in addition to those above.
