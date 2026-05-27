# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

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
