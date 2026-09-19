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

# `familiar-ai gate run` sets GATE_SUMMARY to a path and reads the one-line
# outcomes back from it. Writing them to a file rather than having the caller
# capture this script's whole stdout keeps megabytes of test output streaming
# straight to the terminal, where a hook's non-blocking stdout can take it at
# its own pace, instead of being buffered and relayed in one oversized write.
note() {
    echo "$1"
    if [ -n "${GATE_SUMMARY:-}" ]; then
        echo "$1" >> "${GATE_SUMMARY}"
    fi
}

step() {
    local name="$1"
    shift
    note "--- gate: ${name}"

    # Tee rather than capture: the output still streams to the terminal at
    # whatever pace it can take, and a copy is kept so a red verdict can say
    # what actually failed. "test FAILED" with no test name is honest and
    # useless — you cannot act on it without re-running, and a flaky failure
    # will not reproduce.
    local log
    log="$(mktemp -t familiar-ai-gate-step.XXXXXX)"
    # errexit and pipefail would kill the script on the failing pipeline
    # before PIPESTATUS could be read, aborting the gate at its first red
    # step instead of running every step and reporting each one.
    set +e
    "$@" 2>&1 | tee "${log}"
    local outcome=${PIPESTATUS[0]}
    set -e

    if [ "${outcome}" -ne 0 ]; then
        note "--- gate: ${name} FAILED"
        # Name the failures in the verdict, capped so one catastrophic run
        # cannot write an unbounded row into the ledger.
        local culprits
        culprits="$(grep -oE '^test [^ ]+ \.\.\. FAILED' "${log}" |
            sed 's/^test //; s/ \.\.\. FAILED$//' | head -20 | tr '\n' ' ')"
        if [ -n "${culprits}" ]; then
            note "--- gate: ${name} failures: ${culprits}"
        else
            # Not a test failure — a compile error, a linter, a killed step.
            local lastline
            lastline="$(grep -m1 -E '^error' "${log}" | head -c 200)"
            [ -n "${lastline}" ] && note "--- gate: ${name} error: ${lastline}"
        fi
        rm -f "${log}"
        # Record and keep going: one red step should not hide the others, but
        # any red step fails the gate.
        failed="${failed} ${name}"
        return 0
    fi
    rm -f "${log}"
    note "--- gate: ${name} ok"
}

step fmt    cargo fmt --all -- --check
step clippy cargo clippy --workspace --all-targets -- -D warnings
step test   cargo test --workspace --no-default-features

if [ -n "${failed}" ]; then
    note "gate: FAILED —${failed}"
    exit 1
fi

note "gate: passed"
