# Dev-Server Host Smoothing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: use superpowers:executing-plans or subagent-driven-development. Steps use checkbox syntax.

**Goal:** When a local dev server rejects the tunnel's `Host` (Vite/webpack/Django/Rails allowlists), tnl auto-rewrites the forwarded `Host` to the address it connected to and tells the user once — with a `--host-header` override.

**Architecture:** Client-only. Two new small modules (`host_header`, `block_detect`); the forwarder rewrites the `Host:` request line on evidence (or by flag) and detects host-block responses to flip a per-tunnel rewrite flag. No daemon or wire change.

**Tech Stack:** Rust, tokio, existing `tnl` forwarder pipeline.

Spec: `docs/specs/2026-07-19-dev-server-friction-smoothing-design.md`.

---

### Task 1: `host_header` module (enum + Host-line rewrite)

**Files:**
- Create: `crates/tnl/src/host_header.rs`
- Modify: `crates/tnl/src/lib.rs` (add `pub mod host_header;`)

- [ ] **Step 1: Write failing unit tests** (in `host_header.rs` `#[cfg(test)]`):

```rust
#[test]
fn parse_modes() {
    assert_eq!(HostHeader::parse(None), HostHeader::Auto);
    assert_eq!(HostHeader::parse(Some("preserve")), HostHeader::Preserve);
    assert_eq!(HostHeader::parse(Some("rewrite")), HostHeader::Rewrite);
    assert_eq!(HostHeader::parse(Some("myapp.local")), HostHeader::Fixed("myapp.local".into()));
}

#[test]
fn rewrite_replaces_host_value_preserving_rest() {
    let head = b"GET /x HTTP/1.1\r\nHost: sub.t.example.com\r\nAccept: */*\r\n\r\n";
    let out = rewrite_host(head, "127.0.0.1:4321");
    assert_eq!(
        out,
        b"GET /x HTTP/1.1\r\nHost: 127.0.0.1:4321\r\nAccept: */*\r\n\r\n".to_vec()
    );
}

#[test]
fn rewrite_is_case_insensitive_on_name() {
    let head = b"GET / HTTP/1.1\r\nhost: old\r\n\r\n";
    let out = rewrite_host(head, "[::1]:8080");
    assert_eq!(out, b"GET / HTTP/1.1\r\nHost: [::1]:8080\r\n\r\n".to_vec());
}

#[test]
fn rewrite_inserts_host_when_absent() {
    let head = b"GET / HTTP/1.1\r\nAccept: */*\r\n\r\n";
    let out = rewrite_host(head, "127.0.0.1:9");
    assert_eq!(out, b"GET / HTTP/1.1\r\nHost: 127.0.0.1:9\r\nAccept: */*\r\n\r\n".to_vec());
}
```

- [ ] **Step 2: Run, expect fail** — `cargo test -p tnl --lib host_header` → FAIL (module missing).

- [ ] **Step 3: Implement** `crates/tnl/src/host_header.rs`:

```rust
//! `Host` header handling for forwarded requests (dev-server allowlist smoothing).

/// What `Host` the backend should see.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostHeader {
    /// Forward the real host; rewrite to the connected address only after a
    /// detected dev-server host block (default).
    Auto,
    /// Never rewrite.
    Preserve,
    /// Always rewrite to the connected `resolved_addr` from the first request.
    Rewrite,
    /// Always rewrite to an explicit value.
    Fixed(String),
}

impl HostHeader {
    #[must_use]
    pub fn parse(s: Option<&str>) -> Self {
        match s {
            None => Self::Auto,
            Some("preserve") => Self::Preserve,
            Some("rewrite") => Self::Rewrite,
            Some(v) => Self::Fixed(v.to_string()),
        }
    }
}

/// Replace the `Host:` header value in a raw HTTP/1.x request head, preserving
/// line order and CRLF framing. Case-insensitive on the header name. If no
/// `Host` line exists, one is inserted immediately after the request line.
/// Non-UTF-8 input is returned unchanged.
#[must_use]
pub fn rewrite_host(head: &[u8], new_host: &str) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(head) else {
        return head.to_vec();
    };
    let mut out = String::with_capacity(text.len() + new_host.len() + 8);
    let mut replaced = false;
    let mut rest = text;
    let mut is_first = true;
    while let Some(pos) = rest.find("\r\n") {
        let line = &rest[..pos];
        rest = &rest[pos + 2..];
        if !is_first && !replaced && line.len() >= 5 && line[..5].eq_ignore_ascii_case("host:") {
            out.push_str("Host: ");
            out.push_str(new_host);
            replaced = true;
        } else {
            out.push_str(line);
        }
        out.push_str("\r\n");
        is_first = false;
        if line.is_empty() {
            out.push_str(rest);
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    if !replaced {
        if let Some(pos) = out.find("\r\n") {
            let (a, b) = out.split_at(pos + 2);
            return format!("{a}Host: {new_host}\r\n{b}").into_bytes();
        }
    }
    out.into_bytes()
}
```

Add `pub mod host_header;` to `crates/tnl/src/lib.rs` (alphabetical with siblings).

- [ ] **Step 4: Run, expect pass** — `cargo test -p tnl --lib host_header` → PASS.
- [ ] **Step 5: Commit** — `git add -A && git commit -m "feat(tnl): host_header module (mode enum + Host-line rewrite)"`

---

### Task 2: `block_detect` module (host-block signature detection)

**Files:**
- Create: `crates/tnl/src/block_detect.rs`
- Modify: `crates/tnl/src/lib.rs` (add `pub mod block_detect;`)

- [ ] **Step 1: Failing tests:**

```rust
#[test]
fn detects_vite_block() {
    let body = b"HTTP/1.1 403 Forbidden\r\n\r\nBlocked request. This host is not allowed.";
    assert_eq!(detect(Some(403), body), Some("Vite"));
}
#[test]
fn detects_django_and_webpack_and_rails() {
    assert_eq!(detect(Some(400), b"...DisallowedHost at /..."), Some("Django"));
    assert_eq!(detect(Some(403), b"...Invalid Host header..."), Some("webpack-dev-server"));
    assert_eq!(detect(Some(403), b"...Blocked host: x..."), Some("Rails"));
}
#[test]
fn ignores_normal_403_and_missing_status() {
    assert_eq!(detect(Some(403), b"HTTP/1.1 403 Forbidden\r\n\r\nnope"), None);
    assert_eq!(detect(None, b"Blocked request"), None);
}
```

- [ ] **Step 2: Run, expect fail** — `cargo test -p tnl --lib block_detect` → FAIL.

- [ ] **Step 3: Implement** `crates/tnl/src/block_detect.rs`:

```rust
//! Detects dev-server "host not allowed" responses so the forwarder can flip on
//! Host rewrite. Advice-only: never modifies the response.

/// Returns a short framework label if `(status, body_prefix)` matches a known
/// host-block. `body_prefix` may include the status line + headers + body start.
#[must_use]
pub fn detect(status: Option<u16>, body_prefix: &[u8]) -> Option<&'static str> {
    let status = status?;
    let text = String::from_utf8_lossy(body_prefix);
    const SIGS: &[(u16, &str, &str)] = &[
        (403, "Blocked request", "Vite"),
        (403, "Invalid Host header", "webpack-dev-server"),
        (400, "DisallowedHost", "Django"),
        (400, "Invalid HTTP_HOST", "Django"),
        (403, "Blocked host", "Rails"),
    ];
    SIGS.iter()
        .find(|(code, needle, _)| status == *code && text.contains(needle))
        .map(|(_, _, label)| *label)
}
```

Add `pub mod block_detect;` to `lib.rs`.

- [ ] **Step 4: Run, expect pass** — `cargo test -p tnl --lib block_detect` → PASS.
- [ ] **Step 5: Commit** — `git commit -am "feat(tnl): block_detect module (dev-server host-block signatures)"`

---

### Task 3: Wire `ForwardCtx` + forwarder rewrite/detect

**Files:**
- Modify: `crates/tnl/src/forwarder.rs`

- [ ] **Step 1:** Extend `ForwardCtx` (after `version`):

```rust
    pub host_header: crate::host_header::HostHeader,
    /// Full public host (`<sub>.t.example.com`) for the block notice.
    pub host_public: String,
    /// Set once a host block is detected; makes Auto mode rewrite subsequent requests.
    pub rewrite_active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Ensures the block notice prints at most once per tunnel.
    pub warned: std::sync::Arc<std::sync::atomic::AtomicBool>,
```

Add a constructor so existing call sites stay short:

```rust
impl ForwardCtx {
    #[must_use]
    pub fn new(tunnel: String, log_tx: Option<mpsc::Sender<LogLine>>, version: &'static str) -> Self {
        Self {
            tunnel,
            log_tx,
            version,
            host_header: crate::host_header::HostHeader::Auto,
            host_public: String::new(),
            rewrite_active: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            warned: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}
```

Extend the manual `Debug` impl with the three new fields (`host_header`, `host_public`, and `rewrite_active.load(Relaxed)`), since `#[derive(Debug)]` is not used.

- [ ] **Step 2:** In `forward`, compute the rewrite value and apply it to `head_bytes` **before** `tcp.write_all(&head_bytes)` (i.e. right after the successful `connect_local`, replacing the `let (mut tcp, resolved_addr) = ...` unpack with a follow-on rewrite):

```rust
    // Decide the Host we present to the backend.
    use crate::host_header::{rewrite_host, HostHeader};
    use std::sync::atomic::Ordering;
    let rewrite_to: Option<String> = match &ctx.host_header {
        HostHeader::Preserve => None,
        HostHeader::Fixed(v) => Some(v.clone()),
        HostHeader::Rewrite => Some(resolved_addr.to_string()),
        HostHeader::Auto => ctx
            .rewrite_active
            .load(Ordering::Relaxed)
            .then(|| resolved_addr.to_string()),
    };
    let head_bytes = match &rewrite_to {
        Some(host) => rewrite_host(&head_bytes, host),
        None => head_bytes,
    };
```

(`head_bytes` becomes `let` rebinding; the later `head_bytes.len()` accounting keeps working on the rewritten buffer.)

- [ ] **Step 3:** After `let status = self::peek::parse_response_status(&resp_buf);` (success path, ~line 366), add detection + notice for Auto mode:

```rust
    if matches!(ctx.host_header, HostHeader::Auto) && !ctx.rewrite_active.load(Ordering::Relaxed) {
        if let Some(fw) = crate::block_detect::detect(status, &resp_buf) {
            ctx.rewrite_active.store(true, Ordering::Relaxed);
            if !ctx.warned.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "! dev server blocked host '{}' (looks like {fw}).\n  \
                     auto-rewriting Host -> {} for this tunnel.\n  \
                     override: --host-header=preserve  (or =<host>)",
                    ctx.host_public, resolved_addr,
                );
            }
        }
    }
```

- [ ] **Step 4:** Bump the response tee cap 512 → 1024 so the block body phrase is captured. Change the two `512` literals in `resp_pump` and the `resp_peek` capacity to `1024`.

- [ ] **Step 5: Run** — `cargo build -p tnl` → compiles. (`ForwardCtx` literal call sites now break; fixed in Task 4.)

---

### Task 4: Fix `ForwardCtx` construction sites + add `--host-header` CLI

**Files:**
- Modify: `crates/tnl/src/reconnect.rs`, `crates/tnl/src/commands/http.rs`, `crates/tnl/src/main.rs`
- Modify: every test that builds `ForwardCtx { ... }`

- [ ] **Step 1:** `reconnect.rs` — thread a `host_header` param into `run` (add `host_header: crate::host_header::HostHeader` to the signature). Create shared state **before** the loop so it persists across reconnects:

```rust
    let rewrite_active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let warned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
```

Replace the `ForwardCtx { tunnel, log_tx, version }` literal (~line 124) with:

```rust
        let ctx = crate::forwarder::ForwardCtx {
            tunnel: session.subdomain.clone(),
            log_tx: hooks.log_tx.clone(),
            version: env!("CARGO_PKG_VERSION"),
            host_header: host_header.clone(),
            host_public: session.hostname.clone(),
            rewrite_active: rewrite_active.clone(),
            warned: warned.clone(),
        };
```

- [ ] **Step 2:** `commands/http.rs` — add `host_header: crate::host_header::HostHeader` param to `run`, pass it into `reconnect::run(...)`.

- [ ] **Step 3:** `main.rs` — add to `Cmd::Http`:

```rust
        /// Host header sent to the backend: `preserve` (default behavior is auto:
        /// rewrite only after a dev-server host block), `rewrite` (always rewrite
        /// to the connected address), or an explicit host value.
        #[arg(long, value_name = "MODE")]
        host_header: Option<String>,
```

Destructure `host_header` in the `Cmd::Http { .. }` arm and pass
`tnl::host_header::HostHeader::parse(host_header.as_deref())` into `http::run`.

- [ ] **Step 4:** Update every `ForwardCtx { tunnel: .., log_tx: .., version: .. }` literal in tests to `ForwardCtx::new(.., .., ..)`. Find them: `git grep -n "ForwardCtx {" crates/`. Expected: the `full_roundtrip*` e2e files and any `crates/tnl/tests/` forwarder tests.

- [ ] **Step 5: Run** — `cargo test -p tnl -p tnl-e2e` → compiles and existing tests pass.
- [ ] **Step 6: Commit** — `git commit -am "feat(tnl): --host-header + wire auto host-rewrite through forwarder/reconnect"`

---

### Task 5: e2e — auto-heal against a host-checking backend

**Files:**
- Create: `crates/tnl-e2e/tests/host_rewrite.rs`

- [ ] **Step 1: Write the e2e** (raw-TCP backend that returns `403 Blocked request` unless `Host` is a loopback `ip:port`; assert default Auto serves the block once then `200`, and `--host-header=preserve`... — since the CLI isn't in-process, drive the client via `run_accept_loop` with a `ForwardCtx` whose `host_header` is set, mirroring `full_roundtrip.rs`). Two sub-tests:
  - `auto_rewrite_after_block`: `ForwardCtx::new(..)` (Auto). First request → 403; second request → 200 (rewrite active).
  - `preserve_never_rewrites`: build `ForwardCtx { .., host_header: HostHeader::Preserve, .. }` → always 403.

Backend logic: read request head, parse `Host:`; if it starts with `127.` / `[::1]` / `localhost` → `200 OK` body `ok`; else `403` body `Blocked request. This host is not allowed.` Both with `Content-Length` + `Connection: close`.

- [ ] **Step 2: Run, expect fail then pass** — `cargo test -p tnl-e2e --test host_rewrite` → after Tasks 1–4, PASS.
- [ ] **Step 3: Commit** — `git commit -am "test(e2e): dev-server host-block auto-rewrite + preserve"`

---

### Task 6: Docs + full gate

- [ ] **Step 1:** CHANGELOG `[Unreleased]`:
```
### Added
- `tnl http --host-header <preserve|rewrite|VALUE>`. By default (auto), when a
  local dev server rejects the tunnel host (Vite/webpack/Django/Rails
  allowlists), tnl rewrites the forwarded `Host` to the address it connected to
  and prints a one-time notice, so dev servers work through the tunnel without
  editing their config.
```
- [ ] **Step 2:** README/RUNBOOK: one line documenting the auto-heal + flag.
- [ ] **Step 3: Full gate** — `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`.
- [ ] **Step 4: Commit** — `git commit -am "docs: --host-header + dev-server host smoothing"`

---

## Self-review notes

- Spec coverage: host rewrite (T1/T3), evidence-driven auto + notice (T3), override modes (T1/T3/T4), generic detector (T2), resolved_addr target (T3), client-only (all). ✓
- Out-of-scope kept out: no daemon change, no forwarded-headers, no retry. ✓
- Type consistency: `HostHeader{Auto,Preserve,Rewrite,Fixed}`, `rewrite_host`, `detect(Option<u16>,&[u8])->Option<&'static str>`, `ForwardCtx::new`, `rewrite_active`/`warned` used identically across tasks. ✓
