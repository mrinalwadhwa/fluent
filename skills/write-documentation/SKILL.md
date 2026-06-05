---
name: write-documentation
description: >
  Write or update project documentation. Reads the codebase to ground
  every claim in what the code actually does, follows the writing
  standards in the documentation expertise, and self-checks for
  accuracy, substance, and AI tells before finishing.
---

# Write Documentation

Write documentation that helps someone understand and use the system.
The reader is a human or a coding agent — both need accurate, clear
descriptions of what the system does and how it works.

The writing standards live in `references/documentation.md`. Read that
file before writing. It defines what good documentation looks like and
lists the specific patterns to avoid.

---

## How to run this skill

### Phase 1 — Understand the scope

Determine what needs documenting. Check the context:

- **Run-scoped:** Read the brief, behaviors, and approach. The
  documentation covers what the run built or changed.
- **Targeted request:** The user asked for documentation on a specific
  component, workflow, or concept.
- **Gap-fill:** A reviewer flagged missing or inaccurate documentation.

Identify the audience. Most project documentation targets developers
and coding agents working in the codebase. If the audience is
different (end users, operators), adjust the level of detail and
assumed knowledge.

### Phase 2 — Read the code

Read the code that the documentation will describe. This is not
optional — every factual claim must be grounded in what the code
actually does.

Look for:
- What the component does — its inputs, outputs, and side effects
- How it fits into the larger system — what calls it, what it calls
- Configuration, defaults, and options
- Error handling and edge cases
- File paths, commands, and status values the reader will need

Do not write from memory or general knowledge. If you haven't read
the code, you don't know what it does.

### Phase 3 — Read existing documentation

Check what already exists:
- `documentation/` directory for project-level docs
- README files in relevant directories
- Inline comments and docstrings in the code
- Existing behaviors and architecture docs

Decide whether to update an existing file or create a new one. Prefer
updating — a single maintained file is better than scattered fragments.
Only create a new file when the topic genuinely doesn't fit anywhere
existing.

### Phase 4 — Write

Follow the standards in `references/documentation.md`. The key
principles:

**Lead with what the reader needs.** Put the most useful information
first — commands to run, file paths to check, the thing the reader
came here to learn. Explanation comes after.

**State facts, not claims.** Describe what the system does. Do not
argue that it is good, fast, or elegant. If a concrete number or
example exists, use it instead of an adjective.

**One idea per section.** Each section covers one thing. If a heading
needs "and" in it, split it.

**Show, don't describe.** A command, a file path, a concrete example.
These stick. Abstract descriptions of what something "enables" or
"provides" slide off.

**Use active verb forms.** "Authenticate users" not "User
authentication handling." "Validate data" not "Data validation." Use
"to + verb" over "for + gerund" when describing what a module does.

When writing:
- Use code blocks for commands, file paths, config values, and
  anything the reader might copy
- Use tables for structured comparisons or reference data
- Vary paragraph length — uniform paragraphs are an AI tell
- Keep paragraphs short — if a paragraph runs past six lines, it
  probably covers two ideas

### Phase 5 — Self-check

Before finishing, check your own work against these criteria. Read
`references/documentation.md` again if needed.

**Accuracy.** Does every factual claim match the code? Check file
paths, commands, status values, defaults, and workflows against the
actual source. A wrong file path is worse than no documentation.

**Substance.** Does each paragraph teach the reader something new?
Read each one and ask: what does a reader who knows this area learn?
If the answer is "nothing," cut the paragraph or add specifics.

**AI tells.** Scan for:
- Tier-1 AI vocabulary: delve, leverage, robust, seamless, pivotal,
  crucial, comprehensive, utilize, harness, foster, enhance
- Stock phrases: "It's important to note," "Furthermore," "In
  conclusion"
- The "not just X, it's Y" pattern
- Self-posed questions answered immediately
- Meta-commentary about the document ("In this section, we will...")
- Avoidance of plain "is" / "are"
- Random bolding, inline-header lists, formulaic subheadings

Replace each with the plain alternative. The word list in
`references/documentation.md` has the full set.

**Pitch voice.** Cut any sentence that sells rather than describes.
Stock openers ("In today's..."), amplifiers ("truly elegant"),
adjective stacks ("robust, scalable, enterprise-grade") — delete or
replace with specific facts.

**Specificity.** Does each section have at least one concrete example,
command, file path, or number? Vague claims add no information.

### Phase 6 — Integrate

Place the documentation where it belongs:
- Project-level docs go in `documentation/`
- Component docs go near the component
- Update any files that reference the thing you documented, if the
  references are now stale

If you updated an existing file, check that the surrounding content
still reads coherently with your changes.

---

## Rules

- **Read the code first.** Do not write documentation from general
  knowledge. Every claim must be verifiable in the source.
- **Prefer updating to creating.** Add to existing files before
  creating new ones.
- **Match the project's voice.** Read existing documentation to
  calibrate tone, level of detail, and conventions already in use.
- **Don't document the obvious.** Skip things every developer knows
  (what git does, how HTTP works). Focus on what's specific to this
  project.
- **Cut filler ruthlessly.** Every paragraph earns its place by
  teaching the reader something. Introductions that restate the
  heading, conclusions that summarize what was just said, transitions
  that exist only to connect sections — cut them all.
- **Use `references/documentation.md` as the standard.** The writing
  expertise defines what good documentation looks like. Follow it.
  Don't invent new style rules.
