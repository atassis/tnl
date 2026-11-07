# Per-run visitor access controls — replacement reconciliation specification

**Status:** Phase1 committed in `4a451fc`; phase2 is an incomplete, fail-closed wire/client/admission-primitives checkpoint over the reviewed WS merge. Executable protection is not implemented. See §13 for the exact current boundary.
**Supersedes:** The daemon-profile proposal in `plans/access-controls.md` and its repository copy. Those files are historical, not implementation direction.
**Inspected worktree / branch / baseline:** `/home/atassis/.config/superpowers/worktrees/tnl/access-controls` / `feat/roadmap-access-controls` / `43f2bc0ed29e00821e1ede624b8fe190eb0d5c57`.
**Current checkpoint:** Phase2 adds protected control message shapes, exact-ACK client helpers, shared-loop protected retry decisions and bounded daemon admission primitives. The daemon rejects protected creation/reattachment with `AccessUnavailable`; no protected descriptor can be published yet. No worker staging, commits, children or deployment changes.

## 1. Settled direction and changes from the previous design

The user selected **client-supplied, per-run rules**, not server-managed profiles. All protection is optional. A normal `tnl http 3000` or `tnl http 3000 demo` remains public, requires no visitor password/allowlist/rate/concurrency configuration, and acquires no visitor-auth work capacity. Random, temporary and later-reused hostnames remain a normal demo workflow. An unguessable URL is not authentication.

Each control can be used independently: Basic alone, allowlist alone, rate alone, concurrency alone, or combinations. Basic **does not require rate flags**. Rules belong to a tunnel instance/run, not permanently to a hostname or client account. No daemon profile file, profile selection/ownership step, mandatory global request quota, automatic policy selection, or automatic security upgrade is introduced.

Changed contracts:

* Replace `--access-policy NAME` and daemon profile TOML with per-run CLI controls and a bounded policy sent over the existing client-bearer-authenticated WSS control connection.
* Read a Basic password from a protected file or hidden terminal prompt; hash once locally; send only a bounded verifier. Never accept a plaintext password in argv, environment or a visitor policy/config dump.
* Reuse the **existing authenticated WebSocket metadata capability negotiation** for visitor identity/request IDs. Future distinct protected control variants acknowledge policy only; they must not negotiate a second data protocol or stream prefix. A public metadata-capable tunnel still has no visitor protection unless requested.
* Require positive acknowledgment of the exact canonical policy via its fingerprint. Protected runs never downgrade to old/public creation. Reattach retains immutable policy and limiter state; recreation reuses the same policy/verifier, not fresh password input.
* Consume/remove gate Authorization on the daemon before data reaches the client, backend, metadata/capture or raw inspector memory. When Basic is not enabled, preserve existing application Authorization passthrough, even if other access controls are enabled.
* Integrate with the selected **single authenticated loopback browser inspector** and explicit replay: raw storage is opt-in bounded memory; replay goes directly to the fixed local backend, not to a public hostname or through the daemon gate. The earlier suggestion that replay must traverse public admission is withdrawn.
* Hold a concurrency lease for the entire HTTP exchange and full WebSocket relay lifetime, not only through response headers or 101.

Names, numeric safety bounds and canonicalization below are concrete implementation proposals for lane reconciliation, not a new product-choice questionnaire. User-selected optional/per-run behavior is no longer open.

## 2. Verified baseline and source seams

Read `AGENTS.md`; the earlier session consulted brainstorming/writing-plans skills, and their design boundaries continue to apply. No skill instruction to commit, execute or spawn overrides this assignment. Re-read the relevant source during reconciliation; original roadmap prose is not treated as implementation evidence.

| Current implementation | Consequence for this change |
| --- | --- |
| `crates/tnl/src/main.rs:47-86,197-235`, `commands/http.rs:5-35`: HTTP CLI exposes target/subdomain/host-header/logging, then loads client config and runs reconnect. | Add per-run controls here; do not extend persistent client config or require them for public runs. |
| `crates/tnl/src/config.rs:6-118`: endpoint and client bearer only; `commands/config.rs` masks that bearer. | Visitor passwords/verifiers must not be persisted alongside bearer credentials or appear in config-show. |
| `crates/tnl-protocol/src/messages.rs:9-56,88-92`: internally tagged CreateTunnel/ReattachTunnel/TunnelCreated, with no negotiated data protocol/access. Existing structs ignore unknown fields. | Merely adding optional policy fields to CreateTunnel can open a public tunnel on an old server. Use distinct protected lifecycle variants for exact policy acknowledgment; keep legacy schema and existing WS metadata negotiation intact. |
| `crates/tnld/src/control.rs:70-190`: length-prefixed JSON, 1 MiB server frame limit, `debug!(?msg, ...)`, and a debug-rendered unexpected-message error. | Verifiers now cross this path. Redact their types and replace full-message/payload error logging with safe structural events before enabling the new variants. |
| `crates/tnl/src/client.rs:31-95,97-192`: bearer WS dial, unbounded response-length allocation, public create/random/reattach helpers. | Add the shared negotiated path; bound acknowledgment frames too. Require certificate-verified WSS before sending a protected policy. Preserve old public helper signatures. |
| `crates/tnl/src/reconnect.rs:37-99,170-174`: grace reattach and same-subdomain recreation; mostly string-based error classification. | Retain one in-memory policy/verifier through every branch; use typed policy/protocol failures and typed TunnelLost for the new path rather than broad error-string matching. |
| `crates/tnld/src/registry.rs:20-47,87-149,151-245`: immutable Arc<Tunnel>, separate mutable session binding, grace/GC and creator-token-name checks. | Attach access and negotiated data protocol at construction. Store quotas with the tunnel ID, not session/hostname. No new profile owner concept. |
| `crates/tnld/src/data_plane.rs:51-186`: Host lookup then session handle/open_stream, with no visitor controls. | One admission call belongs after tunnel lookup and before stream open or visitor upgrade. |
| `data_plane.rs:188-243,311-336`: hyper connection task and streamed response body. Authorization is not otherwise stripped by hop-header removal. | Body/connection must jointly retain the exchange lease. Basic gate consumption must be conditional, not blanket Authorization removal. |
| `serve.rs:109-135`: ConnectInfo is installed; API routes precede the fallback on every Host. No typed visitor resolver is consumed by the baseline data plane. | Consume visitor-IP lane's typed resolver result. Do not parse XFF here. Gate covers backend forwarding, not reserved daemon API routes. |
| `data_plane.rs:15-48,218-241`: strips Upgrade; visitor WS relay is absent. `deploy/Caddyfile:21-25` forces data-plane Connection close. | WebSockets lane owns transport/proxy changes; access gate and lease must be shared by HTTP and WS. `/control` WS remains bearer-authenticated and unrelated to visitor Basic. |
| `crates/tnld/src/auth.rs:136-149`: salted default Argon2id; bearer verification is synchronous elsewhere. `tnld hash-password` currently takes plaintext argv. | Use the established Argon2 parameters locally in the client, but not server TokenStore or blocking-on-executor verification. Existing daemon hash CLI need not change for this per-run feature. |
| `crates/tnld/src/pair.rs:12-14,49-60`: governor 0.7 already exists for pairing. | Reuse a direct per-tunnel governor limiter only when the user requests a rate; do not reuse pairing keys/state. |
| `data_plane.rs:339-366`, `tnl-protocol/src/error_page.rs:12-45,143-207`: negotiated no-store errors, component/request ID, optional Retry-After; no Access component. | Add Access rendering and use the request-ids lane's canonical ID. |
| `crates/tnl/src/forwarder.rs:62-129`, `inspector.rs:1-34`: current per-run ForwardCtx and forwarder-local terminal events, not a browser inspector. | Metadata/envelope, browser capture and replay are other lanes' changes; enforce the sanitization boundary before they receive traffic. |

Actual callers/tests inspected in the original investigation include public control creation, direct registry creation, daemon routing/503/error negotiation/logging, fail-closed empty bearer store, reattach and e2e reconnect/create helpers. Re-read `tnl-e2e/tests/wss_dial.rs`: it only asserts that a TLS-capable dial does not report “TLS support not compiled in”; its self-signed TLS failure does **not** prove a trusted WSS control roundtrip. Preserve those baseline tests and add the security cases below.

At reconciliation start, the worktree contained an existing untracked `docs/plans/roadmap-access-controls.md` with the old proposal. It is left untouched. No existing implementation changes were found; branch/HEAD match baseline.

## 3. Approach and bounded scope

**Recommended implementation:** one optional typed per-run access policy; locally salted Basic verifier; shared negotiated control acknowledgment; daemon-owned immutable admission state. Reuse serde/clap/Argon2/governor/TOML conventions, but add no policy TOML at all. This serves the selected demo workflow without operator provisioning.

Two rejected alternatives explain the boundary: retaining daemon profiles contradicts the selected self-service per-run workflow; proxy-only Basic/IP/limits do not follow tunnel instances and can be bypassed by direct daemon reachability. Neither should be implemented alongside the selected approach.

**In scope:** CLI input, verifier lifecycle, bounded wire schema/validation/fingerprint, registry/control binding, Basic/IP/rate/concurrency admission, conditional header consumption, denial responses and targeted tests. **Cross-lane contracts only:** typed identity, request-ID/metadata envelope, WS lease handoff, authenticated browser inspector/raw memory/replay sanitation. **Out:** permanent hostname/account ACLs, daemon policy CRUD/storage, visitor credential persistence, OAuth/cookies/multiple Basic users, path/method exceptions, per-IP/distributed quotas, global public traffic quotas, payload/message throttling, inspector implementation, replay implementation, proxy trust parsing, deployment and unrelated registry fixes.

## 4. Proposed per-run CLI and input lifecycle

```text
tnl http TARGET [SUBDOMAIN]
    [--basic-auth USER [--basic-auth-password-file PATH]]
    [--allow IP_OR_CIDR]...
    [--rate-limit REQUESTS_PER_SECOND [--rate-burst BURST]]
    [--max-concurrent N]
```

* No flags above: public, no policy and no auth/allow/rate/concurrency checks. Metadata negotiation is not a protection flag. Random subdomain remains selected by omitting SUBDOMAIN.
* `--basic-auth USER` enables Basic. With `--basic-auth-password-file`, read that file; otherwise use dialoguer's hidden terminal password prompt with confirmation. No terminal and no file: fail before dialing, never silently disable Basic or echo-read stdin. Password-file requires Basic; burst requires rate. Duplicate singleton flags are usage errors. No password/hash argv option, environment fallback, `USER:PASS` shorthand, implicit password file, or saved per-tunnel config.
* USER: 1–64 printable ASCII bytes excluding colon/space. Password: UTF-8, 1–1024 bytes, no control characters; colon and spaces are allowed. Prompt confirmation is local only. Do not echo rejected secret values in errors.
* File read: open once, inspect the opened descriptor, require a regular file, retain the metadata size check as an early rejection, then read at most 1027 bytes (1024 plus optional final LF/CRLF and one overflow byte). Reject actual lengths above 1026 **before** removing a final line ending, so stale/underreported metadata cannot silently truncate input. Remove only one final line ending and never trim spaces. On Unix refuse any group/other permission bits; 0600 or 0400 are suitable. Check the opened target even if PATH is a symlink; do not claim path/directory/ACL/ownership checks that are not implemented. The user must trust the path/directory. On other systems document that equivalent restrictive ACLs are the user's responsibility; do not claim Unix mode enforcement there. Never create/copy a password file automatically.
* `--allow` accepts repeated IP literals or CIDRs, no comma syntax/hostnames/ports/zones. Omitted means no allowlist. Present entries are ORed; independently enabled controls are ANDed. Reject an empty/missing value at argument parsing.
* Rate N and burst are positive u32 values 1–10,000; omitted burst defaults to N. The wire always sends both values when a rate is selected. `--max-concurrent` is integer 1–10,000. Omitted rate/concurrency are truly absent, not zero/default quotas. Basic does not imply either limit.
* Validate endpoint/options before reading a password; any protected run uses an `https://` endpoint and certificate-verified authenticated WSS. No insecure protected-mode flag or HTTP fallback. Public legacy HTTP behavior remains unchanged. With a TLS-terminating reverse proxy, WSS ends at the trusted edge and the existing local daemon hop is the deployment trust boundary; do not trust a visitor-supplied forwarding header as proof of TLS.
* Hash once locally using OS-random 16-byte salt, Argon2id v19, m=19456 KiB, t=2, p=1, output 32 bytes. Use `spawn_blocking`/pre-runtime work as appropriate rather than blocking an active async request loop. Do not change server bearer hash semantics.
* Keep plaintext in a zeroizing buffer for the shortest input/hash interval; do not retain it in Clap structs, async reconnect hooks or inspector state. Drop it after hashing. Keep one private redacted verifier/policy Arc in process memory for the run. Retain the **same salt/hash and canonical fingerprint** on reattach/recreation; do not reread the file, re-prompt or rehash. Editing the file during the run does not rotate the gate. To rotate, stop and start a new run with new input.
* No disk/session cache of verifier or fingerprint. Clear owned verifier buffers on final drop where possible; document that zeroization is best-effort against copies, allocator history, core dumps and process compromise, not a secrecy guarantee. Verifier is sensitive/offline-guessable even though it is not the password and must not itself be accepted as an HTTP Basic password.
* A successful banner may list enabled control kinds, network ranges and numeric quotas after validated acknowledgment; never print password/PHC, a copyable secret policy JSON, or the password file contents. Existing plain public output remains public.

## 5. Proposed policy schema, bounds and exact acknowledgment

Define shared types in new `crates/tnl-protocol/src/access.rs`, reexported in `lib.rs`:

```text
VisitorAccessPolicyV1 {
  version: u16,                         // exactly 1
  basic: Option<BasicVerifier>,
  allow_ips: Vec<String>,              // [] = no allowlist
  rate_limit: Option<VisitorRateLimit>,
  max_concurrent: Option<u32>
}
BasicVerifier { username: String, password_hash: SecretVerifier }
VisitorRateLimit { requests_per_second: u32, burst: u32 }
```

All five fields are present on the wire, in that canonical order; nulls are explicit. Access-specific objects reject unknown/duplicate fields; do not ignore unknown protections. Use a presence-checking deserializer for required nullable fields: deriving an `Option<T>` alone allows omission in serde and is insufficient. An absent policy uses the unchanged public lifecycle; protected create requires a present, non-null `access` policy. A non-null policy must enable at least one control; reject an all-empty policy rather than ambiguously treating it as public. `allow_ips: []` within a non-null Basic/rate/concurrency policy means no network restriction, not deny-all. No user-facing deny-all shorthand is proposed.

Validation is independent on client and daemon. An authenticated client is allowed to request rules, not arbitrary CPU/memory cost. Bound serialized policy to 16 KiB and at most 128 CIDRs; reject before registry publication. Preserve the existing 1 MiB outer control-frame maximum and apply it to client response reads before allocation as well. Do not serialize full policy in validation errors.

Normalize allowed addresses with ipnet, sharing the visitor-IP lane's canonical address-family treatment: bare IPv4/IPv6 become /32 or /128, mask CIDR host bits, use canonical text, sort strings by UTF-8 bytes and deduplicate. Normalize IPv4-mapped IPv6 addresses to IPv4; mapped prefixes /96–/128 become IPv4 /(prefix−96), and reject mapped-v6 prefixes shorter than /96 rather than inventing cross-family meaning. Prefix/network parsing is not a replacement for the trusted resolver.

Require a PHC string at most 256 ASCII bytes, Argon2id v19 with **exactly** m=19456,t=2,p=1, 16 decoded salt bytes and 32 decoded output bytes, no extra parameters. Reject Argon2i/bcrypt, missing values, oversized parameters/strings and malformed encodings. Client uses the established default cost; daemon parses/bounds without performing a test hash at creation. Never accept “any PHC cost” from the client. USER/password input bounds apply as above; daemon can validate USER but cannot infer plaintext password length from a verifier.

`SecretVerifier` serializes only for the private control path, has explicit redacted Debug and zeroizing owned storage, and must not be a generally serializable inspector/status DTO. Policy/control Debug must not disclose username/verifier. Do not rely on a developer remembering to omit a derived Debug at one call site.

**Exact acknowledgment without echoing the verifier:** compute `policy_fingerprint` as lowercase hex SHA-256 over `b"tnl.visitor-access.v1\0" || canonical_policy_json_bytes`. Canonical JSON is compact serde_json serialization of the normalized named structs in the field order above (Basic fields username then password_hash; rate fields requests_per_second then burst), with explicit nulls, no skipped fields, no whitespace, and the exact submitted, validated PHC string. Do not use JSON map iteration, platform-native integer encoding, Debug output, or a digest of only the enabled-rule names. Pin canonical byte/digest test vectors, including null fields, escaped ASCII username and deduplicated CIDRs. Client and daemon compute independently from normalized typed values. A differently salted hash is intentionally a different policy.

The server's accepted fingerprint identifies the whole policy including verifier and numbers. It is not a bearer capability and need not be logged/exposed in admin/inspector UI. ACK carries fingerprint rather than PHC. Validation may canonicalize CIDR representation but must not weaken/drop controls. A matching fingerprint must be acknowledged before success/forwarding starts.

## 6. Proposed protected wire lifecycle using existing WS metadata negotiation

**Lifecycle contract:** Protected message shapes and client helpers now exist, but daemon publication/enforcement are not integrated; protected create/reattach return `AccessUnavailable` at this checkpoint. Metadata already negotiates on the client-Bearer-authenticated WebSocket upgrade: the client offers `x-tnl-data-stream-protocol: http1-metadata-v1`, and the daemon echoes that exact capability. Reuse `DataStreamProtocol::from_echo` and the existing `Http1MetadataV1` data-substream envelope. There is no JSON `data_protocol` selector, additional metadata handshake, new stream prefix, or numeric control-protocol negotiation here.

Proposed concrete new internally tagged `ControlMsg` variants and named payloads:

```text
CreateProtectedTunnel(CreateProtectedTunnelReq {
  subdomain: String | null,             // omitted also means random
  access: VisitorAccessPolicyV1         // required, non-null, nonempty
})
ReattachProtectedTunnel(ReattachProtectedTunnelReq {
  tunnel_id: ULID,
  subdomain: String,
  policy_fingerprint: String            // required lowercase hex SHA-256
})
ProtectedTunnelCreated(ProtectedTunnelCreatedResp {
  tunnel_id: ULID,
  hostname: String,
  subdomain: String,
  policy_fingerprint: String            // independently computed from stored policy
})
```

These variants add **policy delivery and exact acknowledgment only**. Existing `CreateTunnel`/`ReattachTunnel`/`TunnelCreated` remain unchanged for public runs, including public metadata-capable runs. Only subdomain on protected create may be omitted; all other fields above are required and non-null. New access-specific payloads reject unknown and duplicate fields; reject malformed fingerprints (anything other than 64 lowercase hexadecimal characters) and unsupported policy versions. The policy's existing `version: 1` and the distinct variant names define this lifecycle; no redundant metadata-version field is added. Do not attach optional protection fields to legacy variants: an old daemon can silently ignore those fields, while it must reject an unknown protected variant before creating anything.

* Client authenticates the control connection with the existing client Bearer; that bearer is not a visitor credential. No profile lookup/permission database exists. Existing token-name creator ownership remains the registry principal convention.
* Protected mode requires HTTPS/certificate-verified WSS and the existing exact metadata capability echo **before sending a policy-bearing create or protected reattach**. A missing echo is still legacy raw mode for public callers, but is a terminal unsupported-capability error for protected runs. Invalid/ambiguous echoes are errors, not another offer. No successful banner or forwarder starts before the exact protected ACK is validated.
* Server validates policy/cost, compiles IPs, creates requested per-tunnel limiter/semaphore state and constructs the immutable descriptor using the already negotiated `DataStreamProtocol` **before any registry publication**. Failure leaves no visible public descriptor or hostname reservation. The protected handler requires the already negotiated metadata mode; it does not select a mode from JSON. ACK uses the fingerprint calculated from the stored policy, not a blindly reflected request string.
* Client requires `ProtectedTunnelCreated` and the exact expected fingerprint before returning `ConnectedSession` or printing success. On reattach also require exact tunnel ID and subdomain. Reject ordinary `TunnelCreated`, missing/changed/null fingerprint, weaker policy or malformed ACK. A malicious/misconfigured daemon remains trusted to enforce its acknowledgment; the client cannot cryptographically force server enforcement.
* Store immutable normalized policy/fingerprint and quota state on the tunnel Arc alongside its existing data-stream mode. Reattach checks ID/subdomain/creator/new session, the fingerprint and the WS-negotiated mode **before** mutation. Reuse original state; no policy recompile/reset or fresh Basic salt. Reattach sends fingerprint only, not plaintext/verifier, because the daemon retains the original policy.
* Legacy public reattach to a protected tunnel, protected reattach to public, differing fingerprints or a WS-negotiated mode mismatch fail without changing binding. Public metadata tunnels remain public; protected variants do not redefine metadata negotiation. Do not reinterpret an existing metadata tunnel as raw HTTP.
* Genuine tunnel disappearance/grace expiry yields typed `TunnelLost`; recreate using the same in-memory policy/verifier and existing WS metadata offer, requesting the same subdomain under existing conflict rules. New ID gets fresh quota state; same hostname does not imply same tunnel. Wrong owner/ID-subdomain/mode/policy mismatch is a terminal generic denial, not `TunnelLost` or a policy-replacement request, and must not mutate/downgrade an existing tunnel. An already-active binding is a conflict without recreation.
* Proposed explicit codes: `InvalidAccessPolicy`, `AccessPolicyMismatch`, `UnsupportedProtocol` (and existing `TunnelLost`/`Unauthorized`/subdomain errors). New-path client errors are typed: invalid input/ACK/capability/mismatch terminate; actual network failures may back off **only in the same protected mode**. A server EOF may be retried as transport failure but never converted to public creation. Handle `TunnelLost` structurally; do not string-search server message text to select recreation.
* Protected runs never fall back to legacy/public create or reattach, including after TLS failure, missing capability echo, EOF, old schema, unknown variant, invalid JSON, ACK mismatch or recreated-session failure. Existing optional metadata compatibility for public runs is unchanged and cannot be entered by a protected run.
* Replace full control-message logs and debug-rendered unexpected-message responses before enabling secret-bearing variants. Log variant/category, IDs and safe rule-presence/count metadata only. Decode errors report safe category/line/column, not raw serde text that could contain an offending string/PHC. Never log raw JSON or policy bytes. Redacted Debug also applies when a response unexpectedly contains a policy-bearing message.

Preserve existing public helper signatures as wrappers. A future shared `TunnelOptions`/`RunOptions` holds the optional Arc policy, never a password/file handle; the actual negotiated data-stream mode still comes from the existing WS upgrade and remains on `ConnectedSession`. Add `connect_and_create_with_options`, `connect_and_reattach_with_options` and `reconnect::run_with_options` around one shared loop, not duplicated public/protected loops. No server policy config fields or broad Config-literal churn are needed; the daemon auth-work semaphore can be an internal AppState resource.

## 7. Trusted metadata, header boundary and admission ordering

The negotiated data envelope carries **request ID and typed resolved visitor identity** before request HTTP bytes. Request-ids/envelope lanes own framing, parsing, byte limits, version negotiation and the one canonical request ID. Visitor-IP owns the resolver result/provenance and fail-closed proxy-chain algorithm. Access consumes that result, not raw Forwarded/XFF/Host-derived identities. The envelope serializes only a bounded public projection such as canonical IP plus resolver source/unavailable state, never raw proxy headers, Basic username/password/verifier, client bearer or policy body. Metadata is not a visitor-supplied HTTP header and cannot be overridden by one.

The important shared order is:

1. Obtain canonical request ID and resolver result; route/find the tunnel as today. Unknown tunnel remains current 404.
2. If no policy, perform no access admission and preserve application Authorization. Otherwise check configured allowlist using canonical typed identity; unavailable identity denies only if allowlist is enabled. Rate/Basic/concurrency-only policies do not require an IP.
3. If an explicit rate is configured, check the tunnel's aggregate limiter. Then parse/verify Basic only if configured. Reject before stream open and before consuming request body. Controls apply to every backend-bound method/path, including OPTIONS/HEAD and WebSocket handshakes; no hidden health/preflight exemption.
4. When Basic succeeds, remove **all** Authorization values from the request before envelope/HTTP serialization. Reject duplicates before verification; no copy of gate Authorization is placed in metadata or capture. This consumes the gate credential, so an application Bearer cannot simultaneously occupy that same header. Do not invent a secondary credential header.
5. Acquire a configured concurrency slot with try-acquire; no queue. Admission yields one cloneable RAII lease. Locate session/open stream only now; every failure path releases its ownership.
6. Write the negotiated metadata envelope from trusted context, then HTTP bytes without consumed gate Authorization. HTTP connection/body or WS relay owns the lease as below. Client validates envelope before backend connect or capture; malformed/missing envelope must not be treated as legacy HTTP on a negotiated stream.

**Passthrough invariant:** when Basic is absent, do not remove/rewrite Authorization just because allowlist/rate/concurrency/metadata/raw-capture support exists. It remains application data on public and non-Basic-protected requests, subject only to pre-existing HTTP hop-header rules. Inspection's ordinary redaction may hide it in UI; redaction is not a reason to modify backend traffic. Opt-in raw memory storage may hold application Authorization under the inspector's separate warning/consent model; it must never hold the consumed gate header.

### Basic parser and bounded verification

Exactly one Authorization header, maximum 2048 bytes before decoding; case-insensitive Basic scheme, one token, standard padded Base64, UTF-8 credentials split at first colon. Apply USER/password input limits above. Missing/wrong/malformed Basic, duplicated Authorization, Bearer supplied to a Basic gate, or wrong username all return the same generic 401/challenge without forwarding. For well-formed credentials, verify against the stored hash even for an unknown username, then combine username/password results; do not expose a username-specific message. Parse errors need no hash. No plaintext-success/password cache.

**Necessary internal safety proposal, not a product traffic limit:** one process-wide semaphore of four active visitor Argon2 verification workers, only touched by Basic-enabled requests. Acquire with try-acquire before spawn_blocking; move that permit into the closure, together with a zeroizing decoded-password buffer. Cancellation must not free a worker slot while its hash still runs. Saturation returns bounded 503/Retry-After:1 without queuing an unbounded blocking job. Fixed PHC cost limits per-worker memory to about 19 MiB plus overhead; four is roughly 76 MiB plus other process memory. Document/tune this implementation safety constant through review, not by requiring visitor rate CLI flags or limiting unprotected traffic. This is distinct from the optional user concurrency cap on upstream exchanges.

**Internal anti-abuse budget evaluated:** a hidden per-IP/per-tunnel failed-auth request-rate budget is not recommended for the first slice. It introduces another product-observable lockout/fairness policy and keyed state, and would be misleading if called “no rate limit.” Worker capacity and cost bounds are required resource safety, but do not prevent online password guessing/CPU monopolization. Document this residual risk, recommend strong passwords and optionally `--rate-limit`, and test saturation isolation. Any later failed-attempt budget must have explicit documented semantics and approval; never silently couple Basic to mandatory rate flags or apply it to public requests.

### Explicit optional limits and leases

Use governor 0.7 direct limiter, per tunnel, only when rate is requested. Initial burst as configured, continuous N/second refill. One token for each request reaching the rate check, including later malformed/wrong Basic and concurrency rejection; allowlist rejects consume none. No per-IP map and no global visitor rate quota. Reattach preserves state; recreated ID starts fresh. Retry-After rounds wait to the next token upward, minimum one second. Concurrent requests must not over-admit burst.

`AdmissionLease` is a cloneable internal handle sharing one optional OwnedSemaphorePermit; final owner drop releases once. With no explicit concurrency limit, it has no upstream permit. With a limit, hold it from before stream open through upstream request upload, streamed response and final connection/body termination. Share clones between hyper connection driver and a trailer-preserving response-body wrapper so neither an early response nor received response headers release a live exchange. Release on EOF/error/drop/cancellation, with no orphaned detached ownership. On WS transfer a clone to the relay task **before returning 101**; 101-body drop must not free a live connection. Hold through full relay close/error/abort, including bidirectional cleanup. WS messages do not individually consume request tokens. No session/DashMap guard may be held while hashing or waiting on task IO; no semaphore admission queue.

## 8. Denials and inspector/replay contract

Add `Component::Access`, `X-Tnl-Component: access`, neutral title and JSON `error: "access_denied"`, retaining negotiated text/HTML/JSON, no-store, HEAD behavior and canonical request-ID response header. Proposed stable mapping:

| Cause | Status / kind | Other headers |
| --- | --- | --- |
| Missing/malformed/wrong Basic | 401 / `basic-auth-required` | `WWW-Authenticate: Basic realm="tnl", charset="UTF-8"` |
| Canonical IP not allowed | 403 / `ip-not-allowed` | none |
| Explicit request-rate budget exhausted | 429 / `rate-limited` | rounded positive Retry-After |
| Explicit upstream concurrency exhausted | 429 / `concurrency-limited` | Retry-After: 1 |
| Required resolver identity unavailable, internal hash capacity saturated, admission internal failure | 503 / `access-unavailable` | Retry-After: 1 |

Use safe generic bodies, never usernames/verifiers/raw headers/query strings. No denial opens a stream, reaches local backend or returns 101. Emit one sanitized daemon denial event with ID/tunnel/method/path without query/status/kind; no entire policy Debug. Preserve existing request counter meaning unless its owning lane separately changes it. Daemon denials cannot appear as actual forwarded exchanges in the client inspector; do not open a stream just to make capture show them.

**Selected browser inspector boundary (not implemented by access lane):** one authenticated loopback browser inspector per selected inspection design; explicit replay only, opt-in bounded raw **memory** retention, no gate verifier/password in inspector state/endpoints/UI. No second access-specific UI/token store. Ordinary sanitized display and raw retention both receive a request **after** daemon gate-Authorization consumption. Gate Authorization is never recoverable from raw memory, including for WS handshakes. Do not add it back from client run options, metadata or replay settings.

**Selected replay boundary:** replay is an explicit local owner operation against the fixed configured local backend, not visitor traffic through the public hostname. It does not ask for gate Basic, does not re-open the public control path and does not consume daemon rate/concurrency state. It cannot claim a new trusted remote visitor identity or reuse a visitor request ID as a new live request; replay IDs/linkage follow that lane's design. Replays cannot select an arbitrary URL/Host target, follow redirects to arbitrary destinations or obtain verifier data from access options. Local inspector authentication/CSRF/origin defenses, raw consent, body bounds and replay target/redirect enforcement belong to inspection/replay owners. Access supplies the consumed-header invariant and tests it at that seam rather than taking over those lanes.

Reserved daemon paths (`/healthz`, `/control`, `/whoami`, `/pair*`, `/tunnels*`) still precede backend fallback across hosts. Visitor gate protects backend forwarding, not a blanket all-URLs virtual-host login. Preserve existing separate bearer/public API behavior. Public Basic at the TLS edge and WSS verifier delivery require a trusted edge-to-daemon hop; no deployment change is authorized here.

## 9. Temporary hostnames, compatibility and honest security guarantees

A policy protects one live tunnel ID and its grace retention/reconnect, not a domain forever. Stop, GC expiry or daemon restart removes that reservation; later runs may reuse the same subdomain publicly or with a different policy. No permanent owner/profile mapping is added. The reconnect loop requests the same policy for its own recreation, but cannot prevent another permitted run taking a freed hostname first; report conflict rather than weakening or taking over a different policy. Recreate with a random or different hostname only according to existing/user-selected reconnect behavior, not as an access-specific surprise.

Browsers can cache Basic credentials per origin/realm; reusing a hostname may cause a browser to send previously cached credentials to its new occupant. Document the risk and advise closing/clearing credentials or using a fresh random hostname for sensitive demos. Random names are convenience, not access control or a promise against discovery. No gate can protect a backend that is separately exposed or protect data from an authorized inspector/local backend owner.

Unprotected Authorization must remain untouched by this feature. Basic-enabled policy intentionally prevents simultaneous application bearer auth in the same header; no cookie/query token bypass is proposed, and browser WS credential UX must fail closed if a browser cannot provide the Basic header. Bounded workers mitigate memory/queue abuse but not full denial-of-service or password guessing. Zeroization/file mode checks do not protect against a compromised local user/process, lax ACL/backups or TLS termination compromise.

Public legacy wire fixtures and helper signatures stay supported. New protected behavior requires the negotiated protocol and does not pretend an old daemon supports it. Public use of the new metadata envelope does not turn on any gate, password, quotas or raw memory storage. Do not add blanket server Config fields, defaults or onboarding requirements merely to implement per-run access.

## 10. File ownership, dependencies and implementation order

| Owner / proposed files | Responsibility and overlap |
| --- | --- |
| Access: new `crates/tnl-protocol/src/access.rs`; protocol `lib.rs` | Shared policy/redacted verifier types, normalization/fingerprint/canonical fixtures. Access owns the proposed protected policy/ACK variants in `messages.rs`; reuse the envelope lane's existing authenticated WS capability negotiation without JSON metadata selectors. |
| Access: new `crates/tnl/src/access.rs`; `main.rs`, `commands/http.rs` | CLI parsing, password file/prompt, local hashing/zeroization, per-run options. No client config/password persistence. |
| Shared envelope/client: `tnl/src/client.rs`, `reconnect.rs`, `forwarder.rs` | One negotiated options/ACK path, bounded reads, protected-only reconnect/recreate, stream decoding. Access owns policy retention/non-downgrade checks; request-ids/envelope owner owns metadata format and ID propagation. Keep existing public wrappers. |
| Access: new `crates/tnld/src/access.rs`; `tnld/src/lib.rs` | Bounded PHC/policy compile, Basic parser, optional limiter/semaphore, hash-worker resource and exchange lease/body wrapper. No raw forwarding-header parser or inspector server. |
| Access: `tnld/src/registry.rs`, `control.rs`, `serve.rs` | Atomic-with-publication immutable access/data-protocol attachment, lifecycle/mismatch checks, safe control logging, internal worker semaphore in AppState. No daemon profiles or new access Config section. Preserve WS bridge/yamux role mapping/channel-drop shutdown. |
| Joint data-plane seam: `tnld/src/data_plane.rs` | Access owns admission, conditional Authorization removal and HTTP leases. Visitor-IP owns typed resolver; request-ids/envelope owns ID/envelope write; WS owns upgrade/relay and receives lease. Agree a single handler order and one writer when integrating. WS owns proxy Connection-close fix. |
| Access: `tnl-protocol/src/error_page.rs`, its render tests; `tnl/src/exit.rs` if necessary | Access component/kinds and typed new-path exit mapping to existing server error 70; keep bearer rejection distinct. Do not globally refactor unrelated string-based errors. |
| Shared manifests: root `Cargo.toml`, `Cargo.lock`, affected crate manifests | Propose workspace ipnet 2, base64 0.22, sha2 0.10, zeroize 1, http-body 1, with member workspace usage. Add existing Argon2 workspace dependency to client; protocol needs ipnet/sha2/zeroizing verifier type, server Base64/body. Reuse governor 0.7; centralize version if touched, do not upgrade gratuitously. Coordinate ipnet/lock updates with visitor-IP. |
| Access docs: `README.md`, `docs/RUNBOOK.md` | Approved CLI, optional/public default, file/TLS/reconnect/raw boundary, Basic/app auth limitation, temporary/reused hostname caveats. No edits in this design run. |
| Inspection/replay owners | Authenticated loopback browser, raw memory opt-in, fixed-backend explicit replay. Consume sanitized requests; no new access-owned UI/replay subsystem. |
| Release-validation owner | One later combined test/deploy after reconciled designs and acceptance; no lane deploys independently. |

Dependency order: finalize protected policy-ACK/fingerprint seam against existing WS metadata negotiation → shared schema and redaction → client input and daemon compile/admission unit work → immutable control/registry → negotiated client/reconnect → HTTP admission with resolver/request IDs → WS lease + browser/replay sanitation integration → offline security validation. No operator policy-provisioning prerequisite exists.

### Numbered bounded implementation tasks (future; no execution authorized now)

1. **Common wire and redaction.** Use the protected lifecycle shape in §6 and retain existing authenticated WS capability negotiation; add policy types/canonicalization/digest and legacy-schema fixtures. Test required/null/unknown/duplicate fields, exact digest vectors and Debug/error redaction. Prove old CreateTunnel ignores added fields but old decoder rejects the new variant; do not implement optional-policy-on-legacy. Run `cargo test -p tnl-protocol`.
2. **Client per-run input.** Add access CLI/file/prompt/local hashing types and new `crates/tnl/tests/cli_access_controls.rs`; verify public no-input/no-quota behavior, all independent controls, flag dependency/invalid bounds, protected endpoint rejection before reading secrets, restrictive Unix file modes/regular files/newline preservation/oversize input, hidden prompt via injectable input tests, zero secret output. Run `cargo test -p tnl --test cli_access_controls` and `cargo test -p tnl --lib access`. Keep daemon hash CLI/client config untouched.
3. **Daemon bounded primitives.** Add access.rs policy/PHC compile, Basic verification and optional rate/concurrency unit tests; deterministic governor fake clock, controlled blocking-worker barriers and lease/body-frame tests. Reject bad/oversized verifiers before Argon work; test Basic-alone, allow-alone, rate-alone and concurrency-alone, and worker isolation from public requests. Run `cargo test -p tnld --lib access`.
4. **Immutable control lifecycle.** Add `crates/tnld/tests/control_access_controls.rs` around real control decoding/registry. Verify no publication on invalid policy, exact computed ACK, random/protected creation, mode/fingerprint mismatch without mutation, same-state grace reattach, new state only on new ID, owner checks, and no secret logs under malformed payload/Debug/unexpected-message paths. Run `cargo test -p tnld --test control_access_controls` and existing `cargo test -p tnld --test reattach`.
5. **Client negotiated/reconnect safety.** Extend common options helpers/shared reconnect loop; add `crates/tnl/tests/client_access_controls.rs` using local mock control peers/shared below-dial helpers. Require exact policy fingerprint/ID and the existing WS metadata capability echo; test old peer EOF, ordinary ACK, missing/weaker policy ACK, unsupported metadata and typed errors. Trace outgoing messages to assert no protected branch ever sends public create; assert retained PHC/fingerprint across retry/recreate even if the input file changes/deletes. Run `cargo test -p tnl --test client_access_controls`; retain public CLI/exit-code tests.
6. **HTTP admission and trusted envelope boundary.** Jointly integrate data_plane.rs; add `crates/tnld/tests/data_plane_access.rs` using a counted fake Session to assert `open_stream == 0` for every denial. Combine a real backend with streaming/trailers/cancel/failure cases. Assert Basic credentials never enter envelope/forwarded request, while public and allow/rate/concurrency-only application Authorization passes through unchanged. Use resolver-produced extensions, not a test-only XFF parser. Run `cargo test -p tnld --test data_plane_access`, `cargo test -p tnl-protocol --test error_page_render` and existing negotiation tests.
7. **Joint live integration.** Add `crates/tnl-e2e/tests/access_controls.rs`; coordinate `access_controls_websocket.rs` and capture/replay assertions with respective owners. Prove full-lifetime WS slots, canonical identity/envelope IDs, explicit local replay without restored gate headers and opt-in raw-memory hygiene. For verifier delivery use a local trusted-TLS fixture with an explicitly trusted test CA through a supported test connector; never disable certificate checks or add a production insecure flag. Low-level protocol tests may use in-memory/already-established test streams, but must not be misreported as trusted-WSS CLI coverage. Existing `wss_dial` is insufficient. Run `cargo test -p tnl-e2e --test access_controls` and `cargo test -p tnl-e2e --test access_controls_websocket` after dependencies land.
8. **Docs and independent security review.** Document settled optional/reused-domain semantics, input/limits/safety distinctions and local replay boundary; run full checks below and explicitly cover ignored grace expiry. Review policy logging/serialization paths, immutable state, typed identity, all reconnect branches and every lease clone owner. No deployment in this task.

## 11. Concrete acceptance cases and commands

Each denied public-visitor path must assert HTTP response semantics **and zero session stream opens**; a backend counter alone is insufficient. These are future requirements, not tests claimed to have run.

* **A1 Public default:** plain named/random `tnl http` uses no password prompt/file, policy, admission limiter, concurrency cap or auth-worker permit; old create/reattach fixtures remain valid. Metadata alone does not enable protection or raw storage. Public application Authorization is byte/value-preserved subject only to existing hop-header processing.
* **A2 Independent controls:** Basic-only works without rate/concurrency/IP flags; allow-only/rate-only/concurrency-only work without Basic. Every combination ANDs controls; omitted ones perform no enforcement. An explicitly all-empty policy is rejected. Unprotected and non-Basic-protected application Authorization is not consumed.
* **A3 Input/secrets:** no accepted plaintext argv/env shorthand; hidden prompt doesn't echo, nonterminal/no-file fails, password-file mode and descriptor/type/length checks work, spaces/colon and one final LF/CRLF are handled exactly. No plaintext/PHC in CLI/debug/errors/config/status/inspector; malformed payload errors do not expose offending values. Synthetic password and PHC sentinel tests scan captured logs, not just success output.
* **A4 Bounded hostile verifier:** reject overlong/bad/non-Argon2id/wrong-version/extra-parameter/high-memory/time/parallelism PHC without hashing or publishing any tunnel. Bound policy/CIDRs/control response lengths. Canonical vectors cover ordering/null/normalization; changing username/hash/range/rate/burst/concurrency changes the fingerprint. A server ACK cannot omit a requested control while retaining a matching digest.
* **A5 Basic boundary:** missing/wrong/Bearer/malformed/duplicate/oversized header returns generic 401 with challenge/no-store/request ID; correct credentials succeed and the **whole gate Authorization header is absent** before metadata serialization, client forwarder, backend, sanitized capture and opt-in raw storage. Colon-containing passwords work; error content does not distinguish usernames. OPTIONS/HEAD/WS have no bypass.
* **A6 Trusted allowlist:** bare IPv4/IPv6/CIDR edges, adjacent networks, canonical mapped addresses and mapped-prefix rejection behave exactly. Untrusted spoofed Forwarded/XFF cannot grant entry; trusted proxy chains follow the resolver. Unavailable identity fails closed only for a configured allowlist, not for Basic-only/public runs. Metadata reflects the same typed result used for admission, not re-parsed headers.
* **A7 Explicit rates:** initial burst, continuous refill, concurrent budget safety, positive rounded Retry-After; allowed-IP invalid Basic and later concurrency failures consume configured tokens, IP rejects consume none. Two tunnels have independent budgets; reattach does not reset, new ID does. Rate absent truly has no hidden request quota.
* **A8 Explicit concurrency:** N held exchanges pass/N+1 gets 429 before open. Headers, partial response bodies, trailers and early upstream response with continuing upload do not release prematurely. Open/handshake/send error, body EOF/error/client drop/request cancellation eventually permit a subsequent request, without retained detached clones or double release. No configured cap means no upstream permit acquisition.
* **A9 Necessary auth safety:** controlled blocked hash workers consume only bounded capacity; another Basic request gets bounded 503 without a queued job. Cancellation does not release a slot until the blocking hash ends. Lightweight/public traffic makes progress and does not acquire auth permits. This tests internal resource safety, not a mandatory public/product rate policy; online guessing remains a documented residual risk.
* **A10 Immutable/no downgrade:** server constructs access before publication and computes ACK from stored policy. Legacy/public/new-mode mismatches and wrong owner/ID/fingerprint do not mutate binding. Reattach retains policy/verifier/rate/semaphore counts; TunnelLost recreation uses the exact original policy even after password-file removal. Old peer/ordinary/weaker/malformed ACK or TLS failure never produces public create/active forwarder/success banner. Policy errors terminate rather than loop as transient errors.
* **A11 Temporary/reuse honesty:** stop/GC/restart clears the reservation under existing behavior; a subsequent independent public run can reuse a hostname. Reconnect cannot override a conflicting/new owner's tunnel or silently weaken its own policy. Docs warn about name discoverability, absence of permanent protection and cached browser credentials on reused origins.
* **A12 WS full lifetime:** failed auth/IP/rate/concurrency never opens/returns 101; successful configured gate forwards echo frames without gate Authorization. A live WS occupies its exchange slot until relay close/error/abort, not 101-body drop; failed upgrade/termination releases and a later handshake succeeds. WS messages incur no extra request tokens. Browser inability to send Basic fails closed, never query/cookie bypass.
* **A13 Inspector/replay:** one authenticated loopback UI per inspection contract, raw retention off unless explicitly enabled, no gate credentials even when raw memory is on. Explicit replay reaches only fixed local backend and never public daemon/visitor admission or an arbitrary target/redirect; gate credentials are not restored from run options. Public application Authorization forwarding remains correct. Replay linkage IDs are distinct from new live requests according to request-ids/replay contract. Inspector/replay owners supply their own authentication/CSRF/storage tests; no security claim from access-only tests.
* **A14 Response/transport compatibility:** negotiated text/HTML/JSON/HEAD/no-store/challenge/Retry-After/Access component and canonical IDs; baseline reserved-API auth/routing preserved. Protected verifier delivery is tested through certificate-verified WSS with trusted test CA, not only a TLS compilation-error smoke. Public legacy helper/config behavior stays supported.

Proposed final validation after implementation and cross-lane integration:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p tnld --test reattach reattach_after_grace -- --ignored --nocapture
git diff --check
git diff --cached --name-only
```

Run targeted tests in tasks 1–7 first. New test targets do not exist at baseline. Follow pinned Rust 1.94 and workspace dependency conventions. No toolchain install/network/build/runtime tests are permitted by this reconciliation phase. A full workspace pass alone neither exercises ignored grace expiry nor proves trusted IP/WS/browser/replay security.

## 12. Remaining reconciliation items, risks and evidence

No further choice between profiles/per-run, public/mandatory security, or direct-local/public replay is needed: the user selected those. Remaining work is narrowly technical:

1. **Protected lifecycle/envelope alignment:** use the concrete protected policy-ACK proposal in §6 with the already implemented authenticated WS metadata capability negotiation. Request-ids/visitor-IP owners retain framing, canonical request-ID and typed identity ownership. Do not add a competing JSON metadata selector or bypass admission on a WS/envelope early return. This is an interface reconciliation, not another product questionnaire.
2. **Safety review:** reviewer should ratify the fixed PHC bounds, four-worker memory budget, parser/file restrictions and canonical fingerprint vectors. Recommendation is no hidden failed-attempt rate budget in this slice. If one is demanded, its observable lockout/fairness semantics need explicit approval rather than silently adding mandatory rate behavior.
3. **Joint evidence dependency:** WS lease and authenticated loopback inspector/raw/replay tests belong to their owners and must land before combined acceptance. Add a real trusted TLS application fixture; existing wss_dial does not supply it. An access-only success cannot close these requirements.

Key risks: PHC leaking through Debug/serde errors; client file/input or verifier retained across the wrong lifecycle; shared handshake fallback; a gate header reaching raw capture; conditional Authorization stripping accidentally applied to all tunnels; premature/leaked HTTP/WS leases; forged identity instead of typed resolver context; implicit defaults becoming mandatory product limits; reused-origin browser credential caching. The specification addresses these with explicit ownership and A1–A14, not speculative extra subsystems.

**Required independent review gate:** inspect source and tests for exact ACK/non-downgrade, bounded/cancellation-safe hashing, public optionality/Authorization preservation, immutable per-ID state, trusted identity, downstream credential removal, HTTP/WS complete lease lifetime and direct-local replay boundary. Missing cross-lane coverage remains an integration blocker. Release-validation owns the later single combined test/deploy shot; this lane performs none.

**Evidence from this reconciliation:** branch/HEAD verified; relevant source and prior artifact read; only this replacement external artifact written. Existing untracked repository plan was present before work and left unchanged. Product tests/builds/deployment: **none run**. Read-only artifact and git/staging checks are reported in the final handoff; no implementation correctness is claimed from document inspection.


## 13. Phase2 fail-closed implementation checkpoint

This checkpoint is intentionally **not the complete access-controls feature**. It builds on phase1 `4a451fc` and the parent-started reviewed WS/S0 merge. The Cargo.toml working-tree conflict resolution retains the access dependencies, exact yamux 0.13.10 patch and vendor workspace exclusion; the inherited staged merge/index resolution remains parent-owned.

Implemented:

* The §6 `CreateProtectedTunnel`, `ReattachProtectedTunnel` and `ProtectedTunnelCreated` variants with strict access-specific payloads and required lowercase SHA-256 fingerprints. No metadata JSON selector or second negotiation exists.
* `client::TunnelOptions`, `connect_and_create_with_options`, `connect_and_reattach_with_options` and `reconnect::run_with_options`. Protected helpers require HTTPS and the existing exact WS capability echo before sending policy; exact fingerprint/reattach identity validation precedes returning a connected session. The shared loop only recreates a protected tunnel for typed `TunnelLost`, always with the same retained options. Legacy public wrappers remain intact.
* Client response reads reject frames above the existing 1-MiB bound before allocation. Client/server control decode diagnostics omit raw serde text, and control logs no longer Debug-render entire messages. Sensitive serialized/received control buffers use zeroizing storage.
* `tnld::access::CompiledAccess` independently compiles optional IP, direct aggregate rate, Basic and concurrency state. Its admission order is IP → rate → Basic → concurrency; Basic success alone consumes all gate Authorization values. Non-Basic admission preserves application Authorization.
* `visitor_ip_for_admission` consumes existing typed resolver results. Resolver fallback provenance (missing/invalid forwarding or all-trusted chains) is unavailable for an enabled visitor allowlist, rather than permission to admit the trusted proxy's address. No forwarding-header parser or duplicate request-ID generation was added.
* A four-worker process-wide visitor hash capacity, fixed-cost verification in `spawn_blocking`, no worker queue without a permit, and permit ownership inside the blocking closure so waiter cancellation cannot free active hash capacity.
* Cloneable `AdmissionLease` and trailer-preserving `LeasedBody`, tested for final-owner release, body EOF/error/drop and independent driver-owner lifetime.
* Fail-closed daemon control handling: valid protected create/reattach are explicitly rejected with `AccessUnavailable`, and a real control-session test asserts no registry descriptor or hostname publication.

Evidence boundary:

* A real certificate-verified WSS **mock control peer** uses a generated test CA and the TLS library's supported custom connector, never disabled certificate verification. It checks exact/wrong/public ACK, missing capability and oversized response behavior. This is protected client wire evidence, **not** a protected daemon/backend end-to-end test.
* Admission and lease unit tests do not prove complete HTTP upload/response or real WS relay ownership. The actual WS relay remains unchanged in this checkpoint.

Required next slice before enabling protection:

1. Bind immutable compiled access/fingerprint and already-negotiated mode into atomic registry publication; add owner/mode/policy/active-binding reattach checks with no mutation on mismatch and no quota reset.
2. Integrate admission before stream open/visitor 101, negotiated access denial responses (`Component::Access`, challenge/Retry-After/HEAD/no-store), conditional credential consumption and safe denial events. Preserve the request-ID lane's authoritative context when integrated.
3. Retain separate HTTP driver/body lease owners and transfer a lease into the WS rendezvous/relay before returning 101. Test actual streaming upload, trailers, cancellation, backend failure and full WS lifetime, including zero stream opens on denial.
4. Wire the executable's optional flags only after those server paths are complete and tested; preparation stays pre-runtime and creates one immutable Arc policy per run.
5. Extend the trusted-WSS fixture through the real daemon/enforcement path and exercise actual reconnect/recreation with retained verifier/options. Add deterministic refill/concurrent-burst and complete worker-saturation isolation tests.

Until these gates pass, do not replace the `AccessUnavailable` branch, advertise a protected tunnel, or treat this checkpoint as release-ready security enforcement.
