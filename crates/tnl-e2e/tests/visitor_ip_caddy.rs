//! Credential-free ingress proof against an operator-supplied local Caddy.
//! Never downloads a binary, reads production config, enables ACME, or uses DNS.
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use tnl_protocol::request_metadata::{DataStreamProtocol, VisitorIpSource};
use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug)]
struct Caddy(Child);
impl Drop for Caddy {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an installed TNL_TEST_CADDY_BIN and 127.0.0.2 source binding"]
async fn visitor_ip_caddy_single_edge_defaults_and_explicit_append_hop() {
    let binary = std::env::var_os("TNL_TEST_CADDY_BIN").expect(
        "set TNL_TEST_CADDY_BIN to an already installed local Caddy; no download is performed",
    );
    let version = Command::new(&binary)
        .arg("version")
        .output()
        .expect("run local Caddy version");
    assert!(version.status.success());
    eprintln!(
        "Caddy version: {}",
        String::from_utf8_lossy(&version.stdout).trim()
    );
    for (append, trust_upstream, expected) in [
        (false, false, "127.0.0.2"),
        (true, false, "127.0.0.2"),
        (true, true, "198.51.100.7"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let daemon = spawn_daemon(temp.path(), trust_upstream).await;
        let backend_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = backend_listener.local_addr().unwrap().port();
        let (observed_tx, mut observed_rx) = tokio::sync::mpsc::channel(4);
        let router = axum::Router::new().fallback(move |headers: axum::http::HeaderMap| {
            let tx = observed_tx.clone();
            async move {
                tx.send(headers["x-forwarded-for"].to_str().unwrap().to_string())
                    .await
                    .unwrap();
                "ok"
            }
        });
        let backend = tokio::spawn(async move {
            axum::serve(backend_listener, router).await.unwrap();
        });
        let (_caddy, caddy_addr) =
            start_caddy(&binary, temp.path(), daemon.local_addr, append).await;
        // The authenticated control WS also crosses Caddy: proves echo survives.
        let session = tnl::client::connect_and_create(
            &format!("http://{caddy_addr}"),
            "tnl_CADDY_TEST",
            "demo",
        )
        .await
        .unwrap();
        assert_eq!(
            session.data_stream_protocol,
            DataStreamProtocol::Http1MetadataV1
        );
        let _control = session.control;
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let ctx = tnl::forwarder::ForwardCtx {
            data_stream_protocol: session.data_stream_protocol,
            ..tnl::forwarder::ForwardCtx::new("demo".into(), Some(tx), env!("CARGO_PKG_VERSION"))
        };
        let accept = tokio::spawn(tnl::client::run_accept_loop(
            session.session,
            tnl::target::Target::LocalhostPort(port),
            ctx,
        ));
        visitor_request(caddy_addr).await;
        let observed = tokio::time::timeout(Duration::from_secs(2), observed_rx.recv())
            .await
            .unwrap()
            .unwrap();
        eprintln!("Observed upstream XFF: {observed}");
        assert_eq!(
            observed,
            if append {
                "203.0.113.66, 198.51.100.7, 127.0.0.2"
            } else {
                "127.0.0.2"
            }
        );
        let record = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.visitor_ip, Some(expected.parse().unwrap()));
        assert_eq!(record.peer_ip, Some("127.0.0.1".parse().unwrap()));
        assert_eq!(
            record.visitor_ip_source,
            Some(VisitorIpSource::XForwardedFor)
        );
        assert!(tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .is_err());
        accept.abort();
        backend.abort();
        daemon.join.abort();
    }
}

async fn spawn_daemon(temp: &std::path::Path, trust_upstream: bool) -> tnld::serve::ServerHandle {
    let tokens = temp.join("tokens.toml");
    std::fs::write(
        &tokens,
        toml::to_string(&TokensFile {
            tokens: vec![TokenEntry {
                name: "synthetic".into(),
                hash: hash_plaintext("tnl_CADDY_TEST").unwrap(),
            }],
        })
        .unwrap(),
    )
    .unwrap();
    let mut trusted_proxies = vec!["127.0.0.1/32".into()];
    if trust_upstream {
        trusted_proxies.push("127.0.0.2/32".into());
    }
    let daemon = tnld::serve::spawn_server(tnld::config::Config {
        listen: "127.0.0.1:0".into(),
        public_url: "http://x".into(),
        hostname_root: "t.x".into(),
        tokens_file: tokens.to_string_lossy().into_owned(),
        session_grace_sec: 30,
        trusted_proxies,
    })
    .await
    .unwrap();
    daemon
}

async fn start_caddy(
    binary: &std::ffi::OsStr,
    temp: &std::path::Path,
    upstream: SocketAddr,
    append: bool,
) -> (Caddy, SocketAddr) {
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let caddy_addr = reserved.local_addr().unwrap();
    drop(reserved);
    let header_up = if append {
        "header_up X-Forwarded-For \"{http.request.header.X-Forwarded-For}, {http.request.remote.host}\""
    } else {
        ""
    };
    let config = format!("{{\n admin off\n auto_https off\n}}\nhttp://:{} {{\n bind 127.0.0.1\n reverse_proxy {} {{\n  header_up Connection close\n  {header_up}\n }}\n}}\n", caddy_addr.port(), upstream);
    eprintln!("Effective test Caddyfile (append={append}):\n{config}");
    let config_path = temp.join("Caddyfile");
    std::fs::write(&config_path, config).unwrap();
    let stderr_path = temp.join("caddy.stderr");
    let caddy = Caddy(
        Command::new(binary)
            .args(["run", "--adapter", "caddyfile", "--config"])
            .arg(&config_path)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&stderr_path).unwrap())
            .spawn()
            .expect("start local HTTP-only Caddy"),
    );
    let ready = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if tokio::net::TcpStream::connect(caddy_addr).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
    assert!(
        ready.is_ok(),
        "Caddy failed to listen: {}",
        std::fs::read_to_string(&stderr_path).unwrap()
    );
    (caddy, caddy_addr)
}

async fn visitor_request(caddy_addr: SocketAddr) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket
        .bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)), 0))
        .expect("prerequisite: bind visitor source 127.0.0.2");
    let mut socket = socket
        .connect(caddy_addr)
        .await
        .expect("prerequisite: reach Caddy from 127.0.0.2");
    socket.write_all(b"GET /visitor HTTP/1.1\r\nHost: demo.t.x\r\nConnection: close\r\nX-Forwarded-For: 203.0.113.66, 198.51.100.7\r\nForwarded: for=203.0.113.66\r\n\r\n").await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), socket.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(
        response.starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&response)
    );
}
