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

---

Resolved 2026-06-12 — Implemented per-project Docker images. Factory
now publishes a thin base image tagged `factory-base-<version>` and
each project extends it with `.factory/Dockerfile`. This repo ships
a `.factory/Dockerfile` that installs the Rust toolchain via rustup
so `cargo fmt --check`, `cargo test`, and `cargo clippy` run on
Fargate. The tag scheme uses SHA-256 content hashing for
cache-friendly project image tags (`project-<sha256-prefix>`).
ECR skip-if-exists checks prevent redundant builds. Task definition
revisions update automatically when the project image changes.
