---
name: shell-test-pipelines-fail-under-pipefail
description: Avoid false failures under pipefail by matching captured strings in Bash or consuming streamed command output completely
metadata:
  type: gotcha
---

The shell scripts under `tests/behaviors/` run with `set -euo pipefail`. Inside
that mode any pipeline whose consumer exits early can fail even when the logic
is correct: `printf '%s' "$1" | grep -Fq -- "$2"` short-circuits `grep` on the
first match, `printf` then dies with `SIGPIPE` (exit 141), and `pipefail`
propagates that non-zero `PIPESTATUS` as the pipeline's status. Against large
multi-line output the matcher exits on an early line and the producer is still
writing, so a *present* literal yields a false negative (`PIPESTATUS 141 0`).

Prefer in-process Bash matching over a producer-consumer pipeline for
containment checks: `[[ "$1" == *"$2"* ]]` (and `!=` for the inverse). Quote the
needle (`"$2"`) so pattern metacharacters like `*` and `[` stay literal, and
leave the surrounding `*` unquoted so they remain the wildcards that span the
haystack. No pipeline means nothing to receive `SIGPIPE`.

For streamed command output, keep a consumer that reads the complete stream.
The smoke harness checks Git commit subjects with
`git log --format=%s | grep -Fx -- "$subject" > /dev/null`: omitting `-q`
lets Git finish writing while `-F` and `-x` preserve literal whole-line matching.
Do not suppress the pipeline status, which must still expose a failed producer
or missing match. Exercise this form with an early matching subject followed
by history larger than a pipe buffer; a short log can conceal the failure.

When regression-testing this class of fix, make it mutation-sensitive: build a
deterministic haystack larger than a pipe buffer (>1 MiB via a doubling loop)
with the literal on an early line to reproduce the SIGPIPE timing, and add a
metacharacter case (needle `a*b` against `aZZb`) so unquoting the needle
visibly reverses the outcome. Suppress the helper's own `FAIL:`/`Output:`
diagnostic (`> /dev/null 2>&1`) on the sub-checks you expect to fail so the
multi-megabyte value is not dumped.

Related: [[shell-tests-invisible-to-compiler]]
