# Claude Code: tnl project notes

This file is read automatically at the start of every Claude session run from
this repo. It gives you what you need to be useful here without re-reading the
full spec.

## What this is

A self-hosted ngrok alternative in Rust. `tnl http <port> <subdomain>` on any
machine exposes a local TCP service at `https://<subdomain>.t.example.com`.
Reverse-tunneling architecture: the `tnld` daemon (behind Caddy on `your-gateway`)
multiplexes incoming HTTP requests onto a yamux session that the client holds
open.

Full design rationale: [`docs/superpowers/specs/2026-05-26-tnl-design.md`](docs/superpowers/specs/2026-05-26-tnl-design.md).

## Current state (last updated 2026-05-27)

Tag **`v0.1.0-alpha.1`** on `main`. 21 implementation tasks complete; 22 tests pass;
clippy clean with `-D warnings`; fmt clean. The end-to-end pipeline works
**locally** (`crates/tnl-e2e/tests/full_roundtrip.rs` proves it). It has **not
yet been deployed** to `your-gateway` for real-traffic testing.

Phase plan: see [`docs/superpowers/plans/2026-05-26-tnl-v0.1.0-alpha.md`](docs/superpowers/plans/2026-05-26-tnl-v0.1.0-alpha.md).
Future phases (beta, prod) need their own plans — use `superpowers:writing-plans`
when starting them.

## Architecture in 90 seconds

```
end-user ──HTTPS──► Caddy ──HTTP/1.1──► tnld:7777 ──yamux substream──► tnl client ──TCP──► local backend
                  (TLS, wildcard           (data plane:                (CLI process holds
                   *.t.example.com)          host→session lookup)        session open)
```

- **Caddy** terminates TLS, host-dispatches, reverse-proxies everything to
  `127.0.0.1:7777`. Not reconfigured per tunnel.
- **tnld** has two router groups:
  - `/healthz`, `/whoami`, `/control` (authed; `/control` upgrades to WS → yamux
    server role)
  - data-plane fallback that reads `Host`, finds tunnel in registry, opens a
    new yamux substream to the right CLI session, pumps raw HTTP/1.1 bytes
- **tnl** dials WSS via `async_tungstenite::tokio::connect_async`, builds a yamux
  CLI-role session, opens the control substream, sends `CreateTunnel`, then runs
  an accept loop forwarding every incoming substream to `127.0.0.1:<port>`.

Important non-obvious invariants:

- **yamux role mapping is inverted from who dialed.** Daemon = `yamux::Mode::Client`
  (opens substreams when end-user requests arrive). CLI = `yamux::Mode::Server`
  (accepts substreams). This is intentional; the role-mapping doc comment in
  `crates/tnl-protocol/src/transport.rs` MUST be preserved.
- **`YamuxSession` is type-erased** (not generic over T). The background driver
  task owns the `Connection<Compat<T>>`; the public type drops T. Channel-based
  shutdown (open_rx returning None) terminates the driver; `Drop` is a no-op
  body — calling `JoinHandle::abort()` would discard in-flight `poll_close`
  and lose buffered writes.
- **Transport stack uses `async-tungstenite`, NOT `tokio-tungstenite`.** The
  `ws_stream_tungstenite` adapter (which lets yamux read/write through a WS) is
  written against `async-tungstenite` types. `tokio-tungstenite` is only in the
  dep graph transitively via axum.
- **axum 0.8 → async-tungstenite WS bridge** lives in `crates/tnld/src/control.rs`
  (`axum_ws_to_tungstenite`). It spawns a forwarder task that copies frames between
  axum's `WebSocket` (real socket) and an `async-tungstenite::WebSocketStream`
  built on a `tokio::io::duplex` pipe. This is the most fragile piece in the
  codebase; touch it carefully.

## Repo layout

```
Cargo.toml                 workspace root: deps pinned at workspace level
rust-toolchain.toml        Rust 1.94
rustfmt.toml               group_imports + imports_granularity (nightly-only;
                           stable rustfmt warns "unstable" but still formats)
crates/
  tnl-protocol/            shared wire types + Session/Stream traits + YamuxSession
    src/messages.rs        ControlMsg / CreateTunnelReq / TunnelCreatedResp /
                           ErrorCode / LogLine
    src/transport.rs       Stream/Session traits, YamuxSession (with driver_task),
                           server_session_from_ws / client_session_from_ws
  tnld/                    server binary
    src/main.rs            clap dispatch: Serve, HashPassword
    src/serve.rs           AppState, spawn_server, /healthz, /whoami, bearer_auth
    src/control.rs         /control handler + axum_ws_to_tungstenite bridge +
                           control_loop (length-prefixed JSON ControlMsg)
    src/data_plane.rs      catch-all fallback: Host → registry lookup → open
                           substream → write HTTP/1.1 request → read response
    src/registry.rs        Tunnel, SessionState, Registry (DashMap-backed)
    src/auth.rs            TokenStore (argon2id), hash_plaintext
    src/hash_password.rs   `tnld hash-password` subcommand
    src/config.rs          TOML config loader
    tests/                 integration tests (cli_hash_password, control_create_tunnel,
                           data_plane_route, serve_basics)
  tnl/                     client binary
    src/main.rs            clap dispatch: Version, Auth, Config, Http
    src/client.rs          connect_and_create (WSS dial + yamux + CreateTunnel),
                           run_accept_loop
    src/forwarder.rs       per-substream bidi pump to 127.0.0.1:port
    src/config.rs          ~/.config/tnl/config.toml load/save (0600 perm)
    src/commands/          per-subcommand entry points (version, auth, config, http)
    tests/                 cli_basics, auth_login
  tnl-e2e/                 workspace-level integration crate
    tests/full_roundtrip.rs    spawn dummy backend + tnld + tnl, assert response
deploy/                    Caddy snippets + manual deploy runbook
docs/
  superpowers/specs/       design spec
  superpowers/plans/       implementation plan
  RUNBOOK.md               user-facing local + production runbook
```

## Conventions

- **Commit messages:** conventional commits (`feat:`, `fix:`, `chore:`, `docs:`,
  `test:`, `refactor:`, `style:`). Scope in parens: `feat(tnld):`, `feat(tnl):`,
  `feat(protocol):`.
- **Before committing:**
  ```bash
  cargo fmt --all
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```
- **No `unsafe`:** `unsafe_code = "forbid"` at workspace level. Don't add it.
- **No `unwrap_used`/`expect_used` lint** is currently on — but prefer `?` and
  `anyhow::Context` in non-test code.
- **`tracing` for logging**, never `println!` in library code (CLI banner-style
  output in main.rs is fine).
- **Workspace deps** at root `Cargo.toml`. Per-crate Cargo.toml uses
  `.workspace = true`. Don't pin versions inside crate manifests.
- **Spec deviation note:** plan said HTTP/1.1+`Connection: close` for v0.1.0-alpha
  (not h2c). v0.1.0-beta may switch.

## Common tasks

| Task | Command |
|---|---|
| Build everything | `cargo build --workspace` |
| Build release binaries | `cargo build --release --workspace` |
| Run all tests | `cargo test --workspace` |
| Run just the e2e | `cargo test -p tnl-e2e --test full_roundtrip -- --nocapture` |
| Format check | `cargo fmt --all -- --check` |
| Format apply | `cargo fmt --all` |
| Clippy | `cargo clippy --workspace --all-targets -- -D warnings` |
| Run tnld | `./target/release/tnld serve --config <path>` |
| Run tnl http | `./target/release/tnl http <port> <subdomain>` |
| Hash a token | `./target/release/tnld hash-password <plaintext>` |

## What's next

Two plans are pending. Use `superpowers:writing-plans` when starting either:

1. **v0.1.0-beta** — inspector (live request log in CLI stdout), `tnl status`/`stop`,
   reconnect/reattach window, IP allow-list, basic auth on tunnels, full admin CLI
   (`tnld token add/list/revoke`), rate limiting, healthcheck subcommand.
2. **v0.1.0 production** — Dockerfile multi-stage musl, `/opt/tnld/compose.yaml`
   on `your-gateway`, GitHub Actions CI, real deploy + smoke on the gateway.

When picking a phase, also ask whether to refactor `tnl-protocol` to remove its
direct deps on `async-tungstenite`/`ws_stream_tungstenite`/`yamux` (move into a
new `tnl-transport-yamux` crate) before adding QUIC. Worth doing when QUIC lands;
not worth doing earlier.

## Known issues (don't be surprised)

- **yamux driver latent deadlock at ack_backlog ≥ 256.** Code-reviewed and
  documented; not reachable in v0.1.0-alpha use case (would require 256+
  concurrent un-ACK'd substreams on one session). Has no regression test;
  re-test if you do anything load-related.
- **rustfmt unstable options** (`imports_granularity = "Module"`,
  `group_imports = "StdExternalCrate"`) require nightly; stable rustfmt warns
  and ignores. Workspace works either way.
- **Plan code snippets** for Tasks 13 and 18 were originally written for
  `tokio-tungstenite`; the actual implementation uses `async-tungstenite`. The
  plan has a banner at the top explaining the substitution. If the plan
  contradicts the code, the **code wins**.
- **No `cargo doc` is built** — public types lack doc-comments. Address in v0.1.0
  release-prep, not earlier.

## When working in this repo

- Always run from `~/repositories/ns/atassis/tnl/`. Cargo handles workspace
  resolution automatically.
- Don't add things to user-level config (`~/.cargo`, `~/.config/rustup`, etc.)
  unless explicitly asked.
- If you're stuck on yamux/WS internals, read `transport.rs` end-to-end before
  guessing — the role-inversion and driver-task pattern are subtle.
- When in doubt about whether a behavior is a bug or intentional, check the
  design spec first (`docs/superpowers/specs/`), then the plan
  (`docs/superpowers/plans/`).
