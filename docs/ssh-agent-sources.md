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

Fluent's fixed `agent-source` helper uses snapshot protocol version 1 and
terminal protocol version 2. Install compatible Fluent versions on both ends
before attaching. Snapshots are
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

If a Control request acquires the lease but cannot start its terminal client,
Fluent releases that newly acquired lease and reports the setup error. Reopening
a client preserves an existing lease; switching to Observe releases it even
when the new display client fails. Failure diagnostics retain the request
identity and reported Control state. Startup timeouts include retained helper
errors, elapsed time and PTY setup details when available.

The transport carries bounded protocol messages over an SSH PTY. The helper
keeps socket paths and process receipts on the remote host. It checks runtime
liveness independently of display output and closes the attachment when the
client stops responding. Full-screen output uses nonblocking partial writes and
retains at most 128 KiB of unconsumed display bytes across the connection. The
helper waits for display credit before reading more from its tmux client while
it continues heartbeats, input validation, and invocation-liveness checks. A
slow local reader therefore pauses display delivery without dropping terminal
bytes or granting Observe input or resize authority. Closing a display never
cancels or restarts a worker.

After leaving full-screen, Fluent allows up to ten seconds to restore the local
display. If output stays blocked, it restores terminal input modes, releases the
SSH attachment and closes the dashboard without repainting. It discards queued
full-screen paint and attempts a nonblocking display reset; an unread terminal
cannot confirm that the reset appeared. The remote worker keeps running unless
its owner has already completed or canceled it. Reopen the dashboard after the
terminal accepts output again.

When SSH disconnects, the terminal shows `Disconnected`, releases input focus,
and discards queued local input. Press `t` to reconnect the original invocation;
it always starts in Observe and requires `i` to request Control again. If that
generation ended or changed while disconnected, Fluent reports it unavailable.
It never follows a replacement process or restarts an agent. Select the new
worker explicitly to attach to a later invocation. Workflow execution, provider
evidence, cancellation, and successful-Learner readiness remain remote-owned.

Authentication, unknown host keys, missing helpers, incompatible protocols and
missing tmux produce a diagnostic for the configured source. Establish access
outside the dashboard, install prerequisites through your existing host
administration process, and check the noninteractive SSH PATH. A malformed
configuration file reports a configuration error while local projects remain
available. Remote metadata must use plain text without terminal control
characters. The helper does not perform Fluent update checks or write an
update cache.

Run `bash tests/host/check-ssh-agent-terminal.sh` only with separate host/access
and provider-run authorization. Supply the environment variables listed at the
script's start: a production candidate binary, a local companion project, a
new external evidence directory, and configured disposable remote source names
for Codex, Claude and Pi. The existing remote fixture checkouts and installed
Fluent binaries must identify the exact candidate. Retain their build provenance
with the resulting binary hashes. The script does not enable SSH, change host
keys, provision hosts, install software, or create production Work.

The operator completes each provider's embedded and full-screen interaction,
Observe/Control contention, disconnect/reconnect, completion and cancellation
journey while local projects remain usable. Record concrete action/result
observations and companion remote captures or evidence references for each
scenario. Missing providers or unfinished scenarios fail the check and remain
pending prerequisites. Candidate, command, binary hashes, versions, snapshots,
terminal captures and observations remain in the external evidence directory.
Targeted Tests Review must inspect that evidence before this required host row
can pass. A passing script exit alone does not establish acceptance.

The focused Rust tests use real generation records, ownership, leases and PTYs.
A test-support-only external client stand-in lets them run in Writer sandboxes
that prohibit Unix socket binding. It applies only to fixture sessions without
a tmux socket; production builds and managed tmux sessions ignore it. These
checks do not substitute for the required real SSH/tmux/provider journey.

For reproducible localhost fixtures, the authorized host can run:

```bash
python3 -I tests/fixtures/native-acceptance.py --prepare-ssh
```

Use the prerequisite environment described in [native agent Tasks](native-agent-tasks.md#diagnose-acceptance-prerequisites),
plus explicit Codex model/effort and `FLUENT_SSH_CODEX_HOST`,
`FLUENT_SSH_CLAUDE_HOST`, `FLUENT_SSH_PI_HOST`, and `FLUENT_SSH_DENIED_HOST`.
Each value names an existing localhost OpenSSH alias. The denied alias supplies
the required source-failure scenario. Keep host identity pinned and forwarding
disabled; this command neither enables SSH nor changes its configuration.

Preparation builds a production candidate binary, clones its exact commit for
each provider, and creates separate interaction and cancellation Work Items.
The clones retain the canonical Tester commands. `prepared.json` records paths
and a pending result. The generated `start-<provider>.sh` and
`start-<provider>-cancel.sh` run only their named Writer Tasks; corresponding
`cancel-*.sh` scripts settle those Attempts. Run them from the authorized host
as needed for the canonical scenarios. They use private Fluent configuration,
retain command output, and refuse to overwrite a previous log. To release a
Pi or Codex wait, create the exact `operator-release` file named in that Work
Item's instructions. Claude waits for an answer through its native question.

The prepared `pi-config/models.json` strips credential fields and retains the
selected model compatibility settings. Authenticated local endpoints require
`FLUENT_NATIVE_PI_API_KEY` in the launching host shell. No operator credential
or SSH key is copied into the fixtures. Configure the existing localhost SSH
service's noninteractive PATH to resolve the prepared `bin/fluent` before the
canonical check; retain that service's setup and cleanup evidence. Do not
replace the installed Fluent. The host remains responsible for settling its
separately authorized SSH service, if it starts one.

Set the canonical check's HOME to the prepared `dashboard-home`, its local
binary to the prepared production binary, and its source names to
`native-codex`, `native-claude` and `native-pi`. Keep the canonical output in a
new directory. The driver creates a further private dashboard HOME containing
only those explicit SSH sources, so the operator's registry cannot enlarge the
exercise. OpenSSH still uses the host's existing authentication and pinned host
configuration. Every original Observe, Control, full-screen, reconnect,
completion, cancellation and failure-return scenario remains required.

The separate controlled-reader host check is
`bash tests/host/check-ssh-fullscreen-backpressure.sh`. Set
`FLUENT_SSH_BACKPRESSURE_HOST=1` after host authorization and supply a new external
`FLUENT_SSH_BACKPRESSURE_OUTPUT` directory. It builds the exact clean candidate,
records binary hashes and PTY captures, and stalls Observe and Control readers
for nine seconds. It checks bounded output, byte-preserving recovery, continued
transport activity, detach, completion, cancellation, connection failure and
terminal restoration. Its SSH and tmux clients are hermetic fixtures; the real
SSH/native-provider journey above remains separately required. Missing host
execution or provider evidence leaves acceptance pending.
