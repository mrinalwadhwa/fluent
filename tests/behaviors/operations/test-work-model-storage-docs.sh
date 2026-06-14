#!/usr/bin/env bash
# test-work-model-storage-docs - Verify work model storage documentation.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
ARCH="$ROOT/documentation/architecture.md"

section="$(
  awk '
    /^## Core work model$/ { found=1; next }
    /^## / && found { exit }
    found { print }
  ' "$ARCH"
)"

if [ -z "$section" ]; then
  echo "architecture documentation has no Core work model section" >&2
  exit 1
fi

flat_section="$(printf '%s\n' "$section" | tr '\n' ' ' | tr -s ' ')"

for phrase in \
  'Durable work model state lives under `.factory/work/`' \
  'Managed candidate worktrees do not live under `.factory/work/`' \
  'sibling directories beside the source checkout' \
  '../work-6-work-1-attempt-1' \
  'include a Work Item ID byte-length prefix, Work Item ID, and Attempt ID' \
  '.factory/work/ items/ <work-item-id>.json' \
  'attempts/ <work-item-id>/ <attempt-id>.json' \
  'tasks/ <work-item-id>/ <attempt-id>/ <task-id>.json' \
  'merge-candidates/ <work-item-id>/ <merge-candidate-id>.json' \
  'stores Work Item metadata and planning context' \
  '`WorkModelStore` assembles those split records into the public `WorkItem` shape' \
  'If an item file contains nested Attempts, Tasks, or Merge Candidates' \
  'Factory ignores those nested operational collections' \
  'Factory writes Work Item metadata to `items/` and operational records to the split Attempt, Task, and Merge Candidate collections' \
  'Workspace references stay inside task `workspace_access.reads` and `workspace_access.writes`' \
  'managed sibling worktrees for candidate execution' \
  'does not keep a standalone workspace registry' \
  'Merge Candidates use the public `MergeCandidate` shape and have their own candidate collection' \
  'must parse records into the public Rust model and validate every assembled Attempt, Task, and Merge Candidate' \
  'The `WorkItem.id` inside each item file must match the file stem' \
  'Attempt, Task, and Merge Candidate ids must match their file stems' \
  'Work item IDs, Attempt IDs, Task IDs, and Merge Candidate IDs must not be empty, `.`, `..`, or contain `/` or `\`' \
  'Each stored Attempt must set `work_item_id` to the containing `WorkItem.id`' \
  'Each stored Task must set `work_item_id` to the containing `WorkItem.id`' \
  'must set `attempt_id` to the containing Attempt id' \
  'Invalid JSON, ID mismatches, invalid object IDs, and model validation failures must report the file path or object that failed'
do
  if ! grep -Fq "$phrase" <<<"$flat_section"; then
    echo "work model storage documentation does not contain: $phrase" >&2
    exit 1
  fi
done
