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
echo "---" >> "$LOG"
echo "exit: $CODE" >> "$LOG"

# Belt-and-braces masking of credential-shaped lines before the log is
# published (the CLI already redacts its own evidence; this catches stray
# provider/tool output).
TMP="$LOG.masking"
sed -E \
  -e 's/.*[Aa]uthorization: [Bb]earer .*/[MASKED LINE]/' \
  -e 's/.*sk-(proj|live|ant)-[A-Za-z0-9_-]{8,}.*/[MASKED LINE]/' \
  -e 's/.*github_pat_[A-Za-z0-9_]{8,}.*/[MASKED LINE]/' \
  -e 's/.*AWS_SECRET_ACCESS_KEY.*/[MASKED LINE]/' \
  "$LOG" > "$TMP" && mv "$TMP" "$LOG"

git add "$LOG"
if git commit -q -m "session log: familiar-ai $1 ($STAMP, exit $CODE)" && git push -q origin main; then
  echo "session log pushed: $LOG"
else
  echo "PUSH FAILED — log saved locally at $LOG (commit and push it when convenient)"
fi
exit "$CODE"
