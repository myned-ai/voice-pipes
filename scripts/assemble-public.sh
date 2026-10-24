#!/usr/bin/env bash
# assemble-public.sh — build the public release tree from an allow-list.
#
# Decision D7 of PLAN-OSS-DOCS-AND-RELEASE-PACKAGING: the public repository is
# initialised once from the output of this script (fresh history), after which
# it becomes the development repo and internal material moves to a separate
# private repo. Until then, re-run this whenever you want to see exactly what
# would ship.
#
# Only *tracked* files are considered (git ls-files), so local models, build
# output, caches and .env files can never be copied by accident. Tracked
# files then pass the allow-list (top-level paths that ship) and the
# deny-list (internal material under those paths).
#
# Usage:
#   scripts/assemble-public.sh <checkout> <output-dir>
#
# The output directory must not exist (refuses to overwrite). Exit 0 only if
# the copy succeeded AND scripts/leak-check.sh passes on the result.
set -euo pipefail

SRC="${1:-}"
OUT="${2:-}"
if [[ -z "$SRC" || -z "$OUT" ]]; then
  echo "usage: $0 <checkout> <output-dir>" >&2
  exit 2
fi
SRC="$(cd "$SRC" && pwd)"
if [[ -e "$OUT" ]]; then
  echo "refusing to overwrite existing output dir: $OUT" >&2
  exit 2
fi
if [[ -n "$(git -C "$SRC" status --porcelain)" ]]; then
  echo "warning: source checkout has uncommitted changes; tracked files are copied from the working tree, not HEAD" >&2
fi

# ---------------------------------------------------------------------------
# Allow-list: top-level paths that ship. Anything not listed never leaves.
# ---------------------------------------------------------------------------
ALLOW_RE='^(LICENSE|NOTICE|README\.md|CONTRIBUTING\.md|CODE_OF_CONDUCT\.md|SECURITY\.md|CHANGELOG\.md|AGENTS\.md|CLAUDE\.md|llms\.txt|Cargo\.toml|Cargo\.lock|clippy\.toml|deny\.toml|dist-workspace\.toml|\.gitignore|\.dockerignore|book\.toml|launch-stack\.sh|launch-stack\.ps1|rebuild-stack\.ps1)$|^(\.github|docs|examples|scripts|skills|supply-chain|voice-pipes(-[a-z0-9-]+)?)/'

# ---------------------------------------------------------------------------
# Deny-list: tracked paths under the allow-list that are internal anyway.
# ---------------------------------------------------------------------------
DENY_RE='^(docs/internal/|voice-pipes-admin/frontend/scripts/)|(^|/)(__pycache__/|[^/]*_pb2(_grpc)?\.py$)'

mkdir -p "$OUT"
echo "== selecting tracked files from $SRC"
mapfile -t files < <(git -C "$SRC" ls-files | grep -E "$ALLOW_RE" | grep -Ev "$DENY_RE")
echo "   ${#files[@]} files selected"

echo "== copying"
# tar preserves modes (executable bits on .sh) and creates directories.
( cd "$SRC" && printf '%s\n' "${files[@]}" | tar -cf - -T - ) | ( cd "$OUT" && tar -xf - )

echo "== summary"
total=$(find "$OUT" -type f | wc -l | tr -d ' ')
md=$(find "$OUT" -type f -name '*.md' | wc -l | tr -d ' ')
echo "   files: $total   markdown: $md"
echo "   top level:"
ls -1A "$OUT" | sed 's/^/     /'
for must_not in docs/internal marketing voice-pipes/models voice-pipes/captures; do
  if [[ -e "$OUT/$must_not" ]]; then
    echo "ERROR: $must_not present in output" >&2
    exit 1
  fi
done

# Every tracked top-level path either ships or is named here as internal;
# a new one has to be sorted into one list or the other on purpose.
echo "== every tracked top-level path is accounted for"
INTERNAL_TOP_RE='^marketing$'
unsorted=$(git -C "$SRC" ls-files | cut -d/ -f1 | sort -u \
  | grep -Ev "$INTERNAL_TOP_RE" \
  | while read -r top; do
      [[ -e "$OUT/$top" ]] || echo "$top"
    done)
if [[ -n "$unsorted" ]]; then
  echo "ERROR: tracked top-level paths neither shipped nor listed as internal:" >&2
  echo "$unsorted" | sed 's/^/   /' >&2
  exit 1
fi

# The tree a stranger clones has to be a whole workspace: every member
# crate and every path dependency present.
echo "== the assembled workspace resolves"
( cd "$OUT" && cargo metadata --no-deps --locked --format-version 1 >/dev/null )

echo "== leak check on the assembled tree"
bash "$SRC/scripts/leak-check.sh" "$OUT"

echo
echo "assembled public tree at: $OUT"
echo "next: cd \"$OUT\" && git init && git add -A && git commit -m 'Voice Pipes 0.1.0 - first public release'"
