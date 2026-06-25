# tnl

A self-hosted, open-source ngrok alternative in Rust. One command exposes a local
service on a public `https://<subdomain>.<your-domain>` URL via reverse-tunneling —
it works from anywhere behind NAT, with no mesh required.

- **Server (`tnld`)** — Rust daemon on the data path, behind an existing reverse
  proxy (Caddy) that terminates TLS for one wildcard cert.
- **Client (`tnl`)** — ngrok-style CLI: `tnl http 3000 foo`.
- **Transport** — yamux multiplexed over WSS in v0.1; QUIC is pluggable later via a
  `Transport` trait in `crates/tnl-protocol`.

> Examples below use `example.com` as a placeholder — substitute your own domain.

## Status

**v0.1.0-beta.1.** Foundation plus the beta feature set ship and are tested:
reverse-tunneling end-to-end (e2e test passes locally), a live request inspector in
the CLI, argon2id token administration (`tnld token add/list/revoke`), client
pairing, a first-run `tnld init` wizard, `healthcheck`, and shell completions.
Production hardening (Docker image, CI, automated deploy) is the remaining roadmap.

- Design spec: [`docs/specs/2026-05-26-tnl-design.md`](docs/specs/2026-05-26-tnl-design.md)
- Implementation history: [`docs/plans/`](docs/plans/)
- Deploy guide: [`deploy/README.md`](deploy/README.md)
- Contributor & agent guide: [`AGENTS.md`](AGENTS.md)

## Build

```bash
cargo build --release --workspace   # → target/release/{tnld, tnl}
cargo test --workspace
```

Rust 1.94 is pinned via `rust-toolchain.toml`; rustup installs it on first build.

## Quickstart

**Try it locally — no domain or DNS needed** (see [`docs/RUNBOOK.md`](docs/RUNBOOK.md) §1):
spin up `tnld` and `tnl` on one machine and test with a `Host:` header.

**Stand up your own server:**

```bash
# On the server: generate config + your first client token, interactively.
tnld init                                  # prompts for public URL + wildcard domain
# … prints a copy-paste `tnl auth login …` line and a Caddy snippet …
tnld serve --config /etc/tnld/config.toml

# On your laptop: log in and open a tunnel.
tnl auth login --endpoint https://api.tnl.example.com --token tnl_…
tnl http 3000 foo                          # → https://foo.t.example.com
```

Front the daemon with Caddy doing TLS for `*.<your-domain>` — see [`deploy/`](deploy/).

## Configuration

Server config is a TOML file (`tnld init` generates it; an annotated
[`deploy/config.example.toml`](deploy/config.example.toml) documents every field).
Client config is written by `tnl auth login`. Values resolve with this precedence:

```
flag  >  TNL[D]_* env var  >  config file  >  built-in default
```

Server env vars: `TNLD_LISTEN`, `TNLD_PUBLIC_URL`, `TNLD_HOSTNAME_ROOT`,
`TNLD_TOKENS_FILE`, `TNLD_SESSION_GRACE_SEC`. Client env vars: `TNL_ENDPOINT`,
`TNL_TOKEN` (these let the client run in CI/containers with no config file). A value
set in both the file and the environment prints a warning.

## Layout

```
crates/
  tnl-protocol/   shared wire types + Session/Stream traits + YamuxSession + WSS adapter
  tnld/           server binary (axum, argon2id token store, registry, /control + data plane)
  tnl/            client binary (clap, WSS dial, forwarder, inspector)
  tnl-e2e/        workspace-level end-to-end integration test
deploy/           Caddy config, .env + config templates, deploy guide
docs/             design spec, implementation history, runbook
```

## License

Dual-licensed under [MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE), at your option.
