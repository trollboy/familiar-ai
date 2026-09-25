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

# Git exports these to every process a hook runs, and they override the
# directory a `git` invocation was pointed at. The steps below build fixture
# repositories and run `git add`, `git commit` and `git worktree add` inside
# them -- so run from the pre-push hook, without this, those fixtures write to
# the repository being pushed. Two pushes did exactly that: one committed
# "fixture", deleting every tracked file, the other added a file called
# `file`.
#
# Cleared here rather than only in the hook because this file is the single
# definition every caller runs, and because the hook a contributor has
# installed is whatever they symlinked, possibly from another checkout.
unset GIT_DIR GIT_WORK_TREE GIT_COMMON_DIR GIT_INDEX_FILE \
    GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_NAMESPACE

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
        # Doc-test names carry spaces ("src/lib.rs - module::fn (line 12)"),
        # so match everything between "test " and " ... FAILED".
        culprits="$(grep -oE '^test .+ \.\.\. FAILED$' "${log}" |
            sed 's/^test //; s/ \.\.\. FAILED$//' | head -20 | tr '\n' ' ')"
        if [ -n "${culprits}" ]; then
            note "--- gate: ${name} failures: ${culprits}"
        else
            # Not a named test failure — a compile error, a linter, or the
            # step's process itself dying. FAM-BUG-096: two red verdicts
            # carried neither a test name nor an error line, which left
            # nothing to act on; the exit status and the last lines the step
            # printed are the signature of that class.
            local lastline
            lastline="$(grep -m1 -E '^error' "${log}" | head -c 200)"
            [ -n "${lastline}" ] && note "--- gate: ${name} error: ${lastline}"
            note "--- gate: ${name} exit=${outcome} tail: $(tail -n 3 "${log}" | tr '\n' ' ' | head -c 300)"
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
step build  cargo build -p familiar-ai-daemon --bins
# Prove the production topology too: the supervised daemon is headless and
# the Tauri desktop is the sole tray owner (FAM-BUG-068).
step headless-daemon cargo build -p familiar-ai-daemon --no-default-features --bin familiar-ai-daemon
step clippy cargo clippy --workspace --all-targets -- -D warnings
step test   cargo test --workspace

if [ -n "${failed}" ]; then
    note "gate: FAILED —${failed}"
    exit 1
fi

note "gate: passed"
