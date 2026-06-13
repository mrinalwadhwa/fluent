2026-06-09 — Start gathering signals for how to reduce Work Item
end-to-end elapsed time without lowering quality. Useful signals include
time spent in authoring, review loops, merge reviews, verification,
scheduler/capacity waits, tool invocation failures, invalid test-command
guessing, and human intervention. Separate latency that buys confidence
from latency caused by orchestration friction.

Recent Work Item runs suggest several promising measurements:

- Compare attempt-review findings with merge-review findings to identify
  which checks must always re-run after rebase and which prior review
  signals can be reused as focused context.
- Track reviewer time lost to poor prompt ergonomics, such as commands
  that are not copy-safe or missing the exact `main..HEAD` diff form.
- Track test and verification retries caused by bespoke shell assertions
  or invalid command guesses. Factory could derive valid verification
  commands from behavior/test metadata instead of asking agents to infer
  them.
- Keep reviewer-owned tests in artifact directories when reviewers need
  extra evidence. This preserves candidate cleanliness while improving
  confidence.
- Record whether a delay came from scarce model capacity, sandbox/tooling
  failure, missing context, or real design uncertainty. These categories
  should drive different scheduling or process improvements.
- Track cases where a small change takes a full review loop even though
  the review findings are predictable from the initial brief. The
  `resume --no-sandbox` work item is a useful example: the author could
  have been guided up front to update the general resume behavior docs
  and add persisted operation coverage, rather than relying on reviewers
  to discover that after the first author pass. Better author prompts
  and brief-to-plan checks could reduce this churn without skipping
  reviewers.
- Assess whether Work Items finish slower when they touch high-churn
  shared files by nature, such as `tests/binary.rs`. Large shared test
  files create merge contention, expand reviewer context, and make small
  logical changes look broad. Splitting tests by domain or behavior area
  might reduce elapsed time and conflicts without reducing coverage.
- Assess whether some extensive reviews should move from the pre-merge
  path to periodic post-merge review on `main`. One possible shape: keep
  deterministic tests as the hard rebase regression gate before merge,
  then run broader qualitative reviews after merge on a schedule or from
  the merge queue. This could shorten Work Item elapsed time, but it
  changes where quality risk is caught. The tradeoff needs explicit
  analysis before changing the gate.
- Merge-time reviewers need explicit guidance for running tests against a
  read-only candidate workspace. During `split-storage-only`, one reviewer
  first failed Cargo tests because Cargo tried to lock
  `target/debug/.cargo-lock` inside the candidate workspace; rerunning
  with `CARGO_TARGET_DIR` under the review artifact directory worked.
  Another reviewer skipped test execution because it interpreted the
  read-only candidate rule as "do not run tests." Factory prompts or test
  reviewer skills should make the intended pattern explicit: read the
  candidate, write build outputs and scratch files only under the review
  artifact area, and run tests with redirected target/cache paths when the
  tool normally writes into the checkout.
- Even with artifact-local `CARGO_TARGET_DIR` guidance, reviewer tooling
  can still accidentally interact with ignored build outputs in the
  candidate workspace. During `work-native-review-prompts`, one
  merge-time reviewer tried to remove a candidate `target/` directory and
  hit permission errors. The candidate stayed clean, but Factory should
  make reviewer build/cache isolation more automatic and auditable instead
  of relying only on prompt guidance.
- During `work-cleanup-orphan-artifacts`, merge-time reviewers could read
  the candidate workspace but could not read the sibling merge-check
  artifact directory; `find` and `rg` returned `Operation not permitted`
  for `.factory/work/artifacts/<work>/<attempt>/<candidate>/merge/checks`.
  The reviews still passed from candidate diff and direct verification,
  but merge-check artifacts should either be readable to reviewers when
  the prompt names them, or omitted from reviewer prompts until the
  sandbox grants access.
- During the same Work Item, multiple reviewers initially ran plain
  `git diff -- <path>` in a clean candidate workspace, got empty output,
  then had to rediscover the explicit target-to-candidate commit diff.
  Factory should make the review diff command copy-safe and hard to
  misuse, ideally by including concrete commit ids and path examples
  rather than only prose like `git -C <candidate> diff main..HEAD`.
