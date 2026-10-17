# Test terminal transport with confined clients

## Context

A Writer sandbox may allow PTYs and managed subprocesses while denying Unix
socket binds. A real tmux server cannot start there. Transport tests still need
to prove generation checks, Control contention, disconnect cleanup and input
routing without treating missing host capabilities as a passing skip.

## Mechanism

Create a real `TerminalSession::start_fixture` owner and use its descriptor.
Replace only the external terminal client through the test-support-only
`FLUENT_TEST_TERMINAL_CLIENT` seam, which applies exclusively to fixtures without
a tmux socket. Set the environment on the test child, not process-global state.
The production attachment still resolves the descriptor, validates the live
generation, acquires the actual lease, creates a PTY and inherits the lease into
its child. The stand-in enters raw mode and records received bytes. Drive
full-screen handoff from a controlling PTY and inspect lease contention before
and after return.

Keep real SSH, tmux and provider acceptance in the required host check. Record
that boundary explicitly; the stand-in proves transport and runtime admission,
not a provider's native interface or host confinement.

## Example

`tests/ssh_agent_sources.rs` executes the production remote helper through an
OpenSSH stand-in. `tests/fixtures/ssh-terminal-client.py` replaces the external
tmux client while `ssh-terminal-handoff.py` drives full-screen detach through a
controlling PTY. The host journey remains separately authorized in
`tests/host/check-ssh-agent-terminal.sh`.
