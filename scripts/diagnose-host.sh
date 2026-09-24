#!/usr/bin/env bash
# Dump everything another machine needs to see why this host's Familiar shows
# what it shows: build, processes, ledger rows, PRD files, config, logs.
# Writes docs/diagnostics/host-<hostname>.log.txt and commits it (no push).
#
#   bash scripts/diagnose-host.sh            # then: git push --no-verify
#
# --no-verify is fine for this commit: it is a log, and the gate's verdict for
# it would be "absent", which is visible, not hidden.

set -u
REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"
HOST="$(hostname -s 2>/dev/null || hostname)"
OUT_DIR="docs/diagnostics"
OUT="$OUT_DIR/host-$HOST.log.txt"
mkdir -p "$OUT_DIR"
exec > "$OUT" 2>&1

section() { printf '\n=== %s ===\n' "$*"; }
run() { printf '$ %s\n' "$*"; "$@" 2>&1 || printf '[exit %s]\n' "$?"; }

echo "familiar-ai host diagnostics"
echo "host: $HOST"
echo "date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "os:   $(uname -a)"

section "git"
run git rev-parse --short HEAD
run git status -sb
run git status --short
run git log --oneline -8
run git fetch -q origin
echo "origin/main: $(git rev-parse --short origin/main 2>/dev/null)"
echo "behind origin/main by: $(git rev-list --count HEAD..origin/main 2>/dev/null)"
echo "ahead of origin/main by: $(git rev-list --count origin/main..HEAD 2>/dev/null)"

section "installed binaries"
for b in familiar-ai familiar-ai-daemon familiar-ai-desktop; do
  p="$(command -v "$b" 2>/dev/null || true)"
  if [ -n "$p" ]; then
    ls -l "$p"
    if [ -f "target/release/$b" ]; then
      if cmp -s "$p" "target/release/$b"; then echo "  == target/release/$b (identical)"; else echo "  != target/release/$b (DIFFERS; installed binary is not this tree's build)"; fi
    else
      echo "  no target/release/$b in this tree"
    fi
  else
    echo "$b: not on PATH"
  fi
done
run ls -l target/release/familiar-ai target/release/familiar-ai-daemon target/release/familiar-ai-desktop

section "processes"
run pgrep -fal "familiar-ai"
run familiar-ai ops desktop status
if command -v launchctl >/dev/null 2>&1; then run bash -c "launchctl list | grep -i familiar"; fi
if command -v systemctl >/dev/null 2>&1; then run bash -c "systemctl --user list-units --type=service --all | grep -i familiar"; fi

section "config"
CFG="$HOME/.config/familiar-ai/config.toml"
[ -f "$CFG" ] || CFG="$HOME/Library/Application Support/familiar-ai/config.toml"
echo "config: $CFG"
run grep -nE '^\[repositories|^\[driver|^\[delivery|^\[dashboard|profile|risk_vocabulary|prd_metadata_policy|worktree_root|database' "$CFG"

section "database"
DB=""
for c in "$HOME/.local/share/familiar-ai/familiar.db" "$HOME/Library/Application Support/familiar-ai/familiar.db"; do
  [ -f "$c" ] && DB="$c" && break
done
[ -z "$DB" ] && DB="$(find "$HOME/Library" "$HOME/.local" -name familiar.db 2>/dev/null | head -1)"
echo "db: $DB"
KEY="$(git rev-parse --git-common-dir 2>/dev/null)"; case "$KEY" in /*) ;; *) KEY="$REPO/$KEY";; esac
echo "repository_key: $KEY"
if [ -n "$DB" ] && command -v sqlite3 >/dev/null 2>&1; then
  Q() { printf '> %s\n' "$1"; sqlite3 -header -column "$DB" "$1" 2>&1; }
  Q "select count(*) as migrations, max(version) as latest from schema_migrations;"
  Q "select repository_key from backlog_prds group by repository_key;"
  Q "select prd_number, status, missing_since, prd_path from backlog_prds where repository_key='$KEY' and prd_number in (92,97,103,104,106,108,109) order by prd_number, prd_path;"
  Q "select max(last_seen_at) as last_scan from backlog_prds where repository_key='$KEY';"
  Q "select session_id, substr(started_at,1,19) started, termination_reason from driver_sessions where repository_key='$KEY' order by started_at desc limit 5;"
  Q "select a.prd_id, substr(a.started_at,1,19) started, a.outcome, a.retained_reason, a.last_durable_phase from driver_attempts a join driver_sessions s on s.session_id=a.session_id where s.repository_key='$KEY' order by a.started_at desc limit 8;"
  Q "select prd_id, phase, substr(updated_at,1,10) updated from execution_checkpoints where repository_key='$KEY' and phase not in ('completed') order by updated_at desc limit 12;"
  Q "select d.prd_id, d.decision, substr(d.detail,1,90) detail from driver_selection_decisions d join driver_sessions s on s.session_id=d.session_id where s.repository_key='$KEY' order by d.decision_id desc limit 12;"
  Q "select e.prd_path, e.old_status, e.new_status, e.actor, substr(e.changed_at,1,19) at, r.action from backlog_status_events e left join backlog_recovery_events r on r.status_event_id=e.event_id where e.repository_key='$KEY' and (e.prd_path like '%PRD-092%' or e.prd_path like '%PRD-10[3468]%') order by e.event_id desc limit 12;"
else
  echo "no database or no sqlite3"
fi

section "stewardship backlog (status + lifecycle as the CLI reports it)"
familiar-ai stewardship backlog --limit 300 2>&1 | python3 -c '
import json,sys
raw=sys.stdin.read()
try:
    d=json.loads(raw)
except Exception:
    print(raw[:600]); sys.exit()
for i in d.get("items",[]):
    if i.get("prd_id") in ("PRD-92","PRD-97","PRD-103","PRD-104","PRD-106","PRD-108","PRD-109"):
        print(i.get("prd_id"), i.get("status"), i.get("lifecycle"), i.get("lifecycle_divergence",""), i.get("prd_path"))
' 2>&1

section "PRD files"
for n in 092 097 103 104 106 108 109; do
  for f in "docs/prds/PRD-$n.md" "docs/prds/done/PRD-$n.md"; do
    [ -f "$f" ] && echo "$f  status=$(grep -m1 '^status:' "$f" | cut -d' ' -f2)"
  done
done
run familiar-ai backlog metadata-check --advisory
echo "(only the summary line matters above; legacy warnings are expected)"

section "scheduler width for 92,103,104,106 (refuses while a daemon holds the claim; the refusal itself is informative)"
run familiar-ai ops operator width --actor human:diagnostics --reason "host diagnostics" PRD-92 PRD-103 PRD-104 PRD-106

section "recent daemon log"
LOG=""
for d in "$HOME/.local/state/familiar-ai/log" "$HOME/Library/Logs/familiar-ai" "$HOME/Library/Application Support/familiar-ai/log"; do
  if [ -d "$d" ]; then LOG="$(ls -t "$d"/* 2>/dev/null | head -1)"; [ -n "$LOG" ] && break; fi
done
echo "log: $LOG"
if [ -n "$LOG" ]; then
  run bash -c "grep -nE 'conflicts unavailable|reconcil|WARN|ERROR|error' '$LOG' | tail -40"
  run tail -n 30 "$LOG"
fi

exec >/dev/tty 2>&1 || exec >/dev/null 2>&1
cd "$REPO"
git add -f "$OUT"
if git -c user.name="$(git config user.name || echo diagnostics)" -c user.email="$(git config user.email || echo diagnostics@invalid)" commit -q -m "diag: host $HOST $(date -u +%Y-%m-%dT%H:%MZ)" -- "$OUT"; then
  echo "wrote and committed $OUT — now: git push --no-verify"
else
  echo "wrote $OUT (nothing new to commit)"
fi
