2026-06-11 — Fargate container is missing the toolchains the
configured Factory merge checks invoke. The smoke test merge ran
`factory work merge` end to end on Fargate; rebase succeeded but
the `format` check failed with `cargo: not found` because the
chainguard/node base image only ships node, git, bash, aws-cli, jq,
tmux, and curl. Two reasonable directions: (a) extend the
Dockerfile to install the Rust toolchain so this repo's `cargo fmt`
and `cargo test` checks run in the container, or (b) make
project-specific check tooling pluggable so the Fargate image
ships a thin runtime and each project provides its own check
container or sidecar. Option (b) generalizes better to non-Rust
Factory-managed projects.
