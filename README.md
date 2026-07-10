# fluent

Fluent is an autonomous, self-improving, software factory.

fluent runs on macOS (Apple Silicon and Intel).

## Install

```sh
curl fluent.computer/install | sh
```

This installs the `fluent` binary to `~/.local/bin`.

## Use it with your coding agent

fluent drives your existing coding agent (such as Claude Code). Add its skill:

```sh
npx skills add mrinalwadhwa/fluent
```

Then start the workflow from your agent:

```
/fluent
```

On first run, `/fluent` installs the `fluent` binary if it is not already present,
then takes it from there — capturing intent, planning with you, and executing.

If you prefer, install the binary directly first with the command in Install
above; `/fluent` will use it.

## In a project

Run these from inside your project's git repository.

```sh
fluent init      # set up fluent's working state in the repo
/fluent          # start building
```

fluent creates its work in sibling git worktrees next to your repo, so your
working tree stays clean while it builds.

## Staying up to date

```sh
fluent update
```

Downloads and installs the latest release, and refreshes the skills.
