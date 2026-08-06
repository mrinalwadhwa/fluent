
# Capture Brief

Interview the user. Write a short brief in their own words that names what they want and why. The brief captures intent — behaviors, design, and implementation belong to later stages.

When planning-wide delegation is active, apply the Fluent skill's delegation
contract throughout this stage. Make grounded recommendation-level choices
without asking for intermediate approval. A mandatory interruption still stops
the stage for one focused user decision.

## Listen

Let the user describe what they want. If they've already started, continue from where they are. Otherwise ask:

> "Do you want to (a) build something new, or (b) review existing code?"

If it's a review request, jump to `## Review-only briefs` — skip the sharpen / ground / steelman loop, which is for build requests.

How they describe it tells you the stakes. Match your effort to what you hear — a hedged one-liner is a smaller thing than a considered paragraph. Don't ask "how detailed should this be?".

## Assess clarity

Before asking follow-ups, decide what you are working with:

| Type | Signal | Next step |
|------|--------|-----------|
| Clear | Bug report; specific problem, bounded scope | Skip sharpen — go straight to grounding |
| Partially clear | Hedging phrases ("I think we should probably..."); assumed framing or unstated reasoning | Sharpen the unclear parts |
| Vague | Off-hand idea at the end of a longer conversation; "What if we..."; "it's too slow"; a solution looking for a problem | Sharpen fully |

## Sharpen when the idea needs it

Ask questions rather than assert. Draw from the frameworks in `references/thinking.md` — its *When to use which framework* table matches situations to tools.

Describe the move, not the framework — say "let me check what we're assuming" rather than "applying First Principles." One question at a time. Let each answer land before the next.

Stop when you can fill the brief's What, Why, and Context sections without inventing details, and any remaining uncertainty fits naturally under Unknowns.

## Ground in the codebase and follow up

Before drafting, inspect the local code or docs that most directly match the request. Read enough to identify the likely files, existing pattern, and any obvious contradiction with the user's description. If the area is unclear, start with top-level docs, file names, tests, and recent patterns, then propose two or three candidate subsystems as (a)/(b)/(c) and ask them to pick.

Look for:

- Existing code in the area the user described.
- Patterns and conventions already established.
- Dependencies and integration points that will matter.
- Anything that contradicts or complicates the user's description.
- Recorded project choices in `.fluent/expertise/decisions.md` (if it exists) that the request may conflict with — flag any conflict for the user rather than resolving it here.

Then come back with follow-ups:

> "You mentioned invalidating the cache on write, but `store.rs:142` already does that. Is the intent to (a) surface it as a public event, or (b) rework how it fires?"

> "There's an existing status-line pattern in `dashboard/status.rs`. Should this (a) follow that, or (b) take a different shape?"

If something needs deeper research (external APIs, protocols, unfamiliar libraries), record it as an unknown rather than blocking on it. Solution choices like library, protocol, or storage get resolved in `design-approach`.

## Check understanding

Before writing, steelman the idea — restate it in its strongest form — and check with the user:

> "The strongest version of what I'm hearing: you want a status endpoint that reports cache-invalidation events directly, so the dashboard can rely on real-time state instead of polling. Is that the shape, or am I missing something?"

Without planning-wide delegation, wait for confirmation before writing. With
delegation active, use the strongest grounded restatement as the provisional
brief and ask only if it exposes a mandatory interruption: a behavior or scope
change, a consequential unsupported assumption, a project-decision conflict, or
missing information.

## Set the draft-id

After normal confirmation, or after establishing the provisional restatement
under planning-wide delegation, generate a draft-id in the format
`YYYYMMDD-HHMMSS-kebab-title` (example:
`20260706-143022-cache-status`). If this planning conversation already has a
draft-id, keep using it. On a resumed session, list `.fluent/drafts/` — if a
draft already exists and the user is continuing it, keep that draft-id instead
of generating a new one.

## Write the brief

Write to `.fluent/drafts/<draft-id>/brief.md` in the format below.

**Do not create the Work Item now.** `plan-execution` creates it only after the
brief, behaviors, approach, and plan are approved through their normal gates or
the single delegated-planning confirmation.

## Show and confirm

Without planning-wide delegation, show the brief and ask:

> "Confirm the brief and move to behaviors? Reply **yes (y)**, or name what to revise: (a) Listen, (b) Sharpen, (c) Ground."

If a part is fuzzy, name which part and re-enter the relevant step (Listen, Sharpen, or Ground). Do not start over.

Once the user confirms, stop here. `define-behaviors` picks up next.

When planning-wide delegation is active, write the brief as provisional, give a
concise progress update without asking for approval, and continue directly to
`define-behaviors`. Do not run this stage's confirmation gate or describe the
brief as approved. Its approval is part of the one final planning-set
confirmation. If the user requests a change in response to the update, revise
the brief and every affected downstream draft before that final confirmation.

## Review-only briefs

For a review request rather than a build request: set the draft-id as above, then write to `.fluent/drafts/<draft-id>/brief.md` using this format:

```markdown
# Brief

## What to review
[The whole codebase, or a specific module or area]

## Why
[What prompted the review — a concern, a milestone, a periodic check]

## Focus areas
- [Specific area or question to prioritize]

## Requested reviewers
- [Role or expertise needed, if the user specified]
```

Omit empty sections. Without planning-wide delegation, run the same Confirm step.
When delegation is active and this is the last applicable planning artifact,
show the complete review-planning set and use the Fluent skill's single final
confirmation; otherwise keep the brief provisional and continue through the
remaining applicable planning stages. The `fluent` skill handles Work Item
creation and the review-only flow only after that confirmation.

## Brief format

```markdown
# Brief

## What
[What the user wants — one or two sentences, in their words]

## Why
[The problem it solves, or the outcome they want]

## Context
[What exists in the codebase today that is relevant]

## Constraints
- [Known constraint]

## Assumptions
- [Assumption that surfaced during sharpening]

## Unknowns
- [Thing the user is unsure about]
- [Thing that needs research in a later stage]
```

Omit sections with no content — a clear bug fix might only need *What* and *Why*; a sharpened vague idea will have *Assumptions* and *Unknowns*.

## Rules

- Capture what the user said and what the codebase shows. Don't rephrase into formal language, invent terminology, or add details you weren't given.
- Ask one question at a time, with a blank line after the question stem. Use two archetypes:
  - **Decision** — pick one option. Label the options (a)/(b)/(c), each self-contained; put the
    recommended option first and mark it `(recommended: <why>)`. The answer is a single letter.
  - **Confirm gate** — approve or route back: "Reply **yes (y)**, or name what to revise:
    (a).../(b).../(c)...". The default is yes; a bare `y` is accepted.
  Avoid the anti-pattern: an unlabeled "X or Y?" that forces the user to re-describe an option.
- Planning-wide delegation is the only exception to the question-by-question
  rule: after an explicit request, do not ask recommendation-level questions or
  intermediate confirmation gates; keep mandatory interruptions and the final
  planning-set confirmation.
- Record unknowns explicitly. "I don't know how auth should work" is more useful than omitting auth.
