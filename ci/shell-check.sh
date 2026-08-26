#!/usr/bin/env bash
# Verify shell integration for one shell: rc validity, PATH effect, completion
# loading, idempotency, and preservation of the user's existing content.
#
# Deliberately asserts that completions *load*, not merely that the rc parses:
# the rc guards its source with `[ -r file ]`, so a missing completion script
# is a silent no-op that a syntax check would happily pass.
#
# Usage: ci/shell-check.sh <bash|zsh|fish> <fake-home>
set -euo pipefail

SHELL_NAME="${1:?usage: shell-check.sh <bash|zsh|fish> <fake-home>}"
FAKE="${2:?usage: shell-check.sh <bash|zsh|fish> <fake-home>}"
SYNAPSE="${SYNAPSE_BIN:-./target/release/synapse}"

if ! command -v "$SHELL_NAME" >/dev/null; then
  echo "::error::$SHELL_NAME is not installed on this runner"
  exit 1
fi
SHELL_PATH="$(command -v "$SHELL_NAME")"

fail=0
note() { printf '%s\n' "$*" >&2; }
check() { # check <description> <condition-exit-code>
  if [ "$2" -eq 0 ]; then echo "  ok    $1"; else note "::error::$1"; fail=1; fi
}

rm -rf "$FAKE"
mkdir -p "$FAKE"

case "$SHELL_NAME" in
  bash) RC="$FAKE/.bashrc" ;;
  zsh)  RC="$FAKE/.zshrc" ;;
  fish) RC="$FAKE/.config/fish/config.fish"; mkdir -p "$(dirname "$RC")"; touch "$RC" ;;
esac

# A pre-existing line we must never clobber.
printf '%s\n' "# PREEXISTING_MARKER" >> "$RC"

run_setup() {
  HOME="$FAKE" SHELL="$SHELL_PATH" XDG_CONFIG_HOME="$FAKE/.config" \
    "$SYNAPSE" setup-shell
}

echo "== $SHELL_NAME: first run"
run_setup
echo "== $SHELL_NAME: second run (idempotency)"
second_output="$(run_setup)"

# Assertion commands deliberately return non-zero before `check` records them.
set +e
# --- rc file is valid syntax for this shell --------------------------------
"$SHELL_NAME" -n "$RC" >/dev/null 2>&1
check "rc parses as valid $SHELL_NAME" $?

# --- user content survived --------------------------------------------------
grep -q 'PREEXISTING_MARKER' "$RC"
check "pre-existing user content preserved" $?

# --- exactly one managed block ---------------------------------------------
markers="$(grep -c '>>> synapse >>>' "$RC" || true)"
[ "$markers" -eq 1 ]
check "exactly one managed block (found $markers)" $?

# --- second run reported no change ----------------------------------------
printf '%s' "$second_output" | grep -q 'already configured'
check "second run reported 'already configured'" $?

# --- no temp file left behind ---------------------------------------------
ls "$FAKE"/.*synapse-tmp >/dev/null 2>&1
tmp_status=$?
[ "$tmp_status" -ne 0 ]
check "no .synapse-tmp left behind" $?

# --- completions were generated where the rc expects them ------------------
case "$SHELL_NAME" in
  bash) COMP="$FAKE/.local/share/bash-completion/completions/synapse.bash" ;;
  zsh)  COMP="$FAKE/.local/share/zsh/site-functions/_synapse" ;;
  fish) COMP="$FAKE/.config/fish/completions/synapse.fish" ;;
esac
[ -r "$COMP" ]
check "completion script exists at $COMP" $?

grep -q '.local/share/synapse/profile/bin' "$RC"
check "managed profile PATH is configured" $?

# --- shell-specific behaviour ---------------------------------------------
case "$SHELL_NAME" in
  bash)
    # PATH must gain the profile exactly once, even after sourcing twice.
    count="$(HOME="$FAKE" PATH=/usr/bin:/bin bash -c '
      source "$HOME/.bashrc" >/dev/null 2>&1
      source "$HOME/.bashrc" >/dev/null 2>&1
      printf "%s" "$PATH" | tr ":" "\n" | grep -c "nix-profile/bin"
    ' || true)"
    [ "$count" = "1" ]
    check "PATH has exactly 1 nix-profile entry after 2 sources (got $count)" $?

    # The real proof that completions load, not just that the file exists.
    HOME="$FAKE" bash -c 'source "$HOME/.bashrc" >/dev/null 2>&1; type -t _synapse' >/dev/null 2>&1
    check "bash completion function registered" $?
    ;;

  zsh)
    HOME="$FAKE" zsh -c '
      source "$HOME/.zshrc" >/dev/null 2>&1
      case ":$PATH:" in *":$HOME/.nix-profile/bin:"*) exit 0 ;; *) exit 1 ;; esac
    ' >/dev/null 2>&1
    check "zsh PATH contains nix-profile" $?

    zsh -n "$COMP" >/dev/null 2>&1
    check "zsh completion script parses" $?

    # fpath must actually include the directory holding _synapse.
    HOME="$FAKE" zsh -c '
      source "$HOME/.zshrc" >/dev/null 2>&1
      for d in $fpath; do
        [ "$d" = "$HOME/.local/share/zsh/site-functions" ] && exit 0
      done
      exit 1
    ' >/dev/null 2>&1
    check "zsh fpath includes the completion dir" $?
    ;;

  fish)
    grep -q 'fish_add_path' "$RC"
    check "fish rc uses fish_add_path" $?

    ! grep -q 'export PATH' "$RC"
    check "no POSIX syntax leaked into fish rc" $?

    # fish autoloads its completions dir, so existence in the right place is
    # what makes them load; confirm fish itself accepts the script.
    fish -n "$COMP" >/dev/null 2>&1
    check "fish completion script parses" $?
    ;;
esac

if [ "$fail" -ne 0 ]; then
  echo "--- rc contents for debugging ---" >&2
  cat "$RC" >&2
fi

exit "$fail"
