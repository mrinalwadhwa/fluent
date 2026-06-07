#!/usr/bin/env bash
# test-fargate-entrypoint — Verify the Fargate transfer wrapper uses Rust.

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/../../.." && pwd)"
ENTRYPOINT="${PROJECT_DIR}/infrastructure/run/entrypoint.sh"
RESULT=0

assert_file_contains() {
  local file="$1"
  local expected="$2"
  if ! grep -Fq -- "$expected" "$file"; then
    printf '    FAIL: %s does not contain %s\n' "$file" "$expected"
    RESULT=1
  fi
}

TEST_DIR="$(mktemp -d -t factory-fargate-entrypoint-XXXXXX)"
trap 'rm -rf "$TEST_DIR"' EXIT

MOCK_BIN="${TEST_DIR}/bin"
mkdir -p "$MOCK_BIN"

cat > "$MOCK_BIN/factory" <<'FACTORY'
#!/usr/bin/env bash
set -euo pipefail

{
  printf 'factory-bin=%s\n' "$0"
  printf '%s\n' "$@"
} > "$MOCK_FACTORY_ARGS"
printf 'complete' > ".factory/runs/${FACTORY_RUN_ID}/status"
FACTORY
chmod +x "$MOCK_BIN/factory"

cat > "$MOCK_BIN/aws" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

if [ "$1 $2 $3" != "s3 cp --region" ]; then
  printf 'unexpected aws args: %s\n' "$*" >&2
  exit 1
fi

src="$5"
dst="$6"
case "${src} -> ${dst}" in
  "s3://${FACTORY_S3_BUCKET}/runs/${FACTORY_RUN_ID}/workspace-in.tar -> -")
    cat "$MOCK_WORKSPACE_IN"
    ;;
  "- -> s3://${FACTORY_S3_BUCKET}/runs/${FACTORY_RUN_ID}/workspace.tar")
    cat > "$MOCK_WORKSPACE_OUT"
    ;;
  *)
    printf 'unexpected aws transfer: %s -> %s\n' "$src" "$dst" >&2
    exit 1
    ;;
esac
SH

chmod +x "$MOCK_BIN/aws"

run_entrypoint_case() {
  local name="$1"
  local factory_bin_mode="$2"
  local workspace="${TEST_DIR}/${name}/workspace"
  local input="${TEST_DIR}/${name}/input"
  local output="${TEST_DIR}/${name}/output"
  local workspace_in="${TEST_DIR}/${name}/workspace-in.tar"
  local workspace_out="${TEST_DIR}/${name}/workspace-out.tar"
  local factory_args="${TEST_DIR}/${name}/factory-args"

  mkdir -p "$workspace" "$input/.factory/runs/run-fg" "$output"
  printf 'planned' > "$input/.factory/runs/run-fg/status"
  printf 'Brief\n' > "$input/.factory/runs/run-fg/brief.md"
  printf '[package]\nname = "factory"\nversion = "0.1.0"\nedition = "2024"\n' \
    > "$input/Cargo.toml"
  tar cf "$workspace_in" -C "$input" .

  case "$factory_bin_mode" in
    explicit)
      PATH="${MOCK_BIN}:${PATH}" \
      WORKSPACE="$workspace" \
      FACTORY_RUN_ID="run-fg" \
      FACTORY_S3_BUCKET="bucket" \
      FACTORY_REGION="us-west-1" \
      FACTORY_BIN="$MOCK_BIN/factory" \
      CLAUDE_CODE_OAUTH_TOKEN="token" \
      MOCK_WORKSPACE_IN="$workspace_in" \
      MOCK_WORKSPACE_OUT="$workspace_out" \
      MOCK_FACTORY_ARGS="$factory_args" \
        bash "$ENTRYPOINT"
      ;;
    path)
      env -u FACTORY_BIN \
        PATH="${MOCK_BIN}:${PATH}" \
        WORKSPACE="$workspace" \
        FACTORY_RUN_ID="run-fg" \
        FACTORY_S3_BUCKET="bucket" \
        FACTORY_REGION="us-west-1" \
        CLAUDE_CODE_OAUTH_TOKEN="token" \
        MOCK_WORKSPACE_IN="$workspace_in" \
        MOCK_WORKSPACE_OUT="$workspace_out" \
        MOCK_FACTORY_ARGS="$factory_args" \
        bash "$ENTRYPOINT"
      ;;
    *)
      printf 'unknown factory binary mode: %s\n' "$factory_bin_mode" >&2
      exit 1
      ;;
  esac

  assert_file_contains "$factory_args" "factory-bin=$MOCK_BIN/factory"
  assert_file_contains "$factory_args" "run"
  assert_file_contains "$factory_args" "--runtime"
  assert_file_contains "$factory_args" "local"
  assert_file_contains "$factory_args" "--no-sandbox"
  assert_file_contains "$factory_args" "--in-place"
  assert_file_contains "$factory_args" "--coder"
  assert_file_contains "$factory_args" "claude"
  assert_file_contains "$factory_args" "--run-id"
  assert_file_contains "$factory_args" "run-fg"

  tar xf "$workspace_out" -C "$output"
  assert_file_contains "$output/.factory/runs/run-fg/status" "complete"
  assert_file_contains "$output/.factory/active-run" "run-fg"
}

run_entrypoint_case explicit explicit
run_entrypoint_case default-path path

if [ "$RESULT" -eq 0 ]; then
  printf 'PASS: fargate entrypoint uses Rust session loop\n'
fi

exit "$RESULT"
