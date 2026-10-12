# Native agent Tasks

Select `--terminal-mode native` when creating an Attempt to run its Writer in
the provider's foreground interface. Existing Attempts and new Attempts without
this option use headless execution. Resolve the provider, model, and effort (Pi
thinking level) before launch. Fluent preserves the Attempt's selection for
later Writer Tasks.
Native Codex Review Tasks use a new ephemeral thread in the review artifact
area. Other agent roles wait for a supported native adapter.

Fluent owns the terminal, input admission, provider connection, and Task
evidence. Codex connects its TUI to a private app server through Fluent's
relay. Claude sends lifecycle hooks to a private receiver. Pi loads one owned
extension and writes its assigned session in a private agent directory. The
terminal can be attached for interaction, but its display is not the Task
evidence source.

Each invocation writes `transcript.jsonl` with versioned
`fluent-native-agent-event` records. Fluent reads those records for review,
conversation identity, and usage. Native Claude also saves
`claude-session.jsonl` for continuity and `claude-task-usage.jsonl` for usage
from the current invocation. Native Pi saves `pi-session.jsonl` for continuity.
An interrupted invocation or missing evidence fails through the Task recovery
path; the next Task cannot claim its completion.

Run `tests/host/check-native-agent-tasks.sh` only after host and provider-run
authorization. It requires resolved model and effort variables, a local Pi
endpoint, and `expect` for terminal attachment. The script stores its fixture
repos, Task logs, candidate, and provider versions under a host evidence
directory. Fluent's ordinary final Tester still runs its declared suite after
review.
