# Coordinating agents

Sheltie can keep coding agents off each other's toes before they write code. It predicts which
build targets an issue will touch, and lets only one agent at a time work on issues that
overlap. Pull requests are still serialized at merge time; this avoids the wasted work.

## Setup

1. Let Sheltie see your build graph: call the reusable footprint workflow on pushes to the
   default branch too (see the header of `.github/workflows/footprint.yml`). Each push uploads
   the graph to `POST /v1/graphs`. Only runs for a push to the default branch are accepted.
2. The GitHub App needs **Issues: write** and the **issues** event (see
   `deploy/github-app-manifest.json`). Existing installations must accept the new permission.
3. For agents that call Sheltie, create a token per agent:

   ```sh
   sheltie-server token create --repo acme/widgets --name agent-a
   ```

   The token (`shl_…`) is printed once; Sheltie stores only its SHA-256. The name is who holds
   the agent's leases. Revoke with `sheltie-server token revoke --repo acme/widgets --name agent-a`.

## Agents with MCP

The MCP server is at `/mcp` (Streamable HTTP, stateless). For Claude Code:

```sh
claude mcp add --transport http sheltie https://sheltie.example.com/mcp \
  --header "Authorization: Bearer shl_…"
```

Tools:

| Tool | What it does |
| --- | --- |
| `safe_issues` | Open issues that conflict neither with work in progress nor with each other |
| `claim_issue` | Claims an issue. `granted`/`renewed`: go ahead. `blocked`/`taken`: pick another one |
| `predict_footprint` | Targets a task will write and affect, from `paths` or an `issue` |
| `release` | Gives up a claim |

Set `SHELTIE_PUBLIC_HOSTS` to the server's public host name(s): the MCP endpoint rejects other
`Host` headers (DNS-rebinding protection).

## Agents assigned on GitHub

Agents that work from issue assignments (no MCP) are coordinated through the issue itself:

- Assigning an issue claims it for the assignee.
- If it overlaps an issue someone else holds, Sheltie labels it `sheltie: blocked` and
  comments which issue it waits for. When that work lands, Sheltie claims it for the assignee,
  removes the label and comments again.
- Unassigning releases the claim.
- New issues get a comment with their predicted footprint.

Sheltie cannot stop an agent that ignores the label; its pull request is still serialized at
merge time.

## REST

The same operations, with the same token: `GET /v1/issues/safe`, `POST /v1/leases`
(`{"issue": N}`; 409 when blocked or taken), `DELETE /v1/leases/{issue}`,
`GET /v1/footprint?paths=a,b` or `?issue=N`.

## How it decides

- **Prediction.** File paths, `module::paths` and `` `identifiers` `` in the issue's title and
  description are matched against the default branch's graph. Written targets are assumed to
  change their interfaces, so their direct readers count as affected. A new file belongs to
  the targets of its directory. If nothing matches, the footprint is unknown: the issue can
  be claimed but is never listed as safe.
- **Leases.** Two issues conflict when one writes what the other writes or reads. A claim is
  checked against all active leases atomically. Leases last `SHELTIE_LEASE_TTL_HOURS` (default
  4) and renew when claimed again. They end on `release`, on expiry, when the issue closes,
  or when a pull request that closes it (`Fixes #N`) lands.
