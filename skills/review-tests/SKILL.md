---
name: review-tests
description: Reviews test quality and coverage. Use when checking that a diff has adequate tests, checking a new or edited test, or auditing the test suite of a codebase.
---

## Purpose

Decide whether the code under review is adequately tested. Identify testable code that lacks tests, and improvements that would make the test suite more likely to catch bugs or the codebase safer to refactor.

## Scope

The invoking layer decides what's in scope. For a diff-scoped review, that's the code and tests changed in the diff. For a full-codebase audit, that's the entire codebase and test suite. Check the tests for quality and check the code for tests that should exist but don't.

## Method

1. Read the code under review to understand what should be tested.

2. Read `references/tests.md` for test-writing standards. Read any related guides it points to for the code under review.

3. For each in-scope test:
   - Read it alongside the code it exercises, its setup and fixtures, and any shared helpers.
   - Evaluate against the standards in `references/tests.md`.
   - Identify improvements.

4. Check the code for behavior that lacks tests. For a diff-scoped review, look for changed code paths, edge cases, and error paths that no test covers. For a full-codebase audit, look for significant components with no test coverage.

5. For each improvement, decide if it blocks shipping. Tests that pass without verifying behavior, flaky tests, and testable code shipped without tests typically block. Vague names, duplicated helpers, and style issues typically don't.

The invoking layer may add checks in addition to those above.
