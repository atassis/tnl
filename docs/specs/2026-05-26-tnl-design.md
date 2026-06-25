# tnl — Self-hosted Open-source ngrok Alternative

**Status:** Design approved 2026-05-26
**Author:** atassis
**Target:** v0.1 (MVP)

## Purpose

A self-hosted alternative to ngrok that:

- exposes any local TCP service on a public `https://<subdomain>.t.example.com` URL with one command
- works from any machine — including behind NAT, no mesh required (true reverse-tunneling, ngrok-model)
- reuses the existing `your-gateway` Caddy as the public TLS terminator; introduces only a single new daemon (`tnld`) and a single new CLI binary (`tnl`)
- gives ngrok-compatible CLI ergonomics (`tnl http 3000 foo`)
- ships an inspector (live request log in the terminal)
- is built with replaceable transports so a future QUIC implementation slots in without touching control-plane code

The user already runs:

- Caddy in Docker on `your-gateway` (a cloud VPS, public IP `203.0.113.10`)
- Headscale/Tailscale mesh including `your-gateway` (`100.64.0.1`)
- `example.com` DNS at Cloudflare (will be configured to point a wildcard at `your-gateway`)

## High-level architecture

`tnl` follows the classic ngrok model — the daemon is on the data path, the client holds a long-lived reverse-tunnel session.

```
                          your-gateway                                  any client machine
                          ┌────────────────────────────┐             (behind NAT, in a café, …)
end-user (browser)        │                            │             ┌─────────────┐
   │                      │   Caddy                    │             │             │
   │ HTTPS h1             │   *.t.example.com ──┐       │             │  tnl client │
   ├─────────────────────►│   tnl-api.t.       │       │             │             │
   │                      │                    ▼       │             │             │
   │                      │   ┌─ data port :7777 ◄─h2c─┤             │             │
   │                      │   │                        │             │             │
   │                      │   │  tnld                  │  long-lived │             │
   │                      │   │  (host network)        │  WSS+yamux  │             │
   │                      │   │      ▲ ─── ws upgrade ─┼─────────────┤             │
   │                      │   │      │                 │             │             │
   │                      │   └──────┴── request mux ──┼─yamux stream┤─► 127.0.0.1:3000
   │                      │           per incoming req │  raw bytes  │   user backend
   │                      └────────────────────────────┘             └─────────────┘
```

Component roles:

- **Caddy** — TLS terminator and host-router. Two static sites:
  - `*.t.example.com` → `reverse_proxy h2c://127.0.0.1:7777` (tunnel data plane)
  - `tnl-api.t.example.com` → `reverse_proxy 127.0.0.1:7777` (control channel: WSS upgrade + REST endpoints)
  - Holds one wildcard cert `*.t.example.com` obtained via ACME DNS-01.
  - **Caddy is not configured per-tunnel.** No Caddy Admin API integration in `tnld`.
- **tnld** — the broker daemon. On the data path:
  - Accepts h2c from Caddy on the catch-all data handler; looks up host → session in its in-memory registry; opens a yamux stream to the appropriate client; streams raw HTTP/1.1 bytes both directions.
  - Accepts WSS upgrade on `/control` from CLI clients; runs a yamux server over it for control + per-request data streams.
  - Holds the only authoritative state. In-memory; no persistence; restart-tolerant within a 30-second reattach window.
- **tnl** — the CLI. Holds the long-lived WSS+yamux session. For each incoming yamux stream it opens a TCP connection to the local target port and copies bytes both ways. Emits inspector log lines for each request.

Tunnel naming: `<subdomain>.t.example.com`. The dedicated `*.t.` namespace avoids any collision risk with the 13 existing first-level sites (`jellyfin.example.com`, `nextcloud.example.com`, …).

## Repo layout

`~/repositories/ns/atassis/tnl/`:

```
Cargo.toml                     # workspace root
crates/
  tnl-protocol/                # shared wire types + transport traits
    src/
      lib.rs
      transport.rs             # Transport / Session / Stream traits
      transport_yamux.rs       # MVP impl
      transport_quic.rs        # (v0.2, feature-gated)
      messages.rs              # ControlMsg, AuthSpec, …
  tnl/                         # client binary
    src/
      main.rs
      commands/                # http, status, stop, auth, config
      client.rs                # negotiation + session glue
      forwarder.rs             # incoming stream → local TCP
      ui/                      # banner + inspector stdout formatter
  tnld/                        # server binary
    src/
      main.rs                  # argparse + subcommand dispatch
      lib.rs
      serve.rs                 # daemon entrypoint
      config.rs
      api/
        mod.rs                 # axum router
        auth_mw.rs             # bearer middleware
        rate_limit_mw.rs       # tower-http governor
        control.rs             # /control WS handler → yamux session
        tunnels.rs             # REST GET/DELETE
        data_plane.rs          # catch-all h2c handler
      auth/
        mod.rs
        token_store.rs         # argon2 verify, notify-watched reload
        token_gen.rs           # `tnld token add`
      registry/
        mod.rs
        tunnel.rs
        session.rs
        gc.rs                  # orphan tunnel cleanup
      transport/
        mod.rs                 # server-side transport traits
        yamux_wss.rs           # MVP
        quic.rs                # (v0.2, feature-gated)
      inspector.rs             # per-tunnel LogLine broadcast fan-out
      admin_cli/               # `tnld token …`, `tnld tunnel …`, `tnld config init`
deploy/
  tnld.compose.yaml            # reference compose for your-gateway
  Dockerfile                   # multi-stage musl static build
  tnl.caddy.example            # Caddy snippet for tunnel data plane
  tnl-api.caddy.example        # Caddy snippet for control + REST
  tokens.example.toml
  smoke.sh                     # end-to-end smoke after deploy
  README.md                    # deploy runbook
docs/
  specs/
    2026-05-26-tnl-design.md  # ← this file
```

Workspace dependencies pinned at the root `Cargo.toml`. The shared `tnl-protocol` crate is the only path-dep across all three binaries.

## Wire protocol

### Transport

Pluggable through `tnl-protocol::transport` traits:

```rust
#[async_trait]
pub trait Transport: Send + Sync {
    async fn connect(endpoint: &Url, auth: &Bearer) -> Result<Box<dyn Session>>;
    fn name(&self) -> &'static str;
}

#[async_trait]
pub trait Session: Send + Sync {
    async fn open_stream(&self) -> Result<Box<dyn Stream>>;
    async fn accept_stream(&self) -> Result<Box<dyn Stream>>;
    async fn ping(&self) -> Result<Duration>;
    async fn close(&self) -> Result<()>;
}

#[async_trait]
pub trait Stream: AsyncRead + AsyncWrite + Send + Unpin {}
```

**MVP implementation:** `yamux+WSS` only.

- Client opens WSS to `wss://tnl-api.t.example.com/control` with `Authorization: Bearer …`.
- After Sec-WebSocket-Accept the connection is wrapped in a yamux session — `client side` on the daemon, `server side` on the CLI (inverted, because the daemon must be able to open data streams toward the CLI).

**v0.2 (planned):** QUIC via `quinn`. Listens on a separate UDP port on `your-gateway`, reads its cert from the volume-mounted Caddy data dir (read-only).

### Negotiation (CLI side)

1. If `transport == "auto"`: try QUIC first (3 s deadline), fall back to yamux+WSS.
2. If `transport == "quic"`: only QUIC; failure → exit 1.
3. If `transport == "yamux-wss"`: only that.
4. On mid-session disconnect: reconnect on the **same** transport that succeeded initially. Exponential backoff capped at 60 s; 5 consecutive failures → exit 1.
5. No cross-transport switching in-session; no Happy Eyeballs.

### Control messages

JSON-coded, length-framed on stream id 1.

```rust
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type")]
pub enum ControlMsg {
    // Client → Server
    CreateTunnel(CreateTunnelReq),
    Reattach { tunnel_id: String },
    Heartbeat,
    Close,

    // Server → Client
    TunnelCreated(TunnelCreatedResp),
    Reattached,
    HeartbeatAck,
    Closing { reason: String },
    Error { code: ErrorCode, message: String },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CreateTunnelReq {
    pub subdomain: Option<String>,            // None = server picks 8 random chars
    pub auth:      Option<AuthSpec>,          // basic-auth on the tunnel
    pub allow_ips: Vec<IpNet>,                // empty = any
    // Note: no `port` field — the port is the client's local concern only.
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TunnelCreatedResp {
    pub tunnel_id: String,        // ULID
    pub hostname:  String,        // e.g. "foo.t.example.com"
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AuthSpec {
    pub basic_user:    String,
    pub basic_pw_hash: String,    // argon2id; CLI hashes locally before sending
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ErrorCode {
    InvalidSubdomain,
    SubdomainTaken,
    TunnelNotFound,
    Unauthorized,
    RateLimited,
    Internal,
}
```

Per-request data streams carry **raw HTTP/1.1 bytes**, no extra framing. The daemon does not parse the body; the CLI does not parse the request beyond optionally peeking the first 4 KB to populate the inspector log.

## Daemon (`tnld`)

### Listeners

Single port `127.0.0.1:7777`, axum router with path-based dispatch. Caddy proxies both `tnl-api.t.example.com` and `*.t.example.com` to this port; the daemon discriminates by `Host` header and path:

| Path                  | Handler                                         |
|-----------------------|-------------------------------------------------|
| `GET  /healthz`       | health (no auth)                                |
| `GET  /version`       | version + supported transports (no auth)        |
| `GET  /whoami`        | auth check (debug)                              |
| `GET  /tunnels`       | list tunnels visible to this token              |
| `DELETE /tunnels/:s`  | tear-down tunnel by subdomain                   |
| `GET  /control`       | WSS upgrade → yamux session                     |
| `* /*` (catch-all)    | data plane (h2c request from Caddy; Host-based) |

Auth middleware applies to everything except `/healthz` and `/version`.

### Registry

```rust
pub struct Registry {
    by_subdomain: DashMap<String, Arc<Tunnel>>,
    by_id:        DashMap<TunnelId, Arc<Tunnel>>,
    sessions:     DashMap<SessionId, Arc<SessionState>>,
}

pub struct Tunnel {
    pub id:         TunnelId,                          // ULID
    pub subdomain:  String,
    pub hostname:   String,
    pub auth:       Option<AuthSpec>,
    pub allow_ips:  Vec<IpNet>,
    pub created_at: Instant,
    pub created_by: String,                            // token name
    pub session:    ArcSwap<Option<Weak<SessionState>>>,
    pub log_tx:     broadcast::Sender<LogLine>,
    pub stats:      TunnelStats,                        // atomic counters
}

pub struct SessionState {
    pub id:       SessionId,
    pub token:    String,
    pub session:  Box<dyn Session>,
    pub tunnels:  Vec<TunnelId>,
    pub remote:   SocketAddr,
    pub started:  Instant,
}
```

All state lives in memory. There is no on-disk tunnel state.

### Data-plane flow

1. axum catch-all handler reads `Host`, looks up `Tunnel` by hostname, returns 502 if absent.
2. Applies the tunnel's IP allow-list (from `X-Forwarded-For` set by Caddy) and basic auth.
3. Waits up to 2 s for an active `SessionState` (in case of reattach). On timeout → 503.
4. Opens a new yamux stream on the session.
5. Serialises the incoming `hyper::Request` to raw HTTP/1.1 bytes on the stream.
6. Reads raw HTTP/1.1 response bytes back; deserialises just the response head; couples the bodies with `hyper::body::Body` ↔ stream.
7. For `Upgrade: websocket` (and similar), the daemon takes the `on_upgrade` future and `tokio::io::copy_bidirectional` between the upgraded connection and the stream — protocol transparent.
8. Emits a `LogLine` on `tunnel.log_tx`. Subscribers (inspector WS over the same yamux control stream) receive a copy.

### Lifecycle

- **Startup:** parse args; load `config.toml` and `tokens.toml`; set up tracing; build router; spawn the GC task; bind listener.
- **GC task:** every 5 s, drop tunnels whose `session` has been `None` for more than `session_grace_sec` (default 30).
- **Shutdown (SIGTERM):** stop accepting new connections; broadcast `Closing { reason }` to active sessions; 5 s grace; force-close.

### Auth

Bearer token in `Authorization` header. Tokens stored in `/etc/tnld/tokens.toml` as argon2id hashes:

```toml
[[tokens]]
name       = "laptop"
hash       = "$argon2id$v=19$m=65536,t=3,p=2$..."
scopes     = ["*"]
created_at = "2026-05-26T14:00:00Z"
last_used_at = "2026-05-26T14:23:00Z"  # written on successful verify
```

- Format: `tnl_` + 26 base32 chars (130 bits of entropy).
- Plaintext shown once at `tnld token add`; not recoverable.
- `tokens.toml` is watched via `notify`; edits/`tnld token add` invocations reload without restart.
- `last_used_at` is updated through a write-coalescing background flusher (no more than once per minute per token) to avoid hammering the file.
- Failed auth: 150 ms sleep (anti-timing-oracle) + rate-limit (`tower-http::limit`) 10 failures/min/IP → 60 s block.

Scope model in MVP is binary (`["*"]` allows everything, anything else allows nothing). The shape is preserved for future fine-grained scopes; the validator stub accepts only `["*"]` in v0.1.

### Admin CLI (same binary)

```
tnld serve [--config PATH]
tnld token add <name> [--scopes "*"]
tnld token list
tnld token revoke <name>
tnld tunnel list
tnld config init
tnld healthcheck
```

`tnld token add` writes the new entry atomically (write to `.tmp`, rename), and the running daemon reloads via the file watcher.

### Failure modes

| Case                                                         | Behaviour                                                                                  |
|--------------------------------------------------------------|--------------------------------------------------------------------------------------------|
| End-user request, hostname unknown                           | 502 Bad Gateway with a `no such tunnel` body                                               |
| Tunnel exists but client disconnected                        | wait 2 s for reattach, then 503                                                            |
| Tunnel exists, session live, but `open_stream` fails         | 503; internal log; `Closing` may be sent to the client                                     |
| Client backend (`127.0.0.1:<port>`) not listening            | client gets ECONNREFUSED; closes stream; daemon → Caddy: 502                               |
| Slow client                                                  | yamux flow-control + hyper backpressure prevent buffer growth                              |
| Excessive body                                               | rate-limit per token + `max_request_body_mb` cap                                           |
| `tnld` restart                                               | clients reattach within 30 s; subdomain preserved; longer downtime → tunnels are GC'd      |

## Client (`tnl`)

### Commands

```
tnl http <PORT> [SUBDOMAIN]
    [--auth USER:PASS]
    [--allow CIDR,CIDR,...]
    [--transport auto|quic|yamux-wss]
    [--quiet]
    [--verbose]

tnl status [--all]
tnl stop <SUBDOMAIN>
tnl auth login --endpoint URL --token TOKEN
tnl config show
tnl version
```

User-visible behaviour matches ngrok ergonomics. `tnl http <PORT>` is foreground and blocks until Ctrl-C.

### `tnl http` lifecycle

1. Load config (`~/.config/tnl/config.toml`); resolve `endpoint` and `token` with precedence: CLI flag > env (`TNL_TOKEN`, `TNL_ENDPOINT`) > config file.
2. Transport negotiation (see protocol section). On success: `Box<dyn Session>`.
3. Open control stream; send `CreateTunnel`; receive `TunnelCreated`.
4. Print the banner:
   ```
   ┌─ tnl ─────────────────────────────────────────
   │ Tunnel:    https://foo.t.example.com
   │ Forward:   127.0.0.1:3000
   │ Transport: yamux+WSS (47ms RTT)
   │ Tunnel ID: 01JCMR5XYZ…
   └────────────────────────────────────────────────
   Press Ctrl-C to stop.
   ```
5. Spawn three tasks:
   - **Heartbeat** — `session.ping()` every 15 s; reconnect on 2 misses.
   - **Stream acceptor** — `loop { session.accept_stream(); tokio::spawn(handle_incoming(...)) }`.
   - **Inspector** — drains an mpsc receiver of `LogLine`s and prints to stdout (unless `--quiet`).
6. On Ctrl-C: send `Close` on the control stream, close the session, exit 0.

### Incoming-stream handler (hot path)

```rust
async fn handle_incoming_stream(
    stream: Box<dyn Stream>,
    cfg: ForwardConfig,
    log_tx: mpsc::Sender<LogLine>,
) -> Result<()> {
    let mut tcp = TcpStream::connect(("127.0.0.1", cfg.port)).await?;
    tcp.set_nodelay(true)?;

    // Peek the first 4 KB to populate the inspector log (method, path, request headers).
    // Beyond 4 KB or after the second CRLF we stop capturing.
    let head_peek = Arc::new(parking_lot::Mutex::new(BytesMut::with_capacity(4096)));

    let started = Instant::now();
    let (yr, yw) = tokio::io::split(stream);
    let (tr, tw) = tokio::io::split(tcp);

    // Two-direction byte pump; the request task records into head_peek
    // until full or terminator is seen. The response task captures the
    // first status line for the inspector.

    let (req_bytes, (resp_bytes, status)) = tokio::try_join!(
        pump_request(yr, tw, head_peek.clone()),
        pump_response(tr, yw),
    )?;

    let head = head_peek.lock();
    let (method, path) = parse_request_line(&head).unwrap_or(("?".into(), "?".into()));
    let _ = log_tx.send(LogLine {
        timestamp:   SystemTime::now(),
        method,
        path,
        status,
        duration_ms: started.elapsed().as_millis() as u64,
        bytes_in:    req_bytes,
        bytes_out:   resp_bytes,
        remote_ip:   cfg.remote_ip,
    }).await;

    Ok(())
}
```

The client never interprets the body. WebSocket / SSE / gRPC / HTTP/2 over h2c are all transparent because we only copy bytes.

### Inspector output

Default format, one line per request:

```
14:23:01.234  GET    /                       200    45ms     1.2.3.4     1.2KB
14:23:01.890  GET    /static/app.js          200    12ms     1.2.3.4    23.4KB
14:23:02.108  POST   /api/login              401     8ms     1.2.3.4     320B
14:23:05.443  GET    /api/me                 200    23ms     1.2.3.4     1.8KB
14:23:10.001  GET    /ws                     101    >180s    1.2.3.4    upgrade
```

`--verbose` adds request and response headers and the first 4 KB of body (hex+ASCII). Coloured on TTY via `nu-ansi-term`.

### `tnl status` / `tnl stop`

Plain REST against `tnl-api.t.example.com`. `reqwest` client with bearer auth. Not over yamux — these are one-shot calls and do not need a long-lived session.

### Error UX

Action-oriented messages, never raw stack traces, modelled on `gh`/`cargo`:

- Missing config → `error: not authenticated. Run \`tnl auth login --endpoint URL --token TOKEN\`.`
- Token rejected → `error: token rejected by server. Get a new one and re-run \`tnl auth login\`.`
- Subdomain taken → `error: 'foo' already in use by another tunnel.`
- Server unreachable → `error: cannot reach tnld at <endpoint>: <err>`
- Mid-session disconnect → `warning: connection lost, reconnecting via <transport> (attempt N/5)...`

With `RUST_LOG=debug` or `--verbose`, full `tracing` output is shown.

## Caddy configuration

`tnld` does not configure Caddy at runtime. Two static site files are placed in `/opt/caddy/sites/` once and not touched again.

`/opt/caddy/sites/tnl.caddy`:

```caddyfile
*.t.example.com {
    tls {
        dns <provider> {env.YOUR_DNS_TOKEN}
    }
    reverse_proxy h2c://127.0.0.1:7777 {
        flush_interval -1
        transport http {
            versions h2c
        }
    }
}
```

`/opt/caddy/sites/tnl-api.caddy`:

```caddyfile
tnl-api.t.example.com {
    tls {
        dns <provider> {env.YOUR_DNS_TOKEN}
    }
    reverse_proxy 127.0.0.1:7777
}
```

A more specific matcher (`tnl-api.t.example.com`) takes precedence over the wildcard.

### Wildcard certificate

`tnl` requires a working wildcard cert for `*.t.example.com`. The spec does not prescribe a DNS provider — any provider that supports ACME DNS-01 will do. The deploy README ships examples; production setup is performed by hand.

`tnld` is decoupled from this concern: it never touches certs.

## Deployment

### Files on `your-gateway`

```
/opt/tnld/
├── compose.yaml            # see deploy/tnld.compose.yaml
├── config/
│   ├── config.toml         # daemon settings
│   └── tokens.toml         # argon2id hashes; mode 0600
└── README.md → docs link
```

`/opt/tnld/compose.yaml` (final shape):

```yaml
name: tnld
services:
  tnld:
    build:
      context: https://github.com/atassis/tnl.git#main
      dockerfile: Dockerfile
    # or once CI publishes images:
    # image: ghcr.io/atassis/tnld:latest
    container_name: tnld
    restart: always
    network_mode: host
    user: "65534:65534"
    read_only: true
    tmpfs:
      - /tmp:size=16M
    volumes:
      - ./config:/etc/tnld:ro
    environment:
      - RUST_LOG=info,tnld=debug
    healthcheck:
      test: ["CMD", "tnld", "healthcheck"]
      interval: 10s
      timeout: 3s
      start_period: 5s
    logging:
      driver: "json-file"
      options:
        max-size: "20m"
        max-file: "5"
```

`network_mode: host` is chosen so that `tnld` can bind `127.0.0.1:7777` from the host's perspective (which is where Caddy will reverse-proxy to). The bind is local-only; the only path in from the public network is through Caddy.

### `config.toml`

```toml
listen        = "127.0.0.1:7777"
public_url    = "https://tnl-api.t.example.com"
hostname_root = "t.example.com"

[transports]
yamux_wss = true
quic      = false

[limits]
max_tunnels_total      = 64
max_tunnels_per_token  = 16
max_request_body_mb    = 100
heartbeat_interval_sec = 15
session_grace_sec      = 30

[logging]
access_log_to_stdout = true
```

### Bootstrap runbook

1. DNS: wildcard `A` record `*.t.example.com → 203.0.113.10`.
2. Provider API token for Caddy DNS plugin (scope-restricted to `example.com` zone, DNS:Edit only).
3. `your-gateway`: rebuild Caddy with the DNS plugin via xcaddy (if not already done).
4. `your-gateway`: place `/opt/caddy/sites/tnl.caddy` and `/opt/caddy/sites/tnl-api.caddy`.
5. `your-gateway`: `docker exec caddy caddy reload`.
6. Verify cert: `curl -sI https://tnl-api.t.example.com/healthz` returns 502 (daemon not running yet) but the TLS handshake completes.
7. `your-gateway`: clone `https://github.com/atassis/tnl` and place `compose.yaml` + initial `config.toml`.
8. `your-gateway`: `docker compose -f /opt/tnld/compose.yaml up -d --build`.
9. `your-gateway`: `docker exec tnld tnld token add laptop --scopes '*'` — copy the printed token.
10. laptop: install `tnl` (cargo install or prebuilt musl binary).
11. laptop: `tnl auth login --endpoint https://tnl-api.t.example.com --token tnl_…`.
12. Smoke test: `python3 -m http.server 9999 &`, `tnl http 9999 smoke`, in another terminal `curl -sf https://smoke.t.example.com/`.

### Updates

When the image is rebuilt: `docker compose up -d --build` (or `docker pull && up -d` if pulling from ghcr). Active sessions reattach within the 30 s grace window. Longer downtime → clients exit with a clear error and the user re-runs `tnl http …`.

## Testing

### Unit

- `tnl-protocol` `ControlMsg` serialisation round-trip across every variant.
- Subdomain validator: regex, length, leading/trailing dash rejection, collision detection.
- Token store: add → list → verify (good) → verify (bad) → revoke; atomic rename on write; reload on watcher event.

### Integration (in-process)

- Transport (`yamux+WSS`) round-trip: spawn `tnld` on an ephemeral port; client opens session; pumps 1 MB on a stream; assert byte-identity.
- Auth middleware: valid / invalid / missing bearer; rate-limit trips after 10 failures.

### End-to-end (single process, no Caddy)

A mock Caddy is unnecessary — tests speak h2c directly to `tnld` with the right `Host` header.

- Happy path: spawn `tnld`, dummy backend on `127.0.0.1:NNN`, `tnl http NNN foo`; request to `Host: foo.t.example.com`; assert backend's response.
- Reconnect / reattach: client severs TCP; client reconnects; subdomain unchanged; new requests succeed.
- Auth / IP allow-list: requests with wrong basic auth → 401; requests from disallowed CIDR → 403.
- WebSocket forwarding: dummy WS-echo backend; frames transit byte-identical.

### Smoke after deploy

`deploy/smoke.sh` runs the runbook step 12 from a CI environment or local machine that already has `tnl` authenticated.

### CI

`.github/workflows/ci.yml`:

- `cargo fmt -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `cargo build --release --workspace --target x86_64-unknown-linux-musl`
- Release artifacts: `tnl` and `tnld` static musl binaries on tagged commits.

## Security model

- **Authentication:** bearer token, transport-agnostic. The same code path works whether `tnld` is reached over the Tailscale mesh, a local Docker network, or the public internet.
- **Authorization:** binary in MVP (any token with `["*"]` is full admin). Granular scopes (`tunnel.create`, `tunnel.list.all`, …) reserved in the data model.
- **Transport security:** TLS terminated by Caddy at the public edge. The `tnld` ↔ Caddy hop is loopback only.
- **Defence in depth:** `tnld` container is `read_only: true` with a small tmpfs; runs as uid `65534` (nobody); `network_mode: host` is restricted to `127.0.0.1:7777`; no inbound from the public internet bypasses Caddy.
- **Tokens at rest:** argon2id, m=64 MB, t=3, p=2. Plaintext shown once at creation.
- **Anti-bruteforce:** 150 ms artificial delay on failed auth + 10/min/IP rate-limit.
- **Subdomain hijacking:** impossible across namespaces (`*.t.example.com` vs `*.example.com`); within the namespace the registry enforces uniqueness.
- **Origin IP exposure:** the wildcard A record is DNS-only (grey cloud) — Cloudflare's free plan does not proxy wildcards. The origin IP is already in CT logs from the existing sites; this is not a regression.

## What is explicitly out of scope (MVP)

- TCP / UDP tunnels (`tnl tcp …`). Architecturally compatible (a separate listener on `your-gateway`), planned for v0.2.
- A web UI / Inspector at `localhost:4040`. Inspector lives in the terminal only.
- Persistent/named tunnels (reservations across restarts). Sessions plus subdomain choice are sufficient for MVP.
- ngrok REST API / local Inspector REST compatibility.
- Multi-host failover, HA, leader election.
- Backups of `tokens.toml` (it's a single file the user backs up themselves).
- Cross-transport live migration; auto re-probe of a previously-failed transport mid-session.

## Forward-compatibility hooks

- `tnl-protocol::Target` is shaped as an enum even though MVP only uses one variant — `Direct` style forward-tunneling is reserved for a possible mode where `tnld` only configures Caddy and stays off the data path.
- `Transport` trait stays neutral about backing protocol — QUIC drops in as a second `impl Transport` without changes elsewhere.
- `AuthSpec` carries an enum-style structure ready for OAuth/OIDC/JWT verifiers if ever wanted.
- `Scope` is a `Vec<String>` so granular scopes can be added without breaking the wire format.
- `Tunnel.stats` already collects atomic counters — exporting `/metrics` for Prometheus is purely additive.

## Glossary

- **Control stream** — yamux stream id 1 on a CLI session, carrying `ControlMsg` values.
- **Data stream** — a per-incoming-request yamux stream carrying raw HTTP/1.1 bytes both ways.
- **Reattach** — re-binding an existing `Tunnel` in the registry to a new session after a client reconnect within the grace window.
- **Hostname root** — `t.example.com`. All tunnel hostnames are `<subdomain>.<hostname_root>`.
- **Session grace** — the time window during which a tunnel survives without an active session before GC removes it.
