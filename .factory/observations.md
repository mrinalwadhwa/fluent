# Observations

Append-only log of things noticed during factory usage. Each one is a
potential brief. Promote to a run when ready to act on it.

---

2026-05-11 — During the interactive stages, there were loops where
the user just typed "yes, keep going" repeatedly. These indicate
steps that are potentially automatable and may not need a human in
the loop. The factory should learn from these patterns to reduce
unnecessary pauses.

2026-05-12 — The define-behaviors skill should read existing behaviors
from documentation/behaviors.md before writing new ones. This would
calibrate the level of behavioral definition (what's too detailed,
what's too abstract) and avoid duplicating behaviors that already exist.
Currently the skill only reads the brief and codebase, not the existing
behavioral contract.

2026-05-12 — Consider whether there are other interactive git operations
that could block headless agents beyond commit signing (merge conflict
resolution, gpg passphrase prompts, interactive rebase).

2026-05-12 — Factory output goes to stdout, same as the agent's
print-mode output. Piping factory run output is destructive. The
factory should write its own output to stderr or log to a file by
default so stdout is safe to pipe or discard.

2026-05-13 — On the Fargate test, round 2 reviewers all crashed
(exit 1) after round 1 had 5 reviewers + author session 2. Cause
unknown — could be rate limits, container resource exhaustion, or
something else. Needs investigation with reviewer transcripts next
time it happens.

2026-05-15 — The sandbox allows outbound network, so a malicious
package's postinstall script could exfiltrate workspace contents via
HTTP. The sandbox prevents credential theft and privilege escalation
but not data exfiltration. Options: (A) network proxy allowlisting
API endpoints only, (B) deny outbound except localhost with credential
proxy mediating all API access, (C) read-only package caches. Option
B aligns with isolation-by-impossibility principle.

2026-05-15 — Dashboard has a rendering bug: stray "A" characters
appear at the left edge of the activity feed, breaking the border
outline. Likely caused by line wrapping cutting at wrong byte
boundaries in multi-byte or styled content, or by unparsed content
from stream-json leaking into the display.

2026-05-15 — Dashboard should enable text selection and copying from
the activity feed. Currently mouse capture for scroll wheel prevents
normal terminal text selection. Consider toggling mouse capture off
with a key (e.g. 'c' for copy mode) or using a modifier (hold Shift
for native terminal selection, which some terminals support with
mouse capture enabled).

2026-05-15 — Dashboard auto-scroll should re-enable when the user
scrolls to the bottom. Currently once disabled it stays off until
the user switches agents or runs.

2026-05-15 — Dashboard field refresh is inconsistent. Reviewer status
colors don't always update, phase/status in header can be stale, run
list statuses don't refresh. All displayed fields need to be
re-evaluated on each poll cycle to reflect current state. The
dashboard should make it obvious when something changes.

2026-05-09 — The refine-writing skill at ~/Workspace/skills has
reference files (ai_tells.md, benchmarks.md, sentence_corrections.md,
structural_guidance.md) with much more detail than what was captured
into write-documentation. May want to pull more in later, especially
the sentence corrections as concrete examples.

2026-05-16 — Interactive skills (capture-brief, define-behaviors,
design-approach, plan-execution) have no test scenarios. These are
non-trivial skills that drive the planning phase. Testing them requires
designing scenario-based tests that simulate the interview flow and
verify outputs. Tracked in behaviors.md as "(needs scenarios)" but
warrants its own run.

2026-05-16 — The notification system (macOS osascript notifications
from factory watch) needs a purpose review. What value do notifications
add to the workflow? When are they useful vs noise? Should they be
richer (actionable, with run context) or replaced by something else
(dashboard focus, sound, status bar)?

2026-05-16 — sessions.log only records author session duration, not
review phase duration or wall-clock timestamps. A run that takes 2
hours of wall clock shows 56 minutes of author time because the 7
review rounds (35 reviewer sessions) aren't logged. Add ISO
timestamps and review-phase entries to sessions.log so total run
time and time-per-phase are visible.

2026-05-16 — Skills distribution strategy (confirmed with skills.sh test):

Structure changes needed:
- Add references/ directory to each skill with symlinks to expertise/
- skills.sh dereferences symlinks on install (confirmed: `dereference: true`)
- Review skills: change "Read expertise/X.md" to "Read references/X.md"
- Slim down prompts/review-*.md to minimal factory glue (output path,
  run ID, scope instruction) — point to the skill for procedure
- Audit expertise/ files — split factory-specific content into
  .factory/expertise/ or documentation/. General principles stay.
- Create complementary "create" skills for each "review" skill:
  architect, write-tests, write-documentation, write-skill
- New expertise-heavy skills (test-tui, test-web, etc.) use the
  symlink pattern for their reference material

Key decisions:
- expertise/ is distributable general-purpose content
- .factory/expertise/ is project-specific (decisions, learnings)
- Skills are self-contained after skills.sh install
- ContentResolver unchanged — agents read skills directly
- Prompts are thin factory orchestration glue, not content

2026-05-16 — The dashboard has no complete behavioral spec. Existing
behaviors cover safety (doesn't crash, doesn't modify state) and a
vague TUI description. Missing: keyboard navigation, scroll behavior,
activity feed content, line wrapping, run list refresh. Needs a
dedicated effort to write a full spec — separate from the activity
signaling work.
