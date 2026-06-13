2026-06-05 — Network policy is a separate sandbox design axis from
filesystem roots. Local Seatbelt currently allows outbound network, but
stricter modes or Codex's internal sandbox may deny or constrain network
access. That can break dependency workflows such as package install,
registry metadata lookup, crate/npm/pip downloads, and tool/model
bootstrap. Explore whether Factory should support project-configurable
network policy, dependency-cache writable/read-only mounts, allowlisted
install phases, or explicit dependency setup runs so agents can build
projects without silently weakening credential and filesystem isolation.
