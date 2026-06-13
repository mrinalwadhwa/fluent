2026-06-07 — Factory scheduling should use Codex `/status` as the live
subscription-capacity signal when scheduling Codex-backed runs. The
current Codex `/status` view exposes exactly the live fields Factory
needs: active model, account/plan, context-window remaining, 5-hour
limit remaining with reset time, weekly limit remaining with reset time,
GPT-5.3-Codex-Spark-specific limits, and a stale-warning signal. This is
different from the documented Codex Enterprise Analytics API: the
analytics API is useful for historical, delayed, workspace-level usage
and calibration, but it is not the right live scheduler signal for a
personal or Pro-style 5-hour/weekly subscription window. Factory should
add a Codex usage probe abstraction that tries to obtain `/status`
non-interactively, parses the usage fields, and stores a snapshot such as
`.factory/usage/codex-status.json` for the scheduler and dashboard. If
Codex exposes `/status` through `codex exec --json` or another supported
programmatic surface, use that. If it is TUI-only, evaluate a small
PTY-based probe or manual status import; avoid scraping the web
dashboard. Factory should also maintain its own local usage ledger from
Codex JSON `turn.completed.usage` events as fallback and calibration.
Scheduling should combine live `/status` remaining/reset data with
run-cost estimates so the run queue can burst when the 5-hour window is
healthy, preserve weekly budget when pacing is low, and switch to
planning/curation/reporting work when Codex capacity is scarce.
