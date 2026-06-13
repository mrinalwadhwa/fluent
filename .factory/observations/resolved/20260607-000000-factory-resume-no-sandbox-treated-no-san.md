2026-06-07 — `factory resume --no-sandbox ...` treated `--no-sandbox` as
an extra agent argument because `resume` only read the top-level
`factory --no-sandbox resume ...` flag. Recovery commands did not work as
expected.
→ Resolved: `Resume` clap variant now accepts `--no-sandbox` and
`--coder` as local flags, matching `Run`. Dispatch combines local and
top-level forms with local taking precedence. Tests cover local flags,
global flags, precedence, help output, and no-leak into extra args.
