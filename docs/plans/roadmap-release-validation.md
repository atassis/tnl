# Roadmap release-validation task specification

**Status:** Sections 1–11 are the original design proposal, not blanket approval. Bounded T7 phase1 actual-Caddy and phase2 executable-harness qualification are implemented; see §§12–13 for authority, measured results, and remaining gates.
**Goal:** Qualify one reconciled release containing visitor IP, shared request IDs, application WebSockets, inspection/history, replay, and per-tunnel access controls; perform one coordinated deployment followed by one integrated acceptance campaign, not seven independent deploys.
**Architecture recommendation:** Keep feature logic and unit tests in its owning lane. Extend the existing Rust e2e crate with isolated interaction fixtures, then add an explicit packaged-binary/Caddy acceptance gate and a deployment evidence/rollback checklist. Test the same candidate artifacts that will be installed.
**Stack:** Rust 1.94 toolchain, Tokio, axum/hyper, async-tungstenite/yamux, existing Cargo workspace; Caddy and Docker only when explicitly provisioned/authorized.
**Authority:** Parent/user must reconcile lane designs and approve deployment facts before implementation or remote execution. This task does not approve another lane's API.

## 1. Inspection record and verified baseline

Inspected only the `release-validation` worktree and local tool/skill metadata. No credentials, remote hosts, Docker daemon, DNS, package downloads, build/test workloads, commits, merges, or deployment were accessed/run. Output is the authoritative external artifact requested for this run; no repository files changed. Read `AGENTS.md`, applicable writing-plans guidance, and documentation skill guidance; task-specific restrictions override generic skill suggestions to commit, spawn workers, or reorganize docs.

### Git/tooling

- Current branch `feat/roadmap-release-validation`, HEAD `43f2bc0ed29e00821e1ede624b8fe190eb0d5c57`; initially clean index/worktree.
- All seven roadmap worktrees and `prep-public` point at this baseline. Local `main` is `48f462c04894b076ed413171b7bec000aa6d8d36`, an ancestor 20 commits behind HEAD. Locally recorded `origin/main` points at HEAD; this is not a freshly verified remote fact. Parent must integrate against the intended baseline, not accidentally start from stale local `main`.
- `Cargo.toml:1-17`: four crates, workspace package `0.1.0-beta.4`, Rust minimum 1.94. `rust-toolchain.toml`: channel `1.94`, rustfmt/clippy. Installed active tools report `rustc 1.94.1` and `cargo 1.94.1`; record the actual patch version in release evidence.
- Docker executable exists; daemon availability and cached images are unverified. No Caddy executable was found on PATH. There is no `.github/workflows/` in this baseline. Neither green CI nor a locally usable Caddy/image can be assumed.

### Current deployment contract, not a description of a verified installation

| Evidence | Verified behavior / implication |
|---|---|
| `deploy/README.md`, `deploy/.env.example` | Operator supplies wildcard domain, ACME contact, DNS-provider module/token. The example is not a real target inventory. No target hostname/account/path has been established. |
| `deploy/Caddyfile:14-31` | Wildcard data and more-specific API virtual hosts proxy to loopback `127.0.0.1:7777`. Wildcard uses `flush_interval -1` and **`header_up Connection close`**; API does not. No explicit upstream trusted-proxy configuration appears here. Verify exact Caddy version/header behavior, not assumed XFF semantics. |
| `deploy/Caddy.Dockerfile` | Floating `caddy:2-builder`/`2-alpine`, Cloudflare DNS module built by xcaddy with no version pin. Rebuilding later can change behavior without a tnl source change. Record/reuse an immutable built image for qualification/rollback. |
| `deploy/tnld-compose.yaml.example:8-23` | `tnld:latest`, fixed container name, host networking, restart always, `/etc/tnld:ro`, debug logging, healthcheck every 10s. Caddy service/network/install topology is not supplied. A Caddy container must share a suitable network namespace to reach host loopback; Docker bridge loopback is not the host. |
| `Dockerfile:4-48` | Alpine/musl multi-stage build with dependency-stub cache; only daemon binary packaged, stripped, non-root runtime, tini, config-based healthcheck. The client must be built/distributed separately. Test the resulting image, including real `serve`, not merely a native debug build or version string. |
| `crates/tnld/src/config.rs:6-107` | Required TOML: `listen`, `public_url`, `hostname_root`, `tokens_file`; `session_grace_sec=30` default. Explicit corresponding `TNLD_*` overrides/warnings. No trusted-proxy/access-policy config currently exists. |
| `crates/tnld/src/serve.rs:55-71`; `commands/healthcheck.rs` | Normal server startup refuses empty token store; healthcheck only probes configured `http://<listen>/healthz` with 3s timeout. It does not establish tunnel, policy, TLS, upgrade, or identity readiness. |
| `crates/tnld/src/commands/init.rs:124-154,170-181,234-237` | `init` can mint a supplied synthetic token noninteractively, writes TOML, prints token once; its generated Caddy snippet is simpler than checked-in Caddyfile. Include snippet consistency in documentation reconciliation. Do not use `init -y` on production config during release. |
| `crates/tnld/src/admin.rs:97-116`; `deploy/tnld-compose.yaml.example:17` | Pair redemption appends a token to disk, whereas example config mount is read-only. This is a production compatibility risk to disclose, not a mandate to broaden this lane into pairing/storage redesign. |
| `crates/tnld/tests/file_perms.rs:12-110` | Docker tests can return successfully after printing SKIP and use hardcoded beta.1 image. The purported config-read case actually invokes `hash-password test`, which does not read config. Existing green suite is not sufficient mounted-config/non-root packaging evidence. |

### Actual request path and existing tests

1. `crates/tnld/src/serve.rs:126-150` registers `/healthz`, bearer-protected `/whoami` and `/control`, admin/pair routes, and a fallback data plane. These routes are path-based on the same listener, not separated by Host in the daemon. Reserved paths can shadow backend application paths on wildcard hosts; test/document this existing restriction rather than accidentally claiming every URL is forwarded.
2. `crates/tnld/src/control.rs:17-179` authenticates/control-upgrades, reads length-prefixed tagged JSON, and creates/reattaches registry tunnels. `crates/tnl-protocol/src/messages.rs:22-36` has `CreateTunnelReq { subdomain: Option<String> }` only: no access policy or feature negotiation today. Serde ignores unknown struct fields by default, so sending a future policy to an old daemon could appear successful while leaving a tunnel unprotected. New security options require positive acknowledgment or fail-closed version/capability handling, owned by access-controls.
3. `crates/tnld/src/data_plane.rs:48-56,182-241,307-328`: daemon allocates a ULID, opens one yamux stream per request, drives hyper HTTP/1, forwards streaming body, relays response. `strip_hop_by_hop` at lines 10-46 removes Connection-nominated fields, framing, and Upgrade in both directions; request forces Connection close. No data-plane `with_upgrades` path exists. Therefore existing control-plane WSS is not evidence of application WebSocket support.
4. `crates/tnld/src/data_plane.rs:330-350` includes `X-Tnl-Request-Id` on daemon-generated errors, not all successful responses. `crates/tnl/src/forwarder.rs:180-185` independently allocates another ULID, used by local synth errors and inspector. Current IDs are not end-to-end shared.
5. Daemon is supplied `ConnectInfo<SocketAddr>` (`serve.rs:119-123`) but data-plane handler does not consume it or normalize trusted forwarding headers. `messages.rs:49-59` defines a protocol `LogLine.remote_ip`, but live forwarder uses a **different** `tnl::inspector::LogLine` (`inspector.rs:9-28`), with request ID/target and no visitor IP. Do not accidentally test the unused protocol log type.
6. `crates/tnl/src/main.rs:45-82,205-246` -> `commands/http.rs:5-43` -> `reconnect.rs:55-180` -> `client.rs:191-216` -> `forwarder.rs`: actual CLI constructs a 1024-line inspector channel, retains control stream, spawns per-stream forwarding. `forwarder.rs:523-528` drops log lines when channel full/closed. Existing `--json` logs have `req_id`; output also contains banner/shutdown text from reconnect/http. It is not a pure NDJSON protocol. Verbose variants currently do not implement header/body history.
7. `forwarder.rs:188-200,240-252,422-457`, `peek.rs:10-11`: request-head probe cap 8 KiB; first-response sample feeds status and host-block detection. Host auto-rewrite changes subsequent requests after a recognized denial; learned state survives reconnect (`reconnect.rs:67-71`). Raw bytes are pumped bidirectionally after initial response probe. No persistent history, history-reading CLI, or replay command exists in `main.rs` or the live path inspected.
8. `registry.rs:13-43,185-241` keeps descriptors, stats, and mutable session bindings in memory; reattach checks owner/id/subdomain and disconnected state. New policy must survive reattach and fresh-create fallback, without broadening another lane's implementation here.
9. `tnl-e2e/src/lib.rs` is intentionally empty; tests duplicate setup in `tests/*.rs`, use random loopback ports, temporary token files, library server/client calls, often fixed sleeps. Coverage includes roundtrip, explicit IPv4/IPv6 targets, chunked/103/malformed response attribution, many headers, local-down errors, inspector summary, host rewrite/preserve, reconnect, status/stop, and random subdomains.
10. `tests/wss_dial.rs:21-117` only rejects the specific missing-TLS-feature error; it tolerates certificate failure. It does not prove successful TLS verification/control connection. `tests/inspector_e2e.rs` checks one summary log, not capture/history/security. None of the existing e2e tests traverses actual Caddy or installed release binaries.
11. `tnl/src/commands/version.rs:5-20` returns only name/version; daemon `main.rs` uses Clap package version. No `/version` or build-commit API exists in the current router. README Status and `docs/RUNBOOK.md` still say beta.1, despite package beta.4. Historical design claims of transparent WebSocket/h2c, headers/body inspection, anti-bruteforce, or richer version output are not implemented contracts (compare original spec §§wire/data-plane and beta spec deferred scope/inspector/version sections).

## 2. Approaches and recommendation

| Approach | Benefits | Costs / gaps |
|---|---|---|
| A. Extend only current in-process Rust tests | Fast, deterministic, no credentials, best diagnosis of interaction failures | Misses actual executable CLI wiring, musl packaging, Caddy normalization/upgrades, deployed TLS/identity. Insufficient release gate alone. |
| **B. Layered tests + one coordinated release gate (recommended)** | Feature tests remain small; one explicit binary/Caddy matrix catches integration and packaging issues; same immutable candidate later receives deployed smoke/interaction checks | Requires provisioning Caddy/images and recording target evidence; more fixture lifecycle code. Offline/local HTTP proxy testing cannot prove real public WSS/cert/DNS. |
| C. Disposable replica of whole production stack for every test/feature | Strong infrastructure isolation, can reproduce multiple ingress hops | Heavy Docker/DNS/plugin setup, duplicates existing harness, credential pressure; seven separate stacks/runs obscure combined compatibility. Not needed for this bounded release. |

Recommend B. Build/test once after reconciliation; install exactly the qualified digests once; run one campaign containing local adversarial cases and deployed real-ingress cases. A failure may require rollback and a new candidate/campaign; “one shot” must not prohibit safety fixes/retesting. No publication/CI infrastructure redesign in this task.

## 3. Decisions to reconcile before implementation

These are material approval points, not silently selected defaults. Parent should gather lane answers in one contract table before assigning implementation.

- **D1 — Target and release authority:** Intended main/integration baseline, release version, operator, maintenance window, target OS/arch, container vs native daemon, client installation paths, Caddy topology/version/image, domains, existing gateway routes, and who can authorize rollback. Unknown today.
- **D2 — Visitor trust boundary:** Which immediate proxy peers/CIDRs are trusted; one Caddy hop vs CDN/LB chain; authoritative header and chain-selection rules; malformed/duplicate/missing header semantics; direct access behavior. Recommend explicit trust of actual immediate proxy only and rejection/removal of forged internal metadata; do not trust an entire “private network” or arbitrary XFF. Visitor-ip owns exact config/header names.
- **D3 — ID contract:** Canonical header spelling, generated-vs-preserved external ID, internal spoof handling, backend response collisions, daemon errors vs local errors, inspector/history and replay linkage, WS handshake ID. Recommend server-generated canonical IDs on all tnl responses and replay gets a new ID linked to the original. Request-ids/replay own precise shapes.
- **D4 — WebSocket lifetime:** Supported HTTP/1 upgrade/subprotocol/origin behavior, graceful close/abrupt disconnect, idle timeouts, what history records, and whether live upgraded connections hold concurrency permits until closure. Recommend handshake metadata only (not frame history/replay), permits held for lifetime. Websockets/access-controls/inspection must agree; explicitly no HTTP/2 extended CONNECT claim.
- **D5 — Inspection/history confidentiality:** Opt-in defaults, retention/count/byte caps, file location/mode, redaction for headers/query/body, capture truncation and backpressure semantics, terminal/JSON interface, owner isolation. Recommend bounded local-only storage, credentials redacted by default, no silently lossy records presented as replayable. Inspection owns API/schema; release lane does not introduce a public history service.
- **D6 — Replay route and safety:** Local-target vs public tunnel re-entry; redirects, destination overrides, auth re-entry, unsafe method confirmation, truncation refusal, redacted secret handling. Recommend explicit local-only replay to fixed configured target with no redirects/arbitrary URL and no WS replay, but this bypasses daemon admission by design and must be stated. If user selects public replay, expect all basic/IP/rate/concurrency rules to apply; never add a hidden bypass. Replay/access-controls must settle this before acceptance assertions are frozen.
- **D7 — Policy semantics:** Basic-auth credential source/hash/redaction and Authorization consumption vs pass-through; CIDR family handling; rejection order/statuses; rate scope/algorithm/burst/Retry-After; concurrency lifetime and release; persistence over reattach/recreate; positive policy acknowledgment on old-server mismatch. Recommend unauthorized/disallowed traffic never reaches backend; handshake checks same as HTTP; defaults retain unprotected behavior only when no policy was requested. Access-controls owns exact CLI/wire config.
- **D8 — Evidence and rollout limits:** Retention/access to captured evidence, numeric resource/time thresholds, accepted legacy client/server combinations, immutable-image policy (vs changing repository Caddy pinning), production destructive-test budget. Proposed suite budgets below are isolated fixture settings, not production defaults.

## 4. Bounded ownership and dependency graph

**Release-validation owns:** proposed `crates/tnl-e2e/tests/release_interactions.rs`, `tests/release_acceptance.rs`, `tests/support/release.rs`, and `tests/fixtures/release/`; `scripts/release-acceptance.sh`; `docs/plans/roadmap-release-validation.md` (future repository copy after design approval); `docs/RUNBOOK.md` and `deploy/README.md` release sections. Changes to `crates/tnl-e2e/Cargo.toml` limited to existing workspace dev-dependencies needed for fixtures (e.g. futures for websocket SinkExt); use workspace dependency conventions. Do not move all old test setup or change `src/lib.rs` into a new framework.

**Shared files, not independently edited by this lane:**

| Files/seam | Feature ownership / reconciliation |
|---|---|
| `tnld/src/data_plane.rs`, `serve.rs`, `config.rs` | visitor-ip, request-ids, websockets, access-controls overlap; parent serializes combined patch/review. Release contributes tests, not competing middleware. |
| `tnl/src/forwarder.rs`, `peek.rs`, `inspector.rs`, `reconnect.rs`, `client.rs`, `main.rs`, `commands/http.rs` | request-ids, visitor-ip metadata, websockets, inspection, replay, access-controls overlap. Freeze shared context/metadata/CLI contracts before merging. |
| `tnl-protocol/src/messages.rs`, `tnld/src/control.rs`, `registry.rs` | access policy, capability acknowledgment, logging identity, reconnect compatibility. Feature owners provide wire tests, including old daemon fail-closed cases. |
| `deploy/Caddyfile`, generated snippet in `tnld/src/commands/init.rs`, `deploy/config.example.toml` | visitor-ip and websockets propose production proxy changes; reconcile as one patch. Release owns tests against approved result and documents required topology, not an alternative proxy config. |
| `Dockerfile`, `deploy/Caddy.Dockerfile`, Compose reference | Existing supported packaging. Parent approves pin/label/build corrections only if qualification requires them; no speculative packaging rewrite. Release documentation should use immutable candidate image IDs/tags without needing to redesign these files. |
| Root `Cargo.toml`, `Cargo.lock`, `README.md`, `CHANGELOG.md` | Parent consolidates dependency/version/release claims; release lane supplies verified commands/results. |

**Order:** baseline/target decisions -> shared trust+ID+policy/WS contracts -> feature lane unit/CLI tests -> feature implementations and conflict reconciliation -> inspection/replay metadata/policy interactions -> combined local suite -> package candidate and local real-Caddy gate -> operator-approved single deployment -> same candidate's deployed matrix and evidence -> accept or rollback. Inspection needs shared ID and WS representation; replay needs complete/redacted history semantics; IP allowlists need normalized trusted visitor identity. No feature deploys before reconciliation.

**Non-goals:** Implementing any feature in another lane; remote execution in this phase; acquiring credentials/Caddy packages; cert/DNS automation; benchmarking >100 tunnels; fixing unrelated pairing/admin security; TCP/UDP, QUIC, h2c, public inspector UI, WebSocket frame replay; CI/publication/signing infrastructure; zero-downtime or multi-host migration.

## 5. Proposed release harness interfaces (not existing product CLI)

Prefer Rust integration tests with real process invocation over shell-implemented HTTP/WebSocket clients. Wrapper only validates inputs, orchestrates existing Cargo commands, computes hashes, and preserves evidence. Never `eval` feature flags or run shell fragments from a manifest.

### 5.1 Commands and modes

Proposed wrapper interface:

```text
scripts/release-acceptance.sh --mode local|deployed --manifest ABS_JSON --evidence ABS_NEW_DIR
```

- Exactly one mode; evidence dir must be new, mode 0700, files 0600. Manifest is sanitized nonsecret data, not a credentials file. Reject unknown schema versions, relative executable paths, missing required inputs, dirty candidate checkout, or hash mismatch before sending traffic. Local development may run individual Cargo tests from dirty source, but cannot generate a release PASS record.
- `local`: runs fmt check, clippy, full workspace tests, then ignored packaged/Caddy test. Never downloads, builds Docker images, contacts public endpoints, auto-installs Caddy, or deploys. Uses candidate executables and loopback ephemeral ports; local Caddy missing is **error**, not SKIP/PASS.
- `deployed`: no product build, deployment, remote shell, file installation, token minting, or configuration modification. Runs the acceptance test with prequalified client/probe executables and operator-provided sanitized server identity evidence; only creates/stops run-prefixed disposable tunnels and traffic to their controlled backend. Cargo may compile the test harness offline from the frozen commit, never substitute newly built product binaries. Destructive reload/restart cases remain local; deployed tests may not disrupt unrelated tunnels.
- Process inputs passed to Rust: `TNL_ACCEPTANCE_MODE`, `TNL_ACCEPTANCE_MANIFEST`, `TNL_ACCEPTANCE_EVIDENCE`; these are test harness variables, not product env changes. For deployed credential injection use `TNL_TOKEN` only in child process environment, never printed. Child `TNL_CONFIG` points into private temp state, with endpoint via supported `TNL_ENDPOINT`; no writes to user's normal config.
- Wrapper exits 0 only when all required cases pass and evidence is complete; 1 for observed acceptance failure, 2 for invalid/unavailable prerequisites or required NOT-RUN. Cargo's own exit code is captured in evidence rather than confused with wrapper status.

### 5.2 Manifest and outputs

Proposed manifest v1 has exact top-level fields: `schema_version` (1), `candidate` object, `mode_config` object. `candidate` fields: `commit` (full hex SHA), `version` (workspace version), `lockfile_sha256`, `rustc_version` (full `rustc --version` output), `target` (Rust target triple), `tnl_path`, `tnl_sha256`, `tnld_path`, `tnld_sha256`, `daemon_image_id` (string or null), `daemon_image_exe_sha256` (string or null), `caddy_image_id` (string or null). Paths are absolute paths to retained qualification artifacts on the runner; hashes refer to actual artifacts, not similarly named PATH binaries. Native and musl/stripped image daemon have separate hashes; never compare unlike builds as identical bytes. For a native deployed daemon, running/installed hashes must match `tnld_sha256`; for a container they must match non-null `daemon_image_exe_sha256` and the observed image ID must match `daemon_image_id`.

- Local `mode_config` fields: `caddy_path` (absolute), `caddy_sha256`, `hostname_root` (`t.test`), `timeout_sec` (300). Harness creates ports/config/tokens. Endpoint/keys are not supplied by a production env file. If the deploy artifact is containerized, package qualification also supplies immutable image IDs and runs image checks in task 6 before local gate PASS.
- Deployed `mode_config` fields: `endpoint` (https URL), `hostname_root`, `run_prefix` (DNS label prefix unique to campaign), `allowed_visitor_ips` (observed test driver egress IP strings), `denied_probe` (sanitized description of the approved second vantage or null), `server_evidence_path` (absolute sanitized JSON), `timeout_sec` (300). No tokens, passwords, DNS tokens, or arbitrary argv/URLs. Null denied probe makes deployed cross-source test NOT-RUN; local forged-IP denial coverage still mandatory. Full approval cannot hide this gap.
- Proposed server evidence v1 fields: `schema_version`, `observed_at`, `candidate_commit`, `runtime_kind`, `daemon_path`, `daemon_sha256`, `daemon_image_id`, `container_image_id`, `running_exe_sha256`, `daemon_version`, `caddy_version`, `caddy_image_id`, `config_sha256`, `caddy_config_sha256`, `operator_verified` (boolean). Container/path fields may be null only when not applicable. It contains no config/token contents. Parent/operator captures identity through approved host access; harness does not invent evidence by reading a client-reported version over HTTP.
- Results: `summary.json` with `schema_version=1`, `mode`, `candidate_commit`, UTC `started_at`/`finished_at`, `overall` (`pass|fail|incomplete`), `cases` array, `artifacts` array. Each case: `id`, `status` (`pass|fail|not-run`), `duration_ms`, `evidence_files` (relative paths), `reason` (sanitized string). Artifacts entries: `path`, `sha256`. Keep redacted process logs, selected request/response headers, backend observations, canonical ID relationships, and versions/hash output. Do not dump process env or raw captures. No PASS if required evidence missing.

### 5.3 Feature seam contract for tests

No new product flags, REST endpoints, history schema, or wire messages are invented here. After decisions D2-D7, record exact approved signatures/flags/fields in the repository plan and compile direct adapters in `tests/support/release.rs`; do not build a runtime plugin/configuration language. Required adapter responsibilities:

- Start tunnel with explicit target and named/random subdomain, optional approved access policy and inspection settings, using real `tnl` CLI in binary gate.
- Await registered/ready tunnel using bounded polling of an actual request/status condition, not arbitrary sleep or banner text alone.
- Read one normalized inspection/history record through the approved supported interface and return its ID, visitor, status, headers/body capture state, and replayability. Preserve raw output in sanitized evidence; adapter must fail on missing/unknown required fields.
- Invoke replay through approved CLI/API against one captured record; verify original/new identity linkage and destination semantics. Never use direct internal backend requests as a substitute for testing the replay interface.
- Query backend-owned counters/observations directly on a separate loopback-only fixture admin port. Paths like `/observe`, `/hold`, `/echo-ws` on the tunneled app are fixtures, not new daemon APIs. Backend counters prove denied requests never arrived.

## 6. Numbered implementation subtasks

Each task starts with its specified failing test, demonstrates the failure, implements the narrow harness change, and re-runs the targeted command. Feature behavior failures go back to their owning lane; release code must not weaken assertions to manufacture green.

1. **Freeze contracts and case selection.** Parent resolves D1-D8 and records lane API/CLI examples, supported version pairs, numeric limits, redaction policy and exact rejection status/error headers. Release owner updates the plan plus deployment inventory checklist. Check: each matrix row below has an approved observable and owner; unresolved security semantics block execution, not mark PASS.
2. **Add isolated support fixtures.** Create `tests/support/release.rs` plus fixture directory, no blanket old-test refactor. Add echo/header/method/body observation, chunked stream, held response, raw malformed backend, WS echo/subprotocol/close, private observation counters. Use ephemeral loopback ports, temporary config/tokens/history and bounded readiness; collect children/tasks for teardown on errors. Add `release_interactions.rs::fixture_cleanup_and_denial_observation` proving failure cleanup frees listeners and zero counter remains meaningful. Command: `cargo test --locked --offline -p tnl-e2e --test release_interactions fixture_cleanup_and_denial_observation -- --exact`.
3. **Add feature interaction cases.** Implement default nonignored `release_interactions` tests for matrix I01-I12 using approved library seams or supported feature interfaces; source-named Rust test names match row descriptions. Avoid global env mutation inside concurrent tests: inject config where supported or run child processes. Barrier/oneshot-based held requests and clock-controlled unit tests replace rate/concurrency sleeps. Command: `cargo test --locked --offline -p tnl-e2e --test release_interactions -- --test-threads=1`. Regression matrix must fail if spoofed IP/ID reaches backend, redaction leaks, replay silently changes target, or permit remains stuck.
4. **Add binary and real-Caddy gate.** Create `tests/release_acceptance.rs::release_acceptance_matrix`, explicitly `#[ignore]`, invoked only by wrapper. Local harness starts supplied `tnld` executable with synthetic config, supplied `tnl`, then actual supplied Caddy process. Generate local HTTP fixture from the **approved** production Caddy proxy directives: only TLS/ACME/site addresses/upstream port/admin/storage isolation differ. Add loopback API host alias so real client's `http://127.0.0.1:<port>/control` is routable; visitor data requests use real wildcard Host. Bind only loopback, disable automatic HTTPS and admin, no DNS/public routes. Preserve source + adapted config hashes and assert relevant header/upgrade directives match production; no hand-rolled fake proxy. Local HTTP tests prove Caddy forwarding logic, not public TLS. Run rows C01-C05 and I rows applicable to all features using actual binaries. Command under wrapper env: `cargo test --locked --offline -p tnl-e2e --test release_acceptance release_acceptance_matrix -- --ignored --exact --test-threads=1`.
5. **Implement wrapper and evidence validation.** Create `scripts/release-acceptance.sh` with interface above and manifest/result parsers in Rust support code; reuse existing dependency versions, no new heavyweight test framework. Add tests `manifest_rejects_wrong_binary_hash`, `manifest_rejects_missing_case_evidence`, `missing_caddy_is_error`, `redacted_evidence_contains_no_fixture_secrets`. Wrong identity/unavailable Caddy must fail before fixture traffic; denied token/password/query values must not enter summaries or process logs. Command: `cargo test --locked --offline -p tnl-e2e --test release_interactions manifest_` and full targeted suite.
6. **Qualify packaged candidate and compatibility.** Build from integrated clean commit with lockfile and toolchain recorded; capture client/native/image hashes separately. Operator/build owner builds immutable daemon/Caddy images using provisioned inputs (may need separately authorized network; not part of offline wrapper). Test new image's non-root user, real config/token directory readability, actual `serve` readiness and `healthcheck`, empty/invalid token-store refusal, restart/reconnect. Do not reuse hardcoded beta.1 `file_perms` success as evidence. Compare cached and clean image build behavior at least once to cover dependency-stub path; differences block candidate. Verify supported legacy pairs and security-policy negotiation per I12. Record exact commands/image IDs/results; no implicit pulls. Dockerfile or image-permission defect fixes require parent coordination, not unilateral source scope expansion.
7. **Run one post-integration local campaign.** Only after all lane tests/conflict review pass, run wrapper once on immutable artifacts and freeze `summary.json`/manifest. All local required cases must pass, including Caddy. Full workspace fmt/clippy/test checks remain mandatory; absence of cached dependencies is an offline prerequisite failure, not an excuse to fetch during restricted execution. Re-run when the combined commit/config/image changes; evidence from seven branches is not an integrated result.
8. **Prepare operator runbook and rollback rehearsal.** Update `docs/RUNBOOK.md`/`deploy/README.md`: version truth, full binary paths, trust-boundary diagram, required identities, opaque token placeholders, secret-safe evidence, exact image/config references, observed reserved routes, and rollback procedure below. Rehearse rollout/rollback locally with an active HTTP stream and WS; validate reconnect and policy restoration. Parent consolidates README/CHANGELOG claims; do not claim public deployment passed from localhost.
9. **One authorized rollout and deployed acceptance.** Parent/operator performs approved procedure, installs same qualified IDs, supplies server evidence, then runs wrapper deployed mode on reserved disposable hostnames. Execute deployed-required rows, normal API/control/HTTP/WS checks and approved external trust probes; keep unrelated traffic untouched. Missing identity/actual-Caddy/TLS proof => incomplete, never accepted. Owner signs final campaign result; failures trigger rollback thresholds below.

## 7. Acceptance matrix and meaningful negative cases

Every row is required locally unless noted. `I` rows also form feature-lane handoff checks; `C` rows specifically prevent a library-only false positive. Deployed subset is additive evidence, not permission to rerun destructive fault injection on shared production.

| ID / proposed test | Cases and assertions | Owners; deployed applicability |
|---|---|---|
| I01 `visitor_identity_survives_host_rewrite` | HTTP GET/POST via trusted proxy and direct daemon; allowed IPv4/IPv6; forged XFF/Forwarded/internal visitor headers, duplicate/invalid/missing chains, untrusted peer. Backend and inspector agree on approved canonical source, not loopback/spoof. Repeat with preserve/fixed/learned Host rewrite; proxy source metadata stays unchanged. | visitor-ip + request-ids; deployed real source + spoof probe required, IPv6 dependent on target inventory |
| I02 `shared_id_spans_success_and_failure` | Success 200, backend 401/429/500, unknown tunnel 404, grace 503, local refused/eof/malformed/no-response 502. Canonical ID is same at every participating hop/log/history/error body; forged input and backend response ID cannot override approved origin; parallel requests unique. Daemon-before-forwarding failures need daemon evidence, not nonexistent client records. | request-ids; deployed success, unknown run-host, controlled local-down required |
| I03 `http_framing_and_streaming_remain_correct` | POST/PATCH binary and UTF-8 bodies, empty/HEAD/204/304, duplicate end-to-end headers (Set-Cookie), chunked request/response, leading 103, >64 response headers; Connection-nominated fields removed. Stream delivers first chunk before backend finishes; large body > capture cap still fully relays. Abort mid-body releases resources. | websockets + inspection; deployed small GET/POST/HEAD/stream required |
| I04 `application_websocket_lifecycle` | Valid 101; text/binary, ping/pong, selected subprotocol, echoed Origin according to contract, fragmented/large message within agreed bound, graceful close both directions, abrupt EOF, concurrent HTTP while WS open. No HTTP framing/Connection-close contamination after upgrade. Invalid/missing upgrade tokens and rejected backend handshake return honest non-101; no hung child/session. | websockets; deployed wss echo/close + parallel HTTP required |
| I05 `history_capture_is_bounded_and_redacted` | Opt-out/default vs approved opt-in; headers/query and text/binary bodies; cap-1/cap/cap+1 bytes, invalid UTF-8, duplicate headers, endless stream/WS. Stored completeness/truncation correct; bounded count+bytes/eviction; secrets in Authorization, Proxy-Authorization, Cookie, Set-Cookie, basic config and approved sensitive fields absent from terminal/JSON/disk/evidence. Slow/full inspector must not stall traffic or create unbounded memory. Private file permissions and cross-tunnel access rejection. | inspection; deployed disposable capture/redaction required; disk/exhaustion local only |
| I06 `replay_preserves_payload_without_destination_escape` | Capture complete POST, replay via supported interface, exact method/path/query/body at allowed destination, new ID linked to original. Confirm unsafe method per approved UI. Missing/evicted/truncated/nonreplayable/redacted-secret-dependent record and WS record rejected explicitly. Absolute-form URL, Host manipulation, redirect to another host/loopback service, overridden forwarding headers cannot create SSRF or forge source/ID. No silent retry of side-effecting request. | replay + inspection + request-ids; deployed safe fixture counter POST only if explicitly approved |
| I07 `basic_auth_and_ip_gates_precede_backend` | Missing/wrong/malformed Basic -> approved 401 + WWW-Authenticate; disallowed source -> approved 403 even with valid Basic; valid credentials+CIDR succeeds. Counters unchanged for rejects; no credentials in history/log/status/config output. Decide origin Authorization behavior then test it, including case/duplicate Authorization. CIDR IPv4/IPv6 boundaries, malformed policy rejected at creation, fail closed on unknown visitor when allowlist is active per approved contract. | access-controls + visitor-ip; deployed auth and actual-source allow/deny required; second source if available |
| I08 `rate_and_concurrency_are_tunnel_scoped` | Proposed fixture rate 2/sec burst 2, concurrency 1: deterministic initial allowance then 429/Retry-After per approved algorithm; separate tunnel unaffected. Hold HTTP body or WS open: second admitted request rejected as approved; permit held until completion, released on EOF/cancel/backend error/failed upgrade/stop. Auth/IP rejects do not leak permits; specify whether they consume rate budget. HTTP and WS handshake share limits; WS frames do not become new HTTP requests. Max 16 fixture connections, no flood. | access-controls + websockets; deployed small bounded rate + one held WS only on test tunnel |
| I09 `replay_and_access_policy_interact_as_documented` | All policies enabled + captured request. Public replay: re-enters IP/basic/rate/concurrency checks and cannot reuse trusted visitor or canonical ID from history. Local replay: intentionally bypasses daemon ingress but can only reach fixed backend; UI/documentation/ID prove local origin and never claim admission-policy evaluation. Redacted secrets not reconstructed or silently replaced from another tunnel. | replay + access-controls; deployed safe approved route only |
| I10 `reattach_preserves_policy_and_history_identity` | Drop control locally, requests see bounded graceful 503, reattach same host; then expire reservation to force fresh CreateTunnel. Policy stays active in both paths; invalid bearer/other owner cannot claim protected state. History scope/replay destination and learned Host rewrite remain as approved. WS closure does not leak concurrency reservations or orphan records. | all lanes; forced faults local only; operator observes real reconnect after rollout |
| I11 `all_features_coexist_on_two_tunnels` | Tunnel A: all policies, history and WS; B: unprotected HTTP control. Allowed POST captured/replayed, ID/source correlated; WS on A reserves concurrency, concurrent A request limited, B unaffected. Close WS -> next A request succeeds. Switch A backend down -> attributed shared-ID 502 with sanitized history; restore -> healthy. Disallowed/spoofed requests never increment A backend counter. | all lanes; deployed non-disruptive subset required |
| I12 `legacy_security_negotiation_fails_closed` | Approved old client/new server HTTP continues under defaults; new client/old server ordinary HTTP supported or explicit documented failure. Requesting any unsupported protection must fail tunnel creation (not silently ignore fields). Verify reattach/recreate cannot downgrade protection. New additive log fields do not break documented JSON consumers; existing quiet/version/help behavior retained or versioned. | access-controls + protocol/CLI owners; compatibility pairs local only |
| C01 `actual_caddy_header_boundary` | Through real Caddy, observe canonical source/proto/host at backend, successful and spoofed XFF/Forwarded/internal metadata; trust matches observed immediate socket peer. Direct daemon bypass does not accept arbitrary headers. If CDN exists, test selected chain with approved trusted edges, not synthetic XFF alone. Adapted config assertions plus observed traffic required. | visitor-ip + release; actual deployed ingress required |
| C02 `actual_caddy_http_and_upgrade_paths` | Both virtual hosts, real `/control` connection, HTTP keepalive/streaming/framing, application WS upgrade on wildcard, rejection paths; no unconditional close breaks upgrade. API bearer vs per-tunnel Basic remain separate; reserved `/healthz`, `/whoami`, `/control`, `/tunnels`, `/pair*` behavior is explicitly documented/tested. | websockets + release; deployed required |
| C03 `installed_identity_matches_candidate` | Compare exact client path/hash and version, native/image daemon version/hash, immutable image ID vs running container's image ID, running executable where available, config hashes, commit/lockfile/toolchain/target manifest. Stale beta.1 PATH binary, same-version wrong hash, mutable tag retarget, wrong Caddy/config all fail. Feature-bearing traffic corroborates identity; `/healthz` alone is insufficient. | release + operator; deployed required |
| C04 `real_tls_and_control_auth` | Local current TLS-feature test remains but is not full proof. On real ingress verify valid cert hostname/chain, HTTPS API health, bearer success/failure, successful verified WSS control and application wss echo. Do not use `-k`/invalid-cert acceptance in release verdict. Wrong hostname/untrusted cert is rejected by normal client. No ACME acquisition in local gate. | release + operator; positive real TLS deployed required |
| C05 `image_config_readiness_and_rollback` | Actual image USER reads protected mounted config/token files; valid serve/healthcheck/real tunnel succeeds, empty token store fails. Replacing with previous image/config locally restores HTTP and policy expectations after reconnect; no raw secret output. Current false-positive file-perms case not counted. | release; local rehearsal + operator rollback readiness required |

## 8. Concrete command plan (not executed in this design phase)

### Per-lane and combined checks on provisioned local dependencies

```sh
# Each feature lane runs own unit/integration/CLI tests; these baseline suites are additional regression checks.
cargo test --locked --offline -p tnl-protocol
cargo test --locked --offline -p tnld
cargo test --locked --offline -p tnl
cargo test --locked --offline -p tnl-e2e
cargo test --locked --offline -p tnl-e2e --test full_roundtrip_response_framing
cargo test --locked --offline -p tnl-e2e --test host_rewrite
cargo test --locked --offline -p tnl-e2e --test reconnect
cargo test --locked --offline -p tnl-e2e --test release_interactions -- --test-threads=1

# Required clean checks for integrated candidate, no source reformatting during evidence collection.
cargo fmt --all -- --check
cargo clippy --offline --locked --workspace --all-targets -- -D warnings
cargo test --offline --locked --workspace
cargo build --offline --locked --release --workspace
rustc --version
cargo --version
./target/release/tnl version --json
./target/release/tnld --version
sha256sum Cargo.lock target/release/tnl target/release/tnld
```

Offline failure due to a missing tool/dependency/image is an incomplete prerequisite, never a test success. These commands have not been run here. Normal implementation follows AGENTS formatting requirements before commit; this phase makes no commits.

### Proposed combined invocation after tasks 1-6 and integration

```sh
# prepare candidate.local.json from the measured paths/hashes, outside tracked source
scripts/release-acceptance.sh --mode local \
  --manifest /absolute/private/candidate.local.json \
  --evidence /absolute/private/acceptance-local-new

# Only after explicit operator authorization, installation and identity capture:
# TNL_TOKEN is injected privately by operator; never written into manifest or command text.
scripts/release-acceptance.sh --mode deployed \
  --manifest /absolute/private/candidate.deployed.json \
  --evidence /absolute/private/acceptance-deployed-new
```

Packaging/identity commands for the later authorized operator (use actual approved immutable image reference in `CANDIDATE_IMAGE`; none was supplied or run here):

```sh
docker image inspect "$CANDIDATE_IMAGE" --format '{{.Id}} {{.Os}}/{{.Architecture}} {{.Config.User}}'
docker run --rm --pull=never "$CANDIDATE_IMAGE" --version
docker run --rm --pull=never --entrypoint id "$CANDIDATE_IMAGE"
docker inspect tnld --format '{{.Image}}'
docker exec tnld /usr/local/bin/tnld --version
docker exec tnld sha256sum /usr/local/bin/tnld
# Operator also checks actual running executable identity; installed-path hash alone can miss an old process.
# Run caddy version/list-modules and adapt/validate in the actual supported Caddy namespace.
# Use nonsecret local fixture values for adaptation, never dump production DNS token/env.
```

Do not run reference `docker compose up` in this phase or against an unspecified target. Later deploy commands must use the approved compose path/project/config and immutable candidate, not blindly copy `tnld:latest` from the example.

## 9. Required deployment facts, single rollout and rollback

Before any remote action, operator supplies a sanitized inventory (no credentials in this plan):

- Host alias/account access approval, owner/on-call, OS/arch/runtime/Compose version, exact service/project/install paths and writable directories; container UID/GID and config/token directory access without world-readable secrets.
- Installed and running tnld/tnl/Caddy paths, versions, hashes/image IDs, current release source if known; exact Caddy plugin/version modules and immutable rollback images. Which local binary is actually invoked (PATH/symlink/package/container) on both gateway and test client.
- Control domain + wildcard suffix, real DNS A/AAAA and TLS issuer/expiry/SAN expectations; no DNS changes unless separately authorized; Caddy listener and namespace, daemon loopback binding, CDN/LB hops and proxy CIDRs, external firewall exposure of daemon port, egress addresses of allowed/denied test probes.
- Actual config/compose/Caddy files and their owners/hashes; env override key names (not values), persistent storage and backups, active tunnels/load, limits/retention defaults, effect of read-only mount on token administration, trusted ingress policy and reserved path behavior.
- Dedicated test bearer supplied securely, run-prefixed tunnel namespace, test endpoint ownership, optional approved second source/IPv6, storage/evidence retention, maintenance duration, abort/rollback triggers, permission to exercise bounded auth/rate failure cases.

**Proposed single rollout procedure:**

1. Freeze integrated commit/lockfile/config contract and candidate hashes; local suite + immutable package/Caddy gate PASS first. Save sanitized evidence and previous deployment manifest; store actual secret/config backups only in approved protected location outside artifacts.
2. Ensure previous images/binaries are already available locally; do not rely on registry availability during rollback. Rehearse downgrade config parsing and require feature clients to refuse unsupported policy before fallback.
3. Operator validates candidate config, certificate assumptions, UID/mount readability and Caddy routing using actual runtime. Stage immutable binaries/image refs and approved Caddy changes. Declare maintenance; expect existing HTTP streams/WS sessions to terminate on daemon restart—no zero-downtime promise.
4. Switch daemon and any necessary Caddy config as one approved change set, not per lane; reload only validated Caddy config. Confirm container process health and identity, API bearer/WSS connection, then run deployed matrix once. Retain timestamped change/evidence records without secrets.
5. Proposed fixture readiness limits: poll to 10s per local readiness condition, 5s per bounded request/WS echo, 60s for deployed service readiness, whole matrix 300s. Resource cap: at most two regular fixture tunnels plus temporary compatibility cases, 16 concurrent fixture connections, 64 MiB fixture payload budget. Production performance budgets need D8 approval; these are harness safety bounds, not a throughput claim.

**Rollback triggers:** any authentication/IP bypass, secret leak, wrong running identity/image/config, failed verified TLS/control/application WS, persistent unavailable data plane after readiness window, or unrelated production regression. Stop probes immediately for security issue; operator restricts exposed routes first if needed. Record failure, never keep retrying until a PASS erases it.

**Rollback sequence:** stop only campaign tunnels/probes; operator restores previous immutable daemon binary/image and its compatible config plus previous validated Caddy configuration; restore previous client for smoke testing if protocol mismatch requires it. Do not restore old token-store contents by default: avoid resurrecting revoked tokens or losing newly issued ones. Restore storage only with explicit compatibility assessment. Restart/reload using approved target-specific service commands; verify hashes/running process, `/healthz`, authenticated WSS control, HTTP, expected old feature capability and reconnect. Old release may lack new access controls: it is unsafe to reconnect newly protected public tunnels to it silently. Keep those tunnels offline/restrict ingress and require policy-capable server until approved remediation. Re-run old known-good smoke and document which new features are unavailable. Persist the failure evidence; a fixed candidate requires fresh combined qualification.

## 10. Security/compatibility constraints and integration risks

- Treat proxy headers, history files, backend responses and request IDs as attacker-controlled until validated at the owning boundary. A Caddy request-header test with manually forged XFF is not proof of legitimate visitor identity; compare actual socket/proxy path and spoof resistance.
- Do not expose daemon port broadly, trust loopback as a substitute for provenance without documenting local-process trust, log Basic/bearer credentials, copy production token files into tests, or make token directories world-readable to repair container access.
- API path routing currently precedes tunnel data routing. New auth/IP middleware must not accidentally protect/disable server health/control globally, or let API Basic satisfy bearer auth. Changing reserved route ownership is a separate decision, not an incidental acceptance fix.
- Capture must not buffer whole streams to satisfy history/replay. Include byte/count caps, cancellation/backpressure tests, binary-safe assertions, and clear nonreplayable metadata. Sanitized history is not necessarily semantically replayable; refuse rather than transmit corrupted credentials/body.
- Per-stream task lifetime, HTTP response-body lifetime and upgraded-socket lifetime differ. Holding permits only until response headers will falsely pass a simple sequential test; use barriers and a live WS to prove lifetime. Session reconnect must not leak permits or remove policy.
- Existing `tnl http --json` contains banners; release adapters must use approved structured record contract without pretending all current stdout lines are JSON. Do not silently drop malformed records or count an empty result as successful history inspection.
- Baseline specs promise more than code implements; package version strings and healthchecks alone prove neither commit identity nor feature behavior. Test actual CLI paths and real configured Caddy. Native and stripped-musl daemon hashes are intentionally distinct.
- Cached image builds, floating Caddy/provider dependencies, stale `latest` tags, non-root mount errors, read-only token storage, unavailable IPv6, and missing successful TLS fixture are release risks. Record unresolved environment dependencies, not false-green skips.
- Transport invariant: daemon yamux Client opens streams, CLI Server accepts; retain role-mapping comment and channel-drop shutdown semantics in `tnl-protocol/src/transport.rs`. Avoid unrelated edits to the fragile axum/async-tungstenite bridge. Known ack_backlog >=256 issue remains outside load scope; keep bounded test concurrency below it and disclose it.

## 11. Completion/acceptance evidence and current phase result

Implementation completion requires reviewer approval plus: per-lane meaningful failures tested; shared contracts resolved; clean integrated source identity; full fmt/clippy/workspace results; interaction matrix results; actual Caddy + packaged/runtime identity evidence; documented target/rollback rehearsal; authorized deployed TLS/HTTP/WS/interaction result; sanitized artifact hashes; explicit not-run cases and accepted residual risks. No unresolved security bypass, missing identity, or required test SKIP may be labeled PASS.

**Design-phase validation actually performed:** read-only source/doc inspection; `git status --short`, `git branch --show-current`, `git rev-parse HEAD`, local logs/branches/worktree topology, merge-base/rev-list, `rustup toolchain list`, `rustc --version`, `cargo --version`, executable presence checks, and index check. No build/test suites or runtime/network/deploy commands were run. No staged files. Only this external design artifact was written. Next action belongs to parent/user: reconcile decisions D1-D8 and the other lane specs before authorizing implementation; this document does not initiate rollout.


## 12. T7 phase1: actual-Caddy fixture qualification (implemented)

This is **per-task fixture qualification**, not task 7's final combined campaign,
packaged-artifact acceptance, or deployment approval. Authorized baseline:
`39580c0ff6a2c89e99e17f7d233915cc8acc2862` (reviewed T1/T2/WS/P0/T4a/S0).
Only `crates/tnl-e2e/tests/visitor_ip_caddy.rs` and this plan were changed.
The reviewed repository `deploy/Caddyfile` already removes the unconditional
`header_up Connection close`; no production proxy or product implementation was
changed in this phase.

### Actual executable and safe reproduction

The user authorized read-only extraction of `/usr/bin/caddy` from the already
running gateway container, not a rebuild/download or remote configuration read.
Initial BatchMode authentication failed; execution stopped and escalated. After
explicit user confirmation that KeePassXC was unlocked, the one authorized retry
succeeded. No remote writes, reloads, restarts, or deployments occurred.

Measured artifact:

- Caddy `v2.11.3 h1:/vFbdjcs2DtzcWTIxHybf5R5TspYFFThlZffChyBFHg=`.
- SHA256 `a048a9fe3bd6793c683db69617b0b56067890d030e0e0cea215f29c76be20af2`.
- Local ignored executable: `target/caddy-qualification/caddy`.
- Repository Caddyfile SHA256:
  `ad29f998fbc7690c57e9a7cc0bdda9c4af5f4a03013b97856f08f200f530c7c4`.
- Toolchain: `rustc 1.94.1 (e408947bf 2026-03-25)`,
  `cargo 1.94.1 (29ea6fb6a 2026-03-24)`.

Reproduce with an explicitly supplied, verified local binary; no SSH is needed
for the tests themselves. This worktree used the parent-authorized existing
`../visitor-ip/target` cache, one Cargo build job, and default debug profiles:

```sh
export CARGO_BUILD_JOBS=1
export CARGO_TARGET_DIR=../visitor-ip/target
export TNL_TEST_CADDY_BIN="$PWD/target/caddy-qualification/caddy"
cargo test --locked --offline -p tnl-e2e --test visitor_ip_caddy \
  -- --include-ignored --nocapture --test-threads=1
```

The actual-Caddy tests remain ignored in the default workspace run because the
binary/source-binding prerequisites are environment-dependent. Explicit runs
fail on a missing binary; they never silently skip or install anything. The
nonignored config-fidelity test runs without Caddy.

### Observed cases and RED/GREEN

- RED config-fidelity test exposed the inherited fixture's missing
  `flush_interval -1` and forced-close override. The fixture now copies the
  reviewed data proxy directives from `deploy/Caddyfile`, checks the API proxy
  shape, and isolates the wildcard data host from the loopback API/control host.
  TLS/ACME, addresses/ports, admin, and private filesystem locations are fixture
  adaptations, not an independent production proxy implementation.
- Actual-Caddy RED with forced close only on the wildcard (API/control healthy):
  application upgrade returned attributed **HTTP 400**, not 101. The retained
  `actual_caddy_legacy_forced_close_breaks_upgrade_but_not_http` negative control
  asserts this rejection occurs before backend/client forwarding, then proves
  ordinary HTTP still returns 200 with backend/public/client ID agreement.
- Without forced close, real control WS negotiation and application 101 succeed.
  Real binary frames coalesced with both request and response heads survive;
  text, binary echo, ping/pong and graceful close work. Two ordinary chunked HTTP
  requests share one public keepalive connection while application WS is open;
  duplicate Set-Cookie headers survive and hop-by-hop fields are removed.
- All successful HTTP/101 paths correlate exactly one authoritative ID across
  backend request, public response, and client close-time inspector record.
  Forged visitor IDs and backend response IDs cannot override it; requests have
  distinct IDs. This uses existing library seams, not unfinished history/access
  APIs or hypothetical CLI flags.
- Source `127.0.0.2` is distinguished from daemon proxy peer `127.0.0.1`.
  Default Caddy discards spoofed XFF. An explicit synthetic append-hop fixture
  preserves `203.0.113.66, 198.51.100.7, 127.0.0.2`: with only the immediate peer
  trusted, attribution stops at `127.0.0.2`; explicitly trusting that extra hop
  selects `198.51.100.7`, not the leftmost spoof. This is a synthetic trust-policy
  regression, **not** evidence of a real CDN chain or advice to trust loopback
  networks broadly. Forged Forwarded/internal visitor claims do not control
  client visitor metadata.
- Readiness polls the listener and child exit, bounded to 5s; version probe 3s;
  visitor/application/cleanup tests have 30s/20s/10s deadlines. Child Drop kills
  and waits; task guards abort fixture tasks and JoinSet owns backend children.
  An intentional-panic regression proves child cleanup releases the listener.
  The 25ms readiness poll is conditional polling, not a fixed startup sleep.
- Subprocess HOME/XDG config/data paths are isolated under private temporary
  fixture state. **Local side effect disclosed:** initial RED runs used the
  inherited helper before this isolation fix and Caddy autosaved a synthetic
  local config to `~/.config/caddy/autosave.json`. Only metadata was checked;
  that outside-worktree artifact was not subsequently read, removed, or edited.
  Later runs isolate autosave correctly. No credentials, environment dump, DNS,
  external issuer, or full production config were accessed.

Validation logs and identity evidence are local ignored outputs under
`target/caddy-qualification/`: `red-config.log`, `red-websocket.log`,
`red-data-websocket.log`, `green-caddy.log`, `fmt.log`, `clippy.log`,
`workspace.log`, and `identity.log`. RED failures were expected regression
proofs, not final failures. Final actual-Caddy run: **5 passed, 0 failed, 0
ignored**. Final workspace run: **263 passed, 0 failed, 5 ignored**, including
this fixture's four environment-conditional tests and the existing slow
reattach case. `cargo fmt --all`, workspace all-target clippy with `-D warnings`,
and offline/locked workspace tests passed. An initial workspace tool invocation
hit a 120s outer command deadline; the complete rerun passed.

### Remaining release gates (not qualified here)

- Public HTTPS/WSS certificate chain, hostname verification, real DNS/issuer,
  production CDN/proxy chain and installed config are **not yet qualified**.
  The live gateway's on-disk forced-close directive is an inventory fact, not
  changed by this task. The complete 13-snippet production ingress is not modeled.
- Tests run native debug library fixtures, not packaged or installed tnl/tnld
  executables. Immutable image identity, musl build, protected mounted config,
  non-root readiness, compatibility and rollback rehearsal remain separate gates;
  existing possibly-skipping packaging tests are not substitute evidence.
- Inspection/history/replay/access interactions and the single combined release
  campaign remain pending feature reconciliation and operator approval.
- No staging, commits, merges, pushes, remote changes or paid child execution.
  Parent independent review and commit are required before integration.


## 13. T7 phase2: local executable acceptance harness (implemented)

**This qualifies the harness, not an immutable package or final combined release.**
Source baseline: `53860d467912664835535da24c0882695346f45d`. The supplied
integration debug client includes browser replay wiring from `42169c8`, which is
not yet part of this worktree's source baseline. Tests invoke explicit external
executables; they never substitute Cargo-built product executables or invent
product flags. Schema/review reconciliation and one final release-binary campaign
must follow later.

New files: `crates/tnl-e2e/tests/packaged_acceptance.rs` and
`crates/tnl-e2e/tests/support/packaged.rs`. No dependencies or product code changed.
The five executable tests are ignored by default, like the actual-Caddy tests;
explicit execution fails on absent/nonabsolute executable paths rather than
silently skipping or finding a PATH fallback.

### Reproduction and provisional identities

```sh
export TNL_TEST_CLIENT_BIN=/home/atassis/.cache/tnl-request-ids-target/debug/tnl
export TNL_TEST_DAEMON_BIN=/home/atassis/.cache/tnl-request-ids-target/debug/tnld
export TNL_TEST_CADDY_BIN="$PWD/target/caddy-qualification/caddy"
export CARGO_TARGET_DIR=/home/atassis/.cache/tnl-websockets-target
export CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
cargo test --locked --offline -p tnl-e2e --test packaged_acceptance \
  -- --ignored --nocapture --test-threads=1
# Also explicitly rerun the existing Caddy forced-close negative control:
cargo test --locked --offline -p tnl-e2e --test visitor_ip_caddy \
  -- --include-ignored --nocapture --test-threads=1
```

Measured debug artifacts (both report `0.1.0-beta.4`; versions alone are not
candidate identity):

- Client SHA256: `2946f9ed45ed6e48df27b52b01d27d2bc6a6285b0e248e68d4e0cc35ce616727`.
- Daemon SHA256: `277bef7679f8f522d7109e4f0e17226ebc9de0177441dd6ff0d28bd7b160c862`.
- Caddy remains the verified v2.11.3 artifact/digest in §12; no SSH was needed.
- Toolchain remains Rust/Cargo 1.94.1. The exclusive websockets cache is used with
  debug information disabled; the removed visitor-ip cache was not rebuilt.

### What the real executable tests establish

- Real `tnld token add` mints only a fixture-owned token, captured privately and
  never printed by the harness. Real daemon, CLI, and actual Caddy processes use
  isolated private temporary state, cleared child environments, synthetic config,
  and HOME/XDG isolation from the first launch. Config/log files are private;
  credentials, bootstrap capabilities and confirmation tickets are not dumped.
- Both Caddy virtual hosts use reviewed repository proxy directives, adapted only
  for local HTTP, loopback ports and disabled admin/ACME. All network traffic is
  to 127/8; no public server, DNS issuer, global CA or external proxy is used.
- Default CLI without `--inspect` has no inspector bootstrap, forwards normal
  HTTP unchanged, and emits correlated structured logs. Public spoofed XFF/IDs
  cannot override `127.0.0.2` visitor attribution or the daemon-owned ID across
  backend request, public response and CLI log. Backend ID collisions are ignored.
- A real application 101, not just `/control`, relays binary frames coalesced
  with both request and response heads, binary echo and graceful close. Ordinary
  HTTP succeeds while WS remains open. Existing §12 forced-close negative control
  is rerun against actual Caddy and still detects the upgrade-breaking directive.
- Real daemon restart causes the running CLI to reconnect/recreate the same
  explicit public name and resume attributed traffic. The CLI is also stopped by
  Ctrl-C and restarted as a new process claiming that same name (fixture grace
  disabled, not a product-default change).
- Actual `--inspect --replay-retain-secrets` supports authenticated v1 API reads,
  rejects missing authorization and foreign Origin, correlates complete records,
  redacts authorization/cookies/body-password/response secrets, and keeps both
  request/response body previews off by default. A separate CLI run explicitly
  enables JSON previews and verifies useful sanitized JSON remains visible.
- Replay POSTs independently require matching Origin, `Sec-Fetch-Site:
  same-origin`, mode `cors`, and destination `empty`. Missing any one is rejected.
  Prepare and unacknowledged confirmation deliver zero backend requests.
  Explicit confirmation sends exactly the original method/target, Host,
  Authorization, Cookie, Content-Type and secret-bearing body to the fixed local
  target. Duplicate confirmation is idempotent. One case succeeds; another drops
  the backend response after delivery and produces a failed/possibly-sent outcome
  with no automatic retry. Outcomes remain sanitized and linked to the capture.
- Protected CLI over plain HTTP fails before publishing a tunnel. A protected CLI
  pointed at a generated, untrusted loopback HTTPS issuer actually rejects its
  certificate, sends no application traffic and can be stopped cleanly. No
  insecure TLS flag, product custom CA, or trust-store installation is used.
- Readiness uses real health and authenticated CLI status, not fixed sleeps or
  banner-only guesses. Conditions/commands have 10s deadlines, HTTP requests 3s,
  and test-level budgets are 45–90s. The 25ms interval is conditional polling;
  150ms negative-delivery checks assert absence, not startup timing. Every child,
  including token/status/signal helpers, has a kill/wait guard. Fixture children
  stop before temporary files disappear. Intentional panic and occupied-inspector
  startup failure regressions prove listener cleanup; Ctrl-C closes the inspector
  and allows rebinding its exact address.

### Validation and remaining gates

Local ignored evidence is in `target/packaged-qualification/`: `identity.log`,
`actual-binaries.log`, `green.log`, `fmt.log`, `clippy.log`, and `workspace.log`.
After the independent-review fixes below, the executable suite passes **5/5**
plus one nonignored connection-oracle counterfactual; the existing Caddy suite
passes **5/5**. Workspace validation on this source baseline passes **392 tests,
0 failures, 10 ignored** (nine environment-dependent fixture tests plus slow
reattach). The initial pre-review checkpoint was 4 executable cases and 391
workspace passes / 9 ignores.
Formatting and workspace all-target clippy with `-D warnings` pass.

This does **not** qualify successful protected TLS with production CLI roots:
webpki does not trust the fixture's generated issuer. Existing generated-CA
library tests qualify logic only, and this harness qualifies real CLI rejection.
Successful protected HTTPS/WSS, valid public certificate/hostname/issuer, real
proxy/CDN chain, and complete production ingress must be checked after an
operator-authorized deployment. Local HTTP is never labeled public TLS proof.

Final schema lane/reviews, immutable release tnl/tnld paths and hashes, actual
packaged image/non-root mounted-config readiness, protected all-feature
interactions, compatibility/rollback, and the one combined campaign remain
pending. Re-run this harness with the final explicit release binaries after
integration; debug evidence above is not transferable to new artifacts. No
staging, commits, remote actions or deployment occurred in phase2.


### Independent-review P2 follow-up: strict raw input and redirect enforcement

Review `818ba50e` correctly identified two missing executable coverage areas;
neither was a demonstrated product vulnerability. Both are now qualified by
actual supplied binaries rather than serializer or probe-client behavior:

- `Inspector::raw_mutation` sends bounded authenticated HTTP/1 bytes directly to
  the real CLI inspector. It does not serialize/normalize attacker JSON or merge
  duplicate headers. Every rejection case checks the exact HTTP status, a fully
  framed JSON response, real `Cache-Control: no-store`, and absence of CORS grants.
- The prepare/confirm rejection table sends malformed JSON, invalid UTF-8,
  duplicate schema and confirmation fields, unknown fields, target/header/body
  overrides, unsupported Content-Encoding (including `identity` and duplicated
  encoding), duplicate Host/Origin/Fetch metadata/Authorization/Content-Type/
  Content-Length, transfer coding, and false acknowledgement. Expected statuses
  are 400 for framing/JSON/encoding, 401 for duplicate authorization, and 403 for
  duplicated browser authority/context. An oversized **otherwise valid JSON**
  document tests the actual 1024-byte body boundary; no helper refuses it first.
  This does not claim an HTTP status for oversized heads: the product's head cap
  closes the socket, and that separate behavior is outside this rejection table.
- Backend accept counters increment before reading a request head. Each rejected
  raw request must leave the connection count unchanged and deliver no request.
  The same original valid ticket then confirms successfully after all rejected
  confirmations, proving they did not consume it. Exactly one replay connection
  and duplicate-confirm idempotence remain required.
- New `packaged_replay_does_not_follow_loopback_redirect` makes the real replay
  origin return **302** with Location pointing to a second fixture-owned loopback
  backend. The real CLI replay outcome must preserve status 302 and delivery
  `response_received`. The destination must receive zero TCP connections and the
  origin exactly one replay connection, with no follow-up. Probe-client redirect
  policy is not counted as engine evidence.
- Nonignored `backend_connection_counter_rejects_headerless_followup` is a
  counterfactual oracle test: it deliberately opens a real TCP connection but
  sends no HTTP, then requires the zero-connection assertion to panic. This proves
  an extra connection cannot escape detection just by omitting a request head;
  no fake product regression or product-code mutation was introduced.

Final follow-up logs: `target/packaged-qualification/actual-binaries.log`,
`review-fixes.log`, `clippy.log`, `fmt-check.log`, and `workspace.log`. The supplied
debug executable digests remain those recorded above. All prior packaging,
public-TLS, final-schema/review and single-combined-campaign gaps remain open;
independent parent recheck is required before commit.


## 14. Final beta.5 artifact and public-ingress qualification

The parent completed the single combined rollout after explicit approval of
the maintenance interruption and rollback scope. Released source is 45e0b7a;
all source lanes and independent reviews are composed. The frozen manifest is
release-validation/target/release-candidate/45e0b7a/manifest.json (SHA256
`d19c9551767d20b510e9add30e33589db6729f18bc9319b93d57875d6d61716b`).
The actual beta.5 image, native binaries, extracted musl daemon, Caddy and full
workspace were qualified together: 439 reported passed with all ignored tests
included, plus six image-daemon executable harness cases and eleven actual
image runtime checks. Two obsolete beta.1 packaging tests internally SKIP;
actual beta.5 runtime checks, not those legacy tests, establish package proof.

The image now installed on ru-mos-gw has digest
`sha256:d7d0b367b2084d4d08cb0c8b6291247b665f5e0ab01a1be5b6c0225780792b16`.
Public valid-root HTTPS/WSS positive protected access and application WebSocket
acceptance passed, as did public visitor/IP forgery refusal, canonical IDs,
Basic consumption/refusal, rate/concurrency/WS lifetime checks, and isolated
Brave privacy/confirmed replay using the frozen release client. No browser
security overrides, TLS verification exceptions or system CA changes were used.
The separately approved local installed client is byte-identical to that
candidate. Public test tunnels were cleaned up.

The supplementary stop-as-network-loss fixture was invalid and timed out; its
failure is preserved and not counted as automatic reconnect proof. Controlled
reattach/recreation is covered by the enabled local TLS/e2e campaign. No
production network-chaos or actual rollback rehearsal is claimed. The pinned
old-image rollback and three-file backups remain available; other Caddy files
were hash-verified unchanged. No Git publication was performed. Detailed
chronology and remaining qualifications are in roadmap-execution.md.
