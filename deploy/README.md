# Deploying tnl

Stand up your own tnl server. Examples use `example.com` — substitute your own
domain. For a no-domain local smoke test instead, see
[`../docs/RUNBOOK.md`](../docs/RUNBOOK.md) §1.

## Prerequisites

- A domain you control, with a DNS provider that has an API token (for ACME DNS-01).
- A wildcard DNS record `*.t.example.com` → the host that will run Caddy.
- Docker on that host (the daemon binds `127.0.0.1:7777`; Caddy fronts it).

## Steps

1. **Configure.** Copy the env template and fill in the four values:
   ```bash
   cp deploy/.env.example deploy/.env
   $EDITOR deploy/.env       # TNL_DOMAIN, ACME_EMAIL, TNL_DNS_PROVIDER, TNL_DNS_TOKEN
   ```

2. **Build Caddy with your DNS provider's plugin.** Wildcard certs need the ACME
   DNS-01 challenge, which needs your provider's module compiled in. Edit the
   `--with` line in [`Caddy.Dockerfile`](./Caddy.Dockerfile) for your provider (see
   <https://github.com/caddy-dns>), then build it. Run Caddy with the generic
   [`Caddyfile`](./Caddyfile) and your `.env` — it reads `$TNL_DOMAIN` and the
   `$TNL_DNS_*` values from the environment, so it works unmodified for any operator.

3. **Generate the server config + your first token:**
   ```bash
   tnld init                 # prompts for public URL + wildcard domain
   ```
   It writes `config.toml`, mints an admin token, prints it once, and prints the
   exact `tnl auth login …` line and a Caddy snippet to copy.
   (An annotated [`config.example.toml`](./config.example.toml) documents every field.)

4. **Run the daemon.** [`tnld-compose.yaml.example`](./tnld-compose.yaml.example) is a
   reference Compose service (host networking, mounts `/etc/tnld`, healthcheck):
   ```bash
   docker compose -f deploy/tnld-compose.yaml.example up -d
   ```

5. **Connect a client and smoke-test:**
   ```bash
   tnl auth login --endpoint https://tnl-api.t.example.com --token tnl_…
   python3 -m http.server 9999 &
   tnl http 9999 smoke
   curl -sf https://smoke.t.example.com/
   ```
