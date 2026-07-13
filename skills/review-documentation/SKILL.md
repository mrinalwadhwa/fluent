---
name: review-documentation
description: Reviews the accuracy and writing quality of documentation. Use when reviewing doc changes in a diff, checking a new or edited page, or auditing the documentation of a codebase.
---

## Purpose

Decide whether the code under review has documentation at every level — function docstrings, file-header comments, module READMEs, codebase-level docs, and user-facing pages. Identify code that lacks documentation, and existing docs that are inaccurate, unclear, or poorly organized.

## Scope

The invoking layer decides what's in scope. For a diff-scoped review, that's the code and documentation changed in the diff. For a full-codebase audit, that's the entire codebase and its documentation. Check the documentation for accuracy, writing quality, and vocabulary consistency, and check the code for documentation that should exist but doesn't.

## Method

1. Read the code under review to understand what needs documenting.

2. Read `references/documentation.md` for writing-quality standards.

3. For each in-scope documentation file:
   - Read it alongside the code it describes.
   - Check accuracy against the code.
   - Evaluate writing quality against the standards.
   - Identify improvements.

4. Check the codebase for undocumented capabilities at every level — function docstrings, file-header comments, module READMEs, codebase-level architecture and contributing docs, and user-facing documentation. Skip when the code speaks for itself; not every function needs a docstring.

5. Check the documentation set as a whole for vocabulary drift — the same concept named differently across pages, or a doc term that conflicts with what the code, tests, or user-facing surface calls it.

6. Check the candidate's commit messages over the provided commit range against the project's stated commit-message conventions in `AGENTS.md` or `CLAUDE.md`. If the project states commit conventions, flag any commit that violates them. If the project states no commit conventions, skip this step — do not impose conventions the project did not state.

7. For each improvement, decide if it blocks shipping. Inaccurate paths, commands, or workflows that would break a reader's task typically block. Vague phrasing, minor AI tells, and small clarity nits typically don't.

The invoking layer may add checks in addition to those above.
