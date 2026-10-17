# Configuration

`sheltie-server` reads its settings from environment variables, and from a `.env` file in the
working directory if there is one.

| Variable | Default | Meaning |
| --- | --- | --- |
| `DATABASE_URL` | (required) | `postgres://user:password@host:port/database`. Migrations run on startup. |
| `SHELTIE_BIND` | `127.0.0.1:8080` | Address to listen on. The container image sets `0.0.0.0:8080`. |
| `SHELTIE_GITHUB_APP_ID` | | The App's numeric id. |
| `SHELTIE_GITHUB_PRIVATE_KEY_PATH` | | Path to the App's private key (PEM). |
| `SHELTIE_GITHUB_WEBHOOK_SECRET` | | The App's webhook secret. |
| `SHELTIE_GITHUB_API_URL` | `https://api.github.com` | For GitHub Enterprise Server. |
| `SHELTIE_OIDC_AUDIENCE` | `sheltie` | Audience CI tokens must be issued for (the workflows' `audience` input). |
| `SHELTIE_OIDC_ISSUER` | GitHub Actions on github.com | OIDC issuer of CI tokens. |
| `SHELTIE_EPOCH_SECONDS` | `10` | Time between scheduler epochs (1–3600). |
| `SHELTIE_LEASE_TTL_HOURS` | `4` | How long an agent's claim on an issue lasts unless renewed (1–168). |
| `SHELTIE_PUBLIC_HOSTS` | | Comma-separated public host names; the MCP endpoint rejects other `Host` headers. |
| `RUST_LOG` | `info` | Log filter, e.g. `info,sheltie_server=debug`. |

Set all three `SHELTIE_GITHUB_*` variables, or none of them. Without them the server serves
only `/healthz`.

`compose.yaml` also reads `SHELTIE_POSTGRES_PASSWORD`, `SHELTIE_UID`, `SHELTIE_PORT`,
`SHELTIE_IMAGE` and `SHELTIE_GITHUB_KEY_FILE` from `.env`.

## Per repository

Each repository starts in shadow mode. Change it with the CLI, which uses the same
`DATABASE_URL`:

```sh
sheltie-server repo set-mode --repo owner/name --mode merge         # or shadow
sheltie-server repo set-merge-engine --repo owner/name --state on   # or off
sheltie-server token create --repo owner/name --name agent-a        # agent token, printed once
sheltie-server token revoke --repo owner/name --name agent-a
sheltie-server shadow-report --repo owner/name --days 7
sheltie-server stats --days 30
```

Merge rules and intent replay are configured in the repository itself, in
`.sheltie/merge.toml` on the default branch: see [Merge rules](merge-rules.md).

## GitHub App permissions

From `deploy/github-app-manifest.json`:

- Permissions:
  - Checks: write
  - Contents: write
  - Issues: write
  - Metadata: read
  - Pull requests: write
  - Commit statuses: read
- Events: `check_run`, `check_suite`, `issues`, `pull_request`, `pull_request_review`, `push`, `status`.
