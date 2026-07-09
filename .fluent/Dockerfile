# Project-specific Fluent runtime image for the Fluent repo.
# Extends Fluent base with the Rust toolchain so merge checks
# (cargo fmt --check, cargo test, cargo clippy) run on Fargate.
ARG FLUENT_BASE_URI
FROM ${FLUENT_BASE_URI}

USER root
RUN apk add --no-cache curl bash && \
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain stable --profile minimal && \
    /root/.cargo/bin/rustup component add rustfmt clippy
ENV PATH=/root/.cargo/bin:$PATH
USER node
