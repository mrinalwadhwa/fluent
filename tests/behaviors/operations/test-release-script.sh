#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

setup_fixture() {
  local tmp="$1"
  local root="$tmp/project"
  local bin="$tmp/bin"
  mkdir -p "$root/scripts" "$root/documentation/releases" "$bin"
  cp "$PROJECT_DIR/scripts/release.sh" "$root/scripts/release.sh"
  printf '[package]\nname = "fixture"\nversion = "9.8.7"\n' > "$root/Cargo.toml"
  printf '# Fixture release notes\n' > "$root/documentation/releases/v9.8.7.md"

  cat > "$bin/rustc" <<'EOF'
#!/bin/sh
printf 'rustc 1.0\nhost: aarch64-apple-darwin\n'
EOF
  cat > "$bin/git" <<'EOF'
#!/bin/sh
case "$*" in
  *"status --porcelain"*) printf '%s' "${FAKE_GIT_STATUS:-}" ;;
  *"fetch --quiet origin main --tags"*) exit 0 ;;
  *"rev-parse HEAD"*) printf '%s\n' "${FAKE_HEAD:-aaaaaaaa}" ;;
  *"rev-parse refs/remotes/origin/main"*) printf '%s\n' "${FAKE_ORIGIN_MAIN:-aaaaaaaa}" ;;
  *"show-ref --verify --quiet"*) [ "${FAKE_TAG_EXISTS:-0}" = 1 ] ;;
  *) printf 'unexpected git invocation: %s\n' "$*" >&2; exit 2 ;;
esac
EOF
  cat > "$bin/cargo" <<'EOF'
#!/bin/sh
case "$*" in
  *"build --release"*)
    mkdir -p "$FAKE_RELEASE_ROOT/target/release"
    cat > "$FAKE_RELEASE_ROOT/target/release/fluent" <<'BINARY'
#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_RELEASE_LOG"
exit "${FAKE_TESTER_EXIT:-0}"
BINARY
    chmod +x "$FAKE_RELEASE_ROOT/target/release/fluent"
    ;;
  *"fmt --all"*|*"check --tests --features test-support"*) exit 0 ;;
  *) printf 'unexpected cargo invocation: %s\n' "$*" >&2; exit 2 ;;
esac
EOF
  cat > "$bin/file" <<'EOF'
#!/bin/sh
printf '%s: Mach-O 64-bit executable arm64\n' "$1"
EOF
  cat > "$bin/otool" <<'EOF'
#!/bin/sh
printf '%s:\n\t/usr/lib/libSystem.B.dylib\n' "$2"
EOF
  cat > "$bin/codesign" <<'EOF'
#!/bin/sh
exit 0
EOF
  cat > "$bin/gh" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" > "$FAKE_GH_LOG"
mkdir -p "$FAKE_UPLOAD_DIR"
for argument in "$@"; do
  if [ -f "$argument" ]; then
    cp "$argument" "$FAKE_UPLOAD_DIR/"
  fi
done
EOF
  chmod +x "$bin"/*
}

run_release() {
  local tmp="$1"
  shift
  FAKE_RELEASE_ROOT="$tmp/project" \
  FAKE_RELEASE_LOG="$tmp/release.log" \
  FAKE_GH_LOG="$tmp/gh.log" \
  FAKE_UPLOAD_DIR="$tmp/uploads" \
  PATH="$tmp/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
    env "$@" bash "$tmp/project/scripts/release.sh"
}

test_publishes_checksum_at_exact_commit_after_gates() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  setup_fixture "$tmp"

  run_release "$tmp" > "$tmp/output" 2>&1

  grep -Fq -- '--no-sandbox tester check' "$tmp/release.log"
  grep -Fq -- 'release create v9.8.7 --target aaaaaaaa' "$tmp/gh.log"
  grep -Fq -- '--notes-file' "$tmp/gh.log"
  grep -Fq -- 'documentation/releases/v9.8.7.md' "$tmp/gh.log"
  grep -Fq -- 'fluent-aarch64-apple-darwin.sha256' "$tmp/gh.log"
  local checksum asset
  checksum="$tmp/uploads/fluent-aarch64-apple-darwin.sha256"
  asset="$tmp/uploads/fluent-aarch64-apple-darwin"
  [ -f "$checksum" ] || return 1
  [ -f "$asset" ] || return 1
  [ "$(awk '{print $1}' "$checksum")" = "$(shasum -a 256 "$asset" | awk '{print $1}')" ]
}

test_rejects_dirty_or_unsynchronized_source() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  setup_fixture "$tmp"

  if run_release "$tmp" FAKE_GIT_STATUS=' M source.rs' > "$tmp/dirty" 2>&1; then
    printf '    FAIL: dirty release source was accepted\n'
    return 1
  fi
  grep -Fq 'release source tree is not clean' "$tmp/dirty"

  if run_release "$tmp" FAKE_ORIGIN_MAIN=bbbbbbbb > "$tmp/unsynced" 2>&1; then
    printf '    FAIL: unsynchronized release source was accepted\n'
    return 1
  fi
  grep -Fq 'does not equal origin/main' "$tmp/unsynced"
}

test_rejects_reused_tag_or_failed_release_gate() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  setup_fixture "$tmp"

  if run_release "$tmp" FAKE_TAG_EXISTS=1 > "$tmp/tag" 2>&1; then
    printf '    FAIL: reused release tag was accepted\n'
    return 1
  fi
  grep -Fq 'already exists' "$tmp/tag"

  if run_release "$tmp" FAKE_TESTER_EXIT=7 > "$tmp/gate" 2>&1; then
    printf '    FAIL: failed release gate was accepted\n'
    return 1
  fi
  [ ! -e "$tmp/gh.log" ]
}

test_rejects_missing_release_notes() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  setup_fixture "$tmp"
  mv "$tmp/project/documentation/releases/v9.8.7.md" \
    "$tmp/missing-release-notes.md"

  if run_release "$tmp" > "$tmp/missing-notes" 2>&1; then
    printf '    FAIL: release without version-specific notes was accepted\n'
    return 1
  fi
  grep -Fq 'release notes not found' "$tmp/missing-notes"
  [ ! -e "$tmp/gh.log" ]
}

printf 'test-release-script\n\n'

run_test "publishes checksum at exact commit after gates" \
  test_publishes_checksum_at_exact_commit_after_gates
run_test "rejects dirty or unsynchronized source" \
  test_rejects_dirty_or_unsynchronized_source
run_test "rejects reused tag or failed release gate" \
  test_rejects_reused_tag_or_failed_release_gate
run_test "rejects missing release notes" \
  test_rejects_missing_release_notes

summarize_and_exit
