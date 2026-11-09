# Embedded Trace inspector

`tnl --inspect` serves a private Trace inspector from the client process. The
production React/Vite assets are generated at build time, integrity-checked, and
embedded in `tnl`; a distributed, qualified `tnl` binary requires neither Node
nor adjacent asset files at runtime. The inspector is not served by `tnld`.

## Source checkout setup and builds

Building `tnl` from this repository requires the toolchain pinned in the source:

- Rust `1.94` from [`rust-toolchain.toml`](../rust-toolchain.toml);
- Node `24.14.1` from [`.node-version`](../.node-version); and
- Corepack with the root `packageManager` pin, pnpm `12.18.0`.

Install dependencies explicitly; build commands never install them themselves.
For a checkout that builds the client, use the following locked sequence:

```sh
corepack pnpm install --frozen-lockfile
cargo fmt --all -- --check
corepack pnpm ui:prepare
corepack pnpm ui:lint
corepack pnpm ui:typecheck
corepack pnpm ui:test
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
corepack pnpm build
```

Formatting comes before asset generation: formatting a manifest input after
`ui:prepare` makes the generated assets stale. `corepack pnpm build` prepares the
frontend and performs a locked release workspace build; `corepack pnpm build:dev`
does the same for a debug build. Run `ui:prepare` again after changing a frontend,
contract/exporter, tooling, or lockfile input.

`ui:prepare` exports the Rust public schemas, generates TypeScript and Ajv
standalone validators, type-checks, builds Vite output, and writes a finite
content/input manifest under `crates/tnl/src/inspection/assets/`. The Cargo build
script verifies that manifest, rejects missing/stale/symlinked/tampered files, and
does not run a package manager or access the network. The checks below are also
available when investigating that pipeline:

```sh
corepack pnpm ui:contract-test
corepack pnpm ui:manifest-test
node tools/run-inspector-guard-tests.mjs
cargo run --locked -p tnl-inspector-api --bin export-schema
```

Any Cargo command that builds `tnl`, including workspace tests and workspace
Clippy, needs prepared assets. `cargo fmt` does not. The server-only path stays
independent of frontend prerequisites:

```sh
cargo build --locked -p tnld
```

Generated contract files, frontend intermediates, and embedded assets are ignored
on purpose. Consequently, `cargo install --git` and a partial crate archive are
not supported ways to build a fresh client source checkout. A source distribution
must include the complete frontend/tooling inputs and perform the setup above.
This workflow does not introduce standalone crates.io publication. Before running
a Cargo command that builds `tnl`, prepare the embedded assets with
`corepack pnpm ui:prepare`.

## Development and privacy boundaries

The Vite development server uses only synthetic fixtures; it never proxies a real
capability or relaxes production CORS, Origin, or CSP checks. With locked
dependencies installed, start it locally with:

```sh
corepack pnpm --filter @tnl/inspector exec vite
```

It binds to `127.0.0.1` and visibly labels its data synthetic. Production
integration and browser scenarios instead use the Rust-served embedded assets.

The private capability remains an allowlisted URL-fragment value. The appearance
choice is an allowlisted `theme` fragment value: Auto follows system preference,
Light and Dark override it, and Auto removes the override. Changing theme keeps
the capability, selection, tabs, and replay state in the current document. The
app uses no cookies, localStorage, sessionStorage, or cross-run preference store.
Reload preserves fragment capability/theme but clears replay review,
acknowledgment, and confirmation authority. Do not share the startup URL.

Rendered captures are text/safely constructed elements, not capture-derived HTML.
The production policy permits only local styles/scripts/connections and retains
no-store/referrer/frame protections. The asset manifest accepts only the finite
HTML, hashed JS, and hashed CSS set; source maps are not embedded. Budgets are
512 KiB JavaScript, 128 KiB CSS, and 1 MiB total uncompressed embedded assets.

## Timing meaning

Timing is a bounded snapshot of local client forwarding work and the exchange
with the configured backend. It uses a monotonic forwarder-entry origin and
integer microsecond offsets/durations. It is **not** daemon, reverse-proxy,
visitor-browser, remote, or distributed end-to-end latency.

The timeline intentionally displays overlapping activities: metadata decode,
request-head probe, local resolution/connect, local backend writes, backend reads,
local tunnel writes, and upgraded lifetime. Do not add these intervals to claim a
total latency. A first backend byte is the first actual nonempty backend read;
final backend headers require a completely observed final head. Request/response
framing, accepted local writes, EOF, and overall task termination are distinct
observations. An accepted local write does not prove backend processing or visitor
browser delivery.

Snapshots can be active or terminal. Missing data is not zero: each activity can
be not started, skipped/not applicable, active, completed, failed, cancelled, or
unavailable. Partial failures preserve already observed measurements; malformed,
capped, synthesized, upgraded, and incomplete exchanges remain explicitly
labelled rather than being filled with guessed remote latency. WebSocket timing
separates the HTTP handshake from opaque upgraded lifetime; frames are not
captured or replayed.

## Replay limits

Inspection history is in-process and opt-in. Sanitized previews are distinct from
raw replay retention. Replay additionally requires `--replay-retain-secrets`,
with the command's documented limits of 20 requests, 10 minutes, 16 MiB total and
1 MiB per body. It can retain original query, credentials, cookies, and bodies;
treat the private inspector URL and process memory accordingly.

Only an eligible, complete original request may be prepared for review. Preparing
never executes it. Confirmation requires a fresh explicit acknowledgment for the
current review and consumes one-use authority before awaiting I/O, so repeated
confirmation input cannot issue a second confirmation. There is no request
editing, raw export, redirect following, automatic execution, or mutation retry.
A failed or missing outcome can mean uncertain delivery, not that the backend did
not receive the request; only safe outcome reads are available after that state.

## CI source and qualification boundary

[`.github/workflows/inspector.yml`](../.github/workflows/inspector.yml) is a
check-only source workflow. It has `contents: read` permission, a 45-minute job
bound, no secrets, publication, deployment, artifact uploads, release work, or
manual-dispatch trigger. It pins the official action commits verified by public
`git ls-remote` at authoring time:

- `actions/checkout` `fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09` (`v5.1.0`);
- `actions/setup-node` `a0853c24544627f65ddf259abe73b1d18a591444` (`v5.0.0`).

The workflow sets Node `24.14.1`, reads the `1.94` channel from
`rust-toolchain.toml` before installing rustfmt/Clippy, checks formatting before
asset generation, uses the locked install and documented frontend/Rust gates, and
builds release candidates through `pnpm build`. On Linux it explicitly opts into
the production browser scenarios with those candidates. It searches only
`/usr/bin/google-chrome`, `/usr/bin/google-chrome-stable`, `/usr/bin/chromium`,
and `/usr/bin/chromium-browser`; absence is an error, never an implicit skip.
The workflow relies on a compatible browser being present on `ubuntu-24.04` rather
than installing one; its image-managed version is printed in the job log and is
not a separately pinned browser dependency. Creating this workflow does not
authorize pushing it or executing remote CI. At source authoring time YAML syntax
was parsed with the available Ruby Psych parser, but `actionlint` was unavailable,
so action-specific static linting remains a verification gap.

Local browser evidence supplied for the embedded candidate covers three explicit
Brave scenarios: production appearance/CSP/responsiveness, replay confirmation
and one execution, and restart with old capability `401`, fresh capability `200`,
and empty new history. The restart browser assertion is deliberately narrow: it
shows that expired UI removes confirmation controls and emits no POST. It does
not claim that a click on a detached DOM button invoked a retained React handler;
direct `ReplayController` tests cover controller invalidation separately.

A separate clean-source snapshot (baseline plus allowlisted task files, without
local dependencies or generated assets) passed frozen installation and the
supported release workspace build. Its generated assets and candidate binary
hashes matched the original candidate. All 11 compiled build-guard cases and all
three production browser scenarios also passed against that snapshot's outputs.
The server Dockerfile built successfully; a network-isolated smoke verified uid
100, the daemon version, and absence of frontend runtime tools.

These are inspector-only checkpoints, not final release qualification. Caddy
scenarios remain unqualified: five packaged scenarios and four Caddy integration
scenarios are ignored in the default workspace run. Full Docker runtime
acceptance, Windows, manual suspend/resume, final combined workspace gates, and
production acceptance also remain unqualified. The separate connection-reliability
work, including any heartbeat, is not integrated or claimed by this inspector
documentation.
