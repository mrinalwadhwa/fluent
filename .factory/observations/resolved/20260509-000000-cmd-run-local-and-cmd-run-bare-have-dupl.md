2026-05-09 — cmd_run_local and cmd_run_bare have duplicated session loop
logic (snapshot capture, status checking, review phase). The differences
are small (sandbox + credential refresh vs --dangerously-skip-permissions).
Extract the loop body into a shared function.
→ Resolved: 99c252e (deduplicated into run_session_loop)
