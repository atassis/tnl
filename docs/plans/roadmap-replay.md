# Replay in the single authenticated local browser inspector — revised task specification

**Status:** Design reconciliation only; no implementation authorization. This is a full replacement for the earlier Unix IPC/CLI replay proposal, not an amendment requiring readers to combine both documents.

**Decided by user:** ONE authenticated local browser inspector provides history/search/details and explicit, confirmed replay. Inspection owns the single loopback service, authentication, browser UI, bounded observer and sanitized display history. Replay owns a distinct opt-in bounded raw-request retention module and execution engine. No second control service, replay CLI, Unix socket or Unix-only replay restriction.

**Still proposed:** All limits, privacy defaults beyond explicit consent, exact flag/HTTP/type names, Host policy and confirmation mechanics below require reconciliation/approval. The browser choice does not approve raw retention or the upload sequencing repair.

**Baseline:** `/home/atassis/.config/superpowers/worktrees/tnl/replay`, branch `feat/roadmap-replay`, HEAD `43f2bc0ed29e00821e1ede624b8fe190eb0d5c57`. On revival an untracked `docs/plans/roadmap-replay.md` was already present; it is not modified by this task. Only this external output artifact is authored.

## 1. Evidence and required reconciliation

Read `AGENTS.md`, relevant local brainstorming/writing-plans guidance, the previous replay artifact, current code and these sibling proposals:

- `/home/atassis/.config/superpowers/worktrees/tnl/inspection/docs/plans/roadmap-inspection.md` (read during this run; sibling is itself being revised).
- `/home/atassis/.config/superpowers/worktrees/tnl/visitor-ip/docs/plans/roadmap-visitor-ip.md` (negotiated bounded metadata envelope; request-ID addition is a coordinated seam, not yet implemented).

Verified implementation remains unchanged at baseline:

| Implementation evidence | Replay implication |
| --- | --- |
| `crates/tnl/src/main.rs:37-86,186-225`, `commands/http.rs:5-34`: HTTP command, Host/verbosity/JSON flags, log renderer and reconnect; no browser service, history or replay. | New local API/UI belongs to inspection, not a second replay server. Keep credential config unchanged. |
| `inspector.rs:9-26,65-80`, `forwarder.rs:523-529`: local `LogLine` has ID/method/path/counts/status, no body/header store, and uses lossy `try_send`. The protocol `LogLine` at `tnl-protocol/src/messages.rs:50-71` is a different type. | Neither log stream is replay material. Never serialize raw requests into either or use a lossy queue for executable captures. |
| `peek.rs:13-65`, `forwarder.rs:185-201`: 8 KiB head probe; capped/EOF/timeout are not proof of a complete HTTP request. Metadata parser at `forwarder.rs:131-163` is permissive UTF-8 inspection, not validation. | Observer must explicitly distinguish complete, malformed, truncated and unsupported requests. Observation rejection must not reject ordinary forwarded traffic. |
| `forwarder.rs:242-300,383-420`: effective Host is selected, head/leftover written, backend response prefix awaited, only then remaining request body pumped. | Large upload whose backend waits for the body can stall. This is code-inspection evidence, not a reproduced failure. Concurrent upload/response sequencing is an explicit prerequisite pending approval; replay must not copy that ordering. |
| `forwarder.rs:354-441`: bounded response prefix drives status hint and Host block detection. | Preserve existing Host detection; new observer handles final status after informational responses. Captured effective Host is the backend-hop value, not necessarily public Host. |
| `target.rs:10-17,47-73`, `connect.rs:59-106`: bare port resolves localhost; literal IP targets include non-loopback; three-second connect ceiling. | Fixed backend does not automatically mean loopback. Proposed narrower replay destination rule must be approved and checked before connect. |
| `client.rs:181-206`, `reconnect.rs:53-114,129-142`: separately spawned forwarders, reattach versus recreate with tunnel ID, shared contexts. | Process-scoped service/store; generation follows tunnel identity, NOT successful connection count. Existing literals in reconnect and `tnl-e2e/tests/host_rewrite.rs:147`, and `Hooks` in e2e reconnect need coordinated edits. |
| `tnld/src/data_plane.rs:15-47,171-241`: hyper HTTP/1 on a fresh substream, hop/framing fields stripped, Connection: close, streamed bodies. `serve.rs:123-140`: existing daemon router. | Capture is the post-proxy backend hop, not exact external wire bytes. Replay connects directly to fixed backend; no new daemon route or control message. |

Existing tests inspected previously include `tnl/tests/{forwarder_peek,peek_request_head,forwarder_local_no_response,cli_basics,progress_format}.rs` and `tnl-e2e/tests/{inspector_e2e,host_rewrite,full_roundtrip_response_framing}.rs`. They do not prove replay, complete request capture, browser authorization or redaction. No tests were executed in either design phase.

### Conflicts in the inspected sibling snapshot that this reconciliation replaces

1. Its read-only API/no replay route and replay non-goal become a single service with tightly scoped replay POST routes; read endpoints still cannot fetch raw data.
2. Its `session_generation` increment per successful connection is replaced by `tunnel_generation` stable on reattach, fresh on recreation. Optional `connection_sequence` may count transport connections separately but must not gate replay identity.
3. Its observer scratch remains bounded and presentation history remains sanitized. **Replay**, not inspection history, owns complete unredacted request retention, expiration, execution snapshots and eligibility.
4. Its optional request ID/visitor inference is replaced by consumption of the visitor-IP owner's negotiated metadata envelope containing visitor identity AND canonical request ID after reconciliation. No raw-header trust path or second envelope.
5. Its blanket “no request bodies” local API policy must allow ONLY the bounded strict JSON replay POSTs below. Its API concurrency limits must not be occupied for an entire backend replay; accepted execution is asynchronous.

Parent must install/reconcile both final documents; reading a sibling draft does not imply its owner has approved these proposed exact contracts.

## 2. Architecture, alternatives and scope

**Recommended under the chosen browser architecture:** One bounded observer validates backend-hop framing and synchronously fans borrowed request events to two independent consumers: inspection sanitizes immediately into display history; replay, only after explicit raw-memory consent, builds bounded raw requests. Inspection's authenticated HTTP service invokes a narrow replay facade, never obtains a raw snapshot. The engine sends once directly to the immutable local target and returns safe metadata/outcomes to the same UI.

Alternatives considered within this architecture:

- Put raw bytes inside inspection history: fewer stores, but makes sanitized detail/search/serialization a credential-exfiltration seam and couples display limits to faithful replay. Rejected by ownership contract.
- Reuse only sanitized previews: superficially simpler, but redaction/truncation changes signatures, bodies and auth and can trigger incorrect side effects. Reject rather than offer “best effort replay.”
- Independent replay parser/store fed by another full-stream copy: avoids shared event definitions but duplicates framing state and memory, risks disagreement and an unbounded fan-out queue. Prefer one parser with bounded borrowed events, separate raw ownership. Revisit only if the observer cannot provide the contract below without compromising live forwarding.

**In scope:** one user-confirmed finite HTTP/1 request replay, complete binary-safe entity body, sanitized preview/eligibility, raw-retention opt-in, linked fresh replay ID and bounded outcome history, explicit uncertain-delivery UX.

**Non-goals:** disk storage/export/import/raw downloads, request/header/body editing, arbitrary target override, replay CLI/IPC, second listener, automatic/batch/scheduled replay, redirects/retries, WebSocket/CONNECT/upgrade/frame replay, partial streaming replay, HTTPS upstream targets, response-body archives, changing daemon authorization or visitor/request-ID policy. No Unix-only guard; ordinary supported browser/client platforms share this service design.

## 3. Material unresolved decisions

- **D1 Privacy/consent:** approve proposed `--replay-retain-secrets` requiring `--inspect`, raw request bytes in memory only, off by default, never implied by body previews or verbosity. Includes URL/query, all headers, cookies, authorization, signatures, multipart/binary and body secrets. Redaction protects presentation only; swap/core dumps/local memory inspection remain outside the guarantee; no zeroization claim.
- **D2 Resource values:** proposed raw max 20 builders/completed entries, 16 MiB aggregate charged raw allocations, 1 MiB entity body, 8 KiB head, 10-minute raw lifetime from admission. One execution at a time; response 32 KiB head / 1 MiB entity drain / 30-second total. Confirmation/outcome limits in §7 also remain proposals. These are additive to inspection's budget, not included in its sanitized-history estimate.
- **D3 Destination and Host:** propose only original backend target, loopback IPs only (including validated localhost resolution), captured effective Host with NO per-replay Host editor. This intentionally excludes existing LAN/public literal targets from replay while preserving their normal forwarding. If LAN replay is required, approve that separately; do not silently relax to arbitrary URLs.
- **D4 Confirmation/API policy:** approve mandatory fresh per-attempt browser confirmation for ALL methods, single-use bounded confirmation token bound to exact capture/generation/target policy, asynchronous accepted execution and safe duplicate suppression. No automatic execution retry after browser/network failure; potentially-sent warning persists.
- **D5 Upload sequencing prerequisite:** explicitly approve and assign minimal concurrent request-upload/response-probe repair, with synthesis/cancellation semantics preserved. Coordinate with WebSockets/inspection. This is NOT approved merely because browser replay was selected. Until approved and tested, do not claim complete large/chunked webhook support or implement a hidden forwarder rewrite in replay.
- **D6 Shared authority/privacy:** freeze negotiated metadata envelope including canonical request ID, visitor identity/provenance, and legacy unavailable semantics with visitor-IP/request-ID owners. Visitor IP is personal data; display/history policy is inspection-owned and must be approved. Replay outcomes identify the original visitor only as source context, never as the actual replaying peer.
- **D7 Local bootstrap:** confirm inspection's proposed independent 256-bit fragment-delivered capability, stderr link warning, single IPv4 loopback bind, strict browser mutation policy and UI browser compatibility. Service choice is decided; exact token delivery and operational limits are still proposed. No tunnel account token reuse.

## 4. Proposed CLI and browser flow

```text
tnl http 3000 demo --inspect
# Authenticated history/search/details; no executable raw retention.
tnl http 3000 demo --inspect --replay-retain-secrets
# Same service and UI, plus explicitly opted-in bounded raw retention.
```

Other inspection flags remain inspection-owned; no new TOML/env setting, replay subcommand, replay port or listen address. Without `--inspect`, the retention flag is a usage error before remote work. Configuration cannot be enabled retroactively by browser JavaScript: later consent cannot recover prior/redacted requests. Startup stderr explicitly warns that raw potentially secret bytes are retained and lists caps/lifetime; browser has a persistent “Raw replay retention ON” warning. Display JSON preview consent is separate and never implies replay consent.

Browser flow (inspection owns implementation):

1. Open the single startup fragment link; UI strips token with `history.replaceState`, retains it only in JS memory and polls sanitized history. Search never indexes raw bytes. Select a row; details show sanitized request/response data, source request ID, visitor provenance if available, capture ID and replay availability.
2. No raw consent: show disabled Replay with “Raw retention was not enabled; restart with explicit consent and send a new request.” Pending request: “Waiting for complete request/original attempt to finish.” Unsupported, malformed, truncated, over-limit, old-generation or replay-origin row: show precise non-executable reason. A JSON preview marked available NEVER implies replay eligible. Empty complete body is eligible.
3. Click **Review replay** only for a candidate row. Server revalidates eligibility and creates a short-lived preview; no backend connection. Modal contains sanitized method/path from display history, fixed backend target, captured-Host policy label, body length, capture/request IDs, retention expiry, fidelity limits and “This may repeat real side effects, including for GET. Original credentials/signatures/idempotency headers will be resent; signatures may have expired.” Do not display raw Host/auth/query/body values from replay storage.
4. Require an initially unchecked **I understand this sends another request and may repeat side effects** checkbox and a distinct **Send once** button. The POST must carry server confirmation token and `confirm_side_effects: true`; changing selection, closing modal, expiry or generation change invalidates UI preview. No automatic submission, Enter-to-send on selecting a row, or replay on modal open. Double-click disables button; server independently prevents duplicate token execution.
5. On accepted response, show new replay ID linked to original capture, status polling and an outcome card, not a new replayable external capture. Complete HTTP 4xx/5xx/3xx is a completed response, not retry instruction. No raw replay request/response body is available.
6. If POST response/polling is lost, show **Delivery unknown — the backend may already have acted. Do not resend automatically.** Poll safe GET using the preview-reserved replay ID if possible. Unknown/evicted outcome never means “not sent.” Any further attempt requires opening a new preview and acknowledging side effects again. No browser reload/refresh automatically replays a POST.
7. If raw data expires/evicts between selection, preview and execution: 410/unavailable, close/disable confirmation and show reason; never use a stale display copy. If display row itself has expired, use a plain “Capture no longer available” panel. Distinguish display-body omission from raw replay truncation.

## 5. Shared observer and identity seam (PROPOSAL)

### 5.1 Client-only common types and event ownership

Create `crates/tnl/src/observation.rs`, owned by inspection with integrator serialization of shared edits. Concrete proposed definitions:

```rust
pub struct CaptureId(pub ulid::Ulid);       // local row identity, never an auth token
pub struct TunnelGeneration(pub ulid::Ulid);
pub enum CaptureOrigin {
    External,
    Replay { parent_capture_id: CaptureId, replay_id: ulid::Ulid },
}
pub struct CaptureContext {
    pub capture_id: CaptureId,
    pub tunnel_generation: TunnelGeneration,
    pub origin: CaptureOrigin,
    pub started_at_unix_ms: u64,
    pub source_metadata: Option<tnl_protocol::request_metadata::RequestMetadataV1>,
}
pub enum RequestFraming { Empty, ContentLength(u64), Chunked }
pub struct ObservedRequestHead<'a> {
    pub method: &'a http::Method,
    pub path_and_query: &'a http::uri::PathAndQuery,
    pub headers: &'a [(http::HeaderName, http::HeaderValue)],
    pub raw_head_bytes: usize,
    pub framing: RequestFraming,
}
pub enum RequestCaptureEnd {
    Complete { entity_bytes: u64, has_trailers: bool },
    Incomplete,
    InvalidFraming,
    HeadLimit,
    Upgraded,
}
```

`CaptureId` and `TunnelGeneration` are typed immutable IDs serialized as canonical ULID strings only in safe models. The observer generates ONE CaptureId at admission; both consumers use it. It is not the old local `LogLine.req_id` or a public correlation header. Integrator/request-ID owner decide existing log linkage; never silently equate independent baseline IDs.

One admitted exchange has one context and synchronous request callbacks:

```text
on_request_head(&ObservedRequestHead)       // at most once, validated post-rewrite head
on_request_body(&[u8])                     // zero or more transfer-decoded entity slices
on_request_end(RequestCaptureEnd)          // exactly once
on_attempt_terminal(AttemptTerminal)       // exactly once: completed/failed/cancelled/upgraded
```

`AttemptTerminal` is a closed client-only enum with `Complete`, `Failed`, `Cancelled`, `Upgraded`; it carries no arbitrary error strings. Observer response events/framing remain inspection-owned. Replay needs only request events plus terminal/upgrade state. Captured bytes and callback references cannot escape into event queues; replay copies only within its charged raw budget, inspection immediately sanitizes within its separate working budget. No awaiting locks/I/O/browser consumers, cloning entire streams, or Debug-logging event payloads. Parser header list is bounded by 8 KiB and proposed maximum 128 parsed fields; presentation may retain fewer fields under its own cap without limiting raw fidelity. Bound all parsed/scratch allocations, not just bodies.

Wire seam ordering: visitor-IP-owned envelope reader (when negotiated) -> HTTP head probe -> existing effective Host transform -> bounded observer's HTTP parser -> live backend bytes. Envelope bytes NEVER enter HTTP observation, body counts or retention. Feed head, already-read leftover and subsequent fragments exactly once at the intended backend-write boundary, before each corresponding write; successful-write counters remain separate. A failed write may leave retained intended request bytes that were not delivered, so capture completeness must never be presented as proof of original delivery. Connect failures that short-circuit before a complete intended backend request was observed remain non-replayable; do not drain the visitor upload merely to complete capture after synthesizing a failure. Body events contain dechunked entity bytes but preserve Content-Encoding and bytes exactly; do not decompress, normalize JSON/multipart, decode URL escapes or reconstruct redacted data. On malformed/ambiguous framing, unsupported transfer coding, trailers, upgrade or extra request/pipeline bytes, invalidate raw eligibility; live forwarding remains unchanged. One request per substream is the daemon contract, not keepalive request capture.

Inspection admits at most its proposed 64 observers before allocating parser scratch. Non-admitted traffic still forwards, with no hidden raw-only capture. Raw module independently admits at most its own count/bytes; raw pressure cannot evict/block sanitized history or forwarding. If a display row is evicted while observer active, detach its observation and tell replay to forget that capture; do not retain unreachable raw records. One shared observer/parser is not ownership of replay bytes by inspection.

### 5.2 Metadata authority and generation

Visitor-IP owner supplies/decodes the single negotiated metadata envelope and legacy fallback; request-ID owner freezes its canonical ID field inside that envelope. The `RequestMetadataV1` symbol above means the jointly frozen envelope **including** visitor and request ID; the sibling's visitor-only shape is not sufficient for combined integration. Replay neither implements a second prelude nor infers visitor/request ID from `X-Forwarded-For`, `X-Tnl-Request-Id`, `X-Tnl-Replay` or other incoming application headers. Malformed negotiated metadata follows that lane's fail-closed substream policy before any local connect/observer. Legacy no-envelope mode provides `None`; never synthesize authoritative visitor identity from backend target or inspector browser peer.

`TunnelGeneration` is allocated on initial successful CreateTunnel and each successful fresh recreation; retain across successful reattach to the same server tunnel ID. Validate echoed ID consistency on reattach. A transport `connection_sequence` may increment independently. Failed attempts do not create generations. New generation makes old raw entries non-executable and drops their retained bytes; sanitized old rows remain until inspection eviction/age. Old live forwarders may finish under their original generation and must not overwrite the new one or be declared disconnected solely from a connection counter. Execute-time generation check, snapshot pin and operation acceptance are serialized with generation changes. An already accepted replay keeps its immutable target/generation and is not restarted/rebound on recreation. During reconnect/disconnected state, browsing remains available but new execute is rejected as `tunnel_unavailable` until the same generation is active again.

## 6. Replay-owned raw retention and facade (PROPOSAL)

Owned namespace: `crates/tnl/src/replay/{mod.rs,retention.rs,engine.rs,model.rs}`. No replay HTTP router, browser assets or second store inside inspection history. Export only crate-internal safe facade types to inspection:

```text
ReplayController::observe_start(&CaptureContext) -> Option<RawRequestObserver>
RawRequestObserver::{on_request_head,on_request_body,on_request_end,on_attempt_terminal}
ReplayController::availability(CaptureId) -> ReplayAvailability
ReplayController::prepare(CaptureId) -> Result<ReplayPreview, ReplayError>
ReplayController::confirm(ConfirmationToken, bool) -> Result<ExecutionReceipt, ReplayError>
ReplayController::outcome(ReplayId) -> Result<ReplayOutcome, ReplayError>
ReplayController::forget(CaptureId)
ReplayController::set_tunnel_state(TunnelGeneration, ActiveOrDisconnected, FixedTarget)
ReplayController::shutdown()
```

Inspection performs service authorization/strict input validation and sanitized-record existence check BEFORE invoking prepare/confirm; replay independently enforces token, generation, completeness, limits, fixed destination and concurrency. `ReplayAvailability` contains only `state`, fixed reason code, body length if known and `expires_at_unix_ms|null`. `ReplayPreview`/outcomes described in §7 contain no raw bytes. Raw snapshot type and accessors are private to replay; no derived payload Debug/Serialize and no conversion from inspection's display model. Where repository lints require Debug, implement a redacted ID/length/state-only formatter, never a raw-field formatter. `RawRequestObserver` detaches on budget/TTL failure and is idempotently finalized.

Private retained request contains CaptureId, generation, admission instant/time, trusted original context metadata, method, exact origin-form path/query, ordered repeated validated headers and full entity Bytes, effective Host, framing/completeness/upgrade/trailer flags and origin. Use monotonic time for enforcement and wall clock only for display. Do not retain response body, metadata prelude bytes or full raw wire framing. Raw means unredacted application request, not original TLS/wire reproduction.

Eligibility requires raw retention enabled at admission, External origin, valid complete head/framing/body, request end plus original attempt terminal, no cancellation/upgrade/trailers/extra bytes, current active generation, reachable sanitized row and bounds/policy compliance. An original backend failure/HTTP error does not itself preclude replay if request capture is complete and terminal; cancelled/incomplete requests are ineligible. Request state is not inferred from a successful response or EOF alone. Full finite chunked requests with no trailers are supported after observer and upload prerequisite; streaming/unbounded/partial requests are never replayable.

Proposed availability states: `disabled`, `pending`, `eligible`, `ineligible`, `expired`, `evicted`, `wrong_generation`. Reasons include `not_retained`, `request_incomplete`, `request_truncated`, `head_limit`, `invalid_framing`, `unsupported_method`, `unsupported_target`, `unsupported_trailers`, `upgraded`, `replay_origin`, `capture_limit`, `tunnel_unavailable`. No arbitrary parser/backend input in messages. Presentation-body omission does not select these states. Inspection may store bounded safe tombstone state on the still-retained row when raw is expired/evicted; replay does not maintain an unbounded tombstone map.

Proposed bounds: at most 20 raw builders/completed entries, 16 MiB aggregate charged allocation capacity (head/URI/header/body storage, retained context/metadata, active builders and pinned snapshots), body <=1 MiB, head <=8 KiB, raw TTL <=10 minutes from observer admission, oldest-admitted-first eviction. Reserve budget BEFORE growth/copy, count actual capacities/owned copies, cap parsed field count, and stop retaining rather than spill to disk/backpressure. Empty complete body differs from missing capture. Incomplete builders count and expire even if the stream never ends. Immutable `Bytes` snapshots share rather than duplicate body allocations, and remain charged if pinned through a bounded accepted execution; expiry removes discoverability immediately but an accepted attempt may finish. At most one engine operation pins raw; if budget cannot be reclaimed because it is pinned, drop new raw admission. Scheduled sweep plus every lookup/mutation enforces TTL. Display eviction calls `forget`; whole inspector failure/shutdown disables raw admission, cancels/drops pending previews and raw entries, and cancels accepted execution conservatively without claiming not-sent.

Budget is ADDITIVE to inspection's proposed ~19 MiB payload budget. Proposed replay control/status cap 256 KiB plus at most 8 pending preview tickets; engine response streaming holds at most 32 KiB head and one <=8 KiB scratch chunk, never an entire 1 MiB response. Shared body references, JSON serialization capacities and token/outcome storage must be accounted. These are bounded payload allocations, not a total RSS guarantee for all tunnels or arbitrary live forwarding.

## 7. Single-service HTTP/security/confirmation contract (PROPOSAL)

### 7.1 Service boundary — inspection owned, required for replay

Bind only `127.0.0.1`, chosen port (default 0), with the existing inspection proposal's independent cryptographically random 32-byte capability printed once in a sensitive URL fragment to stderr. No remote bind, `localhost` DNS, second port, public daemon route, account-token reuse, cookies, URL query tokens or implicit HTTP auth. No automatic browser launch required. Browser strips fragment immediately; token stays only in JS memory, not local/session storage, logs, DOM text or outcomes. New process gets a new capability. Constant-time comparison using a supported direct dependency; never log Authorization or JSON input. Same-user malware, malicious extensions and compromised local browser/process are outside this boundary.

Security middleware applies before route/resource lookup:

- Exact authority/Host is `127.0.0.1:<actual-port>` on **all** routes; reject absent/duplicate/foreign Host, aliases, forwarded-authority substitution, wrong port and absolute-form URI authority mismatches. Check socket peer loopback as defense in depth. Reject requests to this service as replay backend even with a different Host.
- Any supplied Origin must equal exactly `http://127.0.0.1:<actual-port>`; reject `null`, foreign scheme/port/host, duplicates and malformed Origin. Mutating routes REQUIRE that exact Origin, even with a correct bearer. Reads may omit Origin because normal same-origin GETs do; missing Origin never substitutes for authentication.
- ALL `/api/v1/*` requests require `Authorization: Bearer <capability>`, including unknown resources/methods and replay status. Exactly one valid bearer; no captured auth value accepted as inspector auth. Static root contains no history/capability and may be unauthenticated under authority/Fetch Metadata checks.
- For replay POSTs require `Sec-Fetch-Site: same-origin`, `Sec-Fetch-Mode: same-origin`, `Sec-Fetch-Dest: empty`, exactly one of each. Missing/foreign/cross-site/same-site/none values fail closed. UI uses `fetch` with `mode: 'same-origin'`, `credentials: 'omit'`, `redirect: 'error'`. Read API requests with Fetch Metadata present must satisfy the same tuple; absent metadata may be allowed for authenticated read-only non-browser callers. Root navigation allows absent metadata or `Site: none|same-origin`, `Mode: navigate`, `Dest: document`; reject cross-site root embedding. Browser support for mandatory mutation metadata is an acceptance gate, not an insecure fallback.
- No CORS responses/preflight allowance. OPTIONS never executes; reject unsupported methods rather than redirect. Root GET/HEAD and existing read API GET only; GET cannot mutate replay state. No method overrides, form endpoints, WebSocket mutation or authenticated redirects.
- POST requires exactly one `Content-Type: application/json` (no parameters/alternate types), no Content-Encoding, exactly one decimal Content-Length in 1..=1024, no Transfer-Encoding, and body read deadline three seconds. Cap headers at 32 KiB/deadline three seconds. Reject oversized/unknown-length/chunked/contradictory-length input before allocating proportional buffers; parser must honor declared length/connection framing. Read routes reject nonempty/transfer-encoded bodies. Limit applies to complete decoded input before deserialization. UTF-8 JSON object only, no trailing data, duplicate/unknown fields rejected, exact field types/enums and schema version required; no coercions or ignored confirmation. Route IDs canonical ULIDs; replay routes accept no query parameters.
- Proposed global four in-flight HTTP handlers with prompt 503 on saturation, bounded response snapshots and read deadlines; no long backend operation holds an HTTP permit. Per-process eight preview tickets and single replay execution add separate non-waiting limits. No unbounded queue/tasks triggered by POSTs.
- All responses: `Cache-Control: no-store`, `Referrer-Policy: no-referrer`, `X-Content-Type-Options: nosniff`; API JSON MIME type, no capability echo. Root CSP uses hashes for embedded script/style, `default-src 'none'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`, no external assets/eval/unsafe HTML insertion. Captured data renders as text nodes/textContent, never innerHTML or active links. No request logging that captures tokens, bodies or payload-bearing errors.

### 7.2 Routes and exact proposed schemas

Existing inspection-owned authenticated history/list/detail/search/stats GET routes stay sanitized. Extend detail with `replay: ReplayAvailability` and optional sanitized linked outcomes; list may contain availability state only. No API route returns a raw request, raw header/Host/query value, confirmation token in history, or replay response body.

**POST `/api/v1/requests/{capture_id}/replay-preview`**

Request: `{"schema_version":1}`. This creates a bounded short-lived confirmation ticket but sends no network traffic to backend and pins no raw payload. Reply 200:

```json
{
  "schema_version": 1,
  "capture_id": "ULID",
  "tunnel_generation": "ULID",
  "replay_id": "ULID",
  "confirmation_token": "opaque-random-token",
  "confirmation_expires_at_unix_ms": 0,
  "raw_expires_at_unix_ms": 0,
  "request_body_bytes": 0,
  "target": "localhost:3000",
  "host_policy": "captured_effective",
  "warnings": ["potential_side_effects", "original_secrets_retained", "signature_may_expire"]
}
```

Numbers above illustrate field types, not production timestamps. UI obtains sanitized method/path/request-ID/visitor context from inspection detail, not the raw module. Preview reserves a fresh replay ULID and a 32-random-byte confirmation token with <=30-second lifetime, bound to exact CaptureId, generation, immutable raw entry revision and fixed target/policy. At most eight tickets; issue fails 429 when full after expired-ticket cleanup. No automatic old-ticket overwrite/reuse; closing a modal drops it client-side and server expiry releases it. Token and reserved ID are not evidence an operation was sent. Capability is still mandatory to use token. Fixed tokens/deterministic ULIDs are not credentials.

**POST `/api/v1/requests/{capture_id}/replay`**

Request: `{"schema_version":1,"replay_id":"ULID","confirmation_token":"opaque-random-token","confirm_side_effects":true}`. No body/header/target/Host override fields; unknown fields fail 400. Missing/false confirmation fails 400 `confirmation_required` even with a valid capability and ticket.

Atomic confirm rechecks ticket binding/expiry, raw availability/completeness/revision, sanitized-row existence, generation-active status and target safety, obtains non-waiting execution permit, pins snapshot, creates running outcome and consumes ticket BEFORE starting any I/O. If busy return 409 without consuming an otherwise valid ticket; UI still requires click, not a hidden wait/retry. Expired raw/ticket never starts a backend request. On success return 202 `{"schema_version":1,"replay_id":"ULID","parent_capture_id":"ULID","state":"running","delivery":"unknown"}`; service spawns/owns one bounded execution task through replay facade, not a detached unbounded job per handler.

After service authentication and strict input validation, consult the consumed-token index before any new-execution path. A repeated execute with the SAME consumed token/ID/capture returns current running receipt (202) or saved outcome (200), even after the original preview expires or generation changes; it never sends again. This also holds if only one of concurrent duplicate HTTP requests got the original response. After the bounded outcome/token index is evicted, an unknown/expired token fails closed; it cannot become a new ticket. A NEW preview is always a new explicit attempt. Browser does not automatically repeat execute at all; duplicate suppression is defense in depth, not a promise of exactly-once application side effects.

**GET `/api/v1/replays/{replay_id}`**

Authenticated read, no body/query. Reply 200:

```json
{
  "schema_version": 1,
  "replay_id": "ULID",
  "parent_capture_id": "ULID",
  "tunnel_generation": "ULID",
  "state": "running",
  "delivery": "unknown",
  "status": null,
  "duration_ms": 0,
  "request_body_bytes": 0,
  "response_body_bytes": 0,
  "response_complete": false,
  "error_code": null
}
```

`state` = `running|complete|failed|cancelled`; `delivery` = `not_sent|possibly_sent|response_received|unknown`. `unknown` is necessary for accepted running or browser loss; it is not not-sent. `error_code` fixed enum/null. Poll no faster than once per second while the outcome panel is visible; pause when hidden, with no automatic execute retry. Record canonical original ID/visitor only as inspection-owned source context (with `source_capture_id`), not a new visitor attribution or new canonical external request ID. Replay ID is the attempt identity. Proposed authoritative outcome/consumed-token index: maximum 32 records, 256 KiB total, 15-minute age, oldest finished first; never evict the running one. Sanitized outcome presentation copies count against inspection's history budget. Unknown/not-yet-accepted/expired outcome returns 404; browser must treat it as unknown delivery after any execute attempt, not proof of refusal.

**Errors:** bounded `{"error":{"code":"...","message":"fixed safe text"}}`; no input echo or arbitrary exception message. 400 malformed/schema/confirmation; 401 missing/invalid capability; 403 authority/origin/Fetch Metadata; 404 unknown display capture/outcome; 405 method; 409 busy, wrong generation, tunnel unavailable, changed preview; 410 known expired/evicted raw/ticket; 413 body limit; 415 media/encoding; 422 known ineligible/unsupported request; 429 preview capacity; 503 service saturation/disabled. Codes: `invalid_request`, `confirmation_required`, `unauthorized`, `forbidden`, `not_found`, `busy`, `wrong_generation`, `tunnel_unavailable`, `preview_changed`, `expired`, `evicted`, `ineligible`, `unsupported_request`, `local_target_required`, `disabled`, `overloaded`, plus engine failure codes below. API middleware/auth ordering must not disclose capture existence to unauthorized callers. Successful preview/execute cannot bypass service auth by calling module internals from an HTTP convenience route.

## 8. Replay engine constraints (PROPOSAL)

**Fixed destination:** process-configured backend target, never captured Host/URI, browser input or public tunnel URL. Initial scope loopback only: resolve bare-port localhost ONCE, require every candidate to be loopback, normalize IPv4-mapped IPv6 before checking, connect only validated literals within the existing three-second deadline. Reject LAN/public/unspecified/multicast candidates before connect, with no second DNS lookup inside `connect_local`. Explicit literal target same check. Reject inspector listener address/port including loopback aliases/mapped forms and the same port on alternate loopback addresses, so Host manipulation cannot replay into the control API. No general hostname/URL override, proxy env, redirecting HTTP client or ambient auth/cookies/.netrc.

**Request validation:** allow GET/HEAD/POST/PUT/PATCH/DELETE/OPTIONS with origin-form path, one syntactically valid Host and valid bounded headers. Reject CONNECT/TRACE/custom methods, absolute/authority/asterisk-form targets, CR/LF/control injection, duplicate/missing Host, unsupported version/framing/transfer coding, trailers, upgrades, oversized/incomplete/truncated requests. Observer may view unsupported requests but never converts their sanitized representation to executable bytes. Require original attempt terminal before eligibility; no simultaneous duplicate of a still-running original request.

**Host/fidelity:** preserve effective captured Host, not current Auto learned state, with no adaptive rewrite or per-replay editor in this milestone. Existing live `--host-header` still affects what was captured. Show policy label, not a raw-secret fetch. Preserve method, path/query octets/escaping, repeated end-to-end header values and complete entity bytes including binary/compressed/multipart. Never decompress, JSON-reencode, normalize query order or synthesize missing secret values. Header casing/order and chunk framing are not byte-faithful. Original credentials/signatures/idempotency headers are resent only from opted-in raw capture; state and timestamp changes can make them fail. Do not inject account token, new replay trust headers, fresh visitor headers or impersonation claims.

**HTTP serialization:** use workspace hyper HTTP/1 connection handshake over a manually connected Tokio socket (`hyper-util::rt::TokioIo`) with `http_body_util::Full<Bytes>`. Add direct workspace/member dependencies as needed, including workspace `http-body-util`; do not rely on transitive dependencies. Strip hop-by-hop and every Connection-nominated field, original framing fields, Expect and Upgrade; reject Connection nominating Host; recompute body framing, HTTP/1.1, Connection: close. One fresh backend connection, no automatic retries, no redirects even for 307/308, no adaptive resend. Drive connection/upload/read concurrently; do not gate upload on first response bytes.

**Response/time:** three-second total connect ceiling, fifteen-second first final-response deadline after connection establishment (within total deadline, without serializing upload), thirty-second total execution deadline. Bound response head parsing/buffer to 32 KiB and consume <=1 MiB response entity using bounded scratch; skip ordinary informational statuses to final response through hyper, bound informational handling by total head/operation limits, reject 101/upgrades and protocol switching. No whole response buffering/body display/archive, no decompression. Body cap/deadline closes connection and records partial response with known status if available, not false success. Completed 3xx/4xx/5xx counts as completed HTTP response. Engine codes: `connect_failed`, `write_failed`, `response_timeout`, `response_invalid`, `response_too_large`, `cancelled`.

**Delivery truth:** validation/connect failure before any bytes possibly written = `not_sent`; once sender may have written any request bytes, failure/timeout/cancel = conservatively `possibly_sent`, even if no HTTP response arrived. Final complete response = `response_received`, not “business action succeeded.” Running/lost browser result = `unknown` until authoritative outcome is fetched. No synthetic 502 pretending to be backend response; report replay outcome independently of live tunnel synth attribution. Browser disconnect does not cancel/retry an accepted operation; bounded task finishes and records result. Whole process/service shutdown cancels with conservative delivery; lost process state cannot prove not-sent.

**Provenance/no recursion:** engine uses direct local connection, NOT daemon/yamux/normal forwarder. Outcome is a Replay-origin child of original CaptureId with fresh replay ID, never a raw-capture candidate. If inspection reuses the observer for replay response metadata, trusted in-process `CaptureOrigin::Replay` disables raw admission before any copy; no replay of outcomes. Spoofed incoming `X-Tnl-Replay` cannot set origin, suppress observation or grant permission. Original visitor identity is contextual metadata from envelope, not current replay source or authority. Arbitrary backend-triggered external callbacks are outside prevention guarantees; tnl itself never automatically initiates another replay.

## 9. Dependency order and file ownership

1. **Parent reconciliation first:** install revised inspection/replay specs together; freeze D1–D7, observer/types, API schemas and outcome budgets. Explicitly approve/name owner of upload sequencing fix. Visitor-IP owns single metadata envelope/negotiation; request-ID owner adds canonical ID there before version freezes. No replay protocol edits.
2. **Inspection foundation:** owns `src/observation.rs`, bounded HTTP framing/observer, `inspection/{model,redact,history,observe,server}.rs`, `inspection/ui.html` and single-service CLI/lifecycle. Sanitized history and parser must work independently of raw opt-in. Inspection provides trusted row-lifecycle/eviction callbacks and authenticated service facade wiring.
3. **Replay module:** owns new `crates/tnl/src/replay/{mod.rs,retention.rs,engine.rs,model.rs}` and source-named unit tests; `crates/tnl/tests/{replay_retention,replay_engine}.rs`. It owns raw privacy warning/help content and engine/retention limits. It does NOT own service/auth/UI implementation or a second parser/listener.
4. **Shared serialized edits:** integrator assigns one writer for `forwarder.rs`, `client.rs`, `reconnect.rs`, `commands/http.rs`, `main.rs`, `lib.rs`, root/member Cargo manifests/lockfile and affected literal test contexts. Prefer inspection hooking one observer that fans events to replay facade; do not independently add multiple ForwardCtx capture IDs/hooks. Generation owner in reconnect explicitly distinguishes CreateTunnel/reattach.
5. **Service/UI tests:** inspection owns `tests/inspection_api.rs`/browser source tests and browser UI; replay supplies required preview/execute/duplicate/uncertain-delivery fixtures, with combined `tnl-e2e/tests/replay_e2e.rs` owned by replay after seam lands. Visitor metadata and WebSockets combined tests use negotiated framing and upgrade terminal signals. No raw WS frames retained.
6. **Docs/release:** inspection owns browser startup/search/detail docs; replay adds opt-in/fidelity/side-effect/limits sections through coordinated README/RUNBOOK/CHANGELOG edits. Release-validation owns eventual combined validation/deployment shot. No deployment, credential access, merge, publish or new children in this task.

## 10. Numbered implementation tasks (after approval; test first)

1. **Contract fixtures and authority.** Add common typed CaptureId/generation/origin and observer event fixtures under inspection-owned seam. Demonstrate single ID across consumers, stable reattach/fresh recreation and envelope-only metadata authority. Coordinate existing `ForwardCtx`/`Hooks` literals. No wire field/header changes in replay.
2. **Replay raw retention in isolation.** Add retention tests before module implementation for consent-off, binary body, empty-versus-missing, strict framing end/terminal requirement, independent display/raw eligibility, exact caps/TTL/generation, pin accounting and no payload-bearing Debug/serialization. Fake monotonic clock; no global test timeouts.
3. **Bounded engine.** Add failing local backend tests for exact entity/query/end-to-end-header delivery, captured Host, destination validation/no double DNS, no redirect/retry, upload concurrency, final status after 1xx, response/time caps and conservative delivery. Implement manually connected hyper path and private snapshot handoff; assert zero connects on policy failure.
4. **Confirmation/outcome facade.** Test prepare sends nothing; <=8 expiring tickets; bind all identity/policy/revision fields; missing/false confirmation rejected; atomic generation race; one execution permit; same token concurrent/repeated execute cannot double-send; outcome/token eviction fails closed; reserved ID recovers outcome after lost POST reply. Bound task/status allocations independently of handler concurrency.
5. **Inspection service/UI integration.** Inspection adds exact protected POST/GET routes, strict JSON/body/method/Origin/Host/Fetch Metadata rules, facade calls and sanitized state display. API tests must prove correct capability ALONE is insufficient to mutate from wrong/missing Origin/Fetch Metadata; no raw fetch/alternate method backdoor. Browser flow requires explicit checkbox/modal, handles expired/truncated/unknown states and never automatically retries. No second service or new replay CLI.
6. **Observer/upload/generation integration.** After distinct sequencing approval, land minimal upload fix through its assigned owner with inspection/WebSockets collaboration; feed bounded parser events once at backend boundary, including leftovers, and preserve synth/block detection/transport drop shutdown behavior. Propagate negotiated metadata before observer. Raw overflow or observer failure leaves forwarding intact. Old stream completion cannot change current generation. Service failure shuts replay down without claiming safe delivery.
7. **Cross-crate/privacy acceptance and docs.** Add e2e fixture: external signed synthetic POST -> sanitized history -> preview -> confirmed replay -> exactly one additional local request -> linked non-replayable outcome, daemon count unchanged by replay. Test cancellation, generation rollover, consent-off, finite chunks and truncated requests. Inspection owner performs real local browser XSS/capability/confirmation checks under later test authorization. Record required gates and independent review; no lane-specific deployment.

## 11. Concrete acceptance matrix

All cases and commands are PROPOSED future validation, not results of this design run.

| ID | Required proof |
| --- | --- |
| A1 Consent separation | `--inspect` alone exposes history but raw retention stays disabled; body preview mode/verbosity cannot enable it. Raw flag without inspection fails usage. No socket/replay CLI/second port/Unix-only restriction. Later consent cannot reconstruct earlier requests. |
| A2 Fidelity | Binary/NUL/compressed/multipart request, repeated end-to-end headers and escaped/duplicate query parameters delivered intact at entity level on explicit replay. Captured effective Host preserved even after Auto state changes. Original synthetic Cookie/Authorization/signature/idempotency fields retained only after consent; no account-token/proxy/cookie-jar injection. |
| A3 Display privacy | Raw query values/Authorization/Cookie/custom-secret header/body canaries absent from history/search/preview/outcomes/logs/errors/Debug; no raw download or export. Tests respect inspection's documented residual path/allowlisted-header/JSON-value redaction limitations rather than claiming DLP. Visitor context comes only from envelope and is labeled original source context. |
| A4 Eligibility truth | Missing/expired/evicted/pending/cancelled/malformed/truncated/head/body-over-limit, unsupported trailers/upgrade/method/target, old generation, missing raw consent and Replay-origin records never connect. Complete empty body eligible; JSON preview available does not imply replayable; JSON preview omitted does not preclude a complete raw binary replay. |
| A5 Streaming prerequisite | Fragment every head/chunk/body boundary; leftover consumed once, finite chunked de-framed exactly, infinite/over-limit stream detaches raw retention without forwarding stall. Large delayed upload whose backend waits for full body passes only AFTER approved sequencing fix; before it no claim of full webhook acceptance. |
| A6 Explicit confirmation | Preview is backend-network-free. GET and POST sources both require separate checkbox/send and valid bound server token. False/missing/altered/expired/cross-capture/cross-generation token never sends; duplicate simultaneous submission sends once. New attempt requires fresh preview/confirmation. |
| A7 Browser/API authority | Correct bearer with evil/alias/duplicate/missing Host, wrong/null/missing mutation Origin, foreign/missing mutation Fetch Metadata fails; cross-site forms/fetch/preflight/GET/HEAD/method override cannot execute. Unknown API resource unauthenticated reveals no record. Only one local service, no daemon replay route. Same-origin legitimate browser requests succeed on supported browser. |
| A8 Strict bounded input | Wrong content type/parameters, encoding, chunked/unknown/overlarge length, overlong headers, slow body, invalid UTF-8/JSON, duplicate/unknown fields, wrong boolean/schema/ID, extra fields attempting target/body override all reject before module execution. No proportional allocation from hostile length, no request body/credential logging. |
| A9 Fixed destination/loop | Stub localhost resolver containing any non-loopback candidate rejected before connect with no second lookup. Normalize mapped IPv6, reject LAN/public/unspecified/multicast and own inspection port across aliases/Host changes. Absolute URI/Host cannot reroute. Direct replay never increments daemon request count or produces executable replay captures. Spoofed public marker cannot change trusted origin/metadata. |
| A10 Delivery/outcomes | Backend counter increments once per accepted ticket. Backend acts then disconnects/timeouts -> possibly_sent; browser loses POST reply -> unknown warning and read-only recovery by reserved replay ID. Poll 404/expired outcome is not not-sent. Completed 4xx/5xx/3xx not retried/followed. 101 invalid. Final response after informational prefix handled; oversized/infinite/malformed response bounded with truthful partial status. |
| A11 Bounds/lifecycle | Raw 20-entry/16-MiB/body/head/TTL caps include builders, capacities and pinned snapshots; raw pressure cannot block display/forwarding. Expire incomplete infinite request, pinned budget remains charged; cap tickets/outcomes/tasks/HTTP handlers and do not hide unbounded queues. Display eviction drops raw; accepted attempt bounded; process/service failure drops raw and records conservative cancellation when possible. |
| A12 Generation/compatibility | Reattach same tunnel keeps generation/capture availability; recreate changes generation and drops old raw, even same subdomain. Generation change racing preview/confirm cannot retarget; accepted attempt keeps original immutable context. Legacy metadata unavailable, no header inference; negotiated envelope visitor and request ID propagate once, no prelude in raw body/counters/backend. Existing Host/framing/synth/CLI tests unchanged when features off. |
| A13 UI/XSS | Malicious HTML/JS/ANSI/UTF-8 in sanitized fields cannot run script, make active links, leak capability or panic terminal rendering. No token persistent browser storage/external assets. Refresh never resubmits replay. Expired/truncated/disabled states are visibly non-actionable; delivery-unknown warning does not offer automatic safe retry. |

Proposed commands after implementation (fixtures/synthetic secrets only):

```sh
cargo test -p tnl --lib observation
cargo test -p tnl --lib replay
cargo test -p tnl --test replay_retention --test replay_engine
cargo test -p tnl --test inspection_api --test cli_inspection --test forwarder_inspection
cargo test -p tnl-e2e --test replay_e2e
cargo test -p tnl --test forwarder_peek --test peek_request_head --test progress_format
cargo test -p tnl --test forwarder_local_no_response -- --test-threads=1
cargo test -p tnl-e2e --test inspector_e2e --test host_rewrite --test reconnect \
  --test full_roundtrip_response_framing
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

New target names above are proposed, not baseline tests. Run the visitor-IP/request-ID negotiated-envelope suites and combined WebSocket handshake coverage after their approved seams land; do not add another protocol here. Browser runtime checks need a supported real browser at local acceptance; Rust build and source scans alone do not prove XSS resistance, Fetch Metadata compatibility or modal behavior. Release-validation selects supported platform/browser runners; no downloads/network/production tests in design phase.

## 12. Risks, review gate and actual evidence

- The sibling documents are concurrently revised. Parent must reconcile shared observer names, namespace, generation semantics, token/body policy and metadata envelope in both final versions before code edits. No claim of sibling sign-off.
- Raw storage is intentionally separate but cannot duplicate framing/body buffers without accounting. Review allocation-before-admission, temporary copies, decoded entity correctness and every terminal/eviction path. Inspection's 4 KiB JSON preview cap does not bound raw retention or prove completeness.
- Existing upload-order issue is a distinct pending-approved fix, not a replay module detail. Separately spawned old forwarders and upgrade lifecycle make naive per-connection generations incorrect.
- Browser auth must remain mandatory even on loopback. A valid stolen capability plus same-origin script compromise can execute; confirmation is not defense against XSS. No raw API, strict render policy and exact authority/Origin/Fetch Metadata are release blockers, not optional hardening.
- No application exactly-once guarantee: network/process loss after sending is inherently ambiguous. Preserved idempotency keys may suppress effects or not; tnl must not regenerate them or promise safety. Full HTTP response does not prove business success.
- Fidelity excludes original external TLS/HTTP framing/header casing/transport IP/timing; the daemon has already stripped/reframed fields. Signature timestamps/nonces can expire, and original backend itself may be a proxy. Fixed loopback is not a general sandbox.
- Shared-file edits and protocol metadata must be serialized by integrator. No replay changes to yamux role mapping, channel-drop shutdown, WSS bridge, daemon routes or auth stores. Independent reviewer gate remains required before implementation acceptance.

**Actual evidence this revision:** read-only AGENTS/skill/source/sibling/previous-plan reads and targeted `rg`; `git status --short --branch`, `git rev-parse HEAD`, `git diff --cached --name-only`. Baseline/branch verified; existing untracked plan left untouched. No source edits, tests added/run, build/fmt/clippy, network, credentials, deployment, commits, staging, merging, publishing or child agents. Only the authoritative external revision artifact is written; final status/index and artifact structure are checked before returning. Proposed acceptance cases are not passing runtime evidence.
