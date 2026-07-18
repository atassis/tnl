//! Dev-server host smoothing: a backend that rejects the tunnel host (Vite-style
//! `403 Blocked request` unless the `Host` is a loopback `ip:port`) must be
//! auto-healed by the default `Auto` mode after the first block, and left
//! untouched by `--host-header=preserve`.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use tnl::forwarder::ForwardCtx;
use tnl::host_header::HostHeader;
use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config as TnldConfig;
use tnld::serve::spawn_server;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// Raw-TCP backend that mimics a dev server's host allowlist: `200` only when
/// the forwarded `Host` is a loopback `ip:port`, else a Vite-shaped `403`.
async fn spawn_host_checking_backend() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let host = req
                    .lines()
                    .find_map(|l| l.strip_prefix("Host: ").or_else(|| l.strip_prefix("host: ")))
                    .unwrap_or("")
                    .trim();
                let allowed = host.starts_with("127.")
                    || host.starts_with("[::1]")
                    || host.starts_with("localhost");
                let (status, body) = if allowed {
                    ("200 OK", "ok\n")
                } else {
                    ("403 Forbidden", "Blocked request. This host is not allowed.\n")
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
                let _ = sock.shutdown().await;
            });
        }
    });
    port
}

async fn spawn_tnld() -> String {
    let hash = hash_plaintext("tnl_HOSTRW").unwrap();
    let tokens = TokensFile {
        tokens: vec![TokenEntry {
            name: "e2e".into(),
            hash,
        }],
    };
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), toml::to_string(&tokens).unwrap()).unwrap();
    let path = tmp.into_temp_path();
    let tokens_file = path.to_string_lossy().into_owned();
    std::mem::forget(path);
    let cfg = TnldConfig {
        listen: "127.0.0.1:0".into(),
        public_url: "http://test".into(),
        hostname_root: "t.example.com".into(),
        tokens_file,
        session_grace_sec: 30,
    };
    spawn_server(cfg).await.unwrap().local_addr.to_string()
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .unwrap()
}

async fn get_status(tnld_addr: &str, host: &str) -> u16 {
    client()
        .get(format!("http://{tnld_addr}/"))
        .header("Host", host)
        .header("Connection", "close")
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn auto_rewrite_after_block() {
    let backend = spawn_host_checking_backend().await;
    let tnld_addr = spawn_tnld().await;

    let session = tnl::client::connect_and_create(&format!("http://{tnld_addr}"), "tnl_HOSTRW", "auto")
        .await
        .unwrap();
    let _ctrl = session.control;
    let ctx = ForwardCtx::new("auto".into(), None, env!("CARGO_PKG_VERSION"));
    let _accept = tokio::spawn(tnl::client::run_accept_loop(
        session.session,
        tnl::target::Target::LocalhostPort(backend),
        ctx,
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;

    // First request: the real host is forwarded and blocked.
    assert_eq!(
        get_status(&tnld_addr, "auto.t.example.com").await,
        403,
        "first request should surface the dev-server block once",
    );
    // Detection flipped on rewrite; subsequent requests present a loopback Host.
    assert_eq!(
        get_status(&tnld_addr, "auto.t.example.com").await,
        200,
        "after the block, Host should be auto-rewritten so the backend serves",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn preserve_never_rewrites() {
    let backend = spawn_host_checking_backend().await;
    let tnld_addr = spawn_tnld().await;

    let session = tnl::client::connect_and_create(&format!("http://{tnld_addr}"), "tnl_HOSTRW", "keep")
        .await
        .unwrap();
    let _ctrl = session.control;
    let ctx = ForwardCtx {
        tunnel: "keep".into(),
        log_tx: None,
        version: env!("CARGO_PKG_VERSION"),
        host_header: HostHeader::Preserve,
        host_public: "keep.t.example.com".into(),
        rewrite_active: Arc::new(AtomicBool::new(false)),
        warned: Arc::new(AtomicBool::new(false)),
    };
    let _accept = tokio::spawn(tnl::client::run_accept_loop(
        session.session,
        tnl::target::Target::LocalhostPort(backend),
        ctx,
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Preserve never rewrites, so both requests stay blocked.
    assert_eq!(get_status(&tnld_addr, "keep.t.example.com").await, 403);
    assert_eq!(get_status(&tnld_addr, "keep.t.example.com").await, 403);
}
