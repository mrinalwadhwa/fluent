2026-06-19 — Factory bug-hunter Task using a clean-room synthetic project

Idea: a Factory subcommand (or skill) that exercises Factory end-to-end
against a freshly-invented project, with the goal of surfacing bugs in
Factory itself.

The shape of the task:

1. Invoke `factory bug-hunt` (name TBD) in a clean directory.
2. The harness invents a small, plausible project from scratch — a
   handful of files, a build system declared in `.factory/tester.yaml`,
   a stub for `.factory/extract-tester-results`. Anything reasonable;
   the project's actual content is not the point.
3. The harness then drives Factory through the full lifecycle: creating
   Work Items, running Attempts, committing changes, running the
   Tester, dispatching reviewers, merging, and so on.
4. The harness watches for anomalies: failed Tasks, sandbox violations,
   inconsistent artifact paths, transcripts the parsers cannot read,
   reviewer verdicts that disagree with the writer's intent, retries
   that loop forever, files written to surprising paths, prompts that
   reference non-existent state, etc.
5. The harness reports any inconsistency as a Factory bug — ideally
   with a minimal reproduction the developer can run by hand.

Why this is worth building:

- Many Factory bugs surface only end-to-end. Unit tests cover model
  invariants and individual functions; integration tests cover task
  spawn and merge; nothing currently exercises the full path with a
  fresh project where Factory makes ALL the decisions.
- Each upcoming change to the writer/reviewer/Tester pipeline (Pi
  routing, parallel-groups, characteristics, prompt overhaul, etc.)
  could be paired with a bug-hunt run as a smoke test.
- A synthetic project removes the "is this a Factory bug or a real
  project bug?" ambiguity that plagues debugging against `main`.

Open questions:

- What surface does the harness use to drive Factory? Direct
  subcommand invocations? An expectation language? A scripted
  conversational agent?
- How does it judge "anomaly"? Heuristics, golden expectations, or a
  reviewer Coder that grades the run?
- Where does it record findings — a report file? Auto-filed
  observations? A dedicated artifact area?
- Does the bug-hunt project ever produce a useful artifact, or is it
  pure throwaway?
- Is the synthetic project always the same shape, or does the harness
  vary it (different languages, different test runners, different
  failure modes seeded intentionally)?

Related precedents inside Factory:

- The Tester smoke runs (`src/tester.rs` tests) exercise the Tester
  runtime in tempdirs but do not drive the full Work model.
- The Pi smoke tests (`src/coder.rs` Pi setup) check individual
  Coder invocations but not the lifecycle around them.

Worth scoping as a Work Item once the current prompt-system overhaul
lands.
