# T5 raw retention / replay engine checkpoint

Baseline: `c760559` (reviewed observer phase 1). This is **not full T5,
production replay authorization, or browser acceptance**. No listener, route,
CLI flag, UI, forwarder tee, reconnect change or display-to-raw reconstruction
is added. The supplied untracked `roadmap-replay.md` remains unchanged.

## Concrete integration API

`tnl::replay::ReplayController` is the single replay-owned controller; do not
create a separate inspection ReplayFacade/store or listener. It implements the
existing `inspection::ConsumerFactory` and returns only safe metadata via:

- `new(retain_secrets: bool, inspector_port: u16) -> Result<Self, ReplayError>`:
  explicit independent consent, nonzero already-bound inspector port mandatory.
  `false` retains no raw bytes, regardless of display-body/inspection settings.
- `activate(TunnelGeneration)` / `deactivate()`: **trusted process lifecycle
  hooks**, not browser inputs. Every activation rotates the epoch and tickets;
  same-generation reattach retains only completed captures. Recreation clears
  old raw entries. Disconnect disallows new execution/admission. An old observer
  finishing after a transition cannot restore eligibility. Accepted operations
  keep their original immutable target and are not restarted on transitions.
- `availability(CaptureId) -> ReplayAvailability`,
  `prepare(CaptureId) -> Result<ReplayPreview, ReplayError>`.
  Preview performs no backend I/O and pins no raw data. It deliberately does not
  expose method/target/query/Host/headers/body; use the sanitized display row.
- `confirm(CaptureId, replay_id: Ulid, token: &str, confirm_side_effects: bool)
  -> Result<ReplayOutcome, ReplayError>`: explicit acknowledgement for **all**
  methods. Returns running metadata promptly, never waits for backend completion.
  A duplicate consumed ticket returns the existing outcome without sending.
- `outcome(Ulid) -> Option<ReplayOutcome>`, `forget(CaptureId)`, `sweep()`,
  `shutdown()`, `raw_bytes()` (charged reservations, not process RSS).

The service must bind first, obtain its real port, then construct the controller
before serving. T4 must use `Inspection::with_consumer` and its coordinated
`admit_backend(generation, metadata, Some(resolved_original_backend))` seam.
This checkpoint adds only the agreed `CaptureContext.backend: Option<SocketAddr>`
and existing history initializer `None`. Consequently current production
forwarders **cannot retain/replay anything yet**. Tests supply trusted Some
contexts or a test-only backend factory. No DNS, URL input, target editor or
captured Host routing exists. All loopback aliases of the inspector port are
forbidden both at admission and execution. Missing/nonloopback backends decline.

Before calling prepare/confirm, the authenticated service must validate strict
schemas and synchronize sanitized-row existence with prepare/confirm and
eviction-driven `forget`. A detached `get()` snapshot followed later by
`confirm()` does not prevent an eviction race. On service failure/shutdown call
`shutdown`; schedule periodic `sweep` even when there is no traffic. Lazy sweeps
on lookups/mutations already enforce expiry. Real reconnect lifecycle calls and
synchronized display-row reachability remain integration work. The controller's epoch/generation/token/quota checks are under
one mutex: the browser alone is **not** the authorization authority. Budget
release uses atomics, never reacquires that mutex; cancellation signals under the
mutex, and engine completion locks only after I/O. No I/O or await holds it.

## Bounds and fidelity

- 20 live raw allocations, including execution pins after eviction; 16 MiB
  aggregate reservations, including retained capacities, pinned snapshots and
  an execution transient reservation. 8 KiB canonical raw head, 64 fields,
  1 MiB entity and ten-minute monotonic lifetime from admission.
- Each entry reserves 32 KiB for its fixed head, structural bookkeeping and
  bounded validation transients. First nonempty body reserves a full 1 MiB Vec
  before allocating/copying. Completion moves it into shared Bytes, no body
  clone allocation. This deliberately sacrifices small-body packing density
  for a simple capacity/accounting proof. Empty bodies allocate no body slab.
- One execution reserves 256 KiB before acceptance (Hyper's 32 KiB buffer,
  bounded header/URI serialization, 8 KiB read scratch and boxed driver future).
  Full<Bytes> shares the charged body allocation. Oldest-admitted-first eviction
  cannot reclaim pins, and new retention declines if pins exhaust quota.
- At most eight 30-second random 256-bit tickets, 32 outcomes/consumed tokens,
  fifteen-minute completed outcome age, one active bounded task, no job queue.
  These fixed metadata structures fit beneath the separate 256 KiB control
  envelope; HTTP serialization/concurrency remains service-owned.
- Strict allowed methods/origin target/header validation; reject CONNECT/TRACE,
  upgrades, trailers, ambiguous/duplicate framing, oversize/incomplete/cancelled
  requests. Binary and Content-Encoding bytes are preserved, never decoded.
  Strip hop-by-hop/Connection-nominated fields and Expect; recompute length;
  preserve captured effective Host, exact query/entity and repeated end-to-end
  fields. No redirects, retry, daemon traversal or replay-origin recapture.
- Fresh manually connected fixed-loopback Hyper HTTP/1 connection; concurrent
  upload/response driving; 3-second connect, 15-second first-response and
  30-second overall deadlines, shutdown cancellation. Response entity is drained
  and discarded with a 1 MiB limit, never archived. Read scratch <=8 KiB,
  cumulative wire heads <=32 KiB and <=8 informational heads; 101 is invalid.
  A frame crossing the entity cap is rejected, with truthful partial-byte count.
- All raw types lack Debug/Serialize/getters. Preview Debug hides its ticket;
  only preview JSON intentionally returns the one-use token. Metadata does not
  contain raw Host/query/authorization/body. Memory/swap/core-dump inspection is
  outside the guarantee; no zeroization claim.

## Observer correction (new independent-review item)

With supervisor approval, cancellation now emits sticky request AND response
`Stopped(cancelled)` before Finished even if both MessageEnd events occurred.
Previously dropping such a capture looked identical to graceful finish. A
behavioral RED against the old implementation and GREEN on the correction prove
revocation while graceful finish remains eligible. Repeated stops cannot restore
retention. A response-only framing/network failure does not itself invalidate a
complete intended request; explicit cancellation does. 101 immediately revokes.

## Evidence and remaining gates

Tests live in `crates/tnl/src/replay/tests.rs`; initial missing-feature RED was
observed before production implementation. Additional behavioral RED/GREEN covers
response-only failure semantics, cancellation after both message ends and
informational/cumulative response-head bounds. Real loopback tests exercise
exact binary/compressed entity/query/Host, hop stripping/repeated headers,
103/final status, no redirect/retry, eight concurrent duplicate confirms,
nonwaiting busy handling, malformed/101/over-limit responses, first-response
clock advancement and bounded conservative cancellation. Store tests cover
consent/destination policy, provisional ends/repeated stops, expiry, count/byte
pressure including private pinned allocations, empty/trailer/head/body limits,
confirmation capacity/binding/expiry, reattach/recreation revocation and privacy
canaries in safe Debug/serialized metadata.

Validation logs use the exclusively allocated existing cache directory
`/home/atassis/.cache/tnl-websockets-target/replay-*.log`, jobs=1 and dev/test
DEBUG=0. Final exact command/results are in the worker acceptance report.

Still required: independent code/resource/authority review; T4 single-service
capability/Host/Origin/Fetch-Metadata/strict JSON routes and descriptive mandatory
checkbox confirmation for ALL methods; real resolved-backend/lifecycle/eviction
wiring and periodic sweeps; safe reason tombstones on sanitized display rows
(the raw controller returns not-retained without retaining unbounded tombstones);
combined forwarder upload/reconnect/metadata/WebSocket e2e; browser runtime/XSS/
uncertain-delivery acceptance. No claim that the original forwarder upload-order
prerequisite is repaired by this direct-engine checkpoint. Connect/overall timeout
and cross-platform IPv6 execution need expanded platform qualification.

## Independent review corrections: response guard and Host syntax

The first independent T5 review blocked on a response-bound bypass and identified
insufficient Host validation. Both are corrected; independent re-review is still
required before integration/commit.

- **P1:** the response wire guard now requires a verified canonical HTTP/1.0 or
  HTTP/1.1 status prefix (three status digits plus the following space) and CRLF
  head line endings. Leading blank lines, bare LF, malformed prefixes and unknown
  heads fail closed instead of becoming an unchecked final head. Hyper still
  parses response semantics/entity framing. The existing cumulative 32 KiB/eight
  informational bounds and 8 KiB scratch reservation are unchanged. Binary entity
  bytes are exempt only after a verified final head.
- **P2:** Host validation retains Authority's existing lexical gate, then checks
  a nonempty URI host, bracketed IP-literal grammar, and any explicit port as
  nonempty ASCII digits within `u16` (0..65535). Host remains an end-to-end value,
  never a destination. No DNS-label policy is imposed: existing underscores,
  trailing dots, sub-delimiters, numeric leading zeros, IPvFuture and RFC 6874
  scoped IPv6 forms remain accepted. Neither name nor scope is resolved.

Five additional tests bring the replay suite to 21. Behavioral RED logs
`replay-review-p1-red.log` and `replay-review-p2-red.log` were recorded before the
production fixes. Both leading-prefix backend suites failed on the old guard
(LF and double-CRLF bypasses observed); single-CRLF and other blank prefixes are
also covered. Prefix writes are split at 1/7/12/8192 bytes; excessive informational
count and cumulative individually bounded heads are tested separately. A positive
backend test fragments canonical HTTP/1.0 and HTTP/1.1 heads and returns a binary
entity containing CR/LF. The invalid-Host table originally both admitted and
connected for malformed ports/literals; after correction every invalid entry is
ineligible and a bound loopback backend observes zero connections. A positive
Host table guards against accidental DNS-label overrestriction.

Final correction validation: replay **21 passed**; workspace **270 passed, zero
failed, two existing ignored**; `cargo fmt --all -- --check` and workspace
all-target clippy with `-D warnings` clean. Logs are `replay-review-*.log` in the
same exclusively allocated cache. No changes to slots/pins, SSRF/inspector-port
policy, confirmation consumption, generation/epoch authority or service wiring.

### T4 API constraints confirmed by review

1. Bind the sole inspector first and give `ReplayController::new` its actual
   nonzero port and independent raw consent. Admission backend is only the
   trusted resolved original socket address; `None` remains unavailable.
2. Feed effective-Host, post-envelope bytes exactly once through the existing
   observer. Preserve provisional ends/sticky stops. `Capture::finish()` asserts
   original-attempt termination, not merely completion of request framing.
3. Trusted successful attach calls `activate`, keeping generation on reattach
   and changing it on recreation. Disconnect calls `deactivate`; service failure
   calls `shutdown`; periodic `sweep` must run without traffic.
4. Synchronize display-row reachability with prepare/confirm/forget; an earlier
   detached row lookup is not a lease. This integration is not implemented here.
5. All replay routes require the single service's capability/authority checks;
   mutations additionally need exact Origin/Fetch Metadata, bounded strict JSON,
   duplicate/unknown-field rejection and explicit side-effect confirmation for
   **every** method. No raw request, destination, Host or entity editing.
6. Prepare performs no I/O. Confirm accepts asynchronously; consumed-token
   duplicates return the existing attempt. Busy/errors never cause automatic
   retry. A new attempt requires a fresh preview and explicit confirmation.
7. Missing outcome after submission means unknown delivery, never not-sent.
   Never expose raw snapshots or put preview tokens in history/logging. T4 owns
   service schema/version wrappers and bounded safe expiry/eviction tombstones.
