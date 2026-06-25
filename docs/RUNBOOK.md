# tnl Runbook — local smoke test

Prove the binaries work end-to-end on one machine, with **no Caddy, no DNS, no
domain**. ~5 minutes. For a real TLS deployment, see [`../deploy/README.md`](../deploy/README.md).

Run everything below from the repository root.

## 1. Build

```bash
cargo build --release --workspace
```

Rust 1.94 is pinned via `rust-toolchain.toml`; rustup installs it on first run.
You'll get `./target/release/tnld` (daemon) and `./target/release/tnl` (client).
Both `tnld --version` / `tnl version` print `0.1.0-beta.1`.

## 2. Create config + a token

Generate the server config and your first client token non-interactively:

```bash
mkdir -p /tmp/tnl-smoke
TOKEN=$(./target/release/tnld init \
    --config /tmp/tnl-smoke/config.toml \
    --tokens-file /tmp/tnl-smoke/tokens.toml \
    --listen 127.0.0.1:7777 \
    --public-url http://127.0.0.1:7777 \
    --hostname-root t.example.com \
    --admin-token tnl_LOCALSECRET -y | tail -1)
```

`hostname_root` need not resolve — for a local test we spoof it with a `Host`
header. (Interactively, just run `tnld init` and answer the prompts.)

## 3. Start the daemon (terminal A)

```bash
RUST_LOG=info,tnld=debug ./target/release/tnld serve --config /tmp/tnl-smoke/config.toml
# → tnld listening on http://127.0.0.1:7777
curl -s http://127.0.0.1:7777/healthz    # → ok
```

`tnld serve` refuses to start with an empty token store — step 2 minted one, so
you're set.

## 4. Log the client in (terminal B)

```bash
./target/release/tnl auth login --endpoint http://127.0.0.1:7777 --token tnl_LOCALSECRET
# writes ~/.config/tnl/config.toml (mode 0600)
```

In CI/containers you can skip this file and export `TNL_ENDPOINT` / `TNL_TOKEN`
instead.

## 5. Start a dummy backend (terminal C)

```bash
python3 -m http.server 9999
```

## 6. Open a tunnel (terminal B)

```bash
./target/release/tnl http 9999 demo
# ┌─ tnl ──────────────────────────────
# │ Tunnel:  https://demo.t.example.com
# │ Forward: localhost:9999
# └────────────────────────────────────
```

The target accepts a bare port (dual-stack `localhost`) or an explicit
`IP:PORT` (`127.0.0.1:5173`, `[::1]:5173`, `192.168.1.50:8080`).

## 7. Hit the tunnel (terminal D)

No real DNS for `demo.t.example.com`, so spoof the host header:

```bash
curl -v -H "Host: demo.t.example.com" http://127.0.0.1:7777/
```

You should get the backend's response — the whole pipeline (axum → registry →
yamux substream → forwarder → TCP → backend) verified end-to-end. A `404` with
`X-Tnl-Component: registry` means the registration didn't land; check the daemon
logs.

## 8. Shutdown

Ctrl-C terminal C (backend), then B (`tnl http`, prints `✓ stopping tunnel`),
then A (`tnld serve`).
