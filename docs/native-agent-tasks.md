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

Run `tests/host/check-native-agent-tasks.sh` only after host and provider-run
authorization. It requires resolved model and effort variables, a local Pi
endpoint matching the selected `local-openai` profile, and `expect` for terminal
attachment. The script stores its fixture
repos, Task logs, candidate, and provider versions under a host evidence
directory. Review the candidate-bound host results through the targeted Tests
Review and remaining domain reviews. The final Tester then runs the complete
declared suite, followed by the normal Learner and landing gates.
The host check first exercises installed Pi session compaction and its queued
follow-up lifecycle against Fluent's event gate, with model responses supplied
by the fixture. This component check runs without a provider connection.
Its real-provider fixture uses a disposable local proxy to return one
retryable Pi response before forwarding to the selected server. It separately
checks that `/compact` during active work aborts that activity, and that
receiver loss fails the Task without sending rejected input to the model.
Codex, Claude, and Pi each run a confined Writer
and native Review with attached input, a settling-boundary input attempt,
detachment, evidence inspection, and cancellation. A disposable Codex Attempt
also holds an observer through automatic Review progression, then checks its
native Learner and a conflict-driven native Rebase. The real-provider check
must pass on the exact candidate before its targeted Tests Review can accept
host evidence. The provider-backed host cases and the final Tester are separate
acceptance gates.
