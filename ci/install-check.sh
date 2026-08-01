#!/usr/bin/env bash
# Verify install.sh end to end without needing a published GitHub release.
#
# Builds a fake release layout (four tarballs, each holding a stub `synapse`,
# plus a real checksums.txt), serves it over localhost HTTP, and points
# install.sh at it through SYNAPSE_BASE_URL.
#
# The load-bearing case is the corrupted checksum: it must abort with a
# non-zero exit and leave nothing installed. A verifier that only ever sees
# good input has not been tested.
#
# Usage: ci/install-check.sh
#
# shellcheck disable=SC2329  # helpers run indirectly, via `check` and `trap`
set -euo pipefail

INSTALLER="${INSTALLER:-./install.sh}"
[ -f "$INSTALLER" ] || { echo "::error::not found: $INSTALLER (run from repo root)" >&2; exit 1; }
INSTALLER="$(cd "$(dirname "$INSTALLER")" && pwd)/$(basename "$INSTALLER")"

command -v python3 >/dev/null || { echo "::error::python3 required" >&2; exit 1; }

FAKE_VERSION='v9.9.9'

WORK="$(mktemp -d)"
SRV_PID=''
cleanup() {
  if [ -n "$SRV_PID" ]; then
    kill "$SRV_PID" 2>/dev/null || true
    wait "$SRV_PID" 2>/dev/null || true
  fi
  rm -rf "$WORK"
}
trap cleanup EXIT

fail=0
note() { printf '%s\n' "$*" >&2; }

# check <description> <command...> — runs the command itself so a failing
# assertion never trips `set -e` and the run reports every failure, not just
# the first.
check() {
  local desc="$1"; shift
  if "$@"; then
    echo "  ok    $desc"
  else
    note "::error::$desc"
    fail=1
  fi
}

# Inverse of check: the command is required to fail.
check_fails() {
  local desc="$1"; shift
  if "$@"; then
    note "::error::$desc (command unexpectedly succeeded)"
    fail=1
  else
    echo "  ok    $desc"
  fi
}

sha256_of() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

no_staging_files() { # destination dir must hold no .synapse.* leftovers
  ! compgen -G "$1/.synapse.*" >/dev/null
}

# --- expected target for this machine --------------------------------------
# Derived here independently of install.sh, so the two can disagree.
case "$(uname -m)" in
  arm64 | aarch64) arch='aarch64' ;;
  x86_64 | amd64)  arch='x86_64' ;;
  *) note "::error::unsupported arch for this test: $(uname -m)"; exit 1 ;;
esac
case "$(uname -s)" in
  Darwin) EXPECT_TARGET="${arch}-apple-darwin" ;;
  Linux)  EXPECT_TARGET="${arch}-unknown-linux-gnu" ;;
  *) note "::error::unsupported os for this test: $(uname -s)"; exit 1 ;;
esac
EXPECT_STUB="synapse 9.9.9 (stub $EXPECT_TARGET)"
echo "== fake release for $EXPECT_TARGET"

# --- build the fake release ------------------------------------------------
# All four targets get an asset, so picking the right one is a real choice and
# not the only option on offer. Each stub names its own target, so a wrong pick
# is visible in the installed binary's output rather than passing silently.
RELEASE="$WORK/release"
STAGE="$WORK/stage"
mkdir -p "$RELEASE" "$STAGE"

for t in aarch64-apple-darwin x86_64-apple-darwin \
         x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do
  cat > "$STAGE/synapse" <<EOF
#!/bin/sh
[ "\${1:-}" = version ] && { echo 'synapse 9.9.9 (stub $t)'; exit 0; }
echo "stub: unexpected args: \$*" >&2
exit 2
EOF
  chmod +x "$STAGE/synapse"
  tar -czf "$RELEASE/synapse-9.9.9-$t.tar.gz" -C "$STAGE" synapse
done

( cd "$RELEASE" && for f in ./*.tar.gz; do
    printf '%s  %s\n' "$(sha256_of "$f")" "${f#./}"
  done > checksums.txt )
cp "$RELEASE/checksums.txt" "$WORK/checksums.good"

GOOD_SUM="$(grep -- "-${EXPECT_TARGET}.tar.gz" "$WORK/checksums.good" | cut -d' ' -f1)"

# --- serve it on a free port ----------------------------------------------
PORT="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')"
python3 -m http.server "$PORT" --bind 127.0.0.1 --directory "$RELEASE" >"$WORK/srv.log" 2>&1 &
SRV_PID=$!

for _ in $(seq 50); do
  curl -fsS "http://127.0.0.1:$PORT/checksums.txt" -o /dev/null 2>/dev/null && break
  sleep 0.1
done
check "fake release served on 127.0.0.1:$PORT" \
  curl -fsS "http://127.0.0.1:$PORT/checksums.txt" -o /dev/null

BASE="http://127.0.0.1:$PORT"

run_installer() { # run_installer <install-dir> <log>
  env SYNAPSE_BASE_URL="$BASE" \
      SYNAPSE_VERSION="$FAKE_VERSION" \
      SYNAPSE_INSTALL_DIR="$1" \
      HOME="$WORK/home" \
      sh "$INSTALLER" >"$2" 2>&1
}

run_installer_at() { # run_installer_at <base-url> <install-dir> <log>
  env SYNAPSE_BASE_URL="$1" \
      SYNAPSE_VERSION="$FAKE_VERSION" \
      SYNAPSE_INSTALL_DIR="$2" \
      HOME="$WORK/home" \
      sh "$INSTALLER" >"$3" 2>&1
}

# ==========================================================================
echo "== happy path"
# ==========================================================================
BIN1="$WORK/bin1"
check "installer exited 0" run_installer "$BIN1" "$WORK/run1.log"

check "selected target $EXPECT_TARGET" \
  grep -q -- "$EXPECT_TARGET" "$WORK/run1.log"
check "downloaded the matching asset" \
  grep -q -- "synapse-9.9.9-${EXPECT_TARGET}.tar.gz" "$WORK/run1.log"
check "reported the verified sha256 $GOOD_SUM" \
  grep -q -- "$GOOD_SUM" "$WORK/run1.log"

check "binary landed at the destination" test -f "$BIN1/synapse"
check "binary is executable"            test -x "$BIN1/synapse"

# Proves the *right* tarball was extracted, not merely some tarball.
installed_version() { [ "$("$BIN1/synapse" version)" = "$EXPECT_STUB" ]; }
check "extracted binary is the $EXPECT_TARGET build" installed_version

check "post-install smoke check ran 'synapse version'" \
  grep -qF -- "$EXPECT_STUB" "$WORK/run1.log"
check "printed 'synapse install' next step" \
  grep -q -- "synapse install" "$WORK/run1.log"
check "printed 'auto-update enable' next step" \
  grep -q -- "auto-update enable" "$WORK/run1.log"
check "no staging temp file left behind" no_staging_files "$BIN1"

# ==========================================================================
echo "== corrupted checksum must be rejected"
# ==========================================================================
# Flip only the recorded hash for this machine's asset. The tarball itself is
# untouched and perfectly valid, so the sole reason to refuse is verification.
python3 - "$RELEASE/checksums.txt" "$EXPECT_TARGET" <<'PY'
import sys
path, target = sys.argv[1], sys.argv[2]
lines = []
for line in open(path):
    h, _, name = line.partition('  ')
    if name.strip().endswith(f'-{target}.tar.gz'):
        h = 'dead' + h[4:]          # same length, wrong value
    lines.append(f'{h}  {name.lstrip()}')
open(path, 'w').write(''.join(lines))
PY
check "checksums.txt corrupted for $EXPECT_TARGET" \
  grep -q "^dead" "$RELEASE/checksums.txt"

BIN2="$WORK/bin2"
check_fails "installer exited non-zero on bad checksum" \
  run_installer "$BIN2" "$WORK/run2.log"
check "nothing installed after rejection"        test ! -e "$BIN2/synapse"
check "no partial staging file after rejection"  no_staging_files "$BIN2"
check "explained the mismatch to the user" \
  grep -qi "mismatch" "$WORK/run2.log"
check "did not extract before verifying" \
  grep -qi "refusing to install" "$WORK/run2.log"

# A failed reinstall over a good binary must not damage it.
check_fails "failed reinstall over an existing install exits non-zero" \
  run_installer "$BIN1" "$WORK/run3.log"
check "failed reinstall left the existing binary intact" installed_version

cp "$WORK/checksums.good" "$RELEASE/checksums.txt"

# ==========================================================================
echo "== release without a build for this target is refused"
# ==========================================================================
grep -v -- "-${EXPECT_TARGET}.tar.gz" "$WORK/checksums.good" > "$RELEASE/checksums.txt"
BIN4="$WORK/bin4"
check_fails "missing target build exits non-zero" \
  run_installer "$BIN4" "$WORK/run4.log"
check "nothing installed when target is absent" test ! -e "$BIN4/synapse"
cp "$WORK/checksums.good" "$RELEASE/checksums.txt"

# ==========================================================================
echo "== unreachable asset host is refused"
# ==========================================================================
BIN5="$WORK/bin5"
check_fails "unreachable base URL exits non-zero" \
  run_installer_at "http://127.0.0.1:1" "$BIN5" "$WORK/run5.log"
check "nothing installed when host is unreachable" test ! -e "$BIN5/synapse"

# ==========================================================================
echo "== non-https base URL is refused"
# ==========================================================================
BIN6="$WORK/bin6"
check_fails "plain http on a non-local host exits non-zero" \
  run_installer_at "http://example.com/synapse" "$BIN6" "$WORK/run6.log"
check "nothing installed for an untrusted URL" test ! -e "$BIN6/synapse"

# ==========================================================================
if [ "$fail" -ne 0 ]; then
  note ''
  note "install-check FAILED"
  exit 1
fi

echo ''
echo "install-check passed"
exit 0
