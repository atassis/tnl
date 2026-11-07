# T4 phase 1 checkpoint (not whole-T4 completion)

Baseline: `a9997be`, inspection worktree only. No staging, commit, deployment,
listener, browser routes, credential bootstrap or replay raw store is added.
The existing CLI does not yet expose inspection; production defaults remain off.
Tests opt in through `ForwardCtx.inspection` / process-scoped `Hooks.inspection`.

## Concrete seam for review/freeze

- `inspection::{CaptureId,TunnelGeneration,CaptureContext}` are shared client-only
  identities. CaptureId is not the metadata request ULID or a replay execution ID.
- `Inspection::admit(generation, Option<&RequestMetadataV1>)` obtains one of 64
  permits **before** allocating parser/preview scratch. No envelope parsing or
  canonical ID/log allocation is duplicated. Only the typed canonical request ID
  and safe visitor provenance label enter the display record; IPs do not.
- One `Capture` gets the effective head after Host rewrite, probe leftover,
  remaining upload fragments, response prefix and response pump fragments once.
  All callbacks are synchronous borrowed inputs before buffer reuse, with no
  event queue and no await. Intended writes are not delivery guarantees.
- `ConsumerFactory::admit(CaptureContext)` may decline independently; its one
  optional `ObservationConsumer` receives validated borrowed heads, transfer-
  decoded (not content-decoded) body fragments, message-end/trailer flags,
  stopped reasons and one Finished. Consumer-owned allocations/admission must
  remain independently bounded. Display omission/eviction does not stop this
  consumer. Replay must implement its own opt-in raw consumer and private store;
  there is no raw lookup, snapshot type, raw Debug/Serialize or view conversion.
- Head events admit at most eight informational heads **before callbacks** and
  at most 32 KiB cumulative response-head input (including the final head); display/final_status ignore
  100/103 and use the actual final response (including 101). 101 immediately
  publishes Upgraded, discards same-read opaque bytes and ignores later inputs
  in both directions. The consumer must stop raw retention upon that 101 head.
- Parser failures only stop observation; forwarding bytes and pump sequencing
  are unchanged. Duplicate Content-Length (including equal duplicates), TE/CL,
  unsupported transfer codings/chunk extensions and malformed/capped framing
  conservatively stop capture. Chunked entities are de-framed; trailers are not
  retained and are indicated to consumers. Trailer names must be nonempty HTTP
  tokens; values allow HTAB/SP/visible/obs-text octets, never NUL, DEL, other
  controls, bare newlines or obs-fold, under the existing line/aggregate bounds.
  Cancellation differs from graceful EOF for close-delimited bodies.
- MessageEnd is provisional framing completion: later unexpected bytes can
  revoke it with Stopped for that direction. Stopped is sticky revocation, not
  exactly-once: cancellation may repeat it with a different reason. Consumers
  must apply repeated stops idempotently and must not restore eligibility.
  Finished is emitted once, but need not immediately follow a 101. Consumer
  nonblocking/nonpanicking behavior is a trusted implementation obligation,
  not enforced isolation or permission for unbounded storage.
- Reconnect allocates a new generation on successful creation/recreation, keeps
  it on successful reattach, and passes it in the actual ForwardCtx literal.
  Connection attempts do not advance it. This is not yet a replay authorization
  or active-generation authority/facade.

## Implemented display bounds/policy

Memory-only history: 200 rows, oldest admission eviction including active rows,
15-minute admission TTL enforced on reads/mutations, no resurrection after
expiry/eviction. Each row reserves a conservative 64 KiB including field/vector
capacities (200 reservations < 16 MiB), instead of accounting only string lengths.
Heads <=8 KiB/direction and 64 fields; response informational input <=32 KiB/8
heads. Entity preview scratch <=4 KiB/direction; body previews default off.
Header names <=128 bytes, values <=1 KiB, text <=8 KiB/direction; method <=32 bytes,
request target <=2 KiB. Search is literal over sanitized fields, query <=256
bytes, at most 100 cloned matches. No persistent index or raw search is present.

Default header allowlist follows the main plan; other values are masked. All
query values are masked, including duplicates and empty values. Only origin-form
URI is displayed; malformed escapes/absolute/userinfo targets fail closed.
Controls are made visible and all truncation is UTF-8 safe. Complete JSON only,
no compressed/binary/text/partial fallback. Secret keys are recursively masked.
Both entity and sanitized output are capped at 4096 bytes; serialization uses a
bounded writer. A pre-parse structural budget (64 separators/containers, depth
32) additionally omits dense/deep JSON before allocating the serde tree. Arbitrary
path segments and explicitly allowed header values are not a DLP guarantee.

Opted-in summary/debug method/target values are sanitized before emission, and
channel-full errors no longer Debug-print dropped LogLine payloads. The unrelated
T4a status parser is not edited, and T2's canonical summary/synth ID work remains
separate. Independent bounded response-prefix status fallback preserves Host
Auto block detection when capture is off, admission is denied, or observer head
validation rejects a usable response (such as >64 fields). It skips complete
leading informational heads, never treats 100/103 as final, and does not UTF-8
decode entity bytes. The fallback remains limited to the existing 1024-byte
response prefix; Host Auto always uses that independent evidence, not a display
status learned beyond the prefix (tested with a signature in a long 103 head).
It does not introduce a second body capture or tee.

## Explicitly incomplete phase 2

1. Independent observer/resource/security review and freeze this consumer seam
   with replay owner; validate exact transient allocation accounting. Current
   history uses bounded synchronous mutex operations; browser/search contention
   and aggregate snapshot budget require review before service integration.
2. Complete display lifecycle/schema: pre-connect/client-synth captures, explicit
   transport failure/source, duration/successful-write counters, header omission
   states, active progress and terminal cancellation/upgrade coordination with
   WebSocket owner. Current records describe observation/framing, not delivery.
3. Process service-owned periodic expiry sweep (currently lazy expiry only),
   shutdown/fatal-service clearing and active-generation/replay revocation.
4. Authenticated IPv4-loopback listener, capability bootstrap, exact Host/Origin/
   Fetch-Metadata defenses, connection/handler/time/serialization budgets, safe
   read API, one UI and explicit replay confirmation adapters. No API is exposed
   early to bypass security. CLI opt-in/body/additive-redaction flags still absent.
5. Replay-owned separate raw-retention consent/storage, eligibility/private lookup,
   fixed-loopback engine, safe facade/outcomes and one-use confirmation semantics.
   Sanitized previews cannot be replayed and no placeholder raw store is present.
6. Expanded exhaustive parser/ambiguity/framing and concurrency/pressure coverage,
   combined metadata/WebSocket/replay e2e, browser security tests, documentation,
   independent review and full integration/release gates. No whole-T4/T5 or
   production acceptance is claimed.

Validation logs are in the worker's home `tnl-inspection-*.log`; build target is
`~/tnl-inspection-target`, jobs=1, dev/test debuginfo=0. Initial tests were authored
first but their dependency build overlapped implementation, so missing-feature
RED was **not** observed before initial coding (TDD deviation). Subsequent actual
behavioral RED-to-GREEN tests caught cancellation incorrectly completing a
close-delimited JSON response, excessive JSON structural allocation, and the
existing multibyte renderer truncation panic. Final command results are reported
in the worker acceptance report rather than asserted by this checkpoint note.


## Independent review correction checkpoint

The phase1 review found one P1 noninterference defect and two P2 parser-contract
defects. Each correction has an observed behavioral RED before its production
edit, followed by GREEN, in `~/tnl-inspection-target/review-*-{red,green}.log`:

- A 403 with 65 short fields lost Host Auto detection only when inspection was
  admitted. Off/on/admission-denied tests now compare exact relay bytes, final
  status and rewrite state, with and without leading 103 and with no final head.
- Colon-only trailer checks accepted empty/invalid names and control values.
  Tests split valid and malformed trailers at every byte boundary, including
  NUL/DEL/bare LF/obs-fold, and test exact 8192/8193 aggregate trailer limits.
- The ninth informational head reached the consumer before rejection. Tests now
  enforce eight callbacks and reject the ninth before emission, independently
  testing exactly 32768/32769 cumulative head bytes under fragmentation.

Actual-forwarder recording tests preload request head/body leftover, stream the
remaining chunked binary entity, compare exact decoded request/response bytes,
assert one CaptureId and no duplicated response prefix, and preserve wire bytes.
101 consumer tests exclude same-read and later binary/HTTP/JSON frame canaries in
both directions. Completion revocation and repeated-stop semantics are tested.
Additional tests cover 64/65 fields, 8192/8193 heads, duplicate equal/overflow CL,
malformed chunks/extensions, HEAD/304, serialized/Debug/search/summary/tracing
privacy sinks, invalid UTF-8/encoded/duplicate-media bodies and JSON redaction
expansion at the output cap. These are stronger checkpoint tests, not a claim
that every exhaustive phase2 case or peak transient allocation is qualified.

The original initial-RED chronology deviation remains unchanged and explicitly
parent-authorized; these new RED/GREEN results do not retroactively repair it.
Independent re-review is required before seam freeze or integration. No browser,
raw retention, replay engine, active-generation authority or commits are added.


Twelve bounded negative mutations were executed sequentially in this worktree,
with exact source restoration in `finally` after each run. All twelve were
killed by behavioral test assertions (not compilation failures): query masking,
header masking, recursive JSON masking, later and same-read upgrade suppression,
duplicate-length rejection, head byte/field caps, cumulative head cap, JSON
output cap, duplicate request-leftover feed and duplicate response-prefix feed.
Per-case logs and the machine-readable summary are under
`~/tnl-inspection-target/review-mutation-*.log` and
`review-negative-mutations.json`. Final gates run against restored sources.
