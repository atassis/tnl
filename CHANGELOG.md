# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0-beta.3] - 2026-07-19

### Fixed
- `tnld serve` never installed a `tracing` subscriber, so **every** daemon log
  event (request logs, `server_failure` warnings) was silently dropped and
  production ran blind — the only output was the startup banner. It now installs
  an stderr `fmt` subscriber honouring `RUST_LOG` (default `info,tnld=debug`).

### Changed
- The `server_failure` warning for an unrelayable tunnel response now includes
  `host`, `method`, `path`, and the debug form of the underlying `hyper` error,
  to make `upstream`/`transport` 502s diagnosable from logs.

## [0.1.0-beta.2] - 2026-07-19

### Changed
- Daemon data plane now relays tunnel responses through hyper's HTTP/1 client
  (`hyper::client::conn::http1`) instead of a hand-rolled parser. This removes
  the fixed 64-header limit, decodes chunked/`gzip, chunked` correctly, skips
  leading `1xx` informational responses, honours HTTP/1.0 close-delimited
  bodies, and streams responses instead of buffering the whole body in RAM.

### Fixed
- A valid backend response with **more than 64 headers**, a leading **`1xx`**
  response, or **`Transfer-Encoding: gzip, chunked`** was previously rejected
  or corrupted by the daemon's hand-rolled parser and served as a misleading
  `502` blaming the client (`X-Tnl-Component: client`,
  `client-malformed-response`, hint "Update to tnl >= 0.1.0-beta.1"). The
  daemon only relays bytes the client forwarded verbatim, so it could never
  legitimately attribute a response-parse failure to the client. These
  responses now pass through unchanged.

### Added
- New `X-Tnl-Component: upstream` attribution for the rare case where a
  response received over the tunnel genuinely cannot be relayed as HTTP/1.1
  (non-HTTP backend, or the backend closed mid-response). Kinds:
  `unparseable-response`, `incomplete-response`. This replaces the previous
  `client` mis-attribution and its version-shaming hint; the message now
  points at the local backend, not the client binary.

## [0.1.0-beta.1] - 2026-05-27

### Added
- `tnl http <TARGET>` accepts an explicit `host:port` (`127.0.0.1:5173`,
  `[::1]:8080`, `192.168.1.50:80`) in addition to the bare port form. Bare
  port forwards to `localhost` and uses dual-stack `/etc/hosts` resolution.
- CLI synthesises an attributed `502 Bad Gateway` for every local-side
  failure: `X-Tnl-Component: client`, `X-Tnl-Origin-Failure: <kind>`, and a
  content-negotiated body. Kinds covered: `connect-refused`,
  `connect-timeout`, `connect-unreachable`, `dns-failed`, `local-eof`,
  `local-malformed`, `local-no-response`.
- Daemon distinguishes failure origin via `X-Tnl-Component`
  (`registry` / `daemon` / `transport` / `client`) on every 4xx/5xx
  response.
- Error response bodies are content-negotiated as HTML, JSON, or plain
  text. JSON shape: `{"error", "kind", "tunnel", "target", "component",
  "reason", "hint", "request_id", "version"}`.
- Per-request inspector log lines now include resolved target and failure
  kind.

### Fixed
- Forwarder hardcoded `TcpStream::connect("127.0.0.1", port)`, breaking
  IPv6-only backends (`[::1]:port` — Vite, uvicorn defaults). Now resolves
  `localhost` via `tokio::net::lookup_host` and tries every returned
  address in order.

### Changed
- Daemon returns `404` (not `502`) for unknown hosts and `503` with
  `Retry-After: 1` for missing-session (grace window) or missing-handle
  cases. The previous `502 + "no such tunnel\n"` response shape is no
  longer emitted.
- HTML user-facing fields (`tunnel`, `target`, `kind`, `reason`, `hint`)
  are now escaped before interpolation into the error page. The `tunnel`
  field originates from the `Host` header at the daemon and was previously
  reflected unescaped.

## [0.1.0-beta.0] - 2026-05-27

Initial public beta snapshot.

## [0.1.0-alpha.2] - 2026-05-27

Alpha bug-fix snapshot (wss:// TLS, Content-Type passthrough, `/etc/tnld`
mode, data-plane logging).

## [0.1.0-alpha.1] - 2026-05-26

First end-to-end working snapshot; client + daemon both build, e2e
roundtrip green locally.
