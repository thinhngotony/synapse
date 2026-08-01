#!/usr/bin/env bash
# Every read-only command must work with no config present and no network.
#
# Usage: ci/offline-commands.sh <path-to-synapse>
set -euo pipefail

SYNAPSE="${1:?usage: offline-commands.sh <path-to-synapse>}"
: "${XDG_CONFIG_HOME:=$(mktemp -d)}"
export XDG_CONFIG_HOME

fail=0
note() { printf '%s\n' "$*" >&2; }

# --- Commands that must exit 0 even with no state at all -------------------
for cmd in version status list log; do
  echo "--- synapse $cmd (empty config)"
  if ! "$SYNAPSE" "$cmd" >/dev/null; then
    note "::error::synapse $cmd failed on a fresh config"
    fail=1
  fi
done

# `doctor` intentionally reports problems, so a non-zero exit is not a failure.
# What matters is that it runs and produces output rather than crashing.
echo "--- synapse doctor (empty config)"
doctor_out="$("$SYNAPSE" doctor 2>&1 || true)"
printf '%s\n' "$doctor_out"
if [ -z "$doctor_out" ]; then
  note "::error::synapse doctor produced no output"
  fail=1
fi

# --- Read-only commands must not create a lock ------------------------------
if [ -e "$XDG_CONFIG_HOME/synapse/.lock" ]; then
  note "::error::a read-only command left a lock file behind"
  fail=1
fi

# --- Version output must carry real build info ------------------------------
version_out="$("$SYNAPSE" version)"
printf '%s\n' "$version_out"
for field in "synapse " "commit:" "target:"; do
  if ! printf '%s' "$version_out" | grep -q "$field"; then
    note "::error::version output missing '$field'"
    fail=1
  fi
done

# --- Populated state must render ------------------------------------------
mkdir -p "$XDG_CONFIG_HOME/synapse"
cat > "$XDG_CONFIG_HOME/synapse/state.json" <<'JSON'
{"packages":{"herdr":{"version":"0.7.5","installed_at":1785542400}}}
JSON

echo "--- synapse list (populated)"
list_out="$("$SYNAPSE" list)"
printf '%s\n' "$list_out"
printf '%s' "$list_out" | grep -q 'herdr' || {
  note "::error::list did not show an installed package"
  fail=1
}
printf '%s' "$list_out" | grep -q '2026-08-01' || {
  note "::error::list did not render the install timestamp"
  fail=1
}

# --- Malformed state must fail loudly, not silently ------------------------
printf 'not json at all' > "$XDG_CONFIG_HOME/synapse/state.json"
echo "--- synapse list (corrupt state)"
if "$SYNAPSE" list >/dev/null 2>&1; then
  note "::error::list silently accepted a corrupt state.json"
  fail=1
else
  echo "corrupt state rejected, as expected"
fi

exit "$fail"
