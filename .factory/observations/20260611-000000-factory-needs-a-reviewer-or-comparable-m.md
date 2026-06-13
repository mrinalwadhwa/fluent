2026-06-11 — Factory needs a reviewer (or comparable mechanism) that
maintains a vocabulary map — nouns and verbs used to describe the
domain — across code, docs, behaviors, and skills, with the goal of
preventing divergent terms for the same concept. Concrete example:
during this session's pre-land hook discussion we discovered that
the code uses both `land` (`MergeCandidateMergeStatus::Landed`,
`record_candidate_landed`, "pre-land checks") and `merge` (`factory
work merge`, `merge_candidate`, "merge-time reviewers") — sometimes
as synonyms, sometimes for distinct steps of the same operation.
Without a vocabulary review, every new contributor (human or agent)
picks one and reinforces the drift.

This also surfaces a more general gap: Factory currently has no
project-local mechanism for capturing things it learns about its
own codebase. Project-specific expertise (`.factory/expertise/`) is
the natural home for durable, learned domain knowledge — a
vocabulary map could live as `.factory/expertise/vocabulary.md`
that names the canonical noun/verb for each concept and lists
known aliases to avoid. Adjacent expertise files could cover
architecture invariants, naming conventions for new code, and
similar guardrails. To populate them, Factory needs a nudging
mechanism: a phase, a skill, or a reviewer role that detects when
a new concept is being introduced (or a synonym for an existing
one) and prompts the author to update the relevant expertise. The
vocabulary case is the smallest concrete instance of a larger
"Factory maintains durable lessons it learns about each project it
manages" capability.
