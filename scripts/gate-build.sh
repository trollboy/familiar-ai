#!/bin/bash
# PRD-092: the gate job that proves the default feature set builds.
#
# Before this PRD, every documented invocation of `cargo build` for
# familiar-ai-daemon carried `--no-default-features`, because the default
# set pulled in the `tray` feature, which had both a pre-existing compile
# error and a hard dependency on the system `libxdo` library. A default
# that does not build on a supported platform is not a default.
#
# This script is deliberately the exact command a host install runs (see
# the README Quick Start and `check-feature-parity.sh`, which enforces
# that the two never drift apart): no `--features`, no `--no-default-features`,
# just the plain package build.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/gate-build.sh

Builds familiar-ai-daemon's default feature set (no flags) and fails if it
does not build clean. This is "the gate" for PRD-092 acceptance criterion 1:
a default feature set that does not build on a supported platform is not a
default.
EOF
}

if [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
  usage
  exit 0
fi

cd "$(dirname "${BASH_SOURCE[0]}")/.."

case "$(uname -s)" in
  Linux|Darwin) ;;
  *)
    echo "gate-build: unsupported platform $(uname -s) — this PRD claims Linux and macOS only" >&2
    exit 1
    ;;
esac

echo "== gate-build: cargo build -p familiar-ai-daemon (default features, no flags)"
# Kept identical in shape to the README install command and the Dockerfile
# builder stage; see scripts/check-feature-parity.sh.
# PRD-092:PARITY
cargo build -p familiar-ai-daemon
echo "== gate-build: PASS — default feature set builds clean on $(uname -s)"
