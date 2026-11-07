# T4 inspection schema / failure checkpoint

This follows the reviewed browser/replay baseline `42169c8`. It supersedes the
older phase2 document's *unimplemented display schema* gap, not its historical
validation chronology. It is a bounded implementation checkpoint, **not whole-T4
or production acceptance**. Independent review remains required.

## Implemented boundary

- One display capture per admitted forwarding attempt, including refused/local
  connect failures, missing heads, probe failures, partial upload/response I/O,
  truncation and cancellation. Preconnect records use
  `request_boundary: preconnect_unavailable`, never a guessed resolved backend,
  and a display-only constructor that never invokes the raw consumer factory.
  Canonical request IDs come only from successfully decoded typed metadata;
  capture IDs remain independently generated. Visitor addresses are masked.
- Synthesized responses update that same record as `client_synth`; an actual
  backend 400/502/503, even with spoofed component headers, remains `backend`.
  When no response is produced, source is `unavailable`. Synthetic error context is not a JSON preview,
  even during an in-flight response write. No new synthesized status/fallback
  policy was introduced; current forwarder synthesis remains 502.
- Records add wall-clock start, monotonic duration, safe failure kind, tunnel
  name/available trusted tunnel ID, external origin, configured display target,
  actual winning resolved address when available, and explicit header/body
  states. Missing identity stays null rather than inferred from headers or
  assigned from another generation. Exchange state and tunnel connection state
  remain separate; cancellation cannot produce `complete`.
- `bytes_to_backend` and `bytes_to_client` count only successful local
  `poll_write` results, including partial writes before a later error.
  `bytes_from_backend` is the successfully forwarded response count only for a
  backend-sourced response, not synthesized output. These are **local accepted
  transport writes, not remote delivery or intended/observed payload**. Active
  API snapshots read atomics; terminal snapshots freeze counters and duration.
  Observed payload counts remain separately named. No second payload tee exists.
- Header states distinguish incomplete/complete/capped/malformed/unavailable;
  bodies distinguish disabled/pending/available/empty/omitted, with explicit mode,
  retained count, redaction flag and bounded static omission reasons. Oversized
  and streaming bodies expose omission before EOF; ordinary active count
  publication is rate-limited. Upgrade discards previews/opaque frame capture,
  while transport write counters may continue counting opaque forwarded bytes.
- Summary/detail fields are additive. Detail links safe eligibility and the most
  recent retained replay outcome from the existing controller, under the same
  history-to-replay lock order. It cannot admit raw data or execute replay.
  Wire overflow drops previews before headers and returns bounded valid JSON.
  Existing raw backend/factory/ticket contracts, consumer revocation, active
  eviction/no-resurrection and replay leases are retained.

## Evidence

Behavioral RED preceded the initial refused-connect, malformed-probe and partial
upload cancellation implementation (`schema-failures-red.log`). Follow-up REDs
caught cancellation misclassification while awaiting a head/response,
missing detail eligibility, active omission states, and synthetic error-context
preview retention. These are behavioral assertions, not compilation failures.

- `forwarder_inspection_failures.rs`: nine actual-forwarder tests, covering
  refused connect, malformed probe, no-head, probe read error, cancellation at
  three stages, partial upload/early response, truncation, backend status
  provenance, typed identity and zero raw-factory admission on preconnect.
- `cli_inspection_tunnel.rs`: real CLI binary through a real daemon/backend in
  default-off and explicit JSON modes; authenticated detail verifies policy,
  canaries, provenance, typed tunnel/request identity and exact write counts.
- Existing binary/1xx/LF/Host-rewrite, streaming, observation/privacy, raw replay,
  lifecycle and API tests remain enabled. Trace/privacy coverage now includes
  failed preconnect with a full summary channel. Unit tests cover partial-write
  accounting/saturation, live synth 400/502/503 omission, active SSE/oversize
  omission and detail wire-budget fallback. Replay detail tests verify actual
  controller eligibility and linked terminal outcome metadata.

Original schema checkpoint gates (before the 1901 follow-up below; owned target,
jobs=1, dev/test debug=0):

- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo test --workspace`: **312 passed, 0 failed, 2 pre-existing ignored**.
- `git diff --check`: passed; index/staged/unmerged lists are empty.

Logs: `~/tnl-inspection-target/schema-{failures-red,cancel-await-red,detail-red,
synth-privacy-red,progress-red,eligible-red,cli-tunnel,clippy-final,workspace,
fmt-check}.log`. No staging, commits, merges, deployment or other-worktree writes.

## Review 1901: admission correction and essential qualification (historical)

**At this checkpoint release was BLOCKED by the retained actual-reconnect
old-response drain regression below.** The later S0 composition and authorized
contract split are recorded in the latest section. Neither checkpoint constitutes
full schema/browser/T4 acceptance.

### Causal admission correction

The review found that `ForwardDisplay::new` allocated two counter Arcs plus
owned tunnel/target labels before the 64-capture admission and retained them
for declined or already-shut-down inspection. The guard now borrows context
and target, holds no owned label strings, and starts with no counters/capture.
Only successful bounded backend/display-only admission initializes counters;
only that capture's record receives allocated sanitized labels. I/O attaches
counter references after admission and before any counted writes. No additional
unbounded Arc layer, blanket tunnel limit, early raw admission or forwarding
fallback was introduced.

`forwarder::admission_tests` first failed behaviorally with
"declined inspection retained owned counters/strings". It now passes for 64
held captures and an additional real long-lived forwarding attempt, and for an
already-closed service. The test verifies no owned inspection state at entry
or after declined admission, exact continued bytes, and reuse of all 64 permits.
The initial missing-test-helper compile error is not that behavioral RED.
Existing partial-write, cancellation, eviction/no-resurrection, synth privacy,
raw-factory and replay-lease tests remain enabled.

### Capacity / pressure and controlled failures

The combined pressure fixture fills 200 history rows, holds 64 observer captures,
pressures raw body reservations near 16 MiB, issues four concurrent detail/search
clients (search limit 100 and 256-byte queries), and then holds all four handlers
with slow request heads while joining service shutdown within one second. An
additional declined actual forwarder continues exact binary bytes across API
pressure and service shutdown. Late capture drops cannot resurrect rows.

Accounting uses owned **capacity**, not serialized lengths: String/Vec capacities,
inline/Arc/Mutex/consumer allowance, header-vector allocations, and the full
1-MiB finished raw Bytes backing reservation. History inline storage is even
counted twice conservatively. Handler accounting reserves full 64-KiB record,
summary and output quotas plus head/JSON/filter/stack allowances (246,784 bytes
per handler), rather than pretending a small response length is its allocation.
This is an inspection-owned allocation model, not allocator RSS/kernel/base
transport accounting. The final measured fixture reports history=3,186,440;
observers=824,320; raw=15,863,232 bytes. Together with four full handler
allowances it reports 20,861,128 bytes, below
the conservative 35-MiB-plus-fixed-metadata envelope. It is not exhaustive
allocator/platform pressure certification or concurrent replay-engine execution
qualification.

Controlled scoped resolver and pending-dial futures exercise DNS failure and an
actual paused-clock connect deadline. Test-only per-context connector failure
hooks qualify forwarder mappings, safe omission, zero guessed backend and zero
preconnect raw-factory invocation. There is no arbitrary-hostname flag, network
blackhole dependency, global failure switch or new production fallback.

### Actual reconnect: meaningful retained failure, OFF and ON

The new E2E uses the real daemon, `reconnect::run`, registry closure and the
existing cancel-first-session hook. No test-held old control/session reference
keeps the old transport alive. After real recreation:

- Old record is still Active and Superseded; the old raw ticket is revoked.
- New-generation CURRENT response bytes succeed exactly.
- After releasing the old backend's DONE suffix, both inspection OFF and ON
  old visitor body drains time out, **not** complete/EOF/reset. Both assertions
  remain active and failing, with a two-second drain deadline inside an overall
  fifteen-second fixture deadline. They are neither ignored nor weakened.
- Backend suffix task finishes. Logs show old control sessions closed, old
  client accept loops ended cleanly, old session handles disappeared, and old
  forwarders failed with connection-is-closed. ON correctly records Failed /
  response-io and 42 accepted client bytes (head + LIVE, not unsent DONE).
- Reconnect remains running and only new session handles remain. Session driver
  JoinHandles are private; driver completion was not directly instrumented.

A preliminary direct-client recreation that retained old control completed old
bytes, but that is **not equivalent** to actual reconnect and is not used as a
substitute for the failing regression. The OFF/ON comparison establishes that
inspection is not required to reproduce this scenario; it does not by itself
prove a precise transport root cause or approve a transport implementation fix.
No transport/driver implementation was changed. Parent explicitly requires this
failure to remain release-blocking pending diagnosis/decision.

### Pre-S0 validation and evidence

Formatting, workspace/all-target clippy `-D warnings`, focused admission/failure/
pressure tests and diff checks passed. `cargo test --workspace` on that source
failed on the two new actual-reconnect drain regressions. A no-fail-fast workspace
run completed with **316 passed, 2 failed, 2 pre-existing ignored**, exposing all
other suite outcomes. Do not substitute the earlier 312-pass or preliminary
317-pass run as final-green evidence.

Logs in `~/tnl-inspection-target/`:

- `admission-1901-{red,green}.log`
- `admission-focused.log`, `admission-pressure-final.log`
- `admission-recreation-off-on.log` (bounded OFF/ON diagnosis)
- `admission-{fmt-check,clippy-final,workspace-final,workspace-all}.log`

Blockers recorded at that checkpoint: actual-reconnect old-response termination/drain diagnosis
and approved correction; independent admission/capacity re-review; fresh browser
acceptance on the all-feature composition (parent-owned, retaining canonical-ID,
provenance, privacy and replay/no-duplicate-execution assertions). Broader sustained
pressure/platform and concurrent engine-execution qualification remain limited.
No staging, commits, merges, deployment or other-worktree writes.


## Reviewed S0 composition and authorized recreation contracts (latest)

Independent review `0b7d31db` approved the P1 admission correction and the bounded
pressure/resolver coverage, with their documented limits. It separately found
that the recreation fixture assumed an HTTP response-chunk boundary.

### Counterfactual diagnosis before changing the termination contract

This worktree still used registry yamux 0.13.10 and did not contain the already
reviewed S0 close-state/wakeup fix. A **temporary Cargo CLI patch**, reading the
integration worktree's vendor read-only, changed no production sources. The own
Cargo.lock and temporarily instrumented test source were restored byte-for-byte
before parent composition. Evidence and the instrumented source remain in the
owned target, including the original failing assertions.

The same serial OFF/ON scenario showed:

| Scenario | Registry yamux | Reviewed vendor yamux |
| --- | --- | --- |
| Retained old session, generation recreation | Exact remaining DONE and CURRENT | Exact remaining DONE and CURRENT |
| Actual control-loss reconnect, inspection OFF | Outer 2-second timeout | Bounded premature-body error |
| Actual control-loss reconnect, inspection ON | Outer 2-second timeout | Bounded premature-body error |

The vendor error was `reqwest::Error` with `is_decode=true`, `is_body=false`,
`is_timeout=false`; its source chain was hyper Body -> typed
`std::io::ErrorKind::UnexpectedEof` / IncompleteBody. After joining the released
backend suffix task, the inspected old record was Failed / response-io /
Superseded with 69 request bytes and 42 accepted response bytes. The observer saw
eight payload bytes, but the late DONE was not accepted for writing to the closed
old session. Those are different facts, not a delivery claim.

Source/order evidence explains the two contracts: the cancel hook causes
`reconnect::run` to drop `_ctrl_keep`; the daemon removes the old session handle
after control EOF; yamux graceful close ends stream state and drains queued
commands. The fixture writes DONE only after that control loss and the new
CURRENT response. Graceful flushing of queued bytes cannot guarantee delivery
of future backend bytes produced after the old transport closes. Without S0,
the pending read does not wake/terminate; this meaningful timeout regression
remains in the saved negative evidence.

Based on that source/counterfactual evidence, parent authorized the two distinct
contracts below, then **parent** composed already-reviewed S0 commit `082d2e4`
with `--no-commit`. Worker did not merge, stage, or change transport code. Normal
Cargo now uses the own-worktree vendor; no temporary override or old-lock
restoration was used after parent composition. Parent's staged merge is retained
unchanged (before/after index hashes match).

### Enabled, specific tests rather than blanket error acceptance

`crates/tnl-e2e/tests/inspection_recreation.rs` now keeps all three contracts:

1. Retained-session generation recreation deliberately keeps old control and
   accept loop alive: exact DONE and CURRENT, old Complete/Superseded state,
   revoked old replay ticket, exact actual backend request count and 46 accepted
   response bytes. Generation changes alone cannot interrupt that old stream.
2. Actual control-loss reconnect with inspection ON.
3. The identical actual reconnect with inspection OFF.

Both control-loss cases require termination within an outer two-second deadline.
Success must contain exact remaining DONE. An error is accepted **only** if it
is non-timeout, body/decode-classified, and has a typed UnexpectedEof in its
source chain. Arbitrary errors and timeouts fail. With inspection ON, eventual
old metadata must agree: Failed/response-io/Superseded and 42 accepted response
bytes on the observed error path, or Complete and 46 on exact success; current
generation identity and CURRENT bytes remain checked. Every scenario retains
an overall fifteen-second fixture deadline.

The HTTP-chunk correction was independently approved, not a terminal-contract
relaxation: accumulate exactly LIVE across chunks, reject mismatches, premature
EOF and body errors, and preserve any excess for the remaining drain. A gated
one-byte first write produced behavioral RED (L != LIVE) before correction.
Five prefix tests now cover fragmentation, surplus, mismatches, EOF and body
errors. The complete eight-test file passed five consecutive runs after S0
composition; no unexpected error class or counter race was observed.

Evidence in `~/tnl-inspection-target/`:

- `s0-diag-original-contract.log`
- `s0-diag-instrumented-{patched,unpatched}.log`
- `s0-diag-instrumented-regression.rs` (temporary diagnostic source retained)
- `s0-diag-{prefix-red,prefix-green}.log`
- `recreation-contract-{first,repeat,fmt-check,clippy,workspace}.log`
- `s0-parent-index-{before,after}.sha256`

Final normal-composition gates passed: formatting check, workspace/all-target
clippy `-D warnings`, and full `cargo test --workspace --no-fail-fast` with
**338 passed, 0 failed, 2 pre-existing ignored**. Diff checks pass. The parent-
staged S0 merge remains staged and unchanged; worker staged no files. Fresh
all-feature browser acceptance and independent review of the final recreation
fixture remain required. In particular, this does not claim fresh canonical
public-request-ID/browser acceptance on the separately owned T2 composition.
Sustained/platform/RSS and simultaneous replay-engine pressure remain limited
as previously documented. No whole-T4 or production acceptance claim follows.
