#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
INSTALL_SCRIPT="${PROJECT_DIR}/tools/install.sh"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)  ASSET_TRIPLE="aarch64-apple-darwin" ;;
  Darwin-x86_64) ASSET_TRIPLE="x86_64-apple-darwin" ;;
  Linux-x86_64)  ASSET_TRIPLE="x86_64-unknown-linux-gnu" ;;
  Linux-aarch64) ASSET_TRIPLE="aarch64-unknown-linux-gnu" ;;
  *) ASSET_TRIPLE="unsupported" ;;
esac
ASSET_NAME="fluent-${ASSET_TRIPLE}"

setup_fixture_release() {
  local tmp="$1"
  local version="${2:-0.99.0}"
  local tag="v${version}"
  local releases_dir="$tmp/releases"
  local download_dir="$releases_dir/download/${tag}"
  mkdir -p "$download_dir"

  printf '#!/bin/sh\nprintf "fluent %s (abc1234)\\n"\n' "$version" \
    > "$download_dir/$ASSET_NAME"
  chmod +x "$download_dir/$ASSET_NAME"

  local latest_file="$releases_dir/latest"
  printf '%s\n' "$tag" > "$latest_file"
}

test_installs_runnable_binary() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN

  setup_fixture_release "$tmp" "0.99.0"

  local install_dir="$tmp/home/.local/bin"
  mkdir -p "$install_dir"

  FLUENT_INSTALL_BASE_URL="file://$tmp/releases" \
  HOME="$tmp/home" \
    bash "$INSTALL_SCRIPT" --install-path "$install_dir" --no-modify-path \
    > "$tmp/stdout.txt" 2>&1

  if [ ! -f "$install_dir/fluent" ]; then
    printf '    FAIL: binary not installed at %s\n' "$install_dir/fluent"
    return 1
  fi

  if [ ! -x "$install_dir/fluent" ]; then
    printf '    FAIL: installed binary is not executable\n'
    return 1
  fi

  local output
  output="$("$install_dir/fluent" 2>&1)" || true
  if ! printf '%s' "$output" | grep -q "fluent 0.99.0"; then
    printf '    FAIL: binary should run and report version; got: %s\n' "$output"
    return 1
  fi
}

test_unsupported_platform() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN

  setup_fixture_release "$tmp" "0.99.0"

  local install_dir="$tmp/home/.local/bin"
  mkdir -p "$install_dir"

  local fake_bin="$tmp/fake-bin"
  mkdir -p "$fake_bin"
  cat > "$fake_bin/uname" <<'WRAPPER'
#!/bin/sh
for arg in "$@"; do
  case "$arg" in
    -s) printf 'FreeBSD\n'; exit 0 ;;
    -m) printf 'x86_64\n'; exit 0 ;;
  esac
done
/usr/bin/uname "$@"
WRAPPER
  chmod +x "$fake_bin/uname"

  local rc=0
  PATH="$fake_bin:$PATH" \
  FLUENT_INSTALL_BASE_URL="file://$tmp/releases" \
  HOME="$tmp/home" \
    bash "$INSTALL_SCRIPT" --install-path "$install_dir" --no-modify-path \
    > "$tmp/stdout.txt" 2>&1 || rc=$?

  if [ "$rc" -eq 0 ]; then
    printf '    FAIL: should exit non-zero for unsupported platform\n'
    return 1
  fi

  if [ -f "$install_dir/fluent" ]; then
    printf '    FAIL: should not install binary on unsupported platform\n'
    return 1
  fi

  if ! grep -qi "unsupported" "$tmp/stdout.txt"; then
    printf '    FAIL: should report unsupported platform; got: %s\n' "$(cat "$tmp/stdout.txt")"
    return 1
  fi
}

test_modifies_path() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN

  setup_fixture_release "$tmp" "0.99.0"

  local install_dir="$tmp/home/.local/bin"
  local home_dir="$tmp/home"
  mkdir -p "$install_dir" "$home_dir"

  FLUENT_INSTALL_BASE_URL="file://$tmp/releases" \
  HOME="$home_dir" \
  SHELL="/bin/zsh" \
    bash "$INSTALL_SCRIPT" --install-path "$install_dir" \
    > "$tmp/stdout.txt" 2>&1

  if [ ! -f "$home_dir/.zshrc" ]; then
    printf '    FAIL: should create .zshrc with PATH entry\n'
    return 1
  fi

  if ! grep -q "$install_dir" "$home_dir/.zshrc"; then
    printf '    FAIL: .zshrc should contain install dir; got: %s\n' "$(cat "$home_dir/.zshrc")"
    return 1
  fi

  if ! grep -q "reload" "$tmp/stdout.txt"; then
    printf '    FAIL: should tell user to reload shell; got: %s\n' "$(cat "$tmp/stdout.txt")"
    return 1
  fi
}

test_no_modify_path_warns() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN

  setup_fixture_release "$tmp" "0.99.0"

  local install_dir="$tmp/home/.local/bin"
  mkdir -p "$install_dir"

  FLUENT_INSTALL_BASE_URL="file://$tmp/releases" \
  HOME="$tmp/home" \
  PATH="/usr/bin:/bin" \
    bash "$INSTALL_SCRIPT" --install-path "$install_dir" --no-modify-path \
    > "$tmp/stdout.txt" 2>&1

  if ! grep -q "not on PATH\|add it manually" "$tmp/stdout.txt"; then
    printf '    FAIL: should warn about PATH; got: %s\n' "$(cat "$tmp/stdout.txt")"
    return 1
  fi

  if [ -f "$tmp/home/.zshrc" ] || [ -f "$tmp/home/.bashrc" ] || [ -f "$tmp/home/.profile" ]; then
    printf '    FAIL: should not modify shell config with --no-modify-path\n'
    return 1
  fi
}

test_download_failure() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN

  local releases_dir="$tmp/releases"
  mkdir -p "$releases_dir"
  printf 'v0.99.0\n' > "$releases_dir/latest"

  local install_dir="$tmp/home/.local/bin"
  mkdir -p "$install_dir"

  local rc=0
  FLUENT_INSTALL_BASE_URL="file://$tmp/releases" \
  HOME="$tmp/home" \
    bash "$INSTALL_SCRIPT" --install-path "$install_dir" --no-modify-path \
    > "$tmp/stdout.txt" 2>&1 || rc=$?

  if [ "$rc" -eq 0 ]; then
    printf '    FAIL: should exit non-zero when download fails\n'
    return 1
  fi

  if [ -f "$install_dir/fluent" ]; then
    printf '    FAIL: should not leave a partial binary\n'
    return 1
  fi

  if [ -f "$install_dir/fluent.new" ]; then
    printf '    FAIL: should not leave a temporary file\n'
    return 1
  fi
}

printf 'test-install-script\n\n'

run_test "installs a runnable binary" test_installs_runnable_binary
run_test "unsupported platform" test_unsupported_platform
run_test "modifies PATH" test_modifies_path
run_test "no-modify-path warns" test_no_modify_path_warns
run_test "download failure" test_download_failure

summarize_and_exit
