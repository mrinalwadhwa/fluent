2026-06-18 — Meta-planning for multi-Work-Item arcs currently lives
outside Factory's tracked artifacts. Today's pattern: a large
initiative (e.g., the 9-slice Tester redesign, the learning
architecture rollout, the Pi/learning-eval thread) lives in
session-handoff documents and ad-hoc .scratch/ directories. Factory
itself has no primitive for an arc / roadmap / program / initiative
that groups multiple Work Items, sequences them, and records why one
Work Item depends on another.

Symptoms:
- Scoping a new Work Item requires loading the relevant handoff doc
  from out-of-band memory.
- No queryable Factory record that Work Items A, B, C belong to arc
  X, nor where in the arc the project currently sits.
- Sequencing decisions ("slice N+1 follows slice N because Y") are
  not durable across compactions except via handoff prose.
- The brief / behaviors / approach / plan skills work at single
  Work Item granularity; they do not have a parent to inherit from.

Options worth exploring (not yet chosen):
- Lightweight convention: stable path like .scratch/<arc>/roadmap.md
  recognized by skills but unmodeled in code.
- New artifact area: .factory/roadmaps/<arc>/ mirroring how
  .factory/observations/ works, holding roadmap.md plus per-arc
  notes; readable by skills.
- First-class Roadmap entity in the Work model: own brief,
  sequencing, references to constituent Work Items; new skills for
  capture-roadmap / sequence-roadmap.
- Meta-Work-Item pattern: a roadmap is itself a Work Item whose
  Tasks plan and launch child Work Items. Layers on top of existing
  primitives.

Recent concrete examples whose meta-planning is currently external:
- Tester redesign with 9 sliced Work Items, captured in
  .scratch/session-handoff-20260618.md.
- Learning architecture (expertise/skills/hooks/observations plus
  learner phases), captured in the same handoff.
- Pi / local-writer / learning-eval discussion captured in
  .scratch/learning-eval-discussion-notes/notes.md.

Resolution wants its own bite-sized discussion: which option fits,
what the primitive's lifecycle looks like, how skills reference it,
how it interacts with cross-Work-Item learning. Not blocking the
Tester slice 1 scoping that the user wants to do next.
