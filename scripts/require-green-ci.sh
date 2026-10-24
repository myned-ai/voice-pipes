#!/usr/bin/env bash
# Wait for the core-tests workflow on a commit and fail unless it passed.
#
# Publishing jobs (crates, images, the release) run this first, so nothing
# ships from a commit whose tests did not pass. A tag push starts core-tests
# for the same commit, so a run still in progress is waited for rather than
# treated as missing.
#
# Usage: require-green-ci.sh <commit-sha>
# Needs: GH_TOKEN with `actions: read`, GITHUB_REPOSITORY (set on runners).
# Optional: CI_WAIT_MINUTES (default 120), CI_WORKFLOW (default core-tests.yml).
set -euo pipefail

sha="${1:?usage: require-green-ci.sh <commit-sha>}"
workflow="${CI_WORKFLOW:-core-tests.yml}"
deadline=$(( $(date +%s) + ${CI_WAIT_MINUTES:-120} * 60 ))

while :; do
  # Every run of the workflow on this commit, newest first: a re-run that
  # passed counts even if an earlier attempt failed.
  runs=$(gh api "repos/${GITHUB_REPOSITORY}/actions/workflows/${workflow}/runs?head_sha=${sha}&per_page=20" \
    --jq '.workflow_runs[] | "\(.status) \(.conclusion // "none") \(.html_url)"')
  if grep -q '^completed success ' <<<"$runs"; then
    echo "${workflow} passed on ${sha}: $(grep -m1 '^completed success ' <<<"$runs" | cut -d' ' -f3)"
    exit 0
  fi
  if [ -n "$runs" ] && ! grep -qv '^completed ' <<<"$runs"; then
    echo "${workflow} finished on ${sha} without passing:" >&2
    echo "$runs" >&2
    exit 1
  fi
  if [ "$(date +%s)" -ge "$deadline" ]; then
    echo "no passing ${workflow} run on ${sha} within the wait; runs seen:" >&2
    echo "${runs:-none}" >&2
    exit 1
  fi
  sleep 30
done
