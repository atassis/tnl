# Visitor IP: implementation-ready task specification

**Status:** Approved for bounded T1 implementation by the user/parent, reconciled with `integration/docs/plans/roadmap-execution.md`. The common `RequestMetadataV1` includes both `visitor` and typed `request_id: Ulid` from the daemon’s existing handler ID; T2 owns subsequent ID propagation/header changes. Authenticated control-WS capability negotiation remains the single format negotiation. Historical proposal language below is retained as design background; settled execution choices take precedence.

**Goal:** Show the daemon-authoritative visitor IP in `tnl http` terminal request rows and JSON request records, with explicit provenance and conservative proxy trust.

**Baseline:** `/home/atassis/.config/superpowers/worktrees/tnl/visitor-ip`, branch `feat/roadmap-visitor-ip`, HEAD `43f2bc0ed29e00821e1ede624b8fe190eb0d5c57` (v0.1.0-beta.4 workspace). Worktree initially clean. This design artifact is external to the worktree; no source changes are authorized in this phase.

**Process:** Read `AGENTS.md` and the installed using-superpowers, brainstorming, and writing-plans skills. Adapted their exploration, alternatives, explicit approval gate, task decomposition, and self-review to the user's bounded design-only instruction: no implementation, commits, child agents, or deployment.

## 1. Verified current behavior

References are to the baseline, not the historical specs.

| Area | Verified implementation and consequence |
|---|---|
| Actual peer | `crates/tnld/src/serve.rs:108–117` serves the router using `into_make_service_with_connect_info::<SocketAddr>()`. The socket peer is available already. `crates/tnld/src/data_plane.rs:51` extracts only state and request; it never uses the peer. |
| Existing peer consumer | `crates/tnld/src/admin.rs:84` extracts `ConnectInfo<SocketAddr>` for pairing redemption. Do not replace that security/rate-limit identity with visitor identity in this lane. |
| Request proxying | `data_plane.rs:147–195` opens one daemon-initiated yamux substream and immediately treats it as an HTTP/1 connection. `data_plane.rs:229–239` strips hop-by-hop/framing headers, forces `Connection: close`, and sends HTTP/1.1 through hyper. `strip_hop_by_hop` at line 29 also removes all fields named by every `Connection` value. Ordinary `Forwarded`, `X-Forwarded-For`, and `X-Real-IP` are otherwise passed through, not authenticated or normalized. |
| Daemon logs | `data_plane.rs:317–325` emits host/method/path/status/request ID but no peer/visitor IP. Existing failure events are at lines 160, 199, and 269; unknown-host has a debug event. Some early returns emit no request event. `tnld/src/main.rs:120–133` installs a text tracing subscriber on stderr; there is no daemon JSON-format CLI switch. |
| Terminal/JSON logs | The live type is **`tnl::inspector::LogLine`**, `crates/tnl/src/inspector.rs:9–28`. It has target/resolved backend address but no visitor IP. `Inspector::run` at line 65 serializes this type for JSON or calls `print_text` at line 81. Backend `resolved_addr` is not the visitor. |
| Misleading existing type | `tnl-protocol/src/messages.rs:50–59` has another `LogLine` with `remote_ip: Option<IpAddr>`. Repository references show only its own round-trip test uses that field; it is not the CLI request-log path. Do not extend or consolidate this dormant type for this task. |
| Client pipeline | `tnl/src/client.rs:181–207` accepts daemon-opened streams and spawns `forwarder::forward`. `forwarder.rs:185–203` generates its own request ID, probes an HTTP head, then connects locally. Successful, connect-failure, and phase-2 synthetic-failure records are constructed at lines 225, 445, and 494. `synth_phase2` starts at line 463. Thread identity through all three, not just successful traffic. |
| Head limits | `tnl/src/peek.rs:13,29–70` caps the request-head probe at 8 KiB, returns leftover body bytes separately, and can return capped/EOF/timeout input. Despite comments about fatal input, `forwarder.rs:198–202` currently continues with these buffers. Do not assume every head is complete or add an HTTP-only metadata field that could leak after the cap. |
| Delivery semantics | `forwarder.rs:523–529` uses `try_send`; full/closed inspector channels drop records with a warning. Some I/O failures return before emitting a record. This task must preserve these existing semantics, not promise lossless logs or a record for every possible transport failure. |
| Control handshake | `tnl/src/client.rs:31–69` builds an authenticated `/control` WS upgrade, currently discards its HTTP response, then opens the control substream. `tnld/src/control.rs:15–21` handles the upgrade. `control.rs:54–59` publishes a `SessionHandle`; `serve.rs:20` currently aliases it to an `Arc<Mutex<Box<dyn Session>>>`. This is a supported HTTP handshake seam for additive capability negotiation. |
| Reconnection | Both create and reattach go through `dial_and_open_control` (`client.rs:117,153`). `reconnect.rs:129–142` constructs the production `ForwardCtx`; tests also call `run_accept_loop` directly with `ForwardCtx::new`. Negotiated format must be copied on each connection/reattach, not cached by tunnel ID. |
| Configuration | `tnld/src/config.rs:7–16` has listen/public URL/hostname root/tokens/grace fields, no trust policy. `load_with_env:29–84` implements injected-env resolution and overlap warnings. `tnld serve` accepts only `--config` (`main.rs:15–19`). `commands/init.rs:170–183` renders config directly. |
| Caddy topology | `deploy/Caddyfile:17–27` sends wildcard traffic to `127.0.0.1:7777`, sets `Connection: close`, and has no explicit forwarding-header/trusted-upstream settings. API traffic uses the same upstream at lines 31–36. `deploy/config.example.toml:6` binds tnld to loopback; the Compose example uses host networking. `deploy/Caddy.Dockerfile` floats on Caddy 2.x images, so actual deployed forwarding behavior/version is **not verified** by these files. |
| CIDR dependency | `ipnet` 2.12.0 exists transitively in `Cargo.lock:977`; it is not an explicitly declared workspace dependency. Declare a direct dependency instead of relying on transitivity or hand-writing CIDR matching. |

### Existing test coverage and gaps

- `crates/tnl-e2e/tests/inspector_e2e.rs` exercises a real daemon/session/backend and asserts method/path/status only. Its name/comment says exactly one log but it does not assert absence of a second record.
- `crates/tnl/tests/progress_format.rs` checks that text rendering does not panic; it does not capture/assert rendered content or JSON.
- `crates/tnld/tests/data_plane_logs.rs` only asserts a substring from the unknown-host/debug or request event, not structured identity or a successful tunnel.
- Existing `forwarder_connect_refused.rs`, `forwarder_local_eof.rs`, `forwarder_local_malformed.rs`, `forwarder_local_no_response.rs`, and `forwarder_peek.rs` provide useful failure/streaming fixtures, but no authoritative metadata.
- Existing e2e tests cover IPv6 **backend targets**, response framing, many response headers, host rewriting, reconnect, and random subdomains. None establishes IPv6 visitor identity or proxy trust.
- No implemented trusted-proxy, forwarding-chain, or visitor-IP tests were found. Historical `docs/specs/2026-05-26-tnl-design.md:276` mentions Caddy XFF for allow-lists; that is not current implemented trust behavior.

## 2. Threat model and bounded scope

A visitor controls every public HTTP header and may send duplicates, malformed lists, misleading internal-looking names, `Connection` nominations, and IPv4/IPv6 forms. A directly connected untrusted peer must not influence identity through headers. An operator-configured trusted proxy is trusted to accurately overwrite or append its actual peer; IP attribution cannot correct a lying/compromised trusted proxy. An untrusted intermediary is the attribution boundary, not proof of the original person/device behind it.

**In scope:** actual data-plane peer extraction; explicit daemon-wide trusted proxy CIDRs; deterministic XFF chain resolution; provenance; authoritative per-request delivery on the existing tunnel; additive client log fields and text presentation; targeted tests and deployment/config documentation.

**Non-goals:** shared request-ID generation/propagation/response headers; access-control decisions, pairing rate-limit changes, per-tunnel allow-lists or basic auth; WS upgrade support; inspection capture/store/UI; replay policy; geo/ASN lookup; PROXY protocol; QUIC or transport-driver changes; client-config trust overrides; public/backend forwarding-header normalization; a new daemon JSON CLI mode; lossless logging; changing JSON banner/shutdown behavior; production deployment or credential work.

The existing `tnl http --json` path also prints a banner from `reconnect.rs:118–123` and a shutdown message from `commands/http.rs:32`. This lane guarantees fields on **request JSON records**, not conversion of the entire stdout stream into pure NDJSON. Inspection/release owners must reconcile any broader stdout contract separately.

## 3. Approaches and recommendation

| Approach | Advantages | Costs/security/compatibility |
|---|---|---|
| **A. Negotiated bounded metadata prelude on each data substream — recommended.** Resolve identity at tnld; send length-prefixed typed JSON before HTTP; consume it before client head probing. Negotiate with one additive header on the existing authenticated `/control` WS handshake. | Separate authority from public HTTP; no reserved metadata headers leak to backend; no cap/order ambiguity; existing length-prefixed JSON convention; reusable request-ID seam; old peers can remain raw HTTP. | Adds a versioned data-substream envelope and per-session negotiated state. Must update direct e2e callers and validate handshake behavior through Caddy. Overlaps request-ID/WS owners at a small explicit seam. |
| B. Daemon-stamped internal HTTP headers with negotiation. | Smaller apparent change to the HTTP stream; no binary prelude. | Still needs negotiation so a new client connected to an old daemon does not trust visitor-supplied internal headers. Requires removing every spoofed duplicate, stamping after hop stripping, byte-safe client extraction/removal, and handling partial/oversize heads without leaking metadata. More fragile around inspection/replay/WS; not recommended. |
| C. Mandatory versioned prelude with coordinated breaking upgrade. | Simplest framing; fail closed without legacy paths. | Old daemon/client combinations stop forwarding; forces synchronized rollout and rollback. The authorized eventual combined deployment does not itself authorize breaking wire compatibility. Use only if explicitly approved instead of A. |

A client-only `X-Forwarded-For` parser is not a viable approach: it cannot authenticate the ingress peer, daemon trust policy, or metadata origin.

## 4. Material approvals and reconciliation gates

These are proposed defaults, **not** decisions already granted:

1. **Trust/production topology:** approve default trust-none and explicit CIDRs, including the actual gateway peer networks to configure at eventual deployment. For the checked-in host-loopback Caddy topology the narrow example is `127.0.0.1/32` (plus `::1/128` only if Caddy actually connects over IPv6). Do not auto-trust loopback/private networks, infer trust from `listen`, or invent container subnet values. Any local process can spoof a trusted loopback proxy; an operator accepting this topology accepts that boundary.
2. **Header policy:** approve XFF-only resolution in this iteration. Ignore `Forwarded` and `X-Real-IP` for identity, including when XFF is absent; no automatic header-family fallback or cross-family precedence. Approve strict rejection-to-peer fallback for duplicate XFF fields, malformed/oversized chains, and all-trusted chains. Full RFC 7239 support would expand the parser and is a separate approval, not an implicit requirement.
3. **Wire/compatibility:** approve approach A, the handshake header/token and framing in §5, and explicit unavailable visitor identity with legacy servers instead of insecure header inference. Request-ID and WS design owners must agree to one envelope/negotiation owner before editing shared files. If another lane already proposes an equivalent envelope, unify it rather than introducing a second one.
4. **Output contract:** approve the additive `visitor_ip`, `peer_ip`, `visitor_ip_source` fields and a labelled terminal suffix, including source names/fallback semantics. Existing `resolved_addr` keeps its backend meaning. IP logs contain personal data; retention/export policy remains operator-owned.
5. **Ingress proof:** approve requiring a credential-free local test against the actual intended Caddy binary/version before combined release. Repo source does not prove its default XFF append/sanitization behavior, especially for Caddy behind another proxy. This blocks a claim of production correctness, not completion of pure resolver unit tests.

No implementation/deployment should begin merely because this document recommends these choices.

## 5. Proposed exact contracts

### 5.1 Configuration and resolver

New top-level TOML key:

```toml
# Default: [] (trust no forwarding headers).
trusted_proxies = ["127.0.0.1/32"]
# Add "::1/128" only for an actual IPv6 loopback proxy hop.
```

- Proposed public `Config` field: `#[serde(default)] pub trusted_proxies: Vec<String>`.
- Proposed env: `TNLD_TRUSTED_PROXIES`, comma-separated CIDRs. Trim ASCII surrounding whitespace per entry; an entirely empty/whitespace value means `[]` (explicitly disable file trust); empty entries in a nonempty list are errors. Env replaces the whole file list and follows existing overlap-warning conventions. No new serve/client flag or interactive wizard question.
- Final resolved values must validate before listening. Invalid prefixes, bare IPs without a prefix, hostnames, zones, or a CIDR with host bits set fail configuration/startup with context naming the key/entry. Validate in `Config::load_with_env` and defensively when programmatic `Config` enters `spawn_server_with_state`; bad CIDRs must never become trust-none silently. Invalid file TOML/type still fails before env resolution, following existing loader behavior.
- Compile once into proposed `TrustedProxies` in `tnld/src/visitor_ip.rs`, store `Arc<TrustedProxies>` in `AppState`. Proposed interface:

```rust
impl TrustedProxies {
    pub fn from_cidrs(cidrs: &[String]) -> anyhow::Result<Self>;
    pub fn resolve(&self, peer: std::net::IpAddr, headers: &http::HeaderMap)
        -> tnl_protocol::request_metadata::VisitorIdentity;
}
```

- Use `ipnet = "2"` as an explicit workspace dependency and `ipnet.workspace = true` in tnld. No custom bitmask implementation. CIDRs cover IPv4 and IPv6 independently. Explicit `/0` is permitted but must produce a startup warning explaining the lost trust boundary; documentation strongly discourages it.
- Normalize IPv4-mapped IPv6 peers and header literals to IPv4 before matching/serialization. Match normalized IPv4 using IPv4 CIDRs; IPv6 ranges, even `::/0`, do not implicitly trust mapped IPv4. Document use of IPv4 CIDRs for those peers. Native IPv6 remains IPv6; private, loopback, and IPv6 ULA addresses are not globally filtered out.

**Resolution algorithm (normative proposal):**

1. Canonicalize the actual socket peer. If it is not in `trusted_proxies`, return that peer with source `peer`; ignore all forwarding headers without attempting their parsing.
2. For a trusted peer, count XFF field occurrences using `get_all`, not `get`. Missing => peer/source `peer_missing_forwarding`. More than one field => peer/source `peer_invalid_forwarding`, even when values are identical. A single field may contain a comma-separated multi-hop list.
3. Enforce at most **4096 bytes** in that field and **32 address tokens** before allocating an unbounded list. Require ASCII-compatible header text. Split on commas; trim only HTTP OWS (space/tab); each nonempty token must parse as a bare `IpAddr`. Reject the whole list for any invalid/empty token, quotes, `unknown`, obfuscated identifiers, IPv4 ports, bracketed IPv6, IPv6 zones, or unsupported syntax. No DNS. Exceeding a limit => peer/source `peer_invalid_forwarding`. Validate the complete list, not just a favorable suffix.
4. Starting from the actual trusted peer, scan the parsed list **right to left**. Skip trusted addresses. The first address outside the configured CIDRs is the visitor, source `x_forwarded_for`. Addresses farther left cannot override this boundary. Repeated IP literals within the one list are allowed; they are not duplicate header fields and scanning is bounded.
5. If every listed address is trusted, do not pretend to know an origin: return the actual peer/source `peer_all_trusted`. All fallback results explicitly identify the socket peer, not the claimed header origin. Never reject an otherwise valid HTTP request just because its forwarding metadata is bad.

The identity must be resolved exactly once, before hop-by-hop stripping and before host/tunnel lookup. Do not derive it from control-session/client/backend addresses. `Forwarded`/`X-Real-IP` remain ordinary application headers and do not affect identity. Existing backend header pass-through stays unchanged and is **not** guaranteed safe for backend authorization.

### 5.2 Shared metadata types and framing

Create `crates/tnl-protocol/src/request_metadata.rs`, exported from `lib.rs`. Proposed types:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DataStreamProtocol {
    #[default]
    RawHttp1,
    Http1MetadataV1,
}

// Serialize this enum as the exact snake_case strings below.
pub enum VisitorIpSource {
    Peer,                  // "peer"
    XForwardedFor,         // "x_forwarded_for"
    PeerMissingForwarding, // "peer_missing_forwarding"
    PeerInvalidForwarding, // "peer_invalid_forwarding"
    PeerAllTrusted,        // "peer_all_trusted"
}

// These wire structs derive Debug, Clone, Serialize, Deserialize, PartialEq, Eq.
pub struct VisitorIdentity {
    pub visitor_ip: std::net::IpAddr,
    pub peer_ip: std::net::IpAddr,
    pub source: VisitorIpSource,
}
pub struct RequestMetadataV1 {
    pub visitor: VisitorIdentity,
}
```

Proposed constants: `DATA_STREAM_PROTOCOL_HEADER = "x-tnl-data-stream-protocol"`, `HTTP1_METADATA_V1 = "http1-metadata-v1"`, `MAX_REQUEST_METADATA_BYTES = 1024`.

- The daemon writes **u32 big-endian payload length**, then exactly that many UTF-8 JSON bytes, then ordinary HTTP/1 request bytes on the **same data substream**. Length must be `1..=1024`. The negotiated token supplies the version; no magic-byte sniffing or body/header fallback.
- Example JSON payload: `{"visitor":{"visitor_ip":"198.51.100.7","peer_ip":"127.0.0.1","source":"x_forwarded_for"}}`.
- Read/write helpers take `AsyncRead`/`AsyncWrite + Unpin + ?Sized`, return a dedicated `thiserror` `RequestMetadataError` for I/O/length/JSON failures, and reject oversized lengths **before allocation**. Serde rejects duplicate recognized fields, bad IP strings, missing mandatory visitor members, and unknown source values. Unknown object fields are tolerated for reconciled additive metadata, not used as identity.
- Proposed functions: `read_request_metadata(reader) -> Result<RequestMetadataV1, RequestMetadataError>` and `write_request_metadata(writer, &RequestMetadataV1) -> Result<(), RequestMetadataError>`. Both validate the bound; write helper flushes before handing the stream to hyper.
- Callers bound the whole read/write operation with a **5-second** timeout. No backend connection until a negotiated prelude has decoded successfully. A malformed/truncated/timed-out prelude closes that substream and returns an error to the existing accept-loop warning path; it never retries parsing the bytes as HTTP, never forwards the prelude, and does not invent a visitor record for an undecodable request. Other substreams/session must remain usable.
- A daemon prelude-write error yields the existing transport-error response pattern: 502, component `transport`, proposed kind `request-metadata-write`, with a structured warning. Existing request-ID/error-page ownership remains untouched.
- Responses remain plain HTTP; no response prelude. Metadata bytes do not count as HTTP `bytes_in`/`bytes_out`. Body streaming, HTTP status parsing, and connection-close behavior remain unchanged.

### 5.3 Negotiation and actual callers

1. New clients add exactly one `X-Tnl-Data-Stream-Protocol: http1-metadata-v1` to their own authenticated `/control` WS upgrade. This is a tunnel-control capability, **not** a public data-plane header.
2. New daemons select metadata-v1 only for exactly one matching offer. Missing, duplicate, or unsupported offers select `RawHttp1` and produce no capability response header. Add the exact selected header to the HTTP **101** response through axum's normal response headers; do not edit the WS bridge or yamux driver.
3. The new client inspects the actual upgrade response. Missing echo selects raw mode. Exactly one matching echo selects metadata-v1. Duplicate, empty, or unsupported echoes are a connection error; do not guess a stream format.
4. Carry the selected immutable mode into `run_session`. Replace the `SessionHandle` alias with a small `Debug` handle struct holding the existing session mutex and immutable `data_stream_protocol`; data-plane callers open via that mutex and read the associated mode. Preserve the DashMap/session lifecycle, locking scope, drop-based shutdown, and yamux role mapping. No separate mutable capability map or per-request negotiation.
5. `dial_and_open_control` returns the selected mode with session/control. Add `pub data_stream_protocol: DataStreamProtocol` to `ConnectedSession`. Copy it into a corresponding `ForwardCtx` field in production reconnect and direct session consumers. `ForwardCtx::new` remains raw by default for standalone byte-stream tests; session-based e2e tests must explicitly use the negotiated mode. Do not silently switch all raw test fixtures to expect a prelude.
6. New client + old daemon: preserve forwarding, log identity unavailable, never infer from public HTTP headers. Emit one stderr warning for raw-mode sessions per reconnect run, not one per request/retry. Old client + new daemon: raw forwarding unchanged, no prelude leakage. New/new: metadata-v1. Reattach negotiates again on its new WS connection, including version transitions; never inherit a stale format from the tunnel reservation.

The protocol `ControlMsg`, CreateTunnel/Reattach/TunnelCreated payloads, and unused protocol `LogLine` need no changes for this recommended negotiation approach.

### 5.4 Logs and terminal contract

Extend the **live client inspector** `LogLine` with these additive serialized fields (one canonical spelling):

```rust
pub visitor_ip: Option<std::net::IpAddr>,
pub peer_ip: Option<std::net::IpAddr>,
pub visitor_ip_source: Option<tnl_protocol::request_metadata::VisitorIpSource>,
```

- Negotiated valid metadata populates all three on success, connect refusal/timeout, and phase-2 synthetic failures. Raw legacy mode populates all three as `None`; JSON includes explicit `null` values, not omitted fields. No use of dormant `remote_ip` or backend `resolved_addr` for visitor identity.
- Keep existing text columns and append `  ip=<address-or-> source=<source-or-unavailable>` at the end. Full canonical IPv6, no source port, no truncation of IP, no ANSI when existing color policy disables it. Examples: `ip=198.51.100.7 source=x_forwarded_for`; `ip=127.0.0.1 source=peer_missing_forwarding`; `ip=- source=unavailable`.
- `--json`, `--quiet`, `-v`, `-V`, and `NO_COLOR` retain their existing meanings. Quiet still suppresses request records. Diagnostics stay on stderr. Add focused deterministic rendering assertions instead of only calling `print_text` and hoping it works.
- Enrich existing daemon data-plane events through a request-scoped tracing span with fields `visitor_ip`, `peer_ip`, `visitor_ip_source`, resolved before host lookup. Instrument the async future rather than holding an entered tracing guard across `.await`. Do not add a new logging backend or claim new log events for every existing early return.

### 5.5 Shared request-ID seam (owned by the request-ID lane)

The metadata prelude is one envelope per daemon-opened request substream. The request-ID owner may add a typed `request_id` field to `RequestMetadataV1` **during design reconciliation before v1 is frozen**, and consume that alongside `visitor` before head probing. This lane must not generate a new shared ID, change the currently independent daemon/client IDs, choose their trust policy, or modify request/response ID headers. The metadata writer is before hyper's handshake; the reader is before `peek_request_head`. Those are the exact producer/consumer seams. Agree one owner for shared envelope code and errors; do not create separate ID and visitor preambles or public-header trust paths.

## 6. Dependency order, owned files, and overlap

**Order:** approvals + common envelope reconciliation → independent resolver/config and envelope/negotiation tests → daemon producer + client consumer → logs → e2e/compatibility/Caddy proof → parent integration → one combined release-validation/deployment shot under separate authorization.

| Ownership proposed for this lane | Files and boundary |
|---|---|
| New focused implementation | `crates/tnld/src/visitor_ip.rs`; `crates/tnl-protocol/src/request_metadata.rs`. The protocol file becomes a jointly agreed seam with request IDs, not two independently edited envelopes. |
| Dependency/config wiring | Root `Cargo.toml`, `Cargo.lock`, `crates/tnld/Cargo.toml`, `crates/tnld/src/config.rs`, `crates/tnld/src/lib.rs`, `crates/tnl-protocol/src/lib.rs`. Root dependency edits overlap access-controls; share one direct ipnet version. |
| Daemon wiring | `crates/tnld/src/serve.rs`, `control.rs`, `data_plane.rs`. Only capability response/state changes in control; no axum↔tungstenite bridge changes. Overlap request IDs, WS, access-controls. Resolve identity once and offer the typed value to access-controls, but do not implement enforcement. |
| Client wiring/logs | `crates/tnl/src/client.rs`, `reconnect.rs`, `forwarder.rs`, `inspector.rs`. Preserve `run_accept_loop` signature; pass negotiated format through `ForwardCtx`. Overlap WS/request IDs/inspection/replay. Preserve HTTP bytes after consuming the prelude. |
| Documentation/init | `deploy/config.example.toml`, `deploy/README.md`, `crates/tnld/src/commands/init.rs` (comments and safe trust opt-in guidance only), `README.md` (request-log fields/limitations). No new CLI flags or automatic trust in `tnld init`. Caddyfile change only if the separate ingress proof demonstrates a necessary correction and that correction is approved. |
| Tests | New `crates/tnld/tests/visitor_ip.rs`, `crates/tnld/tests/control_metadata_negotiation.rs`, `crates/tnl/tests/forwarder_request_metadata.rs`, `crates/tnl-e2e/tests/visitor_ip_roundtrip.rs`, `crates/tnl-e2e/tests/visitor_ip_caddy.rs`; update source-named in-file unit tests, `tnl/tests/progress_format.rs`, `tnld/tests/data_plane_logs.rs`, `tnl-e2e/tests/inspector_e2e.rs`. |
| Mechanical callers | Add `trusted_proxies: vec![]` to existing tnld Config literals across `crates/tnld/tests`, daemon-backed `crates/tnl/tests`, and `crates/tnl-e2e/tests`; copy session mode at every session-based `run_accept_loop` call. Use the inventory commands below; do not refactor unrelated fixtures. |
| Explicitly not owned | `tnl-protocol/src/transport.rs`, request-ID header/generation policy, public WS tunneling implementation, replay/capture storage, pairing/admin identity or authorization, release/deployment scripts. |

Call-site inventory before implementing (also catches later merged callers):

```sh
rg -n '(TnldConfig|Config|tnld::config::Config) \{' crates
rg -n 'SessionHandle|session_handles|ConnectedSession \{|ForwardCtx \{|ForwardCtx::new|run_accept_loop' crates
rg -n 'LogLine \{' crates/tnl crates/tnl-e2e
```

`Config` grep also finds the distinct client config; modify only daemon Config literals. No tests constructing ControlMsg need new fields for approach A.

## 7. Numbered implementation subtasks (execute only after approval)

1. **Freeze contracts and overlap ownership.** Record decisions from §4. Reconcile the prelude/token with request-ID and WS owners, choose one shared-file writer/order, and confirm access-controls will consume the same resolver rather than independently trusting headers. Keep changes in this lane bounded to this spec.
2. **Build resolver first with failing security tables.** Create `tnld/src/visitor_ip.rs`, export it in `lib.rs`, add the direct workspace ipnet dependency, and implement `TrustedProxies` against cases R1–R10 below. Table assertions must compare the whole typed identity including provenance. No HTTP calls or DNS needed for resolver tests.
3. **Add config validation and startup wiring.** Update `Config`, env parsing/overlap warnings, `AppState`, and `spawn_server_with_state`. Exercise default/disable/invalid CIDR cases before implementation; validate before binding. Update daemon Config fixture literals only. Add safe commented wizard output/example guidance; do not silently opt existing deployments into trust.
4. **Implement the versioned codec with failing framing tests.** Add `request_metadata.rs` and exports; test exact envelope/length, fragmented input, byte boundaries and failures in M1–M3. Do not modify transport traits or ControlMsg. Bound allocations before reading payloads.
5. **Negotiate on the existing control upgrade.** Update `control.rs` handler response and immutable session handle; update client handshake return/`ConnectedSession`, reconnect context, and session-backed test callers. Test new/legacy/unknown/duplicate response behavior and reattach before enabling prelude production. No WS bridge rewrite.
6. **Connect producer/consumer and failures.** Resolve identity at data-plane ingress. For metadata-v1 write/flush the prelude before `TokioIo`/hyper handshake; legacy mode stays raw. Client consumes the bounded prelude before existing HTTP probing, and threads identity to all three record constructors and `synth_phase2`. Preserve leftover body bytes, counts, host smoothing, and failure attribution. Add no HTTP metadata header.
7. **Expose log fields and prove presentation.** Extend inspector records/rendering, all affected record fixtures, and daemon request span. Introduce a small internal text-formatting helper only if needed for deterministic assertions; do not redesign inspector. Assert actual suffix/JSON values, null legacy fields, quiet behavior, IPv6 and no ANSI. Preserve the known banner limitation.
8. **Prove through real sessions.** Add the e2e matrix in §8; assert exactly one received record and no second record within a bounded check where expected. Include failure identity, spoof attempts, mixed-session concurrency and reattach. Re-run existing framing, host rewrite, many-header, target IPv6, and reconnect suites; no regression in application bytes.
9. **Run local Caddy proof and integration checks.** Add the credential-free ignored Caddy fixture described below; run only when a supplied local binary is available under authorized testing. Record its actual version and effective config, not secret-bearing production config. Complete documentation and required workspace gates. Hand results to parent/reviewer; do not independently deploy, publish, or merge.

For each subtask with behavior: add the named failing tests first, verify the failure is meaningful, implement narrowly, run focused tests, then run workspace gates at the integration boundary. This document intentionally specifies contracts/tests rather than supplying a speculative implementation patch.

## 8. Concrete acceptance cases

Use documentation-only IP ranges. Trusted example set below is `127.0.0.1/32`, `192.0.2.0/24`, `2001:db8:1::/48`, unless overridden. Tests must use the typed resolver for synthetic remote peers; do not depend on binding documentation addresses locally.

| ID | Input / failure | Required assertion |
|---|---|---|
| R1 | Trust empty; actual peer `198.51.100.7`; arbitrary forged XFF/Forwarded/X-Real-IP including malformed bytes | Visitor/peer both actual peer, source `peer`; no header changes identity. |
| R2 | Actual peer `127.0.0.1`, trust empty vs explicit `/32`; XFF `198.51.100.7` | Without trust => loopback/`peer`; with trust => `.7`/`x_forwarded_for`, peer stays loopback. |
| R3 | Trusted loopback; XFF `198.51.100.7, 192.0.2.9` | Visitor `.7`; trusted suffix skipped. |
| R4 | Trusted loopback; XFF `203.0.113.66, 198.51.100.8, 192.0.2.9` | Visitor `198.51.100.8`; leftmost spoof ignored beyond first untrusted hop. |
| R5 | Trusted peer with absent XFF, empty XFF, identical/different repeated XFF fields, mixed-case duplicate names | Absent => `peer_missing_forwarding`; all other listed forms => `peer_invalid_forwarding`. HTTP request still forwards. |
| R6 | XFF contains empty element, leading/trailing comma, bad IP, `unknown`, `_hidden`, quote, IPv4 `:port`, bracketed IPv6, zone ID, invalid header text | Full-chain fallback to actual peer/`peer_invalid_forwarding`; no partial successful attribution. Include bad token left of a valid untrusted suffix. |
| R7 | 4096 vs 4097 field bytes; 32 vs 33 valid IP tokens | At/below limits valid when syntax valid; above either limit => invalid fallback. Include OWS in byte count. |
| R8 | Trusted peer; list contains only trusted addresses or repeated trusted addresses | Actual peer/`peer_all_trusted`; no invented leftmost visitor. Duplicate literals within a single valid list are not duplicate fields. |
| R9 | Native IPv6 peer/client/chain; mixed v4/v6 CIDRs; `::ffff:198.51.100.7` | Canonical native IPv6 preserved; mapped form normalized to IPv4 and matches only v4 CIDRs. IPv6 peer matching tests are distinct from IPv6 backend tests. |
| R10 | Trusted peer; contradictory Forwarded/X-Real-IP and valid XFF; Forwarded-only; private/ULA client literal | XFF alone decides; Forwarded-only => missing fallback; valid private/ULA visitors allowed, no public-only heuristic. |
| C1 | Old minimal config; TOML list; env list replaces file; empty env disables; overlap warning | Correct resolved list/default and warning behavior, no auto trust. |
| C2 | Bad prefix, host bits, hostname, bare IP, empty env segment, invalid programmatic Config | Explicit startup/config error before listener bind; `/0` warns but works as explicitly configured. |
| M1 | Codec exact example, IPv4 and IPv6, fragmented length/payload, payload immediately followed by HTTP+body in same write | Round-trip identity; next read begins at exact HTTP boundary; HTTP/body bytes neither lost nor consumed by metadata. |
| M2 | Length 0/1025/u32::MAX, truncation at length/payload, malformed JSON, duplicate visitor/source fields, bad IP/source/missing visitor | Bounded error; no huge allocation, panic, backend connect, or raw HTTP fallback. |
| M3 | Metadata stream stalls; another stream is valid; backend records first bytes | Timeout closes only bad substream within bound; good stream succeeds; backend never sees prelude bytes. |
| N1 | Matching/absent/duplicate/unsupported capability offers and echoes | Deterministic §5.3 mode; invalid echo errors. Absence is legacy, not sniffed. No capability echo on unauthenticated control request. |
| N2 | Simulated old/new combinations; new client reattaches to peer with different capability support | HTTP bytes still round-trip in legacy modes; unavailable identity rather than forged XFF; no stale prelude mode or per-request warning spam. |
| E1 | Real daemon + client + backend, direct request with spoofed XFF/Forwarded/internal-looking metadata header; public data request carrying the capability header; `Connection: X-Forwarded-For` | Actual peer identity reaches the request record; forged HTTP values never become metadata or change session mode. Resolve once before hop stripping, so Connection nominations cannot remove/replace the authoritative identity. Backend still receives non-hop application headers per old policy, not a generated metadata header. |
| E2 | Trusted proxy peer with valid/malformed/duplicate chain over actual tunnel | Log fields equal resolver result and provenance; malformed metadata does not turn valid HTTP into 4xx/5xx. Use raw TCP ingress to preserve duplicate header fields rather than an HTTP client that coalesces them. |
| E3 | Valid metadata, local connect refused/timeout, EOF, malformed response, first-byte timeout | Synth 502 still works; emitted failure log keeps visitor/peer/source and existing failure_kind. Deterministic connect-timeout fixture, not an Internet blackhole dependency. |
| E4 | Large request head beyond existing 8 KiB probe, streamed/chunked body, host rewrite, large response header set | Metadata remains independent of HTTP cap; no prelude reaches backend; application request/response byte behavior remains compatible. |
| E5 | Concurrent requests with distinct chains, concurrent raw and metadata sessions, reconnect/reattach | No cross-request/session identity contamination; exactly one completed-request record when existing success/synth paths emit one. |
| L1 | Successful/failing records, legacy nulls, native IPv6, Text/Json/Quiet/no-color | Exact additive JSON values/types and terminal suffix; full IPv6 preserved; backend resolved_addr unchanged; quiet produces no request row/record. |
| L2 | Daemon successful/failure/unknown-host observable events | Captured tracing event/span carries visitor/peer/source where the event exists; existing component/kind/req_id fields remain intact. |
| P1 | Credential-free real Caddy, default single-edge reverse_proxy settings, trusted daemon peer `/32`; request source `127.0.0.2`, forged XFF/Forwarded | Record `127.0.0.2`, not forged address or daemon's Caddy peer; capability echo survives the control upgrade; HTTP response remains correct. |
| P2 | Credential-free multi-hop fixture where Caddy appends its actual immediate peer to a deliberately preserved incoming chain | With upstream proxy CIDR also trusted, resolve through it; without that trust stop at it. Verify actual observed XFF ordering. Configure preservation only inside this controlled synthetic fixture, never publish unsafe blanket-trust guidance as a production default. |

### Caddy proof fixture contract

`visitor_ip_caddy.rs` is ignored by default, uses `TNL_TEST_CADDY_BIN` to invoke an already installed local binary, records `caddy version`, and creates a temporary **HTTP-only loopback** Caddyfile with `admin off`, `auto_https off`, and reverse_proxy to the test daemon. No DNS provider, ACME, images, network downloads, production `.env`, tokens, or credentials. Test-owned temporary auth tokens are synthetic like existing e2e tests.

P1 must exercise unmodified single-edge reverse_proxy forwarding defaults (apart from the existing Connection-close setting), not overwrite XFF in the fixture and claim defaults were tested. Bind the request source to `127.0.0.2` and trust only `127.0.0.1/32` so the visitor is distinguishable. The fixture must report unsupported loopback source binding as a test failure with a clear prerequisite, not silently skip the security assertion. P2 uses a separate explicitly configured append-only synthetic hop: inside its test-only reverse_proxy block propose `header_up X-Forwarded-For "{http.request.header.X-Forwarded-For}, {http.request.remote.host}"`, and send a known nonempty chain from `127.0.0.2`. Assert the actual upstream value as well as the resulting identity, first trusting only `127.0.0.1/32`, then also `127.0.0.2/32`. This tests the chain contract, not a claim that production upstream proxies are already correctly configured. Before release, the release-validation owner must verify the intended Caddy version/topology uses an equivalent trustworthy append/sanitize boundary. If defaults fail P1, the fixture syntax is unsupported, or multi-hop behavior is incompatible, stop and reconcile an approved ingress configuration correction; do not loosen tnld trust to make a test pass.

## 9. Validation commands and evidence expectations

**Actually run in this design phase:** read-only filesystem/source/history/status searches only (`pwd`, `git status --short`, `git branch --show-current`, `git rev-parse HEAD`, `git log -5 --oneline`, `git diff --cached --name-only`, `find`, `rg`, file reads). No cargo/build/unit/e2e/Caddy tests, network, credentials, deployment, commit, or source edits. The initial optional skill-directory probe returned exit 2 because two optional locations did not exist; installed skills were subsequently found/read under `~/.agents/skills/superpowers`.

**Proposed focused commands after approval/implementation** (new targets do not exist at baseline):

```sh
cargo test --offline -p tnld visitor_ip
cargo test --offline -p tnld config::tests
cargo test --offline -p tnl-protocol request_metadata
cargo test --offline -p tnld --test control_metadata_negotiation
cargo test --offline -p tnl --test forwarder_request_metadata
cargo test --offline -p tnl --test progress_format
cargo test --offline -p tnld --test data_plane_logs
cargo test --offline -p tnl-e2e --test visitor_ip_roundtrip
cargo test --offline -p tnl-e2e --test inspector_e2e
cargo test --offline -p tnl-e2e --test host_rewrite
cargo test --offline -p tnl-e2e --test full_roundtrip_response_framing
cargo test --offline -p tnl-e2e --test full_roundtrip_many_headers
cargo test --offline -p tnl-e2e --test full_roundtrip_ipv6
cargo test --offline -p tnl-e2e --test reconnect
# Only with an approved, already installed Caddy binary; no download/build here:
TNL_TEST_CADDY_BIN="$(command -v caddy)" cargo test --offline -p tnl-e2e --test visitor_ip_caddy -- --ignored --nocapture
```

Expected: focused semantic assertions above pass, including negative/security assertions. Missing offline dependencies are a prerequisite failure, not grounds to access the network during this phase.

Required repo integration gates before any eventual commit (not run now):

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git diff --check
git diff --cached --name-only
```

Record actual command exit statuses, test counts, ignored tests and their separately run results, actual Caddy version, and residual platform constraints. Stable rustfmt's known nightly-option warnings are not a new behavior regression. No staged files should be left by a design/implementation worker unless separately authorized.

## 10. Integration risks and stop conditions

- **Cross-lane protocol divergence:** request IDs may also require authoritative metadata, and WS changes touch the same substream start. Reconcile the single envelope/token before code. WS upgrades must not send another prelude after a 101; visitor attribution belongs to the opening request, not subsequent frames.
- **Legacy downgrade:** compatibility preserves HTTP but cannot create trusted IP on old daemons. An intermediary stripping the capability produces visibly unavailable identity, not unsafe fallback. A malicious/compromised daemon remains able to lie; the authenticated endpoint is the authority.
- **Wrong proxy CIDRs:** too broad a range lets clients claim identities; too narrow produces proxy-peer fallback. Host networking versus bridged-container peer addresses must be observed/approved in the release lane; no production inference here. Trusting loopback trusts local processes as well as Caddy.
- **Caddy uncertainty:** floating image tags and lack of explicit upstream proxy configuration prevent source-only proof of multi-hop correctness. P1/P2 and actual deployment topology review are mandatory before the eventual combined release claim. Never treat incoming HTTP headers as proof of the actual peer.
- **Duplicate policy compatibility:** strict single-field XFF may fall back for a proxy that emits multiple list-valued fields. This is deliberate conservative attribution, not request rejection. Document it; supporting ordered duplicate concatenation is a product/security policy change requiring approval.
- **All-trusted fallback:** intentionally avoids claiming a trusted-hop address is the origin. Operators see the proxy with explicit provenance; this differs from libraries that return the leftmost token.
- **Logging expectations:** IP provenance must remain visible; a fallback proxy IP must not be presented as independently verified origin. Existing channel drops/transport-error omissions and JSON banner pollution are not fixed by this lane.
- **Backend security remains separate:** preserving public forwarding headers can still mislead applications that trust them blindly. This change secures tnl attribution, not the backend's authentication/rate limits. Replay must not elevate captured public headers into authoritative metadata.
- **Shared files:** dependency manifests, state, forwarder, reconnect, and inspector are high-conflict integration points. Apply reconciled shared edits once; run the full matrix only after all lane changes are combined. No opportunistic refactors or driver changes.

**Design exit:** deliver this artifact and stop. The next authorized step is parent/user design reconciliation and approval of §4, followed by bounded implementation. This document neither authorizes nor performs integration into main or deployment.
