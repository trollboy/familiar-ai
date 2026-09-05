#!/bin/bash
# PRD-092 acceptance criterion 2: no documented invocation in the README,
# the scripts directory, or the operator documentation carries
# `--no-default-features` as a workaround. This scans exactly those three
# places for a `cargo ...--no-default-features...` invocation and fails on
# any occurrence that does not declare why, so the flag cannot quietly creep
# back into a doc line and become permanent again (which is how it got here
# the first time).
#
# Declaring a reason: put `workaround-ok: <reason>` on the offending line or
# the line immediately above it. Absent that, any match is a failure.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/check-no-workaround-flags.sh
       scripts/check-no-workaround-flags.sh --selftest

Scans README.md, scripts/, and SPECTRA_AUTONOMOUS_HANDOVER.md (the operator
handover doc) for a documented `cargo ... --no-default-features` invocation.
Fails listing every match that has no `workaround-ok: <reason>` marker on its
own line or the line above it.

--selftest exercises the scan against synthetic fixtures (an undeclared
match, and a declared one) instead of the real repository, so the pass/fail
behaviour is pinned independent of whatever the docs currently say.
EOF
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Prints one "path:lineno" per undeclared `cargo ... --no-default-features`
# invocation found under $1 (a directory or a single file).
scan() {
  local target="$1"
  local files=()
  if [ -f "$target" ]; then
    files=("$target")
  else
    while IFS= read -r -d '' f; do
      # The check-*.sh family enforces this rule; it is not itself a
      # documented invocation a user would run, and its fixtures and
      # prose legitimately reference the flag under test.
      case "$(basename "$f")" in
        check-*.sh) continue ;;
      esac
      files+=("$f")
    done < <(find "$target" -type f -print0)
  fi

  local f lineno line prev violations=0
  for f in "${files[@]}"; do
    lineno=0
    prev=""
    while IFS= read -r line || [ -n "$line" ]; do
      lineno=$((lineno + 1))
      if [[ "$line" == *cargo*--no-default-features* ]]; then
        if [[ "$line" == *workaround-ok:* || "$prev" == *workaround-ok:* ]]; then
          : # declared — allowed
        else
          echo "$f:$lineno"
          violations=$((violations + 1))
        fi
      fi
      prev="$line"
    done < "$f"
  done
  return $((violations > 0 ? 1 : 0))
}

run_selftest() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN

  printf 'cargo build --no-default-features -p familiar-ai-daemon\n' > "$tmp/undeclared.md"
  if scan "$tmp/undeclared.md" > /tmp/.selftest-out 2>&1; then
    echo "selftest FAILED: an undeclared occurrence was not flagged" >&2
    return 1
  fi
  grep -q "undeclared.md:1" /tmp/.selftest-out

  printf '<!-- workaround-ok: pinned CI image lacks libfoo, tracked in FAM-BUG-999 -->\ncargo build --no-default-features -p familiar-ai-daemon\n' > "$tmp/declared.md"
  if ! scan "$tmp/declared.md" > /dev/null 2>&1; then
    echo "selftest FAILED: a declared occurrence was wrongly flagged" >&2
    return 1
  fi

  printf 'cargo build -p familiar-ai-daemon --bin familiar-ai\n' > "$tmp/clean.md"
  if ! scan "$tmp/clean.md" > /dev/null 2>&1; then
    echo "selftest FAILED: a clean file was wrongly flagged" >&2
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
found=0
out="$(mktemp)"
trap 'rm -f "$out"' EXIT

for target in README.md scripts SPECTRA_AUTONOMOUS_HANDOVER.md; do
  if [ -e "$target" ] && ! scan "$target" >> "$out"; then
    found=1
  fi
done

if [ "$found" -ne 0 ]; then
  echo "check-no-workaround-flags: FAIL — undeclared --no-default-features invocation(s):" >&2
  sed 's/^/  /' "$out" >&2
  echo "Declare a reason with 'workaround-ok: <reason>' on the line or the line above, or remove the flag." >&2
  exit 1
fi

echo "check-no-workaround-flags: PASS — no undeclared --no-default-features invocations"
