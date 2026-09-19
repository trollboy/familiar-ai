#!/usr/bin/env bash
#
# THE definition of verification for this repository (PRD-099).
#
# Every caller runs this file and nothing else: the GitHub Actions workflow,
# the compose `test` service, and a contributor at a terminal. If you are
# about to add a verification step to a workflow, a compose command or the
# README, add it here instead. `gate_contract.rs` fails the build when a
# second list of steps appears anywhere else, because two lists that are
# supposed to match eventually will not.
#
# What each step does, and which features it enables, belongs to PRD-092.
# This file's job is that the steps run at all, in one place, for everyone.

set -euo pipefail

failed=""

step() {
    local name="$1"
    shift
    echo "--- gate: ${name}"
    if ! "$@"; then
        # Record and keep going: one red step should not hide the others, but
        # any red step fails the gate.
        echo "--- gate: ${name} FAILED"
        failed="${failed} ${name}"
        return 0
    fi
    echo "--- gate: ${name} ok"
}

step fmt    cargo fmt --all -- --check
step clippy cargo clippy --workspace --all-targets -- -D warnings
step test   cargo test --workspace --no-default-features

if [ -n "${failed}" ]; then
    echo "gate: FAILED —${failed}"
    exit 1
fi

echo "gate: passed"
