# How to write shell scripts

Guidelines for writing clear, reliable shell scripts. Drawn from
Google's Shell Style Guide, MIT SIPB safe shell practices, and
practical experience.

## When to use shell

Shell is the right tool for:
- Small utilities and wrappers
- Glue between other programs
- Scripts that primarily call other commands
- Build and deployment automation

Switch to a different language when:
- The script exceeds a few hundred lines
- You need data structures beyond arrays
- You need complex error handling or retry logic
- Performance matters (shell is slow for computation)
- You need to parse structured data (JSON, XML, CSV)

(Google Shell Style Guide)

## Safety

Start every bash script with:

```bash
set -euo pipefail
```

- `set -e` (errexit) — exit immediately when any command fails. Without
  this, the script keeps running after errors, often making things
  worse. Suppress for individual commands with `|| true` when failure
  is expected and handled.
- `set -u` (nounset) — treat unset variables as errors. Catches typos
  in variable names that would otherwise silently expand to empty
  strings.
- `set -o pipefail` — a pipeline fails if any command in it fails, not
  just the last one. Without this, `broken_cmd | grep foo` succeeds if
  grep succeeds, hiding the failure of broken_cmd.

For POSIX sh (not bash), use `set -eu` — pipefail isn't available.

(MIT SIPB "Writing Safe Shell Scripts")

### Debugging

Enable trace mode through an environment variable rather than
hardcoding it:

```bash
if [[ "${TRACE-0}" == "1" ]]; then set -o xtrace; fi
```

Users can then debug with `TRACE=1 ./script.sh` without modifying
the script.

## Quoting

**Always quote variables.** This is the single most common shell bug.
Unquoted variables undergo word splitting and glob expansion:

```sh
# Wrong — breaks if path has spaces or glob characters
cp $file $dest
rm $temp_dir/*

# Right
cp "$file" "$dest"
rm "${temp_dir:?}"/*
```

**Use `"${var}"` when adjacent to other text:**

```sh
echo "${prefix}_suffix"
```

**Use `"$@"` to pass all arguments.** Never use unquoted `$@` or
`$*`:

```sh
wrapped_command "$@"
```

**Use `--` to prevent argument injection.** User-supplied arguments
that start with `-` can be misinterpreted as flags:

```sh
# Unsafe — if "$file" is "-rf /", this is dangerous
rm "$file"

# Safer
rm -- "$file"
```

(MIT SIPB, Google Shell Style Guide)

## Variables

**Declare constants with readonly:**

```sh
readonly MAX_RETRIES=3
readonly CONFIG_DIR="${HOME}/.config/myapp"
```

**Use local in functions** to avoid polluting global scope:

```sh
my_function() {
  local result
  result="$(some_command)"
  printf '%s\n' "$result"
}
```

**Use `${VARNAME-}` for optional variables** when nounset is enabled.
This provides an empty default without triggering the unset variable
error:

```sh
OPTIONAL_FLAG="${1-}"
```

## Naming

| Thing | Convention | Example |
|---|---|---|
| Functions | lowercase_underscores | `setup_sandbox` |
| Variables | lowercase_underscores | `run_id` |
| Constants | UPPERCASE_UNDERSCORES | `MAX_RETRIES` |
| File names | lowercase-hyphens | `test-run` |

(Google Shell Style Guide)

## Functions

**Organize code into functions.** A script should have a clear
structure: function definitions at the top, execution logic at the
bottom. Don't scatter executable statements between function
definitions.

**Use a main function** when the script has significant logic:

```sh
main() {
  parse_args "$@"
  setup
  run
}

main "$@"
```

This makes the script's entry point explicit and keeps top-level
code minimal.

**Each function should do one thing.** If the function name needs
"and" in it, split it.

**Document functions** that aren't self-explanatory:

```sh
# Resolve the work item ID from flag or env var.
# Sets WORK_ITEM_ID and WORK_DIR.
resolve_work_item_id() {
```

List what globals the function reads and sets if it's not obvious
from the name.

## Error handling

**Send errors to stderr:**

```sh
die() {
  printf 'error: %s\n' "$1" >&2
  exit 1
}
```

**Check for required tools early:**

```sh
command -v aws >/dev/null 2>&1 || die "aws CLI not found"
command -v docker >/dev/null 2>&1 || die "docker not found"
```

**Use traps for cleanup:**

```sh
cleanup() {
  rm -f "$TEMP_FILE"
  kill "$BG_PID" 2>/dev/null || true
}
trap cleanup EXIT INT TERM
```

The EXIT trap runs on normal exit and errors (with set -e). INT and
TERM handle Ctrl-C and kill signals.

**Use `|| true` deliberately.** When you write `command || true`,
you're saying "I expect this might fail and I intentionally don't
care." Make sure that's actually true. Don't use it to silence
errors you should be handling.

## Conditionals

**Prefer `[[ ]]` over `[ ]`** in bash. It handles quoting better,
supports pattern matching (`[[ $x == *.txt ]]`), and doesn't do
word splitting:

```bash
if [[ -z "$var" ]]; then
```

**Use `(( ))` for arithmetic** — cleaner than test-based comparison:

```bash
if (( count > max )); then
```

**In POSIX sh, use `[ ]`** with careful quoting:

```sh
if [ -z "$var" ]; then
if [ "$count" -gt "$max" ]; then
```

## Command substitution and output

**Use `$()` not backticks.** `$()` nests properly and is easier to
read:

```sh
result="$(some_command)"
```

**Use printf over echo.** `printf` is more portable and handles
special characters predictably:

```sh
printf 'Processing %s...\n' "$file"
printf 'error: %s not found\n' "$file" >&2
```

**Prefer builtins over external commands** when the builtin is
equivalent. Parameter expansion, `[[ ]]`, and `(( ))` are faster
than spawning `expr`, `test`, or `grep` for simple checks.

## Portability

Use `#!/usr/bin/env sh` for portability.
When writing for POSIX sh:

- No `[[ ]]` — use `[ ]` with careful quoting
- No `(( ))` — use `[ "$x" -gt "$y" ]`
- No arrays
- No `set -o pipefail`
- No process substitution `<()`
- Functions: `func_name() {` without `function` keyword
- No `local` in strict POSIX (most implementations support it, but
  it's technically not specified)

When a script needs bash features, use `#!/usr/bin/env bash`
explicitly.

## Anti-patterns

**Parsing ls output.** Use glob patterns or find instead. `ls` output
is unreliable with special characters in filenames.

**Unquoted variables in conditionals.** `[ -z $var ]` breaks when
var is empty or contains spaces. Use `[ -z "$var" ]`.

**cd without error checking.** Always handle failure:
`cd "$dir" || die "cannot cd to $dir"`.

**Long pipelines without intermediate variables.** When a pipeline
is hard to read, break it into named steps. Readability matters more
than conciseness.

**Magic numbers.** Use named constants:
`readonly TIMEOUT=300` not `sleep 300`.

**Swallowing errors with 2>/dev/null.** Only suppress output you've
deliberately decided to ignore. Every suppressed error is a clue
you won't see when debugging.

**Using eval.** Almost never necessary and creates injection risks.
Find another way.

**Not using ShellCheck.** ShellCheck catches most of the issues above
automatically. Run it on every script.
