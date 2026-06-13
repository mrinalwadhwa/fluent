2026-06-13 — Parallel Factory Attempt launches race on
.git/config.lock. Two concurrent `factory work attempt run`
invocations (or any pair of Factory operations that mutate
.git/config simultaneously) can produce:

  Error: git config commit.gpgsign false failed (exit 255) while
  disable commit signing
  stderr: error: could not lock config file
  /Users/mrinal/Workspace/factory/main/.git/config: File exists

The losing process exits immediately. Concrete incident: launched
keep-awake-toggle and claude-auth-detection Attempts back-to-back
(< 1 second apart); the second one died with the lock error.

The git config write happens during workspace setup in Factory's
git-wrapper. The fix is straightforward: detect the "File exists"
lock error class, sleep briefly (10–50ms with jitter), and retry
up to a small bound. This mirrors how git itself handles the lock
elsewhere.

Out of scope for the immediate need but worth a small Work Item.
Adjacent: any other shell-out to git from Factory could race on
the same lock; the wrapper should retry uniformly.
