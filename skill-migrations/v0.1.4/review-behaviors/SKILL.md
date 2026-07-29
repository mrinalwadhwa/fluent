---
name: review-behaviors
description: Reviews the quality and coherence of behavior statements. Use when reviewing behavior changes in a diff, checking a new or edited behavior statement, or auditing the behavior statements of a codebase.
---

## Purpose

Decide whether the behavior statements under review are fit to ship. Identify improvements that would make each statement more precise about what the system does, or the collection more coherent and complete as a whole.

## Scope

The invoking layer decides what's in scope. For a diff-scoped review, that's the behavior statements changed in the diff. For a full-codebase audit, that's the entire set of behavior statements. Check each statement for quality and check the collection for coherence and completeness.

## Method

1. Read `references/behaviors.md` for behavior-writing standards.

2. For each in-scope behavior statement:
   - Verify it uses one of the six EARS patterns from `references/behaviors.md`. Statements without a recognized trigger (WHEN, WHILE, IF, WHERE) or without "THE SYSTEM SHALL" are hard to verify.
   - Evaluate against the "Properties of a good behavior statement" and "What not to specify" sections in `references/behaviors.md`.
   - Identify improvements.

3. Check the collection for coherence — vocabulary consistency, contradictions, coverage, and redundancy — using the "Coherence across behavior statements" section of `references/behaviors.md`.

4. For each improvement, decide if it blocks shipping. Statements that leak implementation, combine multiple behaviors, or lack a way to verify typically block. Style issues and minor phrasing don't.

The invoking layer may add checks in addition to those above.
