#!/usr/bin/env bash
# Rebuild Familiar from this tree, install all three binaries, and make the
# per-user supervisor run them. Stops at the first step that does not take.
#
#   bash scripts/reinstall.sh              # build + install + restart + status
#   bash scripts/reinstall.sh --no-restart # build + install only
#
# Every install is verified with cmp, and the final status refuses if the
# supervisor still points at some other binary (FAM-BUG-084).

set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"
BIN="${FAMILIAR_BIN_DIR:-$HOME/.local/bin}"
RESTART=1
[ "${1:-}" = "--no-restart" ] && RESTART=0

step() { printf '\n==> %s\n' "$*"; }

step "building CLI"
cargo build --release -p familiar-ai-daemon --bin familiar-ai

# The Tauri desktop is the sole tray owner.  Building the supervised daemon
# headless is an intentional deployment topology, not a way to evade the
# default-feature verification performed by scripts/gate.sh.
step "building headless daemon (desktop owns the tray)"
cargo build --release -p familiar-ai-daemon --no-default-features --bin familiar-ai-daemon

step "building desktop"
cargo build --release -p familiar-ai-desktop

step "installing to $BIN"
mkdir -p "$BIN"
for b in familiar-ai familiar-ai-daemon familiar-ai-desktop; do
  install -m755 "target/release/$b" "$BIN/$b"
  if cmp -s "$BIN/$b" "target/release/$b"; then
    echo "  $b: installed ($(ls -l "$BIN/$b" | awk '{print $6, $7, $8}'))"
  else
    echo "  $b: install did not take; $BIN/$b differs from target/release/$b" >&2
    exit 1
  fi
done

case ":$PATH:" in
  *":$BIN:"*) ;;
  *) echo "warning: $BIN is not on PATH; 'familiar-ai' may resolve elsewhere: $(command -v familiar-ai || echo none)" >&2 ;;
esac

if [ "$RESTART" = 1 ]; then
  step "reinstalling supervisor definitions (restarts daemon and desktop)"
  "$BIN/familiar-ai" ops desktop uninstall || true
  "$BIN/familiar-ai" ops desktop install
  sleep 3
fi

step "status"
"$BIN/familiar-ai" ops desktop status

step "running processes"
pgrep -fa "familiar-ai-(daemon|desktop)" || echo "none running"
