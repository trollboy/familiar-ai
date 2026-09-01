#!/bin/bash
# Run any familiar-ai command with its full console output captured to a
# committable session log — then commit and push the log automatically.
#
# Usage:    ./scripts/logged.sh drive --max-prds 1 --prd PRD-076
#           ./scripts/logged.sh resume all --dry-run
# Result:   docs/session-logs/<timestamp>-<command>.log on origin/main.
set -u
if [ $# -eq 0 ]; then
  echo "usage: $0 <familiar-ai args...>" >&2
  exit 2
fi
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
SLUG="$(echo "$1" | tr -c 'A-Za-z0-9' '-')"
OUT_DIR="docs/session-logs"
LOG="$OUT_DIR/$STAMP-$SLUG.log"
mkdir -p "$OUT_DIR"

# The log must reach origin even if this script errors or is interrupted —
# finalize exactly once from an EXIT trap.
FINALIZED=0
finalize() {
  CODE=${1:-${CODE:-130}}
  [ "$FINALIZED" = 1 ] && return
  FINALIZED=1
  {
    echo "---"
    echo "exit: $CODE"
  } >> "$LOG" 2>/dev/null
  TMP="$LOG.masking"
  sed -E \
    -e 's/.*[Aa]uthorization: [Bb]earer .*/[MASKED LINE]/' \
    -e 's/.*sk-(proj|live|ant)-[A-Za-z0-9_-]{8,}.*/[MASKED LINE]/' \
    -e 's/.*github_pat_[A-Za-z0-9_]{8,}.*/[MASKED LINE]/' \
    -e 's/.*AWS_SECRET_ACCESS_KEY.*/[MASKED LINE]/' \
    "$LOG" > "$TMP" 2>/dev/null && mv "$TMP" "$LOG"
  # -f: .gitignore's blanket *.log would silently drop the session log,
  # leaving finalize to report PUSH FAILED with nothing staged (FRICTION-005).
  git add -f "$LOG" 2>/dev/null
  if git commit -q -m "session log: familiar-ai ${SLUG%-} ($STAMP, exit $CODE)" 2>/dev/null \
     && git push -q origin main 2>/dev/null; then
    echo "session log pushed: $LOG"
  else
    echo "PUSH FAILED - log saved locally at $LOG (commit and push it when convenient)"
  fi
}
trap 'finalize' EXIT
trap 'echo "logged.sh: error near line $LINENO" | tee -a "$LOG"' ERR

{
  echo "command: familiar-ai $*"
  echo "date: $(date -u)"
  echo "host: $(uname -n) ($(uname -s))"
  echo "head: $(git rev-parse HEAD) ($(git status --porcelain | wc -l | tr -d ' ') dirty files)"
  echo "binary: $(command -v familiar-ai)"
  echo "---"
} > "$LOG"

# Live output on the terminal AND into the log.
familiar-ai "$@" 2>&1 | tee -a "$LOG"
CODE=${PIPESTATUS[0]}

exit "$CODE"
