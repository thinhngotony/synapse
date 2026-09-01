#!/bin/sh
# Synapse installer — downloads a released binary, verifies it, installs it.
#
# Usage:
#   curl -fsSL https://synapse.hyberorbit.com/install | sh
#
# Environment overrides (all optional):
#   SYNAPSE_VERSION      release tag to install; unset means latest
#   SYNAPSE_INSTALL_DIR  destination directory (default ~/.local/bin)
#   SYNAPSE_BASE_URL     base URL holding the release assets; must be https,
#                        or http on localhost for testing
#
# shellcheck disable=SC2059  # color vars are deliberately part of the format
set -eu

REPO="thinhngotony/synapse"
DEST_DIR="${SYNAPSE_INSTALL_DIR:-$HOME/.local/bin}"
DEST="$DEST_DIR/synapse"

# ── Colors (POSIX compatible, suppressed when not a tty) ─────────────────────
if [ -t 1 ]; then
  RED='\033[0;31m'
  GREEN='\033[0;32m'
  YELLOW='\033[0;33m'
  DIM='\033[2m'
  BOLD='\033[1m'
  NC='\033[0m'
else
  RED=''; GREEN=''; YELLOW=''; DIM=''; BOLD=''; NC=''
fi

RULE="${DIM}  ────────────────────────────────────────────────────────────────${NC}"

ok()   { printf "      ${GREEN}o${NC} %s\n" "$1"; }
warn() { printf "      ${YELLOW}!${NC} %s\n" "$1"; }

die() {
  printf "\n      ${RED}x${NC} ${BOLD}%s${NC}\n" "$1" >&2
  shift
  for _line in "$@"; do
    printf "        ${DIM}%s${NC}\n" "$_line" >&2
  done
  printf "\n" >&2
  exit 1
}

# ── Temp workspace, always cleaned up ────────────────────────────────────────
TMP=''
DEST_TMP=''
cleanup() {
  [ -n "$TMP" ] && rm -rf "$TMP"
  [ -n "$DEST_TMP" ] && rm -f "$DEST_TMP"
  return 0
}
trap cleanup EXIT
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 143' TERM

# ── Detect platform ─────────────────────────────────────────────────────────
# Windows is supported through WSL2, which reports itself as Linux, so the
# Darwin/Linux pair covers the whole v1 platform matrix.
SUPPORTED="macOS (Intel, Apple Silicon), Linux (x86_64, aarch64), Windows via WSL2"

UNAME_S="$(uname -s)"
UNAME_M="$(uname -m)"

case "$UNAME_M" in
  arm64 | aarch64)  ARCH='aarch64' ;;
  x86_64 | amd64)   ARCH='x86_64' ;;
  *) die "Unsupported CPU architecture: $UNAME_M" \
         "Synapse supports: $SUPPORTED" ;;
esac

case "$UNAME_S" in
  Darwin) TARGET="${ARCH}-apple-darwin" ;;
  Linux)  TARGET="${ARCH}-unknown-linux-gnu" ;;
  *) die "Unsupported operating system: $UNAME_S" \
         "Synapse supports: $SUPPORTED" \
         "On Windows, run this installer inside WSL2." ;;
esac

# ── Pick a downloader ───────────────────────────────────────────────────────
if command -v curl >/dev/null 2>&1; then
  DL='curl'
elif command -v wget >/dev/null 2>&1; then
  DL='wget'
else
  die "Neither curl nor wget is available." \
      "Install one of them and re-run this installer."
fi

api_get() {
  if [ "$DL" = 'curl' ]; then
    curl -fsSL --proto '=https' "$1"
  else
    wget -qO- "$1"
  fi
}

download() { # download <url> <dest>
  if [ "$DL" = 'curl' ]; then
    # shellcheck disable=SC2086  # PROTO_OPT must word-split into two args
    curl -fsSL $PROTO_OPT "$1" -o "$2"
  else
    # shellcheck disable=SC2086
    if [ -n "${WGET_OPT:-}" ]; then
      wget -q $WGET_OPT -O "$2" "$1"
    else
      wget -q -O "$2" "$1"
    fi
  fi
}

# ── Pick a sha256 tool ──────────────────────────────────────────────────────
# Verification is a trust boundary: no tool means no install, never a skip.
if command -v sha256sum >/dev/null 2>&1; then
  sha256_of() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
  sha256_of() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
  die "No SHA-256 tool found (looked for sha256sum and shasum)." \
      "Synapse verifies its download before installing and will not skip that." \
      "On Debian/Ubuntu:  apt-get install coreutils" \
      "On macOS:          shasum ships with the base system; check your PATH."
fi

# ── Resolve version and asset location ──────────────────────────────────────
VERSION="${SYNAPSE_VERSION:-}"

if [ -n "${SYNAPSE_BASE_URL:-}" ]; then
  BASE_URL="${SYNAPSE_BASE_URL%/}"
  [ -n "$VERSION" ] || VERSION='(SYNAPSE_BASE_URL)'
else
  if [ -z "$VERSION" ]; then
    API="https://api.github.com/repos/${REPO}/releases/latest"
    VERSION="$(api_get "$API" 2>/dev/null \
      | awk -F'"' '/"tag_name"/ {print $4; exit}')" || VERSION=''

    [ -n "$VERSION" ] || die \
      "Could not determine the latest Synapse release." \
      "Tried: $API" \
      "The GitHub API may be unreachable, rate-limited, or the repository" \
      "may have no published release yet." \
      "Pin a known version instead:  SYNAPSE_VERSION=v1.0.0 sh install.sh"
  fi
  BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"
fi

# Remote assets must come over TLS. Plain http is allowed only against
# localhost, which is how ci/install-check.sh exercises this script.
# Reject any URL containing userinfo (@ before first / after scheme) to
# prevent http://127.0.0.1:80@example.com being treated as localhost.
case "$BASE_URL" in
  *://*@*/*|*://*@*) die "Refusing to download over an untrusted URL: $BASE_URL" \
         "URL contains userinfo (@) — refusing for security" \
         "SYNAPSE_BASE_URL must use https, or http on localhost for testing." ;;
esac
case "$BASE_URL" in
  https://*)                              PROTO_OPT="--proto =https" ; WGET_OPT="--https-only" ;;
  http://127.0.0.1|http://127.0.0.1:*|http://127.0.0.1/*|http://localhost|http://localhost:*|http://localhost/*)
                                          PROTO_OPT="--proto =http" ; WGET_OPT="" ;;
  *) die "Refusing to download over an untrusted URL: $BASE_URL" \
         "SYNAPSE_BASE_URL must use https, or http on localhost for testing." ;;
esac
# Extra validation for localhost HTTP: ensure host is exactly 127.0.0.1 or localhost
# with optional numeric port and optional path, no userinfo already rejected.
if [ "${BASE_URL#http://}" != "$BASE_URL" ]; then
  _tmp="${BASE_URL#http://}"
  _host="${_tmp%%/*}"; _host="${_host%%:*}"
  case "$_host" in
    127.0.0.1|localhost) ;;
    *) die "Refusing to download over an untrusted URL: $BASE_URL" \
           "http:// is only allowed for 127.0.0.1 and localhost" ;;
  esac
  # Validate port if present
  _portpart="${_tmp%%/*}"; _portpart="${_portpart#"${_host}"}"
  if [ -n "$_portpart" ]; then
    case "$_portpart" in
      :[0-9]*|:[0-9]*/*|"") ;;
      *) die "Refusing to download over an untrusted URL: $BASE_URL" \
             "Invalid port in localhost URL" ;;
    esac
  fi
fi

# ── Header ──────────────────────────────────────────────────────────────────
printf "\n"
printf "                          ${BOLD}Synapse${NC} ${DIM}%s${NC}\n" "$VERSION"
printf "                 ${DIM}AI harness installer and auto-updater${NC}\n"
printf "\n"
printf "%b\n" "$RULE"
printf "\n"
printf "  ${BOLD}System${NC}\n"
printf "\n"
printf "      OS         ${BOLD}%s${NC} ${DIM}(%s)${NC}\n" "$UNAME_S" "$UNAME_M"
printf "      Target     ${BOLD}%s${NC}\n" "$TARGET"
printf "      Install    ${DIM}%s${NC}\n" "$DEST"
printf "\n"
printf "%b\n" "$RULE"
printf "\n"
printf "  ${BOLD}Download${NC}\n"
printf "\n"

TMP="$(mktemp -d)"

# ── Checksums first: they name the exact asset to fetch ─────────────────────
download "${BASE_URL}/checksums.txt" "$TMP/checksums.txt" 2>/dev/null || die \
  "Failed to download checksums.txt" \
  "From: ${BASE_URL}/checksums.txt" \
  "Check your network, or confirm that release $VERSION exists."
ok "checksums.txt"

CK_LINE="$(grep -- "-${TARGET}\.tar\.gz\$" "$TMP/checksums.txt" | head -n1)" || CK_LINE=''
[ -n "$CK_LINE" ] || die \
  "Release $VERSION has no build for $TARGET." \
  "checksums.txt lists no asset ending in -${TARGET}.tar.gz" \
  "Synapse supports: $SUPPORTED"

EXPECTED="$(printf '%s\n' "$CK_LINE" | awk '{print $1}')"
ASSET="$(printf '%s\n' "$CK_LINE" | awk '{sub(/^\*/, "", $2); print $2}')"

# ── Validate ASSET is a simple basename, no path traversal
# Must be exactly synapse-<version>-<target>.tar.gz with no slash, backslash, or ..
case "$ASSET" in
  */*|*\\*|*..*) die "SHA-256 mismatch — refusing to install." \
         "asset    $ASSET" \
         "Asset name contains path traversal (/, \\, ..)" \
         "The checksums.txt is malformed or tampered with." ;;
esac
# Also enforce exact pattern: synapse-<version>-<target>.tar.gz
# Version may contain dots, hyphens, etc., but must not contain slashes
case "$ASSET" in
  synapse-*-"${TARGET}".tar.gz) ;;
  *) die "SHA-256 mismatch — refusing to install." \
         "asset    $ASSET" \
         "Asset name does not match expected pattern synapse-*-${TARGET}.tar.gz" ;;
esac

# Download to fixed path to avoid $TMP/$ASSET traversal
ASSET_TMP="$TMP/asset.tar.gz"
download "${BASE_URL}/${ASSET}" "$ASSET_TMP" 2>/dev/null || die \
  "Failed to download $ASSET" \
  "From: ${BASE_URL}/${ASSET}"
ok "$ASSET"

# ── Verify before extracting ────────────────────────────────────────────────
printf "\n"
printf "%b\n" "$RULE"
printf "\n"
printf "  ${BOLD}Verify${NC}\n"
printf "\n"

ACTUAL="$(sha256_of "$ASSET_TMP")"
if [ "$ACTUAL" != "$EXPECTED" ]; then
  die "SHA-256 mismatch — refusing to install." \
      "asset    $ASSET" \
      "expected $EXPECTED" \
      "actual   $ACTUAL" \
      "The download was corrupted or tampered with. Nothing was installed."
fi
ok "sha256 ${DIM}${ACTUAL}${NC}"

# ── Extract and install atomically ──────────────────────────────────────────
printf "\n"
printf "%b\n" "$RULE"
printf "\n"
printf "  ${BOLD}Install${NC}\n"
printf "\n"

tar -xzf "$ASSET_TMP" -C "$TMP" || die "Failed to extract $ASSET"
[ -f "$TMP/synapse" ] || die \
  "$ASSET does not contain a 'synapse' executable at its root."

mkdir -p "$DEST_DIR" || die "Cannot create $DEST_DIR"

# Stage inside the destination directory so the final step is a same-filesystem
# rename: a concurrent run or a running synapse never observes a partial file.
DEST_TMP="$(mktemp "$DEST_DIR/.synapse.XXXXXX")" || die "Cannot write to $DEST_DIR"
cat "$TMP/synapse" > "$DEST_TMP" || die "Cannot write to $DEST_DIR"
chmod 755 "$DEST_TMP"
mv -f "$DEST_TMP" "$DEST" || die "Cannot install to $DEST"
DEST_TMP=''
ok "installed ${DIM}${DEST}${NC}"

# ── Environment checks ─────────────────────────────────────────────────────
printf "\n"
printf "%b\n" "$RULE"
printf "\n"
printf "  ${BOLD}Environment${NC}\n"
printf "\n"

case ":${PATH}:" in
  *":${DEST_DIR}:"*) ok "on PATH" ;;
  *)
    warn "$DEST_DIR is not on your PATH"
    case "$(basename "${SHELL:-sh}")" in
      bash) _rc="$HOME/.bashrc"
            _line="export PATH=\"$DEST_DIR:\$PATH\"" ;;
      zsh)  _rc="$HOME/.zshrc"
            _line="export PATH=\"$DEST_DIR:\$PATH\"" ;;
      fish) _rc="$HOME/.config/fish/config.fish"
            _line="fish_add_path $DEST_DIR" ;;
      *)    _rc="your shell profile"
            _line="export PATH=\"$DEST_DIR:\$PATH\"" ;;
    esac
    printf "\n"
    printf "        Add this to ${BOLD}%s${NC}\n" "$_rc"
    printf "\n"
    printf "            ${BOLD}%s${NC}\n" "$_line"
    printf "\n"
    printf "        ${DIM}Or let Synapse do it:  %s setup-shell${NC}\n" "$DEST"
    printf "\n"
    ;;
esac

# Synapse drives Nix but never installs it: on macOS the official installer
# creates an APFS volume and edits system files, which is not a change to make
# on someone's behalf. Read-only commands work fine without it.
if command -v nix >/dev/null 2>&1 || [ -x /nix/var/nix/profiles/default/bin/nix ]; then
  ok "nix found"
else
  warn "Nix not found — Synapse needs it to install packages"
  printf "\n"
  printf "        Install Nix yourself with the Determinate Systems installer\n"
  printf "\n"
  printf "            ${BOLD}curl -fsSL https://install.determinate.systems/nix | sh -s -- install${NC}\n"
  printf "\n"
  printf "        ${DIM}Synapse will not do this for you: it creates a volume on macOS${NC}\n"
  printf "        ${DIM}and modifies system files. Read-only commands (status, doctor,${NC}\n"
  printf "        ${DIM}list, version) work without Nix.${NC}\n"
  printf "\n"
fi

# ── Post-install smoke check ───────────────────────────────────────────────
if ! SMOKE="$("$DEST" version 2>&1)"; then
  die "Installed binary failed to run: $DEST version" \
      "$SMOKE" \
      "The downloaded build may not match this machine ($TARGET)."
fi
ok "$(printf '%s\n' "$SMOKE" | head -n1)"

# ── Next steps ─────────────────────────────────────────────────────────────
printf "\n"
printf "%b\n" "$RULE"
printf "\n"
printf "  ${BOLD}Next${NC}\n"
printf "\n"
printf "      ${BOLD}synapse install${NC}              ${DIM}pick and install AI harnesses${NC}\n"
printf "      ${BOLD}synapse auto-update enable${NC}   ${DIM}keep them up to date${NC}\n"
printf "      ${BOLD}synapse status${NC}               ${DIM}show what is installed${NC}\n"
printf "\n"
