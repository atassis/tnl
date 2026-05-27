# tnl Runbook

What **you** need to do (manually, outside Claude) to test the v0.1.0-alpha
build. Two scenarios: §1 is a fully local smoke that bypasses Caddy entirely
and proves the binaries work; §2 is the real production wiring on `your-gateway`
with TLS and DNS.

Start with §1. If anything fails there, the deployment in §2 won't help — debug
locally first.

---

## §1. Local smoke test (no Caddy, no DNS)

Goal: prove `tnld` accepts a `tnl http …` reverse tunnel and that an HTTP
request reaches your local backend through it. ~5 minutes of your time.

### 1.1. One-time build

```bash
cd ~/repositories/ns/atassis/tnl
cargo build --release --workspace
```

Rust 1.94 is pinned via `rust-toolchain.toml`; rustup will install it on first
run. First build takes ~5 min cold (~300 transitive crates); subsequent builds
are seconds.

After this you'll have:
- `./target/release/tnld` — the daemon
- `./target/release/tnl` — the client

Verify:
```bash
./target/release/tnld --version
./target/release/tnl version
```
Both should print `tnld 0.1.0-alpha.1` / `tnl 0.1.0-alpha.1`.

### 1.2. Hash a token and create config files

Pick any plaintext token (lowercase letters/digits OK; convention: `tnl_…`):

```bash
HASH=$(./target/release/tnld hash-password tnl_LOCALSECRET)
```

Create a small workspace for the run (use anywhere; example uses `/tmp`):

```bash
mkdir -p /tmp/tnl-smoke
cd /tmp/tnl-smoke

cat > tokens.toml <<EOF
[[tokens]]
name = "local"
hash = "${HASH}"
EOF
chmod 600 tokens.toml

cat > config.toml <<EOF
listen        = "127.0.0.1:7777"
public_url    = "http://127.0.0.1:7777"
hostname_root = "t.example.com"
tokens_file   = "/tmp/tnl-smoke/tokens.toml"
EOF
```

Note `hostname_root = "t.example.com"` even though there's no real DNS here — we
spoof it with the `Host` header in the test request. The string just has to
match between server config and the subdomain you claim.

### 1.3. Start the daemon (terminal A)

```bash
cd ~/repositories/ns/atassis/tnl
RUST_LOG=info,tnld=debug ./target/release/tnld serve --config /tmp/tnl-smoke/config.toml
```

Expected output:
```
tnld listening on http://127.0.0.1:7777
```

Leave this running. If you see "tokens file not found" or "address in use", fix
and retry.

Sanity check from a third terminal:
```bash
curl -s http://127.0.0.1:7777/healthz
# → ok
```

### 1.4. Log the client in (terminal B)

In a new terminal:

```bash
cd ~/repositories/ns/atassis/tnl
./target/release/tnl auth login \
    --endpoint http://127.0.0.1:7777 \
    --token tnl_LOCALSECRET
```

Expected:
```
✓ logged in; config written to /home/<you>/.config/tnl/config.toml
```

This writes your endpoint and token to `~/.config/tnl/config.toml` with mode
0600. If you don't want to pollute your real config, override with
`TNL_CONFIG=/tmp/tnl-smoke/tnl-config.toml ./target/release/tnl auth login …`
and then prefix the next command the same way.

### 1.5. Start a dummy local backend (terminal C)

Anything that listens on a port works. Quickest:

```bash
python3 -m http.server 9999
```

(Or `cargo run --example` of any axum app you have lying around. The backend's
content doesn't matter; we just need *something* to forward to.)

### 1.6. Open a tunnel (back to terminal B)

```bash
./target/release/tnl http 9999 demo
```

`<TARGET>` accepts either a bare port (forwards to `localhost` via dual-stack
`/etc/hosts` resolution — works for backends bound to `127.0.0.1:port` AND
`[::1]:port`, e.g. Vite/uvicorn defaults) or an explicit `IP:PORT`:

```bash
./target/release/tnl http 5173 demo                # bare port (dual-stack)
./target/release/tnl http 127.0.0.1:5173 demo      # explicit IPv4
./target/release/tnl http "[::1]:5173" demo        # explicit IPv6
./target/release/tnl http 192.168.1.50:8080 demo   # LAN host
```

Hostnames are not accepted in this release.

Expected:
```
┌─ tnl ─────────────────────────────────────────
│ Tunnel:    https://demo.t.example.com
│ Forward:   localhost:9999
│ Press Ctrl-C to stop.
└────────────────────────────────────────────────
```

Leave this running. It blocks until you Ctrl-C.

### 1.7. Hit the tunnel (terminal D)

We don't have real DNS for `demo.t.example.com`, so spoof the host header:

```bash
curl -v -H "Host: demo.t.example.com" http://127.0.0.1:7777/
```

Expected: the response body should contain the Python `http.server` directory
listing (or whatever your backend serves).

**If you see a 404 with `X-Tnl-Component: registry`** the registration didn't land. Check terminal A
for `tnld` logs and B for `tnl` errors.

**If you see the directory listing** — congratulations, the reverse tunnel works
locally. The whole pipeline (axum → registry → yamux substream → forwarder → TCP →
backend) is verified end-to-end.

### 1.8. Shutdown

- Ctrl-C in terminal C (Python server)
- Ctrl-C in terminal B (`tnl http`) — should print `✓ stopping tunnel`
- Ctrl-C in terminal A (`tnld serve`)

That's the smoke. Move to §2 when ready for the real thing.

---

## §2. Production deployment on `your-gateway`

Goal: expose `https://<sub>.t.example.com` for real, with TLS, from any client
machine. ~30–60 minutes the first time, mostly waiting for Caddy to rebuild and
LE to issue a wildcard cert.

`tnld` runs as a docker compose service on the gateway. The image is built
from the repo root `Dockerfile` (multi-stage musl, ~21 MB) and shipped to the
host via `docker save | ssh … docker load` (no registry required for
v0.1.0-alpha).

### 2.1. What you need to do manually (out of Claude scope)

These steps touch Cloudflare and the your-gateway host. Claude can guide but
shouldn't do them autonomously.

#### a. DNS record on Cloudflare

In Cloudflare dashboard for `example.com` zone → DNS → Add record:
- **Type:** A
- **Name:** `*.t`
- **IPv4:** `203.0.113.10` (your-gateway public IP)
- **Proxy status:** DNS only (grey cloud) — Cloudflare free plan doesn't proxy
  wildcards on `*.x.example.com` patterns, and we don't want it to.
- **TTL:** Auto

Verify after ~1 minute:
```bash
dig +short any-subdomain.t.example.com
# → 203.0.113.10
```

#### b. Cloudflare API token (scoped, for Caddy DNS-01 challenge)

In Cloudflare → My Profile → API Tokens → Create Token → **Custom token**:
- **Permissions:**
  - `Zone : Zone : Read`
  - `Zone : DNS : Edit`
- **Zone Resources:** Include → Specific zone → `example.com`
- TTL: default (no expiry, or set per your policy)

Save the token — you'll add it to Caddy's env in step c.

#### c. Rebuild Caddy on `your-gateway` with the Cloudflare DNS plugin

The existing Caddy image (`caddy:2.8.4-alpine`) doesn't include DNS plugins.
Replace it with an `xcaddy`-built image. The canonical Dockerfile is checked
into the repo at [`deploy/caddy.Dockerfile.example`](../deploy/caddy.Dockerfile.example).
Smoke-tested locally: produces caddy `v2.11.3` with `dns.providers.cloudflare`
loaded, ~155 MB image.

> Note on the version: the Dockerfile pins `caddy:2-builder` (float) rather
> than the running `2.8.4`. Hard-pinning older minors with `xcaddy build`
> currently fails because `xcaddy` resolves `go.uber.org/zap` fresh and newer
> zap dropped `zapslog.HandlerOptions`, which 2.8.4 references. 2.x → 2.11
> is config-compatible.

Ship and stage on `your-gateway`:
```bash
# from laptop
scp deploy/caddy.Dockerfile.example your-gateway:/tmp/caddy.Dockerfile

# on your-gateway
sudo mv /tmp/caddy.Dockerfile /opt/caddy/Dockerfile
sudo tee /opt/caddy/cloudflare.env >/dev/null <<'EOF'
CF_API_TOKEN=<paste your CF token from step b>
EOF
sudo chmod 600 /opt/caddy/cloudflare.env
```

Update `/opt/caddy/compose.yaml` to add `env_file: cloudflare.env` and use
`build: .` instead of the stock image:

```yaml
services:
  caddy:
    build: .                            # ← changed from `image: caddy:2.8.4-alpine`
    container_name: caddy
    restart: always
    network_mode: host
    env_file: cloudflare.env            # ← new
    volumes:
      - ./Caddyfile:/etc/caddy/Caddyfile:ro
      - ./sites:/etc/caddy/sites:ro
      - ./data:/data
      - ./config:/config
      - ./logs:/var/log/caddy
```

Rebuild and bring up:
```bash
sudo docker compose up -d --build
sudo docker logs caddy --tail 50
```

Wait for the rebuild to complete (a few minutes the first time). Existing 13
sites should keep working — verify with `curl -sI https://jellyfin.example.com`.

#### d. Drop the tnl Caddy snippets

```bash
sudo cp <(curl -s https://raw.githubusercontent.com/.../deploy/tnl.caddy.example) \
    /opt/caddy/sites/tnl.caddy
# or: scp the files from your local repo's deploy/ directory
```

Since the repo isn't on GitHub yet, easiest is `scp` from your laptop:
```bash
# from laptop
scp deploy/tnl.caddy.example       your-gateway:/tmp/tnl.caddy
scp deploy/tnl-api.caddy.example   your-gateway:/tmp/tnl-api.caddy

# on your-gateway
sudo mv /tmp/tnl.caddy      /opt/caddy/sites/tnl.caddy
sudo mv /tmp/tnl-api.caddy  /opt/caddy/sites/tnl-api.caddy
```

Edit both files: replace `<provider>` with `cloudflare` and
`{env.YOUR_DNS_TOKEN}` with `{env.CF_API_TOKEN}` (the env var name from step c):

```caddyfile
tls {
    dns cloudflare {env.CF_API_TOKEN}
}
```

Reload Caddy:
```bash
sudo docker exec caddy caddy reload --config /etc/caddy/Caddyfile
sudo docker logs caddy --tail 80
```

You should see `obtained certificate for *.t.example.com` within ~30 seconds.

Verify the wildcard cert:
```bash
curl -sIv https://nonexistent-tunnel.t.example.com/ 2>&1 | grep -E "subject|issuer"
# subject: CN=*.t.example.com
# issuer: Let's Encrypt …
```

You'll get a 502 from Caddy because `tnld` isn't running yet — that's expected.
The important thing is the TLS handshake succeeded.

#### e. Build the `tnld` Docker image on your laptop and ship it to your-gateway

The image is a multi-stage musl build (alpine builder → alpine runtime, ~21 MB).
No registry is required for v0.1.0-alpha; we just stream the image over SSH.

```bash
# from laptop, in the repo root
docker build -t tnld:0.1.0-alpha.1 .

# pipe straight into docker load on the remote — no intermediate file needed
docker save tnld:0.1.0-alpha.1 | ssh your-gateway 'sudo docker load'
```

On your-gateway, confirm:
```bash
sudo docker images tnld
# REPOSITORY   TAG               IMAGE ID       CREATED         SIZE
# tnld         0.1.0-alpha.1     …              …               21.4MB
```

#### f. Set up tokens + config on your-gateway

The container runs as a non-root `tnld` user (UID created inside the image).
Mount `/etc/tnld` read-only; the files need to be world-readable (mode 644) so
the in-container UID can read them. If you want stricter perms, switch the
compose to `user: 0:0` and rely on the container being read-only.

```bash
# on your-gateway
sudo mkdir -p /etc/tnld
sudo chown root:root /etc/tnld

# Generate a real token (save the plaintext somewhere safe — bitwarden, etc.)
PLAINTEXT="tnl_$(openssl rand -base64 18 | tr -d '+/=' | cut -c1-26)"
HASH=$(sudo docker run --rm tnld:0.1.0-alpha.1 hash-password "$PLAINTEXT")
echo "Plaintext token (save this!): $PLAINTEXT"

sudo tee /etc/tnld/tokens.toml >/dev/null <<EOF
[[tokens]]
name = "laptop"
hash = "$HASH"
EOF
sudo chmod 644 /etc/tnld/tokens.toml

sudo tee /etc/tnld/config.toml >/dev/null <<'EOF'
listen        = "127.0.0.1:7777"
public_url    = "https://tnl-api.t.example.com"
hostname_root = "t.example.com"
tokens_file   = "/etc/tnld/tokens.toml"
EOF
sudo chmod 644 /etc/tnld/config.toml
```

#### g. Run `tnld` as a docker compose service

Copy the compose snippet from the repo:
```bash
# from laptop
scp deploy/tnld-compose.yaml.example your-gateway:/tmp/tnld-compose.yaml

# on your-gateway
sudo mkdir -p /opt/tnld
sudo mv /tmp/tnld-compose.yaml /opt/tnld/compose.yaml
```

Bring it up:
```bash
sudo docker compose -f /opt/tnld/compose.yaml up -d
sudo docker compose -f /opt/tnld/compose.yaml logs --tail 20
```

Expected last line: `tnld listening on http://127.0.0.1:7777`.

Verify locally on the host (loopback, before TLS):
```bash
curl -sf http://127.0.0.1:7777/healthz
# → ok
```

…and through Caddy (this is the path the laptop will use):
```bash
curl -sf https://tnl-api.t.example.com/healthz
# → ok
```

To upgrade later (new image tag, e.g. v0.1.0-alpha.2):
```bash
# laptop
docker build -t tnld:0.1.0-alpha.2 .
docker save tnld:0.1.0-alpha.2 | ssh your-gateway 'sudo docker load'
# your-gateway — edit /opt/tnld/compose.yaml's image: tag, then
sudo docker compose -f /opt/tnld/compose.yaml up -d
```

### 2.2. Use it from your laptop

```bash
cd ~/repositories/ns/atassis/tnl

./target/release/tnl auth login \
    --endpoint https://tnl-api.t.example.com \
    --token tnl_<plaintext from step f>

# Start something to expose:
python3 -m http.server 9999 &

# Open a tunnel:
./target/release/tnl http 9999 demo
```

In another terminal (or another machine entirely — any network):
```bash
curl -sf https://demo.t.example.com/
```

You should see the Python directory listing. The full path was:
your-laptop ← yamux ← WSS ← Caddy ← TLS ← internet ← Caddy:443 ← `*.t.example.com` →
`tnld:7777` → registry lookup → yamux substream back to laptop → `localhost:9999`
(dual-stack — tries `127.0.0.1` then `[::1]`).

### 2.3. Troubleshooting

Every daemon-side error response carries an `X-Tnl-Component` header
(`registry` / `daemon` / `transport` / `client`) attributing the failure, plus
a content-negotiated body (HTML / JSON / plain text per `Accept`). Use
`curl -i` to see the headers.

- **`tnl auth login` says "connect to https://tnl-api.t.example.com: ..."** — Caddy
  site for `tnl-api.t.example.com` isn't there or DNS isn't propagated yet. Check
  step d files + `dig tnl-api.t.example.com`.
- **`tnl http …` says "server error (Unauthorized)"** — token mismatch. Re-run
  `tnld hash-password` on the same plaintext, replace the hash, restart `tnld`.
  Plaintext is required to match byte-for-byte.
- **404 with `X-Tnl-Component: registry`** — no tunnel registered for this host.
  Start `tnl http` on a client; check terminal for errors.
- **503 with `X-Tnl-Component: daemon` and `Retry-After: 1`** — tunnel exists
  in the registry but the client session is missing or in the reattach grace
  window. The CLI should reconnect automatically; retry the request.
- **502 with `X-Tnl-Component: client` and `X-Tnl-Origin-Failure: connect-refused`**
  — the local backend the CLI is forwarding to is not listening on the
  resolved address. Body lists the resolved target. Start your dev server,
  or pass an explicit `tnl http <IP>:<PORT>` if you want to bypass dual-stack
  resolution.
- **502 with `X-Tnl-Origin-Failure: local-eof | local-malformed | local-no-response`**
  — the backend accepted the TCP connection but misbehaved on the response.
  Check the local dev server output.
- **502 with `X-Tnl-Component: transport`** — daemon-side yamux or socket
  failure. Check `journalctl -u tnld -g 'server_failure'`.
- **Returns 503** — `tnld` couldn't open a yamux substream. Likely a transport
  bug; capture `tnld` logs and report.
- **Caddy reload fails with "no DNS provider"** — xcaddy build didn't include
  `caddy-dns/cloudflare`. Re-check step c Dockerfile, rebuild.

### 2.4. Cleanup / rollback

To revert your-gateway to its pre-tnl state:
```bash
sudo docker compose -f /opt/tnld/compose.yaml down 2>/dev/null
sudo docker image rm tnld:0.1.0-alpha.1 2>/dev/null
sudo rm -rf /opt/tnld /etc/tnld
sudo rm -f /opt/caddy/sites/tnl.caddy /opt/caddy/sites/tnl-api.caddy
sudo docker exec caddy caddy reload --config /etc/caddy/Caddyfile
```

Cloudflare DNS record and API token: delete from the Cloudflare dashboard if
you don't want them lingering.

---

## What to bring back to Claude

After you've tested:

- **If §1 (local) failed** — share the full output of all three terminals
  (`tnld serve`, `tnl http`, `curl`). The next Claude session can debug from
  logs.
- **If §1 worked but §2 failed** — share the specific step that broke and the
  output. Probably a Caddy/DNS config issue, not a code issue.
- **If §2 worked** — great. Tell Claude you're ready to plan v0.1.0-beta
  (inspector, reattach, admin CLI) or jump straight to v0.1.0 production
  (Dockerfile + CI + systemd unit + deploy automation).
