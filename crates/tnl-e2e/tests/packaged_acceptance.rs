//! Per-task executable harness qualification, not immutable release acceptance.
//! Explicit binaries only; all traffic and state stay in isolated local fixtures.
#![cfg(unix)]
#[path = "support/packaged.rs"]
mod packaged;

use std::time::Duration;

use packaged::{
    header, http, poll, public_post, Backend, Fixture, Inspector, Process, Task, DEADLINE,
};
use reqwest::Method;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

fn assert_private(value: &Value) {
    let text = value.to_string();
    for secret in [
        "AUTH_CANARY",
        "COOKIE_CANARY",
        "BODY_CANARY",
        "RESPONSE_CANARY",
        "SESSION_CANARY",
    ] {
        assert!(
            !text.contains(secret),
            "secret must not appear in inspector display/API"
        );
    }
}
async fn correlation(child: &mut Process, backend: &mut Backend, id: &str, body: &[u8]) {
    let observed = backend.next().await;
    assert_eq!(observed.body, body);
    assert_eq!(header(&observed.head, "x-tnl-request-id"), id);
    assert_eq!(header(&observed.head, "x-forwarded-for"), "127.0.0.2");
    let log = child.log(id).await;
    assert_eq!(log["status"], 201);
    assert_eq!(log["visitor_ip"], "127.0.0.2");
    assert_eq!(log["peer_ip"], "127.0.0.1");
    assert_eq!(log["visitor_ip_source"], "x_forwarded_for");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit TNL_TEST_CLIENT_BIN, TNL_TEST_DAEMON_BIN and TNL_TEST_CADDY_BIN"]
async fn packaged_public_defaults_websocket_and_daemon_restart() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let mut fixture = Fixture::start().await;
        let mut backend = Backend::start().await;
        let mut child = fixture.client(backend.addr, "public", &[]);
        fixture.registered(&mut child, "public").await;
        let (id, body) = public_post(&fixture, "public").await;
        correlation(&mut child, &mut backend, &id, &body).await;
        assert!(
            !child.stderr().contains("Inspector (private"),
            "inspection must stay opt-in"
        );
        application_websocket(&fixture, &mut child, &mut backend).await;
        fixture.restart_daemon().await;
        fixture.registered(&mut child, "public").await;
        let (new_id, body) = public_post(&fixture, "public").await;
        assert_ne!(id, new_id);
        correlation(&mut child, &mut backend, &new_id, &body).await;
        child.interrupt().await;
        drop(child);
        let mut restarted = fixture.client(backend.addr, "public", &[]);
        fixture.registered(&mut restarted, "public").await;
        let (restart_id, body) = public_post(&fixture, "public").await;
        correlation(&mut restarted, &mut backend, &restart_id, &body).await;
        restarted.interrupt().await;
        backend.none().await;
    })
    .await
    .expect("bounded executable default/reconnect fixture");
}

async fn application_websocket(fixture: &Fixture, child: &mut Process, backend: &mut Backend) {
    use async_tungstenite::{
        tokio::TokioAdapter,
        tungstenite::{protocol::Role, Message},
        WebSocketStream,
    };
    use futures::StreamExt;
    let mut io = tokio::net::TcpStream::connect(fixture.addr).await.unwrap();
    io.write_all(b"GET /socket HTTP/1.1\r\nHost: public.t.test\r\nConnection: upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n\x82\x85\x00\x00\x00\x00early").await.unwrap();
    let response = packaged::head(&mut io).await;
    assert!(
        response.starts_with("HTTP/1.1 101"),
        "application WebSocket must upgrade"
    );
    assert!(!response.to_ascii_lowercase().contains("connection: close"));
    let id = header(&response, "x-tnl-request-id");
    assert_eq!(header(&backend.next().await.head, "x-tnl-request-id"), id);
    let mut ws = WebSocketStream::from_raw_socket(TokioAdapter::new(io), Role::Client, None).await;
    for bytes in [b"first", b"early"] {
        assert_eq!(
            ws.next().await.unwrap().unwrap(),
            Message::Binary(bytes.to_vec().into())
        );
    }
    let (http_id, body) = public_post(fixture, "public").await;
    correlation(child, backend, &http_id, &body).await;
    let binary = Message::Binary(vec![0, 255, 128, 1].into());
    ws.send(binary.clone()).await.unwrap();
    assert_eq!(ws.next().await.unwrap().unwrap(), binary);
    ws.close(None).await.unwrap();
    assert!(matches!(
        ws.next().await.unwrap().unwrap(),
        Message::Close(_)
    ));
    drop(ws);
    assert_eq!(child.log(&id).await["status"], 101);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit TNL_TEST_CLIENT_BIN, TNL_TEST_DAEMON_BIN and TNL_TEST_CADDY_BIN"]
async fn packaged_inspection_preview_redaction_and_confirmed_original_replay() {
    tokio::time::timeout(Duration::from_secs(90), async {
        let mut fixture = Fixture::start().await;
        let mut backend = Backend::start().await;
        for (name, options, mode) in [
            (
                "inspect-off",
                vec!["--inspect", "--replay-retain-secrets"],
                "disabled",
            ),
            (
                "inspect-json",
                vec![
                    "--inspect",
                    "--replay-retain-secrets",
                    "--inspect-bodies",
                    "json",
                ],
                "json",
            ),
        ] {
            let mut child = fixture.client(backend.addr, name, &options);
            let inspector = child.inspector().await;
            fixture.registered(&mut child, name).await;
            assert_eq!(
                http()
                    .get(format!("{}/api/v1/requests", inspector.origin))
                    .send()
                    .await
                    .unwrap()
                    .status(),
                401
            );
            assert_eq!(
                inspector
                    .request(Method::GET, "/api/v1/stats")
                    .header("Origin", "http://foreign.invalid")
                    .send()
                    .await
                    .unwrap()
                    .status(),
                403
            );
            let stats = inspector.get("/api/v1/stats").await;
            assert_eq!(stats["schema_version"], 1);
            assert_eq!(stats["tunnel"]["connected"], true);
            let (id, body) = public_post(&fixture, name).await;
            let original = backend.next().await;
            assert_eq!(header(&original.head, "x-tnl-request-id"), id);
            assert_eq!(original.body, body);
            let detail = inspector.capture(&id).await;
            check_preview(&detail, mode);
            replay(
                &inspector,
                &detail,
                &original,
                &mut backend,
                if mode == "json" {
                    ReplayResponse::Closed
                } else {
                    ReplayResponse::Created
                },
            )
            .await;
            child.interrupt().await;
            assert!(
                tokio::net::TcpStream::connect(inspector.addr)
                    .await
                    .is_err(),
                "Ctrl-C must close inspector listener"
            );
            let _listener = tokio::net::TcpListener::bind(inspector.addr)
                .await
                .expect("no orphan inspector listener");
        }
    })
    .await
    .expect("bounded executable inspector/replay fixture");
}
fn check_preview(detail: &Value, mode: &str) {
    assert_private(detail);
    assert_eq!(detail["status"], 201);
    assert_eq!(detail["state"], "complete");
    if mode == "disabled" {
        for message in ["request", "response"] {
            assert!(detail[message]["body"]["text"].is_null());
            assert_eq!(detail[message]["body"]["reason"], "disabled");
        }
    } else {
        let preview = detail["request"]["body"]["text"]
            .as_str()
            .expect("explicit JSON preview");
        let value: Value = serde_json::from_str(preview).unwrap();
        assert_eq!(value["message"], "hello");
        assert_eq!(value["password"], "[REDACTED]");
    }
    let headers = detail["request"]["headers"].as_array().unwrap();
    for name in ["authorization", "cookie"] {
        assert!(headers.iter().any(
            |h| h["name"].as_str().unwrap().eq_ignore_ascii_case(name) && h["redacted"] == true
        ));
    }
}
#[derive(Clone, Copy)]
enum ReplayResponse {
    Created,
    Closed,
    Redirect(std::net::SocketAddr),
}

fn rejected_headers(inspector: &Inspector, length: usize) -> Vec<(&'static str, String, u16)> {
    vec![
        (
            "identity encoding",
            "Content-Encoding: identity\r\n".to_string(),
            400,
        ),
        ("gzip encoding", "Content-Encoding: gzip\r\n".into(), 400),
        (
            "duplicate encoding",
            "Content-Encoding: identity\r\nContent-Encoding: identity\r\n".into(),
            400,
        ),
        (
            "duplicate Host",
            format!("Host: {}\r\n", inspector.addr),
            403,
        ),
        (
            "duplicate Origin",
            format!("Origin: {}\r\n", inspector.origin),
            403,
        ),
        (
            "duplicate Fetch site",
            "Sec-Fetch-Site: same-origin\r\n".into(),
            403,
        ),
        (
            "duplicate Fetch mode",
            "Sec-Fetch-Mode: cors\r\n".into(),
            403,
        ),
        (
            "duplicate Fetch destination",
            "Sec-Fetch-Dest: empty\r\n".into(),
            403,
        ),
        (
            "duplicate authorization",
            "Authorization: Bearer invalid\r\n".into(),
            401,
        ),
        (
            "duplicate length",
            format!("Content-Length: {length}\r\n"),
            400,
        ),
        (
            "duplicate content type",
            "Content-Type: application/json\r\n".into(),
            400,
        ),
        (
            "transfer coding",
            "Transfer-Encoding: chunked\r\n".into(),
            400,
        ),
    ]
}

async fn reject_raw_mutations(
    inspector: &Inspector,
    path: &str,
    valid: &Value,
    backend: &mut Backend,
) {
    let valid = valid.to_string();
    let mut cases = vec![
        ("malformed JSON", b"{".to_vec(), String::new(), 400),
        ("invalid UTF-8", vec![255], String::new(), 400),
        (
            "duplicate schema",
            valid
                .replacen('{', "{\"schema_version\":1,", 1)
                .into_bytes(),
            String::new(),
            400,
        ),
        (
            "JSON body bound",
            [vec![b' '; 1025], valid.as_bytes().to_vec()].concat(),
            String::new(),
            400,
        ),
    ];
    for (name, extra, status) in rejected_headers(inspector, valid.len()) {
        cases.push((name, valid.as_bytes().to_vec(), extra, status));
    }
    for (name, field) in [
        ("target override", "\"target\":\"127.0.0.1:1\""),
        ("header override", "\"headers\":{}"),
        ("body override", "\"body\":\"override\""),
        ("unknown JSON field", "\"unknown\":true"),
    ] {
        cases.push((
            name,
            valid.replacen('{', &format!("{{{field},"), 1).into_bytes(),
            String::new(),
            400,
        ));
    }
    let parsed: Value = serde_json::from_str(&valid).unwrap();
    if path.ends_with("/confirm") {
        for key in ["replay_id", "confirmation_token", "confirm_side_effects"] {
            let duplicate = valid.replacen('{', &format!("{{\"{key}\":{},", parsed[key]), 1);
            cases.push((
                "duplicate confirmation field",
                duplicate.into_bytes(),
                String::new(),
                400,
            ));
        }
        let mut unconfirmed = parsed;
        unconfirmed["confirm_side_effects"] = json!(false);
        cases.push((
            "acknowledgement false",
            unconfirmed.to_string().into_bytes(),
            String::new(),
            400,
        ));
    }
    for (name, body, extra, expected) in cases {
        let before = backend.connections();
        eprintln!("raw mutation rejection case: {name}");
        assert_eq!(
            inspector.raw_mutation(path, &body, &extra).await,
            expected,
            "{name}"
        );
        backend.unchanged_connections(before).await;
    }
}

async fn reject_missing_browser_context(inspector: &Inspector, path: &str, request: &Value) {
    // Each Fetch Metadata/Origin requirement is independently enforced.
    for missing in [
        "Origin",
        "Sec-Fetch-Site",
        "Sec-Fetch-Mode",
        "Sec-Fetch-Dest",
    ] {
        let mut req = inspector.mutation(path, request).build().unwrap();
        req.headers_mut().remove(missing);
        assert_eq!(http().execute(req).await.unwrap().status(), 403);
    }
}

async fn replay(
    inspector: &Inspector,
    detail: &Value,
    original: &packaged::Observation,
    backend: &mut Backend,
    response: ReplayResponse,
) {
    let before = backend.connections();
    let capture = detail["capture_id"].as_str().unwrap();
    let path = format!("/api/v1/requests/{capture}/replay");
    let prepare_path = format!("{path}/prepare");
    let request = json!({"schema_version": 1});
    reject_missing_browser_context(inspector, &prepare_path, &request).await;
    if matches!(response, ReplayResponse::Created) {
        reject_raw_mutations(inspector, &prepare_path, &request, backend).await;
    }
    let prepared = inspector
        .mutation(&prepare_path, &request)
        .send()
        .await
        .unwrap();
    assert_eq!(prepared.status(), 200);
    let preview: Value = prepared.json().await.unwrap();
    assert_eq!(preview["schema_version"], 1);
    assert_private(&preview);
    backend.none().await; // Prepare must deliver zero requests.
    let ticket = json!({"schema_version": 1, "replay_id": preview["preview"]["replay_id"],
        "confirmation_token": preview["preview"]["confirmation_token"], "confirm_side_effects": true});
    let confirm_path = format!("{path}/confirm");
    if matches!(response, ReplayResponse::Created) {
        reject_raw_mutations(inspector, &confirm_path, &ticket, backend).await;
    }
    let mut unconfirmed = ticket.clone();
    unconfirmed["confirm_side_effects"] = json!(false);
    assert_eq!(
        inspector
            .mutation(&confirm_path, &unconfirmed)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    backend.none().await;
    match response {
        ReplayResponse::Created => {}
        ReplayResponse::Closed => backend.fail_next_response(),
        ReplayResponse::Redirect(target) => backend.redirect_next_response(target),
    }
    for _ in 0..2 {
        let result = inspector
            .mutation(&confirm_path, &ticket)
            .send()
            .await
            .unwrap();
        assert_eq!(result.status(), 200);
        let result: Value = result.json().await.unwrap();
        assert_eq!(result["schema_version"], 1);
        assert_eq!(result["outcome"]["replay_id"], ticket["replay_id"]);
        assert_private(&result);
    }
    let replayed = backend.next().await;
    assert_eq!(
        replayed.body, original.body,
        "replay must use original secret-bearing bytes"
    );
    assert_eq!(replayed.head.lines().next(), original.head.lines().next());
    for name in ["host", "authorization", "cookie", "content-type"] {
        assert_eq!(header(&replayed.head, name), header(&original.head, name));
    }
    let outcome_path = format!("/api/v1/replays/{}", ticket["replay_id"].as_str().unwrap());
    let outcome = tokio::time::timeout(DEADLINE, async {
        loop {
            let value = inspector.get(&outcome_path).await;
            assert_eq!(value["schema_version"], 1);
            if value["outcome"]["state"] != "running" {
                break value;
            }
            poll().await;
        }
    })
    .await
    .unwrap();
    if matches!(response, ReplayResponse::Closed) {
        assert_eq!(outcome["outcome"]["state"], "failed");
        assert_eq!(outcome["outcome"]["delivery"], "possibly_sent");
        assert!(outcome["outcome"]["status"].is_null());
    } else {
        assert_eq!(outcome["outcome"]["state"], "complete");
        assert_eq!(
            outcome["outcome"]["status"],
            if matches!(response, ReplayResponse::Redirect(_)) {
                302
            } else {
                201
            }
        );
        assert_eq!(outcome["outcome"]["delivery"], "response_received");
    }
    assert_eq!(outcome["outcome"]["parent_capture_id"], capture);
    assert_private(&outcome);
    backend.unchanged_connections(before + 1).await; // Exactly one replay TCP connection, never a follow-up or retry.
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit TNL_TEST_CLIENT_BIN, TNL_TEST_DAEMON_BIN and TNL_TEST_CADDY_BIN"]
async fn packaged_protected_cli_rejects_plaintext_and_untrusted_tls() {
    tokio::time::timeout(Duration::from_secs(45), async {
        let fixture = Fixture::start().await;
        let mut backend = Backend::start().await;
        let mut child = fixture.client(backend.addr, "denied-http", &["--max-concurrent", "1"]);
        assert!(!child.exited().await.success());
        assert!(child.stderr().contains("require an https endpoint"));
        assert!(!child.stdout().contains("Tunnel:"));
        backend.none().await;
        reject_self_signed(&fixture, &mut backend).await;
    })
    .await
    .expect("bounded executable protected-endpoint rejection");
}
async fn reject_self_signed(fixture: &Fixture, backend: &mut Backend) {
    use std::sync::Arc;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let certificate = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.cert.der().clone()],
            rustls::pki_types::PrivateKeyDer::try_from(certificate.key_pair.serialize_der())
                .unwrap(),
        )
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let config = fixture.alternate_config(&format!("https://{addr}"));
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let _server = Task(tokio::spawn(async move {
        let (io, _) = listener.accept().await.unwrap();
        let result = tokio_rustls::TlsAcceptor::from(Arc::new(tls))
            .accept(io)
            .await;
        assert!(
            result.is_err(),
            "production CLI must reject the untrusted test issuer"
        );
        tx.send(()).await.unwrap();
    }));
    let mut cmd = fixture.client_command();
    cmd.env("TNL_CONFIG", config).args([
        "http",
        &backend.addr.to_string(),
        "denied-tls",
        "--max-concurrent",
        "1",
    ]);
    let mut child = Process::start(cmd, fixture.home.path(), "denied-tls");
    tokio::time::timeout(DEADLINE, rx.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(DEADLINE, async {
        loop {
            child.alive();
            if child.stderr().to_ascii_lowercase().contains("certificate") {
                break;
            }
            poll().await;
        }
    })
    .await
    .expect("specific certificate rejection diagnostic");
    assert!(!child.stdout().contains("Tunnel:"));
    child.interrupt().await;
    backend.none().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit TNL_TEST_CLIENT_BIN, TNL_TEST_DAEMON_BIN and TNL_TEST_CADDY_BIN"]
async fn packaged_child_cleanup_after_panic_and_inspector_startup_failure() {
    tokio::time::timeout(Duration::from_secs(45), async {
        let mut fixture = Fixture::start().await;
        let mut backend = Backend::start().await;
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = occupied.local_addr().unwrap().port().to_string();
        let mut failed = fixture.client(
            backend.addr,
            "startup-failure",
            &["--inspect", "--inspect-port", &port],
        );
        assert!(!failed.exited().await.success());
        assert!(failed.stderr().contains("bind loopback inspector"));
        assert!(!failed.stderr().contains("#token="));
        assert!(!failed.stdout().contains("Tunnel:"));
        backend.none().await;
        let mut child = fixture.client(backend.addr, "panic-cleanup", &["--inspect"]);
        let inspector = child.inspector().await;
        fixture.registered(&mut child, "panic-cleanup").await;
        let addr = fixture.addr;
        let panic = std::panic::catch_unwind(move || {
            let _owned_children = (child, fixture);
            panic!("intentional fixture panic exercises child kill/wait");
        });
        assert!(panic.is_err());
        for released in [addr, inspector.addr] {
            let _listener = tokio::net::TcpListener::bind(released)
                .await
                .expect("panic must release fixture listeners");
        }
    })
    .await
    .expect("bounded executable failure cleanup");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit TNL_TEST_CLIENT_BIN, TNL_TEST_DAEMON_BIN and TNL_TEST_CADDY_BIN"]
async fn packaged_replay_does_not_follow_loopback_redirect() {
    tokio::time::timeout(Duration::from_secs(45), async {
        let mut fixture = Fixture::start().await;
        let mut backend = Backend::start().await;
        let mut destination = Backend::start().await;
        let mut child = fixture.client(
            backend.addr,
            "redirect",
            &["--inspect", "--replay-retain-secrets"],
        );
        let inspector = child.inspector().await;
        fixture.registered(&mut child, "redirect").await;
        let (id, _) = public_post(&fixture, "redirect").await;
        let original = backend.next().await;
        let detail = inspector.capture(&id).await;
        replay(
            &inspector,
            &detail,
            &original,
            &mut backend,
            ReplayResponse::Redirect(destination.addr),
        )
        .await;
        destination.unchanged_connections(0).await;
        child.interrupt().await;
    })
    .await
    .expect("bounded executable redirect refusal");
}

#[tokio::test]
#[should_panic(expected = "unexpected backend TCP connection")]
async fn backend_connection_counter_rejects_headerless_followup() {
    let mut backend = Backend::start().await;
    let _socket = tokio::net::TcpStream::connect(backend.addr).await.unwrap();
    tokio::time::timeout(DEADLINE, async {
        while backend.connections() == 0 {
            poll().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(backend.connections(), 1);
    // Counterfactual: a follow-up TCP connection without HTTP must fail the same
    // zero-connection oracle used by rejection and redirect assertions.
    backend.unchanged_connections(0).await;
}
