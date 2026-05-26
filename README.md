# tnl

Self-hosted open-source ngrok alternative. Single-command tunnels on
`https://<subdomain>.t.example.com` with reverse-tunneling (works from anywhere,
no mesh required).

- **Server (`tnld`)** — Rust daemon that sits on the data path, behind an
  existing Caddy reverse proxy.
- **Client (`tnl`)** — ngrok-style CLI: `tnl http 3000 foo`.
- **Transport** — yamux over WSS in v0.1; QUIC pluggable later via a `Transport`
  trait in `crates/tnl-protocol`.
- **Caddy** — terminates TLS for one wildcard `*.t.example.com` cert and reverse-proxies
  to `tnld`. No Admin API, no per-tunnel reconfiguration.

## Status

**v0.1.0-alpha** shipped (tag `v0.1.0-alpha.1`, 2026-05-27): foundation crates,
end-to-end reverse-tunneling working **locally** (in-process e2e test passes).
Production deployment to `your-gateway` is documented but not yet automated.

- Design spec: [`docs/superpowers/specs/2026-05-26-tnl-design.md`](docs/superpowers/specs/2026-05-26-tnl-design.md)
- v0.1.0-alpha implementation plan: [`docs/superpowers/plans/2026-05-26-tnl-v0.1.0-alpha.md`](docs/superpowers/plans/2026-05-26-tnl-v0.1.0-alpha.md)
- Deploy runbook: [`deploy/README.md`](deploy/README.md)
- Next-session context: [`CLAUDE.md`](CLAUDE.md)

## Layout

```
crates/
  tnl-protocol/    # shared wire types (ControlMsg, LogLine) + Session/Stream
                   # traits + YamuxSession + WSS adapter
  tnld/            # server binary (axum, token store, registry, /control + /data-plane)
  tnl/             # client binary (clap, WSS dial, forwarder)
  tnl-e2e/         # workspace-level integration test
deploy/            # Caddy snippets + runbook (no Dockerfile / CI yet)
docs/              # specs, plans, status notes
```

## Quick build

```bash
cargo build --release --workspace
cargo test --workspace
```

Two binaries land in `target/release/`: `tnld` (server) and `tnl` (client).
Rust 1.94 is pinned via `rust-toolchain.toml`; rustup will install it on first
build if missing.

## Running

For first-time setup and testing, see:

- **Local smoke test** — [`docs/RUNBOOK.md`](docs/RUNBOOK.md) §1.
  Spin tnld+tnl on the same machine, test with `curl --resolve` or `Host` header.
  No Caddy or DNS required.
- **Production deployment** on `your-gateway` — [`docs/RUNBOOK.md`](docs/RUNBOOK.md) §2
  and [`deploy/README.md`](deploy/README.md). Requires Cloudflare DNS API token,
  Caddy with DNS plugin, `tnld` running on the gateway.

## Roadmap

- **v0.1.0-beta** — live request inspector in CLI stdout, `tnl status`/`stop`,
  reconnect/reattach, IP allow-list, basic-auth on tunnels, full admin CLI
  (`tnld token add/list/revoke`), rate limit, healthcheck subcommand.
- **v0.1.0 production** — Dockerfile, compose for `your-gateway`, GitHub Actions CI,
  real deployment.

## License

MIT OR Apache-2.0 (see `Cargo.toml`).
