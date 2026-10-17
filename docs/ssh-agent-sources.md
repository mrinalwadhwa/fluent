# Attach to agents over SSH

Configure existing macOS Fluent projects in the operator's
`~/.config/fluent/dashboard-sources.json`:

```json
{
  "schema_version": 1,
  "ssh": [
    {
      "name": "build-mac",
      "host": "my-mac",
      "project_root": "/Users/operator/Workspace/project"
    }
  ]
}
```

`host` is an explicitly selected OpenSSH alias. `project_root` is the canonical
absolute path on that host. Each name must be unique. Fluent reads this file
from the operator's home, never from the project checkout. Opening `fluent
dashboard` includes these sources beside local projects and labels them with
the selected host. Each remote source refreshes independently. A failed refresh
retains its last snapshot with a stale diagnostic.

Establish access with `ssh my-mac` outside the dashboard first and verify the
host key through your usual trusted channel. Snapshot requests use batch
authentication and strict host-key checks. Remote hosts must already have a
compatible Fluent binary on their noninteractive SSH PATH and tmux installed.
The dashboard does not provision machines, install software, dispatch Attempts,
or copy provider credentials. It disables agent forwarding and other SSH
forwardings for these connections. Providers and execution ownership stay on
the remote host.

Fluent's fixed `agent-source` helper publishes protocol version 1. Snapshots are
limited to 2 MiB and contain published Work and worker metadata, without
transcripts, evidence paths, or executable commands. The receiver verifies the
configured project, source and worker identities before displaying descriptors.
Unknown protocol versions and unexpected fields fail that source. Remote
metadata cannot supply a local socket, executable, or attachment endpoint.

Select a remote agent worker and press `t` to observe, then `i` for Control.
The remote runtime revalidates the complete descriptor, including its execution
generation, and holds the same exclusive Control lease used by local clients.
Use `Ctrl-]` to return to navigation and `f` for full-screen access. Both views
share one SSH PTY connection and one remote lease. Native tmux detach
(`Ctrl-b`, then `d`) returns from full-screen. Only Control can resize the
shared worker; Observe changes only its client viewport.

The transport carries bounded protocol messages over an SSH PTY. The helper
keeps socket paths and process receipts on the remote host. It checks runtime
liveness independently of display output and closes the attachment when the
client stops responding. Closing a display never cancels or restarts a worker.

When SSH disconnects, the terminal shows `Disconnected`, releases input focus,
and discards queued local input. Press `t` to reconnect the original invocation;
it always starts in Observe and requires `i` to request Control again. If that
generation ended or changed while disconnected, Fluent reports it unavailable.
It never follows a replacement process or restarts an agent. Select the new
worker explicitly to attach to a later invocation. Workflow execution, provider
evidence, cancellation, and successful-Learner readiness remain remote-owned.
