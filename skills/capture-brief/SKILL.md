---
name: capture-brief
description: >
  Capture the user's intent for a new piece of work through a structured
  interview. Adapt depth to the clarity of the idea — sharpen vague ideas,
  ground clear ones in the codebase. Produce a brief.md that starts a
  Factory Work Item or legacy run.
---

# Capture Brief

Interview the user to capture their intent. The brief is short,
conversational, and in the user's words. It captures intent before any
elaboration — behavioral definitions, solution design, and implementation
details come in later stages.

Adapt your approach to the idea's clarity. A vague idea needs sharpening.
A clear request needs grounding in the codebase. A bug report needs
almost none of this — just confirm and write.

---

## How to run this skill

### Phase 1 — Listen

Let the user describe what they want. If they've already started
explaining, continue from there. If not:

> "What do you want to build?"

Do not explain the factory process. Do not ask setup questions. Let them
talk. If they trail off or are vague, that's expected — capture whatever
they give you.

Read the stakes from how they describe it. A carefully worded request
deserves deeper probing than a quick fix. Calibrate depth implicitly —
do not ask "how detailed should this be?"

### Phase 2 — Assess clarity

Before asking follow-ups, understand what you're working with:

**Clear idea** — the user knows what they want, the problem is specific,
the scope is bounded. Move to phase 4 (codebase research).

**Partially clear** — the problem is real but the framing might be off,
or there are obvious gaps. Move to phase 3 (sharpen), focusing only on
the unclear parts.

**Vague idea** — half-formed, solution-first, or the motivation is murky.
Move to phase 3 (sharpen), using the full set of tools.

Signals for each:

| Type | Signal |
|------|--------|
| Clear | Specific problem, bounded scope, user has thought it through |
| Partially clear | Real need but assumed framing, or "I think we should..." with unstated reasoning |
| Vague | "What if we...", broad problem ("it's too slow"), solution looking for a problem |

### Phase 3 — Sharpen (when the idea needs it)

Use the thinking tools below selectively based on where the vagueness is.
Not all apply to every conversation. Describe the move, not the framework
name — say "let's check what we're assuming" not "I'm applying First
Principles." One question at a time. Let the answer land before probing
further.

#### When the problem is unclear

**5 Whys — drill past symptoms to the root cause.** When a problem is
stated, it's almost always a symptom. Ask "why does this happen?", take
the answer, ask "why?" again, and repeat until you hit a cause you can
actually act on. The surface statement and the root cause are often far
apart — "users don't come back" might trace to an incentive structure
problem, not a UX problem. Different "why?" branches can surface
different root causes; following multiple branches is valid. Stop when the
next "why?" has no useful answer.

**Socratic questioning — expose assumptions through questions, not
assertions.** Follow a hierarchy: clarification ("What do you mean by
X?"), assumptions ("What are you taking for granted here?"), evidence
("What makes you confident in that?"), perspective ("How would someone who
disagrees see this?"), implications ("If this is true, what else
follows?"). The method is non-confrontational — you're helping the person
examine the idea themselves, not arguing it's wrong. It works best when
the questioner is genuinely curious, not leading the witness.

#### When the framing feels inherited or assumed

**First Principles — break the idea down to what's actually known.**
Most ideas are inherited — built on conventions, analogies, and
assumptions that were never questioned. List every assumption embedded in
the idea, then challenge each: "Is this actually true, or just how things
are usually done?" Strip away convention and see what survives. The result
is often a simpler, more direct framing — or reveals the idea was built
on a shaky premise. This is particularly useful when someone proposes a
solution that mirrors how something is done elsewhere without examining
whether the same constraints apply here.

**System 1 vs System 2 — notice when intuition is driving.** The mind
operates in two modes: System 1 is fast, automatic, pattern-matching — it
generates confident-feeling conclusions without conscious reasoning.
System 2 is slow, deliberate, and checks logic. Most of the time, System
1 produces a plausible answer and System 2 endorses it without scrutiny —
the user feels like they reasoned their way to a conclusion, but they
mostly rationalized a gut response. When the user's conviction is instant
and feels obvious, that's System 1. Slow things down: "Let's make sure
we're reasoning this through rather than going on feel." The goal isn't to
dismiss intuition — it's to verify whether it holds up under examination.

#### When the motivation is murky

**Jobs to Be Done — identify the real outcome being sought.** People
don't adopt ideas because of what they are — they "hire" them to fulfill
a job: a functional, emotional, or social outcome. Understanding the job
reframes what the idea actually needs to do. Instead of asking "what
problem does this solve?", ask "what are you trying to get done — or
become — when you reach for this?" A useful probe: "When does the urge to
have this arise? What triggers it? What would you do instead if this
didn't exist?" The answer often changes the idea significantly — the
stated problem and the actual job may be different things.

#### When it all sounds too neat

**WYSIATI (What You See Is All There Is) — surface invisible unknowns.**
When evaluating an idea, the mind takes whatever information is available,
builds the most coherent story it can, and presents it as a complete
picture. It doesn't flag gaps — missing evidence is invisible, not
weighed as uncertainty. The less you know, the easier it is to build a
confident story. The question is not "does this make sense given what we
know?" but "what information are we not seeing that would change this?"
Actively name the unknowns as specific things, not vague uncertainty:
"What would someone who disagrees have access to that we don't?" / "What
assumptions are we making because we have no data, not because the data
supports them?"

**Inversion — think about what would guarantee failure.** The human mind
is better at spotting problems than designing solutions. Instead of asking
"how do we make this work?", ask "what would make this definitely fail?"
Then check whether any of those failure modes are present in the idea.
This bypasses optimism bias — people generating plans tend to underweight
risks, but failure-mode thinking naturally surfaces uncomfortable
possibilities. It can also be applied to any assumption: instead of
arguing why something is true, ask what would have to be true for it to be
false.

#### Before writing the brief

**Steelmanning — articulate the strongest version.** Before writing the
brief, build the strongest possible version of the idea — not a defense
of every claim, but the most compelling, generous articulation of what
this could be. Say: "Here's the strongest version of what I think you're
saying..." and present it. Check with the user: "Does this capture what
you're going for, or does it drift from your intent?" This ensures the
brief captures the best version of the idea, not just the literal words.
Skipping this produces a brief full of hedges with no positive spine.

---

Stop sharpening when the idea has converged — the user's answers are
consistent, the problem is specific, and you can describe the intent
back to them clearly. Do not run through every tool. Exit by alignment,
not by checklist.

### Phase 4 — Research the codebase

Read the relevant code in context of what the user described. Understand
what exists, how it's structured, what the current state is.

This is not optional. The brief should be grounded in what the codebase
actually looks like, not what the user assumes it looks like.

Look for:
- Existing code in the area the user is describing
- Patterns and conventions already established
- Dependencies and integration points that will matter
- Anything that contradicts or complicates the user's description

### Phase 5 — Informed follow-ups

Come back to the user with questions grounded in both their words and
the codebase:

- "You mentioned X, but I see the code currently does Y — is the intent
  to change that, or build alongside it?"
- "There's an existing pattern for Z. Should this follow that, or is
  this different?"
- "I see a dependency on W that might affect this. Is that a constraint?"

If something needs deeper research (external APIs, libraries, protocols),
note it as an unknown. The define-behaviors stage will resolve it.

### Phase 6 — Write the brief

Generate a work-id using the format `YYYYMMDD-HHMMSS-kebab-title`
(e.g., `20260507-143022-cache-status`) and a matching legacy run-id
using the timestamp prefix when fallback artifacts are needed.

Prefer the Work model for new delegated build work, but do not create
the Work Item immediately after writing only the brief. Work Item
planning context is set at `factory work create` time, and write Tasks
derive their stored instructions from that context later. Defer Work
Item creation until the approved brief, behaviors, approach, and plan can
be passed as Work Item planning context. The `plan-execution` stage
creates the Work Item after the user approves `plan.md`:

```sh
factory work create <work-id> --title "<short title>" \
  --brief-file <brief.md> \
  --behaviors-file <behaviors.diff.md> \
  --approach-file <approach.md> \
  --plan-file <plan.md>
```

Store approved planning text in Work Item planning context with
`--planning-context <text>`, `--planning-context-file <path>`, or the
separate planning file flags so write Tasks receive the context through
durable Work state. Write bridge planning artifacts when later skills
need files to review, revise, or pass to legacy fallback:

```
.factory/runs/[run-id]/
  brief.md
  status          ← write "briefed"
```

Write `.factory/active-run` containing the run-id only for the legacy
fallback path.

If the brief is a full-codebase review request, write the review target,
focus areas, and requested reviewers in the brief. Do not start review
execution until the user confirms the brief.

Use the legacy review-run state only for compatibility or explicit
recovery after the user confirms:

- Write `.factory/runs/[run-id]/mode` containing `review`
- If the user wants specific reviewers, write
  `.factory/runs/[run-id]/reviewers` containing a comma-separated
  list (e.g., `documentation,behaviors`)
- If the user wants to focus on specific areas, write
  `.factory/runs/[run-id]/scope` with the paths or description
  (e.g., `skills/`, `src/session.rs`, `the session loop logic`)
- Set status directly to `planned` — skip define-behaviors,
  design-approach, and plan-execution (there are no new behaviors
  to define or approaches to design)

The brief for a review run is short: what to review, why, and
optionally which reviewers and which areas to focus on.

### Phase 7 — Confirm

Show the brief to the user:

> "Here's the brief. Does this capture what you want, or is something
> missing?"

If something is still fuzzy, identify which part and re-enter the
relevant phase. Do not start over.

When the user confirms, the skill is done.

**Review-only work:** If the confirmed brief is a full-codebase review
request (the user wants reviewers to inspect the existing codebase, not
build something new), default to the Work-model review-only path after
capture is complete:

- Write the brief as usual so the Work Item has durable context
- Create the Work Item with the approved brief:
  `factory work create <work-item-id> --title "<short title>" --brief-file <brief.md>`
- Use `factory work review-codebase <work-item-id> <attempt-id>` to add
  the review-only Attempt
- Run the Attempt with `factory work attempt run <work-item-id>
  <attempt-id>`

---

## Brief format

```markdown
# Brief

## What
[What the user wants — one or two sentences, in their words]

## Why
[The problem it solves or outcome they want]

## Context
[What exists in the codebase today that's relevant]

## Constraints
- [Known constraint]

## Assumptions
- [Assumption surfaced during sharpening, with confidence level]

## Unknowns
- [Thing the user is unsure about]
- [Thing that needs research in the define-behaviors stage]
```

Omit sections that have no content. A clear request might only need
"What" and "Why." A sharpened vague idea will have "Assumptions" and
more "Unknowns." Both are valid briefs.

---

## Rules

- **Stay in the user's words.** Do not rephrase into formal language or
  add terminology they didn't use.
- **Do not elaborate.** The brief captures intent, not design.
- **One question at a time.** Do not stack questions.
- **Short is correct.** A brief that's too long has crossed into the
  define-behaviors territory. Push detail to the next stage.
- **Record unknowns explicitly.** "I don't know how auth should work" is
  more useful than omitting auth entirely. Things that need external
  research are unknowns too.
- **Read the code.** The codebase is context. An uninformed brief leads
  to behaviors built on wrong assumptions.
- **Adapt depth to clarity.** A bug fix gets a quick pass. A vague idea
  gets the full treatment. Read the situation.
- **Steelman before writing.** Articulate the strongest version of the
  idea back to the user before capturing it. The brief should reflect
  the best version, not just the literal words.
- **Exit by alignment, not by checklist.** The brief is done when the
  user says it captures their intent — not when all phases are complete.
