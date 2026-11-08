---
status: design-reconciled-pending-contract-freeze
scope: tnl
created: 2026-11-07
updated: 2026-11-07
summary: "Request IDs carried in the visitor-IP-owned negotiated bounded envelope; daemon authority retained, raw-mode limitations explicit."
---

# Request IDs: reconciled implementation task specification

## 1. Status and selected direction

**Design/task composition only. No source/test implementation, commits, integration, or deployment is authorized by this document.** This revision replaces the earlier recommendation to extract IDs from an internal HTTP header. The user selected **one negotiated bounded metadata envelope carrying visitor identity and the shared request ID**, with the visitor-IP owner responsible for common envelope/negotiation and this lane adding request-ID behavior through sequential integration.

Baseline remains `/home/atassis/.config/superpowers/worktrees/tnl/request-ids`, branch `feat/roadmap-request-ids`, HEAD `43f2bc0ed29e00821e1ede624b8fe190eb0d5c57`. Re-read local `AGENTS.md`, the current request-ID plan, and sibling `/home/atassis/.config/superpowers/worktrees/tnl/visitor-ip/docs/plans/roadmap-visitor-ip.md`, especially §§5.2–5.5 and §6. Verified the existing control-handshake and forwarder seams against source. Applied the installed planning skills only within the explicit design-only scope; their implementation/commit/subagent steps are not authorized here.

**Output handling:** this full replacement specification is written only to the run's authoritative external output path. It is intended as the replacement content for `docs/plans/roadmap-request-ids.md`; the final explicit output-path instruction supersedes writing that repository copy in this run. The pre-existing untracked worktree copy, sibling plan, all source/tests, and the existing root-checkout `AGENTS.md` edit are left untouched.

### Selected decisions versus remaining proposals

**Selected by the user:**

- One daemon-minted request ID per parsed public data-plane ingress; the final public response header remains authoritative from that daemon context.
- Carry the ID with visitor identity in the **same negotiated bounded envelope**, not in a trusted internal HTTP header.
- Visitor-IP owns the common codec, negotiation, immutable session mode, and context plumbing. Request-ids extends that common contract sequentially; no competing preamble/token/negotiation state.
- Do not raise the HTTP-head cap merely to extract identity. Raw legacy forwarding remains possible but has **no complete shared-ID guarantee**; public visitor headers are never an authority source.

Exact wire details below reproduce the sibling's proposed contract and identify this lane's field addition. They must be frozen by the common owner before implementation/release. Other product decisions remain open (§4); selecting the envelope does not approve those decisions implicitly.

**Negotiated-mode invariant:** a successfully decoded envelope supplies the same daemon ID used by the daemon's diagnostics/final header and the client's existing synth bodies/headers and inspector records. With the proposed backend-header projection approved, the backend request header has that ID too. Visitors/backends cannot override the daemon's response ID. Requests rejected before reaching a component need no record there; inspector delivery remains best-effort, not lossless.

## 2. Verified baseline behavior and gaps

All source references are repository-relative at baseline `43f2bc0`.

| Verified seam | Actual behavior / consequence |
| --- | --- |
| `crates/tnld/src/serve.rs:123-135` | Public tunnel requests use the router fallback. Health/control/admin/identity routes bypass it; pre-handler HTTP parse errors also bypass handler ID generation. The guarantee is data-plane handler ingress, not every daemon socket byte. |
| `crates/tnld/src/data_plane.rs:51-56` | The daemon already mints `Ulid::new()` before Host/registry/session checks. Retain this one ID; never replace it with a header value or the client/session ID. |
| `data_plane.rs:29-48,233-245` | Hop filtering includes every field named by `Connection`. Ordinary incoming `X-Tnl-Request-Id`, forwarding headers, and internal-looking headers are otherwise passed through. There is no trusted request metadata producer yet. |
| `data_plane.rs:77-222,249-306,329-349` | Missing Host → 400/registry; no tunnel → 404/registry; grace/missing handle → 503/daemon with `Retry-After: 1`; stream open/handshake → 502/transport. Send errors distinguish upstream parse/incomplete responses from tunnel I/O. Generated bodies and headers already share the daemon ID via `ErrorContext`. |
| `data_plane.rs:224-230,313-326` | A parsed tunnel response is relayed after hop filtering without installing the daemon's ID. Backend/client-synth IDs can survive. Success tracing uses the daemon ID; the spawned connection-driver diagnostic omits it. |
| `crates/tnl/src/forwarder.rs:180-202` | A second independent ULID is generated per accepted stream. HTTP-head probing occurs before backend connect; no metadata is consumed. Complete, capped, EOF, and timed-out probes currently all proceed with their buffers despite comments suggesting incomplete probes are fatal. |
| `crates/tnl/src/peek.rs:13-65` | Head probe is 8 KiB, with a five-second caller deadline, and separates complete heads from leftover body bytes. The envelope will be read before this existing probe; request-ID availability must not depend on complete HTTP headers. |
| `forwarder.rs:208-244,462-512`; `crates/tnl/src/synth.rs:13-68` | Local connect/first-response failures synthesize HTTP 502 with the client ID consistently in body/header/log. Attribution remains `client` with origin-failure/target/tunnel fields. The daemon cannot distinguish this from a backend-supplied HTTP response. |
| `forwarder.rs:420-457,523-529`; `crates/tnl/src/inspector.rs:9-25,65-105` | Active inspector `LogLine` already has `req_id: Ulid`. JSON serializes it; text omits it. Success/synth paths emit records; some `?`/cancellation paths do not, and `try_send` can drop records. No guaranteed one-record-per-stream delivery exists. |
| `crates/tnl-protocol/src/messages.rs:49-59`; `src/lib.rs` | The different re-exported protocol `LogLine` has no ID and has `remote_ip`; it is not the active inspector path. Leave it alone. |
| `crates/tnl/src/client.rs:31-69,181-195` | The existing authenticated `/control` WS handshake discards the HTTP response. The accept loop spawns `forward` with cloned `ForwardCtx`. This is where visitor-IP's negotiated mode must reach the per-stream consumer; IDs themselves are not per-tunnel context. |
| `crates/tnld/src/control.rs:15-23,54-59`; `serve.rs:20` | Upgrade handler starts `run_session`; session handle currently holds only the session mutex. Visitor-IP owns adding capability echo and immutable selected format, without changing the fragile WS bridge or transport roles. |
| `crates/tnl/src/reconnect.rs:129-144`; `src/commands/http.rs:15-30` | Production CLI constructs the context on each connection/reattach and supplies the live inspector channel. Raw-mode warning/session mode must be integrated once through that path, not independently cached by request-ids. |
| `crates/tnl-protocol/src/error_page.rs:106-120,184-224` | `ErrorContext.req_id` is already a ULID. Error JSON uses `request_id`, text uses `Request:`, HTML includes the ID. Preserve those names and types. |
| `crates/tnl/src/host_header.rs:40-85`; `deploy/Caddyfile` | Host rewriting is independent of identity; Caddy config does not mint/normalize IDs. Do not depend on a proxy ID, HTTP header order, or whole-head UTF-8 decoding for the envelope ID. |

### Tests read and existing gaps

- `crates/tnld/tests/data_plane_route.rs` only checks that a 404 ID header exists; 404/503/negotiation tests do not compare it with downstream identity. `data_plane_logs.rs` accepts a substring, not a correlation match.
- `crates/tnl-e2e/tests/inspector_e2e.rs` checks method/path/status, not ID equality. `full_roundtrip_local_down.rs` checks attribution/negotiation, not daemon/header/body/inspector equality.
- `crates/tnl/tests/synth_response.rs` already accepts a supplied deterministic ULID. Existing direct-forwarder tests use raw HTTP and no envelope; those should **remain raw fixtures by default**, matching sibling `ForwardCtx::new` semantics, rather than be mechanically converted to ID headers.
- `peek_request_head.rs` covers complete/capped/EOF probes; no new ID header parser belongs there. `progress_format.rs` is only a no-panic smoke test.
- Preserve `full_roundtrip_response_framing.rs` (chunked, leading 103, honest upstream attribution), `full_roundtrip_many_headers.rs` (response headers), Host modes, target IPv6, and reconnect regressions. Add large-request correlation independent of the existing 8 KiB probe; do not change the cap to make that test pass.

## 3. Approaches and disposition

1. **Selected: negotiated bounded envelope owned by visitor-IP.** One u32-length-prefixed typed JSON payload precedes HTTP on each metadata-mode data stream. ID and visitor identity are available before head probing. Legacy raw bytes stay raw when negotiation is absent. Common versioning, bounds, errors, negotiation, and state must have one owner.
2. **Superseded: daemon-stamped internal HTTP header as ID source.** Even sanitization leaves header-order/cap ambiguity and unsafe interpretation with old daemons. Public `X-Tnl-Request-Id` may remain a backend-visible projection but is never the client's source of authority. Remove the prior header-extraction helper, private-prefix scaffold, complete-head requirement, and 512 KiB cap proposal.
3. **Not selected: mandatory unnegotiated preamble/coordinated breaking upgrade.** It would force mixed peers to fail. No sniffing and no second framing path are authorized; the user's combined deployment opportunity is not approval to break all raw compatibility.

## 4. Remaining product decisions and contract-freeze gate

Do not reopen the selected envelope direction. Keep these narrower questions visibly open until approved:

| ID | Remaining decision | Recommendation / constraint |
| --- | --- | --- |
| P1 | Public backend request-ID projection | Keep `X-Tnl-Request-Id` as a backend-visible header, replaced by the daemon after request hop filtering from its same local ID. It is a projection, not a trusted tunnel input. Final public response normalization is already selected; allowing a separate visitor-supplied authoritative ID is not an option. |
| P2 | Exact ID value validation | Proposed mandatory `request_id: ulid::Ulid`, JSON string of 26 canonical uppercase ULID characters, nonzero; no default, missing/null/duplicate/invalid values reject metadata mode. Do not silently downgrade invalid negotiated metadata to raw/local identity. Exact serializer/validator is frozen with the common owner, not a second codec. |
| P3 | Unrelated reserved-header/attribution scope | No private HTTP metadata headers are needed for the envelope. Do not reserve/strip all `x-tnl-*` or introduce `x-tnl-meta-*` ownership by implication. Broader header sanitization and authenticated `X-Tnl-Component`/origin attribution remain open separate scope; backend-forged attribution is not solved by a shared ID. |
| P4 | Inspector text / legacy wording | Propose full `req_id=<ID>` suffix on existing non-quiet rows, JSON key `req_id` unchanged. Agree ordering with visitor-IP's `ip=... source=...` suffix. Raw-mode IDs are local-only; document and warn once per reconnect run through the common raw-mode warning, not duplicate warnings from each lane. Showing IDs only in verbose mode remains an unapproved alternative. |

**Common-owner freeze dependencies:** sibling proposals for the exact token/header, length/timeout, offer/echo validation, `DataStreamProtocol`, unknown-field behavior, and raw-mode warning must be frozen once by visitor-IP with this lane's `request_id` addition. Visitor-IP's trust CIDRs, XFF algorithm, IP provenance/output details, and Caddy proof remain that lane's open decisions; they are not approved or implemented here.

The shared v1 **design contract must include `request_id` before release**. Visitor-IP owns freezing and implementing common framing/negotiation first; request-ids then adds the agreed field/validation and updates callers sequentially against that frozen design. A temporary visitor-only v1 implementation branch is not a separately supported released wire format. If visitor-only v1 has already shipped/frozen as an external protocol, stop for the common owner's versioning decision; do not silently make its missing field mandatory under the same released token.

## 5. Reconciled exact interface and wire contracts

### 5.1 Common contract inherited from visitor-IP (owner: visitor-IP)

This lane adopts, rather than independently redefines, sibling §§5.2–5.3:

- Module: `crates/tnl-protocol/src/request_metadata.rs`, exported from `lib.rs`.
- `DataStreamProtocol::{RawHttp1, Http1MetadataV1}`, with `RawHttp1` default.
- Control-handshake header `x-tnl-data-stream-protocol`; matching token `http1-metadata-v1`. This is an authenticated `/control` capability, not a public data-request header.
- Exactly one matching client offer selects metadata-v1; missing/duplicate/unsupported offers select raw without an echo. Exactly one matching 101 echo selects metadata-v1 on the client; missing echo selects raw; duplicate/invalid echoes fail the connection. No sniffing, downgrade-on-payload-error, or per-request format choice.
- Carry mode immutably with the session handle and `ConnectedSession`, then into `ForwardCtx` through reconnect and direct session consumers. `ForwardCtx::new` stays raw by default. Create/reattach negotiate on every new connection; never cache a format by tunnel reservation.
- For metadata mode: one u32 big-endian payload length `1..=1024`, exactly that many UTF-8 JSON bytes, then HTTP request bytes on the same substream. Whole read/write timeout: five seconds. Writer flushes before handing the stream to hyper; reader consumes exactly the payload, leaving HTTP/body bytes untouched.
- `read_request_metadata` / `write_request_metadata` take the sibling's `AsyncRead`/`AsyncWrite + Unpin + ?Sized` seam and return the common `RequestMetadataError`. Reject invalid length before allocation; validate malformed/truncated/duplicate-recognized-field payloads. Unknown object fields may be tolerated as the common owner specifies, but cannot replace required typed members.
- Writer is before daemon `TokioIo`/hyper handshake; reader is before client `peek_request_head`. No response envelope; no metadata on the control substream itself or again after an eventual WebSocket 101. Metadata bytes do not count as HTTP inspector byte counts.

### 5.2 This lane's proposed additive field to that single frozen contract

Retain the sibling's `VisitorIdentity` and provenance enum unchanged. Extend its same type, not a new request-ID envelope:

```rust
// In the visitor-IP-owned request_metadata module; derives per common contract.
pub struct RequestMetadataV1 {
    pub visitor: VisitorIdentity,
    pub request_id: ulid::Ulid,
}
```

Proposed combined JSON (object member order is not a protocol guarantee):

```json
{"visitor":{"visitor_ip":"198.51.100.7","peer_ip":"127.0.0.1","source":"x_forwarded_for"},"request_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV"}
```

Proposed ID contract under P2: required canonical 26-character uppercase string; nonzero; reject missing/null, duplicate `request_id`, bad alphabet, overflow, wrong length/type, lowercase/noncanonical values, or nil. The **common codec** validates on read and write. Use the existing ULID/serde dependencies; if serde's default accepts noncanonical forms, the common module's field serializer/deserializer must enforce the frozen rule. No whitespace trimming for a JSON ID value, no HTTP-header parser, no separate request-ID frame/error taxonomy.

Once the full envelope has decoded successfully, use its typed `request_id` throughout the client. If any required member (including visitor identity) is invalid, treat the envelope as undecodable: close that substream before backend connect, don't salvage a partial ID/visitor object, don't mint a negotiated-mode fallback, and don't retry as raw HTTP. Diagnostic correlation cannot claim a trusted decoded ID from malformed bytes. Daemon metadata-write errors keep the common owner's `request-metadata-write` transport attribution and the daemon's local ID in the generated 502 body/header.

### 5.3 Daemon authority and HTTP projections (request-ids owner)

- Keep the existing daemon mint at data-plane handler entry, before Host/session checks and eventual access-control denials. Visitor-IP independently resolves identity once; the request-ids lane supplies **that same local ID** in `RequestMetadataV1.request_id` when the negotiated producer serializes visitor identity. No new ID for metadata encoding, retries, or error responses.
- Under P1, after normal request hop-by-hop filtering, replace **all** `X-Tnl-Request-Id` values with one canonical value from the local daemon ID in both raw and metadata modes. `Connection: X-Tnl-Request-Id` cannot remove it because normalization is after hop filtering. This header is for backend correlation, not client authority. Leave `X-Request-Id` and `traceparent` as unrelated application data; neither feeds shared identity.
- In metadata mode the client obtains the ID **only from the decoded envelope**. It does not inspect, validate, or trust the HTTP `X-Tnl-Request-Id` for identity and does not require it to appear within the head probe. The daemon's common producer supplies the same ID to envelope and backend header; normal e2e equality proves that projection. This lane needs no full-head rewrite on the client.
- Every final public response produced by the updated handler gets exactly one `X-Tnl-Request-Id` from its local daemon context **after** response hop filtering. Replace, not append; never adopt an upstream/client ID. Cover ordinary backend success/4xx/5xx, client synth, and daemon errors. Backend duplicate IDs or `Connection` nominations cannot win. No body buffering or generic JSON rewriting.
- Negotiated client synth `SynthInput.req_id`, active inspector `LogLine.req_id`, and generated error `ErrorContext.req_id` remain ULID fields. Preserve error JSON `request_id`, inspector JSON `req_id`, HTML footer and text `Request:`. A backend's own body property named `request_id` is application data, not tnl-owned content.
- Keep existing failure classification: registry/daemon early errors, transport failures, client synth local failures, neutral upstream parse errors. Sharing an ID does not prove whether a relayed component/origin header came from the client or backend.

### 5.4 Legacy raw behavior: explicit non-guarantees

`RawHttp1` is the common owner's negotiated absence-of-capability mode, **not** recovery from a bad negotiated envelope.

- New client in raw mode retains a per-stream local `Ulid::new()` for its own existing inspector/synth contracts. It never copies `X-Tnl-Request-Id`, any internal-looking header, `X-Request-Id`, XFF, or body text into that local ID. Local raw IDs are not claimed to be the daemon's ID.
- An updated daemon still mints its own ID and normalizes its final response header even for raw clients. Under P1 it also projects that ID to the backend request. It cannot repair a raw client's independently generated synth body/inspector ID without new compatibility work; do not rewrite old arbitrary error bodies or advertise full correlation.
- Old daemon + new client: raw HTTP continues; any visitor-supplied ID header the old daemon relays remains untrusted application data. It neither selects mode nor becomes client identity. Client synth/inspector use a local ID, visitor identity is unavailable as specified by visitor-IP, and old daemon response headers are not retrospectively authoritative.
- Combine legacy notification with visitor-IP's single stderr warning per reconnect run. Proposed wording: `legacy raw HTTP mode: visitor identity unavailable; request IDs are local and not shared with the daemon`. Wording/presentation is P4, but truthful limitations are mandatory. No new persistent per-request provenance field or duplicate warning channel is imposed here.

| Daemon | Client/session | Correlation guarantee |
| --- | --- | --- |
| Updated with combined contract | Updated, metadata-v1 selected | Full daemon/envelope/client synth/inspector/final-header equality; backend equality once P1 approved. |
| Updated | Old client or raw selected | Daemon response ID authoritative; raw synth body and client inspector remain independent. No full shared-ID guarantee. |
| Old daemon / missing echo | Updated client, raw | Forwarding compatible; local client IDs only; visitor headers never authoritative. Old daemon's ID/header guarantees remain old behavior. |
| Any peer claiming metadata-v1 | Invalid/truncated/missing required envelope members | Reject that stream; no raw fallback, no backend connection, no invented shared identity. Other streams/session stay usable. |

A stripped capability header is a visible legacy downgrade, not permission to trust HTTP headers. Authentication/configured daemon trust remains the protocol boundary; this is not cryptographic protection against a malicious configured daemon.

### 5.5 HTTP cap, logs, and unchanged interfaces

- **Keep `REQUEST_HEAD_PROBE_MAX = 8 * 1024` and the existing HTTP-probe timeout/behavior unchanged in this lane.** No `request_id_from_head`, enlarged cap, all-head UTF-8 decode, or new reject-on-capped-head policy. Envelope ID is already available before any Complete/Capped/EOF/Timeout result; propagate it to the existing synth/log paths when those paths execute. General incomplete-head cleanup requires separate approval.
- `ForwardCtx` gains only the common owner's immutable session mode, not a request-ID cache. `run_accept_loop` and `forward` signatures need no request-ID parameter; each stream supplies its envelope ID. Raw default test contexts remain valid.
- Preserve active inspector JSON schema for `req_id`, independently of visitor-IP's additive visitor fields. P4 governs text suffix ordering. Quiet/verbosity/NO_COLOR, exit codes, CLI flags, TOML, and environment variables stay unchanged by request-ids. Leave dormant protocol `LogLine` untouched.
- Add daemon early-error and connection-driver diagnostics with the local ID; add client diagnostics with the decoded envelope ID (or explicitly local raw ID). Compose with visitor-IP's request span rather than entering guards across awaits. Do not print arbitrary rejected metadata/header bytes or invent an ID for an undecodable envelope.
- Preserve best-effort channel delivery and existing missing records on some broken/canceled streams. After response headers have been sent, never emit a second HTTP response to report a body failure; keep the original authoritative header and correlate available diagnostics.

## 6. Bounded scope, ownership, and sequential integration

Included: request-ID member added to the jointly frozen envelope; daemon-to-client ID propagation; daemon-owned final response header; proposed backend ID projection; existing synth/error/inspector/diagnostic correlation; tests for authority, negotiated failures, and honest legacy behavior.

Excluded: independent codec/negotiation implementation, visitor-IP trust/resolution/config/headers, extra preamble/magic sniffing/version token, HTTP cap changes, arbitrary backend body rewriting, general provenance authentication for component headers, access enforcement, full inspection storage/UI, replay implementation, WebSocket frames/upgrades, control/admin route ID middleware, new deployment/config/CLI knobs, guaranteed logging, commits/deployment. Root checkout `AGENTS.md` and other lane files are not writable by this task.

### Ownership and integration order

| Owner | Files / responsibility |
| --- | --- |
| Visitor-IP common owner, lands first | `crates/tnl-protocol/src/request_metadata.rs`, `src/lib.rs`; `crates/tnld/src/control.rs`, session-handle/state portions of `serve.rs`; `crates/tnl/src/client.rs`, mode plumbing in `reconnect.rs`/`ForwardCtx`; common codec, negotiation, timeout/error policy and mode tests. Freeze the combined v1 shape before external release. |
| Request-ids, sequential extension after common base | Add agreed `request_id` to the **same** `RequestMetadataV1`, ID field validation/tests, and all affected common metadata literals. Coordinate this shared-module edit with its owner; no parallel re-creation of the file or independently selectable codec. |
| Request-ids producer/finalizer | `crates/tnld/src/data_plane.rs`: keep single mint, supply envelope field at common writer seam, optional approved backend projection, final-header normalization on every return path, ID diagnostic fields. High overlap with access-controls and WebSockets; review all final response branches after combined integration. |
| Request-ids consumer | `crates/tnl/src/forwarder.rs`: select envelope ID in metadata mode vs independent local ID in raw mode; thread through existing synth/log/phase-2 paths. Do not duplicate the envelope read already added by visitor-IP; retain its decoded object and read once. No change to `peek.rs` for ID extraction. |
| Shared presentation owner agreed before edit | `crates/tnl/src/inspector.rs`, `tests/progress_format.rs`: agreed ID suffix plus visitor suffix; unchanged inspector ID JSON key. Request-ids supplies ID assertions, not a competing formatting refactor. |
| Request-ids dedicated tests | New `crates/tnl/tests/forwarder_request_ids.rs`, `crates/tnld/tests/data_plane_request_ids.rs`, `crates/tnl-e2e/tests/request_ids.rs`; focused correlation additions to existing inspector/local-down/log tests. Common framing/security tests remain visitor-IP-owned with ID cases added sequentially. |
| Fixture ownership | Add deterministic `request_id` to all common `RequestMetadataV1` literals, including visitor-IP tests, through a single integration writer. Session-backed e2e callers copy common negotiated mode; standalone raw `ForwardCtx::new` tests remain raw and need no fake ID header. |

**Sequence:** (1) common owner freezes one combined envelope/negotiation contract; (2) visitor-IP lands its shared base and plumbing; (3) request-ids adds the agreed ID field, validation, literals, producer/consumer behavior and response normalization; (4) reconcile presentation and other lane changes; (5) run combined tests/review; (6) release-validation owns the eventual single deployment/test opportunity. Do not ship or claim v1 completion between steps 2 and 3. Any shared-code handoff is serialized, not two writers independently resolving the same hot file.

Replay seam: a replay re-entering daemon ingress receives a fresh daemon ID; captured HTTP headers cannot choose it. A local-only replay has no negotiated daemon envelope and needs a separate provenance/ID policy owned by replay, not a fabricated shared ID. Future WebSocket handshakes use the opening request ID; no per-frame IDs or additional envelope after 101.

## 7. Numbered implementation subtasks (only after remaining approvals)

1. **Freeze and sequence the shared contract.** Visitor-IP owner records the selected single envelope and request-ID field in its common v1 design. Confirm P1–P4, shared-module writer order, exact token/length/timeout rules, and scope of remaining raw warning. If already-released visitor-only v1 conflicts, stop for owner versioning rather than silently breaking it.
2. **Extend the existing common schema and tests.** After visitor-IP base lands, add mandatory `request_id: Ulid` under the approved grammar to the same type/codec, plus deterministic fixtures. Add missing/null/duplicate/noncanonical/nil and write-validation cases to common source-named tests. Retain its 1024-byte bound, exact read boundary, error enum, and unknown-field policy. No new module/codec or HTTP-header parser.
3. **Supply one daemon ID and finalize HTTP headers.** Populate the common writer from the existing mint, compose with visitor resolution, and stamp approved backend/public response projections after respective hop stripping. Include pre-stream errors and metadata-write failure in ID correlation. Test behavior before changing diagnostics, so header authority is measured independently.
4. **Consume once and branch honestly by negotiated mode.** Reuse the common reader's decoded `request_id` before the unchanged HTTP probe; no second envelope read. In metadata mode use it for synth, phase-2 and inspector paths; never mint a fallback. In raw mode generate only a local per-stream ID and never inspect a visitor ID header. Preserve the existing HTTP cap, raw fixtures, body leftovers, counts, Host modes and pump. Test conflicting HTTP IDs and malformed envelope behavior.
5. **Integrate diagnostics/presentation once.** Add ID fields to applicable daemon/client diagnostics and the approved inspector suffix in coordination with visitor-IP/inspection. Common raw warning truthfully covers local IDs and unavailable visitor identity once per run. Assert actual text and JSON rather than just no-panic output. No general new log-delivery semantics.
6. **Prove real-session equality and compatibility.** Add §8 cases: metadata round trip, synth/error bodies, authoritative header under spoofing/Connection nominations, large request beyond head cap, raw peers/headers, reattach mode transitions, and bad metadata with another good stream. Capture tracing with existing writer pattern and wait for completed body/logs with finite timeouts.
7. **Review the combined tree and document limitations.** Run focused and workspace gates only under implementation/test authorization. Review common owner diff plus this extension, all response/denial/upgrade returns, and every metadata constructor/caller. Update user-facing contracts with negotiated-only full correlation and raw limitations. No merge/deployment by this lane; release-validation retains the combined release step.

## 8. Acceptance cases: semantic and adversarial

| ID | Case | Required result |
| --- | --- | --- |
| I1 | Real metadata-v1 daemon/client/backend success; concurrent same-path requests | Canonical nonzero ID in envelope = inspector `req_id` = daemon diagnostic = final response ID; backend header equality under P1. One header value, distinct IDs per ingress, no cross-request contamination. Visitor identity uses the same single envelope. |
| I2 | Client local connect refused/timeout, EOF, malformed prefix, first-byte timeout | Existing client attribution/kinds preserved; negotiated ID equal in synth header, JSON/HTML/text body, inspector record, daemon event/final response. Deterministic local fixtures, no external blackhole. |
| I3 | Missing Host, unknown host, grace/missing session handle, fake open failure, metadata-write failure, upstream parse error | Existing status/component behavior and 503 Retry-After preserved; daemon ID equals generated header/body and available diagnostic. No client/backend record required for unreached components; invalid HTTP rejected before handler remains excluded. |
| I4 | Backend application 4xx/5xx and forged duplicate response ID with Connection nominations | Backend status/body preserved, final response contains exactly one daemon ID after hop filtering. Do not add a false tnl component claim to ordinary application errors; do not rewrite backend JSON properties. |
| I5 | Visitor absent/forged canonical/invalid/nil/duplicate/comma/mixed-case `X-Tnl-Request-Id`; conflicting `X-Request-Id`, traceparent | None selects authoritative identity. Under P1 daemon replaces every public request-ID value after hop filtering. Decoded envelope, client synth/inspector, final header use daemon ID regardless of HTTP values; unrelated application headers remain unrelated. |
| I6 | Visitor/body contains capability header or bytes resembling framed metadata; payload/header IDs conflict in direct-stream fixture | Only the authenticated negotiated session mode chooses framing. In metadata mode envelope ID wins for client identity; in raw mode no sniffing/header promotion. To test a direct conflict, use a failing backend so synth/body/inspector must take envelope ID; header projection is separately proved through the real daemon producer. |
| I7 | Metadata missing/null/duplicate/bad type/invalid alphabet/overflow/noncanonical/nil ID; valid visitor but invalid ID, valid ID but invalid visitor | Whole envelope fails before backend connect; no fallback local shared ID, no HTTP interpretation, no partial identity salvage or raw header diagnostic leak. Daemon error response keeps its local ID; bad stream does not poison a separate good stream. |
| I8 | Fragmented length/JSON/ID; payload followed immediately by HTTP head+body in same write; 0/1025/u32::MAX lengths, truncation, whole-op timeout | Common owner codec rejects overbound lengths before allocation, reads exactly one payload and no HTTP bytes, uses one five-second operation bound, preserves subsequent stream bytes. ID extension fits the same 1024-byte budget; do not enlarge the budget just for the field. |
| I9 | Request head >8 KiB with daemon-stamped ID located after old probe boundary; POST buffered leftovers plus later streamed bytes | Shared identity remains available from envelope before probing. Keep head cap and baseline HTTP forwarding behavior; no complete-head requirement added to establish ID. Application payload/body byte-for-byte intact, including body lines that resemble request-ID headers. |
| I10 | Raw mode through new/old combinations, including attacker ID headers | Client local inspector/synth ID never equals a chosen visitor ID by adoption; no envelope bytes reach old clients. Updated daemon final header authoritative but raw synth body/inspector may differ, explicitly documented/tested as lack of full guarantee. Old daemon/new client still forwards raw and never treats passed-through ID as authority. |
| I11 | Valid/absent/invalid offers/echoes, raw and metadata sessions concurrently, reattach with capability transition | Common owner's deterministic mode policy maintained; reattach uses new connection mode, no stale format/ID cache, no second preamble. Legacy warning once per run, no duplicate ID/visitor warnings. Public data headers cannot change control negotiation. |
| I12 | Inspector Text/Json/Quiet, success/failure, visitor suffix, legacy records | Existing `req_id` JSON type/name unchanged, proposed full text ID appears in agreed suffix order, visitor fields unchanged, quiet suppresses rows; raw limitation made truthful in documentation/common warning. No promise that banner output is pure NDJSON. |
| I13 | Chunked/close-delimited responses, leading 103, malformed HTTP/2, Host preserve/rewrite/auto, IPv6 backend, many response headers, truncated body | Existing streaming/framing/Host/target behavior preserved; upstream parse attribution stays neutral upstream. Mid-body failure keeps initial response ID and correlates available diagnostics, never emits a second response. Best-effort inspector omissions are not disguised as passing delivery guarantees. |
| I14 | Backend supplies forged component/origin headers | ID cannot be overridden, but test/document that existing in-band component attribution is not authenticated by the envelope request ID. Do not claim P3's broader attribution problem has been solved. |

Common framing/negotiation negative tests should be extended in the visitor-IP-owned test targets, not duplicated with different semantics. Request-ID tests assert the additional member and identity equality. Direct raw tests remain valuable compatibility evidence; don't convert all fixtures to metadata and lose the fallback matrix.

### Proposed owned tests and validation commands

- Common owner: extend in-file `request_metadata` tests and `control_metadata_negotiation`, `forwarder_request_metadata`, and visitor-IP e2e fixture literals with deterministic IDs after that owner lands its base.
- Request-ids: new `crates/tnl/tests/forwarder_request_ids.rs`, `crates/tnld/tests/data_plane_request_ids.rs`, `crates/tnl-e2e/tests/request_ids.rs`; focused equality/diagnostic assertions in `synth_response.rs`, `progress_format.rs`, daemon route/404/503/log tests, `inspector_e2e.rs` and `full_roundtrip_local_down.rs`.
- Do not add an HTTP-head ID extraction test target or change `peek.rs` solely for this lane. Test cap independence through the combined producer/consumer path.

Future commands only (new targets do not yet exist). Require installed Rust/dependencies and authorized local socket tests; none was run in this design revision:

```sh
cargo test --offline -p tnl-protocol request_metadata
cargo test --offline -p tnld --test control_metadata_negotiation --test data_plane_request_ids --test data_plane_route --test data_plane_404_no_tunnel --test data_plane_503_no_session --test data_plane_logs
cargo test --offline -p tnl --test forwarder_request_metadata --test forwarder_request_ids --test synth_response --test progress_format --test peek_request_head
cargo test --offline -p tnl-e2e --test request_ids --test visitor_ip_roundtrip --test inspector_e2e --test full_roundtrip_local_down --test full_roundtrip_response_framing --test full_roundtrip_many_headers --test host_rewrite --test reconnect --test full_roundtrip_ipv6
cargo fmt --all
cargo clippy --offline --workspace --all-targets -- -D warnings
cargo test --offline --workspace
git diff --check
git diff --cached --name-only
```

All repository-mandated fmt/clippy/workspace gates must be clean before an eventual commit. `--offline` only prevents dependency fetching; tests may still open local sockets and need explicit execution authorization. Actual Caddy capability-echo/topology proof remains visitor-IP/release-validation owned; no Caddy config/version/network/credential work in this lane or this revision.

## 9. Integration risks and completion gates

- **Contract version risk:** making an ID mandatory after a visitor-only v1 release would break peers despite the same token. Freeze the combined schema first and complete sequential integration before v1 release; otherwise seek the common owner's version decision.
- **Two reads/two preambles:** the highest implementation overlap risk is independently adding another metadata read/write. Visitor-IP's decoded object must be retained and extended; request-ids reads its member, not another frame. Mode/negotiation/session state has one owner.
- **Legacy downgrade:** raw compatibility is forwarding compatibility, not full identity equality. A new daemon can own its response header while a raw client's error body still reports its local ID. Do not hide that mismatch with an insecure header fallback or generic body rewrite.
- **HTTP cap independence:** envelope parsing solves ID availability before incomplete/capped head probes; it does not fix those probes' pre-existing semantics. Don't expand this lane into general HTTP parsing cleanup or raise caps.
- **Producer consistency:** the same local daemon value must populate envelope, proposed backend header, generated error contexts, tracing, and final header. Separate minting in any of those branches defeats equality even with perfect framing.
- **Source trust/attribution:** configured authenticated daemon remains trusted. Shared ID does not authenticate backend component/origin headers or make visitor/IP data an authorization input. Keep P3 separate.
- **Hot-file merges:** data-plane access denials/WebSocket handshakes can bypass finalization; inspector/replay code can independently mint or relabel IDs. Review the combined tree, not just isolated request-ID changes.
- **Lifecycle:** daemon success events precede full body completion while client logs follow pump completion. Tests must consume bodies and wait finitely. Keep channel-drop/loss behavior honest; no guaranteed terminal row on every transport failure.

**Ready to implement:** selected architecture retained; common owner freezes combined v1 and serial edit order; P1–P4 and any common wire-detail approvals resolved. **Ready to integrate/release:** ID semantic/security matrix and common owner tests pass on the reconciled tree, mandatory workspace gates pass, independent reviewer approves, and no unapproved staging/deployment occurs. Release-validation owns the single combined deployment/test opportunity.

## 10. Exact revision delta and actual validation

Changes from the previous request-ID specification:

1. Marked the user's negotiated single-envelope direction selected; the internal-header-source recommendation is superseded.
2. Adopted visitor-IP's common `DataStreamProtocol`, bounded codec and handshake seams, with mandatory proposed `request_id` added to the same `RequestMetadataV1` alongside `visitor` before release.
3. Removed request-ID-from-HTTP extraction, its helper/namespace scaffold, complete-head validation requirement, and 512 KiB head-cap increase. Preserved the existing 8 KiB probe behavior.
4. Kept daemon mint/final-header authority; separated optional-approved backend header projection from the envelope's trusted ID source.
5. Replaced strict rejection of all legacy missing-header traffic with explicit negotiated raw mode: independent local client IDs, no header authority, no full shared-ID promise, combined common warning.
6. Assigned codec/negotiation/session plumbing to visitor-IP, ID extension to sequential follow-on integration, and froze a pre-release coordination gate rather than creating a competing preamble.
7. Reworked subtasks/tests/commands for envelope validation, raw compatibility, cap independence, mode transitions and joint fixture ownership. Kept unrelated output/reserved-attribution/value-format decisions open.

Actual checks in this revision: worktree/branch/HEAD and staging status inspection; reads of both plans, local AGENTS and relevant handshake/forwarder/probe source; planning-skill reads; artifact consistency/read-only Git checks. The worktree plan existed as an **untracked file before this revision**; do not claim an initially fully clean worktree. No source/tests/commits/deployments/network/credential access or child agents. The root-checkout AGENTS edit was not opened for modification or changed. **Tests/builds run: none.** Proposed cargo commands are not claimed results.
