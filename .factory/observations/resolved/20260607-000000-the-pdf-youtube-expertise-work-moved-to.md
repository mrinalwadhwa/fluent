2026-06-07 — The PDF/YouTube expertise work moved to a separate
conversation thread because Factory does not yet let the coordinating
agent trigger several independent peer runs in parallel from one
planning conversation. That is a workflow smell: separate chat threads
are being used as a substitute for a first-class independent run queue.

→ Resolved: Workflow gap closed: Factory now supports multiple independent peer Work Items from one planning conversation. Demonstrated 2026-06-12 when fargate-image-rust-toolchain, git-non-interactive-defaults, and rate-limit-ux ran concurrently and landed at b75939f, f8b2df6, cf48f94 respectively.
