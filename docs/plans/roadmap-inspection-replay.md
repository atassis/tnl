# Authenticated browser replay wiring checkpoint

Base: browser inspector `48dfd2b` plus parent-merged reviewed raw engine
`9362954`. This is one functional replay path, not whole-T4 acceptance. The
working merge conflict resolutions preserve actual backend Some/admit_backend
and identical unconditional both-direction cancellation stops. An automatically
merged duplicate getrandom dependency was removed. The parent owns merge-index
resolution/staging/commit; this worker did not stage anything.

## Consent, ownership and lifecycle

`--replay-retain-secrets` requires `--inspect`, defaults false and is independent
of `--inspect-bodies`. The CLI binds BoundInspector first, obtains its nonzero
port, constructs ReplayController with explicit consent and that immutable
forbidden port, then constructs Inspection::with_replay and starts the same
listener. Enabled mode prints an explicit stderr warning. Persistent UI warnings
state original secret risks and caps:20 entries/10minutes/16MiB raw budget/1MiB
body, one execution. Previews never grant raw consent. None/non-loopback/inspector-
port backends decline raw admission; no browser destination override exists.

History owns the same controller used by the single ConsumerFactory. Lock order
is **history -> replay**, never reversed; the controller has no history callback.
Insertion and raw consumer admission share the history lease. Expiry/count
pressure calls forget under that lease. Availability/prepare use with_replay_row;
confirm_replay holds the same history lease and first checks exact consumed-ticket
metadata, then requires a reachable row for every new/unconsumed execution.
Both paths try-lock history, reject closed state and retain history -> replay
lock order throughout controller acceptance. A detached get/confirm is never
used for authority. No await or network I/O is performed by this critical
section; confirm schedules the engine's independently owned bounded operation.
Shutdown closes admissions before locking and authority explicitly checks this
closed flag, then shutdown clears raw entries/tickets and cancels execution.

Existing trusted reconnect activate calls now activate the controller only after
successful attachment. Same generation reattach rotates epoch/tickets; recreation
changes generation. All disconnect/error/cancel paths use existing availability
guard plus disconnected -> deactivate. Old detached tasks are not assumed ended.
The one-second idle service sweep invokes raw sweep; fatal listener exit and
service/process shutdown call controller shutdown through history shutdown.

## Single authenticated HTTP service

All new successful DTOs carry schema_version1. Existing exact Host/Origin,
Fetch Metadata, bearer, no-CORS/security-header and bootstrap rules remain.

- GET `/api/v1/requests/{capture}/replay`: safe availability under display lease.
- POST `.../replay/prepare`: strict JSON `{ "schema_version":1 }`; sends nothing.
  Reply includes sanitized method/path plus real preview (fixed original backend,
  risk metadata and sensitive expiring one-use confirmation nonce).
- POST `.../replay/confirm`: only schema_version, replay_id, confirmation_token,
  confirm_side_effects. Explicit true is mandatory for every original method.
  Missing/duplicate/unknown fields, alternate versions and destination/header/
  body overrides are rejected. Exact consumed ticket/capture/replay identity plus
  explicit acknowledgement recovers the saved outcome even after parent display
  eviction; this lookup is metadata-only and cannot retain raw data or execute.
  Every new/unconsumed ticket still needs a reachable display row under the atomic
  history lease. Safe outcome GET is also independent of display-row existence.
- GET `/api/v1/replays/{replay}`: safe linked outcome, never raw response bytes.
  A null/missing outcome means **unknown delivery**, not not-sent.

Mutations require POST, bearer plus exact Origin and unique same-origin Site,
Mode cors|same-origin and Dest empty. JSON is application/json, declared length
<=1024 bytes, with no Content-Encoding (including identity, empty or duplicates),
transfer coding or upgrade. Encoding is rejected before JSON/controller calls. The admitted input buffer is
32KiB head+1KiB JSON; one three-second read deadline covers both. Existing four
handler/eight connection and64KiB serialization caps still apply. Schematized
replay errors contain only safe enum codes; no raw source or secrets are logged.

## UI and attempt semantics

Details checks live availability; only real eligibility exposes Review. Review
POSTs prepare on explicit click and shows sanitized method/path, fixed original
backend, body length, expiry and risks. The nonce stays in JS closure only: it is
never rendered, logged, stored or placed in URL/history. Explicit acknowledgement
is reset for each review. Confirm invalidates the ticket/disables itself
synchronously before awaiting, so multiple clicks cannot automatically resubmit.
A new attempt requires a fresh explicit review and acknowledgement. Polling is
GET only. Linked outcome shows safe status/delivery/counts and parent capture;
network errors or absent outcomes display unknown delivery and no automatic
retry. No editing, redirects, automatic retry or display-to-request reconstruction.

## Evidence and remaining gaps

Real HTTP/backend tests exercise fixed-backend exact query/auth/cookie/body replay,
prepare-with-no-connection, duplicate confirms,302 without follow/retry, strict
JSON/CSRF/method policy, every-method acknowledgement, eviction between prepare
and confirm, same-generation reconnect and recreation ticket revocation, forbidden
port/None/non-loopback admission, default-off despite JSON previews and safe DTO
canaries. Deterministic internal tests assert the history mutex is held inside
authority and idle eviction clears raw retention without API activity. CLI tests
exercise both consent modes and explicit warning/caps plus Ctrl-C shutdown.

Behavioral RED was observed for missing CLI consent flag, missing replay API,
missing real UI controls and closed-history authority acceptance. Logs:
`~/tnl-inspection-target/replay-{cli-red,ui-red,dom-red,closed-lease-red}.log`.
Final original gate logs use `replay-wire-*`. Parent subsequently reported actual
Brave replay acceptance at `~/tnl-inspection-target/browser-replay-parent/result-fixed.log`:
bootstrap/auth, original method/query/auth/cookie/Host/body replay, prepare without
I/O, explicit checkbox/double-click suppression, no XSS execution, no raw/ticket
DOM leaks and no JS exceptions. That browser run was the parent's, not this worker's.

The raw-boundary audit corrections are covered by real HTTP RED/GREEN tests:
encoded prepare/confirm reject without creating/consuming tickets or backend I/O;
normal confirmation still succeeds. Consumed confirmation recovers identical saved
metadata after display eviction, while wrong identity/token/acknowledgement and
unconsumed evicted tickets cannot recover or execute. The earlier row-reachability
narrowing for consumed tickets was incorrect and is removed. Audit-fix logs use
`replay-audit-*`; independent re-review of these corrections remains required.

Still pending: independent integration and allocation/security review, broad pressure and actual reconnect/recreation with
old transport tasks e2e, and full preconnect/client_synth/transport status/timing/
successful-write schema. No whole-T4/production/deployment acceptance is claimed.
