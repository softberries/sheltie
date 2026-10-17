#!/usr/bin/env bash
#
# backup.sh: daily dump of the demo instance's database.
# Output: /var/backups/sheltie/<YYYYmmddTHHMMZ>/sheltie.dump, kept 14 days.
# Run by sheltie-backup.timer; by hand: sudo /opt/sheltie/deploy/demo/backup.sh
set -euo pipefail
ROOT=/var/backups/sheltie
STAMP="$(date -u +%Y%m%dT%H%MZ)"
DEST="${ROOT}/${STAMP}"
COMPOSE=(docker compose -f /opt/sheltie/compose.yaml -f /opt/sheltie/deploy/demo/compose.demo.yaml)

umask 077
mkdir -p "$DEST"
"${COMPOSE[@]}" exec -T postgres pg_dump -U sheltie -d sheltie -Fc --no-owner --no-privileges > "${DEST}/sheltie.dump"

find "$ROOT" -mindepth 1 -maxdepth 1 -type d -name '20*' -mtime +14 -exec rm -rf {} +
echo "backup ok: $(du -sh "$DEST" | cut -f1) in $DEST"
