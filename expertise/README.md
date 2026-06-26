# Expertise

Principles, patterns, and conventions an agent draws on when making code decisions. A skill tells the agent what to do in what order; expertise tells it what to consider while doing it — architectural principles, testing rules, documentation standards, project conventions.

## Layout

Two tiers, same structure:

- `expertise/` — general, distributable across projects.
- `.factory/expertise/` — project-specific. Same shape; rooted in the project's `.factory/` directory.

Each tier follows a fractal convention:

```
expertise/
├── INDEX.md              ← lists topic files with "load when" triggers
├── tests.md              ← topic overview (≤500 lines)
└── tests/                ← topic depth, created when needed
    ├── patterns/         ← reusable patterns for this topic
    │   └── <pattern>.md
    └── <sub-topic>.md    ← can grow its own tests/<sub-topic>/ the same way
```

Patterns are narrow, situational rules with a single-line load trigger. They live in `<topic>/patterns/<pattern-name>.md` and are indexed under a `## Patterns` section in the corresponding `<topic>.md`.

## How expertise is referenced

Agents load expertise through three paths:

1. **Prompt indexes.** Writer and reviewer prompts read `expertise/INDEX.md` (and `.factory/expertise/INDEX.md` if present) early in their procedure, then pick topic files per step from those indexes.

2. **Skill `references/` symlinks.** Each `skills/<name>/references/` directory symlinks to the expertise files that skill needs. Symlinks are dereferenced into copies on distribution.

3. **Pattern indexes.** A topic file's `## Patterns` section lists pattern files with explicit load triggers; an agent reads a pattern only when its trigger applies.

## Progressive disclosure

Expertise loads on demand, not all at once. Keep each topic file under ~500 lines; when a topic grows past that threshold, split depth into the sibling `<topic>/` directory with explicit load triggers in the top file. See `skills.md` for the underlying principle.

Each topic file opens with a `## Contents` section — a nested bullet list of its sections and sub-sections. An agent reads the contents first to see the file's full shape, then jumps to the section relevant to the task at hand without scanning every paragraph.
