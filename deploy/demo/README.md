# The demo instance (demo.sheltie.dev)

The public demo runs the root `compose.yaml` on a VPS it shares with other services, behind a
shared Caddy that terminates TLS for every site on the host. This directory holds only what is
specific to that host; a normal self-hosted setup needs none of it (see `docs/self-hosting.md`).

```
/opt/caddy/conf/sites/sheltie.caddy   copy of sheltie.caddy (the shared Caddy imports sites/*.caddy)
/opt/sheltie/                         git clone of this repository, compose project "sheltie"
  .env                                settings and secrets (root, mode 600, git-ignored)
  sheltie-github-app.pem              the App key (uid 65532, mode 600, git-ignored)
/var/backups/sheltie/                 daily database dumps (sheltie-backup.timer)
```

The server listens on `127.0.0.1:8080` and Postgres is not published. `compose.demo.yaml` caps
memory, CPU, compile jobs and log size so a build or a busy queue cannot starve the other
services on the host. The host pulls this repository with a read-only deploy key
(`/root/.ssh/sheltie_deploy`, host alias `github-sheltie`).

## Deploy

```bash
sudo /opt/sheltie/deploy/demo/deploy.sh            # origin/main
sudo /opt/sheltie/deploy/demo/deploy.sh <sha|tag>  # a specific ref
```

Run it detached when your SSH session might drop (a deploy killed halfway through can leave a
renamed container behind):
`sudo systemd-run --unit sheltie-deploy --wait /opt/sheltie/deploy/demo/deploy.sh`, and follow it
with `journalctl -u sheltie-deploy -f`.

It builds the image on the host (a few minutes cold), recreates changed containers and, if
`sheltie.caddy` changed, validates and reloads the shared Caddy. The server applies migrations at
start. To roll back, deploy the previous commit by sha; migrations only add, so an older binary
runs against a newer schema, but check `crates/sheltie-store/migrations` first.

## First install

1. DNS: an A record `demo.sheltie.dev` pointing at the host.
2. Register the App from a workstation, in a directory without an existing `.env` or key:
   `scripts/register-github-app.py --url https://demo.sheltie.dev --name "Sheltie Demo"`.
   Copy the resulting `.env` and `sheltie-github-app.pem` to `/opt/sheltie` without printing them,
   then set `SHELTIE_UID=65532` in `.env` and `chown 65532 sheltie-github-app.pem`.
3. `sudo /opt/sheltie/deploy/demo/deploy.sh`, then check `curl -s 127.0.0.1:8080/healthz`.
4. Go live: copy `sheltie.caddy` to `/opt/caddy/conf/sites/`, then
   `docker compose -f /opt/caddy/docker-compose.yml exec caddy caddy validate --config /etc/caddy/Caddyfile`
   and, if it is valid, the same command with `reload`. Caddy issues the certificate.
5. Backups: copy `sheltie-backup.{service,timer}` to `/etc/systemd/system/` and
   `systemctl enable --now sheltie-backup.timer`.

## Operate

```bash
C="docker compose -f /opt/sheltie/compose.yaml -f /opt/sheltie/deploy/demo/compose.demo.yaml"
sudo $C ps
sudo $C logs -f --tail=200 sheltie
curl -s https://demo.sheltie.dev/healthz
sudo $C exec -T postgres pg_restore -U sheltie -d sheltie --clean --if-exists --no-owner < sheltie.dump
```

Settings change: edit `/opt/sheltie/.env`, then `sudo $C up -d` (containers read it at start).
Never run `down -v`: it deletes the `sheltie_postgres` volume.
