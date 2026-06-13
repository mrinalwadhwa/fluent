2026-06-13 — macOS clamshell sleep (laptop lid closed) pauses
Factory background processes silently. The user's monitor processes,
detached factory work attempt run children, and post-merge-review
debounce timers all freeze when the lid closes, and resume from
where they froze when the lid opens — but the wall-clock gap is
charged against the Work Item's lifecycle as if nothing happened.

Concrete incident: behavior-tests-task Attempt-1 wall-clock measured
~7 hours (00:14:43 → 07:12:14), but actual agent work was ~40
minutes. The remaining 6+ hours was the laptop being asleep
overnight while a Merge Candidate sat ready to merge.

Considerations:

- caffeinate -i (CLI tool) prevents idle sleep for the duration of
  a wrapped command. Factory could wrap long-running Tasks in
  caffeinate when running on macOS local runtime.
- Per-process power assertions via IOKit are more granular but
  add a macOS-specific code path.
- The right answer may not be "prevent sleep" — letting the laptop
  sleep when work is paused is a feature, not a bug. The real fix
  is the merge-auto-trigger work plus a way for Factory to "wake
  back up" reliably on lid-open without dropping detached work.
- Fargate runtime sidesteps this entirely — ECS tasks run in the
  cloud, no local power management to fight.
