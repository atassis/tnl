# Contributor & agent guide

Guidance for anyone — human or AI coding agent — changing tnl. Keep this file
thin; detail lives in `docs/`. The code is the source of truth: if a doc
contradicts the code, the code wins (open a PR to fix the doc).

## Setup & commands

```sh
cargo build --release --workspace     # builds the `tnld` (server) and `tnl` (client) binaries
cargo test --workspace                # full test suite (unit + integration + e2e)
cargo run -p tnld -- init             # interactive first-run server wizard
cargo run -p tnl  -- http 3000 demo   # open a tunnel to localhost:3000 (needs a configured server)
```

Rust 1.94 is pinned via `rust-toolchain.toml`; rustup installs it on first build.

## Before every commit (required, non-negotiable)

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

All three must be clean. CI rejects anything that isn't.

## Code style

- Errors: `thiserror` for protocol/library types, `anyhow` with `.context(...)`
  at the binary boundary. No `unwrap()`/`expect()` on user-reachable paths — use `?`.
- `tracing`, not `println!`, for diagnostics in library code (CLI user-facing
  output via `eprintln!`/`println!` is fine).
- Workspace-level dependency versions at the root `Cargo.toml`; member crates
  use `.workspace = true`.
- Unit tests in-file under `#[cfg(test)] mod tests`; integration tests under
  each crate's `tests/`; the cross-crate end-to-end test lives in `crates/tnl-e2e`.

## Architecture (see `docs/specs/` for the full design)

- `tnl-protocol` — shared wire types (`ControlMsg`, `LogLine`) + the `Session`/`Stream`
  transport traits + the yamux-over-WSS adapter.
- `tnld` — the daemon: axum router, argon2id token store, tunnel registry,
  `/control` (WSS) + data plane.
- `tnl` — the client CLI (`tnl http`, `tnl auth login`, ...).
- Transport in v0.1 is yamux multiplexed over WSS; QUIC is pluggable later via the
  `Transport` trait.

### Non-obvious invariants (don't break these)

- **yamux roles are inverted from who dialed.** The daemon is `yamux::Mode::Client`
  (it *opens* substreams when end-user requests arrive); the CLI is `yamux::Mode::Server`
  (it *accepts* them). Preserve the role-mapping comment in `tnl-protocol/src/transport.rs`.
- **`YamuxSession` is type-erased** — a background driver task owns the connection and
  the public type drops `T`. Shutdown is via channel-drop, not `JoinHandle::abort()`
  (abort would discard in-flight `poll_close` and lose buffered writes).
- **The transport stack uses `async-tungstenite`, not `tokio-tungstenite`** (the
  `ws_stream_tungstenite` adapter is written against async-tungstenite types). The
  axum↔tungstenite WS bridge in `tnld/src/control.rs` is the most fragile piece — touch
  it carefully.

## Config & onboarding

- **Server config** is a TOML file (default `/etc/tnld/config.toml`). Generate it
  with `tnld init` (interactive wizard) — do not hardcode a domain.
- **Client config** is written by `tnl auth login` to `~/.config/tnl/config.toml`.
- **Env overrides** (precedence `flag > env > file > default`): `TNLD_*` for the
  server, `TNL_ENDPOINT`/`TNL_TOKEN` for the client. A value set in both the file
  and the env emits a warning.

## Repo etiquette

- **Commits:** Conventional Commits — `type(scope): subject` (`feat`/`fix`/`chore`/
  `docs`/`refactor`/`test`), subject ≤ 72 chars; the body explains *why*. No
  tool/assistant attribution lines.
- **Branches & PRs:** work on a feature branch; open a PR against `main`. Don't
  force-push shared branches.

## Gotchas

- Production needs a wildcard domain + a reverse proxy (Caddy) doing TLS — see
  `deploy/`. `example.com` throughout the docs/configs is an illustrative placeholder.
- `tnld serve` refuses to start with an empty token store; run `tnld init` or
  `tnld token add` first.
- Known latent issue: the yamux driver can deadlock at `ack_backlog ≥ 256` un-ACK'd
  substreams on one session — not reachable in normal use, no regression test yet.
- `rustfmt.toml` sets nightly-only options (`imports_granularity`, `group_imports`);
  stable rustfmt warns and ignores them — formatting still works on stable.
