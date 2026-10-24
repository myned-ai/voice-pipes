#!/usr/bin/env bash
# leak-check.sh — fail if internal names, personal paths, account ids or
# pricing figures appear in the *public* part of the tree.
#
# Runs in CI (core-tests.yml) and as the last step of assemble-public.sh.
# Only files that would ship are scanned: inside a git checkout that means
# tracked files (so local .env, caches and benchmark output never trigger
# it); on an assembled tree (no .git) every file is scanned.
#
# Usage:
#   scripts/leak-check.sh [ROOT]      # default: repo root
#
# Exit 0 = clean, 1 = findings printed to stdout.
set -euo pipefail

ROOT="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
cd "$ROOT"

# Patterns are grep -E, case-insensitive. Keep in sync with
# docs/internal/plans/PLAN-OSS-DOCS-AND-RELEASE-PACKAGING.md §WS-A3.
# Note: `myned-ai/<model>` Hugging Face ids are public model references and
# are deliberately NOT matched; only GitHub links to the private org are.
PATTERNS=(
  'github\.com/myned-ai($|[^/]|/($|[^v])|/v($|[^o]))'  # private repos in the org; the public voice-pipes repo is allowed
  'Projects[/\\]spike'          # old personal checkout path
  'OneDrive'                    # personal cloud path
  'Users[/\\]Goran'             # personal home dir
  'agent_01jz'                  # leaked ElevenLabs agent id prefix
  '\$50K|\$500K|50K–500K'       # enterprise pricing figures
  'nyxclaw|openclaw|zeroclaw'   # private integration code names
  'tesla avatar|tesla-avatar'   # private integration code name
)

# Paths that are internal by design and never copied to the public repo,
# plus the files that legitimately contain the patterns (this script, the
# migration tool, lockfiles).
EXCLUDE_RE='^(docs/internal/|marketing/|voice-pipes/VibeVoice/|scripts/leak-check\.sh$|scripts/assemble-public\.sh$|scripts/move-docs.py$|.*/Cargo\.lock$|.*/package-lock\.json$|Cargo\.lock$)'

# Text that ships.
INCLUDE_RE='\.(md|toml|ya?ml|json|rs|py|tsx?|jsx?|sh|ps1|html|txt|env\.example)$|(^|/)Dockerfile[^/]*$'

list_files() {
  if git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    git -C "$ROOT" ls-files
  else
    find . -type f -not -path './.git/*' -not -path '*/node_modules/*' -not -path '*/target/*' | sed 's|^\./||'
  fi
}

joined=$(IFS='|'; echo "${PATTERNS[*]}")
mapfile -t files < <(list_files | grep -Ev "$EXCLUDE_RE" | grep -E "$INCLUDE_RE" || true)

if [[ ${#files[@]} -eq 0 ]]; then
  echo "leak-check: no files to scan under $ROOT" >&2
  exit 2
fi

out=$(printf '%s\0' "${files[@]}" | xargs -0 grep -niE -- "$joined" 2>/dev/null || true)

if [[ -n "$out" ]]; then
  echo "leak-check: FOUND internal references in ${#files[@]} scanned files:"
  echo "$out" | sed 's/^/  /'
  echo
  echo "Either move the file under docs/internal/ (never copied) or remove the reference."
  exit 1
fi
echo "leak-check: OK - ${#files[@]} files scanned, no internal references."
