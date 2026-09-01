#!/bin/bash
# FAM-BUG-030 companion diagnosis: answer WHY `cargo test --workspace` takes
# 45+ minutes on the Mac. The 20260901T093435Z run proved the 45 minutes are
# dominated by cargo BUILD time, not test execution (storage suite tested in
# 15.60s; no 180 s stall — "very slow, not stuck"), so build pathology and the
# original hang are now two separate questions. One unattended run separates
# them cleanly:
#
#   phase 1 (build): `cargo test --workspace --no-run`, wall-clocked, with
#     cargo's own per-crate timings when the installed cargo supports them,
#     macOS suspect snapshots before/after (Spotlight, memory, thermal, disk,
#     APFS snapshots, concurrent processes), and a periodic top-CPU snapshot
#     so we can see WHO is eating the machine mid-build (rustc? dsymutil?
#     mds_stores? XProtect? Codex?). No stall detector — slow is the expected
#     finding — just a generous 90 m cap.
#
#   phase 2 (tests): `cargo test --workspace` on the now-warm build, which
#     isolates test execution. Same 180 s stall detector + sample/lsof capture
#     as diagnose-suite-hang.sh, 20 m cap. A clean completion here at a HEAD
#     containing fix candidate 5f517db is the validation FAM-BUG-030 waits on.
#
# Division of labor: diagnose-suite-hang.sh answers "is it stuck";
# this script answers "why is it slow" (the hang check rides along in phase 2).
#
# macOS-oriented, but every Mac-only command is guarded so it degrades on
# Linux (where it doubles as the control measurement).
#
# Usage:   ./scripts/diagnose-mac-build-speed.sh
# Result:  docs/diagnostics/mac-build-speed-<timestamp>.txt committed to main
#          (plus a per-crate timings HTML beside it when cargo produced one).
set -u

# Self-update before diagnosing: the 20260901T093435Z run diagnosed a HEAD
# that predated the fix under test. Pull, then re-exec the (possibly new)
# script exactly once — bash reads scripts incrementally, so continuing after
# the file changed underneath us would corrupt execution.
if [ -z "${FAMILIAR_DIAGNOSE_PULLED:-}" ]; then
  git pull --ff-only -q || echo "== WARNING: git pull failed; diagnosing current checkout" >&2
  FAMILIAR_DIAGNOSE_PULLED=1 exec "$0" "$@"
fi

BUILD_MAX_MINUTES=90       # generous cap; the point is to measure, not abort
SNAPSHOT_SECONDS=600       # top-CPU snapshot cadence during the build
TEST_STALL_SECONDS=180     # phase 2: no new output for this long = stalled
TEST_MAX_MINUTES=20        # phase 2 absolute cap (compile is warm by then)
FIX_CANDIDATE=5f517db2b043fe0c24050664a01b6ba1b4fdbf98  # process_group(0) + group SIGKILL in daemon integration.rs
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT_DIR="docs/diagnostics"
REPORT="$OUT_DIR/mac-build-speed-$STAMP.txt"
BUILD_LOG="$(mktemp "${TMPDIR:-/tmp}/familiar-build-log.XXXXXX")"
TEST_LOG="$(mktemp "${TMPDIR:-/tmp}/familiar-test-log.XXXXXX")"
mkdir -p "$OUT_DIR"

note() { echo "== $*" | tee -a "$REPORT"; }
have() { command -v "$1" >/dev/null 2>&1; }

# One labeled capture of every macOS build-slowness suspect. Guarded so the
# same script runs on Linux (missing tools are simply skipped).
mac_snapshot() {
  {
    echo "-- suspect snapshot ($1) $(date -u '+%H:%M:%SZ') --"
    echo "- df -h . (full disk makes APFS crawl):"
    df -h .
    if have mdutil; then
      echo "- mdutil -s / (Spotlight volume indexing state):"
      mdutil -s / 2>&1
    fi
    echo "- Spotlight exclusion markers (per-folder Privacy exclusions are root-only; watch mds_stores CPU in the mid-build snapshots instead):"
    echo "  repo/.metadata_never_index: $([ -e .metadata_never_index ] && echo present || echo absent)"
    echo "  target/.metadata_never_index: $([ -e target/.metadata_never_index ] && echo present || echo absent)"
    echo "  target/CACHEDIR.TAG: $([ -e target/CACHEDIR.TAG ] && echo present || echo absent) (Spotlight does NOT honor CACHEDIR.TAG)"
    if have vm_stat; then
      echo "- vm_stat (pageouts/swapins rising across snapshots = memory pressure):"
      vm_stat 2>&1
    fi
    if have memory_pressure; then
      echo "- memory_pressure -Q:"
      memory_pressure -Q 2>&1 | head -20
    fi
    if have sysctl; then
      echo "- sysctl hw.memsize / kern.memorystatus_vm_pressure_level:"
      sysctl -n hw.memsize 2>/dev/null
      sysctl -n kern.memorystatus_vm_pressure_level 2>/dev/null
    fi
    if have pmset; then
      echo "- pmset -g therm (thermal throttle):"
      pmset -g therm 2>&1
    fi
    if have tmutil; then
      echo "- tmutil listlocalsnapshots / (APFS local snapshots pin disk space):"
      tmutil listlocalsnapshots / 2>&1
    fi
    echo "- top processes by RSS (is Codex/another cargo running?):"
    ps aux | head -1
    ps aux | awk 'NR>1' | sort -nrk6 | head -12
    echo "- build/agent/scanner processes (cargo, rustc, dsymutil, codex, mds, XProtect, syspolicyd):"
    ps aux | grep -iE 'cargo|rustc|dsymutil|codex|mds_stores|mdworker|XProtect|syspolicyd' | grep -v grep | head -20
    echo
  } >> "$REPORT"
}

# Lightweight mid-build capture: who has the CPU while cargo crawls.
cpu_snapshot() {
  {
    echo "-- top CPU at build+${1}s $(date -u '+%H:%M:%SZ') --"
    ps aux | head -1
    ps aux | awk 'NR>1' | sort -nrk3 | head -12
    echo
  } >> "$REPORT"
}

HEAD_SHA="$(git rev-parse HEAD)"
{
  echo "FAM-BUG-030 build-speed + hang diagnosis run"
  echo "date: $(date -u)"
  echo "host: $(uname -a)"
  echo "head: $HEAD_SHA"
  echo "rustc: $(rustc --version)"
  echo "cargo: $(cargo --version)"
  echo
} > "$REPORT"

# Was the FAM-BUG-030 fix candidate actually under test? The 20260901T093435Z
# report was ambiguous on exactly this; never again.
if git cat-file -e "$FIX_CANDIDATE^{commit}" 2>/dev/null; then
  if git merge-base --is-ancestor "$FIX_CANDIDATE" HEAD 2>/dev/null; then
    FIX_IN_TREE="yes"
  else
    FIX_IN_TREE="no"
  fi
else
  FIX_IN_TREE="unknown (commit not present in this clone)"
fi
note "fix candidate ${FIX_CANDIDATE:0:9} (daemon integration.rs process-group SIGKILL) is ancestor of HEAD: $FIX_IN_TREE"

# Cache-busting suspects: RUSTFLAGS or CARGO_* differing between runs
# invalidates incremental caches wholesale.
{
  echo "-- CARGO/RUST environment (cache-busting suspects) --"
  env | grep -E '^(CARGO|RUST)' | sort || echo "(none set)"
  echo
} >> "$REPORT"

if [ -d target ]; then
  TARGET_BEFORE="existed, $(du -sh target 2>/dev/null | cut -f1) (incremental expected)"
else
  TARGET_BEFORE="absent (cold build expected)"
fi
note "target/ before: $TARGET_BEFORE"

# Probe cargo's timing support on a throwaway crate (flag validation only
# happens after manifest discovery, so an empty-dir probe cannot work).
# --timings=json is nightly-only; plain --timings (HTML) is stable since 1.60.
TIMINGS_ARG=""
PROBE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/familiar-timings-probe.XXXXXX")"
mkdir -p "$PROBE_DIR/src"
printf '[package]\nname = "probe"\nversion = "0.0.1"\nedition = "2021"\n' > "$PROBE_DIR/Cargo.toml"
echo 'fn main() {}' > "$PROBE_DIR/src/main.rs"
if cargo build --manifest-path "$PROBE_DIR/Cargo.toml" --timings=json >/dev/null 2>&1; then
  TIMINGS_ARG="--timings=json"
elif cargo build --manifest-path "$PROBE_DIR/Cargo.toml" --timings >/dev/null 2>&1; then
  TIMINGS_ARG="--timings"
fi
rm -rf "$PROBE_DIR"
note "cargo per-crate timings support: ${TIMINGS_ARG:-none} (probed on a throwaway crate)"

# a daemon leaked by a previous run can hold pipes open and skew results
pkill -f familiar-ai-daemon 2>/dev/null

# ---------------------------------------------------------------- phase 1
mac_snapshot "pre-build"
note "phase 1 (build): cargo test --workspace --no-run $TIMINGS_ARG (cap ${BUILD_MAX_MINUTES}m, no stall detector — slow IS the finding; log=$BUILD_LOG)"
# shellcheck disable=SC2086
cargo test --workspace --no-run $TIMINGS_ARG </dev/null >"$BUILD_LOG" 2>&1 &
BUILD_PID=$!
BUILD_START=$(date +%s)
LAST_SNAP=$BUILD_START
BUILD_OUTCOME="unknown"
BUILD_SECONDS=0

while kill -0 "$BUILD_PID" 2>/dev/null; do
  sleep 10
  NOW=$(date +%s)
  if [ $((NOW - LAST_SNAP)) -ge "$SNAPSHOT_SECONDS" ]; then
    LAST_SNAP=$NOW
    cpu_snapshot $((NOW - BUILD_START))
  fi
  if [ $((NOW - BUILD_START)) -ge $((BUILD_MAX_MINUTES * 60)) ]; then
    BUILD_OUTCOME="cap"
    BUILD_SECONDS=$((NOW - BUILD_START))
    note "phase 1 REACHED ${BUILD_MAX_MINUTES}m cap — the BUILD alone exceeds it; killing cargo (stray rustc children may linger briefly)"
    tail -40 "$BUILD_LOG" >> "$REPORT"
    cpu_snapshot $((NOW - BUILD_START))
    kill "$BUILD_PID" 2>/dev/null
    pkill -P "$BUILD_PID" 2>/dev/null
    break
  fi
done

if [ "$BUILD_OUTCOME" = "unknown" ]; then
  wait "$BUILD_PID"
  BUILD_CODE=$?
  BUILD_SECONDS=$(( $(date +%s) - BUILD_START ))
  BUILD_OUTCOME="completed exit=$BUILD_CODE"
  note "phase 1 build finished: exit=$BUILD_CODE wall=${BUILD_SECONDS}s ($((BUILD_SECONDS / 60))m$((BUILD_SECONDS % 60))s)"
fi

COMPILED_UNITS=$(grep -c '^ *Compiling' "$BUILD_LOG")
note "phase 1 compiled units: $COMPILED_UNITS (0-5 means the incremental cache was warm; hundreds means cold/invalidated — see FAM-BUG-030 hypotheses)"
note "phase 1 last build-log lines:"
tail -15 "$BUILD_LOG" >> "$REPORT"
if [ -d target ]; then
  note "target/ after: $(du -sh target 2>/dev/null | cut -f1)"
fi

# Preserve cargo's own per-crate view next to the report.
TIMING_COPY=""
if [ "$TIMINGS_ARG" = "--timings" ]; then
  TIMING_HTML="$(sed -n 's/^ *Timing report saved to //p' "$BUILD_LOG" | tail -1)"
  if [ -n "$TIMING_HTML" ] && [ -f "$TIMING_HTML" ] \
     && [ "$(wc -c < "$TIMING_HTML" | tr -d ' ')" -lt 3000000 ]; then
    TIMING_COPY="$OUT_DIR/mac-build-speed-$STAMP-timings.html"
    cp "$TIMING_HTML" "$TIMING_COPY"
    note "per-crate timings captured: $TIMING_COPY (open in a browser; the widest bars name the slow crates)"
  fi
elif [ "$TIMINGS_ARG" = "--timings=json" ]; then
  note "per-crate timing-info json (from nightly cargo):"
  grep '"reason":"timing-info"' "$BUILD_LOG" | head -300 >> "$REPORT"
fi

# ---------------------------------------------------------------- phase 2
TEST_OUTCOME="skipped"
if [ "$BUILD_OUTCOME" = "cap" ]; then
  note "phase 2 SKIPPED: the build never completed, so test timing would still be measuring compilation"
elif [ "${BUILD_CODE:-1}" -ne 0 ]; then
  note "phase 2 SKIPPED: the build failed (exit ${BUILD_CODE:-?}); fix the build first"
else
  mac_snapshot "pre-tests"
  pkill -f familiar-ai-daemon 2>/dev/null
  note "phase 2 (tests): cargo test --workspace on the warm build (stdin=/dev/null, stall=${TEST_STALL_SECONDS}s, cap=${TEST_MAX_MINUTES}m, log=$TEST_LOG)"
  cargo test --workspace </dev/null >"$TEST_LOG" 2>&1 &
  SUITE_PID=$!
  START=$(date +%s)
  LAST_SIZE=0
  LAST_CHANGE=$START
  TEST_OUTCOME="unknown"

  while kill -0 "$SUITE_PID" 2>/dev/null; do
    sleep 10
    NOW=$(date +%s)
    SIZE=$(wc -c < "$TEST_LOG" | tr -d ' ')
    if [ "$SIZE" != "$LAST_SIZE" ]; then
      LAST_SIZE=$SIZE
      LAST_CHANGE=$NOW
    fi
    if [ $((NOW - LAST_CHANGE)) -ge "$TEST_STALL_SECONDS" ]; then
      TEST_OUTCOME="stalled"
      note "STALL DETECTED after $((NOW - START))s total, $TEST_STALL_SECONDS s without output"
      note "last 60 log lines at stall:"
      tail -60 "$TEST_LOG" >> "$REPORT"
      note "test binaries running:"
      ps aux | grep 'target/debug/deps' | grep -v grep | tee -a "$REPORT"
      HUNG_PIDS=$(ps aux | grep 'target/debug/deps' | grep -v grep | awk '{print $2}')
      for PID in $HUNG_PIDS; do
        note "sample of pid $PID:"
        if have sample; then
          sample "$PID" 3 2>/dev/null | head -80 >> "$REPORT" || echo "(sample unavailable)" >> "$REPORT"
        else
          echo "(sample is macOS-only; not present on this host)" >> "$REPORT"
        fi
        note "open files of pid $PID (first 40):"
        if have lsof; then
          lsof -p "$PID" 2>/dev/null | head -40 >> "$REPORT" || true
        else
          echo "(lsof not present on this host)" >> "$REPORT"
        fi
      done
      note "killing suite"
      kill "$SUITE_PID" 2>/dev/null
      pkill -f 'target/debug/deps' 2>/dev/null
      break
    fi
    if [ $((NOW - START)) -ge $((TEST_MAX_MINUTES * 60)) ]; then
      TEST_OUTCOME="cap"
      note "phase 2 REACHED ${TEST_MAX_MINUTES}m cap without a $TEST_STALL_SECONDS s stall (tests trickling, not stuck — execution itself is slow)"
      tail -60 "$TEST_LOG" >> "$REPORT"
      note "processes at cap (test binaries and daemons):"
      ps aux | grep -E 'target/debug/deps|familiar-ai-daemon' | grep -v grep | tee -a "$REPORT"
      CAP_PIDS=$(ps aux | grep -E 'target/debug/deps|familiar-ai-daemon' | grep -v grep | awk '{print $2}')
      for PID in $CAP_PIDS; do
        note "sample of pid $PID at cap:"
        if have sample; then
          sample "$PID" 3 2>/dev/null | head -80 >> "$REPORT" || echo "(sample unavailable)" >> "$REPORT"
        else
          echo "(sample is macOS-only; not present on this host)" >> "$REPORT"
        fi
      done
      kill "$SUITE_PID" 2>/dev/null
      pkill -f 'target/debug/deps' 2>/dev/null
      pkill -f familiar-ai-daemon 2>/dev/null
      break
    fi
  done

  if [ "$TEST_OUTCOME" = "unknown" ]; then
    wait "$SUITE_PID"
    TEST_CODE=$?
    TEST_SECONDS=$(( $(date +%s) - START ))
    TEST_OUTCOME="completed exit=$TEST_CODE"
    note "phase 2 suite COMPLETED with exit $TEST_CODE in ${TEST_SECONDS}s"
    note "result summary:"
    grep -E 'test result|FAILED' "$TEST_LOG" | tail -40 >> "$REPORT"
  fi
  mac_snapshot "post-tests"
fi

# ---------------------------------------------------------------- verdict
note "SUMMARY: head=$HEAD_SHA fix_candidate_in_tree=$FIX_IN_TREE"
note "SUMMARY: phase 1 (build): $BUILD_OUTCOME wall=${BUILD_SECONDS}s compiled_units=$COMPILED_UNITS target_before=$TARGET_BEFORE"
note "SUMMARY: phase 2 (tests): $TEST_OUTCOME"
case "$TEST_OUTCOME" in
  completed*)
    if [ "$FIX_IN_TREE" = "yes" ]; then
      note "VERDICT: FAM-BUG-030 hang NOT REPRODUCED at this HEAD (fix candidate ${FIX_CANDIDATE:0:9} validated by this clean unattended run)"
    else
      note "VERDICT: hang not reproduced, but fix candidate ${FIX_CANDIDATE:0:9} is NOT an ancestor of this HEAD — this run says nothing about the fix; pull and rerun"
    fi
    ;;
  stalled)
    if [ "$FIX_IN_TREE" = "yes" ]; then
      note "VERDICT: FAM-BUG-030 hang REPRODUCED at a HEAD that CONTAINS fix candidate ${FIX_CANDIDATE:0:9} — the candidate is insufficient; the stack sample above names the blocked binary"
    else
      note "VERDICT: hang reproduced, but fix candidate ${FIX_CANDIDATE:0:9} was NOT under test — pull and rerun before drawing conclusions"
    fi
    ;;
  cap)
    note "VERDICT: no stall, but test execution alone exceeded ${TEST_MAX_MINUTES}m on a warm build — slowness is not confined to the build phase"
    ;;
  skipped)
    note "VERDICT: build-phase problem only — the hang question was not reached this run"
    ;;
esac

git add "$REPORT"
[ -n "$TIMING_COPY" ] && git add "$TIMING_COPY"
git commit -q -m "diagnostics: FAM-BUG-030 mac build-speed run (build=${BUILD_OUTCOME%% *}, tests=${TEST_OUTCOME%% *}) $STAMP" && git push -q origin main \
  && note "report pushed: $REPORT" \
  || note "PUSH FAILED - report saved locally at $REPORT; commit and push it manually"
