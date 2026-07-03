Rebase outcome check trusts exit_code over give-up.md, but Coder LLMs can't reliably control exit codes.

In src/work_merge_executor.rs line 757, the outcome classification is:
  if exit_code == 0 { Success(new_tip = HEAD) }
  else if give-up.md exists { NeedsUser(diagnostic) }
  else { Failed }

The bug: Coder LLMs (Claude Code, Codex, Pi CLI) exit 0 when the model finishes responding normally, regardless of whether the task actually succeeded. If the Coder writes give-up.md and stops working normally, the CLI likely exits 0 — and Factory treats it as Success, taking the aborted HEAD as new_tip. Since git rebase --abort restored the pre-rebase HEAD, the merge candidate proceeds with the un-rebased tip, which will then fail check-pre-merge or the actual merge in confusing ways.

Fix: check give-up.md first. If it exists, treat as NeedsUser regardless of exit code. Only if it doesn't exist should exit_code matter. Something like:

  if give_up_path.exists() { NeedsUser(diagnostic) }
  else if exit_code == 0 { Success(new_tip = HEAD) }
  else { Failed }

Related: this also removes the requirement (in the rebase prompt) that the Coder "exit with a non-zero exit code" — an instruction the LLM cannot reliably follow. The prompt can then describe what the Coder CAN do: write give-up.md + abort.

Downstream check: the exit_code == 0 success path should also verify the workspace is genuinely rebased (e.g., HEAD is not the pre-rebase commit). Today it trusts that a zero exit means the rebase completed, which combined with the bug above means an aborted rebase looks like a successful no-op rebase.
