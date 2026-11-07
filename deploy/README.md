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

4. **Build the image and run the daemon.** Build `tnld` from the root
   [`Dockerfile`](../Dockerfile), then start it with the reference Compose service
   ([`tnld-compose.yaml.example`](./tnld-compose.yaml.example) — host networking,
   mounts `/etc/tnld`, healthcheck):
   ```bash
   docker build -t tnld:latest .
   docker compose -f deploy/tnld-compose.yaml.example up -d
   ```

5. **Connect a client and smoke-test:**
   ```bash
   tnl auth login --endpoint https://tnl-api.t.example.com --token tnl_…
   python3 -m http.server 9999 &
   tnl http 9999 smoke
   curl -sf https://smoke.t.example.com/
   ```

## Visitor IP attribution and trusted proxies

The daemon trusts **no forwarding headers** unless configured with explicit
`trusted_proxies` CIDRs. With the checked-in host-network/loopback topology, opt
in to `trusted_proxies = ["127.0.0.1/32"]` only after confirming Caddy's actual
upstream peer. This also trusts any local process that can reach the daemon.
Add `::1/128` only for an actual IPv6 proxy hop. Do not infer trust from private
networks, the listen address, or an assumed container subnet.

`TNLD_TRUSTED_PROXIES="127.0.0.1/32,::1/128"` replaces the complete file list;
an empty/whitespace value disables it. Entries require a prefix and no host bits;
invalid file/resolved/programmatic entries fail startup. `/0` is permitted with
a warning but strongly discouraged: it removes the untrusted-peer boundary.
IPv4-mapped IPv6 peers are normalized to IPv4 and require IPv4 CIDRs, even if
`::/0` is configured.

Only a **single** `X-Forwarded-For` field from a trusted socket peer is used.
Its complete chain (maximum 4096 bytes / 32 bare IPs) must be valid. Scan from the
right past trusted hops and stop at the first untrusted address; claims farther
left cannot override it. Missing, duplicate, malformed, oversized, or all-trusted
chains fall back to the socket peer with explicit provenance. Native IPv6 and
private/ULA addresses are allowed. `Forwarded` and `X-Real-IP` never affect tnl's
identity. Bad forwarding metadata does not reject an otherwise valid request.

Backend application headers remain ordinary pass-through headers, not sanitized
authorization inputs. This feature secures tnl's attribution, not applications
that blindly trust inbound forwarding headers. IPs are personal data; log
retention/export policy is operator-owned.

Before a production claim, verify the actual intended Caddy version/topology.
The ignored credential-free fixture exercises single-edge defaults and a
separate deliberately append-only synthetic hop, checks observed XFF order and
the authenticated control capability echo, and binds its visitor to 127.0.0.2:

```sh
TNL_TEST_CADDY_BIN="$(command -v caddy)" cargo test -p tnl-e2e --test visitor_ip_caddy -- --ignored --nocapture
```

It requires an **already installed** local binary; no download, DNS, ACME,
production credentials, or deployment is performed. It prints the version and
test-only effective Caddyfile. Do not publish the fixture's incoming-XFF
preservation as safe production upstream-trust configuration. A missing binary
or ignored test is not ingress proof.

## Application WebSockets

Classic HTTP/1.1 WebSocket upgrades use the same tunnel host and paths as HTTP.
This is separate from the owner's authenticated `/control` WSS transport. Deploy
an upgrade-capable daemon **and** the updated Caddyfile together: do not add
`header_up Connection close` to the wildcard site. Caddy handles the upgrade;
`flush_interval -1` remains for streaming HTTP, and the more-specific API site
remains separate. Existing explicit daemon routes (including `/control`,
`/whoami`, `/healthz`, and admin routes) are reserved, not backend WS paths.

Public `wss://` terminates TLS at Caddy; the configured local backend still uses
plain HTTP/TCP. Only classic WebSocket upgrades are supported, not generic
Upgrade, CONNECT, h2c, or HTTP/2 extended CONNECT. The daemon validates the
handshake then relays opaque bytes with bounded buffers. It does not interpret,
capture, or replay WebSocket frames, decompress extensions, or synthesize pings.
The existing client request summary is emitted at connection close, not live for
each frame. A backend non-101 response (including auth/Origin refusal) remains a
normal streamed HTTP response; it is never retried to bypass rejection.

Origin and visitor Authorization/Cookie are passed through. The existing
`--host-header` modes change only Host, **not Origin**: configure the backend's
allowed origins explicitly rather than assuming Host rewriting grants access.
The tunnel owner's `/control` token does not authenticate visitors. Backend
Origin/auth checks remain essential; this feature does not add visitor policy.

An upgrade has a 30-second complete-response deadline and a 5-second visitor
socket rendezvous deadline. After 101, errors only close the connection; no HTTP
error body is injected. There is no new idle/max-age timer or half-close drain
timeout. An uncooperative half-open peer can persist until session/process loss.
Reconnect creates new connections, never resumes an existing WebSocket. Caddy
reloads and deployments may disconnect upgraded connections; applications must
handle reconnect themselves.

The source tests exercise the daemon and real tunnel directly. Actual Caddy
runtime behavior, public TLS/browser WSS, reload behavior, and combined rollout
remain separate release gates; see the local fixture in
[`../docs/RUNBOOK.md`](../docs/RUNBOOK.md#application-websocket-release-check).

Handshake framing validation operates on Hyper's normalized HTTP headers:
repeated identical `Content-Length: 0` lines can arrive as one zero-length
field and are accepted. Nonzero, conflicting, malformed framing and transfer
encoding are rejected; no HTTP handshake body is forwarded as WebSocket data.
This is not a guarantee about singleton cardinality of the original wire bytes.
