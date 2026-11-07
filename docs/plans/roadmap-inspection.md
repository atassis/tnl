# Local browser inspection and confirmed replay — reconciled execution specification

## Status, decisions and evidence

**Design reconciliation only; no implementation is authorized by producing this document.** This full specification supersedes the inspection lane's earlier read-only-interface proposal. It reconciles the sibling replay proposal at `/home/atassis/.config/superpowers/worktrees/tnl/replay/docs/plans/roadmap-replay.md` with the subsequent user choices. It does not claim the sibling document has already been updated or that remaining numerical/privacy choices have been approved.

### Selected by the user — preserve these decisions

1. **One authenticated local browser inspector**, supporting recent history, search, details and explicit confirmed replay. One loopback service, one capability-auth boundary and one UI. No parallel Unix replay socket or standalone replay CLI in this milestone.
2. Inspection owns that shared service/auth/UI, the bounded incremental observer and the **sanitized display store**. Replay owns a **distinct opt-in raw retention module**, replay eligibility, private snapshot lookup and execution engine. Inspection must not own the replay secret store or reconstruct executable requests from sanitized views.
3. **One observer seam feeds independent bounded consumers, using one CaptureId per original request.** Capture ID is distinct from canonical request ID and from replay execution ID.
4. **Tunnel generation is stable on successful reattach and changes on recreation**, even if the subdomain is unchanged. A connection attempt counter is not a tunnel generation.
5. A **negotiated bounded envelope carries visitor identity and request ID**, owned by visitor-IP lane. Inspection consumes validated typed metadata from that lane, never re-parses envelope bytes or infers visitor identity/canonical ID from arbitrary headers.
6. Browser replay mutations live in the same authenticated service, with exact Host/Origin/Fetch-Metadata defenses, no CORS, capability auth and explicit per-action confirmation. **No raw fetch endpoint.**

Everything described as a limit, policy, flag spelling, route/schema or bootstrap mechanism below remains a precise **implementation proposal** unless included in these selected decisions. Material open questions are isolated in §11; do not reopen the selected browser/ownership/envelope decisions as if they were unchosen alternatives.

### Design-phase evidence

Worktree `/home/atassis/.config/superpowers/worktrees/tnl/inspection`, branch `feat/roadmap-inspection`, HEAD `43f2bc0ed29e00821e1ede624b8fe190eb0d5c57`. Read `AGENTS.md` and prior inspected sources/callers/tests; re-read forwarding/reconnect and the sibling replay specification for this revision. No worktree skill files were discovered in the original inspection; no new provider is supplied. This revision writes only the run's authoritative external artifact. `docs/plans/roadmap-inspection.md` was already untracked when this resumed task started and is left untouched. No staged files; no source changes, tests, credentials, network, commits or deployment.

## 1. Current verified implementation, not historical intent

References are repository-relative at the baseline above.

| Area | Verified behavior and implications |
| --- | --- |
| CLI and renderer | `crates/tnl/src/main.rs:41–74,188–225` exposes `http TARGET [SUBDOMAIN]`, Host control, quiet/verbosity/JSON; no history, browser service or replay. `crates/tnl/src/inspector.rs:9–30,40–48,86–137` defines a client-local summary `LogLine` and drains it to stdout; all non-quiet verbosity variants render the same detail. No retained headers/bodies. UTF-8 byte slicing in its truncator can panic on a multibyte path. |
| Ownership and callers | `commands/http.rs:14–30` creates a 1024-entry summary channel and renderer. `reconnect.rs:19–26,129–139` passes a sender in Hooks/ForwardCtx. `client.rs:181–203` spawns forwarding tasks for accepted data substreams. Process-scoped history/replay must live outside an individual connection. Literal context/hook callers include production reconnect and `tnl-e2e/tests/{host_rewrite,reconnect}.rs`; most forwarding tests use `ForwardCtx::new`. |
| Request head and Host | `forwarder.rs:180–211,246–266`; `peek.rs:12–13,30–78`: 8-KiB/5-second probe preserves leftover body only on a complete head; timeout loses buffered bytes. Forwarder collapses capped/EOF/timeout outcomes into a buffer rather than rejecting them. Host transformation happens after connect. Neither permissive peek metadata parsing nor a buffer cap proves replay completeness or validity. |
| Upload prerequisite | `forwarder.rs:268–305,357–423` sends only head/probe leftover, waits for backend response prefix, then starts pumping remaining upload. A backend waiting for more body can hit first-byte timeout. **Ordering verified by code inspection, not reproduced by a runtime test here.** A tee does not fix it; see separate approval-gated task P0. |
| Response, errors and logging | `forwarder.rs:305–356,357–460,464–529` probes 9 response bytes, retains ~1024 bytes for status/Host-block detection and logs after both pumps; selected local synth failures log, but early I/O `?` returns can omit logging. Long streams have no completion log until close. `try_send` can drop summary lines; its debug warning is unsafe for any new raw-bearing error. Keep history independent of this channel and preserve block-detection behavior. |
| Daemon/visitor transport | `crates/tnld/src/data_plane.rs:14–55,185–229,316–340` strips hop-by-hop/framing headers, sets `Connection: close`, uses Hyper HTTP/1 and streams response bodies. One request per data substream. No visitor upgrade-forwarding path at baseline; WSS control transport is not visitor WebSocket support. No inspection/replay API exists on daemon. New envelope negotiation is an approved cross-lane addition, not already-shipped behavior. |
| IDs and generations | `forwarder.rs:185` and `data_plane.rs:64` independently allocate request ULIDs. `tnl-protocol/src/messages.rs:50–71` has a different wire LogLine, not the client summary type. `reconnect.rs:57–83,107–110` retains tunnel ID on reattach, recreates after TunnelLost; use explicit outcome/identity to manage generation, not every successful connection. |
| Targets/config/dependencies | `target.rs`, `connect.rs` accept localhost-port/literal IP targets, including non-loopback addresses for normal forwarding. Replay must impose its own loopback-only rule. `config.rs:7–10,33–104` is endpoint/token only. Root workspace already declares axum/httparse/Hyper; client currently lacks axum/httparse. No credential-config or server-permission extension is needed for local browser replay. |

Tests inspected in the original work or sibling evidence: `tnl/tests/{forwarder_peek,peek_request_head,forwarder_local_no_response,progress_format}.rs`; `tnl-e2e/tests/{inspector_e2e,host_rewrite,reconnect,full_roundtrip_response_framing}.rs`. They cover one summary GET, Host behavior, reconnect and response framing including 103/chunking; they do not prove history, redaction, complete streamed request capture, local browser authorization or replay. None were executed for either inspection design pass.

## 2. Interface reconciliation and bounded scope

Earlier alternatives were live CLI/NDJSON only, CLI+private IPC, and loopback browser/API. **User selected the third, now including confirmed replay.** The sibling Unix-only CLI/socket recommendation and inspection's prohibition on all POST/replay routes are superseded. They must not survive as parallel interfaces in implementation tasks.

In scope: opt-in process-local browser UI, bounded searchable recent sanitized request/response records and previews, typed identity metadata, incomplete/error/upgrade lifecycle, distinct opt-in raw request retention, explicit preview/confirmation, one-at-a-time direct-backend replay and linked safe outcomes. Inspector remains useful when replay retention is off. Replay must remain unavailable for earlier/redacted-only/incomplete captures.

Non-goals: public/daemon inspection URLs, durable history, raw download/export, disk imports, multi-tunnel aggregation, arbitrary request editing/destination selection, automatic/batch/scheduled replay, full plaintext/HTML/form/hex preview, decompression, HTTP/2 observer, WebSocket frames/session replay, additional CLI/socket transport, analytics/telemetry and deployment. Visitor-IP owns envelope semantics; WebSockets owns upgrade support; release-validation owns eventual combined deployment/test shot.

## 3. Proposed CLI, defaults and service lifetime

```text
tnl http TARGET [SUBDOMAIN] [existing flags]
    --inspect
    [--inspect-port PORT]
    [--inspect-bodies off|json]
    [--inspect-redact-header NAME ...]
    [--inspect-redact-json-key NAME ...]
    [--replay-retain-secrets]
```

- All new flags except `--inspect` require `--inspect` (usage exit 2). Nothing creates a second listener. `--inspect` defaults false; raw replay retention defaults false and is **not** implied by inspect, verbosity, JSON output or display-body preview. `--replay-retain-secrets` warns about unredacted URLs/headers/cookies/signatures/bodies in memory. No runtime opt-in switch that pretends earlier captures can be recovered.
- `--inspect-port`: 0–65535, default 0. Bind only `127.0.0.1`; no bind-host override, wildcard fallback, hostname lookup or remote address. Proposed IPv4-only inspector does not restrict replay backend IPv6 loopback support.
- `--inspect-bodies off|json`, default off, controls sanitized previews only. Raw binary/compressed body retention is a separate replay policy and remains possible under explicit raw opt-in when display previews are omitted.
- Header redaction names use valid ASCII header tokens; JSON keys use 1–128 printable ASCII characters; repeatable additive case-insensitive matching, invalid/empty values fail usage. No regex/selectors. No new TOML keys or `TNL_*` variables; existing endpoint/account-token resolution remains unchanged.
- Process-scoped service, display store, optional replay module and generation authority are created in `commands/http` scope, not on every reconnect. Bind/credential/setup failure exits clearly before remote connect; no false success banner. No automatic browser launch.
- Proposed independent 256-bit random bearer printed once to stderr as `Inspector: http://127.0.0.1:<port>/#token=<capability>`, even under quiet/JSON. It is not the account bearer. Bootstrap delivery/privacy still needs approval (§11). No credentials in stdout/API models/tracing/process arguments.
- Browser removes fragment immediately using `history.replaceState`, retains bearer only in JS memory, never local/session storage or cookies. Refresh without fragment asks to reopen startup link. Warn not to share/save stderr/link. Reuse a supported cryptographic random API after checking its actual dependency support; no ULID/timestamp credential.
- Quiet only suppresses summary rendering, not enabled history. Existing stdout LogLine schema gains no bodies/headers. With inspection enabled, sanitized target also feeds its summary/debug path fields; fix UTF-8 slicing and escape terminal controls for inspected values. Default no-inspection behavior remains compatible except an explicitly reviewed panic fix.
- On shared service fatal failure: atomically revoke new replay authorization, stop capture consumers, clear stores, cancel any started replay conservatively and warn once without secrets; ordinary forwarding continues. Request/client disconnect is not service failure. Normal process shutdown closes listener and clears stores; no payload files or claim of cryptographic zeroization/swap/core-dump protection.

## 4. One observer seam, independent consumers and typed identity

### Shared types and ownership (proposed client-only contract)

One inspection-owned shared types module (`inspection/observe.rs` or `inspection/model.rs`) defines `CaptureId` (ULID newtype), `TunnelGeneration` (ULID newtype) and the observation event contract. These are not extensions of either existing LogLine. Replay imports these shared types; it does not mint another capture ID. Envelope types are imported from the visitor-IP-owned protocol metadata module; final concrete naming/version limits belong to that lane.

```text
CaptureContext {
  capture_id: CaptureId,
  generation: TunnelGeneration,
  tunnel_id: existing tunnel ULID,
  origin: External,
  metadata: ValidatedRequestMetadata  // decoded/validated envelope result
}

Observation, in-order for one capture:
  Begin(CaptureContext)
  EffectiveRequestHead(borrowed validated HTTP head, intended effective Host)
  RequestBody(borrowed transfer-decoded, non-content-decoded bytes)
  RequestEnd { framing_valid, complete, has_trailers, was_upgrade }
  ResponseHead(borrowed validated head, informational/final classification, source)
  ResponseBody(borrowed transfer-decoded, non-content-decoded bytes)
  UpgradeConfirmed
  End { terminal_kind, successful_transport_byte_counts, duration }
  ObservationStopped { reason } // parser/cap/cancellation, never pretend complete
```

This is an ownership/lifetime contract, not an instruction to allocate an owned event for every fragment. Synchronous borrowed callbacks return before relay buffers are reused; consumers retain only their independently bounded copies. No unbounded event bus/channel or awaiting consumers on the live path. Begin may carry only bounded typed metadata. Consumers' admission/retention failures are local; display stopping must not imply raw stopping or vice versa. Shared parser invalidity correctly prevents raw eligibility while still allowing partial sanitized display.

- Inspection owns **one incremental framing observer** and its tap in the forwarder; replay does not add another tee or interpret summary JSON. Feed head/body leftover/prefix exactly once at the intended post-Host-transformation backend boundary. Feed intended write fragments before buffer reuse; completion means the full intended request was observed/validated, **not** proof it reached or succeeded at the backend. Transport byte counters separately count successful writes. If connect fails before an effective backend request can be constructed, display can show sanitized pre-connect input with unavailable effective request, but replay is ineligible.
- Display consumer sanitizes immediately into the display store. Replay consumer, only under explicit opt-in, privately retains full request bytes/head metadata under replay limits. It ignores response payload retention. Display scratch is not the raw store; raw lookup never goes through display-record deserialization.
- Borrowed raw head/body values and envelope metadata are private in-process inputs, never Debug/Serialize API events, logs or queue errors. Final raw snapshot access exists **inside replay's module only**; server/UI get safe eligibility/preview/result types from a replay facade.
- Shared observer admission is at most 64 concurrent captures before allocating capture scratch. Either consumer may decline. If display record later evicts, it does not resurrect just because raw exists; browser selection returns 404. Raw eviction merely makes the visible record ineligible. Replay owns raw lifecycle independently and may retain an inaccessible orphan until its own bounded expiry/eviction; it cannot keep display rows alive or create raw-only list/detail endpoints.

### Envelope metadata boundary

Visitor-IP lane owns negotiation, byte length/version caps, dispatch before HTTP parsing, validation, trust/provenance, canonical visitor identity and canonical request ID, plus old-peer behavior. Inspection receives `ValidatedRequestMetadata`, containing the lane's typed optional canonical request ID and typed visitor identity/provenance (or a typed negotiated-legacy/unavailable result). It must not parse envelope bytes, trust inbound `X-Forwarded-*`/request-ID headers instead, or skip arbitrary bytes by guessing whether they are envelope/HTTP.

- `capture_id` is always local and unique regardless of missing canonical request ID.
- Display `request_id` is sourced solely from validated typed metadata. Header redaction does not turn an arbitrary header into canonical correlation.
- Proposed conservative display of visitor metadata: `{available:bool, source:string, display_ip:string|null}`; `display_ip` is `[REDACTED]` when present, otherwise null. `source` is a safe bounded label mapped from the visitor lane's validated provenance, not an arbitrary supplied string. No raw IP search/storage in the sanitized view and no new show-IP flag in this slice. User approval may change this privacy choice before implementation.
- Replay does not re-emit/replay the transport envelope or inject a synthetic visitor identity. Original application-level visitor headers, if any, are handled under replay's independently reviewed header policy; direct local replay is not a new external visitor. Request ID is correlation, not capture selection or authorization.

### Framing and lifecycle

Use bounded incremental HTTP/1 parsing with workspace `httparse` for heads and explicit body framing states: arbitrary fragmentation, duplicate fields, Content-Length, chunk decoding, close-delimited responses, HEAD/204/304, informational responses (maximum 8 and 32 KiB cumulative response head input). Head scratch is 8 KiB per direction. No trailers retained for display; any request trailers flag raw replay ineligible. Reject ambiguous TE/CL/conflicting lengths for observation, not by changing proxy traffic. Unsupported coding/malformed/incomplete/capped heads/chunks stop body observation and record a safe reason; never invent completeness. No content decompression or de/re-encoding for raw request bytes.

Display is visible before completion; progress at most once/second plus head/terminal transitions. Lifecycle guard issues one terminal result even on early `?`/cancellation. On confirmed 101, publish sanitized handshake immediately, mark upgraded, stop HTTP body/framing observation and raw retention, and never parse frame bytes as HTTP. WebSockets lane owns actual relay and cancellation signals; no requirement to wait for socket close to expose the handshake.

Generation authority lives with reconnect/session ownership: first successful CreateTunnel allocates a generation; successful ReattachTunnel retains it; successful fresh CreateTunnel after loss/recreation allocates a new one before accepting requests. Generation is not derived from subdomain or connection success count. Browsing old records remains allowed until expiry. Replay snapshot/preview/execution must use current active generation; disconnect makes new execution unavailable until reattach. Recreation atomically revokes prior-generation confirmations and eligibility. Already-started replay cancellation uses conservative delivery semantics; it never silently changes destination/generation. Do not infer that every old forwarding task ended merely because accept loop ended; coordinate actual cancellation/upgrade ownership with WebSockets/transport owner.

## 5. Sanitized display schema, redaction, search and bounds

### API record (proposed schema v1)

`InspectionRecord` contains:

- `schema_version:1`, `capture_id:string`, `generation:string` (stable tunnel generation), `request_id:string|null`, `tunnel:string`, `sequence:u64`, `origin:"external"`.
- `started_at_ms:u64`, `duration_ms:u64`, `state:"active"|"complete"|"failed"|"disconnected"|"upgraded"|"incomplete"`, `failure_kind:string|null`, `visitor` as above.
- `method:string`, `request_target:string` (sanitized), `display_target:string`, `resolved_addr:string|null`, `status:u16|null`, `response_source:"backend"|"client_synth"|"unavailable"`, `request_boundary:"effective_backend"|"preconnect_unavailable"`.
- `request` and `response`: `{headers:[{name,value,redacted:bool}], headers_state:"complete"|"capped"|"malformed"|"incomplete"|"unavailable", body:BodyPreview}`. Preserve bounded duplicate fields/input order, lowercase display names only.
- `BodyPreview`: `{mode:"off"|"json", state:"disabled"|"pending"|"available"|"empty"|"omitted", text:string|null, reason:string|null, observed_payload_bytes:u64, retained_bytes:u32, redacted:bool}`. Reasons: `too-large`, `unsupported-media-type`, `encoded`, `streaming`, `invalid-json`, `incomplete`, `invalid-framing`, `upgraded`, `capture-limit`, or null. `text` is compact sanitized JSON, never a raw prefix.
- `bytes_to_backend:u64`, `bytes_from_backend:u64` successfully forwarded including HTTP framing, not browser-delivered or decoded payload counts; `observation_incomplete:bool`, `omission_reasons:string[]` of bounded catalog codes.
- `replay:{eligible:bool, reason:string|null}` obtained dynamically from **replay's facade**, never inferred from body-preview state, original response status or display completeness. Also expose at most the most recent linked replay outcome summary; replay outcome storage is separately bounded below. No raw handle/value/body accessor in this model.

Raw store missing/off/expired can make replay unavailable even when preview is available. Conversely a binary request can be replay-eligible while its display preview is omitted. Original daemon-only errors before data-substream creation have no fabricated local capture.

### Redaction (proposed conservative policy)

- Show clear header values only for `accept`, `accept-encoding`, `accept-language`, `content-type`, `content-length`, `transfer-encoding`, `connection`, `host`, `cache-control`, `pragma`, `expires`, `date`, `vary`, `allow`, `retry-after`, `x-tnl-component`, `x-tnl-origin-failure`, `x-tnl-request-id`. All other values are `[REDACTED]`, including Authorization/Cookie/Set-Cookie/Location/Referer/IP/WebSocket credentials. Additional configured names override the allowlist. Canonical metadata remains separate.
- Retain bounded query key names, but **all query values** become `[REDACTED]`, including duplicates/unknown/empty values. Invalid percent encoding, absolute-target userinfo or unparseable URI displays `[unavailable]`, never raw fallback. Do not change forwarded URI. Arbitrary path segments and allowed values can contain secrets: explicit limitation, not DLP guarantee.
- JSON preview only with opt-in, complete entity ≤4096 bytes, valid UTF-8/JSON, depth ≤32, `application/json` or `+json`, no non-identity Content-Encoding. Recursively replace whole values for case-insensitive `password`, `passwd`, `secret`, `token`, `access_token`, `refresh_token`, `id_token`, `api_key`, `apikey`, `authorization`, `cookie`, `set-cookie` and configured names; traverse arrays. Sanitized output also ≤4096 bytes or omit. Partial JSON never exposed; key/value split across reads cannot leak a prefix.
- Binary/text/HTML/forms/multipart/compressed/SSE previews omitted with reason; no hex/base64/download fallback. Finite chunked JSON may preview only when completely decoded and otherwise eligible. Endless bodies stop retention at cap and remain omitted/incomplete. Replay raw entity bytes are independent and never JSON-normalized/decompressed.
- Sanitize before display store/search/API/terminal output; no raw parse/error input in logs or derived Debug. Capture secret buffers may exist only inside bounded parser scratch and replay-owned opt-in retention, not hidden in display models. Heap zeroization/swap/core dumps are outside guarantees.

### Proposed fixed resource limits

| Resource | Limit/behavior |
| --- | --- |
| Display history | 200 original request rows; 16 MiB accounted allocated capacities; 15-minute TTL from admission. Oldest admission sequence evicts first, including active rows. No reinsertion after eviction. Enforce on reads/mutations and periodic sweep. |
| Shared observer | 64 active admissions, nonblocking permit before scratch allocation; excess forwarding continues with drop counter. Scratch ≤32 KiB/capture: 8 KiB head/direction + 4 KiB display-body/direction + 8 KiB metadata/state. Parser streaming retains no hidden body-sized buffer. Detached consumers release their scratch; parser may remain for other consumer. |
| Display fields | 64 header fields/direction, 1 KiB/value, 8 KiB retained header text/direction; target ≤2 KiB, method ≤32 bytes. Character-safe truncation only after sanitation. Full JSON record ≤64 KiB including escaping; omit body then excess headers with explicit states rather than emitting broken JSON. |
| Replay raw retention — replay owned | Proposed sibling limits: 20 captures, 16 MiB header/body allocated capacity including pending reservations/pinned snapshots; 10-minute TTL; head ≤8 KiB, entity ≤1 MiB. No raw retention without opt-in, no partial eligibility, no spill. Executing snapshot stays charged to same budget; deny new retention rather than exceed it. |
| Browser service | HTTP/1 only; at most 8 admitted TCP connections **before HTTP parsing/task creation**, closing excess immediately; at most 4 concurrent HTTP request handlers, no unbounded queue. 32-KiB HTTP head, 3-second header/body read and idle deadlines; replay JSON body ≤4 KiB, no other request bodies. List ≤100 summaries, detail/preview/outcome ≤64 KiB, error ≤1 KiB. Serialize outside store locks; ≤1 MiB aggregate API snapshot/serialization working budget. Slow writes timeout after 3 seconds; accepted replay engine proceeds independently. These caps must be enforced in the listener/Hyper driver, not only post-parse route middleware. |
| Confirmation tickets | 32 entries, ≤1 KiB each, 60-second TTL, one-use cryptographic nonce bound to capture/generation/options. No raw content. Used-ticket tombstones may share this bound until TTL; eviction never makes an old nonce valid again. |
| Replay operation/outcome | One non-waiting engine permit; no execution queue. Replay-owned bounded safe outcomes: 100 entries/1 MiB/15 minutes, including pending operation. Display facade reads outcomes, never response bodies. |

Counters saturate u64. Critical sections are bounded synchronous metadata operations with no await/network I/O/serialization/printing; do not use the lossy summary channel as capture transport. Normal forwarding must not await UI consumers. Inspection payload ≤19 MiB (history/scratch/API); replay adds ≤16 MiB charged raw, ≤1 MiB outcomes and bounded streaming-engine scratch (proposed ≤1 MiB); tickets add ≤32 KiB. This is **not** a total-RSS or whole-tunnel-concurrency bound. All allocation capacities/copies/depth/serialized expansion must be accounted, not just string lengths. Hyper/parser internals require validation against these hard caps before accepting the implementation.

## 6. One local authenticated HTTP/UI contract

### Auth, browser defenses and rendering

One independent random capability authorizes all `/api/v1/*`, including eligibility/stats/not-found and replay. Static `GET /` is nonsensitive and need not embed/authenticate secret data. No cookie auth, account-token reuse, query token, basic auth or raw endpoints.

- Check peer loopback and **exact Host/authority `127.0.0.1:<actual-port>`** on every route; reject hostname aliases, localhost, attacker domains/wrong ports/ambiguous duplicate Host; never trust forwarded-host. No bind fallback.
- If Origin exists on reads/root it must exactly match `http://127.0.0.1:<port>`; reject null/foreign origins. Reject cross-site/same-site-not-same-origin Fetch Metadata. Read requests without Origin/Fetch Metadata may use capability auth (e.g. local tools); root direct navigation may have Site none. No CORS, including preflight/error responses.
- **Replay POST routes require** bearer plus exact Origin and `Sec-Fetch-Site: same-origin`, `Sec-Fetch-Dest: empty`, `Sec-Fetch-Mode: cors` or `same-origin`. Missing/mismatched mutation context fails 403, even with capability; body must be application/json, no ambient/simple-form mutation. These headers are browser defenses, not protection against a local malicious process able to forge them.
- Authentication precedes revealing API route/resource existence; authority/origin rejection can occur first without resource disclosure. OPTIONS/CORS preflight unsupported; no permissive Access-Control-Allow-* headers. Every route except the explicitly listed POSTs is read-only.
- `Cache-Control: no-store`, JSON MIME for API, `Referrer-Policy: no-referrer`, `X-Content-Type-Options: nosniff`; CSP `default-src 'none'`, hashed script/style assets, `connect-src 'self'`, `base-uri 'none'`, `form-action 'none'`, `frame-ancestors 'none'`. No blanket unsafe-inline/eval. No external assets, storage, service worker, telemetry or automatic browser launch.
- Render captured values exclusively using text nodes/textContent, never innerHTML/executable links. No token/confirmation nonce in page text/console/logs/URL query; no middleware recording Authorization/request JSON. Same-user malware, extensions and local memory access remain outside protection. An inspector XSS is a replay-authorization compromise: mandatory security review.

### Read routes

| Route | Contract |
| --- | --- |
| `GET /` | Tiny embedded UI: search/filter table, request/response detail and omission states; replay eligibility/disabled reasons, explicit preview/confirm action and linked outcome panel. Poll every 2 seconds while visible, no websocket/SSE endpoint. |
| `GET /api/v1/requests?q=&method=&status=&state=&before=&limit=` | `{schema_version:1,items:[Summary],next_before:u64|null,stats:Stats}`; newest admission sequence first. Summary has IDs/generation/time/method/sanitized target/status/state/duration/failure/replay eligibility, not headers/bodies. Default limit 50, max 100; exclusive sequence cursor. |
| `GET /api/v1/requests/{capture_id}` | Full sanitized InspectionRecord, dynamic safe replay availability. Missing/evicted/expired display row =>404 even if replay module privately still has raw. |
| `GET /api/v1/stats` | Bounded counts: display records/bytes, active observers, dropped/evicted/stopped totals; replay retention enabled and numeric raw-budget usage through safe replay facade, never raw data/keys. |
| `GET /api/v1/replays/{replay_id}` | Replay-owned safe operation/outcome: replay ID, parent capture ID, generation, state pending/complete/failed, nullable final HTTP status, duration/body byte counts, response_complete, delivery and safe error code. No raw request/response. 404 on expired/unknown result. |

`q`: case-insensitive literal substring, ≤256 UTF-8 bytes, ANDed with filters, only across retained sanitized method/target/IDs/status/failure/header names+values/available JSON; never raw store or masked visitor IP. `method` ASCII token ≤32 bytes; `status` exact100–599; `state` enum; `before` u64; `limit`1–100. Query ≤2 KiB; unknown/duplicate/invalid UTF-8 parameters rejected400. No regex or disk index. Pagination not a stable snapshot under new traffic/eviction. Bounded search across ≤200 rows; UI restarts page1 on filter change.

### Replay preview and execution routes (new reconciled proposal)

The shared server owns transport validation, tickets and UI; delegates raw lookup/eligibility/options validation/execution to replay facade. Original capture must still have a display row when preparing/executing; never synthesize one from raw.

1. **`POST /api/v1/requests/{capture_id}/replay-preview`** body `{generation:string,host_header:null|string}`. Strict schema/4-KiB cap; no unknown fields. `host_header` is proposed optional preserve/rewrite/fixed override; absence means captured effective Host. Replay validates generation/eligibility/options with **zero backend connections**, returns sanitized ReplaySummary only. On eligibility success server adds `{confirmation_token:string,expires_at_ms:u64}`; random nonce bound to capture ID, current generation and normalized options. Invalid/ineligible preview issues no ticket. Issuing a ticket is not backend permission by itself.
2. UI shows a separate confirmation panel with method, **sanitized** target, fixed loopback backend, body size, Host mode (not secret raw header values), warnings about non-idempotent effects, preserved credentials/signatures and uncertain delivery. Require a new explicit unchecked acknowledgment and a deliberate **Confirm replay once** click for **every method including GET**. Cancel does not execute. No auto-confirm from selecting a row, search, preview, page refresh or previous execution. Clear acknowledgment on any capture/generation/options change.
3. **`POST /api/v1/requests/{capture_id}/replays`** body `{generation:string,host_header:null|string,confirmation_token:string,confirm_side_effects:true}`. Service requires matching unexpired unused ticket and current normalized options; missing/false confirmation rejected. Replay atomically revalidates raw eligibility/current active generation and obtains one non-waiting execution permit; no queued execution. On successful acceptance, consume ticket once and return202 `{schema_version:1,replay_id:string,parent_capture_id:string,generation:string,state:"pending"}`. No backend call is made on auth/confirmation/capture/eligibility/permit failure. Generation/ticket acceptance and engine-start gating must be serialized to prevent a stale-generation start.
4. The bounded replay job/outcome is replay-owned and persists independently of this HTTP handler; poll the GET result route. HTTP/browser disconnect after acceptance does not retry/cancel by inference; job finishes once within engine bounds and records result. After uncertain HTTP delivery the UI fetches safe outcome if it has the ID; it never automatically resends. A repeated used ticket cannot execute twice (409 `confirmation_used`; may return already associated safe replay ID). If ticket/tombstone expired/evicted return `confirmation_invalid`, **not** a fresh execution. Manual repeat requires new preview and new explicit acknowledgment.
5. Recreation, shutdown or inspector failure revokes tickets. Eligibility also rechecks on execute, not just preview; raw expiry/eviction/pending state cannot be bypassed with a ticket. No raw snapshot request/response fields exposed at any route, even with the local bearer.

Common errors: bounded `{error:{code:string,message:string}}` with fixed safe message, optional associated replay ID only when already accepted. HTTP400 invalid input,401 auth,403 authority/origin/fetch context,404 missing display/resource,409 ineligible/wrong_generation/confirmation_required/confirmation_invalid/confirmation_used/busy,503 service unavailable/overload. Runtime replay failures are safe result fields, not a leak of arbitrary Hyper/backend exceptions. Unknown mutation routes405/404 after auth; no PUT/PATCH/DELETE/raw-fetch/export/target-change route.

## 7. Replay-owned module contract and execution constraints

This section reconciles ownership, not transfers replay implementation into inspection. Proposed replay facade exports safe `availability(capture_id,generation)`, `preview(capture_id,generation,options)`, `try_start(capture_id,generation,options) -> ReplayId`, `outcome(replay_id)`, lifecycle revocation and numeric budget stats. Private `ReplaySnapshot` lookup/retention/eligibility/request building remain inside replay. Neither inspector server nor display store can deserialize/construct a raw snapshot.

Raw snapshot includes same capture ID/generation, complete origin-form target/query, validated method, effective post-transformation headers/Host (including opaque duplicate values), full transfer-decoded **non-content-decoded binary entity bytes**, completion/framing/trailer/upgrade flags and trusted External provenance. Snapshot is immutable, not derived Debug/Serialize, and private to replay. A display-body cap does not truncate it. Late opt-in cannot restore earlier material; complete empty body differs from not retained.

Preserve these conservative sibling engine proposals pending numerical/policy approval:

- Eligibility only for complete, valid, unexpired, External, current-active-generation raw request under head/body limits, with one valid Host, no ambiguity/trailers/upgrades. GET/HEAD/POST/PUT/PATCH/DELETE/OPTIONS only; reject CONNECT/TRACE/custom methods and non-origin-form target. Inspection can display more than replay supports. Original response failure/4xx/5xx does not alone prohibit a fully captured request.
- Destination fixed from original running tunnel target and generation, never Host/captured absolute URI/envelope/browser input. Loopback-only replay including reviewed IPv4-mapped IPv6 handling; for localhost validate **all resolved candidates** are loopback, connect validated literals without a second resolution. Reject LAN/public/unspecified/multicast and known inspector listener target. Normal forwarding's broader target support remains unchanged.
- Default preserve effective captured Host; proposed explicit preserve/rewrite/validated fixed override only, no adaptive Host auto-retry. Validate before connect; Host changes routing header only, not destination; no authority/userinfo/CRLF injection. No backend target editor. Optional Host override scope remains a material approval question.
- Existing Hyper HTTP/1 on manually connected Tokio socket, full retained body, driver concurrent with upload/response. Strip hop-by-hop/Connection-nominated/framing/Expect/Upgrade fields, recompute framing, fresh connection and `Connection: close`. Preserve permitted application auth/cookie/signature/idempotency headers **only from opted-in raw bytes**, not account config. No ambient proxies/cookie jars/redirects/retries; no envelope replay or invented visitor header trust.
- Proposed deadlines: 3-second total connect,15-second first-response,30-second operation. Final response parsing via Hyper; response head cap32 KiB, entity drain cap1 MiB with bounded discard buffers, never retain/display raw response. A completed 4xx/5xx is a valid outcome, not retry signal. Response cap/timeout/malformed/incomplete status is explicit failure with known partial status if any.
- Safe outcome delivery is `not_sent|possibly_sent|response_received`. Once write may have begun, never say safe to retry; full response receipt is not proof side effects succeeded. One execute permit, no automatic retry after browser/API disconnect. Revocation cancels safely and records uncertainty rather than silently restarting.
- Replay uses a **new ReplayId as execution identity linked to the original CaptureId**, not a second raw-capture ID. It calls backend directly, never public tunnel/daemon/yamux accept loop. Outcome is not a new executable original request; no replay-of-replay control. Do not trust inbound replay-suppression headers or create a second raw snapshot from the replay attempt. Tnl cannot prevent arbitrary callbacks implemented by the backend itself.

The sibling's raw-store ownership in inspection, UnixListener/control framing, `tnl replay --socket/--yes`, Unix-only platform restriction and filesystem socket cleanup tasks are **removed**, not optional secondary implementations. Shared browser confirmation replaces --yes. Engine/raw retention policies otherwise stay replay-owned; its owner must update that spec to the same seam before code starts.

## 8. Separate approval-gated prerequisite P0: streaming-upload sequencing

**Requires explicit scope approval before implementation.** Bounded task, not an incidental observer/parser refactor; assign one coordinated owner with WebSockets lane.

Owned hotspot: `crates/tnl/src/forwarder.rs` and focused tests (`tests/forwarder_upload_streaming.rs` proposed). Once request head is connected/transformed/written, drive remaining request upload concurrently with backend response probing/relay. Do not gate upload on backend producing nine response-prefix bytes. Preserve existing connect/no-response/malformed/EOF synth attribution where no backend response has been sent; never splice a second synthetic response after response bytes begin. Preserve half-close behavior, Host auto detection, transport channel-drop shutdown and cancellation. Define first-response timer semantics explicitly with approval (proposed retain existing 15-second response-start deadline while upload progresses; no blanket infinite upload grace).

Tests before/with repair: backend reads >initial leftover before replying; finite chunked and slow multi-write uploads; backend responds early while upload continues; client upload half-close; backend write/early-close failures; no-response timer; cancellation during simultaneous upload/probe; malformed response with upload in flight; existing response framing/Host/synth regressions. Compare inspect/raw-off/on to show observers never introduce the sequencing dependency. Inject test limits rather than add more global timeout overrides.

No general HTTP proxy replacement, header policy changes, HTTP/2 support, visitor WebSocket implementation or unbounded prebuffering under P0. If needed timeout/synthesis changes exceed these boundaries, return for a scope decision. Large/streaming inspection+replay acceptance remains blocked until approved P0 lands and its tests pass; no claim that the design artifact fixed it.

## 9. Dependency order, owned files and implementation subtasks

Execute only after §11 approvals and independent design review. Integrator serializes shared-file changes; no new parallel context types or independent tees.

1. **Freeze inter-lane contracts.** Visitor-IP publishes negotiated bounded envelope/typed metadata and mixed-version policy; request-ids agrees canonical ID semantics through it. Freeze one CaptureId/TunnelGeneration/event contract and replay safe facade. Assign P0 owner/scope. Replay updates its sibling spec to remove Unix/control/raw-store-inspection ownership. Review browser mutation security with access-controls lane; no new daemon scope is implied.
2. **P0, separately authorized.** Implement focused sequencing repair and tests in §8 before declaring complete streamed request capture. WebSockets owner reconciles same pump/cancellation edits rather than independently replacing them.
3. **Pure inspection model/policies/history.** New `crates/tnl/src/inspection/{mod.rs,model.rs,redact.rs,history.rs}`; export in `lib.rs`. Implement limits, sanitized-only store/search, fake-clock expiry, IDs/generation view and bounded outcome projection; inline unit tests prove raw secrets absent. No raw snapshot type in display module.
4. **Shared incremental observation.** Inspection owns `inspection/observe.rs`, optional hook/tap edits to `forwarder.rs`, coordinated `reconnect.rs`/`client.rs`/`commands/http.rs` lifecycle and literal caller updates. Add `httparse.workspace = true`; avoid changing old probe behavior or block-detection buffer without reviewed equivalence. Feed borrowed validated events to display and optional replay retention; no new envelope parsing. Tests `tests/forwarder_inspection.rs` plus exhaustive split/cap/parser unit cases.
5. **Replay raw consumer and engine, replay owned.** Proposed `crates/tnl/src/replay/{mod.rs,retain.rs,eligibility.rs,engine.rs}` or agreed equivalent namespace. Raw allocated/pinned budgeting, private snapshots, current-generation validation, direct loopback Hyper execution, one job permit, bounded safe outcomes. Tests `tests/replay_{retention,engine}.rs`. No `replay_control.rs`, Unix socket or standalone command. Replay alone owns any new http-body-util/Hyper dependency requirements; root/member versions follow workspace conventions.
6. **Shared authenticated service.** Inspection owns `inspection/server.rs` and `inspection/confirm.rs` (bounded non-raw tickets), actual-port auth/Origin/Fetch-Metadata policy, resource caps/read routes, POST preview/start adapters and safe outcomes route. Client axum/random dependencies only after checking supported APIs. Tests `tests/inspection_api.rs` and `tests/inspection_replay_api.rs` use a stub replay facade to prove no engine invocation on rejected mutations, then real bounded integration fixtures.
7. **One browser UI and CLI wiring.** Inspection owns `inspection/ui.html`, `main.rs`, coordinated `commands/http.rs`, limited stdout renderer safety fixes in `inspector.rs`. One detail/confirmation/outcome workflow, no raw values or executable captured HTML. Tests `tests/cli_inspection.rs`, UI static/DOM checks; raw flag never inferred from preview setting. Update literal `Hooks`/`ForwardCtx` callers in `tnl-e2e/tests/{reconnect,host_rewrite}.rs` once.
8. **Combined e2e/docs/review.** Inspection history fixtures in `tnl-e2e/tests/inspection_history.rs` or expanded `inspector_e2e.rs`; replay owner adds `replay_e2e.rs`; visitor-envelope negotiation and WebSocket handshake fixtures come from respective owners. Document CLI/limits/privacy/confirmation/delivery uncertainty/generation and non-replayability of views in README/RUNBOOK/CHANGELOG. Run targeted tests/workspace gates at implementation time; independent security/acceptance reviewer required. Release-validation alone owns combined deployment shot after reconciliation, not individual lane deployments.

Ownership boundaries: inspection new namespace, observer tap, loopback/auth/UI/sanitized types; replay new raw/engine namespace and private snapshot; visitor-IP envelope/control negotiation/protocol encoding; request-ids canonical correlation rules; WebSockets upgrade relay/lifecycle. Shared hotspots are `forwarder.rs`, `reconnect.rs`, `client.rs`, `main.rs`, `commands/http.rs`, `lib.rs`, root/member Cargo manifests/lockfile and user docs. Inspection does not independently edit daemon/protocol/envelope implementation. Selected envelope addition supersedes earlier plans' blanket “no protocol changes”; it is a visitor-IP-owned dependency, not license for inspection/replay to invent wire framing.

## 10. Acceptance cases and proposed commands

All following tests are **future tests, not executed evidence**. Use synthetic credentials/payloads and loopback fixtures only.

1. **Off/defaults/compatibility:** no inspect => no listener/display/raw allocation; inspect alone => authenticated browser/history, no raw retention; JSON-preview opt-in alone still no raw. Raw opt-in requires inspect; options invalid before remote work. Bind failure explicit, only IPv4 loopback listener, no Unix replay socket/second service. Existing summary schema/Host/synth unaffected; quiet still captures, no capability in stdout.
2. **One ID/two views:** signed binary/compressed POST with repeated/escaped query and header values reaches original backend intact; display masked/omitted, same CaptureId used by raw consumer. Explicit replay yields original entity bytes, signature/auth/idempotency values, not sanitized strings. Complete display JSON without raw is ineligible; complete raw binary with omitted preview can be eligible. No raw fetch route, no raw Debug/JSON/error output, no snapshot conversion from display models.
3. **Typed negotiated identity:** visitor-IP fixture envelope negotiated/decoded before HTTP; expected canonical request ID/visitor provenance reaches typed Begin event. Raw metadata envelope bytes never become request header/body or replay input. Spoofed HTTP forwarding/request-ID headers cannot replace typed metadata. Legacy/malformed/oversize/version cases obey envelope owner's explicit policy, never heuristic inspection parsing. Display visitor IP masked under proposed default.
4. **Generation:** reattach retains generation, CaptureId remains one ID, browser/history/raw lookup remains valid subject to TTL. Recreation with same hostname changes generation; existing confirmations/raw captures cannot execute, new captures use new generation. Preview/start race with recreation does not connect under stale generation; in-flight revocation records uncertainty rather than retarget/retry. Disconnect/reconnect distinguishes availability from permanent recreation; no false shutdown of healthy old streams by inference.
5. **Framing/streaming/P0:** body >peek leftover with backend waiting for completion succeeds after P0; finite chunked upload split at every boundary; early backend response concurrent upload; 103 then final200; HEAD/204/304; request leftovers/response prefix not duplicated. Conflicting lengths/TE/CL, invalid chunks, trailers/unsupported coding/capped heads never become eligible raw or malformed display fallback; ordinary relay behavior unchanged by observer. Streaming response visible before completion.
6. **Privacy/body limits:** 4096 vs4097-byte display JSON, serialized-redaction expansion/depth, incomplete/invalid UTF-8, secret key split across boundary => no partial secret preview. Binary/multipart/text/HTML/gzip/SSE omitted with reason; raw can retain eligible binary/compressed entity only under own cap. Raw1-MiB boundary, pending oversized/infinite body, TTL/cap release; no missing body interpreted as empty. Canary secrets absent from sanitized history/search/API/debug/inspection stdout/stderr; path/allowed-field arbitrary-secret limitation remains explicit.
7. **Local service security:** missing/wrong auth cannot enumerate reads/preview/outcomes or mutate; reject wrong/duplicate Host, localhost/foreign domain/port, null/foreign Origin, cross-site metadata and absent mutation Origin/Sec-Fetch context even with valid bearer. Reject simple forms, unsupported content type, oversized/slow JSON, unknown fields, preflight and query tokens. No CORS/cache/referrer/auth logs. Captured script/HTML/OSC/multibyte text cannot execute/panic/manipulate terminal; confirmation UI is not populated with innerHTML.
8. **Confirmation and races:** preview/dry-run has zero backend connections; every method requires explicit acknowledgment and one-time ticket. Wrong capture/generation/options/expired/missing/false confirmation => zero attempts. Double click/concurrent same-ticket POST creates at most one execution; second returns used/busy without queue. Raw eviction between preview/start denies. Changing options invalidates acknowledgment. Search/detail/navigation/poll/page refresh never execute. HTTP read failure/disconnect after accepted202 never retries; UI uses GET result, new manual execution requires new preview/confirm.
9. **Replay engine effects:** fixed validated original loopback backend, all-candidate localhost checks/no second resolution; nonloopback/inspector target/absolute URI/CONNECT/TRACE/upgrade rejected before connect. No redirects/retries/ambient proxies/account-token injection. Host modes validated, effective captured Host stable despite live Auto changes. Backend counter increments once per explicit accepted action. 4xx/5xx complete responses are valid outcomes; timeout/truncation/malformed/response cap terminates under deadline, records possibly_sent when appropriate, never reports safe-to-retry. Replay bypasses daemon request counter and is not executable replay-origin capture.
10. **Failures/lifecycle/upgrades:** connect refusal, probe/head/read/write/cancellation errors yield one truthful admitted terminal display record; no daemon-only record fabricated. Replayed request completeness independent of original response success. Browser disconnect never kills live forwarding; inspector fatal failure revokes/cancels once without killing normal forwarding. WebSockets combined lane: sanitized101 appears immediately, no frame canary in raw/display/search, no continued HTTP parsing/retention while socket stays open; lifecycle permits released correctly.
11. **Resource bounds:** >200 history entries, 65 active observers, >20 raw/pending entries, aggregate capacity/pinned snapshot caps, TTL expiry with fake clocks, 33 tickets, >100 outcomes, concurrent read/search/slow clients, and >8 simultaneous pre-header/slowloris connections. Deterministic bounded eviction/accounting, no raw-to-display resurrection, no unbounded parser/event/task/confirmation/replay queues; socket admission and deadlines apply before route middleware. Consumer pressure independent: display cap does not truncate raw or manufacture complete state; raw cap does not stop sanitized live display/forwarding. Check allocated capacities and engine memory limits, not flaky RSS-only assertions.

Proposed commands once authorized code/tests exist:

```sh
cargo test -p tnl --lib inspection
cargo test -p tnl --lib replay
cargo test -p tnl --test forwarder_upload_streaming --test forwarder_inspection
cargo test -p tnl --test inspection_api --test inspection_replay_api --test cli_inspection
cargo test -p tnl --test replay_retention --test replay_engine
cargo test -p tnl --test forwarder_peek --test peek_request_head --test progress_format
cargo test -p tnl --test forwarder_local_no_response -- --test-threads=1
cargo test -p tnl-e2e --test inspector_e2e --test inspection_history --test replay_e2e \
  --test host_rewrite --test reconnect --test full_roundtrip_response_framing
# Include visitor-envelope and WebSocket lane targets after their names are frozen.
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Test target names above are proposals and must match the chosen files (do not claim execution against nonexistent targets). Manual local browser acceptance must verify fragment removal, no storage/console leakage, explicit per-action flow, XSS-safe detail rendering and no mutation on reload/error. Independent reviewer checks raw/display ownership, allocation/admission/expiry, parser ambiguity, all terminal paths, mutation middleware/ticket atomicity, generation races, engine destination and delivery semantics. No deployment or real credential use in this design task.

## 11. Remaining material questions and integration risks

Selected browser/service/ownership/envelope/generation decisions in the opening section are **not open**. Before implementation, parent/user must still resolve:

- **M1 P0 scope/owner and timeout semantics:** explicit approval for bounded concurrent upload/response sequencing repair; assign coordinated WebSockets/forwarder owner. Current code ordering is verified but not runtime-reproduced here. No capture claims waive this prerequisite.
- **M2 Privacy/default limits:** approve proposed display allowlist, JSON-only preview policy, masked visitor-IP display, raw-retention flag/warnings, caps/counts/TTLs and no durable storage. User approval of browser replay is not implicit approval to retain secrets by default; proposed raw remains explicit opt-in.
- **M3 Bootstrap delivery:** approve dedicated256-bit capability via stderr fragment URL (sensitive link/log risk), or select safer private delivery within the **same browser service**. Do not fall back to Unix replay IPC/account bearer/unauthenticated loopback.
- **M4 Replay fidelity/options:** approve proposed loopback-only fixed backend, supported methods, binary request cap, no request editor, preserved captured auth/signatures and optional Host override. If Host overrides are unnecessary, omit this option and bind confirmation to default captured Host only; do not silently expand arbitrary edits.
- **M5 Envelope compatibility contract:** visitor-IP owner must publish exact negotiation/version/length caps, typed metadata and old-peer failure/fallback policy, coordinated with request-ids. This is an integration contract gap, not a request to reverse the selected envelope architecture. Observer consumes typed unavailable/legacy only if that policy permits it.

Confirmation ticket route/schema/limits and safe asynchronous result contract are proposed execution-ready details; reviewers must validate atomic consume/start/generation behavior before code. No raw-material access can be added to make a UI shortcut easier. Same-user attackers/extensions are not sandboxed; preserved credentials may authorize destructive operations even on loopback. Signature timestamps/idempotency/backend state can invalidate expected replay behavior. Memory-only still permits OS swap/core dumps. Observer and Hyper may disagree: omit/reject replay eligibility rather than repair traffic. P0/WebSockets/envelope/context edits are high-collision seams requiring sequential coordinated integration, not independently merged rewrites.

## 12. Exact changed contracts from earlier specs

| Previous contract | Revised contract |
| --- | --- |
| Inspection read-only service; no replay/POST routes | One authenticated browser service includes bounded POST preview and confirmed execute, linked GET outcomes; all original read features retained. |
| Replay Unix socket, standalone `tnl replay --socket --yes`, Unix-only control | Removed. Same browser UI/service/auth on supported client platforms; explicit per-action browser confirmation replaces CLI --yes; no second listener. |
| Inspection-owned complete/raw replay store and snapshot lookup in sibling proposal | Removed. Inspection owns common framing observer/sanitized store; replay independently owns raw retention/private snapshot/eligibility/engine; only safe facade crosses to server. |
| Potentially separate IDs or inspection `session_generation` incrementing per reconnect | One shared CaptureId; separate canonical request ID and execution ReplayId; TunnelGeneration stable on reattach, changed on recreation, validated at execution. |
| Request ID optional inferred header/no envelope planned | Negotiated bounded visitor/request-ID envelope selected; visitor-IP owns wire/validation, inspection consumes typed metadata only. Legacy policy must be supplied by owner. |
| Replay postponed/disabled entirely in inspection milestone | Replay action included, but only for separately opted-in, eligible raw captures, with confirmation, no raw fetch and no reconstruction from display. |
| API accepts no request bodies; no mutation-specific context rule | Only two replay POSTs accept strict ≤4-KiB JSON; exact Origin and same-origin Fetch Metadata mandatory plus bearer/Host/no-CORS; all other request bodies unsupported. |
| Upload repair buried among observer integration prerequisites | Separate bounded approval-gated P0 with explicit owner/tests and unchanged forwarding scope constraints. |

**Actual validation:** read-only git status/HEAD/index check, source reads and sibling/previous artifact reads. HEAD matches baseline. Pre-existing untracked worktree plan left untouched; index empty. **Tests run: none.** This revised external design artifact is the only authored change; parent installs/reconciles it and requests independent review before implementation.
