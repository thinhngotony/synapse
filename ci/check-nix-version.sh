#!/usr/bin/env bash
# Assert the available Nix satisfies the same 2.24+ floor that src/nix.rs
# enforces at runtime.
#
# This duplicates the parser in src/nix.rs because the nix-flake CI leg runs
# *before* the Rust binary is built, so it cannot delegate to `synapse doctor`.
# Two implementations of one gate drift, so `--self-test` below re-checks the
# same version shapes as the Rust unit tests, and CI runs it. If the two ever
# disagree, the self-test fails rather than a reviewer having to notice.
#
# Usage:
#   ci/check-nix-version.sh              # check the nix on PATH
#   ci/check-nix-version.sh --self-test  # verify the parser itself
set -euo pipefail

MIN_MAJOR=2
MIN_MINOR=24

# Extract the Nix version from `nix --version` output.
#
# Distributions wrap their own product version in parentheses, and it can be
# *higher* than the Nix it ships:
#   nix (Nix) 2.35.1
#   nix (Determinate Nix 3.21.9) 2.34.8   <- 3.21.9 is NOT the Nix version
# Taking the first version-like token reads 3.21.9 and would accept a Nix below
# the floor. So: strip parenthesised groups, then take the LAST version token.
parse_version() {
    printf '%s' "$1" \
        | sed 's/([^)]*)//g' \
        | tr ' \t' '\n\n' \
        | grep -E '^[0-9]+\.[0-9]+' \
        | tail -1
}

# Print "major minor", or nothing if unparseable.
version_fields() {
    local version major rest minor
    version="$(parse_version "$1")"
    [ -n "$version" ] || return 0

    major="${version%%.*}"
    rest="${version#*.}"
    minor="${rest%%.*}"
    # Strip any non-digit suffix: 2.24.0pre20240101 -> minor stays 24.
    minor="$(printf '%s' "$minor" | grep -oE '^[0-9]+' || true)"

    [ -n "$major" ] && [ -n "$minor" ] || return 0
    printf '%s %s' "$major" "$minor"
}

# Exit 0 if the given version string meets the floor, 1 otherwise.
satisfies_floor() {
    local fields major minor
    fields="$(version_fields "$1")"
    [ -n "$fields" ] || return 1
    major="${fields%% *}"
    minor="${fields##* }"

    if [ "$major" -gt "$MIN_MAJOR" ]; then return 0; fi
    if [ "$major" -eq "$MIN_MAJOR" ] && [ "$minor" -ge "$MIN_MINOR" ]; then return 0; fi
    return 1
}

# ── Self-test ─────────────────────────────────────────────────────────────────
# Mirrors the cases in src/nix.rs's unit tests so the two parsers cannot drift.
self_test() {
    local failures=0

    expect_version() { # expect_version <input> <expected "maj min">
        local got
        got="$(version_fields "$1")"
        if [ "$got" = "$2" ]; then
            echo "  ok    parse '$1' -> $2"
        else
            echo "::error::parse '$1' expected '$2', got '$got'"
            failures=$((failures + 1))
        fi
    }

    expect_accept() {
        if satisfies_floor "$1"; then
            echo "  ok    accept '$1'"
        else
            echo "::error::'$1' should satisfy the floor but was rejected"
            failures=$((failures + 1))
        fi
    }

    expect_reject() {
        if satisfies_floor "$1"; then
            echo "::error::'$1' is below the floor but was ACCEPTED"
            failures=$((failures + 1))
        else
            echo "  ok    reject '$1'"
        fi
    }

    echo "== parser self-test =="
    expect_version "nix (Nix) 2.35.1"                        "2 35"
    expect_version "nix (Nix) 2.24.0pre20240101_abcdef"       "2 24"
    expect_version "2.28.4"                                   "2 28"
    # The case that motivated this: the parenthesised product version is higher
    # than the real Nix version.
    expect_version "nix (Determinate Nix 3.21.9) 2.34.8"      "2 34"
    expect_version "nix (Determinate Nix 3.21.9) 2.18.1"      "2 18"

    echo "== floor decisions =="
    expect_accept "nix (Nix) 2.35.1"
    expect_accept "nix (Nix) 2.24.0"
    expect_accept "nix (Determinate Nix 3.21.9) 2.34.8"
    expect_reject "nix (Nix) 2.23.4"
    expect_reject "nix (Nix) 1.11.16"
    # Regression guard: a too-old Nix inside a newer Determinate release. The
    # previous first-token parser read 3.21 here and accepted it.
    expect_reject "nix (Determinate Nix 3.21.9) 2.18.1"

    echo "== unparseable input =="
    expect_reject ""
    expect_reject "command not found"

    if [ "$failures" -ne 0 ]; then
        echo "::error::parser self-test failed ($failures case(s))"
        return 1
    fi
    echo "parser self-test passed"
    return 0
}

# ── Main ──────────────────────────────────────────────────────────────────────

if [ "${1:-}" = "--self-test" ]; then
    self_test
    exit $?
fi

raw="$(nix --version)"
echo "$raw"

fields="$(version_fields "$raw")"
if [ -z "$fields" ]; then
    echo "::error::could not parse a version out of: $raw"
    exit 1
fi
major="${fields%% *}"
minor="${fields##* }"

if satisfies_floor "$raw"; then
    echo "nix ${major}.${minor} satisfies the ${MIN_MAJOR}.${MIN_MINOR}+ floor"
    exit 0
fi

echo "::error::nix ${major}.${minor} is below the required ${MIN_MAJOR}.${MIN_MINOR}"
exit 1
