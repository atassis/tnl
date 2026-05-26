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

v0.1 design is approved. See
[`docs/superpowers/specs/2026-05-26-tnl-design.md`](docs/superpowers/specs/2026-05-26-tnl-design.md).
Implementation has not started.

## Layout

```
crates/
  tnl-protocol/    # shared wire types + transport traits
  tnl/             # client binary
  tnld/            # server binary
deploy/            # compose, Dockerfile, Caddy snippets, smoke test
docs/              # specs and notes
```
