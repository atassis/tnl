# Design: seamless-but-secure dev-server friction smoothing

- **Date:** 2026-07-19
- **Status:** approved (pending spec review)
- **Scope:** one implementation plan

## Problem

A tunnel puts a local backend behind a new public origin
(`https://<sub>.t.atassis.ru`). That breaks assumptions the backend makes about
its environment, producing friction that today surfaces as opaque errors:

- **Host allowlists.** Dev servers reject an unknown `Host` for DNS-rebinding
  protection: Vite/Astro `server.allowedHosts` (`403 Blocked request`),
  webpack-dev-server (`403 Invalid Host header`), Django `ALLOWED_HOSTS`
  (`400 DisallowedHost`), Rails `config.hosts` (`403 Blocked host`). This is the
  case that made a tunnel to an Astro dev server on `:4321` return a bare `403`
  the user could not interpret.
- **Proto confusion.** The backend sees `http` (TLS terminates at Caddy), so it
  may emit `http://localhost` absolute URLs, refuse to set `Secure` cookies, or
  redirect-loop to https.

The established tools (ngrok `--host-header=rewrite`, cloudflared
`httpHostHeader`) address only the Host case, only manually, and only if you
already know the flag before you hit the wall.

## Principle: the security dividing line

Two categories of "block", handled oppositely:

- **Category 1 — the backend doesn't know it's behind a proxy.** Host
  allowlists, http↔https confusion, client IP. These misfire *only* because the
  backend cannot see the real proxy chain. Smoothing them means telling the
  backend the truth (forwarded headers) or presenting the `Host` it expects.
  This does **not** weaken security — it is correct reverse-proxy behavior, and
  the dev server's guard still applies to non-tunnel traffic. **tnl smooths
  these** (free-and-safe by default; changes-what-the-backend-sees are opt-in).

- **Category 2 — the app protecting the end user.** CORS, CSP, CSRF tokens,
  cookie `Secure`/`SameSite`/`HttpOnly`, the app's own auth. These protect the
  browser/user. Bulldozing them (e.g. injecting `Access-Control-Allow-Origin: *`
  or stripping CSP) silently turns the tunnel into a footgun. **tnl never
  touches these** — it only *explains* them when they block.

Chosen posture: **truthful-by-default, host-rewrite opt-in, never a cryptic
block.**

## Components

### ① Truthful forwarded headers — daemon, default-on, safe

The daemon guarantees the backend receives a correct proxy-chain description
before it forwards the request via hyper:

- `X-Forwarded-Host` — the public host (`<sub>.t.atassis.ru`, i.e. the original
  `Host`). Set if absent.
- `X-Forwarded-Proto` — `https` in production. **Prefer the value the fronting
  proxy set** (Caddy sets `https` on the loopback hop); if absent, default to the
  daemon's own scheme (`http`), which is correct for direct/local use.
- `X-Forwarded-For` — the end-user IP. Prefer the existing value (Caddy sets it);
  if absent, set it to the connecting peer (`ConnectInfo<SocketAddr>`).

- **Location:** `crates/tnld/src/data_plane.rs`, in the request-building block
  before `send_request` (alongside the existing hop-by-hop strip + `Connection:
  close`). Must run *after* hop-by-hop stripping so a `Connection: x-forwarded-*`
  cannot delete them.
- **Trust boundary:** the daemon binds loopback (`127.0.0.1:7777`); only Caddy
  reaches it in production, so preferring the peer's `X-Forwarded-*` is safe.
  Document this assumption; do not trust `X-Forwarded-*` from arbitrary peers if
  the bind address ever changes.
- **Audit note:** because `X-Forwarded-*` are not hop-by-hop, Caddy's values may
  already flow through today. This component *ensures and normalizes* them
  (and covers the no-Caddy local path); part of the work is verifying current
  behavior rather than adding net-new forwarding.

Fixes the http↔https friction class for any app that respects forwarded headers.
No security impact.

### ② `--host-header` on `tnl http` — client, opt-in, default `preserve`

New CLI option controlling the `Host` the **backend** sees:

- `preserve` *(default)* — backend sees `Host: <sub>.t.atassis.ru`, unchanged.
- `rewrite` — backend sees `Host: <backend authority>` (`localhost:<port>` for
  the bare-port form; `ip:port` / `[::1]:port` for an explicit target) → passes
  dev-server host allowlists. `X-Forwarded-Host` still carries the true public
  host, so the app can build correct absolute URLs. This is the "truthful" part
  of the rewrite.
- `<value>` — an explicit host string, verbatim.

- **Model:** `enum HostHeader { Preserve, Rewrite, Fixed(String) }`, parsed from
  the flag (`preserve`/`rewrite` keywords; anything else = `Fixed`).
- **Location:** flag on the `Http` subcommand (`crates/tnl/src/main.rs`) →
  `commands/http.rs` → new `ForwardCtx.host_header` field →
  `crates/tnl/src/forwarder.rs`.
- **Mechanism:** the forwarder already pre-buffers the request head
  (`peek_request_head`). When `host_header != Preserve`, rewrite the `Host:`
  line's value in that head buffer (case-insensitive header name match, preserve
  ordering and the rest of the head) before `tcp.write_all(&head_bytes)`.
- Default `preserve` means tnl never silently changes the backend's view unless
  asked — the security-conservative default the user chose.

### ③ Block detection + framework-aware guidance — client, default-on, advice-only

The forwarder already tees the response status line + first ~512 bytes
(`resp_peek`). A new `crates/tnl/src/block_detect.rs` exposes
`detect(status: u16, body_prefix: &[u8]) -> Option<Block>` backed by an
extensible signature table:

| Framework | Signature (status · body substring) | Hint |
|---|---|---|
| Vite/Astro | 403 · `Blocked request` + `allowedHosts` | add `allowedHosts: ['.t.atassis.ru']` **or** `--host-header=rewrite` |
| webpack-dev-server | 403 · `Invalid Host header` | set `allowedHosts` **or** `--host-header=rewrite` |
| Django | 400 · `DisallowedHost` / `Invalid HTTP_HOST` | add to `ALLOWED_HOSTS` **or** `--host-header=rewrite` |
| Rails | 403 · `Blocked host` | add to `config.hosts` **or** `--host-header=rewrite` |

- On a match the client prints **one** hint to stderr, parameterized with the
  live subdomain, public host, and target port, e.g.:
  `! dev server blocked host 'regal-leaf-69.t.atassis.ru' (Vite allowedHosts).`
  `  fix: add allowedHosts: ['.t.atassis.ru'] to your Vite config, or restart with:`
  `       tnl http 4321 --host-header=rewrite`
- **Dedup:** at most one hint per `(tunnel, framework)` for the life of the
  session — `ForwardCtx` gains an `Arc<Mutex<HashSet<&'static str>>>` of
  already-warned frameworks (or equivalent shared state), since `ForwardCtx` is
  cloned per substream.
- **Never modifies the response** — the backend's own body/status pass through
  untouched; the hint is out-of-band terminal output.
- This module is the future home for Category-2 *explainers* (mixed-content,
  CORS) marked "won't auto-fix, here's why" — out of scope for v1.

## Explicitly out of scope (Category 2, by design)

No auto-CORS, no stripping/relaxing CSP or other security headers, no touching
`Secure`/`SameSite`/`HttpOnly`/CSRF/auth, no `Forwarded` (RFC 7239) header
(future; `X-Forwarded-*` is the de-facto standard and sufficient here). HMR /
WebSocket dev ergonomics are also out of scope for v1.

## Testing

- **Unit (client):** `Host:`-line rewrite over a raw head buffer (preserve/
  rewrite/fixed, case-insensitivity, IPv6 authority); `block_detect::detect`
  signature matching per framework and non-matches.
- **Unit (daemon):** forwarded-header normalization (present-value preferred vs
  derived-default) — via the existing data-plane test harness.
- **e2e:** (a) a mock backend that returns `403` unless `Host == localhost:<port>`
  → `--host-header=rewrite` turns the tunneled response into `200`; (b) a backend
  emitting a Vite-style block → the client prints the hint exactly once.

## Interfaces changed

- `tnl http`: new `--host-header <preserve|rewrite|VALUE>` (default `preserve`).
- `ForwardCtx`: `+ host_header: HostHeader`, `+ warned: Arc<Mutex<HashSet<..>>>`.
- No wire-protocol change (this is all client-local + daemon-local).
- CHANGELOG entry; RUNBOOK/README note on `--host-header` and the friction hints.
