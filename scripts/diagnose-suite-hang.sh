#!/bin/bash
# FAM-BUG-030 unattended diagnosis: run the workspace test suite the way
# preflight does (null stdin), detect a stall, capture the hung test binary's
# identity, stack sample, and open files, then commit and push the evidence.
#
# Usage:   ./scripts/diagnose-suite-hang.sh
# Result:  docs/diagnostics/suite-hang-<timestamp>.txt committed to main.
set -u

# Self-update before diagnosing: the 20260901T093435Z run diagnosed a HEAD
# that predated the fix under test. Pull, then re-exec the (possibly new)
# script exactly once — bash reads scripts incrementally, so continuing after
# the file changed underneath us would corrupt execution.
if [ -z "${FAMILIAR_DIAGNOSE_PULLED:-}" ]; then
  git pull --ff-only -q || echo "== WARNING: git pull failed; diagnosing current checkout" >&2
  FAMILIAR_DIAGNOSE_PULLED=1 exec "$0" "$@"
fi

STALL_SECONDS=180          # no new output for this long = stalled
MAX_MINUTES=45             # absolute cap
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT_DIR="docs/diagnostics"
REPORT="$OUT_DIR/suite-hang-$STAMP.txt"
LOG="$(mktemp -t familiar-suite-log)"
mkdir -p "$OUT_DIR"

note() { echo "== $*" | tee -a "$REPORT"; }

{
  echo "FAM-BUG-030 diagnosis run"
  echo "date: $(date -u)"
  echo "host: $(uname -a)"
  echo "head: $(git rev-parse HEAD)"
  echo "rustc: $(rustc --version)"
  echo
} > "$REPORT"

# a daemon leaked by a previous run can hold pipes open and skew results
pkill -f familiar-ai-daemon 2>/dev/null
note "starting: cargo test --workspace (stdin=/dev/null, log=$LOG)"
cargo test --workspace </dev/null >"$LOG" 2>&1 &
SUITE_PID=$!
START=$(date +%s)
LAST_SIZE=0
LAST_CHANGE=$START
OUTCOME="unknown"

while kill -0 "$SUITE_PID" 2>/dev/null; do
  sleep 10
  NOW=$(date +%s)
  SIZE=$(wc -c < "$LOG" | tr -d ' ')
  if [ "$SIZE" != "$LAST_SIZE" ]; then
    LAST_SIZE=$SIZE
    LAST_CHANGE=$NOW
  fi
  if [ $((NOW - LAST_CHANGE)) -ge $STALL_SECONDS ]; then
    OUTCOME="stalled"
    note "STALL DETECTED after $((NOW - START))s total, $STALL_SECONDS s without output"
    note "last 60 log lines at stall:"
    tail -60 "$LOG" >> "$REPORT"
    note "test binaries running:"
    ps aux | grep 'target/debug/deps' | grep -v grep | tee -a "$REPORT"
    HUNG_PIDS=$(ps aux | grep 'target/debug/deps' | grep -v grep | awk '{print $2}')
    for PID in $HUNG_PIDS; do
      note "sample of pid $PID:"
      sample "$PID" 3 2>/dev/null | head -80 >> "$REPORT" || echo "(sample unavailable)" >> "$REPORT"
      note "open files of pid $PID (first 40):"
      lsof -p "$PID" 2>/dev/null | head -40 >> "$REPORT" || true
    done
    note "killing suite"
    kill "$SUITE_PID" 2>/dev/null
    pkill -f 'target/debug/deps' 2>/dev/null
    break
  fi
  if [ $((NOW - START)) -ge $((MAX_MINUTES * 60)) ]; then
    OUTCOME="cap"
    note "REACHED ${MAX_MINUTES}m cap without a $STALL_SECONDS s stall (slow or trickling output)"
    tail -60 "$LOG" >> "$REPORT"
    note "processes at cap (test binaries and daemons):"
    ps aux | grep -E 'target/debug/deps|familiar-ai-daemon' | grep -v grep | tee -a "$REPORT"
    CAP_PIDS=$(ps aux | grep -E 'target/debug/deps|familiar-ai-daemon' | grep -v grep | awk '{print $2}')
    for PID in $CAP_PIDS; do
      note "sample of pid $PID at cap:"
      sample "$PID" 3 2>/dev/null | head -80 >> "$REPORT" || echo "(sample unavailable)" >> "$REPORT"
    done
    kill "$SUITE_PID" 2>/dev/null
    pkill -f 'target/debug/deps' 2>/dev/null
    pkill -f familiar-ai-daemon 2>/dev/null
    break
  fi
done

if [ "$OUTCOME" = "unknown" ]; then
  wait "$SUITE_PID"
  CODE=$?
  OUTCOME="completed exit=$CODE"
  note "suite COMPLETED with exit $CODE in $((($(date +%s) - START)))s"
  note "IMPORTANT: a clean manual completion while the drive preflight hangs means the hang is drive-context-specific."
  note "result summary:"
  grep -E 'test result|FAILED' "$LOG" | tail -40 >> "$REPORT"
fi

note "outcome: $OUTCOME"
git add "$REPORT"
git commit -q -m "diagnostics: FAM-BUG-030 suite run ($OUTCOME) $STAMP" && git push -q origin main \
  && note "report pushed: $REPORT" \
  || note "PUSH FAILED - report saved locally at $REPORT; commit and push it manually"
