# fluent

Fluent is an autonomous, self-improving, software factory.

fluent runs on macOS (Apple Silicon and Intel).

## Install

### From the binary

```sh
curl fluent.computer/install | sh
fluent skills add
```

The first command installs the `fluent` binary to `~/.local/bin`. The second
installs the fluent skill into your coding agent so you can invoke `/fluent`.

### From the skill registry

```sh
npx skills add mrinalwadhwa/fluent --skill fluent
```

This installs a bootstrap shim. On first run, `/fluent` installs the binary
if it is not already present, then materializes the full skill from the binary
and continues.

## Use it with your coding agent

Start the workflow from your agent (such as Claude Code):

```
/fluent
```

## In a project

Run these from inside your project's git repository.

```sh
fluent init      # set up fluent's working state and install the skill
/fluent          # start building
```

fluent creates its work in sibling git worktrees next to your repo, so your
working tree stays clean while it builds.

## Staying up to date

```sh
fluent update
```

Downloads and installs the latest release, and refreshes the skills.
