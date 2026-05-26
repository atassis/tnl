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

Expected:
```
┌─ tnl ─────────────────────────────────────────
│ Tunnel:    https://demo.t.example.com
│ Forward:   127.0.0.1:9999
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

**If you see "no such tunnel"** the registration didn't land. Check terminal A
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

There is **no Dockerfile yet** (v0.1.0 scope). For v0.1.0-alpha you'll run
`tnld` directly on the host (under tmux or as a quick systemd unit).

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
Replace it with a `xcaddy`-built image. SSH to `your-gateway`:

```bash
cd /opt/caddy
sudo tee Dockerfile <<'EOF'
FROM caddy:2.8.4-builder AS builder
RUN xcaddy build \
    --with github.com/caddy-dns/cloudflare

FROM caddy:2.8.4-alpine
COPY --from=builder /usr/bin/caddy /usr/bin/caddy
EOF

sudo tee cloudflare.env <<EOF
CF_API_TOKEN=<paste your CF token from step b>
EOF
sudo chmod 600 cloudflare.env
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

#### e. Get `tnld` binary onto your-gateway

Build a musl-static binary on your laptop and scp it:

```bash
# from laptop
rustup target add x86_64-unknown-linux-musl
cargo build --release --workspace --target x86_64-unknown-linux-musl

scp target/x86_64-unknown-linux-musl/release/tnld your-gateway:/tmp/tnld

# on your-gateway
sudo mv /tmp/tnld /usr/local/bin/tnld
sudo chmod +x /usr/local/bin/tnld
```

(If musl build fails because of any C dep, fall back to `cargo build --release`
and scp the glibc-linked binary. your-gateway is Debian 13, glibc 2.36-ish; the
binary should run.)

#### f. Set up tokens + config on your-gateway

```bash
sudo mkdir -p /etc/tnld
sudo chown root:root /etc/tnld

# Generate a real token (save the plaintext somewhere safe — bitwarden, etc.)
PLAINTEXT="tnl_$(openssl rand -base64 18 | tr -d '+/=' | cut -c1-26)"
HASH=$(tnld hash-password "$PLAINTEXT")
echo "Plaintext token (save this!): $PLAINTEXT"

sudo tee /etc/tnld/tokens.toml >/dev/null <<EOF
[[tokens]]
name = "laptop"
hash = "$HASH"
EOF
sudo chmod 600 /etc/tnld/tokens.toml

sudo tee /etc/tnld/config.toml >/dev/null <<'EOF'
listen        = "127.0.0.1:7777"
public_url    = "https://tnl-api.t.example.com"
hostname_root = "t.example.com"
tokens_file   = "/etc/tnld/tokens.toml"
EOF
```

#### g. Run `tnld` (tmux for v0.1.0-alpha; systemd later)

Quick path (tmux):
```bash
sudo apt install -y tmux
sudo tmux new-session -d -s tnld 'RUST_LOG=info,tnld=debug tnld serve --config /etc/tnld/config.toml'
sudo tmux ls
# tnld: 1 windows (created …)

# attach if you want to watch:
sudo tmux attach -t tnld
# Ctrl-B, D to detach
```

Verify:
```bash
curl -sf https://tnl-api.t.example.com/healthz
# → ok
```

Hardened path (systemd one-liner):
```bash
sudo tee /etc/systemd/system/tnld.service >/dev/null <<'EOF'
[Unit]
Description=tnl tunneling daemon
After=network-online.target docker.service
Wants=network-online.target

[Service]
Type=simple
User=tnld
Group=tnld
Environment=RUST_LOG=info,tnld=debug
ExecStart=/usr/local/bin/tnld serve --config /etc/tnld/config.toml
Restart=on-failure
RestartSec=5s
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/log/tnld

[Install]
WantedBy=multi-user.target
EOF

sudo useradd -r -s /usr/sbin/nologin tnld
sudo chown tnld:tnld /etc/tnld/tokens.toml /etc/tnld/config.toml
sudo systemctl daemon-reload
sudo systemctl enable --now tnld
sudo systemctl status tnld --no-pager
sudo journalctl -u tnld -n 40 --no-pager
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
`tnld:7777` → registry lookup → yamux substream back to laptop → `127.0.0.1:9999`.

### 2.3. Troubleshooting

- **`tnl auth login` says "connect to https://tnl-api.t.example.com: ..."** — Caddy
  site for `tnl-api.t.example.com` isn't there or DNS isn't propagated yet. Check
  step d files + `dig tnl-api.t.example.com`.
- **`tnl http …` says "server error (Unauthorized)"** — token mismatch. Re-run
  `tnld hash-password` on the same plaintext, replace the hash, restart `tnld`.
  Plaintext is required to match byte-for-byte.
- **`curl https://demo.t.example.com/` returns 502 from Caddy with body "no such
  tunnel"** — `tnl http` not running or `tnld` lost the session. Check the `tnl
  http` terminal for errors.
- **Returns 502 from Caddy with body "client session not ready"** — the WS dropped
  mid-flight. Restart `tnl http`. (v0.1.0-beta will reconnect automatically.)
- **Returns 503** — `tnld` couldn't open a yamux substream. Likely a transport
  bug; capture `tnld` logs and report.
- **Caddy reload fails with "no DNS provider"** — xcaddy build didn't include
  `caddy-dns/cloudflare`. Re-check step c Dockerfile, rebuild.

### 2.4. Cleanup / rollback

To revert your-gateway to its pre-tnl state:
```bash
sudo systemctl disable --now tnld 2>/dev/null
sudo tmux kill-session -t tnld 2>/dev/null
sudo rm -f /opt/caddy/sites/tnl.caddy /opt/caddy/sites/tnl-api.caddy
sudo docker exec caddy caddy reload --config /etc/caddy/Caddyfile
sudo rm -rf /etc/tnld /usr/local/bin/tnld
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
