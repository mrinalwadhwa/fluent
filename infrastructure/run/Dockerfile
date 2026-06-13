FROM rust:alpine AS builder

RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY build.rs ./
COPY prompts ./prompts
COPY sandboxes ./sandboxes
COPY src ./src
RUN cargo build --release

FROM cgr.dev/chainguard/node:latest-dev

USER root
RUN apk add --no-cache git bash aws-cli jq tmux curl
RUN npm install -g @anthropic-ai/claude-code @openai/codex

COPY --from=builder /src/target/release/factory /usr/local/bin/factory
COPY infrastructure/run/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

# Pre-create the worktrees parent owned by node so the entrypoint
# (which runs unprivileged) can populate it from the S3 tarball.
RUN mkdir -p /worktrees && chown -R node:node /worktrees

USER node
WORKDIR /worktrees
ENTRYPOINT ["/entrypoint.sh"]
