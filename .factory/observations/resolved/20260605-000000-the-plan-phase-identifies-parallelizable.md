2026-06-05 — The plan phase identifies parallelizable steps but the
factory has no mechanism to execute them in parallel. The factory should
support decomposing a plan into parallel child runs, launch them
simultaneously, and gate later work on completion.
→ Resolved: e49d797, 9d62538, 2014fff, 992930e (structured parallel
plans create child runs, launch parallel groups, gate sequential groups,
and land completed child branches)
