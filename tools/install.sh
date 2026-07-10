#!/bin/sh
set -eu

FLUENT_REPO="mrinalwadhwa/fluent"
DEFAULT_INSTALL_DIR="${HOME}/.local/bin"

say() {
  printf 'fluent-install: %s\n' "$1"
}

err() {
  printf 'fluent-install: error: %s\n' "$1" >&2
  exit 1
}

required() {
  command -v "$1" >/dev/null 2>&1 || err "$1 is required but not found"
}

detect_triple() {
  local _os _arch
  _os="$(uname -s)"
  _arch="$(uname -m)"

  case "$_os" in
    Darwin)
      case "$_arch" in
        arm64)  echo "aarch64-apple-darwin" ;;
        x86_64) echo "x86_64-apple-darwin" ;;
        *) return 1 ;;
      esac
      ;;
    Linux)
      case "$_arch" in
        x86_64)  echo "x86_64-unknown-linux-gnu" ;;
        aarch64) echo "aarch64-unknown-linux-gnu" ;;
        *) return 1 ;;
      esac
      ;;
    *) return 1 ;;
  esac
}

resolve_latest_tag() {
  local _base_url="$1"
  local _url="${_base_url}/latest"

  if printf '%s' "$_base_url" | grep -q '^file://'; then
    local _path
    _path="$(printf '%s' "$_url" | sed 's|^file://||')"
    cat "$_path"
    return
  fi

  curl --proto '=https' --tlsv1.2 --fail --silent --location \
    --connect-timeout 10 --max-time 30 \
    "$_url"
}

download_asset() {
  local _base_url="$1"
  local _tag="$2"
  local _asset_name="$3"
  local _dest="$4"

  local _url="${_base_url}/download/${_tag}/${_asset_name}"

  if printf '%s' "$_base_url" | grep -q '^file://'; then
    local _path
    _path="$(printf '%s' "$_url" | sed 's|^file://||')"
    cp "$_path" "$_dest"
    return
  fi

  curl --proto '=https' --tlsv1.2 --fail --silent --location \
    --connect-timeout 10 --max-time 300 \
    --output "$_dest" \
    "$_url"
}

modify_path() {
  local _install_dir="$1"
  local _profile

  case "${SHELL:-}" in
    */zsh)  _profile="${HOME}/.zshrc" ;;
    */bash) _profile="${HOME}/.bashrc" ;;
    */fish) _profile="${HOME}/.config/fish/config.fish" ;;
    *)      _profile="${HOME}/.profile" ;;
  esac

  if [ -f "$_profile" ] && grep -q "$_install_dir" "$_profile" 2>/dev/null; then
    return
  fi

  local _line="export PATH=\"${_install_dir}:\$PATH\""

  if [ -f "$_profile" ]; then
    printf '\n%s\n' "$_line" >> "$_profile"
  else
    printf '%s\n' "$_line" > "$_profile"
  fi

  say "added ${_install_dir} to PATH in ${_profile}"
  say "reload your shell or run: source ${_profile}"
}

main() {
  local _version=""
  local _install_dir="$DEFAULT_INSTALL_DIR"
  local _no_modify_path=0

  while [ $# -gt 0 ]; do
    case "$1" in
      --version)
        shift
        [ $# -gt 0 ] || err "--version requires a value"
        _version="$1"
        ;;
      --install-path)
        shift
        [ $# -gt 0 ] || err "--install-path requires a value"
        _install_dir="$1"
        ;;
      --no-modify-path)
        _no_modify_path=1
        ;;
      *)
        err "unknown option: $1"
        ;;
    esac
    shift
  done

  required curl

  local _triple
  _triple="$(detect_triple)" || err "unsupported platform: $(uname -s) $(uname -m)"

  local _asset_name="fluent-${_triple}"
  local _base_url="${FLUENT_INSTALL_BASE_URL:-https://github.com/${FLUENT_REPO}/releases}"

  local _tag
  if [ -n "$_version" ]; then
    _tag="v${_version}"
  else
    say "resolving latest version..."
    _tag="$(resolve_latest_tag "$_base_url")" || err "failed to resolve latest release"
    _tag="$(printf '%s' "$_tag" | tr -d '[:space:]')"
    [ -n "$_tag" ] || err "failed to resolve latest release tag"
  fi

  say "installing fluent ${_tag}..."

  mkdir -p "$_install_dir"

  local _tmp="${_install_dir}/fluent.new"

  if ! download_asset "$_base_url" "$_tag" "$_asset_name" "$_tmp"; then
    rm -f "$_tmp"
    err "download failed"
  fi

  chmod 0755 "$_tmp"
  mv "$_tmp" "${_install_dir}/fluent"

  say "installed fluent ${_tag} to ${_install_dir}/fluent"

  if [ "$_no_modify_path" -eq 1 ]; then
    case ":${PATH}:" in
      *":${_install_dir}:"*) ;;
      *) say "warning: ${_install_dir} is not on PATH — add it manually" ;;
    esac
  else
    modify_path "$_install_dir"
  fi
}

main "$@"
