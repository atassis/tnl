# T4 phase2 bounded browser checkpoint — not whole-T4 acceptance

Baseline: approved/committed `c760559`. No T5 raw store, replay controller/engine,
confirmation semantics, fake replay buttons, daemon changes or WebSocket branch
integration are included. No staging, commits or deployment by this worker.

## Implemented interface

`tnl http TARGET [SUBDOMAIN] --inspect [--inspect-port PORT]
[--inspect-bodies off|json]` enables the service. Other inspection options require
`--inspect`; previews default off and JSON previews are not raw-retention consent.
The existing public `commands::http::run` signature remains a default-off wrapper.
Clap completion still derives from the same command definition.

One IPv4 loopback listener binds `127.0.0.1`, port 0 by default. Bind/randomness
failure is explicit before remote connection/tunnel banner. The capability is
32 OS-cryptorandom bytes, hex encoded, independent of account credentials. It is
printed only in the private startup stderr fragment URL. Treat that URL/stderr
as sensitive. Authentication compares fixed-length bearer values with subtle's
constant-time equality. Tokens are not Debug/Serialize values or API fields.

Static GET `/` and `/inspector.js` contain no captured data or capability. JS
extracts the fragment and immediately replaces browser history, keeps the token
only in its closure, uses authenticated fetch with no credentials/cache/redirect,
and renders captured values with textContent. No storage, innerHTML, eval,
external assets, auto browser launch, export or replay controls are present.
Read APIs: `/api/v1/requests`, `/api/v1/requests/{capture_id}`, `/api/v1/stats`.
Lists are sanitized summaries, not full-record arrays. Query parameters are
bounded literal `q`, `method`, `status`, `state`, `before`, `limit` (max100).
Duplicate/unknown/invalid parameters are rejected. Pagination is admission-
sequence based and is not a stable snapshot under traffic/eviction.

HTTP is deliberately a narrow HTTP/1.1, one-request-per-connection subset parsed
with bounded httparse input. No request bodies, transfer encoding, upgrades,
pipelining, keepalive, POST handlers or raw-fetch endpoints are implemented.
Exact Host is required on all routes; present Origin must match the actual
loopback origin; foreign/same-site Fetch Metadata is denied. Cross-site requests
remain denied except user-activated GET of the exact static root `/`, with unique
Fetch-Mode `navigate`, Fetch-Dest `document`, and Fetch-User `?1`. This narrow
printed-link bootstrap exception still passes exact Host, existing Origin and
framing/body checks; it does not apply to scripts, APIs, queries, iframes or
mutations and does not grant a capability or captured data. APIs require bearer
auth before resource lookup. Any non-GET API request additionally requires exact
Origin and same-origin/empty/cors-or-same-origin Fetch Metadata, then returns405:
there are no mutation routes. No CORS headers or credential cookies are issued.
Responses include no-store/no-cache, nosniff, no-referrer, frame DENY and CSP
(default none, static script self, connect self, no forms/base/framing).

## Resource ownership and lifecycle

- Existing history: 200 rows/15-minute admission TTL, conservative64-KiB row
  reservations. Service sweeps expiry every second, including with no API reads.
- At most8 admitted connection tasks before parsing/task creation; at most4
  non-waiting handler permits before request buffer allocation. Excess sockets
  are closed; there is no request/connection waiting queue in this process.
- Request head <=32KiB/64fields; URI/query <=2KiB, q<=256bytes; read/write deadlines
  3seconds; connection closes after one response. Response serialization is a
  bounded64-KiB writer and never serializes raw observer input.
- Four handler permits each reserve a conservative256KiB working envelope
  (aggregate1MiB): at most one64-KiB record clone, <=64-KiB summaries, <=64-KiB
  output,32-KiB request head and bounded search/parser scratch. Lists clone one
  record at a time, release the history lock, then search outside the lock.
  Summary admission uses a conservative escaped-output cost before cloning.
  API access uses try_lock (503 on contention); no history lock crosses network
  I/O, serialization or awaiting a service client. The old public search helper
  is retained for compatibility/testing but is not used by any HTTP route.
  This is source-level bounded accounting, not a measured total-RSS guarantee;
  independent transient allocation/contention review is still required.
- Shutdown revokes new admissions, clears rows, closes listener, aborts/joins
  idle connection tasks and joins the service. Fatal listener exit clears state
  and warns without failing normal forwarding. CLI Ctrl-C explicitly awaits
  service shutdown and terminates the summary renderer.
- Reconnect calls activate(generation) after success, disconnected() on session
  loss, and an availability guard marks cancellation disconnected. Same reattach
  retains generation; recreation changes it. Per-record connection labels are
  untracked/connected/disconnected/superseded, separate from exchange state.
  Old detached forwarding tasks are not assumed dead and can still complete;
  their connection label remains superseded after recreation. Closed inspection
  cannot be reactivated by a later reconnect.

## Coordinated T5 seams (no engine integration yet)

`CaptureContext.backend: Option<std::net::SocketAddr>` is the exact shared field.
`Inspection::admit(gen, metadata)` remains and supplies None. New
`admit_backend(gen, metadata, backend)` supplies Some(actual resolved address),
used at the existing post-Host rewrite forwarding tap. Backend is not part of
serialized display records/summaries. T5 must decline None/non-loopback itself.
There is still exactly one CaptureId and one observation tee.

`BoundInspector::bind(port)` returns a bound, non-serving listener with actual
nonzero `addr().port()`. A future T5 controller must be constructed with that
immutable forbidden inspector port before `bound.start(history)` spawns routing.
No raw controller or duplicate replay facade is wired here. Future integration
must explicitly connect disconnect/activate/same-generation reattach/recreation
and shutdown revocation callbacks to T5 authority, and link safe eligibility/
outcomes through the same CaptureId and the same authenticated service. Browser
mutation routes/confirmation tickets stay pending their agreed engine API.

The approved cancellation correction is retained identically: non-upgraded
terminate(false) unconditionally emits request.stop(cancelled) and
response.stop(cancelled), even after both MessageEnds, before Finished. Normal
finish emits no cancellation stops; 101 preserves Upgraded. Stops remain
idempotent revocations, not exactly-once notifications.

## Validation and explicit gaps

Tests include real HTTP list/detail/auth/security/query/caps, cryptorandom token
separation, bind failure, slow header expiry, excess clients, API serialized
limits, real CLI private startup/quiet+JSON/Ctrl-C shutdown, actual forwarding
resolved backend, both-End cancellation, fake-clock periodic expiry, reattach,
and recreation labels with an old capture still alive. Seven negative mutations
(auth, Host, Origin, Fetch Metadata, CSRF, request-head and output caps) were
killed by behavioral assertions and sources restored. Logs are
`~/tnl-inspection-target/phase2-*.log`; final gates are reported in the acceptance
report. Builds reuse the owned target, jobs1, dev/test debuginfo0.

Observed behavioral RED preceded CLI options, connection labels, actual backend
wiring, both-End cancellation, real reattach availability, CSRF context, lowercase
state filters and closed-state reactivation corrections. The new API test's
initial RED was missing-module compilation, not behavioral HTTP evidence; later
real HTTP tests and negative mutations provide behavioral qualification. The
original phase1 initial-RED chronology deviation remains recorded and is not
retroactively repaired by these tests.

Not complete/accepted yet: actual browser/manual DOM/CSP acceptance (no browser
was run); independent code/security/allocation review; full pre-connect and
client-synth capture/source/transport-byte/duration schema; pressure qualification
beyond these bounded fixtures; actual recreation with old transport tasks e2e;
engine lifecycle revocation, raw retention, replay routes/confirmation/engine;
combined WebSocket/T2/T5 integration and deployment. No whole-T4 or production
acceptance claim follows from this checkpoint.


## Printed-link browser bootstrap correction

Parent Chrome observed a correct-Host, no-Origin, extension-initiated navigation
with `Sec-Fetch-Site: cross-site`, Mode `navigate`, Dest `document`, User `?1`.
The unconditional site guard rejected it with403. A real HTTP regression observed
that RED before the narrow static-root exception above was implemented. Negative
HTTP tests preserve rejection for missing/wrong/duplicate navigation headers,
duplicate Site, foreign/null/duplicate Origin, wrong/duplicate Host, non-GET,
script/API/query targets (including valid API bearer), and forbidden framing.
The root response is checked to contain neither capability nor captured canary.
No T5 integration or schema changes accompany this correction. Actual Chrome
fragment removal/script/authenticated-fetch acceptance is still for the parent
to repeat with unmodified browser headers; worker HTTP tests do not claim it.
Logs: `~/tnl-inspection-target/bootstrap-navigation-{red,green}.log` and final
`bootstrap-{fmt-check,clippy,workspace}.log`.
