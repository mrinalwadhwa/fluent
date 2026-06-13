2026-05-16 — The notification system (macOS osascript notifications
from factory watch) needs a purpose review. What value do notifications
add to the workflow? When are they useful vs noise? Should they be
richer (actionable, with run context) or replaced by something else
(dashboard focus, sound, status bar)?

→ Resolved: Resolved by Work Item notify-strip-osascript. macOS Notification Center retained osascript-originated notifications with the body replaced by a generic 'Notification' placeholder, so the body content was invisible to users. notify() now logs to stderr uniformly; call sites are preserved so a future general notification system (Discord/Slack/push) can replace the implementation in one place.
