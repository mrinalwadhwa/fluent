You are seeding fluent's project expertise model. Your job: read the codebase in this workspace and produce two files that orient a future writer to the project.

## Workspace

The workspace root is {{workspace_path}}.

## Output files

Write exactly these two files:

1. **{{overview_path}}** — a codebase overview covering:
   - What the project does (one paragraph)
   - Entry points and where major components live
   - Key conventions (naming, error handling, code organization)
   - How to build and test
   - Important dependencies

   Keep it concise — orientation, not a full audit. A future writer should read this and know where to look and what patterns to follow.

2. **{{index_path}}** — an index in this exact table format:

   ```markdown
   # Project Expertise Index

   Load expertise files on demand based on what your task involves.

   | File | Covers | Load when |
   |------|--------|-----------|
   | overview.md | Codebase structure, entry points, conventions, build/test | Orienting to the project for the first time or checking conventions |
   ```

   If hand-written expertise files already exist under `.fluent/expertise/` (e.g. `decisions.md`, `testing.md`), add rows for them — do not overwrite or remove them.

## After writing

Commit both files with the message "Seed project expertise overview". Do not commit anything else.
