#!/usr/bin/env bash
# Creates one GitHub issue per open task in docs/backlog.md, labelled by milestone.
# Usage: scripts/create-issues.sh [owner/repo] [--dry-run]
# Requires: gh (authenticated). Tasks already marked [x] are skipped.
set -euo pipefail

repo="${1:-}"
dry_run=false
[[ "${2:-}" == "--dry-run" || "${1:-}" == "--dry-run" ]] && dry_run=true
[[ "$repo" == "--dry-run" ]] && repo=""
repo_args=()
[[ -n "$repo" ]] && repo_args=(--repo "$repo")

backlog="$(dirname "$0")/../docs/backlog.md"
milestone=""
title_prefix=""

ensure_label() {
  $dry_run && return 0
  gh label create "$1" --color "$2" --force "${repo_args[@]}" >/dev/null
}

ensure_label task 0E8A16

while IFS= read -r line; do
  if [[ "$line" =~ ^\*\*(M[0-9.]+)\ —\ (.*)\*\*$ ]]; then
    milestone="${BASH_REMATCH[1]}"
    title_prefix="${BASH_REMATCH[2]}"
    ensure_label "$milestone" 1D76DB
    continue
  fi
  if [[ -n "$milestone" && "$line" =~ ^-\ \[\ \]\ (.*)$ ]]; then
    task="${BASH_REMATCH[1]}"
    title="[$milestone] ${task%% \**}"
    body="Milestone **$milestone — $title_prefix**.

Task: $task

See \`docs/design.md\` (Implementation plan, $milestone) for context and the milestone's \"done when\" criterion, and follow \`CLAUDE.md\`.

## Done when

<!-- Fill in the concrete test or observable result before starting. -->"
    if $dry_run; then
      echo "would create: $title"
    else
      gh issue create "${repo_args[@]}" --title "$title" --body "$body" --label "$milestone" --label task
    fi
  fi
done < "$backlog"
