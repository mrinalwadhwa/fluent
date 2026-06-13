2026-06-07 — New model adoption should be a breaking redesign, not a
permanent compatibility bridge. The target operational model is Work
Item, Attempt, Task, Workspace, and Merge Candidate. `.factory/runs`
should stop being a first-class execution model once the replacement
works; old run/session-loop code, run-centric docs, run-centric tests,
and run-centric dashboard concepts should be deleted rather than carried
indefinitely.

Adopt the new model in this sequence:

1. Define the target state in code and docs. Work Items hold durable
   intent and planning artifact versions. Attempts represent bounded
   tries or phases to satisfy a Work Item. Tasks are schedulable units:
   `write`, `review`, `merge`, `report`, `learn`, and `probe`.
   Workspaces are Factory-managed filesystem/git contexts. Merge
   Candidates are reviewed results waiting to land.
2. Extend durable storage under `.factory/work/` beyond
   `.factory/work/items/`. Avoid making one nested Work Item JSON file
   carry all live operational state once tasks are running. Store live
   objects in separate collections, with references between Work Items,
   Attempts, Tasks, Workspaces, and Merge Candidates.
3. Replace run creation with Work Item and Attempt operations. Add the
   missing transition from `WorkItem -> Attempt -> initial write Task`.
   Existing command names may stay only if they map fully to the new
   concepts; otherwise prefer explicit `work`, `attempt`, `task`, and
   merge-candidate commands.
4. Implement task execution. Start with `write` tasks: allocate a
   workspace, run the selected coder, require clean committed output
   before task completion, and record produced commits/artifacts. Then
   implement `review` tasks as read-only candidate-workspace tasks that
   write only task artifacts and create follow-up `write` tasks for
   concrete fixes.
5. Implement the Attempt loop. An Attempt creates an initial write task,
   runs review tasks from explicit review policy, creates follow-up write
   tasks for failed reviews, moves uncertain review output to
   `needs-user`, and creates a Merge Candidate only after review passes.
6. Implement the Merge queue. Merge Candidates become the only path to
   `main`: rebase, run checks, run the full required reviewer set,
   fast-forward land, record reporting/learning artifacts, and clean
   workspaces.
7. Update dashboard/status around Work Items, Attempts, Tasks,
   Workspaces, Review artifacts, Merge Candidates, and Needs-user items.
   The first adoption slice should preserve legacy Runs while exposing
   Work state; a later breaking slice can remove the old Runs view or
   replace it with an Attempts-oriented view.
8. Rewrite skills and documentation to use the new vocabulary. Briefs,
   behaviors, approaches, and plans attach to Work Items and Attempts.
   Execution happens through Tasks. Landing happens through Merge
   Candidates.
9. Delete the old model after the new execution/review/merge path works:
   remove `.factory/runs` readers/writers, legacy run/session-loop code,
   old run tests, run-centric docs, and compatibility language.
10. Iterate from the new base: independent task scheduling,
   Codex/Claude capacity planning, Fargate task execution, learning
   capture, dashboard interventions, and team permissions.

Progress:
- `407ca59` added the first operational transition:
  `factory work attempt <work-item-id> <attempt-id>` appends a planned
  Attempt plus initial `write` Task from an existing Work Item.
- `73b01db` added write Task execution with the clean committed
  workspace invariant.
- `d699981` and `0ec8788` added review Task planning/execution,
  read-only candidate review enforcement, review artifacts, and stale
  review artifact protection.
- `2cba3a2` and `afb28cf` added the Attempt loop. It drives one Attempt
  through write Task execution, review Task planning/execution,
  follow-up write Tasks for failed reviews, `needs-user` handoffs for
  uncertain or missing verdicts, and stops at the Merge Candidate
  boundary.
- `fc5b54a`, `208dde2`, and `4862b23` added Merge Candidate creation.
  Passed Attempt reviews now create or return one durable Merge Candidate,
  candidates record source/target workspace and branch provenance, the
  Work model enforces one candidate per passed Attempt, and users can
  inspect candidates with `factory work merge-candidate`.
- `9852155` added Merge Candidate execution through `factory work merge`.
  Merge execution now validates candidate provenance and clean workspaces,
  rebases against the target branch, runs configured checks, runs the full
  merge-time reviewer set, fast-forwards the target branch, records
  durable merge status and artifacts, and cleans managed candidate
  workspaces after landing.
- `1630e30`, `11fa927`, `25cb457`, `a80d021`, and `605475d` added Work
  status/dashboard visibility. `factory status` now shows Work Items
  beside legacy Runs, and the dashboard has a Work Items view with
  Attempts, selected Tasks, Merge Candidates, merge state, needs-user
  state, read errors, polling refresh, and actionable/error counts.
- `8ebf4b2` updated the build workflow skills and architecture/behavior
  documentation to teach Work Items, Attempts, Tasks, Workspaces, and
  Merge Candidates as the target lifecycle, while keeping legacy
  `.factory/runs` commands documented as a transitional fallback.
- `03051d8`, `0790846`, and `79444f4` added durable Work task
  instructions. `factory work create` can now store rich Work Item
  instructions from inline text or a file, initial and follow-up write
  Tasks copy that context into `Task.instructions`, and write Task prompt
  generation uses durable Work state while keeping extra CLI args as
  coder flags.
- `4ade899` and `e2b5a5d` made approved planning context first-class in
  Work state. `factory work create` can now store separate brief,
  behaviors, approach, and plan files or a combined planning context;
  initial and follow-up write Tasks derive prompt text from that durable
  context when explicit Work Item instructions are absent. The
  build-in-the-factory, capture-brief, and plan-execution skills now
  teach this Work path instead of defaulting to a legacy
  `.factory/runs/<run-id>/execution-instructions.md` bridge.
- `4f9c52f` and `bc2c4e6` made split Work storage authoritative.
  `WorkModelStore` now reads Work Item metadata from `.factory/work/items/`
  and assembles Attempts, Tasks, and Merge Candidates only from their
  split collections. Nested operational collections in item JSON are
  ignored, and architecture/behavior docs plus storage tests describe the
  split collections as the Work storage contract.
- `201e8a5` made reviewer prompts Work-native. Bundled reviewer prompts
  now provide `[work-system]` sections for Work review Tasks, Work review
  prompt construction names Work artifact paths and artifact-local
  writable output locations, merge-time reviewers prefer `[work-system]`
  with legacy fallback, and architecture/behavior docs describe the
  prompt contract.
- `c10bd34` demoted legacy planning bridge guidance. Planning skills now
  distinguish active pre-Work-Item planning context from durable Work
  Item planning context after `factory work create`; legacy
  `.factory/runs` planning files are fallback or recovery state only.
- `3b9d0aa` made Work write Tasks use a Work-native author prompt.
  Work write Task execution now resolves `prompts/work-author.md`
  instead of the legacy `prompts/author.md`, the Task completion prompt
  accurately states that no committed Task output fails under the current
  executor contract, and tests assert that Work write prompts exclude
  legacy `.factory/runs` status and handoff instructions.

The next adoption slices should keep using the Work path end to end, then
delete legacy `.factory/runs` compatibility once the replacement path is
stable enough to stop carrying the old model.
