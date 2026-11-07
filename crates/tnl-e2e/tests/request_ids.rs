//! One daemon-owned ID across HTTP projections, negotiated streams and diagnostics.
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};

#[derive(Clone, Default)]
struct VecWriter(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for VecWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for VecWriter {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

fn response_id(response: &reqwest::Response) -> String {
    assert_eq!(
        response
            .headers()
            .get_all("x-tnl-request-id")
            .iter()
            .count(),
        1
    );
    let id = response.headers()["x-tnl-request-id"]
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(id.len(), 26);
    assert_ne!(id, "00000000000000000000000000");
    id
}

async fn record(
    rx: &mut tokio::sync::mpsc::Receiver<tnl::inspector::LogLine>,
) -> tnl::inspector::LogLine {
    tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap()
}

async fn daemon() -> (tempfile::NamedTempFile, tnld::serve::ServerHandle) {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        file.path(),
        toml::to_string(&TokensFile {
            tokens: vec![TokenEntry {
                name: "test".into(),
                hash: hash_plaintext("tnl_TEST").unwrap(),
            }],
        })
        .unwrap(),
    )
    .unwrap();
    let daemon = tnld::serve::spawn_server(tnld::config::Config {
        listen: "127.0.0.1:0".into(),
        public_url: "http://x".into(),
        hostname_root: "t.x".into(),
        tokens_file: file.path().to_string_lossy().into_owned(),
        session_grace_sec: 30,
        trusted_proxies: vec![],
    })
    .await
    .unwrap();
    (file, daemon)
}

async fn backend() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let router = axum::Router::new().fallback(|req: axum::extract::Request| async move {
        assert_eq!(req.headers().get_all("x-tnl-request-id").iter().count(), 1);
        let id = req.headers()["x-tnl-request-id"].to_str().unwrap().to_string();
        let other = req.headers()["x-request-id"].to_str().unwrap().to_string();
        let body = axum::body::to_bytes(req.into_body(), 2 * 1024 * 1024).await.unwrap();
        let mut response = axum::http::Response::new(axum::body::Body::from(
            serde_json::json!({"backend_id": id, "body": String::from_utf8(body.to_vec()).unwrap(), "other": other, "request_id": "application-data"}).to_string()
        ));
        *response.status_mut() = axum::http::StatusCode::IM_A_TEAPOT;
        response.headers_mut().append("x-tnl-request-id", "backend-forgery".parse().unwrap());
        response.headers_mut().append("x-tnl-request-id", "01BX5ZZKBKACTAV9WEVGEMMVRZ".parse().unwrap());
        response.headers_mut().insert("connection", "X-Tnl-Request-Id, close".parse().unwrap());
        response
    });
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (port, task)
}

async fn concurrent_uploads(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    rx: &mut tokio::sync::mpsc::Receiver<tnl::inspector::LogLine>,
    sink: &VecWriter,
) {
    let mut tasks = Vec::new();
    for n in 0..8 {
        let client = client.clone();
        tasks.push(tokio::spawn(async move {
            let body = format!(
                "upload-{n}\r\nX-Tnl-Request-Id: body-data\r\n{}",
                "z".repeat(128 * 1024)
            );
            let response = client
                .post(format!("http://{addr}/same"))
                .header("host", "ids.t.x")
                .header("connection", "X-Tnl-Request-Id, close")
                .header("x-tnl-request-id", "visitor-forgery")
                .header("X-Tnl-Request-Id", "01BX5ZZKBKACTAV9WEVGEMMVRZ")
                .header("x-request-id", "application-id")
                .header("x-large", "a".repeat(9000))
                .body(body.clone())
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 418);
            assert!(!response.headers().contains_key("x-tnl-component"));
            let id = response_id(&response);
            let json: serde_json::Value = response.json().await.unwrap();
            assert_eq!(json["backend_id"], id);
            assert_eq!(json["body"], body);
            assert_eq!(json["other"], "application-id");
            assert_eq!(json["request_id"], "application-data");
            id
        }));
    }
    let mut ids = HashSet::new();
    for task in tasks {
        assert!(ids.insert(task.await.unwrap()));
    }
    let captured = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    for _ in 0..8 {
        let log = record(rx).await;
        assert_eq!(log.path, "/same");
        assert_eq!(log.status, Some(418));
        let id = log.req_id.to_string();
        assert!(ids.remove(&id), "unexpected or duplicate log ID {id}");
        assert!(
            captured
                .lines()
                .any(|line| line.contains("data-plane request")
                    && line.contains(&format!("req_id={id}"))),
            "missing daemon ID {id}: {captured}"
        );
    }
    assert!(ids.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn request_ids_correlate_concurrent_uploads_spoofed_headers_and_local_errors() {
    let sink = VecWriter::default();
    tracing_subscriber::fmt()
        .with_env_filter("tnld=debug,tnl=debug")
        .with_ansi(false)
        .with_writer(sink.clone())
        .try_init()
        .unwrap();
    let (_file, daemon) = daemon().await;
    let (port, backend) = backend().await;
    let session = tnl::client::connect_and_create(
        &format!("http://{}", daemon.local_addr),
        "tnl_TEST",
        "ids",
    )
    .await
    .unwrap();
    let _control = session.control;
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let accept = tokio::spawn(tnl::client::run_accept_loop(
        session.session,
        tnl::target::Target::LocalhostPort(port),
        tnl::forwarder::ForwardCtx {
            data_stream_protocol: session.data_stream_protocol,
            ..tnl::forwarder::ForwardCtx::new("ids".into(), Some(tx), env!("CARGO_PKG_VERSION"))
        },
    ));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .pool_max_idle_per_host(0)
        .build()
        .unwrap();
    concurrent_uploads(&client, daemon.local_addr, &mut rx, &sink).await;

    // Stop the backend: the daemon, client synth body and inspector must agree.
    backend.abort();
    let _ = backend.await;
    for accept_type in ["application/json", "text/html", "text/plain"] {
        let response = client
            .get(format!("http://{}/down", daemon.local_addr))
            .header("host", "ids.t.x")
            .header("accept", accept_type)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 502);
        assert_eq!(response.headers()["x-tnl-component"], "client");
        let id = response_id(&response);
        let body = response.text().await.unwrap();
        assert!(body.contains(&id), "{body}");
        let log = record(&mut rx).await;
        assert_eq!(log.req_id.to_string(), id);
        assert_eq!(log.failure_kind.as_deref(), Some("connect-refused"));
        let captured = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        assert!(
            captured
                .lines()
                .any(|line| line.contains("local connect failed")
                    && line.contains(&format!("req_id={id}"))),
            "{captured}"
        );
    }
    // An unreached tunnel still owns a matching header/body/diagnostic ID.
    let response = client
        .get(format!("http://{}/unknown", daemon.local_addr))
        .header("host", "missing.t.x")
        .header("accept", "application/json")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    let id = response_id(&response);
    let json: serde_json::Value = response.json().await.unwrap();
    assert_eq!(json["request_id"], id);
    let captured = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    assert!(
        captured
            .lines()
            .any(|line| line.contains("no tunnel registered")
                && line.contains(&format!("req_id={id}"))),
        "{captured}"
    );
    assert!(tokio::time::timeout(Duration::from_millis(100), rx.recv())
        .await
        .is_err());
    accept.abort();
    daemon.join.abort();
}
