#!/usr/bin/env bash
# Integration test driver for the install/update/rollback/uninstall lifecycle.
#
# Each stage asserts *outcomes*, not just exit codes:
#   - fresh-install: state.json has the package with the right version
#   - update: state.json shows new version AND preserves the old one in history
#   - rollback: version is reverted in state.json, rollback target discarded
#   - uninstall: state.json is empty, managed shell block is gone
#
# Usage: ci/lifecycle.sh <stage>
#
# Stages may be run sequentially with XDG_CONFIG_HOME pointing at the same
# directory; each stage reads the state left by the previous one.
set -euo pipefail

STAGE="${1:?usage: lifecycle.sh <fresh-install|update|rollback|uninstall>}"
SYNAPSE="${SYNAPSE_BIN:-./target/release/synapse}"
if [ ! -x "$SYNAPSE" ]; then
    printf '::error::%s is not an executable file\n' "$SYNAPSE" >&2
    printf '  build it first: cargo build --release\n' >&2
    exit 1
fi
: "${XDG_CONFIG_HOME:?XDG_CONFIG_HOME must be set so stages share state}"
export SYNAPSE_PROFILE="${SYNAPSE_PROFILE:-$XDG_CONFIG_HOME/synapse-profile}"
export SYNAPSE_FLAKE_DIR="${SYNAPSE_FLAKE_DIR:-$PWD}"
NIX_BIN="${NIX_BIN:-$(command -v nix || true)}"
if [ -z "$NIX_BIN" ] && [ -x /nix/var/nix/profiles/default/bin/nix ]; then
  NIX_BIN=/nix/var/nix/profiles/default/bin/nix
fi
: "${NIX_BIN:?nix is required for lifecycle profile assertions}"

STATE_FILE="$XDG_CONFIG_HOME/synapse/state.json"

fail=0
check() { # check <label> <expr>
  if eval "$2"; then
    echo "  ok    $1"
  else
    printf '::error::%s\n' "$1" >&2
    fail=1
  fi
}

# ── helpers ────────────────────────────────────────────────────────────────

state_version() { # state_version <pkg>
  jq -r --arg p "$1" '.packages[$p].version // ""' "$STATE_FILE" 2>/dev/null
}
state_history_len() { # state_history_len <pkg>
  jq --arg p "$1" '(.packages[$p].history // []) | length' "$STATE_FILE" 2>/dev/null
}
state_history_version() { # state_history_version <pkg> <index>
  jq -r --arg p "$1" --argjson i "$2" '.packages[$p].history[$i].version // ""' \
    "$STATE_FILE" 2>/dev/null
}

# ── stages ─────────────────────────────────────────────────────────────────

stage_fresh_install() {
  echo "=== fresh-install ==="

  # Seed a distinct old closure so executable output proves the profile really
  # transitions old → new → old across update and rollback.
  mkdir -p "$XDG_CONFIG_HOME/synapse" "$XDG_CONFIG_HOME/herdr/bin"
  cat > "$XDG_CONFIG_HOME/herdr/bin/herdr" <<'EOF'
#!/bin/sh
echo 'herdr 0.7.4'
EOF
  chmod 755 "$XDG_CONFIG_HOME/herdr/bin/herdr"
  store_path="$("$NIX_BIN" store add-path "$XDG_CONFIG_HOME/herdr")"
  "$NIX_BIN" profile install --profile "$SYNAPSE_PROFILE" "$store_path"
  jq -n --arg store "$store_path" '
    {packages:{herdr:{version:"0.7.4",installed_at:1785542400,store_path:$store}}}
  ' > "$STATE_FILE"

  # Smoke: read-only commands work with this state.
  "$SYNAPSE" list >/dev/null
  "$SYNAPSE" status >/dev/null
  check "state.json is valid JSON" "jq '.' \"$STATE_FILE\" >/dev/null 2>&1"

  actual="$(state_version herdr)"
  check "herdr recorded as 0.7.4" "[ \"$actual\" = '0.7.4' ]"
  check "no history on fresh install" "[ \"$(state_history_len herdr)\" -eq 0 ]"
  check "old executable prints 0.7.4" \
    "\"$SYNAPSE_PROFILE/bin/herdr\" --version | grep -q '0.7.4'"
  check "log created after list" "\"$SYNAPSE\" log >/dev/null"

  echo "fresh-install complete"
}

stage_update() {
  echo "=== update ==="

  # Verify we have a prior version to update from.
  before="$(state_version herdr)"
  check "herdr present before update" "[ -n \"$before\" ]"

  "$SYNAPSE" update herdr

  # Assert the outcome the update command is contractually obligated to produce.
  check "version bumped to 0.7.5" "[ \"$(state_version herdr)\" = '0.7.5' ]"
  check "old version in history" "[ \"$(state_history_version herdr 0)\" = '$before' ]"
  check "history length is 1" "[ \"$(state_history_len herdr)\" -eq 1 ]"
  check "state.json still valid" "jq '.' \"$STATE_FILE\" >/dev/null 2>&1"
  check "updated executable prints 0.7.5" \
    "\"$SYNAPSE_PROFILE/bin/herdr\" --version | grep -q '0.7.5'"

  # synapse list must reflect the updated version.
  list_out="$("$SYNAPSE" list)"
  printf '%s' "$list_out" | grep -q '0.7.5'
  check "list shows new version" "printf '%s' \"$list_out\" | grep -q '0.7.5'"

  echo "update complete"
}

stage_rollback() {
  echo "=== rollback ==="

  before="$(state_version herdr)"
  before_hist="$(state_history_version herdr 0)"

  check "version before rollback is 0.7.5" "[ \"$before\" = '0.7.5' ]"
  check "history has 0.7.4" "[ \"$before_hist\" = '0.7.4' ]"

  # Rollback must repoint the dedicated profile to the recorded store path,
  # not merely change state.json.
  "$SYNAPSE" rollback herdr

  after="$(state_version herdr)"

  # Outcome assertions — not just "command ran".
  check "version reverted to 0.7.4" "[ \"$after\" = '0.7.4' ]"
  check "bad version gone from history" \
    "! jq -e '.packages.herdr.history[] | select(.version == \"0.7.5\")' \"$STATE_FILE\" >/dev/null 2>&1"
  check "state.json still valid" "jq '.' \"$STATE_FILE\" >/dev/null 2>&1"
  check "rolled-back executable prints 0.7.4" \
    "\"$SYNAPSE_PROFILE/bin/herdr\" --version | grep -q '0.7.4'"

  echo "rollback complete"
}

stage_uninstall() {
  echo "=== uninstall ==="

  # Set up a fake HOME so we can verify the managed shell block is removed.
  FAKE_HOME="${XDG_CONFIG_HOME}-home"
  rm -rf "$FAKE_HOME" && mkdir -p "$FAKE_HOME"
  printf '# USER\n' > "$FAKE_HOME/.bashrc"
  HOME="$FAKE_HOME" SHELL=/bin/bash XDG_CONFIG_HOME="$XDG_CONFIG_HOME" \
    "$SYNAPSE" setup-shell >/dev/null 2>&1 || true

  check "managed block in rc before uninstall" \
    "grep -q '>>> synapse >>>' \"$FAKE_HOME/.bashrc\""

  HOME="$FAKE_HOME" XDG_CONFIG_HOME="$XDG_CONFIG_HOME" \
    "$SYNAPSE" uninstall --all 2>&1 | head -10

  # Outcome: state is empty.
  pkg_count="$(jq '.packages | length' "$STATE_FILE" 2>/dev/null || echo -1)"
  check "state.json is empty after uninstall" "[ \"$pkg_count\" -eq 0 ]"
  check "uninstalled executable removed from profile" "[ ! -e \"$SYNAPSE_PROFILE/bin/herdr\" ]"

  # Outcome: managed shell block removed. The block must actually be gone, not
  # just that setup-shell might have failed — check the file content directly.
  check "managed block removed from rc" \
    "! grep -q '>>> synapse >>>' \"$FAKE_HOME/.bashrc\""

  # User content must survive.
  check "user content preserved after uninstall" \
    "grep -q '# USER' \"$FAKE_HOME/.bashrc\""

  check "state.json still valid JSON" "jq '.' \"$STATE_FILE\" >/dev/null 2>&1"

  echo "uninstall complete"
}

# ── dispatch ───────────────────────────────────────────────────────────────

case "$STAGE" in
  fresh-install) stage_fresh_install ;;
  update)        stage_update ;;
  rollback)      stage_rollback ;;
  uninstall)     stage_uninstall ;;
  *)
    echo "::error::unknown stage: $STAGE"
    echo "valid stages: fresh-install, update, rollback, uninstall"
    exit 1
    ;;
esac

if [ "$fail" -ne 0 ]; then
  echo "--- state.json ---" >&2
  jq '.' "$STATE_FILE" >&2 2>/dev/null || cat "$STATE_FILE" >&2
fi

exit "$fail"
