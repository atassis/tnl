---
status: proposed
scope: tnl-websockets
created: 2026-11-07
updated: 2026-11-07
summary: "Application WebSocket upgrade forwarding: verified baseline, approval gates, bounded implementation tasks and acceptance matrix."
---

# Application WebSockets through tnl — task specification

## Status and authority

Design only; **no implementation or deployment is authorized by this document**. Recommendations below are proposals pending reconciliation/user approval. Investigated worktree `/home/atassis/.config/superpowers/worktrees/tnl/websockets`, branch `feat/roadmap-websockets`, clean baseline `43f2bc0ed29e00821e1ede624b8fe190eb0d5c57`. This artifact is written only to the run's authoritative output location; no repository files were changed. After reconciliation, the parent may place the approved plan at `docs/plans/roadmap-websockets.md`.

Goal: a visitor's application `wss://<tunnel-host>/<path>` reaches a local HTTP/1.1 WebSocket backend through Caddy → tnld → a yamux data substream → tnl → backend, including the handshake and opaque bidirectional bytes. This is **different from** the existing WSS `/control` connection carrying yamux.

Read `AGENTS.md` and the available documentation-manager skill. Applied its documentation/frontmatter guidance, not its unrelated repository reorganization/commit workflow. Repository source and cached dependency source, not old design promises, establish the facts below. No credentials, external network, child agents, builds, tests, deployment, or Git mutations were used.

## 1. Verified current behavior

References are repository-relative at the baseline unless marked dependency source.

| Seam | Verified behavior / implication |
| --- | --- |
| Public reverse proxy | `deploy/Caddyfile:14-23` wildcard site sets `flush_interval -1` and unconditionally `header_up Connection close`. The specific `tnl-api` site at lines 25-31 does not set this override. The wildcard override is incompatible with forwarding an application upgrade and must be removed/replaced. Caddy runtime behavior has **not** been tested here. |
| Generated example | `crates/tnld/src/commands/init.rs:234-239` already prints an ordinary `reverse_proxy` without the close override. Do not unnecessarily change its output or CLI tests. `deploy/README.md` describes the wildcard/control split. |
| Routing / authentication | `crates/tnld/src/serve.rs:123-168`: `/control` and `/whoami` get bearer middleware; data-plane fallback does not. `/healthz`, admin routes, and these explicit routes precede fallback regardless of Host. A public tunnel path `/control` is therefore an existing reserved-path collision, not currently an application backend route. Do not silently change router dispatch in this lane. |
| Data-plane routing | `crates/tnld/src/data_plane.rs:51-184` negotiates errors, requires Host, looks up the registered hostname and current session, opens one substream under a session mutex, then releases the mutex. Missing Host → 400; unknown host → 404; disconnected grace/no handle → 503; open failure → 502. Host is currently split at `:` for lookup; this lane does not redesign hostname parsing. |
| HTTP proxying | `data_plane.rs:13-49,196-243,313-326`: Hyper HTTP/1 client parses responses over the substream, but driver is spawned as `conn.await` **without** `.with_upgrades()`. Both request and response lose `Connection`, `Upgrade`, all connection-nominated fields, framing headers, and other static hop-by-hop headers. Request always gets `Connection: close`, version HTTP/1.1. Response body is streamed as `Body::new(Incoming)`. There is no downstream/upstream upgrade bridge. |
| Existing stripping detail | `strip_hop_by_hop` examines **all** Connection values, parses comma-separated header names, removes those fields, then removes static fields including Content-Length. Simply exempting Upgrade from the static list is insufficient: Connection can nominate Upgrade too. Preserving every nominated field would be a security regression. |
| Error attribution / counters | `data_plane.rs:246-326` distinguishes upstream parse/incomplete errors from transport failures. Requests counter increments once after receiving a backend response, not at body completion. Registry byte counters exist (`crates/tnld/src/registry.rs:15-17`) but the current data plane does not update them. No new metric meaning should be invented here. |
| Client pipeline | `crates/tnl/src/forwarder.rs:180-420`: buffers request head (8 KiB / 5 seconds in `peek.rs`), connects locally (3 seconds, `connect.rs:16`), writes head and leftover bytes, probes nine response-prefix bytes with a 15-second deadline, replays the prefix, then runs two 8 KiB-buffered pumps with half-close propagation. It does not parse or terminate application WebSocket frames. `101` already has an HTTP-looking prefix and can pass this pipeline. Response-prefix replay is essential. |
| Host rewriting | `forwarder.rs:243-257`, `host_header.rs:4-82`, `main.rs:53-66`: default Auto initially preserves public Host; a recognized completed dev-server rejection activates rewriting for **subsequent** requests. Preserve never rewrites; Rewrite uses resolved backend socket address; any other CLI string is Fixed. There is no explicit `auto` flag spelling: omission selects Auto, `--host-header=auto` is a fixed Host. Rewriting changes only Host, not Origin. |
| Origin / visitor credentials | Data plane has no Origin policy and passes ordinary end-to-end headers unless Connection nominates them. tnl does not rewrite Origin. Visitor Authorization/Cookie belong to the backend, distinct from the tunnel-owner `/control` bearer. Backend response headers such as Set-Cookie are forwarded. Browser-origin protection remains an application responsibility today. |
| Inspection | `forwarder.rs:367-467,523-529` tees at most 1 KiB of response prefix for status/block detection; it is not a WebSocket message inspector. A successful `LogLine` is emitted only after both pumps finish, so a long-lived WebSocket will not immediately appear in CLI request logs. Pump I/O errors currently return before that final log. `crates/tnl/src/inspector.rs` contains the actual client log type. |
| Task lifetime | `client.rs:181-205` spawns detached per-stream forwarders. `reconnect.rs:128-176` owns an accept task; the forced-disconnect hook dropping its JoinHandle does **not** abort it. `commands/http.rs` cancellation returns on Ctrl-C; full process/runtime exit closes sockets. Do not use that test hook as proof of actual tunnel loss. No comprehensive active-forwarder shutdown supervision exists. |
| Transport | `crates/tnl-protocol/src/transport.rs`: byte-stream interfaces suffice; daemon is yamux Client and CLI Server, public session type is erased, drop signals graceful driver closure rather than abort. Flow control plus awaited writes provide backpressure, but queues of streams/open requests are unbounded. Actual driver now polls inbound while awaiting outbound ACKs; the AGENTS latent-deadlock warning is stale relative to this source. Do not claim this lane fixes all transport resource limits. |

Existing regression evidence to retain: `crates/tnl-e2e/tests/full_roundtrip_response_framing.rs` (chunking, leading 103, honest HTTP/2-response error attribution); `full_roundtrip_many_headers.rs` (65 custom response headers); `host_rewrite.rs` (Auto/Preserve); `inspector_e2e.rs`; `full_roundtrip_local_down.rs`; `crates/tnl/tests/forwarder_*`; `crates/tnld/tests/data_plane_*`, `control_*`, `reattach.rs`. `wss_dial.rs` proves tunnel-transport WSS, **not** application WebSockets. The old `docs/specs/2026-05-26-tnl-design.md` claims generic upgrades and h2c transparency that current code does not implement.

### Hyper APIs checked locally

Versions locked: Hyper **1.9.0**, hyper-util **0.1.20**, Axum **0.8.9**, Tokio **1.52.3**. Inspected their cached sources under `/home/atassis/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/` without fetching dependencies.

- `hyper-1.9.0/src/upgrade.rs`: `hyper::upgrade::on(&mut Request)` and `on(&mut Response)` consume their upgrade extension and return `OnUpgrade`; await returns `Upgraded`. Hyper explicitly leaves method/header/status validation to callers.
- `hyper-1.9.0/src/client/conn/http1.rs:257-266,286-297`: drive the tunnel connection with **`conn.with_upgrades().await`**. Plain `conn.await` reports manual upgrade handling rather than fulfilling the upgrade future.
- `hyper-1.9.0/src/proto/h1/role.rs` client decoder treats **any 101** as upgraded and bodyless; Hyper does not certify a valid WebSocket handshake. An unsolicited/malformed 101 must be rejected before reaching the visitor.
- `axum-0.8.9/src/serve/mod.rs:385-396` uses `serve_connection_with_upgrades`; existing daemon serving already supports downstream upgrade ownership. No new server implementation required.
- `Upgraded` exposes Hyper runtime read/write; wrap each in `hyper_util::rt::TokioIo` to use Tokio IO. Its internal Rewind preserves read-ahead bytes; do not downcast/extract the raw socket and discard `read_buf`.
- Await downstream `OnUpgrade` **only in a spawned task after constructing/returning 101**. Awaiting it in the request handler before returning the response can deadlock the handshake.
- `tokio-1.52.3/src/io/util/copy_bidirectional.rs`: default two 8 KiB buffers; EOF shuts the opposite write half while reverse copying continues; error immediately ends the copy and may discard pending bytes. This is bounded buffering, **not** a timeout/lifetime policy.
- Inspected cached `tungstenite-0.26.2.crate` (used via async-tungstenite 0.29) with `tar -xOf`: public `async_tungstenite::tungstenite::handshake::derive_accept_key(&[u8]) -> String` is available. Its client response verifier is private and has different token handling; do not depend on that private type or introduce another WebSocket stack. The public derive helper avoids new crypto dependencies.

## 2. Approaches and recommendation

1. **Recommended: HTTP/1 handshake proxy plus Hyper upgrade-to-upgrade raw relay.** Preserve/validate the limited WebSocket handshake, keep one yamux substream, get downstream and upstream `Upgraded` objects, then copy opaque bytes. Reuse existing tnl forwarder unchanged unless lifecycle acceptance reveals a specific blocker. No wire protocol change; backend owns WebSocket protocol semantics. Smallest change with strong non-upgrade isolation.
2. **Terminate/recreate application WebSockets at tnld and/or tnl.** Axum WebSocketUpgrade + an async-tungstenite client could establish independent frame sessions. This duplicates handshakes, extension/subprotocol handling, masking/close behavior, and frame-size policies; may alter application semantics and increases bridge complexity. Existing `/control` bridge is not a suitable abstraction to reuse. Reject for this scope.
3. **Raw HTTP relay/rework transport with an explicit upgraded-stream type.** Could avoid Hyper on the data path or introduce a control/data stream preamble. Would undo recent framing fixes or require protocol negotiation and mixed-version handling. Not justified for application WebSockets; reject absent a separately approved general transport redesign.

## 3. Material decisions for user approval

Approve/reject these before implementation; the recommended answers define the proposed contracts below.

| ID | Decision | Recommendation / consequence |
| --- | --- | --- |
| D1 | Which protocols and activation? | Automatically support classic **HTTP/1.1 RFC6455 WebSocket upgrades only**, no CLI/config opt-in. Not generic Upgrade, CONNECT, h2c, HTTP/2 extended CONNECT, or HTTP/3 WebSockets. Public WSS terminates at Caddy; configured backend remains plain HTTP/TCP. |
| D2 | Origin, auth and Host policy? | Preserve Origin, visitor Authorization/Cookie, and existing Host modes. Do not auto-rewrite Origin or retry a failed handshake. Backend/access-controls lane enforces access/origins; a WebSocket is not authenticated merely by existence of the owner-authenticated tunnel. Host rewriting can cause legitimate Origin rejection; document it instead of bypassing checks. |
| D3 | Handshake strictness and failure status? | Before opening a stream reject malformed/unsupported upgrade attempts with 400; validate backend 101 structurally and its accept key/subprotocol, else 502/upstream. Do not implement full extension negotiation or WebSocket frame validation. See exact rules below. |
| D4 | Timeout / shutdown product contract? | Upgrade-only complete-response deadline 30 s; after valid 101, downstream upgrade rendezvous deadline 5 s. No upgraded idle/max-age timer, synthesized ping, or connection resurrection. Preserve standard half-close semantics, accepting that a deliberately uncooperative half-open peer can persist until session/process loss. If bounded half-close drain or concurrency quotas are required, approve a separate bounded lifecycle addition **before** claiming release readiness. |
| D5 | Caddy / compatibility activation? | Remove wildcard `header_up Connection close` and rely on Caddy's supported reverse_proxy WebSocket behavior; retain flush setting and separate API site. Allow Caddy↔tnld keepalive for ordinary HTTP, while tnld↔backend still uses a fresh stream + Connection close for ordinary HTTP. Verify reuse regressions. Roll out daemon and Caddy together; no old-daemon fallback promise. |
| D6 | Inspection contract? | Keep current client close-time summary, status 101 and cumulative raw connection-byte/duration semantics; daemon logs successful handshake once, not frame bodies. Do not add public log schema, replay, live frame views or daemon byte-counter meanings here. Inspector/replay owners must explicitly exclude opaque upgraded payloads. |

These are open approvals, not hidden implementation choices. If D4's unconstrained connection lifetime is unacceptable, stop and revise ownership/tasks rather than add speculative cancellation APIs across all forwarders.

## 4. Bounded scope and proposed contracts

### External/config/wire/CLI contract — PROPOSED

- No additions to `ControlMsg`, `CreateTunnelReq`, Session/Stream traits, server/client TOML, environment variables, commands, flags, or dependencies. `tnl http TARGET [SUBDOMAIN] [--host-header MODE]` remains unchanged; application WebSockets work on the same host/path namespace as HTTP (except existing reserved routes).
- Exactly one fresh substream per handshake; after accepted 101 that stream becomes one opaque full-duplex connection. It is not returned to an HTTP pool. Reattach restores the tunnel URL for **new** requests, never migrates an existing WebSocket.
- Host routing occurs on the original public Host before existing tnl Host rewrite. URI path/query and end-to-end headers retain HTTP proxy behavior; do not rewrite Origin, Sec-WebSocket-Key, protocols, extensions or cookies.
- Public WSS does not imply TLS to the local backend. Do not add `wss://` backend targets or expose visitor credentials to `/control`.

### Request classification and header handling — PROPOSED

Keep helpers private to `data_plane.rs` (no new exported API). Suggested source-named types/functions:

- `WebSocketHandshake { expected_accept: String, offered_protocols: Vec<String> }` (Debug must not dump sensitive request headers).
- `classify_websocket_upgrade(&Request) -> Result<Option<WebSocketHandshake>, UpgradeRequestError>`; small private error enum with stable safe reason strings, mapped via existing error renderer. `None` means existing normal HTTP path. An Upgrade header or `Connection` token `upgrade` denotes an attempted upgrade; `Sec-WebSocket-*` alone does not.
- `validate_websocket_response(&HeaderMap, &WebSocketHandshake) -> Result<(), UpgradeResponseError>` before any response header stripping; used only on status 101.
- Keep existing `strip_hop_by_hop(&mut HeaderMap)` for both paths. For a validated upgrade, run it and then insert **canonical** `Connection: upgrade` and `Upgrade: websocket`. Never blindly preserve Connection or its nominated fields.

Exact classification rules:

1. Attempted upgrade requires HTTP/1.1 GET, all Connection values parsed as comma-separated case-insensitive tokens including `upgrade`, **no `close`**, and exactly one Upgrade value equal to `websocket` (case-insensitive, optional surrounding OWS). Multiple upgrade protocols and malformed token lists → 400. Non-WebSocket attempted upgrades → 400. Normal HTTP without upgrade intent is unchanged.
2. Require one nonempty Sec-WebSocket-Key and one Sec-WebSocket-Version equal to `13`. This is envelope validation, not a new base64 decoder: backend remains responsible for full RFC6455 request validity. Compute expected accept using the existing public tungstenite derive helper. Duplicate singleton fields → 400.
3. No Transfer-Encoding; Content-Length absent or a single valid zero; an HTTP body is not part of this handshake. Reject nonzero/ambiguous framing → 400 before opening a stream. Bytes already read beyond headers remain owned by Hyper and are not silently consumed as a body.
4. Parse offered subprotocol values as comma-separated case-sensitive HTTP tokens, preserving original header values when forwarding; invalid token lists → 400. Origin and extensions are not rewritten/interpreted as authorization.
5. Connection-nominated `host`, `origin`, `authorization`, `cookie`, or any `sec-websocket-*` field → 400 for upgrade attempts, rather than silently removing a security-critical handshake field. Ordinary Connection-nominated fields such as `X-Hop-Secret` are stripped; reconstruct only the two canonical upgrade fields. Existing static proxy-auth/hop stripping continues.
6. Require the original request to contain the public `hyper::upgrade::OnUpgrade` extension, otherwise return 400 before opening a stream. Capture it using `hyper::upgrade::on(&mut req)` before decomposition; do not move that server-side extension into the upstream request. Tests must use real `axum::serve` for successful upgrades (Router::oneshot does not provide socket ownership).

Exact response rules:

- Non-101 from the backend, including 200/401/403/426/redirect/502 synth, follows the existing streamed HTTP response path, with ordinary stripping and preserved response body/cookies/status. No visitor upgrade, retry, or backend re-dial.
- **Any 101 without a classified WebSocket request → 502/upstream.** Any invalid 101 on a classified request → 502/upstream. Never expose a bare 101 that Hyper will treat as a connection takeover.
- Valid 101 requires HTTP/1.1, a Connection token `upgrade` and no `close`, exactly one Upgrade value `websocket`, and exactly one Sec-WebSocket-Accept matching expected accept. If Sec-WebSocket-Protocol is present it must be one token, exactly one value, and one originally offered token (case-sensitive); absence is allowed even when protocols were offered.
- Reject 101 with Transfer-Encoding, any Content-Length, or Connection nomination of Sec-WebSocket-* / Set-Cookie / security-critical fields listed above. Preserve backend Sec-WebSocket-Extensions transparently: backend and visitor negotiate extensions; this proxy must not decompress, validate frames or fabricate extension offers. Rejecting extensions would break some legitimate apps, so full extension verification remains endpoints' responsibility.
- For valid 101 obtain `hyper::upgrade::on(&mut response)`, preserve end-to-end response fields, strip hops, rebuild canonical upgrade headers, set HTTP/1.1 status 101 and `Body::empty()`. Never pipe the upgraded connection through `Body::new(Incoming)`.
- New pre-upgrade errors use existing `Cache-Control: no-store`, request ID and attribution conventions: invalid visitor handshake → 400 / registry component (existing caller-error convention); invalid backend 101 → 502 / upstream; complete-response timeout → 502 / upstream; upgrade rendezvous failure after 101 → transport warning + close only. Safe reason strings must not echo credentials or raw frames. Reconcile any new failure-kind spelling with request-ids/inspection owners.

### Upgrade lifecycle / backpressure — PROPOSED

1. Enable `.with_upgrades()` on the Hyper tunnel client driver; spawn its polling just as current code does. Never await a connection driver inline with `send_request`. Retain its JoinHandle while negotiating: timeout/invalid-101 paths must explicitly stop/join this per-request **Hyper** driver if still running so a stalled backend cannot retain the substream. This is not the YamuxSession driver, whose graceful-drop invariant must remain untouched. Ordinary response streaming must keep its Hyper driver running after handler return.
2. For classified WebSocket requests only, create one absolute deadline (now + private 30-second constant immediately before `send_request`) and apply `timeout_at` to receiving complete response headers and, on valid 101, acquiring upstream upgraded IO. This supplements, not replaces, tnl's existing 3-second connect/15-second response-prefix synth behavior. Ordinary HTTP deadlines remain unchanged. Timeout drops request/driver-facing resources and returns one attributed HTTP error; verify backend/substream release in tests. Existing open_stream wait is not newly bounded by this deadline.
3. Once a valid upstream 101 is available, acquire its upgraded IO before committing visitor 101, with the same handshake budget remaining. Upstream upgrade failure before committing → 502 / transport, not an invalid successful handshake. It does not depend on returning visitor response. Spawn one task owning this upstream IO and the downstream OnUpgrade future; return visitor 101 immediately. Downstream rendezvous gets 5 seconds. Failure/timeout drops upstream IO, emits a metadata-only warning and ends the task.
4. After both upgrade sides are available, wrap in TokioIo and use `tokio::io::copy_bidirectional`. Buffers stay bounded independent of session duration/payload; no frame queues or collect-to-end. Retain Hyper's read-ahead and tnl's prefix replay exactly once.
5. Normal EOF half-closes the opposite writer while reverse bytes drain. On IO error drop both endpoints; after 101 do not synthesize HTML/JSON into the opaque channel. There is no newly promised timeout after one-sided EOF. Broken transport/session ends active connections; reconnect only serves new handshakes. Preserve YamuxSession graceful drop, role mapping and driver behavior.
6. Count/log one received handshake response consistently with existing request accounting. Client LogLine remains close-time; status 101 is derived from response prefix, byte counts include HTTP head plus WebSocket bytes, not message counts. No payload/header logging is added. No tnl/protocol production changes are expected unless a failing lifecycle test demonstrates necessity and the parent approves the additional seam.

### Non-goals

No access-control feature, visitor-IP trust policy, request-ID redesign, recording/replay, frame inspection/redaction UI, generalized transport rewrite, TLS backend support, WebSocket fan-out, automatic Origin rewriting, handshake retries, generic Upgrade/CONNECT support, HTTP/2 end-to-end support, global daemon graceful-shutdown redesign, rate limiting or quota system. No migration/deployment in this phase.

## 5. Dependencies, ownership and overlap

| Owned/proposed files | Work and coordination |
| --- | --- |
| `crates/tnld/src/data_plane.rs` | Primary implementation: classification, validation, handshake error mapping, two response paths and raw upgraded relay. **Shared hotspot** with visitor-ip, request-ids, access-controls. Parent must reconcile ordering before anyone edits this file. Access policy must run before opening a stream/committing 101 and must not be bypassed by upgrade fast paths. Visitor-IP/request-ID headers must be attached during handshake, never injected into subsequent frames. |
| `crates/tnld/tests/data_plane_websocket.rs` (new), in-file helper tests | Envelope/token/security rejection tests, real socket tests for upgrade availability, unknown-host/grace regressions. Keep test fixtures scoped; no broad harness refactor. |
| `crates/tnl-e2e/tests/websocket_roundtrip.rs` (new) | Real application handshake over existing authenticated yamux/WSS control path, endpoint echo/close/ping/raw-prefix/failure tests. Use existing async-tungstenite and Axum; if SinkExt/StreamExt is needed, add `futures.workspace = true` only to e2e dev-dependencies (`crates/tnl-e2e/Cargo.toml`). No root dependency changes anticipated. |
| `crates/tnl-e2e/tests/full_roundtrip_response_framing.rs` | Add non-upgrade framing/stripping/keepalive regressions if not better isolated in the new integration file. Existing test behavior must remain unchanged. |
| `deploy/Caddyfile`, `deploy/README.md`, `docs/RUNBOOK.md` | Remove incompatible wildcard override and document upgrade/Origin/Host limits; release-validation owns eventual Caddy runtime/combined deployment verification. Do not edit DNS/token/config secrets. |
| `crates/tnl/src/forwarder.rs`, `client.rs`, `reconnect.rs`, `inspector.rs` | **Not planned production edits.** Inspect/test only. Inspection/replay owns new schemas and payload policies. Any demonstrated lifecycle blocker requiring cancellation/pump changes must be reconciled with these owners before widening scope. |
| `crates/tnl-protocol/src/*`, configs/CLI, admin/registry | No planned edits or protocol version changes. |

Dependency order: approve D1-D6 and reconcile shared header/auth/log seams → helper validation tests → daemon upgrade bridge → end-to-end and failure/lifetime tests → Caddy config/doc change → workspace checks/reviewer → release-validation's one combined deployment/test shot with the other lanes. Design lanes need not implement each other. There is no implementation dependency on replay or frame inspection. Access-controls policy decisions are an integration dependency even if independently implemented.

## 6. Numbered execution subtasks (after approval)

1. **Freeze contracts and seams.** Parent records D1-D6 approvals and agrees data-plane ordering with visitor-ip/request-ids/access-controls. Confirm no new wire/config/public LogLine fields. Define fixture cleanup and real transport-loss trigger; do not rely on reconnect hook cancellation.
2. **Add failing handshake/security tests and private validation helpers.** Cover the exact token/singleton/framing/security-nomination rules above, including repeated Connection fields, mixed casing and comma lists. Add invalid-101 tests that prove Hyper alone is not sufficient. Existing ordinary stripping tests should assert behavior, not private implementation layout.
3. **Implement split HTTP vs upgrade request path.** Capture downstream OnUpgrade; retain existing host/session/error logic; ordinary requests continue Connection close and HTTP streaming. For upgrades sanitize then restore only canonical upgrade headers. Drive conn.with_upgrades(). Add the upgrade-only response deadline without adding public timeout flags.
4. **Implement validated 101 response and raw relay ownership.** Check 101 before stripping/returning. Acquire upstream Upgraded, spawn downstream rendezvous/relay, return empty 101 body; forward non-101 normally. Test coalesced 101+first data bytes and downstream early data to prevent read-ahead loss. Test timeout/error cleanup and no HTTP synth after upgrade.
5. **Exercise full path and failure isolation.** Use real tnld + authenticated tnl session + local WebSocket backend. Test text/binary/ping/pong/close, concurrency with ordinary HTTP and another WebSocket, rejection/error paths, Host/Origin/credential invariants, backpressure and genuine session loss. Leave existing client/protocol behavior untouched unless a concrete blocker is escalated.
6. **Align Caddy and operator documentation.** Remove wildcard close override, keep API site/flush behavior, document D1/D2/D4/D5 limits and frontend versus backend TLS. Provide reproducible loopback-only Caddy fixture instructions for release-validation; do not run deployment or request certificates here. No change needed to already-compatible init snippet.
7. **Run focused and workspace checks; independent review.** Gather assertions, test names/output and clean diff/staging evidence. Reviewer must inspect rejection paths and socket ownership, not just happy-path echo. Parent reconciles all lanes before any combined release/deployment shot.

## 7. Concrete acceptance matrix

Every async test has a bounded outer deadline; use readiness barriers/oneshots and join fixture tasks instead of fixed sleeps as the correctness condition. Use ephemeral loopback listeners and throwaway test tokens only. Tests listed as new are implementation tasks, **not tests already run**.

| Case / proposed test location | Required evidence |
| --- | --- |
| `websocket_roundtrip::application_websocket_text_binary_ping_close` | Visitor receives valid 101; text and binary (including zero bytes and >64 KiB message), server-initiated data, ping/pong and close code/reason survive through tnld/yamux/tnl. Frames are not interpreted by proxy. Close handshake ends fixture/backend tasks. |
| `websocket_roundtrip::upgrade_read_ahead_is_not_lost` | Raw backend writes 101 and first frame in **one write**; visitor receives complete first frame once. Raw visitor coalesces request head + masked first frame; backend receives it once after handshake. Fragmented handshake headers also work. |
| `websocket_roundtrip::handshake_headers_host_origin_and_credentials` | Path/query retained; visitor Cookie/Authorization/Origin and key offered protocols/extensions reach backend; selected protocol and Set-Cookie reach visitor. Verify Preserve, Rewrite, Fixed, Auto initially preserve, Auto already activated rewrites only Host. Origin remains exact in every mode; backend Origin-rejection 403 is relayed and not auto-bypassed/retried. No control bearer injected into visitor request. |
| `data_plane_websocket::connection_tokens_and_protected_headers` | Repeated Connection lines and mixed-case tokens accepted if valid; ordinary nominated X-Hop field stripped on both handshake legs. Nomination of Host/Origin/auth/cookie/Sec-WebSocket-* rejected before local backend connection; malformed lists, close+upgrade, duplicate Upgrade/key/version, bad method/version, nonzero body/TE rejected. Backend connection counter stays zero for locally rejected envelope. |
| `data_plane_websocket::invalid_and_unsolicited_101_are_502` | 101 to ordinary request, wrong/missing Connection/Upgrade, missing/wrong/duplicate accept, non-offered/multiple selected subprotocol, framing-bearing 101 and protected-field nomination → attributed 502, never a visitor upgrade/raw relay. Backend dropped promptly. |
| `websocket_roundtrip::backend_rejection_remains_http` | Backend 401/403/426/200/redirect: exact status/body, useful headers and cookies survive; no 101 and no second backend connection. Missing Origin is passed, not fabricated; backend can reject it. |
| `websocket_roundtrip::upgrade_local_failures_and_header_timeout` | Connection refused, EOF, malformed local response, no first bytes retain existing synth behavior; a backend sending only `HTTP/1.1 ` then stalling is stopped by the new complete-header deadline. Assert attribution and no hanging stream. For short deadlines use Tokio virtual time/helper-level testing or a scoped test-only seam, not a new global timeout override/config flag. |
| `websocket_roundtrip::disconnects_do_not_poison_session` | Normal close and real RST/disconnect drop this stream; a second WS and normal HTTP still work. Backend-to-visitor and visitor-to-backend half-close each allow reverse final bytes (test with upgraded raw byte streams, not an invalid WebSocket-close assertion). D4 intentionally does not promise bounded cleanup for an uncooperative half-open endpoint. |
| `websocket_roundtrip::transport_loss_closes_existing_websocket` | Actually close/drop a retained underlying session transport (or terminate a test child client in a future authorized test phase); active WS cannot continue, backend task closes, reconnect/new session can accept a **new** WS. Do not claim existing WS resumes. No child processes are spawned in the present design phase. |
| `websocket_roundtrip::slow_reader_backpressure_and_parallel_http` | Bounded duplex/raw endpoint fixture saturates its small capacity; a sufficiently large producer blocks while reader is paused, then completes byte-exactly once reader resumes. Reverse direction progresses, another HTTP request/WS succeeds. Assert no unlimited application queue or payload accumulation in source review; avoid flaky exact RSS assertions. |
| `data_plane_websocket::registry_and_auth_paths_remain_closed` | Missing/unknown Host and grace/no-session statuses remain 400/404/503 for upgrade intent. Invalid/missing `/control` bearer remains 401; normal application WS needs no owner bearer. Reconciled access-control denial occurs before backend contact and before 101. Reserved paths remain reserved, explicitly documented. |
| Non-upgrade regression suite | Chunked requests/responses, close-delimited response, HEAD, 204/304, 103 then 200, 65+ headers, duplicate Set-Cookie, Connection-nominated stripping, streaming/SSE flushing and honest upstream errors remain correct. Two requests on one downstream keepalive TCP connection both route correctly and use independent substreams; required because Caddy close override is removed. |
| Inspection integration | One daemon handshake log/status and current close-time client summary with 101; no frame data/visitor secrets in logs. Record that abrupt pump error may omit client summary under current contract; inspection owner must not classify that as a new live-event API. No WS frames recorded or replayed. |
| Caddy release-validation fixture | Offline/local Caddy config (no ACME/DNS) fronts tnld over HTTP/1.1 with wildcard-style Host routing. Application upgrade/echo/close and ordinary keepalive/SSE work **through Caddy**; `/control` still connects. Adapt final reference config with only dummy env values and confirm no wildcard Connection-close rewrite remains. Installed Caddy behavior must be verified, not assumed from textual config. Public TLS/browser WSS smoke belongs to the authorized combined shot, not this design phase. |

### Commands to execute later, not executed here

From repository root after implementation (use `--offline` where dependencies/toolchain are preinstalled; otherwise stop and obtain network authorization rather than fetching silently):

```sh
cargo test --offline -p tnld --test data_plane_websocket
cargo test --offline -p tnl-e2e --test websocket_roundtrip
cargo test --offline -p tnl-e2e --test full_roundtrip_response_framing
cargo test --offline -p tnl-e2e --test full_roundtrip_many_headers
cargo test --offline -p tnl-e2e --test host_rewrite
cargo test --offline -p tnl-e2e --test inspector_e2e
cargo test --offline -p tnl
cargo test --offline -p tnld
cargo fmt --all
cargo clippy --offline --workspace --all-targets -- -D warnings
cargo test --offline --workspace
cargo build --offline --release --workspace

git diff --check
git diff --stat
git diff --cached --name-only
```

Caddy configuration syntax check for the later validation lane, without loading actual secrets or contacting DNS/ACME (requires installed Caddy binary and the DNS module used by the reference config):

```sh
TNL_DOMAIN=example.com TNL_DNS_PROVIDER=cloudflare TNL_DNS_TOKEN=dummy \
ACME_EMAIL=ops@example.com caddy adapt --config deploy/Caddyfile --adapter caddyfile > /tmp/tnl-websocket-caddy.json
```

`adapt` is not a runtime handshake test and does not prove certificate issuance. If the provider module is absent, report that prerequisite; do not install/fetch it in this design phase. The implementation's loopback fixture can use a plaintext site address to avoid DNS/TLS entirely; release-validation owns its exact runner and port allocation alongside the combined test plan. Required review evidence includes tests actually run, their outputs, changed files, residual risks and empty staging—not merely a successful build.

## 8. Integration risks / review gates

- Three independently upgrade-hostile behaviors must be fixed together: Caddy close rewrite, daemon request/response stripping and close insertion, and missing Hyper client `.with_upgrades()`. A direct-daemon echo alone misses Caddy breakage.
- Shared data-plane edits can accidentally bypass access checks, strip tracing/IP headers or duplicate request accounting; reconcile ordering before implementation and review both return branches.
- Hyper treats all 101s specially. A generic response-body return, missed upstream validation or waiting for downstream upgrade before returning 101 can lose data, switch an ordinary request into raw IO or hang the handshake.
- Accepted protocol-specific headers/extensions are end-to-end, not permission to retain arbitrary hop headers. Protected Connection nominations must not erase Origin/auth before backend checks. Raw post-upgrade bytes intentionally are not HTTP-filtered.
- Backend host allowlist smoothing is not Origin authorization. Some dev servers will continue to reject WS after Host rewrite; that is expected unless the operator configures the backend. Do not silently relax it in this lane.
- D4 retains no connection quota/idle limit; existing unbounded stream queues and detached tasks remain resource-exhaustion risks. Slow half-open peers may persist. A release claiming bounded connection lifetime would require an explicitly expanded task and cancellation ownership, not just copy_bidirectional.
- Older clients likely forward 101 bytes already, but no mixed-version support is promised without a test. Older daemons cannot handle this feature; old Caddy wildcard config also prevents it. Coordinate activation.
- Caddy may close upgraded connections on reload according to its own behavior; do not advertise uninterrupted WS across proxy reloads, tunnel reconnects or deployments. The combined shot should document observed disconnect/reconnect behavior.
- New live inspection/replay behavior must exclude opaque upgraded content unless separately designed; do not infer message boundaries or leak cookies/auth/frames from this transport work.
- No automated runtime validation was performed for this design. Independent reviewer approval and subsequent focused/full tests plus Caddy-integrated validation are mandatory before integration/deployment.

## 9. Investigation record

Read-only commands used: `pwd`, `git status --short`, `git branch --show-current`, `git rev-parse HEAD`, `git diff --name-only`, `git diff --cached --name-only`; local `find`/`ls`/`rg`/`nl`/`sed` for repository and available skill/dependency sources; `tar -xOf` on the cached tungstenite crate; `date +%F`. Read tools inspected the named implementation, callers and tests above. No cargo command, Caddy command, test, network call, secret file, deployment, commit, merge, staging or child-agent launch occurred. Final repository status/staging verification is reported with the task result.
