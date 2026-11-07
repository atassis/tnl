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
Both `tnld --version` / `tnl version` print `0.1.0-beta.5`.

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

## Application WebSocket release check

Run the automated direct-daemon/tunnel scenarios first:

```bash
cargo test --offline -p tnld --test data_plane_websocket
cargo test --offline -p tnl-e2e --test websocket_roundtrip
cargo test --offline -p tnl-e2e --test full_roundtrip_response_framing
```

These are **not** Caddy runtime validation. For an offline loopback-only check,
run the daemon/client from the earlier steps with a local WebSocket echo backend
instead of Python's HTTP-only server. With an already installed Caddy binary,
use this separate plaintext fixture (no DNS module, ACME, or certificates):

```caddyfile
{
	admin off
	auto_https off
}
http://:8080 {
	bind 127.0.0.1
	reverse_proxy 127.0.0.1:7777 {
		flush_interval -1
	}
}
```

Start Caddy with `caddy run --config /tmp/tnl-smoke/Caddyfile --adapter caddyfile`.
Use a WebSocket client targeting `ws://127.0.0.1:8080/socket` with Host
`demo.t.example.com` (or resolve that host to loopback). Require a valid 101,
text/binary echo, ping/pong and close. Check backend Origin/auth refusal is still
HTTP, not a retry or Origin rewrite. Send two ordinary requests over one
keepalive socket and verify streaming/SSE is flushed while a WebSocket stays
open. Also point a test owner's endpoint at this proxy to check `/control` still
works. Stop only this local fixture afterward.

Before rollout, adapt the reference `deploy/Caddyfile` using dummy environment
values and the installed DNS module, and verify the wildcard block has no
Connection-close rewrite. Adaptation alone is not a runtime check. The combined
release campaign must still qualify the actual candidate binaries/Caddy,
public TLS/browser WSS, reload/disconnect behavior, and rollback. This work does
not promise uninterrupted WebSockets across tunnel reconnect or proxy reload.

Release inspection qualification must also pass
`cargo test --offline -p tnl --test forwarder_binary_status`: it tracks a
pre-existing client observer limitation exposed by application WS tests
(binary frame/body bytes can erase an otherwise valid status). Do not infer
that a working 101 relay proves correct inspector status reporting. The observer
follow-up must qualify both WS 101 and ordinary binary HTTP final status.


## Per-run visitor access qualification

See README's optional visitor controls. No daemon profiles or visitor-password
persistence are required. Password files must be trusted regular files; Unix mode
checks do not validate ownership, directories, ACLs or backups. Restrictive ACLs
are the user's responsibility on other systems. Secret-buffer zeroization is best
effort, not protection against process compromise, allocator copies or core dumps.

Protected control requires verified HTTPS/WSS and the existing authenticated WS
metadata capability. Test Basic denial before backend/stream contact, consumed
Authorization on success, and application Authorization passthrough without Basic.
Exercise full streaming-upload/response and WebSocket concurrency lifetime, not
just headers/101. Reattach must retain policy and quota state; expired-instance
recreation must preserve the original policy and requested hostname or fail on
conflict. Policies do not permanently reserve hostnames. No gate protects a backend
that is separately exposed, and local replay/inspector owners must never restore a
consumed gate credential. Combined release qualification still owns public Caddy,
packaged binaries and browser behavior; local trusted-CA tests are not deployment
acceptance.
