# Native agent Tasks

Select `--terminal-mode native` when creating an Attempt to run assigned agent
Tasks in the provider's foreground interface. `--terminal-mode headless`
explicitly selects headless execution. Either CLI option overrides configuration.
Without a CLI option, Fluent reads `terminal.mode` from the project's
`.fluent/config.yaml`, then the user's `~/.config/fluent/config.yaml`, and
defaults to headless when neither sets a mode. Fluent saves the selected mode
on the Attempt for subsequent Tasks and retries. Existing Attempts use their
saved mode; only legacy Attempts without a saved mode resolve to headless.
Resolve the provider, model, and effort (Pi thinking level) before launch.
Writer Tasks retain the Attempt's selected conversation across authorized
continuations. Review, Learner, and
rebase Tasks launch new ephemeral conversations; Codex never resumes a Writer
thread for these roles. Fluent still advances each Task through its normal
review, Tester, Learner, and landing gates.

Fluent owns the terminal, input admission, provider connection, and Task
evidence. Codex connects its TUI to a private app server through Fluent's
relay. Claude sends lifecycle hooks to a private receiver. Pi loads one owned
extension and writes its assigned session in a private agent directory. The
terminal can be attached for interaction, but its display is not the Task
evidence source.

To steer an active Task, list its terminal sessions and attach with Control:

```bash
fluent terminal list --source <checkout>
fluent terminal attach <session-id> --source <checkout> --control
```

Attaching without `--control` is read-only. The terminal session ends when its
invocation settles; detaching does not hold up Task progression.

For a fresh Codex Task, Fluent starts the assigned thread before opening the
TUI and keeps that app-server connection subscribed. Codex does not create a
resumable rollout until the first turn. The relay answers the TUI's initial
resume request with the app-server's original thread response, then forwards
the TUI's single Task prompt over that same connection. The app-server records
the real turn and Fluent retains its original events.

The Codex display relay keeps one FIFO with separate queue limits: 12 MiB for
ordinary frames and 64 MiB for large responses, plus one in-flight message of
at most 64 MiB. Partial writes retain their unsent bytes and release the
WebSocket lock between retries, so admitted input can progress while output
drains. Three seconds without a successful socket write detaches the display;
continuing progress can take longer. Queue overflow, permanent transport
failure, cancellation, and provider closure still bound teardown. Display
failure never supplies provider completion or changes the assigned conversation.
The payload-free relay outcome `send-pending` identifies actual transport
pressure once per retained message; it is not a provider error or completion.

Native Claude requires Claude Code 2.1.295 or later. This version supports
`onFailure: "block"` for the managed hooks, including hook launch errors and
timeouts. Fluent checks this capability before reserving a native Task.

Native Pi requires a selected `local-openai` model in the operator's
`PI_CODING_AGENT_DIR/models.json` or `~/.pi/agent/models.json`. Set
`FLUENT_NATIVE_PI_ENDPOINT` only to replace that model's local server URL;
the selected model's capabilities and limits remain in the private Task
configuration. `FLUENT_NATIVE_PI_API_KEY` is read only when
`FLUENT_NATIVE_PI_ENDPOINT` is set; it then replaces that model's credential.
Fluent rejects a missing or incompatible selected model. The owned Pi extension
returns `handled` when Fluent does not acknowledge input and shuts down its
session when reporting fails. The owner also checks liveness and periodic
heartbeats, then fails the Task through recovery if evidence is incomplete.

Each invocation writes `transcript.jsonl` with versioned
`fluent-native-agent-event` records. Fluent reads those records for review,
conversation identity, and usage. Native Claude also saves
`claude-session.jsonl` for continuity and `claude-task-usage.jsonl` for usage
from the current invocation. Native Pi saves `pi-session.jsonl` for continuity.
An interrupted invocation or missing evidence fails through the Task recovery
path; the next Task cannot claim its completion.

If Pi's persisted session fails validation after settlement, Fluent saves
`pi-session-validation-<generation-sha256>.json` in the Task artifact directory
before cleaning up the private home. The filename hashes the execution generation,
and publication never replaces an existing diagnostic. Recovery can reuse the
same Task artifact directory without losing earlier failures. The validation
error reports the exact retained path. The valid `pi-session.jsonl` snapshot still
replaces the previous valid snapshot for resume. A diagnostic records the
validation stage, execution generation, owned session path, identity fingerprints
and structural metadata. The read stops at
64 KiB; the hash describes the whole file only when `covers_entire_file` is true.
An oversized header fails closed. Diagnostics omit message bodies, unknown
field names and configuration values. A retained diagnostic does not establish
successful completion or explain how the session became invalid.

Native evidence consumer checks:

| Consumer | Named test |
| --- | --- |
| Task validation | `src/review_inspection.rs::accepts_successful_native_codex_inspection_and_rejects_failed_command` |
| Review prompt | `src/work_task_executor.rs::native_evidence_reaches_review_prompt` |
| Usage accounting | `tests/native_agent_tasks.rs::native_evidence_reaches_existing_consumers` |
| Recovery reader | `src/transcript.rs::native_evidence_reaches_recovery_reader` |

If Codex exits before its first turn, run
`tests/host/check-native-codex-first-turn.sh` after setting
`FLUENT_TEST_REAL_NATIVE_AGENT_HOST=1`, `FLUENT_NATIVE_CODEX_MODEL`, and
`FLUENT_NATIVE_CODEX_EFFORT`. Set the latter two to the resolved Codex model and
effort. The script stops after an assigned `turn/started` and
cancels the disposable Task through the public owner. Inspect the fixture's
`native-codex-relay-diagnostics.jsonl` for the first failing direction and
`native-codex-client-diagnostics.jsonl` for the client exit status. These files
carry the execution generation and assigned thread. The relay trace stores
protocol method names without payloads; the client record stores only known
output signals from the native pane. This focused check does not replace the
full real-provider acceptance check.

To verify display pressure on an authorized host, run
`FLUENT_TEST_REAL_NATIVE_AGENT_HOST=1 bash tests/host/check-native-codex-display-backpressure.sh`.
The check builds the production binary without `test-support`, uses the installed
Codex CLI with `gpt-6-astra` and low effort, and creates a disposable native
Writer. It requests bounded tool output, requires an actual `send-pending`
diagnostic, submits Control input, and requires successful canonical completion
for the latest admitted input in that same generation and conversation. It
fails if pressure was never observed. It does not pause or signal any process.

The command creates a fresh temporary evidence directory by default. Set
`FLUENT_NATIVE_HOST_OUTPUT_DIR` to a new, nonexistent directory to choose its
location. Keep that directory: it contains the exact candidate, production
binary hash, installed CLI identity, original Task and attachment logs, native
transcript, pressure record, and generation-matched settlement record. Fixture
failure invokes public cancellation through its owned Task child. A passing
host result requires successful Task exit and verified settlement, rather than
crediting that fallback cancellation as completion. The socket producer/consumer
regressions are hermetic and provide no real-provider or SSH acceptance credit.
This check remains separate from the canonical all-provider SSH journey.

Run `tests/host/check-native-agent-tasks.sh` only after host and provider-run
authorization. Set `FLUENT_TEST_REAL_NATIVE_AGENT_HOST=1` and the resolved
`FLUENT_NATIVE_CODEX_MODEL`, `FLUENT_NATIVE_CODEX_EFFORT`,
`FLUENT_NATIVE_CLAUDE_MODEL`, `FLUENT_NATIVE_CLAUDE_EFFORT`,
`FLUENT_NATIVE_PI_MODEL`, and `FLUENT_NATIVE_PI_EFFORT`. Set
`FLUENT_NATIVE_PI_ENDPOINT` to the local HTTP server with an explicit port for
the selected `local-openai` profile. The script also requires `expect` for
terminal attachment. It stores its fixture
repos, Task logs, candidate, and provider versions under a host evidence
directory. Review the candidate-bound host results through the targeted Tests
Review and remaining domain reviews. The final Tester then runs the complete
declared suite, followed by the normal Learner and landing gates.
The host check first compacts an idle session through Pi's installed SDK, then
queues a follow-up while its installed Agent is running. Pi's own queue,
next-turn hooks, and settlement callbacks run against deterministic model
responses; Fluent checks their events with its Pi gate. This component check
runs without a provider connection.
Its real-provider fixture uses a disposable local proxy to return one
retryable Pi response before forwarding to the selected server. It separately
checks that `/compact` during active work aborts that activity, and that
receiver loss fails the Task without sending rejected input to the model.
Pi records a generation-bound input-handler rejection in the Task artifact
when its owner cannot acknowledge an attempted input.
Codex, Claude, and Pi each run a confined Writer
and native Review with attached input, a settling-boundary input attempt,
detachment, evidence inspection, and cancellation. A disposable Codex Attempt
also holds an observer through automatic Review progression, then checks its
native Learner and a conflict-driven native Rebase. The real-provider check
must pass on the exact candidate before its targeted Tests Review can accept
host evidence. The provider-backed host cases and the final Tester are separate
acceptance gates.

## Diagnose acceptance prerequisites

The retained 2026-10-24 acceptance receipt for candidate
`cca3f9a1908c11a9109a818ade9fbd85c5f9036d` records incomplete journeys,
not passing acceptance. Its immutable input is
`.fluent/work/artifacts/20261025-restore-native-acceptance/inputs/0000-intake.json`.
The receipt embeds the original files with SHA-256 hashes; keep those bytes
unchanged when comparing a later candidate.

Claude Code 2.1.307 rejected the fixture's mandatory standalone `sleep 900`.
Its Stop event reports that the CLI required Monitor with an until-loop;
the fixture explicitly prohibited that alternative. Replace the assignment
with a supported native interaction. The prerequisite fixture uses
`AskUserQuestion`, whose dialog stays open until the operator answers, and
checks the corresponding `PreToolUse`, `PostToolUse` and `Stop` events in the
same generation. A final response saying it is waiting does not prove a live
invocation. Monitor watches run in the background, so their existence alone
cannot hold a Fluent invocation open. See the [Claude tools reference](https://code.claude.com/docs/en/tools-reference#askuserquestion-tool-behavior).

Pi 1.3.0 admitted input, emitted `agent_start`, and recorded an assistant
`message_end` with `stopReason: error`: the selected local server returned
HTTP 400, `Unexpected message role.` The subsequent `agent_before_settle`
reported `outcome: error`. Fluent correctly withheld settlement. The original
Pi transcript hash is
`6026a435281e6d2a4fbfb5086efb3a7f5bf05c69624117dd294349c9a20cd481`.
This establishes a server request prerequisite failure, not a missing native
protocol callback. The receipt does not contain the outbound HTTP request or
the complete model configuration, so it does not establish which role failed.

The installed Pi 1.3.0 `schemas/models.schema.json` supports
`compat.supportsDeveloperRole`. Its bundled OpenAI completions adapter selects
`developer` for reasoning models when that capability is enabled and otherwise
uses `system`. For a server verified to reject `developer`, set
`"compat": {"supportsDeveloperRole": false}` on the selected model in a private
fixture `models.json`. Preserve its other limits and capabilities. Fluent's
existing model selector already preserves this setting; no adapter change is
needed. Do not change operator configuration or disable reasoning speculatively.
The authorized host must exercise the corrected configuration with the real
installed Pi and retain its successful activity, or the actual error and a
pending result. A reachable endpoint or a selected model alone is insufficient.

The original dashboard inherited the operator's project registry. Give the
acceptance dashboard a private HOME and explicit sources. Keep provider
credentials in their normal host-owned locations; never copy private SSH keys
or credentials into evidence. The native prerequisite check and the complete
SSH journey remain separate candidate-bound gates.

Run the separate prerequisite command only on an authorized host:

```bash
bash tests/host/check-native-acceptance-prerequisites.sh
```

Supply these environment variables in that host shell:

| Variable | Value |
| --- | --- |
| `FLUENT_TEST_REAL_NATIVE_AGENT_HOST` | `1`, after host/provider authorization |
| `FLUENT_NATIVE_DISPOSABLE_FIXTURES` | `1`, after disposable-state authorization |
| `FLUENT_NATIVE_ACCEPTANCE_OUTPUT` | A new absolute directory outside the candidate |
| `FLUENT_NATIVE_CLAUDE_MODEL`, `FLUENT_NATIVE_CLAUDE_EFFORT` | Explicit Claude selection |
| `FLUENT_NATIVE_PI_MODEL`, `FLUENT_NATIVE_PI_EFFORT` | Explicit local Pi selection |
| `FLUENT_NATIVE_PI_ENDPOINT` | Local HTTP endpoint with an explicit port |
| `PI_CODING_AGENT_DIR` | Optional source directory containing `models.json`; defaults to `~/.pi/agent` |
| `FLUENT_NATIVE_PI_SUPPORTS_DEVELOPER_ROLE` | Optional `false` or `true`, chosen from the endpoint's observed behavior |

Keep provider authentication in the host environment or keychain. The driver
rejects synthetic `FLUENT_TEST_*` overrides, builds production Fluent from the
clean candidate in a separate output directory, and records the build command,
binary digest, exact candidate, provider versions and selected model metadata.
It does not install Fluent. It prepares small disposable projects for this
prerequisite check, with a private Fluent HOME and a declared fixture Tester.
Before launching Tasks, it copies the installed Claude/Pi executable or package
and its declared dependencies into that HOME's existing sandbox tool paths.
It resolves npm bin links and pnpm cmd-shims, copies the Node runtime for package
launches, and stages tmux beside the providers. Pi's interactive startup calls
Node's `spawn("tmux", ...)`; staging tmux lets that lookup finish before reaching
operator PATH entries outside the isolated HOME. Fluent's own resolved tmux path
does not control Node's child-process lookup. `provider-launches.json` records
the original tmux path, staged path, and executable digest. The driver rejects
package resource links that leave the package. It never
copies the operator's package-manager configuration or provider homes. The
staged PATH also appears in prepared SSH launch scripts. A second version check
uses that PATH and isolated HOME and retains original output and exit status;
version checks alone do not grant native acceptance. Production confinement
remains unchanged. Unsupported installation layouts fail with their source path
so the host can diagnose them without broadening filesystem access.
The complete product Tester still requires the native Codex and SSH host gates.
Real tmux dashboard tests also run through the required
`bash tests/host/check-dashboard-terminal.sh` gate, with
`FLUENT_TEST_REAL_TERMINAL_HOST=1` supplied only by an authorized host. The
ordinary nextest command and that host command use complementary selections
from `tests/fixtures/dashboard-terminal-host.filter`. No scenario is optional:
the host gate retains real input routing, observer sizing, fullscreen resize,
repaint, flood, ended-generation, and nested cancellation/retry checks. Confined
tests use the existing terminal-client seam to exercise live generations,
Control leases, PTYs, and dashboard navigation without granting worker socket
access. Those fixture passes do not establish real tmux or provider acceptance.

For Claude, answer the pending native question through Other with the token
printed by the harness and `COMPLETE`. For Pi, send the printed token through
Control, then detach; the harness releases its file condition after recording
admitted input. The driver requires actual input, activity and successful
settlement in that recorded generation. It retains terminal captures, original
native transcripts, command output, exit status and incomplete observations.
While awaiting interaction, it captures the exact Task's published tmux pane
read-only into command logs, before cleanup can remove startup errors. If the
terminal disappears before capture, Task and preflight logs remain available;
a missing pane or empty transcript cannot satisfy acceptance.
If Fluent rejects cancellation because a pane lacks its generation-specific
sandbox grant, the driver preserves that error and leaves cleanup incomplete.
An exited provider or a later "already suspended" response does not establish
settlement. Keep ownership checks intact and require fresh host evidence of
successful settlement and owned cleanup after correcting the launch.
A failed Pi response points to the original `message_end` diagnostic without
claiming success or rewriting history. Cleanup cancels only the prepared
Attempt through Fluent; interrupted local attachment processes belong to a
separate owned process group. Cleanup failure also withholds a pass.

The driver copies the selected Pi configuration only into a private temporary
directory outside retained evidence and removes it after the check. It does not
change operator configuration. `result.json` explicitly covers only the native
prerequisites; it cannot satisfy the full SSH gate or released-0.6.0 upgrade
acceptance. The host must submit the exact candidate's evidence for targeted
Tests Review before the workflow can continue.
