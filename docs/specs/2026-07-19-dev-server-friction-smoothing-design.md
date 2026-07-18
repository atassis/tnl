# Design: evidence-driven dev-server host smoothing

- **Date:** 2026-07-19
- **Status:** approved (pending spec review)
- **Scope:** one implementation plan, client-only

## Problem

A tunnel puts a local backend behind a new public origin
(`https://<sub>.t.atassis.ru`). Dev servers reject an unknown `Host` for
DNS-rebinding protection — Vite/Astro `server.allowedHosts` (`403 Blocked
request`), webpack-dev-server (`403 Invalid Host header`), Django `ALLOWED_HOSTS`
(`400 DisallowedHost`), Rails `config.hosts` (`403 Blocked host`) — so the tunnel
serves an opaque `403` instead of the app. This is the case that made a tunnel to
an Astro dev server on `:4321` return a bare `403` the user could not interpret.

The established tools (ngrok `--host-header=rewrite`, cloudflared
`httpHostHeader`) fix only this, only manually, and only if you already know the
flag before you hit the wall.

## Principle: the security dividing line

- **Category 1 — the backend doesn't know it's behind a proxy** (host allowlists).
  These misfire only because the backend can't see the real chain. Presenting the
  `Host` it expects is correct proxy behavior and does not weaken security — the
  dev server's guard still applies to non-tunnel traffic. **tnl smooths this.**
- **Category 2 — the app protecting the end user** (CORS, CSP, CSRF, cookie
  flags, the app's own auth). Bulldozing these turns the tunnel into a footgun.
  **tnl never touches these; it only explains them if asked later.**

Posture: **truthful by default; rewrite only on evidence; a block is never a
cryptic dead-end.**

## Behavior

Default (no flag): the client forwards the **real** `Host` unchanged. When a
response is a recognized host-block, the client, for that tunnel:

1. Flips an "auto-rewrite" flag so **subsequent** requests send
   `Host: <resolved_addr>` — the exact `ip:port` the forwarder connected to
   (`127.0.0.1:4321`, `[::1]:8080`, …), never a guessed name. Two reasons this is
   the right rewrite target: it is by definition an address the dev server answers
   on, and host-check guards allow bare IP literals unconditionally (they exist to
   block domain *names*), so an IP:port passes every framework's default allowlist.
2. Prints **one** notice to stderr (deduped per tunnel), e.g.:
   ```
   ! dev server blocked host 'regal-leaf-69.t.atassis.ru' (looks like Vite).
     auto-rewriting Host -> 127.0.0.1:4321 for this tunnel.
     override: tnl http 4321 --host-header=preserve   (or =<host>)
   ```

The first blocked request still returns the dev server's 403 (switch-and-notify;
no retry in v1 — one browser refresh, then it works). Auto-retry of the blocked
GET is a documented future enhancement.

### `--host-header` override

`tnl http <target> [--host-header <mode>]`:

- *(unset)* — the evidence-driven auto behavior above.
- `preserve` — never rewrite, never auto-activate (opt out entirely).
- `rewrite` — rewrite to `resolved_addr` from the **first** request (skip the
  one-block wait; for users who already know their dev server checks hosts).
- `<value>` — rewrite to an explicit host string every request (for a custom
  allowlist like `myapp.local` that accepts neither the public host nor an IP).

If the auto IP-rewrite is *itself* blocked (a strict custom allowlist), the
detector fires again and the notice points at `--host-header=<value>`.

## Components (all in `crates/tnl/`)

- **`main.rs`** — add `--host-header <mode>` to the `Http` subcommand; parse into
  `enum HostHeader { Auto, Preserve, Rewrite, Fixed(String) }` (default `Auto`).
- **`commands/http.rs`** — thread the mode into `ForwardCtx`.
- **`ForwardCtx`** — `+ host_header: HostHeader`, plus shared per-tunnel state
  (`Arc<AtomicBool>` "rewrite active" + a warned flag), since `ForwardCtx` is
  cloned per substream.
- **`forwarder.rs`**
  - *Request:* the head is already pre-buffered (`peek_request_head`) and written
    to the backend after `connect_local` yields `resolved_addr`. When rewrite is
    active (forced by the mode, or auto-flag set), replace the `Host:` line's
    value in that head buffer (case-insensitive name, preserve the rest) before
    `tcp.write_all`.
  - *Response:* the forwarder already tees the response status + first bytes
    (`resp_peek`). After the pump, run block detection over `(status, resp_peek)`;
    on a match in `Auto` mode, set the rewrite flag and print the notice once.
    (Bump the tee to ~1 KiB so the block body phrase is captured.)
- **`block_detect.rs`** *(new)* — `detect(status: u16, body_prefix: &[u8]) ->
  Option<&'static str>` (returns a framework label or None) backed by a tiny
  phrase table; all matches drive the **same** action:

  | Framework | status · body substring |
  |---|---|
  | Vite/Astro | 403 · `Blocked request` |
  | webpack-dev-server | 403 · `Invalid Host header` |
  | Django | 400 · `DisallowedHost` / `Invalid HTTP_HOST` |
  | Rails | 403 · `Blocked host` |

  Extensible; also the future home for Category-2 *explainers* (CORS,
  mixed-content), which advise only and never change forwarding.

## Explicitly out of scope

No daemon or wire-protocol change. No `X-Forwarded-*` normalization (the
http↔https / proto class is a separate, rarer pain — a future item). No
auto-retry of the blocked request (future). No Category-2 auto-fixing ever.

## Testing

- **Unit:** `Host:`-line rewrite over a raw head (preserve / IPv4 / IPv6
  authority, case-insensitivity); `block_detect::detect` positive per framework
  and negatives (a normal 403 with no block phrase must not trigger).
- **e2e:** a mock backend that returns `403 Blocked request` unless `Host` is a
  loopback `ip:port` → default `Auto` serves the block once then the next request
  gets `200`; `--host-header=preserve` stays `403`; the notice prints exactly
  once per tunnel.

## Interfaces changed

- `tnl http`: new `--host-header <preserve|rewrite|VALUE>` (default = auto).
- `ForwardCtx`: `+ host_header`, `+ shared rewrite/warned state`.
- CHANGELOG entry; README/RUNBOOK note on the auto-heal + `--host-header`.
