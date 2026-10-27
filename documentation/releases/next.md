# Next release: local Work and service transition

## Continue Claude Work from v0.6.0

Use a binary containing the Claude session recovery change in the existing
project. Keep `.fluent`, the managed candidate workspace, and the saved
configuration; do not run `init` again. The published v0.6.0 release is commit
`be19732c42ac645cb9fac5701c8e93171b1aa480`. An arbitrary development build
showing the same version is not the released upgrade fixture.

Run `fluent status`. For an eligible failed Writer, status reports the exact
command `fluent attempt run <work-item-id> <attempt-id>`. Run that command to
recover the same Attempt. Fluent verifies the clean original candidate,
unchanged saved assignment and findings, settled execution owners, and the
retained zero-turn rejection of the exact missing conversation. It records
the recovery and retries only the failed Writer obligation with at most one
fresh persistent conversation. The first Writer commit, review findings,
failed transcripts, and previous execution records remain available.

This includes the retained Claude Code 2.1.307 result with `stop_reason: null`,
`result_index: 0`, and `usage.fallback_credit: null`. Fluent validates these
values explicitly along with zero usage and the exact rejected session ID.
Unknown metadata, additional results, positive usage or credit, and model or
tool progress do not authorize this recovery.

If the evidence changed, a worker still owns the execution, or the retained
failure cannot prove this exact case, resume refuses with a diagnostic. Keep
the reported evidence for investigation. Ordinary round caps, user stops,
authentication failures, and executions with model or tool progress retain
their existing recovery requirements. Inspecting Work never authorizes or
performs this recovery. Reviews, the final Tester, host acceptance where
required, and Learning still govern the recovered candidate.

New Claude Writers save conversations in the Attempt's private home and
resume available conversations with the current assignment and findings.
After recovery records new authority, older binaries refuse to mutate that
Work; continue with the upgraded binary.

Deterministic upgrade tests prepare the checksum-verified published executable
with `bash tests/fixtures/prepare-released-060.sh`, then use isolated projects
and provider doubles. Real CLI persistence requires the separate host check
`bash tests/host/check-claude-writer-session-upgrade.sh` on the unchanged
candidate, with `FLUENT_CLAUDE_SESSION_HOST=1`,
`FLUENT_CLAUDE_SESSION_MODEL`, and `FLUENT_CLAUDE_SESSION_EFFORT` set by the
host operator. This check runs paid Claude turns in both headless and native
execution modes and retains candidate and binary identities with its evidence.

Run the additional host check
`bash tests/host/check-claude-real-legacy-upgrade.sh` with
`FLUENT_CLAUDE_LEGACY_HOST=1` and the same model and effort variables. It creates
a separate disposable project with the checksum-verified v0.6.0 binary, runs
actual Claude under production confinement, and requests a first candidate
that needs a review correction. It requires the real missing-conversation
failure before invoking the candidate's public resume command. It retains the first
commit, failure artifacts, recovered Writer evidence, and both Git histories.
It cancels through the public command once the recovered Writer completes.
A pass proves recovered Writer progress; reviews, final Tester, Learning, and
merge readiness receive no credit from this check.

Both checks require a clean, unchanged candidate. Each prints its evidence
directory under `tests/output`; `FLUENT_CLAUDE_LEGACY_OUTPUT_DIR` may select a
new output directory. The legacy check also retains a private disposable
fixture and records its path. That fixture contains provider credentials;
share only the separate evidence directory. Neither check consumes or resets
the frozen combined-upgrade project. The combined Node/Claude upgrade exercise
remains required before recommending a release to the reporter.

### Verify the integrated upgrade

The coordinator runs `bash tests/host/check-integrated-upgrade-prerequisites.sh`
only after the combined Writer candidate is clean and paused for required host
evidence, with no executing Tasks. Set `FLUENT_INTEGRATED_UPGRADE_HOST=1`,
`FLUENT_INTEGRATED_SOURCE_ROOT` to the project that owns the integration Work,
`FLUENT_INTEGRATED_WORK`, `FLUENT_INTEGRATED_ATTEMPT`, and
`FLUENT_INTEGRATED_OUTPUT` to a new directory outside the candidate checkout.
The corresponding `--source-root`, `--work`, `--attempt`, and `--output` arguments
are also supported. Keep the real-provider settings required by the constituent
host checks, including the selected Claude model/effort and native Pi setup.
Remove synthetic `FLUENT_TEST_*` settings before invoking the driver.

The driver builds a production executable without installing it, runs every
source prerequisite with its original command, and runs the four integration
boundary regressions with actual Seatbelt required. It records candidate and
binary identities, full selected logs, native generation/settlement receipts,
and dashboard/SSH PTY phases. It rechecks the paused Attempt and candidate after
execution. A constituent failure leaves an incomplete receipt; prior source
passes cannot fill its gaps. The real all-provider SSH journey and the complete
canonical Tester command union remain separately required. Attach the resulting
evidence through the public Fluent command and obtain targeted Tests Review.

Root alone may run `bash tests/host/check-integrated-released-060-upgrade.sh`.
This command now verifies two separate exercises under the approved independent
exercise option. Exercise A remains historical: its actual continuation used
`6167cf4d`, reached a ready fixture candidate, and retained missing standalone
Writer package proof and nonterminal unused Learner reservations. The driver
checks its original envelope, executable, 26 hashes, public records and artifacts
before and after B. It never resumes A or treats those reservations as settled.
Supply `--historical-evidence` (or `FLUENT_INTEGRATED_HISTORICAL_EVIDENCE`) with the
original failed envelope whose SHA-256 is
`d394e05a73df43e99f37528af419bac5b2a7993e3cb84ff349f549db611f912c`.
Its embedded paths identify the retained original files; keep those files intact.

Exercise B uses its own isolated genuine released-0.6.0 project on the final clean
candidate. Root creates and operates that project through the existing released
initial Writer/tag/correction sequence and public missing-session recovery.
The source Writer only changes the harness and its hermetic regressions. A new
exercise cannot borrow A's Work, session, Task artifacts or package results.

Use the same integration settings and explicitly set `FLUENT_INTEGRATED_FIXTURE`
and `FLUENT_INTEGRATED_FIXTURE_MANIFEST` (or `--fixture` and
`--fixture-manifest`) for B. Freeze the root-owned dynamic manifest before its
continuation. It supplies actual observed identities, never invented results:

- `schema_version: 1`, `scope: "additional-genuine-060-writer-package-exercise"`,
  `fixture_root` (absolute), `project` (relative), `work`, and `attempt`;
- `released`: `path`, `sha256`, and `version`, matching the pinned
  `fluent 0.6.0 be19732c` executable;
- `candidate`: final integration `commit`, `binary_path`, `binary_sha256`, and
  `version`. The driver also builds and records its own production executable
  from that same clean commit; the two builds retain their own byte identities;
- `initial`: `seed_commit`, `writer_commit`, `writer_task_id`, the exact
  `writer_output` object, absolute `assigned_workspace`, and `host_tag`;
- `failure`: `task_id`, `session_id`, source-relative `artifact_path` of the
  original failed Task's `transcript.jsonl`, `artifact_sha256`, `num_turns: 0`,
  and the original Attempt's `public_pause` object. The retained initial
  `writer_runs` session must match that genuine zero-turn failure;
- `configuration`: source-relative paths to original SHA-256 digests, including
  `.fluent/config.yaml` and `.fluent/tester.yaml`;
- `snapshot_root` (absolute), `original_public_snapshot` (relative paths to
  SHA-256 digests), and `original_item` (one of those paths). That frozen public
  Work export must include the failed released Task and its initial Writer.
  Also include the original native failure bytes and configuration files in
  the snapshot, with configuration paths matching the `configuration` map;
- `prerequisites` and `prerequisites_sha256`, identifying the complete receipt
  for the final integration candidate;
- `stage2_marker` (fixture-relative) and `stage2_marker_sha256`, identifying the
  existing marker after root advances it. The driver only reads that marker.

Continue B once with the command above. The driver saves
`continuation.json`, the original manifest, before/after records, preserved
history hashes, the integration binary identity, and the installed npm symlink,
runtime path and hash. It then exits nonzero with instructions to select the
completed package evidence. This first invocation is not a passing host check.
A driver journal under the fixture's `.scratch` prevents another invocation of
`attempt run` for the same integration candidate.

After continuation settles, select evidence from its actual completed Tasks in
a separate JSON file, outside completed Task history. This selection supplies:

- `schema_version: 1`, `work`, and `attempt` for the retained fixture;
- `writer.task_id` and `writer.candidate`, matching the final Writer's output;
- `writer.session_id` and `writer.generation`, matching the native invocation
  and its settled execution record; `writer.execution` and `writer.receipt`,
  each with source-relative `path` and `sha256`, select the exact assigned
  `execution.json` and `execution-receipt` (including an audited retry area);
- `writer.commands`, in order, for
  `npm ci --offline --ignore-scripts --no-audit --no-fund` and `npm test`.
  Each entry records the exact `command`, native
  `tool_use_id`, and an `artifacts` array containing exactly the current Task's
  complete `transcript.jsonl` with source-relative `path` and `sha256`.
  The verifier pairs each standalone Claude Bash request with its synchronous,
  uninterrupted native success result in the assigned workspace. Commands must
  run sequentially as separate calls: the ci result precedes the test request,
  and both successful results precede the final producing commit request and
  result. Its native Git output must resolve to the final Writer commit.
  Explicit native `is_error: false` supplies success without a numeric exit;
  include `exit_code` only when the native result actually supplies `exitCode`.
  Pipelines, background calls, masked
  failures, string-only evidence, foreign results and unsupported formats fail;
  a selected zero status or whole-Task success alone supplies no proof;
- `tester.task_id` and `tester.candidate`, matching the final canonical Tester's
  `review_context.candidate_commit` (Tester output may be null);
- `tester.results`, with the source-relative `path` and `sha256` of that Task's
  registered `tester-results.json`, and `tester.artifacts`, containing `path`
  and `sha256` for every stdout/stderr log referenced by its command results.

Keep the frozen manifest, instructions and `.fluent/tester.yaml` unchanged.
Neither role needs to emit a new proof document. The driver validates the
original npm commands and their results; the separate Node prerequisite retains
broader npx coverage. The installed npm receipt and original execution artifacts
remain available for targeted review.

The verifier recognizes literal external commands joined by `&&` or `;` when
checking inherited shell context. Keep package calls separate, and use a literal
`git commit` call, optionally preceded by `git add` calls. Dynamic command names,
assignments, substitutions, aliases, functions, sourced scripts, nested shells
and unsupported syntax leave the working directory unproven. Such transcripts
fail closed; absence of a literal `cd` does not establish the directory.

For independent B, the final Writer's assigned workspace must be clean at its
producing tip. Compare that Git tree to the first Writer regression commit,
`bc7a52c09a154de5831378bf5f7504bdb2ad1078`, rather than the earlier seed.
Retain every tracked path and mode, and every blob except `story.js` and
`README.md`. This protects the regression, package scripts, lockfile, local
dependencies, configuration and agent instructions even when the source-project
checkout has not changed.

Using the same integration settings and a fresh output directory, run:

```bash
bash tests/host/check-integrated-released-060-upgrade.sh \
  --verify-only --continuation-output /absolute/first-output \
  --package-proofs /absolute/package-selection.json \
  --output /absolute/verification-output
```

To retain the exact declared host command without flags, set
`FLUENT_INTEGRATED_VERIFY_ONLY=1`, `FLUENT_INTEGRATED_CONTINUATION_OUTPUT`,
`FLUENT_INTEGRATED_PACKAGE_PROOFS`, and a fresh `FLUENT_INTEGRATED_OUTPUT` instead.

Verify-only never invokes `attempt run`. It can validate evidence selected after
continuation, including after public recovery has settled the same fixture. It
requires all five ordinary reviews, succeeded no-expertise Learning, every
registered execution durably settled, the ready Merge Candidate, matching
Writer/Tester commits and ancestry, complete hashed package outputs, and preserved
configuration, unrelated Work, prior Attempts and completed history. Missing or
changed evidence fails without consuming the fixture again. Each verification
retains a digest reference to the original continuation and its binary identity.

If continuation pauses, retain the history and follow the public recovery
guidance. Do not reset the fixture or claim another unchanged 0.6.0 reproduction.
A changed integration candidate needs fresh prerequisites and its own recorded
continuation of B through public recovery. A remains historical on its actual
executable. The aggregate receipt names both stages separately. Neither driver
publishes, installs an operator
CLI, lands Work, or grants verification credit itself.

## Move unfinished Work forward

Stop the old shared scheduler before the new binary writes Work state. With
v0.5.0, run `fluent scheduler stop` in each registered project to drain its
coordinator and settle its active Work. This command does not stop the shared
service. Stop the original supervised service process after every project has
settled, then verify that the process has exited and its health socket no
longer answers. The `--all` flag belongs to the newer binary and is not
accepted by v0.5.0. Start the replacement only after the old owner exits.

Inspect Work with `fluent work-item show <id>` and run
`fluent work-item upgrade <id>` for a legacy split record. The command retains
the Work and Attempt identities and is safe to repeat after an interrupted
publication. Continue with the next public command shown by `work-item show`
or `fluent status`; a canceled Attempt needs its normal recovery authority.
Read-only inspection leaves the stored format unchanged.

The release-transition checks use the published v0.5.0 executable at commit
`6875b4091c88d0012181181bc641739a74fb2588` (SHA-256
`8bb76717be942e4a0c97c297c318c0a1331e92105180edb386354dddbae12974`)
in isolated projects. The focused tests verify planned Work migration,
interrupted publication recovery, and refusal by the predecessor against a
current record and a pending current transaction. The strict host service
check exercises two registered projects, queue retention, service stop, and
replacement; its result is recorded by the release-transition host suite.

## Rollback boundary

After the newer binary writes a Work record or publication transaction,
reinstalling v0.5.0 alone cannot roll it back. The predecessor must reject
mutation of that state. Continue forward with the compatible binary and the
public recovery command. This release does not offer a supported backup
restoration procedure: no complete restore set has been rehearsed, and
restoring an older backup would discard later Work. There is no automatic
lossy downgrade.
