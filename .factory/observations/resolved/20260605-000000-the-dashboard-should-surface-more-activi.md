2026-06-05 — The dashboard should surface more activity beyond the
header phase label and active agent tabs. Add active run indicators
in the run tabs (spinner next to status), sort active runs and agents
first in their respective lists, and consider a global activity
indicator in the dashboard title bar when any run is active. The
dashboard should feel alive when work is happening and completely
still when everything is done.
→ Resolved: fff24a9, 145d75d, and follow-up dashboard title work. The
dashboard now renders faster, shows active run markers in run tabs,
keeps actionable runs sorted ahead of terminal runs during polling, and
shows a dashboard-title activity spinner when any run is active. Agent
tabs already show running status; active-agent reordering was left out to
preserve stable author/report/reviewer tab positions.
