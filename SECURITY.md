# Security policy

Sheltie holds merge rights on protected branches and runs agent jobs with user-provided API keys,
so we take security reports seriously.

## Reporting a vulnerability

Report privately through GitHub's
[security advisory form](https://github.com/softberries/sheltie/security/advisories/new),
or email **security@softberries.dev**. Please do not open public issues for security problems.

Include what you found, how to reproduce it, and the impact you expect. We aim to acknowledge reports
within 3 working days and to agree on a disclosure timeline with you.

## Supported versions

Sheltie is 0.x. Only the latest release and `main` receive fixes.

## Scope

Of particular interest:

- Webhook signature verification, GitHub App key handling, and OIDC verification of CI uploads
- Anything that lets a PR bypass verification or land on a protected branch
- Parsing of `.sheltie/merge.toml` and `sheltie-intent` blocks from untrusted PRs
- The intent replay job: escaping its sandbox, or reaching the API key or a write token
- Agent tokens, leases and the MCP endpoint
- Leakage of API keys, tokens or private repository content
