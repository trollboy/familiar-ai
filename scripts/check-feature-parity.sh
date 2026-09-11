#!/bin/bash
# PRD-092 acceptance criterion 5: the verification gate and a host install
# build the same feature set, so a gate-green build cannot differ from what
# a user installs.
#
# The gate (scripts/gate-build.sh), the Dockerfile builder stage, and the
# README install command all build `familiar-ai-daemon` with the *default*
# feature set. Grepping loosely for "any cargo build line mentioning
# familiar-ai-daemon" is too fragile — it also matches the intentionally
# *different* opt-in `--features tray` example, and prose that merely talks
# about a command rather than running one. Instead, each of the three files
# marks its one canonical build line with a `PRD-092:PARITY` marker on the
# line immediately above it; this script compares only those three marked
# lines.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/check-feature-parity.sh
       scripts/check-feature-parity.sh --selftest

Reads the line following a `PRD-092:PARITY` marker in the Dockerfile, in
README.md, and in scripts/gate-build.sh, and fails if those three lines do
not resolve the same cargo feature set (a `--features` or
`--no-default-features` on one but not the others).

--selftest exercises the comparison against synthetic fixtures instead of
this repository's real files.
EOF
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MARKER="PRD-092:PARITY"

# Prints the feature-set "shape" of a cargo invocation line: the sorted,
# space-joined set of `--no-default-features` / `--features <list>` tokens
# it carries. Empty means "default features, no flags".
feature_shape() {
  local line="$1"
  local shape=""
  if [[ "$line" == *--no-default-features* ]]; then
    shape="no-default-features"
  fi
  if [[ "$line" =~ --features[[:space:]=]+([A-Za-z0-9_,-]+) ]]; then
    shape="${shape} features:${BASH_REMATCH[1]}"
  fi
  echo "$shape" | xargs echo
}

# Prints "file:lineno:shape" for the line following the marker in $1, or
# fails loudly if $1 has no marker.
extract_marked() {
  local file="$1"
  local lineno
  lineno="$(grep -n "$MARKER" "$file" | head -1 | cut -d: -f1)"
  if [ -z "$lineno" ]; then
    echo "check-feature-parity: $file has no '$MARKER' marker" >&2
    return 1
  fi
  local target_lineno=$((lineno + 1))
  local line
  line="$(sed -n "${target_lineno}p" "$file")"
  echo "$file:$target_lineno:$(feature_shape "$line")"
}

check_parity() {
  local dockerfile="$1" readme="$2" gate_script="$3"
  local rows shapes
  rows="$(extract_marked "$dockerfile" && extract_marked "$readme" && extract_marked "$gate_script")" || return 1
  echo "$rows"
  shapes="$(echo "$rows" | cut -d: -f3- | sort -u)"
  if [ "$(echo "$shapes" | wc -l)" -gt 1 ]; then
    echo "check-feature-parity: FAIL — divergent feature sets:" >&2
    echo "$shapes" | sed 's/^/  shape: [/;s/$/]/' >&2
    return 1
  fi
  return 0
}

run_selftest() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN

  # Matching shapes (both default, no flags) — must PASS.
  printf '# %s\ncargo build --release --bin familiar-ai-daemon\n' "$MARKER" > "$tmp/Dockerfile"
  printf '<!-- %s -->\ncargo build --release -p familiar-ai-daemon --bin familiar-ai\n' "$MARKER" > "$tmp/README.md"
  printf '# %s\ncargo build -p familiar-ai-daemon\n' "$MARKER" > "$tmp/gate-build.sh"
  if ! check_parity "$tmp/Dockerfile" "$tmp/README.md" "$tmp/gate-build.sh" > /dev/null; then
    echo "selftest FAILED: matching feature sets were wrongly flagged as divergent" >&2
    return 1
  fi

  # Divergent shapes (one carries --no-default-features) — must FAIL.
  printf '# %s\ncargo build --release --no-default-features --bin familiar-ai-daemon\n' "$MARKER" > "$tmp/Dockerfile"
  if check_parity "$tmp/Dockerfile" "$tmp/README.md" "$tmp/gate-build.sh" > /dev/null 2>&1; then
    echo "selftest FAILED: a divergent feature set was not flagged" >&2
    return 1
  fi

  # A missing marker — must FAIL loudly rather than silently skip.
  printf 'cargo build --release --bin familiar-ai-daemon\n' > "$tmp/Dockerfile"
  if check_parity "$tmp/Dockerfile" "$tmp/README.md" "$tmp/gate-build.sh" > /dev/null 2>&1; then
    echo "selftest FAILED: a missing marker was not flagged" >&2
    return 1
  fi

  echo "selftest: PASS"
}

if [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
  usage
  exit 0
fi

if [ "${1:-}" = "--selftest" ]; then
  run_selftest
  exit $?
fi

cd "$REPO_ROOT"
if check_parity Dockerfile README.md scripts/gate-build.sh; then
  echo "check-feature-parity: PASS — gate, Dockerfile, and README install agree"
else
  exit 1
fi
