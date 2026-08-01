#!/usr/bin/env bash
# Run every scheduler-invoked command path under a minimal environment.
#
# Why this exists: launchd, systemd, and cron do not give a job the environment
# an interactive shell has. A launchd probe on macOS reports exactly:
#
#   HOME, USER, LOGNAME, SHELL, PWD=/, TMPDIR, SSH_AUTH_SOCK,
#   PATH=/usr/bin:/bin:/usr/sbin:/sbin
#
# Note what is absent: no XDG_*, and no Nix profile directory on PATH. A bug in
# this class passes every interactive test and then fails silently at 02:00 —
# which is what happened: every `Command::new("nix")` resolved via PATH, so the
# daily auto-update failed with ENOENT while working perfectly by hand.
#
# Usage: ci/scheduler-env.sh [path-to-synapse]
set -uo pipefail

# An empty first argument is a caller mistake; treat it as unset rather than
# resolving to a nonsense path and failing much later with a confusing error.
SYNAPSE="${1:-}"
[ -n "$SYNAPSE" ] || SYNAPSE="./target/release/synapse"

if [ ! -x "$SYNAPSE" ]; then
    printf '::error::%s is not an executable file\n' "$SYNAPSE" >&2
    printf '  build it first: cargo build --release\n' >&2
    exit 1
fi

# Absolute, so the `cd /` invocations below still find it.
SYNAPSE="$(cd "$(dirname "$SYNAPSE")" && pwd)/$(basename "$SYNAPSE")"

# The PATH a scheduler actually provides. Deliberately does NOT include the Nix
# profile bin directory.
MINIMAL_PATH="/usr/bin:/bin:/usr/sbin:/sbin"

fail=0
note() { printf '%s\n' "$*" >&2; }

# ── Precondition: the check must be able to fail ────────────────────────────
#
# If `nix` were reachable on MINIMAL_PATH, every assertion below would pass
# regardless of whether the resolution logic works. Prove it is absent first.
echo "== precondition =="
if env -i PATH="$MINIMAL_PATH" sh -c 'command -v nix' >/dev/null 2>&1; then
    note "::error::nix is on $MINIMAL_PATH — this check would pass vacuously."
    note "  The point is to exercise the absolute-path fallback. Cannot verify."
    exit 1
fi
echo "  ok    nix is absent from $MINIMAL_PATH (fallback will be exercised)"

# Sanity: nix must exist *somewhere*, or there is nothing to fall back to.
if ! command -v nix >/dev/null 2>&1 && [ ! -x /nix/var/nix/profiles/default/bin/nix ]; then
    note "::error::nix not found on the normal PATH either — nothing to resolve."
    exit 1
fi
echo "  ok    nix exists outside the minimal PATH"

# ── Harness ─────────────────────────────────────────────────────────────────

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# run_minimal <label> <expect-exit> <args...>
run_minimal() {
    local label="$1" expect="$2"; shift 2
    local out rc

    out="$(env -i \
        PATH="$MINIMAL_PATH" \
        HOME="$HOME" \
        XDG_CONFIG_HOME="$WORK/config" \
        "$SYNAPSE" "$@" 2>&1)"
    rc=$?

    # A scheduled job runs from /, so exercise that too rather than inheriting
    # the repo directory, which would mask CWD assumptions.
    local out_root rc_root
    out_root="$(cd / && env -i \
        PATH="$MINIMAL_PATH" \
        HOME="$HOME" \
        XDG_CONFIG_HOME="$WORK/config" \
        "$SYNAPSE" "$@" 2>&1)"
    rc_root=$?

    local ok=1

    if [ "$rc" -ne "$expect" ]; then
        note "::error::$label: exit $rc from repo dir, expected $expect"
        note "$out"
        ok=0
    fi
    if [ "$rc_root" -ne "$expect" ]; then
        note "::error::$label: exit $rc_root from PWD=/, expected $expect"
        note "$out_root"
        ok=0
    fi

    # ENOENT leaking through means a binary was looked up on PATH and not found.
    for text in "$out" "$out_root"; do
        case "$text" in
            *"No such file or directory"*|*"os error 2"*|*ENOENT*)
                note "::error::$label: a binary was not found — PATH assumption"
                note "$text"
                ok=0
                ;;
        esac
    done

    if [ "$ok" -eq 1 ]; then
        echo "  ok    $label"
    else
        fail=1
    fi
}

# ── Read-only commands: must work with nothing installed ────────────────────
echo "== read-only commands =="
run_minimal "version" 0 version
run_minimal "status"  0 status
run_minimal "list"    0 list
run_minimal "log"     0 log
run_minimal "doctor"  0 doctor

# ── Commands that touch state ───────────────────────────────────────────────
#
# With no packages installed these exit 0 after reporting there is nothing to
# do. What matters is that they get far enough to say so rather than dying on a
# missing binary or an unresolvable flake directory.
echo "== state commands (empty state) =="
run_minimal "update --all"   0 update --all
run_minimal "rollback"       0 rollback
run_minimal "auto-update now" 0 auto-update now

# ── Same commands with a populated state ────────────────────────────────────
#
# This is the path that actually invokes nix, so it is where a PATH or
# flake-location bug surfaces.
echo "== state commands (populated state) =="
mkdir -p "$WORK/config/synapse"
cat > "$WORK/config/synapse/state.json" <<'JSON'
{"packages":{"herdr":{"version":"0.7.5","installed_at":1785542400}}}
JSON

run_minimal "update --all (installed)"    0 update --all
run_minimal "auto-update now (installed)" 0 auto-update now
run_minimal "list (installed)"            0 list

# Rollback has no history here, so it reports that and exits 0.
run_minimal "rollback (no history)" 0 rollback

# ── The interactive installer must refuse, not crash ────────────────────────
#
# A scheduler has no TTY. `install` should explain that rather than failing with
# a raw "Device not configured (os error 6)".
echo "== TTY-requiring command =="
install_out="$(env -i PATH="$MINIMAL_PATH" HOME="$HOME" \
    XDG_CONFIG_HOME="$WORK/config" "$SYNAPSE" install 2>&1 </dev/null)"
install_rc=$?

if [ "$install_rc" -eq 0 ]; then
    note "::error::install exited 0 with no TTY; it should refuse"
    fail=1
elif printf '%s' "$install_out" | grep -q "interactive terminal"; then
    echo "  ok    install refuses without a TTY and says why"
else
    note "::error::install failed without explaining that it needs a terminal:"
    note "$install_out"
    fail=1
fi

# ── No HOME at all ──────────────────────────────────────────────────────────
#
# HOME is normally set by launchd, but `su`-style invocations drop it. State must
# land in the real home directory, derived from the passwd database.
#
# Design notes:
# - We omit BOTH HOME and XDG_CONFIG_HOME. If XDG_CONFIG_HOME is present,
#   dirs_from_env() returns early from the XDG branch and never touches the
#   HOME/passwd logic at all, making this an empty test.
# - We use `auto-update enable`, which unconditionally calls write_config()
#   regardless of whether packages are installed. `update --all` with nothing
#   installed exits before writing anything.
# - We assert positively that the yaml appeared under the passwd home. A probe for
#   known-bad paths (/tmp/.config, /tmp/synapse, ...) can only catch the exact
#   wrong paths it lists; a whitelist catches every wrong answer.
echo "== no HOME =="

# Derive the passwd home without using HOME so we are not just reading back the
# variable under test.
PASSWD_HOME="$(
    python3 -c 'import pwd,os; print(pwd.getpwuid(os.getuid()).pw_dir)' 2>/dev/null ||
    getent passwd "$(id -un)" 2>/dev/null | cut -d: -f6 ||
    dscl . -read "/Users/$(id -un)" NFSHomeDirectory 2>/dev/null | awk '{print $2}'
)"

if [ -z "$PASSWD_HOME" ] || [ ! -d "$PASSWD_HOME" ]; then
    note "::error::could not resolve the passwd home; cannot verify no-HOME path"
    fail=1
else
    echo "  ok    passwd home: $PASSWD_HOME"
    EXPECTED_YAML="$PASSWD_HOME/.config/synapse/auto-update.yaml"

    # Move any pre-existing config aside so we observe only what this run writes.
    STASH=''
    if [ -e "$EXPECTED_YAML" ]; then
        STASH="$(mktemp)"
        cp "$EXPECTED_YAML" "$STASH"
        rm -f "$EXPECTED_YAML"
    fi

    # Write through dirs_from_env with neither HOME nor XDG_CONFIG_HOME set.
    write_out="$(env -i PATH="$MINIMAL_PATH" "$SYNAPSE" auto-update enable 2>&1)"
    write_rc=$?
    # disable to clean up, ignoring errors
    env -i PATH="$MINIMAL_PATH" HOME="$PASSWD_HOME" "$SYNAPSE" auto-update disable \
        >/dev/null 2>&1 || true

    if [ "$write_rc" -ne 0 ]; then
        note "::error::auto-update enable failed with HOME unset:"
        note "$write_out"
        fail=1
    else
        echo "  ok    auto-update enable returned 0 with HOME unset"
    fi

    # Positive assertion: yaml must have appeared under the passwd home.
    if [ -e "$EXPECTED_YAML" ]; then
        echo "  ok    config yaml at $EXPECTED_YAML"
    else
        note "::error::auto-update.yaml not found at $EXPECTED_YAML"
        note "  It should have landed there because HOME was unset and"
        note "  dirs_from_env() must fall back to the passwd home."
        # Show where it actually went, if anywhere.
        actual="$(find /tmp "$PASSWD_HOME" / -maxdepth 6 -name 'auto-update.yaml' \
            2>/dev/null | head -5 || true)"
        [ -n "$actual" ] && note "  Found at: $actual" || note "  Not found anywhere"
        fail=1
    fi

    # Belt-and-suspenders: also assert no plausible wrong paths have the file.
    for wrong in /tmp/.config/synapse/auto-update.yaml \
                 /tmp/synapse/auto-update.yaml \
                 /.config/synapse/auto-update.yaml \
                 .config/synapse/auto-update.yaml; do
        if [ -e "$wrong" ]; then
            note "::error::config yaml appeared at $wrong — dirs_from_env() fell through to a wrong fallback"
            fail=1
        fi
    done

    # Restore whatever was there before.
    if [ -n "$STASH" ]; then
        mkdir -p "$(dirname "$EXPECTED_YAML")"
        cp "$STASH" "$EXPECTED_YAML"
        rm -f "$STASH"
    fi
fi

if [ "$fail" -ne 0 ]; then
    echo "::error::scheduler-environment check failed"
    exit 1
fi
echo "scheduler-environment check passed"
