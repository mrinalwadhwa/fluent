# Expertise

Factory-level reference material for decision-making. Applies to all
projects built by the factory. Skills reference these files when agents
need principles, patterns, or conventions to inform their choices.

Skills are procedures (what to do). Expertise is reference material
(what to consider when deciding).

## Usage

Skills reference expertise via their `references/` directory:

```markdown
Read references/architecture.md when evaluating structural decisions.
```

Each skill's `references/` directory contains symlinks to expertise
files. On distribution, symlinks are dereferenced into copies.

Agents load expertise on demand — not all at once.
