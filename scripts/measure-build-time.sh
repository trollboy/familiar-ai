#!/bin/bash
# PRD-092 acceptance criterion 4: clean and incremental build wall-clock is
# measured per supported platform against a declared ceiling, and recorded,
# so a build-speed pathology (docs/running_bugs.md FAM-BUG-030: a macOS cold
# build once took ~44 minutes) is a reported failure rather than folklore.
#
# Ceilings are declared here, not discovered empirically per run — a build
# that gets slower over time should fail this script, not quietly raise the
# bar. macOS ceilings are set from the FAM-BUG-030 record: the validated-good
# warm build was 3m50s (230s); the pathological cold build was ~44 minutes
# (2640s). The ceiling sits between them so the good case passes and the
# pathology fails — see --selftest, which pins exactly that.
set -euo pipefail

LINUX_CLEAN_CEILING_SECS=600      # 10 min
LINUX_INCREMENTAL_CEILING_SECS=60 # 1 min
MACOS_CLEAN_CEILING_SECS=900      # 15 min
MACOS_INCREMENTAL_CEILING_SECS=360 # 6 min — comfortably above the validated 230s warm build

usage() {
  cat <<'EOF'
Usage: scripts/measure-build-time.sh [--report <path>]
       scripts/measure-build-time.sh --selftest

Times a clean build and an incremental rebuild of familiar-ai-daemon's
default feature set, compares each against the declared ceiling for the
current platform, and exits non-zero if either is exceeded. With --report,
also appends a timestamped record to <path>.

--selftest checks the ceiling-comparison logic itself against fixed
durations — the FAM-BUG-030 macOS pathology (~44 min cold) and the
validated-good case (3m50s warm) — instead of spending 45 minutes
reproducing a historical bad build on demand.
EOF
}

# Exit 0 if $1 (seconds) is within the ceiling $2 (seconds), else 1.
within_ceiling() {
  local elapsed="$1" ceiling="$2"
  [ "$elapsed" -le "$ceiling" ]
}

ceilings_for() {
  case "$1" in
    Linux) echo "$LINUX_CLEAN_CEILING_SECS $LINUX_INCREMENTAL_CEILING_SECS" ;;
    Darwin) echo "$MACOS_CLEAN_CEILING_SECS $MACOS_INCREMENTAL_CEILING_SECS" ;;
    *) return 1 ;;
  esac
}

run_selftest() {
  local pathological_cold=2640  # ~44 min — docs/running_bugs.md FAM-BUG-030
  local validated_warm=230      # 3m50s — same record, post-fix

  local macos_clean macos_incr
  read -r macos_clean macos_incr <<< "$(ceilings_for Darwin)"

  if within_ceiling "$pathological_cold" "$macos_clean"; then
    echo "selftest FAILED: the FAM-BUG-030 pathology (${pathological_cold}s) did not exceed the declared macOS clean ceiling (${macos_clean}s)" >&2
    return 1
  fi
  if ! within_ceiling "$validated_warm" "$macos_incr"; then
    echo "selftest FAILED: the validated-good warm build (${validated_warm}s) exceeded the declared macOS incremental ceiling (${macos_incr}s)" >&2
    return 1
  fi

  local linux_clean linux_incr
  read -r linux_clean linux_incr <<< "$(ceilings_for Linux)"
  if ! within_ceiling 1 "$linux_clean" || ! within_ceiling 1 "$linux_incr"; then
    echo "selftest FAILED: a trivially fast build was reported as exceeding a ceiling" >&2
    return 1
  fi

  echo "selftest: PASS — pathology (${pathological_cold}s) fails the macOS clean ceiling (${macos_clean}s); the validated warm build (${validated_warm}s) passes the incremental ceiling (${macos_incr}s)"
}

run_measurement() {
  local report="${1:-}"
  local os
  os="$(uname -s)"
  local ceilings
  if ! ceilings="$(ceilings_for "$os")"; then
    echo "measure-build-time: unsupported platform $os — this PRD claims Linux and macOS only" >&2
    return 1
  fi
  local clean_ceiling incr_ceiling
  read -r clean_ceiling incr_ceiling <<< "$ceilings"

  cd "$(dirname "${BASH_SOURCE[0]}")/.."

  echo "== measure-build-time: cargo clean -p familiar-ai-daemon"
  cargo clean -p familiar-ai-daemon

  local start elapsed_clean
  start="$(date +%s)"
  cargo build -p familiar-ai-daemon >/dev/null
  elapsed_clean=$(( $(date +%s) - start ))

  # touch the bin entry point so the incremental measurement recompiles and
  # relinks something real without redoing the full dependency graph.
  touch crates/familiar-ai-daemon/src/main.rs
  start="$(date +%s)"
  cargo build -p familiar-ai-daemon >/dev/null
  local elapsed_incremental=$(( $(date +%s) - start ))

  local clean_status incr_status overall=0
  if within_ceiling "$elapsed_clean" "$clean_ceiling"; then
    clean_status="PASS"
  else
    clean_status="FAIL"
    overall=1
  fi
  if within_ceiling "$elapsed_incremental" "$incr_ceiling"; then
    incr_status="PASS"
  else
    incr_status="FAIL"
    overall=1
  fi

  local line
  line="$(date -u +%Y-%m-%dT%H:%M:%SZ) platform=$os clean=${elapsed_clean}s(ceiling=${clean_ceiling}s,${clean_status}) incremental=${elapsed_incremental}s(ceiling=${incr_ceiling}s,${incr_status})"
  echo "== measure-build-time: $line"
  if [ -n "$report" ]; then
    echo "$line" >> "$report"
  fi

  return $overall
}

if [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
  usage
  exit 0
fi

if [ "${1:-}" = "--selftest" ]; then
  run_selftest
  exit $?
fi

report=""
if [ "${1:-}" = "--report" ]; then
  report="${2:?--report requires a path}"
fi

run_measurement "$report"
