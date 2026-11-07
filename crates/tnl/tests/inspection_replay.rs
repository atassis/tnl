use tnl::inspection::{
    BoundInspector, CaptureId, Inspection, InspectorService, PreviewMode, TunnelGeneration,
};
use tnl::replay::ReplayController;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn call(
    service: &InspectorService,
    method: &str,
    path: &str,
    body: &str,
    extra: Option<&str>,
) -> (u16, serde_json::Value) {
    let url = service.bootstrap_url();
    let token = url.split("#token=").nth(1).unwrap();
    let headers=extra.map_or_else(||format!("Origin: http://{}\r\nSec-Fetch-Site: same-origin\r\nSec-Fetch-Mode: cors\r\nSec-Fetch-Dest: empty\r\n",service.addr()),str::to_owned);
    let mut socket = tokio::net::TcpStream::connect(service.addr())
        .await
        .unwrap();
    socket.write_all(format!("{method} {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {token}\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",service.addr(),body.len()).as_bytes()).await.unwrap();
    let mut out = Vec::new();
    socket.read_to_end(&mut out).await.unwrap();
    let text = String::from_utf8(out).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    (
        head.split_whitespace().nth(1).unwrap().parse().unwrap(),
        serde_json::from_str(body).unwrap(),
    )
}
async fn fixture(
    backend: std::net::SocketAddr,
) -> (
    InspectorService,
    Inspection,
    ReplayController,
    CaptureId,
    TunnelGeneration,
) {
    let bound = BoundInspector::bind(0).await.unwrap();
    let replay = ReplayController::new(true, bound.addr().port()).unwrap();
    let history = Inspection::with_replay(PreviewMode::Off, replay.clone());
    let generation = TunnelGeneration::new();
    history.activate(generation);
    let mut capture = history.admit_backend(generation, None, backend).unwrap();
    let id = capture.id();
    capture.request(b"POST /action?token=QUERY_RAW_CANARY HTTP/1.1\r\nHost: fixed.example\r\nAuthorization: Bearer AUTH_RAW_CANARY\r\nCookie: COOKIE_RAW_CANARY\r\nContent-Length: 4\r\n\r\nBODY");
    capture.response(b"HTTP/1.1 204 No Content\r\n\r\n");
    capture.finish();
    (
        bound.start(history.clone()),
        history,
        replay,
        id,
        generation,
    )
}
fn confirm(preview: &serde_json::Value) -> String {
    serde_json::json!({"schema_version":1,"replay_id":preview["preview"]["replay_id"],"confirmation_token":preview["preview"]["confirmation_token"],"confirm_side_effects":true}).to_string()
}
#[tokio::test]
#[allow(clippy::too_many_lines)] // Ordered end-to-end protocol assertions share one fixture.
async fn authenticated_browser_replay_preserves_original_bytes_and_never_redirects_or_retries() {
    let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let redirect = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (service, _history, _replay, id, _) = fixture(backend.local_addr().unwrap()).await;
    let id = serde_json::to_value(id)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    let path = format!("/api/v1/requests/{id}/replay");
    let (status, availability) = call(&service, "GET", &path, "", None).await;
    assert_eq!(status, 200);
    assert_eq!(availability["availability"]["state"], "eligible");
    let (status, preview) = call(
        &service,
        "POST",
        &format!("{path}/prepare"),
        "{\"schema_version\":1}",
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(preview["method"], "POST");
    assert_eq!(preview["request_target"], "/action?token=[REDACTED]");
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), backend.accept())
            .await
            .is_err()
    );
    let ticket = confirm(&preview);
    let engine = tokio::spawn(async move {
        let (mut socket, _) = backend.accept().await.unwrap();
        let mut wire = Vec::new();
        loop {
            let mut chunk = [0; 1024];
            let n = socket.read(&mut chunk).await.unwrap();
            assert_ne!(n, 0);
            wire.extend_from_slice(&chunk[..n]);
            if wire.ends_with(b"\r\n\r\nBODY") {
                break;
            }
        }
        let text = String::from_utf8(wire).unwrap();
        assert!(text.contains("/action?token=QUERY_RAW_CANARY"));
        assert!(text.to_ascii_lowercase().contains("host: fixed.example"));
        assert!(text.contains("AUTH_RAW_CANARY"));
        assert!(text.contains("COOKIE_RAW_CANARY"));
        socket.write_all(format!("HTTP/1.1 302 Found\r\nLocation: http://{}/next\r\nContent-Length: 20\r\n\r\nRESPONSE_RAW_CANARY!",redirect.local_addr().unwrap()).as_bytes()).await.unwrap();
        drop(socket);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), backend.accept())
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), redirect.accept())
                .await
                .is_err()
        );
    });
    let (status, first) = call(&service, "POST", &format!("{path}/confirm"), &ticket, None).await;
    assert_eq!(status, 200);
    let (status, duplicate) =
        call(&service, "POST", &format!("{path}/confirm"), &ticket, None).await;
    assert_eq!(status, 200);
    assert_eq!(
        first["outcome"]["replay_id"],
        duplicate["outcome"]["replay_id"]
    );
    let outcome_path = format!(
        "/api/v1/replays/{}",
        preview["preview"]["replay_id"].as_str().unwrap()
    );
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let (status, out) = call(&service, "GET", &outcome_path, "", None).await;
            assert_eq!(status, 200);
            if out["outcome"]["state"] != "running" {
                break out;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(outcome["outcome"]["status"], 302, "{outcome}");
    assert_eq!(outcome["outcome"]["delivery"], "response_received");
    for safe in [
        preview,
        availability,
        outcome,
        call(&service, "GET", &format!("/api/v1/requests/{id}"), "", None)
            .await
            .1,
    ] {
        let text = safe.to_string();
        for secret in [
            "QUERY_RAW_CANARY",
            "AUTH_RAW_CANARY",
            "COOKIE_RAW_CANARY",
            "RESPONSE_RAW_CANARY",
            "BODY",
        ] {
            assert!(!text.contains(secret));
        }
    }
    engine.await.unwrap();
    service.shutdown().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Ordered end-to-end protocol assertions share one fixture.
async fn replay_mutations_require_post_strict_json_csrf_and_every_method_confirmation() {
    let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (service, history, _replay, id, generation) = fixture(backend.local_addr().unwrap()).await;
    let id_text = serde_json::to_value(id)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    let path = format!("/api/v1/requests/{id_text}/replay");
    for extra in ["", "Origin: http://evil.invalid\r\nSec-Fetch-Site: same-origin\r\nSec-Fetch-Mode: cors\r\nSec-Fetch-Dest: empty\r\n"] {
        assert_eq!(call(&service,"POST",&format!("{path}/prepare"),"{\"schema_version\":1}",Some(extra)).await.0,403);
    }
    for field in [
        "Sec-Fetch-Site: cross-site",
        "Sec-Fetch-Mode: navigate",
        "Sec-Fetch-Dest: document",
    ] {
        let base=format!("Origin: http://{}\r\nSec-Fetch-Site: same-origin\r\nSec-Fetch-Mode: cors\r\nSec-Fetch-Dest: empty\r\n",service.addr());
        let key = field.split(':').next().unwrap();
        let headers = base.lines().filter(|line| !line.starts_with(key)).fold(
            String::new(),
            |mut text, line| {
                text.push_str(line);
                text.push_str("\r\n");
                text
            },
        ) + field
            + "\r\n";
        assert_eq!(
            call(
                &service,
                "POST",
                &format!("{path}/prepare"),
                "{\"schema_version\":1}",
                Some(&headers)
            )
            .await
            .0,
            403
        );
    }
    for method in ["GET", "PUT", "DELETE"] {
        assert_eq!(
            call(&service, method, &format!("{path}/prepare"), "", None)
                .await
                .0,
            405
        );
    }
    for body in [
        "{}",
        "{\"schema_version\":1,\"schema_version\":1}",
        "{\"schema_version\":1,\"destination\":\"127.0.0.1:1\"}",
        "{\"schema_version\":2}",
        "[]",
    ] {
        assert_eq!(
            call(&service, "POST", &format!("{path}/prepare"), body, None)
                .await
                .0,
            400
        );
    }
    let (status, preview) = call(
        &service,
        "POST",
        &format!("{path}/prepare"),
        "{\"schema_version\":1}",
        None,
    )
    .await;
    assert_eq!(status, 200);
    let ticket = confirm(&preview);
    for body in [
        ticket.replace("\"confirm_side_effects\":true,", ""),
        ticket.replace(
            "\"confirm_side_effects\":true",
            "\"confirm_side_effects\":false",
        ),
        ticket.replacen('{', "{\"confirm_side_effects\":true,", 1),
        ticket.replacen('{', "{\"target\":\"127.0.0.1:1\",", 1),
        ticket.replacen('{', "{\"headers\":{},", 1),
        ticket.replacen('{', "{\"body\":\"override\",", 1),
        "x".repeat(1025),
    ] {
        assert_eq!(
            call(&service, "POST", &format!("{path}/confirm"), &body, None)
                .await
                .0,
            400
        );
    }
    // GET requests also require acknowledgement: there is no safe-method exemption.
    let mut get = history
        .admit_backend(generation, None, backend.local_addr().unwrap())
        .unwrap();
    get.request(b"GET / HTTP/1.1\r\nHost: fixed.example\r\n\r\n");
    get.response(b"HTTP/1.1 204 No Content\r\n\r\n");
    get.finish();
    let get_id = serde_json::to_value(get.id())
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    let get_path = format!("/api/v1/requests/{get_id}/replay");
    let preview = call(
        &service,
        "POST",
        &format!("{get_path}/prepare"),
        "{\"schema_version\":1}",
        None,
    )
    .await
    .1;
    assert_eq!(
        call(
            &service,
            "POST",
            &format!("{get_path}/confirm"),
            &confirm(&preview).replace(
                "\"confirm_side_effects\":true",
                "\"confirm_side_effects\":false"
            ),
            None
        )
        .await
        .0,
        400
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), backend.accept())
            .await
            .is_err()
    );
    service.shutdown().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Ordered end-to-end protocol assertions share one fixture.
async fn display_eviction_and_lifecycle_revoke_tickets_before_execution() {
    let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (service, history, replay, id, generation) = fixture(backend.local_addr().unwrap()).await;
    let id_text = serde_json::to_value(id)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    let path = format!("/api/v1/requests/{id_text}/replay");
    let mut preview = call(
        &service,
        "POST",
        &format!("{path}/prepare"),
        "{\"schema_version\":1}",
        None,
    )
    .await
    .1;
    history.disconnected();
    assert_eq!(
        call(
            &service,
            "POST",
            &format!("{path}/confirm"),
            &confirm(&preview),
            None
        )
        .await
        .0,
        400
    );
    history.activate(generation);
    assert_eq!(
        call(
            &service,
            "POST",
            &format!("{path}/confirm"),
            &confirm(&preview),
            None
        )
        .await
        .0,
        400
    );
    preview = call(
        &service,
        "POST",
        &format!("{path}/prepare"),
        "{\"schema_version\":1}",
        None,
    )
    .await
    .1;
    for _ in 0..200 {
        drop(history.admit(generation, None).unwrap());
    }
    assert!(history.get(id).is_none());
    assert_eq!(replay.raw_bytes(), 0);
    assert_eq!(call(&service, "GET", &path, "", None).await.0, 404);
    assert_eq!(
        call(
            &service,
            "POST",
            &format!("{path}/confirm"),
            &confirm(&preview),
            None
        )
        .await
        .0,
        404
    );
    let mut capture = history
        .admit_backend(generation, None, backend.local_addr().unwrap())
        .unwrap();
    capture.request(b"GET / HTTP/1.1\r\nHost: fixed.example\r\n\r\n");
    capture.response(b"HTTP/1.1 204 No Content\r\n\r\n");
    capture.finish();
    let id = serde_json::to_value(capture.id())
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    let path = format!("/api/v1/requests/{id}/replay");
    let preview = call(
        &service,
        "POST",
        &format!("{path}/prepare"),
        "{\"schema_version\":1}",
        None,
    )
    .await
    .1;
    history.activate(TunnelGeneration::new());
    assert_eq!(
        call(
            &service,
            "POST",
            &format!("{path}/confirm"),
            &confirm(&preview),
            None
        )
        .await
        .0,
        400
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), backend.accept())
            .await
            .is_err()
    );
    let (_, unknown) = call(
        &service,
        "GET",
        &format!("/api/v1/replays/{}", ulid::Ulid::new()),
        "",
        None,
    )
    .await;
    assert!(unknown["outcome"].is_null());
    assert!(!unknown.to_string().contains("not_sent"));
    service.shutdown().await;
    assert_eq!(replay.raw_bytes(), 0);
}

#[test]
fn replay_ui_has_explicit_review_confirmation_and_private_tickets_not_fake_controls() {
    let html = include_str!("../src/inspection/ui.html");
    let script = include_str!("../src/inspection/ui.js");
    for id in [
        "replay-warning",
        "replay-review",
        "replay-preview",
        "replay-ack",
        "replay-confirm",
        "replay-outcome",
    ] {
        assert!(html.contains(id), "missing {id}");
    }
    assert!(script.contains("confirm_side_effects: true"));
    assert!(script.contains("let pendingTicket = null"));
    assert!(script.contains("Delivery unknown"));
    for forbidden in [
        "innerHTML",
        "localStorage",
        "sessionStorage",
        "console.",
        "JSON.stringify(preview)",
        "JSON.stringify(pendingTicket)",
    ] {
        assert!(!script.contains(forbidden));
    }
}

#[tokio::test]
async fn preview_consent_is_not_raw_consent_and_unsafe_backends_never_become_replayable() {
    for retain in [false, true] {
        let bound = BoundInspector::bind(0).await.unwrap();
        let port = bound.addr().port();
        let replay = ReplayController::new(retain, port).unwrap();
        let history = Inspection::with_replay(PreviewMode::Json, replay.clone());
        let generation = TunnelGeneration::new();
        history.activate(generation);
        let backends = if retain {
            vec![
                None,
                Some(format!("127.0.0.1:{port}").parse().unwrap()),
                Some(format!("127.0.0.2:{port}").parse().unwrap()),
                Some("203.0.113.1:80".parse().unwrap()),
            ]
        } else {
            vec![Some("127.0.0.1:3000".parse().unwrap())]
        };
        let mut ids = Vec::new();
        for backend in backends {
            let mut capture = backend
                .map_or_else(
                    || history.admit(generation, None),
                    |addr| history.admit_backend(generation, None, addr),
                )
                .unwrap();
            capture.request(b"GET / HTTP/1.1\r\nHost: fixed.example\r\n\r\n");
            capture.response(b"HTTP/1.1 204 No Content\r\n\r\n");
            capture.finish();
            ids.push(capture.id());
        }
        let service = bound.start(history);
        for id in ids {
            let id = serde_json::to_value(id)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned();
            let path = format!("/api/v1/requests/{id}/replay");
            let (_, availability) = call(&service, "GET", &path, "", None).await;
            assert_ne!(availability["availability"]["state"], "eligible");
            assert_eq!(
                call(
                    &service,
                    "POST",
                    &format!("{path}/prepare"),
                    "{\"schema_version\":1}",
                    None
                )
                .await
                .0,
                if retain { 404 } else { 409 }
            );
        }
        assert_eq!(replay.raw_bytes(), 0);
        service.shutdown().await;
    }
}

fn origin_headers(service: &InspectorService) -> String {
    format!("Origin: http://{}\r\nSec-Fetch-Site: same-origin\r\nSec-Fetch-Mode: cors\r\nSec-Fetch-Dest: empty\r\n", service.addr())
}
fn capture_path(id: CaptureId) -> String {
    let id = serde_json::to_value(id).unwrap();
    format!("/api/v1/requests/{}/replay", id.as_str().unwrap())
}
async fn prepare(service: &InspectorService, path: &str) -> serde_json::Value {
    let (status, preview) = call(
        service,
        "POST",
        &format!("{path}/prepare"),
        "{\"schema_version\":1}",
        None,
    )
    .await;
    assert_eq!(status, 200);
    preview
}
async fn finish_backend_once(backend: &tokio::net::TcpListener) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        let (mut socket, _) = backend.accept().await.unwrap();
        let mut wire = Vec::new();
        while !wire.ends_with(b"\r\n\r\nBODY") {
            let mut chunk = [0; 1024];
            let n = socket.read(&mut chunk).await.unwrap();
            assert_ne!(n, 0);
            wire.extend_from_slice(&chunk[..n]);
        }
        socket
            .write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
            .await
            .unwrap();
    })
    .await
    .unwrap();
}
async fn saved_outcome(
    service: &InspectorService,
    preview: &serde_json::Value,
) -> serde_json::Value {
    let path = format!(
        "/api/v1/replays/{}",
        preview["preview"]["replay_id"].as_str().unwrap()
    );
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let (status, reply) = call(service, "GET", &path, "", None).await;
            assert_eq!(status, 200);
            if reply["outcome"]["state"] != "running" {
                return reply;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}
const FORBIDDEN_ENCODINGS: [&str; 7] = [
    "Content-Encoding: gzip\r\n",
    "Content-Encoding: identity\r\n",
    "Content-Encoding: br\r\n",
    "Content-Encoding: unknown\r\n",
    "Content-Encoding:\r\n",
    "Content-Encoding: gzip\r\nContent-Encoding: identity\r\n",
    "Content-Encoding: identity\r\ncontent-encoding: identity\r\n",
];

#[tokio::test]
async fn replay_prepare_rejects_every_content_encoding_before_json_or_ticket_creation() {
    let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (service, _history, _replay, id, _) = fixture(backend.local_addr().unwrap()).await;
    let path = capture_path(id);
    let preview = prepare(&service, &path).await;
    for encoding in FORBIDDEN_ENCODINGS {
        let headers = origin_headers(&service) + encoding;
        let (status, _) = call(
            &service,
            "POST",
            &format!("{path}/prepare"),
            "{\"schema_version\":1}",
            Some(&headers),
        )
        .await;
        assert_eq!(status, 400, "{encoding}");
    }
    // Rejections must not allocate tickets: seven more fit beside the first.
    for _ in 0..7 {
        prepare(&service, &path).await;
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), backend.accept())
            .await
            .is_err()
    );
    // The original unencoded ticket remains usable.
    assert_eq!(
        call(
            &service,
            "POST",
            &format!("{path}/confirm"),
            &confirm(&preview),
            None
        )
        .await
        .0,
        200
    );
    finish_backend_once(&backend).await;
    assert_eq!(
        saved_outcome(&service, &preview).await["outcome"]["state"],
        "complete"
    );
    service.shutdown().await;
}

#[tokio::test]
async fn replay_confirm_rejects_every_content_encoding_without_consuming_the_ticket() {
    let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (service, _history, _replay, id, _) = fixture(backend.local_addr().unwrap()).await;
    let path = capture_path(id);
    let preview = prepare(&service, &path).await;
    let ticket = confirm(&preview);
    for encoding in FORBIDDEN_ENCODINGS {
        let headers = origin_headers(&service) + encoding;
        let (status, _) = call(
            &service,
            "POST",
            &format!("{path}/confirm"),
            &ticket,
            Some(&headers),
        )
        .await;
        assert_eq!(status, 400, "{encoding}");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), backend.accept())
                .await
                .is_err()
        );
    }
    assert_eq!(
        call(&service, "POST", &format!("{path}/confirm"), &ticket, None)
            .await
            .0,
        200
    );
    finish_backend_once(&backend).await;
    assert_eq!(
        saved_outcome(&service, &preview).await["outcome"]["state"],
        "complete"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), backend.accept())
            .await
            .is_err()
    );
    service.shutdown().await;
}

#[tokio::test]
async fn consumed_confirmation_recovers_exact_saved_metadata_after_display_eviction_only() {
    let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (service, history, replay, id, generation) = fixture(backend.local_addr().unwrap()).await;
    let path = capture_path(id);
    let preview = prepare(&service, &path).await;
    let unconsumed = prepare(&service, &path).await;
    let ticket = confirm(&preview);
    assert_eq!(
        call(&service, "POST", &format!("{path}/confirm"), &ticket, None)
            .await
            .0,
        200
    );
    finish_backend_once(&backend).await;
    let saved = saved_outcome(&service, &preview).await;
    assert_eq!(saved["outcome"]["state"], "complete");
    for _ in 0..200 {
        drop(history.admit(generation, None).unwrap());
    }
    assert!(history.get(id).is_none());
    assert_eq!(replay.raw_bytes(), 0);
    let (status, duplicate) =
        call(&service, "POST", &format!("{path}/confirm"), &ticket, None).await;
    assert_eq!(status, 200);
    assert_eq!(duplicate, saved);
    assert_eq!(
        call(
            &service,
            "POST",
            &format!("{path}/confirm"),
            &confirm(&unconsumed),
            None
        )
        .await
        .0,
        404
    );
    let original: serde_json::Value = serde_json::from_str(&ticket).unwrap();
    let mut wrong_token = original.clone();
    let token = original["confirmation_token"].as_str().unwrap();
    wrong_token["confirmation_token"] = format!(
        "{}{}",
        &token[..63],
        if token.ends_with('0') { "1" } else { "0" }
    )
    .into();
    let mut wrong_replay = original.clone();
    wrong_replay["replay_id"] = ulid::Ulid::new().to_string().into();
    for wrong in [wrong_token, wrong_replay] {
        assert_eq!(
            call(
                &service,
                "POST",
                &format!("{path}/confirm"),
                &wrong.to_string(),
                None
            )
            .await
            .0,
            404
        );
    }
    let wrong_parent = format!("/api/v1/requests/{}/replay/confirm", ulid::Ulid::new());
    assert_eq!(
        call(&service, "POST", &wrong_parent, &ticket, None).await.0,
        404
    );
    let mut no_ack = original.clone();
    no_ack
        .as_object_mut()
        .unwrap()
        .remove("confirm_side_effects");
    let mut false_ack = original;
    false_ack["confirm_side_effects"] = false.into();
    for invalid in [no_ack, false_ack] {
        assert_eq!(
            call(
                &service,
                "POST",
                &format!("{path}/confirm"),
                &invalid.to_string(),
                None
            )
            .await
            .0,
            400
        );
    }
    assert_eq!(replay.raw_bytes(), 0);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), backend.accept())
            .await
            .is_err()
    );
    service.shutdown().await;
}
