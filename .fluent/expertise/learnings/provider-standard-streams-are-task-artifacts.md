---
name: provider-standard-streams-are-task-artifacts
description: Captured providers receive task-owned standard streams so scheduler log placement cannot break sandboxed runtime startup
metadata:
  type: architecture
---

A sandboxed provider must not inherit Fluent's scheduler stderr when that
descriptor may point outside the provider's readable roots. Claude Code 2.1.220
uses Bun to inspect its standard stream descriptors during startup; an inherited
scheduler log outside the Seatbelt boundary produces an `fstat` denial before a
transcript or model work exists.

For transcript-bearing launches, pre-open a provider stderr file beside the
canonical transcript and pass that descriptor to the child. Treat failure to
create the diagnostic as transcript infrastructure failure, because relaunching
after an uncertain capture boundary can repeat side effects. Optional autonomous
calls with no transcript should receive stable device streams rather than
operator or scheduler descriptors.

The sandbox must also grant read access to the exact host-owned artifact
directory containing that descriptor. This is especially important for the
Learner: its writable `staging` directory is a sibling of the host-owned run
evidence, so granting only the staging path still makes Bun's descriptor `fstat`
fail. Keep the run directory readable but not writable; do not grant the common
Learner artifact parent or fold host evidence into the writable staging tree.

Keep the startup classifier narrow: require the Bun `fstat` denial together with
standard-stream construction evidence. An unrelated workspace permission error
is not a host-sandbox launch failure. The typed launch failure bypasses generic
coder retries and resumes the same Task without charging a Writer round.

Native Codex can retry execution generations within one Task artifact directory.
Archive each invocation's provider stderr with exclusive creation at a distinct
available path so a later failure or success preserves earlier diagnostics.
Keep each archive byte-bounded, and retain the primary provider error when
adding archive context. A later successful generation must not fail merely
because an earlier generation already published an archive. Test repeated
failures followed by success and verify the retained contents of earlier files.
