---
name: provider-readiness-crosses-reservation
description: Provider readiness is one no-inference, provider-keyed boundary shared by setup and autonomous launches, and its prepared proof must cross a durable reservation into execution
metadata:
  type: architecture
---

Provider readiness has one shared `CoderKind`-keyed boundary
(`provider_readiness::ProviderReadiness`) for configured setup and autonomous
Writer, Reviewer, Learner, and Rebase launches. Provider adapters must verify
only local command and authentication readiness: they must not start an
interactive session or request model inference. When a provider returns
structured authentication status, parse and validate the declared state rather
than treating a successful process exit as proof of authentication.

Run readiness before a phase records its durable in-progress reservation. If
readiness prepares a launch capability, such as the private Codex worker
environment, retain the complete readiness object through the reserved launch
rather than reconstructing a provider-specific subset. Rechecking after the
reservation can turn a preflight failure into an already-reserved phase and
creates divergent readiness semantics between providers.

Launch-route regressions must prove both properties: a readiness failure leaves
the phase unreserved and does not invoke the coder, while a successful reserved
run invokes the provider readiness check exactly once. See also
[[atomic-task-start-reservation]] and [[route-tests-drive-real-launch-wiring]].
