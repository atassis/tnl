# Inspector frontend and client-side timing

Status: approved by the user on 2026-11-08. Implementation planning requested;
implementation, commits, publication and deployment have not been authorized
by that planning request.

## Agreed decisions

- Replace the minimal HTML/JavaScript inspector with a Vite, React, strict
  TypeScript and compiled Tailwind application.
- Use the Trace layout: request list at left, selected-request metadata and
  timeline above headers/body details. Responsive layouts may stack detail panes.
- Provide Auto / Light / Dark appearance independently of layout. Auto follows
  the browser's system preference and responds to preference changes.
- Generate production assets; do not commit dist, node_modules or source maps.
  Building the client from a Git checkout requires Node, the pinned package
  manager, and Rust. Running a released client requires only its binary.
- Include real measurements in this work, not decorative phase durations.
- Measurement scope is the local client's forwarding work and its exchange with
  the configured backend. No daemon, Caddy, visitor-browser or distributed timing.

## Scope and invariants

Keep the existing forwarding, inspection admission, privacy and replay authority
contracts. Do not introduce transport negotiation, daemon changes, proxy config,
new forwarding retries, buffering, backend destinations, or network deadlines.
Normal tunnels remain public; inspection and raw replay retention remain opt-in.

The frontend must display only data actually present in a capture. Synthetic
mockup values and phase splits must not enter production code. Failed, incomplete,
unavailable, superseded and upgraded states remain distinguishable.

This work does not authorize dependency security remediation, Git publication,
release publication, production changes or installed-client replacement.

## Frontend boundaries

Source lives in `frontend/inspector/`:

- `app/`: composition, responsive shell, selection, appearance and lifecycle.
- `api/`: generated public types/validators and the same-origin API client.
- `features/requests/`: bounded history, search, pagination, detail and states.
- `features/timing/`: interval/milestone rendering and measurement explanations.
- `features/replay/`: explicit review, acknowledgment, submit and linked outcome.
- `components/`, `styles/`: presentation primitives and Tailwind theme tokens.

Use React state/reducers and explicit API operations; do not add a global state
framework, client-side router, SSR, PWA, analytics or a service worker. Request
lists retain server bounds and pagination rather than accumulating every poll.
Cancel/ignore stale reads when selection changes; do not let an older response
replace a newer selection. Background history refresh must not reset selection
while that capture remains available. Show eviction, disconnect and capability
refusal. Filters must state their scope; do not present filtering of a loaded
page as filtering the complete retained history.

Display captures as text or safely constructed elements. Do not render captured
HTML, inject capture-derived markup, spread captured objects into element props,
or use raw HTML as a syntax-highlighting shortcut. Use local assets/system fonts.
Warnings remain visible in context: concise privacy status on the main screen,
and explicit secret/side-effect warnings in replay review. Do not remove consent
or hide security-relevant states to achieve a cleaner appearance.

## Appearance and URL state

Persist a manual theme for this inspector URL using an allowlisted fragment
parameter. Preserve the capability parameter when changing appearance; never
copy it to query strings, logs, DOM text, browser storage or API URLs. Auto removes
that override. Theme changes do not reset selection, tabs or replay state.
Reload retains capability/theme but clears replay tickets and acknowledgment.
A new CLI invocation generates a fresh capability; its printed URL defaults to
Auto. No cross-run preference storage is introduced.

## API contract and generation

Keep authority, capture buffers, synchronization and replay execution in Rust.
Separate public serialized DTOs from internal capture state in a small
`tnl-inspector-api` crate that does not depend on the client, assets or daemon.
It contains only the public inspection/replay contract and schema definitions;
internal clocks, atomics, raw retained requests and authority handles do not move
there. DTO projections must preserve existing allocation/accounting bounds.

Export JSON Schema from these Rust DTOs before building the UI. Generate TS types
and standalone response validators from that schema at build time. Production
validation must not compile schemas using eval/new Function. Match actual serde
representation, nullability and numeric ranges with contract tests. TypeScript
casts alone are not validation. Exported confirmation-ticket fields are private
execution inputs, not history/display fields; keep their values out of rendering.

Schema/type generation must build independently of `tnl`, avoiding a cycle in
which generating frontend types requires already-generated frontend assets.
Keep authenticated `/api/v1` routes, existing fields/schema versions and
server-side validation authoritative. Add a `timing` object with its own
`schema_version: 1`; do not rename existing fields or change tunnel metadata.

## Build and embedding

The repository provides a cross-platform explicit build entry point that runs:

1. Locked dependency installation as a documented setup step.
2. Rust public-schema export and frontend contract generation.
3. TypeScript checks and the Vite production build.
4. Asset-manifest generation, then the requested Cargo build.

Pin the Node/tooling versions and package-manager lockfile. Use the same entry
point locally and in CI/release builds; no hidden global frontend tooling.
Production output is staged under `crates/tnl/src/inspection/assets/` and ignored
by Git. Cargo package/source-release inclusion rules must explicitly preserve
required generated files when creating a distributable package.

`build.rs` validates required assets and their input/content manifest and emits
an actionable error when missing or stale. It never installs packages, invokes
npm, accesses the network, or substitutes old/empty assets. Ordinary Rust commands
that build `tnl` require the documented frontend preparation step. Do not promise
that `cargo install --git` can build a fresh checkout without that preparation.

Embed a finite manifest of HTML, JS and CSS as static bytes. One physical file is
not required. Serve exact known asset paths and MIME types, with no filesystem
fallback or generic SPA fallback for API errors. Do not clone entire bundles into
per-request Vecs. Preserve JSON-response and handler admission limits separately.
Initial uncompressed build budgets: all JS <=512 KiB, CSS <=128 KiB, all embedded
frontend files <=1 MiB. Measure the actual build; exceeding a budget is a review
failure, not permission to silently increase it. Do not embed source maps.

The existing Dockerfile builds `tnld`, not the inspector-bearing client. Keep the
server-only build independent of frontend tooling. Release binaries embed the
qualified production assets and need no companion files or Node at runtime.

## HTTP and CSP

Preserve IPv4-loopback binding, exact Host checks, Origin/Fetch Metadata checks,
capability authentication, strict framing, mutation protections and bounded
shutdown. Preserve the narrow top-level root navigation bootstrap exception.

Add only the stylesheet permission needed for local CSS (`style-src 'self'`).
Keep scripts/connect limited to self, default deny, no inline/eval allowances,
no framing, no referrer and no-store behavior. Components, charts, highlighting
and theme updates must work under the real policy, not a relaxed test policy.
Use CSS classes, safe SVG attributes and compiled styles instead of injecting
inline style attributes. Test native browser enforcement of the policy.

Vite development uses synthetic fixtures by default. Do not relax production
CORS/Origin/CSP or proxy real capabilities through a dev server to obtain HMR.
Integration/browser tests consume the Rust-served production frontend.

## Timing model

Use one monotonic per-forward origin. Wall-clock start time is only a display
anchor; wall-clock changes cannot change durations. Serialize integer microsecond
offsets/durations (`*_offset_us`, `duration_us`) relative to forwarder entry,
not Instant values. Enforce nonnegative JS-safe integers; an unrepresentable
measurement is explicitly unavailable, not silently rounded or saturated.

Represent a fixed, finite set of intervals/milestones, never a per-chunk event
log or growing per-address attempt list. Outcomes distinguish not-started,
skipped/not-applicable, active, completed, failed, cancelled and unavailable.
Missing data is not zero. Finalization happens once and cannot resurrect evicted
history. Preserve measurements already observed when a later stage fails.

| Observation | Meaning |
| --- | --- |
| Client receipt / metadata | Entry into the client forwarder and completion of authenticated metadata decoding; raw transport marks metadata decoding not applicable. |
| Request-head probe | Time spent obtaining the bounded initial request head; cap/timeout/EOF remains explicit and does not become complete parsing. |
| Resolve | Actual localhost resolution when needed; explicit numeric addresses mark resolution not applicable. |
| TCP connect | Existing attempts to the backend, with their existing aggregate deadline and outcome; do not silently include a new resolver deadline. |
| Request writes | Local backend-write activity, including backpressure. Completion requires the appropriate observed framing/write evidence; intended bytes are not accepted bytes. |
| Backend first byte | First actual nonempty backend read, before the multi-byte HTTP-prefix probe completes. It is not time spent executing backend application code. |
| Final response headers | A completely observed final head, distinct from informational heads and first byte. Missing/capped/malformed evidence remains unavailable. |
| Response reads and writes | Backend-read and tunnel-write activity with independent outcomes. Successful tunnel writes do not prove visitor/browser delivery. |
| Upgraded lifetime | A successful upgrade and subsequent opaque connection lifetime, without frame capture or replay eligibility. |

Render overlapping intervals on one relative axis. Never sum upload, wait and
response spans to claim total latency. Name first-byte offsets/interval origins
explicitly; do not call overlapped upload time backend processing time.

HTTP framing completion, transport EOF, local accepted writes and overall task
termination are different observations. Preserve existing provisional framing
revocation. An EOF may terminate an IO activity while the message remains
truncated. Synthesized responses must be labeled client-synthetic; unavailable
backend milestones must not be filled using synthesized header/byte times.

Live SSE/long responses remain active until observed termination. For WebSockets,
separate the HTTP handshake from upgraded lifetime and never label connection
age an HTTP response-body transfer duration. Forced stop, cancellation, failed
resolve/connect, early responses, malformed replies, 1xx and superseded sessions
must produce truthful partial timelines.

Detailed spans originate at the normal forwarding IO boundaries. Replay-origin
records must show only actual executor/outcome measurements already available;
missing detailed phases are explicitly unavailable. This design does not replace
or refactor the separate replay executor to fabricate uniform phase coverage.

## Forwarding implementation constraints

Record early timestamps in fixed stack state before capture admission. Do not
allocate labels, counter Arcs, event vectors or retained payloads before the
existing active-capture permit. Transfer fixed timing state into an admitted
record; declined/off inspection must not acquire new retained state.

Instrument actual resolver/dial/read/write/parser boundaries. A metadata-only
observer must not influence forwarding decisions, wait on UI work, create
background upload tasks, capture a second copy of request bytes, retry a stream,
or reinterpret a late final response as new evidence for Host Auto rewriting.
Preserve inverted yamux roles and the graceful driver shutdown fix.

Recalculate bounded capture/serialization memory after adding timing fields.
Keep per-record/event capacity finite and prove maximum-size API responses still
fit their limits. Do not weaken privacy, accounting or pressure tests to fit the
new schema. Bound response rendering and dispose stale polling/timers.

## Replay state machine

Review/prepare, explicit per-attempt acknowledgment and confirmation stay
separate. Preparing does not execute. Confirmation consumes local ticket state
synchronously before awaiting IO. Do not trigger mutations from effects,
selection, remount, polling, reconnect, theme changes or refresh. No automatic
mutation retry. Unknown delivery remains unknown; safe outcome GETs cannot grant
a new execution. Preserve raw-retention consent, original-backend restrictions,
server epochs/one-use authority and WebSocket/incomplete-request exclusions.

## Verification and completion

- Rust deterministic boundary tests: phase ordering, overlaps, immediate first
  byte versus delayed prefix, 1xx versus final head, partial writes, EOF/truncated
  frames, early responses during upload, slow/backpressured streams, failure,
  cancellation, DNS skip/failure and actual socket closure. Use controlled
  readers/resolvers/clocks and barriers; no network blackholes or timing sleeps
  as the correctness oracle.
- Existing off/on identity, Host Auto, replay authority/privacy, upload streaming,
  transport close, reattach/recreation and memory-pressure regressions remain.
- Frontend type/lint/unit tests cover generated contracts, stale reads, eviction,
  theme/system changes, unavailable timings, long/hostile text, explicit replay
  transitions and no mutation retries under remount or repeated input.
- Real bounded browser tests use compiled embedded assets: CSP, XSS canaries,
  redaction, themes and F5, capability rotation, no storage, no automatic replay,
  duplicate-click single execution, error/disconnect states and responsive views.
  Preserve the independent process-group watchdog and child-cleanup regressions.
- Clean-checkout production build and asset-staleness failure tests demonstrate
  that generated files are not an undocumented local prerequisite. Verify package
  contents and explicit binary/image artifacts for each distribution actually
  claimed. Server-only builds must not require frontend assets.
- Run repository fmt/clippy/workspace gates, frontend checks, contract generation,
  production build and the enabled browser scenario. Report genuine internal
  skips separately. Record the source/tooling/asset/binary hashes of qualification.

Implementation starts only after this design is reviewed. Final package,
installation/deployment and Git publication are separate explicit actions.
