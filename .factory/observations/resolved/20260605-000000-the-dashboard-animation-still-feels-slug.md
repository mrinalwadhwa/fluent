2026-06-05 — The dashboard animation still feels sluggish despite
the 100ms render interval. The spinner needs to cycle faster to
feel responsive — consider 50-80ms or a different animation style
that communicates activity more clearly at lower frame rates.
→ Resolved: fff24a9 (dashboard render cadence now uses a 75ms interval
and the behavior documentation reflects the faster animation target)
