2026-06-05 — The dashboard never removes runs that were deleted from
disk. App::poll discovers new runs but never prunes stale ones, leaving
removed runs in the list with "[-]" status.
→ Resolved: 1fc4b8c (dashboard polling removes deleted source runs and
selects an existing run or the empty state)
