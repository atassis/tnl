# tnl roadmap execution tracker

**Current state (2026-11-07): beta.5 deployed and accepted; local main and
installed client updated. See the final delivery checkpoint below. Earlier
blocked/pending statements are retained as historical evidence.**

## Approved direction

User approved implementation via separate high-reasoning subagents/worktrees, eventual integration into main, and one combined deployment/acceptance campaign. Existing root checkout AGENTS.md edit is not ours and must remain untouched. Baseline: 43f2bc0. Local main is 20 commits behind this baseline; reconcile only during reviewed local integration. No push or publication authorized.

Settled choices:
- One negotiated bounded data-substream envelope contains daemon-authoritative visitor identity and request ID. Legacy raw forwarding remains possible with explicit unavailable authoritative metadata. No trusting inbound public metadata headers.
- One opt-in authenticated IPv4 loopback browser inspector with history/search/details and explicit confirmed replay. No second Unix IPC service or replay CLI.
- All visitor access controls are optional, client-supplied per run. Normal public/random/temporary tunnel behavior remains unchanged. Protected mode must receive positive exact-policy acknowledgment, never silently downgrade.
- Inspection: opt-in --inspect, memory-only 200 records / 15 minutes, redacted headers, body previews off by default, optional small JSON previews.
- Replay: separate explicit raw-retention consent, memory-only 20 entries / 16 MiB / 1 MiB entity per request / 10 minutes; fixed original loopback backend; no request/destination editor; confirmation per attempt. Sanitized display data never becomes executable raw data.
- Separate prerequisite fixes upload/response sequencing concurrently while preserving error attribution and transport invariants.
- Classic HTTP/1.1 application WebSockets, no frame recording/replay or Origin rewriting. Requested concurrency limits last through entire exchange/upgrade lifetime.
- Proxy header trust defaults to none; explicit trusted CIDRs required.

## Tasks and integration order

| ID | Task | Worktree | Status |
|---|---|---|---|
| B0 | Diagnose/fix pre-existing reconnect readiness failure (separately approved) | baseline-readiness | reviewed, verified, committed 1d794e0; integrated |
| P0 | Concurrent streaming upload/response prerequisite | upload-streaming | reviewed, parent checks pass, committed 6826171; integrated |
| T1 | Visitor resolver, negotiated common envelope, visitor logs | visitor-ip | independently reviewed/corrected; 3b9a8b3, integrated with P0 in a9997be; parent combined gates pass |
| T2 | Shared request-ID propagation and response correlation | request-ids | reviewed 75bbb97; parent lane gates pass (221 tests, 2 existing ignored); merge into b654a9a working tree reconciled with WS, combined gates pass (262 tests, 2 existing ignored); independent integration review approved and parent gates rerun (262 passed, 2 existing ignored); committed merge 39580c0 |
| S0 | Graceful session close wakes pending readers and preserves buffered data (separately approved) | session-shutdown | minimal licensed yamux patch 082d2e4, integrated d887619; reviewed, parent gates and actual Docker build passed |
| T3 | Application WebSocket relay and Caddy configuration | websockets | independently reviewed and parent workspace 257-pass verified; committed b654a9a; subsequent T2 integration preserves WS and adds passing request-ID/opaque-echo regression |
| T4a | Binary body/WS frame must not erase parsed response status | status-observer | independently reviewed, parent gates passed;6e43504 integrated in0d9e9b1; WS untracked regression preserved (FF merge safely refused overwrite) |
| T4 | Bounded observer, sanitized history, authenticated browser inspector | inspection | observer/LF integrated 3ba8806; authenticated loopback inspector/lifecycle integrated be94d4b; raw-core composition 56d43fb parent-reviewed with 371-pass/5-ignored baseline; cancellation-stop-both correction is present; browser replay wiring integrated e45ed97; reviewed full preconnect/client-synth/transport record schema caef33d is content-composed on 4b5f296, pending parent review/staging and the fresh browser/package final campaign |
| T5 | Opt-in raw retention and confirmed browser replay | replay | reviewed bounded raw storage/engine 9362954 integrated 56d43fb, including backend capture context and cancellation revocation; independently reviewed browser consent/routes/confirmation wiring 42169c8 integrated e45ed97 after parent 402-pass/5-ignored locked gates and actual integrated Brave replay acceptance |
| T6 | Optional per-run Basic/IP/rate/concurrency controls | access-controls | phase2a/observer composition committed e415cae; real registry/enforcement/CLI a3caead independently reviewed (23dcc), parent lane 310-pass/2-ignored; composition independently approved e32a8b0a, committed 53860d4 after parent 391-pass/5-ignored gates and actual Caddy 5/5; subsequent browser replay composition independently approved 03c3b768 and committed e45ed97 after parent 402-pass/5-ignored locked gates |
| T7 | Combined packaged-binary/Caddy acceptance and rollout checklist | release-validation | reviewed local Caddy qualification 2ff475c integrated in 32ca01b; real-access/observer/raw-composed source repeats local Caddy 5/5 with explicit v2.11.3 binary; executable/Caddy harness integrated 4b5f296; schema-composed fresh debug binaries pass six executable cases and five actual-Caddy cases; final immutable packaged/public-TLS campaign and independent composition review remain pending |

Each task: failing behavioral tests first, focused implementation, relevant scenario tests, fmt/clippy/full workspace tests before any commit, independent specification review then code/security review. Parent owns commits and integration after reviewing exact diff/staging. Workers must not publish, deploy, modify main, or touch another worktree.

Shared-file owners integrate sequentially. Metadata negotiation stays on authenticated control WebSocket capability handshake; access lifecycle must reuse it rather than create a competing metadata negotiation. Inspection owns observer/service/display/UI; replay owns raw retention/engine/confirmation semantics, with one agreed facade and one set of routes. Interface details in lane specs remain proposals when inconsistent with these settled constraints; stop for material scope changes, resolve routine naming from conventions.

## Release gates

Per-task tests do not replace combined acceptance. Qualify integrated source, immutable candidate binaries and actual Caddy behavior; use the same candidate for deployment. One combined deployment followed by acceptance, with rollback on security/availability failures. No false PASS for missing environments. User selected existing tnl server for eventual deployment; host/SSH alias, service/access/maintenance/rollback facts remain required before remote action. No production credentials copied into fixtures/artifacts. No sudo.

## Evidence

Planning completed for seven lanes. Baseline workspace tests failed in both first-wave worktrees at reconnect.rs first request (404 vs 200); focused unchanged rerun also failed. Fixed 400ms setup sleep is a readiness-race hypothesis, not proven root cause. User separately approved bounded diagnosis and minimal test-readiness correction. Runtime evidence confirmed request-before-registration race, not an isolated Argon2 cause. Test-only correction replaces setup sleeps with bounded registry/session observations and verifies same tunnel/new session; negative mutations caught premature readiness and missing disconnect. Independent review approved; parent reran fmt/clippy/full workspace: 178 passed, 0 failed, 1 preexisting ignored. Commit 1d794e0 integrated into first-wave and integration branches. P0 concurrent upload/response fix independently reviewed, parent fmt/clippy/workspace passed (189 passed, 0 failed, 1 preexisting ignored), committed 6826171 and integrated. Eleven new tests cover upload/backpressure/deadline/half-close/cancellation including 1MiB yamux; client first-informational-status observer limitation remains deferred to T4. No roadmap feature completion or deployment has passed yet. Plans live at each lane's docs/plans/roadmap-<lane>.md; original/revised external artifacts are in workflow 461235ca-9bbd-416e-8a38-bc058fb8d994.

Bounded WS integration on d887619 preserves the T1 authoritative metadata envelope and visitor tracing, scoped Hyper-driver cancellation, and the full four-test T4a binary-status file. Both copied WS fixtures now exercise Http1MetadataV1; the e2e fixture asserts the authenticated negotiated mode and transfers it into ForwardCtx. The reviewed complete-header-timeout test retains its corrected 40-second outer guard. Fresh integration checks: cargo fmt --all (only known stable-rustfmt configuration warnings), workspace clippy with -D warnings, and cargo test --workspace pass (257 passed, 0 failed, 2 existing ignored). Focused protocol tests pass (52); daemon WS tests pass (10); unfiltered real-tunnel WS tests pass (8), including the exact daemon-local Session.close regression; binary-status tests pass (4). The existing slow ignored reattach test also passes when explicitly invoked. Logs: integration/target/integration-logs/. No Caddy binary or public WSS/candidate-binary topology has been qualified; the existing Caddy fixture remains unrun. Working-tree conflict content is reconciled, but conflicted index entries and all inherited staged changes are deliberately left for parent review/staging; no commit or deployment performed.

Subsequent T2/WS integration on b654a9a reconciles reviewed 75bbb97 without staging or altering other merge inputs. The handler mints one ID and passes it with visitor identity into the mutable inner request handler. Authoritative request and response IDs are installed after hop filtering, including the new 101 response path, while WS accept/subprotocol headers and the upgrade-versus-close branch remain intact. A real tungstenite regression covers all four request/response Connection-nomination combinations, forged duplicate IDs, binary echo and close, and equality of backend/public-101/inspector IDs; inspector IDs come from the authenticated envelope. RED: the uncorrected 101 path exposed two backend IDs instead of one. GREEN: the post-filter 101 stamp passes the unchanged test. Fresh combined gates pass: fmt, workspace clippy -D warnings, cargo test --workspace (262 passed, 0 failed, 2 existing ignored), request-ID e2e (1), daemon request metadata (4), forwarder metadata (6), protocol metadata (6), daemon WS (10), unfiltered WS e2e (9), exact daemon-local-close regression (1), and binary-status tests (4). Logs: integration/target/integration-logs/t2-ws/. Existing Caddy and slow-reattach ignores are unchanged; no new test is ignored. Parent owns independent review, conflict-index resolution/staging, and commit. No Caddy/public-WSS/candidate-binary qualification or deployment is claimed.

Observer-phase1 integration on 39580c0 reconciles only forwarder tracing and reconnect-test conflicts from reviewed c760559. Sanitized opted-in method/target tracing retains T2's authoritative req_id; capture admission still receives the authenticated metadata, and post-Host-rewrite head/leftover/pump callbacks and independent bounded Host Auto status evidence are unchanged. Reconnect preserves both T2 backend/public/log correlation and inspection-generation assertions, additionally comparing both capture request IDs against the two correlated response IDs and checking peer provenance. The echo-backend fixture was extracted without behavior changes after the composed test exceeded clippy's 100-line limit; no lint suppression was added. Fresh checks pass: fmt, workspace clippy -D warnings, full workspace (294 passed, 0 failed, 2 existing ignored), observer/privacy/exact-byte-consumer tests (26), reconnect/request-ID/WS e2e (11), exact daemon-local-close regression (1), binary status/request metadata/upload forwarding (20), and protocol tests (53). Logs: integration/target/integration-logs/observer-p1/. Exclusive cache: /home/atassis/.cache/tnl-request-ids-target, jobs=1, dev/test debug=0; visitor-ip/target was not used. Both conflict contents are reconciled, but inherited staged inputs and conflicted index entries remain for parent review/staging. No commit or deployment performed. The phase1 document's initial missing-feature RED chronology deviation and subsequent twelve killed mutations are preserved, not retroactively rewritten.

Known cancellation contract gap (parent steering, deliberately not changed in this merge): observe.rs terminate(false) stops only incomplete directions. If both directions already emitted MessageEnd, Drop can emit Finished without cancellation stops, indistinguishable to a raw consumer from graceful completion. This checkpoint is NOT yet replay-safe. T5 owns the separately integrated minimal correction and RED tests: on non-upgraded Drop, stop both request and response as cancelled before Finished; T4's UI owner will adopt the same correction. No raw retention, replay safety, full UI, Caddy/public-WSS, or production acceptance is claimed.

Pre-commit P2 correction from independent integration review f96aacb0: the new live final-status fallback had unintentionally lost T4a's LF-line support. An actual-forwarder regression observed RED (inspection off, LF 200 plus binary entity yielded None rather than Some(200)) before the production edit. The bounded fallback now recognizes LF and CRLF status lines and skips informational heads only after their complete blank-line terminator; strict version/three-digit status grammar and the existing prefix/count bounds remain unchanged. GREEN covers off/admitted/admission-denied inspection, LF 100/103 followed by a final binary response, incomplete informational/final lines, and malformed status tokens, with exact relayed bytes asserted. All original binary-status tests and T5-owned cancellation code remain untouched. Fresh fmt/check, workspace clippy -D warnings, focused observer/privacy/binary tests (31), reconnect/request-ID/WS e2e (11), and full workspace (295 passed, 0 failed, 2 existing ignored) pass. Logs: integration/target/integration-logs/observer-lf/. Independent recheck and parent staging remained pending at that checkpoint; the parent subsequently approved and committed 3ba8806. No replay-safety claim.

Bounded access composition on 32ca01b combines reviewed d174904 with observer phase1, S0, T2, T3, T4a and the local Caddy fixtures. Cargo.lock retains both conflicted required dependencies (httparse/libc), access/zeroize/TLS dependencies and patched yamux; cargo metadata --locked and all test builds accept the resolved lockfile. reconnect retains immutable options, typed protected failures, assigned protected recreation hostname across failed attempts, observer Hooks and generation logic (same tunnel keeps generation; recreation changes it). Existing public wrappers remain intact. A new real public run_with_options reattach regression repeats backend/public/log/capture ID and peer-provenance checks alongside the original wrapper test. A new generated-test-CA certificate-verified WSS mock regression asserts outgoing policy fingerprints and the same retained Arc/verifier across exact/wrong/exact ACK attempts; this is NOT actual protected reconnect or protected-daemon enforcement evidence. The real daemon still returns AccessUnavailable without publication, CLI flags remain unexposed, and phase2b is separate.

Fresh access-composition validation: fmt/check, workspace clippy -D warnings, full --locked workspace (333 passed, 0 failed, 5 existing ignored); policy/wire 9, input helpers 9, fail-closed real control 1, admission primitives 9, reconnect generation/decision units 4, certificate-verified WSS mock 2, actual public reconnect 2, observer/privacy/binary/metadata 37, and ID/WS e2e 10 pass. The first combined shell validation timed out partway through its workspace invocation after focused checks and clippy passed; a fresh complete workspace rerun passed and is recorded as workspace-final.log. Actual Caddy tests ran explicitly with /home/atassis/.config/superpowers/worktrees/tnl/release-validation/target/caddy-qualification/caddy (v2.11.3), --include-ignored: 5 passed, none ignored. This proves local plaintext Caddy visitor/WS behavior only, not public TLS or production protection. Logs: integration/target/integration-logs/access-composition/. Exclusive request-IDs cache, jobs=1, dev/test debug=0; no other target used. Remaining index conflicts are exactly Cargo.lock and crates/tnl/src/reconnect.rs, content reconciled but deliberately unstaged for parent review/staging. No worker commit/merge/deployment. The phase1 cancellation gap was still present at that historical checkpoint; the subsequently reviewed raw-core merge 56d43fb includes the stop-both correction.

Real-access composition checkpoint on 56d43fb: reviewed a3caead (not the superseded unreferenced access staging commit) is reconciled without staging. Five content conflicts are resolved: README retains both inspection and access documentation; client mock tests retain earlier Arc/verifier checks and the access lane's typed protocol/verified-TLS failures; one configured HTTP command path combines prepared TunnelOptions with InspectionOptions and inspector shutdown; main prepares policy once before runtime creation; daemon retains authoritative handler ID/visitor tracing and stamps the ID after hop filtering on requests, ordinary responses and 101 while preserving trailer reframing. Access admission stays before session lock/open; Basic success consumes gate Authorization before metadata/backend/capture; driver/body and actual WS relay/rendezvous retain their lease owners. P0, S0, binary/LF fallback, observer backend context and cancellation-stop-both/raw engine sources are unchanged.

Added real-control HTTP/WS composition coverage across Basic, non-Basic protected and public runs using the generated-test-CA verified WSS edge. It compares backend/public/log/capture/envelope-derived IDs despite forged duplicate and Connection-nominated visitor/backend IDs, proves Basic credentials absent from backend/raw consumer, preserves application Authorization for public/non-Basic, checks denied Basic causes no backend/capture, ordinary HTTP raw eligibility, and opaque binary WS echo with raw revocation on 101. The existing counted-session denial tests independently assert zero stream opens. Holding back the resolved normal-response ID stamp produced behavioral RED (zero public ID headers versus one); restoring it produced GREEN. This is explicit merge-mutation evidence, not a new claim about the original lanes' test chronology. Existing real protected body/upload/WS/trailer tests remain asserted. The actual protected reconnect test now attaches observer Hooks and verifies same generation on reattach, new generation on recreation, and correlated metadata IDs through network loss and failed recreation. CLI coverage verifies access and inspection flags share the configured lifecycle without a tunnel banner on inspector bind failure.

Parallel combined Basic fixtures initially produced legitimate 503s because independent test daemons shared the process-wide four-worker auth pool. Test fixture admission now bounds unrelated fixtures to four; production worker capacity, per-scenario request concurrency and denial assertions are unchanged, with no denial retries added. Final focused client tests pass (15), CLI access/inspection (14), real daemon control/counting denial tests (3), observer/privacy/API/binary tests (41), and ID/WS/public reconnect (12). fmt/check and locked workspace clippy pass. A combined validation shell timed out partway through its first workspace invocation; a fresh complete --locked workspace rerun passes: 391 passed, 0 failed, 5 existing ignored. Explicit local Caddy v2.11.3 validation with release-validation/target/caddy-qualification/caddy and --include-ignored passes 5/5. Logs: integration/target/integration-logs/real-access/. Exclusive request-IDs cache, jobs=1, dev/test debug=0. Remaining index conflicts are exactly README.md, crates/tnl/src/client.rs, crates/tnl/src/commands/http.rs, crates/tnl/src/main.rs and crates/tnld/src/data_plane.rs; parent owns review/staging/commit. Browser replay wiring, full schema, immutable packaged/public-TLS campaign and deployment remain pending; no new replay flags/routes or production acceptance claimed.

## Schema / all-feature composition checkpoint (on 4b5f296)

Reviewed schema commit `caef33d` is content-composed with the parent's existing
no-commit merge on `4b5f296` (access `53860d4`, browser replay `e45ed97`, and the
executable harness). **Schema/replay/access are integrated in source, pending
parent independent review/staging and the fresh browser/package final campaign.**
This is not whole-T4, immutable release, public-TLS or production acceptance.

The sole source conflict, `crates/tnl/src/forwarder.rs`, retains both the T2
metadata-derived authoritative `req_id` and schema guard metadata. Opted-in
connect/phase-2 warnings keep that ID and bounded kind only, not error/target
context; ordinary warnings retain their original diagnostic context plus ID.
The sanitized request-head trace also retains the same ID. P0 concurrent pumps,
first-byte deadline/cancellation, opaque WS, binary/LF final-status fallback,
post-Host-rewrite single backend factory seam and actual successful-write
counters are retained. The guard still borrows context/target: counter Arcs,
capture wrappers and owned labels are created only after bounded capture
admission. Its typed ULID/IP/enum metadata copy requires no heap allocation.
Preconnect capture never invokes the raw factory. Basic gate scrubbing and all
HTTP/upload/body/trailer/actual-WS-relay access leases remain unchanged. No
transport or vendored S0 implementation was edited.

Auto-merged reconnect still uses the actual shared immutable `TunnelOptions`,
prepared protected policy and exact ACK flow; the schema activation labels the
real tunnel ID without changing retry/generation decisions. The actual protected
network-loss/failed-recreation test now additionally verifies trusted tunnel-ID
and tunnel-name labels on initial create, same-generation reattach, and new
recreation. The request-ID E2E preserves the original inspection-OFF scenario
and repeats it ON, adding backend/public/log/capture identity, safe source,
preconnect and successful-write metadata checks. Its original 9-KiB header
remains intact: the 8-KiB probe cannot claim a complete effective boundary, but
actual forwarding and authoritative identity still succeed. Initial new-test
expectations incorrectly assumed a complete boundary (and then capped rather
than unavailable stored headers); source inspection corrected those expectations
without changing the original large-header/body assertions or production code.

Merge-mutation evidence, not a rewritten lane TDD chronology: temporarily
omitting `req_id` from the opted-in connect warning failed the unchanged combined
identity test at its warning-ID assertion; restoring the resolved stamp passed.
`identity-mutation-{red,green}.log` retain that final OFF/ON evidence. The first
clippy pass found duplicated test assertions; those were factored without lint
suppression. All eight recreation tests retain their reviewed exact-DONE versus
specific non-timeout typed-UnexpectedEof contracts and exact 46/42 accepted-byte
checks, including chunk-accumulation coverage; no assertions were relaxed.

Final locked validation (jobs=1, dev/test debug=0, exclusive target
`/home/atassis/.cache/tnl-request-ids-target`):

- `cargo fmt --all`, `cargo fmt --all -- --check`: pass (known stable rustfmt option warnings only).
- `cargo clippy --locked --workspace --all-targets -- -D warnings`: pass.
- `cargo test --locked --workspace --no-fail-fast`: **429 passed, 0 failed, 10 existing ignored**, 100 suite results.
- Focused eight recreation tests: **8/8**; real protected client/shared-loop tests: **6/6**.
- Pressure filter: **2/2**, including history/64 observers/raw/four API handlers,
  continued declined forwarding and service join; conservative capacity model
  remains **20,861,128 bytes**. Admission regression **1/1**; scoped resolver and
  controlled-connect deadline/failure regressions **2/2**.
- Identity/WS/public reconnect: **12/12**; observer/privacy/schema/binary/metadata/API/replay/actual CLI policy suites: **65/65**.
- Fresh `cargo build --locked --workspace`, then explicit same debug client and
  daemon binaries: executable harness **6/6**, actual local Caddy **5/5**, no
  ignores. Caddy is explicit `release-validation/target/caddy-qualification/caddy`,
  **v2.11.3**. SHA-256 manifests plus after-both-campaign hash checks pass.
  An earlier hash comparison detected that intervening workspace test builds had
  replaced the debug artifacts; both campaigns were therefore rerun after the
  final rebuild against one freshly hashed pair. Initial hashes are preserved,
  not represented as the final candidate.
- `git diff --check`: pass; source conflict markers absent. The index's full
  stage listing is byte-identical before/after worker changes (manifest SHA-256
  `3e91c4f63410dfab490dae1cd40de0855f129a00c62e21489992d0f0ef5770c2`).

Logs: `target/integration-logs/schema-composition/`; authoritative final gates are
`clippy-final.log`, `workspace-final.log`, `executables-final.log`,
`actual-caddy-final.log`, `executable-sha256.txt` and
`executable-hash-check.log`. Ten default ignores are the existing five
explicit-binary harness cases, four installed-Caddy cases and slow reattach.
No new ignore, worker staging, commit, merge, push, deployment, child agent or
other-worktree edit occurred. The **index still has one UU entry,
`crates/tnl/src/forwarder.rs`**, with reconciled working content; all inherited
parent-staged inputs remain staged. This tracker is still untracked. Parent owns
conflict-index resolution/staging and independent acceptance review.

Remaining release work: a fresh full browser campaign on this exact composition
with strict backend/public/capture IDs, privacy, nonce and redirection checks;
immutable packaged/public-TLS qualification; and deployment/rollback approval.
The debug-binary/local-Caddy campaign is deliberately not substituted for any of
those gates. Capacity accounting remains an inspection-owner model, not
allocator/platform RSS or simultaneous replay-engine pressure certification.


## Parent final-source qualification

The complete feature composition is committed in 9433bcf. The inherited schema
conflict index was resolved by the parent after independent review e21cf5c1.
Fresh parent locked fmt/clippy/no-fail-fast workspace gates pass: 429 passed,
zero failed, ten environment-dependent ignored. The strict all-feature browser
probe passes on these debug binaries: backend/public/capture canonical IDs,
fragment removal, authenticated history/details, JSON redaction and HTML canary,
prepare without delivery, explicit confirmation, exact original replay, no
second execution after duplicate clicks or polling, and linked safe outcome.

Version 0.1.0-beta.5 is being prepared. No final release artifacts, image,
packaged/public-TLS campaign, main integration, publication or deployment has
been completed. Root checkout's unrelated AGENTS.md modification is preserved.
Earlier worker checkpoint statements about UU entries and pending browser
acceptance are historical and superseded by this parent qualification.


## Parent final beta.5 delivery

Released source: `45e0b7a213b81421b94cc4d0f7c179375bfb1030`. Reviewed schema and
all-feature composition are integrated; local main was fast-forwarded to this
candidate before deployment. Later changes to this tracker are documentation
only, not a replacement candidate. No push, remote Git publication, or tag was
performed. Root prep-public's unrelated AGENTS.md change remains untouched.

- Frozen local release workspace campaign explicitly includes every ignored
  case: 439 reported passed, zero failed, zero ignored. Two legacy tests
  internally SKIP the absent beta.1 image; they are not packaging proof. Actual
  beta.5 image qualification provides eleven separate runtime checks, including
  UID100, real serve, read-only mounted config/token store, healthcheck and
  released-CLI control/data/shared-ID traffic. Extracted image daemon executable
  harness: six passed. Artifact hashes remain unchanged.
- Native client SHA256:
  `23d5c1cf73f2c0894b1f58c6820f511f0b30fa7a719d2969671273418c88b6e1`.
  Image daemon SHA256:
  `6e88c85fce141b6e4fe41bc40cb6046cc388e6bc80b4157b298a203f3e4da300`.
- After explicit maintenance-window approval, ru-mos-gw now runs
  `tnld@sha256:d7d0b367b2084d4d08cb0c8b6291247b665f5e0ab01a1be5b6c0225780792b16`.
  Daemon version, binary hash, OCI source revision and healthy state were
  independently checked after public acceptance. Its only trusted proxy is
  127.0.0.1/32. The tnl wildcard snippet no longer forces Connection close.
- Shared Caddy live configuration matched disk before mutation and after the
  validated graceful reload. Other snippets/root Caddyfile hashes did not
  change. Caddy and gost remained running without restart.
- Public production-root-verified HTTPS/WSS campaign passed: public-default
  application Authorization preserved; forged visitor/request IDs replaced;
  combined Basic/exact-IP/rate/concurrency policy positively acknowledged;
  missing/wrong Basic denied without backend delivery; Basic consumed before
  forwarding; denied IP cannot spoof allowlisting; exact burst and Retry-After;
  binary WebSocket/read-ahead/subprotocol/Origin/shared IDs; WS holds the
  concurrency lease until closure and releases it afterwards.
- Isolated Brave acceptance using the frozen release client and actual public
  ingress passed canonical backend/public/capture ID correlation, redaction,
  fragment removal, no HTML execution, explicit confirmation, exact local
  replay, duplicate-click/polling single execution and linked safe outcome.
- A supplementary public probe incorrectly used `tnl stop` as physical control
  loss and timed out waiting for automatic recreation. The failed artifact is
  retained and is NOT a passing reconnect test: close_by_subdomain removes
  routing registry entries but does not close the still-live control session.
  Automatic reattach/recreation remains qualified by the enabled local trusted
  TLS/shared-loop and eight recreation tests, not production network-chaos.
- After separate approval, ~/.cargo/bin/tnl was atomically replaced with the
  exact frozen native client. The previous beta.4 binary is retained privately.
  Installed `tnl version`, artifact hash and `tnl doctor` pass; config/token and
  already-running processes were not changed. Synthetic acceptance tunnels
  were cleaned up.

Rollback backups and a syntax/compose-validated pinned-old-image rollback
script remain at `/opt/tnld/releases/beta5-45e0b7a-20261107T194526Z`. Rollback was
not needed and no live rollback rehearsal is claimed. Full frozen manifest,
logs, artifact hashes, public fixtures/results and old client backup are under
release-validation/target/release-candidate/45e0b7a (private ignored artifacts).


## Unreleased beta.6 follow-up

The intentional-stop/refresh follow-up is described in
`2026-11-07-intentional-stop-and-refresh.md`. Ctrl+C/SIGTERM now request
session-scoped acknowledged release without network grace; active HTTP/WS stop
immediately. Replay authority is revoked before cleanup awaits. The private
inspector fragment remains for F5 without cookies/storage or replay recovery.
Configured beta.6 debug source tests report 447 passed, zero failed/ignored,
including real CLI signals, stale-session protection, delayed/missing ACK,
protected named/random WS shutdown, actual Caddy and Brave F5/replay safety.
No frozen beta.6 artifacts, deployment or global client replacement is implied;
the deployed daemon and installed CLI remain the qualified beta.5 versions.
