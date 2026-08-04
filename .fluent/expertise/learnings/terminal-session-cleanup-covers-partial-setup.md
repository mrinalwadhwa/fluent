---
name: terminal-session-cleanup-covers-partial-setup
description: Arm terminal cleanup before enabling raw mode or alternate-screen features so every setup failure restores prior terminal state
metadata:
  type: gotcha
---

A terminal session that enables raw mode, the alternate screen, or mouse capture
must create and arm its cleanup guard before beginning setup. Record each feature
as it becomes active and make cleanup idempotently restore only the features that
were enabled.

Do not rely on the completed session object's destructor alone: construction can
fail after one or more terminal changes have succeeded. The partially initialized
guard must therefore unwind those changes while preserving the setup error. This
keeps an ordinary `Terminal::new` or terminal-command failure from leaving the
operator in raw mode or an alternate screen.
