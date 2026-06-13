2026-06-11 — Add a Rust `factory fargate teardown` command that
replaces `infrastructure/teardown.sh`, the same way JIT bootstrap
replaced `infrastructure/setup.sh`. Two different workflows for
parallel concerns (setup vs teardown) is unnecessary surface; both
should live behind the binary. The teardown command should: remove
the CloudFormation stack, optionally clean ECR images and the S3
bucket, and clear `~/.config/factory/fargate.state.json` so the
next `--runtime fargate` invocation bootstraps fresh.
→ Resolved: `factory fargate teardown [--keep-ecr] [--keep-s3]`
implemented in `src/fargate_bootstrap.rs::teardown()` with CLI
dispatch from `src/main.rs`. `infrastructure/teardown.sh` deleted.
Behaviors, architecture docs, and tests updated.
