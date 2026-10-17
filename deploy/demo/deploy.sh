#!/usr/bin/env bash
#
# deploy.sh: redeploy the demo instance from its git clone at /opt/sheltie.
#   sudo /opt/sheltie/deploy/demo/deploy.sh [git-ref]   (default: origin/main)
# Fetches the ref, builds the image on the host, recreates changed containers (the server applies
# migrations at start) and updates the shared Caddy site block if it changed.
set -euo pipefail
cd /opt/sheltie
REF="${1:-origin/main}"
COMPOSE=(docker compose -f compose.yaml -f deploy/demo/compose.demo.yaml)

git fetch --prune origin
git checkout -q --detach "$REF"
echo "deploying $(git rev-parse --short HEAD): $(git log -1 --format=%s)"

"${COMPOSE[@]}" build sheltie
"${COMPOSE[@]}" up -d --remove-orphans
"${COMPOSE[@]}" ps

# Shared Caddy: update the site block if it changed. Creating it is the go-live step
# (deploy/demo/README.md), never done here. A config that does not validate is not installed.
SITE_SRC=deploy/demo/sheltie.caddy
SITE_DST=/opt/caddy/conf/sites/sheltie.caddy
CADDY=(docker compose -f /opt/caddy/docker-compose.yml exec -T caddy)
if [ -f "$SITE_DST" ] && ! cmp -s "$SITE_SRC" "$SITE_DST"; then
  cp "$SITE_DST" "$SITE_DST.prev"
  install -m 0644 "$SITE_SRC" "$SITE_DST"
  if "${CADDY[@]}" caddy validate --config /etc/caddy/Caddyfile >/dev/null; then
    "${CADDY[@]}" caddy reload --config /etc/caddy/Caddyfile
    echo "caddy site block updated and reloaded"
  else
    mv "$SITE_DST.prev" "$SITE_DST"
    echo "new caddy site block does not validate; kept the old one" >&2
    exit 1
  fi
  rm -f "$SITE_DST.prev"
fi
