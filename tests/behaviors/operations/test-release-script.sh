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
  mkdir -p "$root/tools"
  cp "$PROJECT_DIR/tools/ghostty-LICENSE" "$root/tools/ghostty-LICENSE"
  cat > "$root/scripts/prepare-terminal-deps.sh" <<'PREPARE'
#!/bin/sh
printf 'prepare %s\n' "$1" >> "$FAKE_CARGO_LOG"
test "${FAKE_PREPARE_FAIL_TARGET:-}" != "$1"
PREPARE
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
printf '%s\n' "$*" >> "$FAKE_CARGO_LOG"
case "$*" in
  *"build --release"*)
    target=""
    expect_target=0
    for argument in "$@"; do
      if [ "$expect_target" -eq 1 ]; then
        target="$argument"
        break
      fi
      [ "$argument" = "--target" ] && expect_target=1
    done
    [ -n "$target" ] || { printf 'missing --target\n' >&2; exit 2; }
    if [ "${FAKE_BUILD_FAIL_TARGET:-}" = "$target" ]; then
      printf 'injected build failure for %s\n' "$target" >&2
      exit 7
    fi
    mkdir -p "$FAKE_RELEASE_ROOT/target/$target/release"
    cat > "$FAKE_RELEASE_ROOT/target/$target/release/fluent" <<'BINARY'
#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_RELEASE_LOG"
exit "${FAKE_TESTER_EXIT:-0}"
BINARY
    chmod +x "$FAKE_RELEASE_ROOT/target/$target/release/fluent"
    ;;
  *"fmt --all"*|*"check --tests --features test-support"*) exit 0 ;;
  *) printf 'unexpected cargo invocation: %s\n' "$*" >&2; exit 2 ;;
esac
EOF
  cat > "$bin/lipo" <<'EOF'
#!/bin/sh
case "$2" in
  *aarch64-apple-darwin) target="aarch64-apple-darwin"; arch="arm64" ;;
  *x86_64-apple-darwin) target="x86_64-apple-darwin"; arch="x86_64" ;;
  *) printf 'unknown architecture for %s\n' "$2" >&2; exit 2 ;;
esac
if [ "${FAKE_ARCH_MISMATCH_TARGET:-}" = "$target" ]; then
  printf 'unexpected-arch\n'
else
  printf '%s\n' "$arch"
fi
EOF
  cat > "$bin/codesign" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_CODESIGN_LOG"
if [ -n "${FAKE_SIGN_FAIL_TARGET:-}" ]; then
  case "$*" in
    *"$FAKE_SIGN_FAIL_TARGET"*) exit 8 ;;
  esac
fi
exit 0
EOF
  cat > "$bin/shasum" <<'EOF'
#!/bin/sh
if [ -n "${FAKE_CHECKSUM_FAIL_TARGET:-}" ]; then
  case "$*" in
    *"$FAKE_CHECKSUM_FAIL_TARGET"*) exit 9 ;;
  esac
fi
/usr/bin/shasum "$@"
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
  FAKE_CARGO_LOG="$tmp/cargo.log" \
  FAKE_CODESIGN_LOG="$tmp/codesign.log" \
  FAKE_GH_LOG="$tmp/gh.log" \
  FAKE_UPLOAD_DIR="$tmp/uploads" \
  PATH="$tmp/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
    env "$@" bash "$tmp/project/scripts/release.sh"
}

test_publishes_both_mac_architectures_at_exact_commit_after_gates() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  setup_fixture "$tmp"

  run_release "$tmp" > "$tmp/output" 2>&1

  grep -Fq -- '--no-sandbox tester check' "$tmp/release.log"
  grep -Fq -- 'release create v9.8.7 --target aaaaaaaa' "$tmp/gh.log"
  grep -Fq -- '--notes-file' "$tmp/gh.log"
  grep -Fq -- 'documentation/releases/v9.8.7.md' "$tmp/gh.log"
  grep -Fq -- '--target aarch64-apple-darwin' "$tmp/cargo.log"
  grep -Fq -- '--target x86_64-apple-darwin' "$tmp/cargo.log"

  grep -Fq 'ghostty-LICENSE' "$tmp/gh.log"
  local target checksum asset
  for target in aarch64-apple-darwin x86_64-apple-darwin; do
    grep -Fxq "prepare $target" "$tmp/cargo.log"
    test "$(grep -n "prepare $target" "$tmp/cargo.log" | cut -d: -f1)" -lt "$(grep -n "build --release --target $target" "$tmp/cargo.log" | cut -d: -f1)"
    grep -Fq -- "fluent-${target}" "$tmp/gh.log"
    grep -Fq -- "fluent-${target}.sha256" "$tmp/gh.log"
    grep -Fq -- "--sign - --force $tmp/project/target/release-staging/fluent-${target}" \
      "$tmp/codesign.log"
    grep -Fq -- "--verify --strict $tmp/project/target/release-staging/fluent-${target}" \
      "$tmp/codesign.log"
    checksum="$tmp/uploads/fluent-${target}.sha256"
    asset="$tmp/uploads/fluent-${target}"
    [ -f "$checksum" ] || return 1
    [ -f "$asset" ] || return 1
    [ "$(awk '{print $1}' "$checksum")" = \
      "$(shasum -a 256 "$asset" | awk '{print $1}')" ] || return 1
    [ "$(awk '{print $2}' "$checksum")" = "fluent-${target}" ] || return 1
  done
}

test_rejects_failed_native_preparation_before_publication() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  setup_fixture "$tmp"
  if run_release "$tmp" FAKE_PREPARE_FAIL_TARGET=x86_64-apple-darwin > "$tmp/output" 2>&1; then
    return 1
  fi
  [ ! -e "$tmp/gh.log" ]
}

test_rejects_failed_target_build_before_publication() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  setup_fixture "$tmp"

  if run_release "$tmp" FAKE_BUILD_FAIL_TARGET=x86_64-apple-darwin \
    > "$tmp/build-failure" 2>&1; then
    printf '    FAIL: release with a missing Intel build was accepted\n'
    return 1
  fi
  grep -Fq 'injected build failure for x86_64-apple-darwin' "$tmp/build-failure"
  [ ! -e "$tmp/gh.log" ]
}

test_rejects_failed_target_signature_before_publication() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  setup_fixture "$tmp"

  if run_release "$tmp" FAKE_SIGN_FAIL_TARGET=x86_64-apple-darwin \
    > "$tmp/sign-failure" 2>&1; then
    printf '    FAIL: release with an unsigned Intel build was accepted\n'
    return 1
  fi
  [ ! -e "$tmp/gh.log" ]
}

test_rejects_wrong_target_architecture_before_publication() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  setup_fixture "$tmp"

  if run_release "$tmp" FAKE_ARCH_MISMATCH_TARGET=x86_64-apple-darwin \
    > "$tmp/architecture-failure" 2>&1; then
    printf '    FAIL: release with the wrong Intel architecture was accepted\n'
    return 1
  fi
  grep -Fq 'contains architecture unexpected-arch, expected x86_64' \
    "$tmp/architecture-failure"
  [ ! -e "$tmp/gh.log" ]
}

test_rejects_failed_target_checksum_before_publication() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  setup_fixture "$tmp"

  if run_release "$tmp" FAKE_CHECKSUM_FAIL_TARGET=x86_64-apple-darwin \
    > "$tmp/checksum-failure" 2>&1; then
    printf '    FAIL: release without an Intel checksum was accepted\n'
    return 1
  fi
  [ ! -e "$tmp/gh.log" ]
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

run_test "publishes both Mac architectures at exact commit after gates" \
  test_publishes_both_mac_architectures_at_exact_commit_after_gates
run_test "rejects failed target build before publication" \
  test_rejects_failed_target_build_before_publication
run_test "rejects failed target signature before publication" \
  test_rejects_failed_target_signature_before_publication
run_test "rejects wrong target architecture before publication" \
  test_rejects_wrong_target_architecture_before_publication
run_test "rejects failed target checksum before publication" \
  test_rejects_failed_target_checksum_before_publication
run_test "rejects dirty or unsynchronized source" \
  test_rejects_dirty_or_unsynchronized_source
run_test "rejects reused tag or failed release gate" \
  test_rejects_reused_tag_or_failed_release_gate
run_test "rejects missing release notes" \
  test_rejects_missing_release_notes

run_test "rejects failed native preparation before publication" test_rejects_failed_native_preparation_before_publication
summarize_and_exit
