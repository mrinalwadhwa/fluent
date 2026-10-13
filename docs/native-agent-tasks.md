# Native agent Tasks

Select `--terminal-mode native` when creating an Attempt to run assigned agent
Tasks in the provider's foreground interface. Existing Attempts and new Attempts
without this option use headless execution. Resolve the provider, model, and
effort (Pi thinking level) before launch. Writer Tasks retain the Attempt's
selected conversation across authorized continuations. Review, Learner, and
rebase Tasks launch new ephemeral conversations; Codex never resumes a Writer
thread for these roles. Fluent still advances each Task through its normal
review, Tester, Learner, and landing gates.

Fluent owns the terminal, input admission, provider connection, and Task
evidence. Codex connects its TUI to a private app server through Fluent's
relay. Claude sends lifecycle hooks to a private receiver. Pi loads one owned
extension and writes its assigned session in a private agent directory. The
terminal can be attached for interaction, but its display is not the Task
evidence source.

Native Claude requires Claude Code 2.1.295 or later. This version supports
`onFailure: "block"` for the managed hooks, including hook launch errors and
timeouts. Fluent checks this capability before reserving a native Task.

Native Pi requires a selected `local-openai` model in the operator's
`PI_CODING_AGENT_DIR/models.json` or `~/.pi/agent/models.json`. Set
`FLUENT_NATIVE_PI_ENDPOINT` only to replace that model's local server URL;
the selected model's capabilities and limits remain in the private Task
configuration. `FLUENT_NATIVE_PI_API_KEY` may replace its credential. Fluent
rejects a missing or incompatible selected model. The owned Pi extension
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

If Codex exits before its first turn, run
`tests/host/check-native-codex-first-turn.sh` with the authorized host flag and
resolved Codex model and effort. It stops after an assigned `turn/started` and
cancels the disposable Task through the public owner. Inspect the fixture's
`native-codex-relay-diagnostics.jsonl` for the first failing direction and
`native-codex-client-diagnostics.jsonl` for the client exit status. These files
carry the execution generation and assigned thread. The relay trace stores
protocol method names without payloads; the client record stores only known
output signals from the native pane. This focused check does not replace the
full real-provider acceptance check.

Run `tests/host/check-native-agent-tasks.sh` only after host and provider-run
authorization. It requires resolved model and effort variables, a local Pi
endpoint matching the selected `local-openai` profile, and `expect` for terminal
attachment. The script stores its fixture
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
