FROM rust:alpine AS builder

RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY build.rs ./
COPY prompts ./prompts
COPY scripts/assets ./scripts/assets
COPY src ./src
RUN cargo build --release

FROM cgr.dev/chainguard/node:latest-dev

USER root
RUN apk add --no-cache git bash aws-cli jq tmux curl
RUN npm install -g @anthropic-ai/claude-code

COPY --from=builder /src/target/release/factory /usr/local/bin/factory
COPY infrastructure/run/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

USER node
WORKDIR /workspace
ENTRYPOINT ["/entrypoint.sh"]
