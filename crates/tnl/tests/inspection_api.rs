use tnl::inspection::{Inspection, InspectorService, PreviewMode, TunnelGeneration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn request(service: &InspectorService, headers: &str, path: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(service.addr())
        .await
        .unwrap();
    stream
        .write_all(
            format!(
                "GET {path} HTTP/1.1\r\nHost: {}\r\n{headers}\r\n",
                service.addr()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    stream.read_to_end(&mut out).await.unwrap();
    String::from_utf8(out).unwrap()
}
#[tokio::test]
async fn real_loopback_service_authenticates_redacted_history_and_shuts_down() {
    let history = Inspection::new(PreviewMode::Json);
    let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
    capture.request(b"GET /?token=SECRET_CANARY HTTP/1.1\r\nHost: localhost\r\nAuthorization: HEADER_CANARY\r\n\r\n");
    capture.response(b"HTTP/1.1 204 No Content\r\n\r\n");
    capture.finish();
    let service = InspectorService::start(history.clone(), 0).await.unwrap();
    assert!(service.addr().ip().is_loopback());
    let url = service.bootstrap_url();
    let token = url.split("#token=").nth(1).unwrap();
    assert_eq!(token.len(), 64);
    let missing = request(&service, "", "/api/v1/requests").await;
    assert!(missing.starts_with("HTTP/1.1 401"));
    let auth = format!("Authorization: Bearer {token}\r\n");
    let response = request(&service, &auth, "/api/v1/requests").await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(!response.contains("CANARY"));
    assert!(response.contains("[REDACTED]"));
    let body = response.split("\r\n\r\n").nth(1).unwrap();
    let list: serde_json::Value = serde_json::from_str(body).unwrap();
    let id = list["items"][0]["capture_id"].as_str().unwrap();
    let detail = request(&service, &auth, &format!("/api/v1/requests/{id}")).await;
    assert!(detail.starts_with("HTTP/1.1 200"));
    assert!(!detail.contains("CANARY"));
    let foreign = request(
        &service,
        &format!("{auth}Origin: http://evil.invalid\r\n"),
        "/api/v1/requests",
    )
    .await;
    assert!(foreign.starts_with("HTTP/1.1 403"));
    let addr = service.addr();
    service.shutdown().await;
    assert!(history.search("").is_empty());
    assert!(tokio::net::TcpStream::connect(addr).await.is_err());
}

async fn raw(service: &InspectorService, head: &str) -> String {
    let mut socket = tokio::net::TcpStream::connect(service.addr())
        .await
        .unwrap();
    socket.write_all(head.as_bytes()).await.unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(4),
        socket.read_to_end(&mut bytes),
    )
    .await
    .unwrap()
    .ok();
    String::from_utf8(bytes).unwrap()
}
#[tokio::test]
async fn authority_auth_browser_context_and_query_validation_are_fail_closed() {
    let service = InspectorService::start(Inspection::new(PreviewMode::Off), 0)
        .await
        .unwrap();
    let url = service.bootstrap_url();
    let token = url.split("#token=").nth(1).unwrap();
    let auth = format!("Authorization: Bearer {token}\r\n");
    let host = service.addr();
    for headers in [
        format!("Host: localhost:{}\r\n", host.port()),
        format!("Host: {host}\r\nHost: {host}\r\n"),
        format!("Host: {host}\r\nOrigin: null\r\n"),
        format!("Host: {host}\r\nOrigin: http://{host}\r\nOrigin: http://{host}\r\n"),
        format!("Host: {host}\r\nSec-Fetch-Site: same-site\r\n"),
        format!("Host: {host}\r\nSec-Fetch-Site: cross-site\r\n"),
    ] {
        let response = raw(
            &service,
            &format!("GET /api/v1/stats HTTP/1.1\r\n{headers}{auth}\r\n"),
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
    }
    for headers in [
        String::new(),
        "Authorization: Bearer bad\r\n".to_string(),
        format!(
            "Authorization: Bearer {}{}\r\n",
            &token[..63],
            if token.ends_with('0') { "1" } else { "0" }
        ),
        format!("{auth}{auth}"),
    ] {
        assert!(request(&service, &headers, "/api/v1/requests/nonexistent")
            .await
            .starts_with("HTTP/1.1 401"));
    }
    for query in [
        "q=%ff",
        "q=%GG",
        "q=a&q=b",
        "token=secret",
        "limit=101",
        "limit=0",
        "unknown=value",
        "before=18446744073709551616",
    ] {
        assert!(
            request(&service, &auth, &format!("/api/v1/requests?{query}"))
                .await
                .starts_with("HTTP/1.1 400"),
            "{query}"
        );
    }
    let post = raw(
        &service,
        &format!("POST /api/v1/requests/x/replays HTTP/1.1\r\nHost: {host}\r\n{auth}\r\n"),
    )
    .await;
    assert!(post.starts_with("HTTP/1.1 403"), "{post}");
    let post=raw(&service,&format!("POST /api/v1/requests/x/replays HTTP/1.1\r\nHost: {host}\r\n{auth}Origin: http://{host}\r\nSec-Fetch-Site: same-origin\r\nSec-Fetch-Dest: empty\r\nSec-Fetch-Mode: cors\r\n\r\n")).await;
    assert!(post.starts_with("HTTP/1.1 405"));
    let root = request(&service, "", "/").await;
    for header in [
        "Cache-Control: no-store",
        "X-Content-Type-Options: nosniff",
        "X-Frame-Options: DENY",
        "frame-ancestors 'none'",
        "script-src 'self'",
    ] {
        assert!(root.contains(header));
    }
    assert!(!root.contains(token));
    assert!(!root.contains("Access-Control-Allow"));
    let script = request(&service, "", "/inspector.js").await;
    assert!(script.contains("history.replaceState"));
    assert!(script.contains("textContent"));
    for forbidden in [
        "innerHTML",
        "localStorage",
        "sessionStorage",
        "eval(",
        "console.",
    ] {
        assert!(!script.contains(forbidden));
    }
    assert!(!format!("{service:?}").contains(token));
    service.shutdown().await;
}

#[tokio::test]
async fn bounded_summaries_paginate_without_exporting_full_records() {
    let history = Inspection::new(PreviewMode::Off);
    for n in 0..200 {
        let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
        capture.request(format!("GET /{}?token=HIDDEN_CANARY HTTP/1.1\r\nHost: localhost\r\nCookie: COOKIE_CANARY\r\n\r\n", "é".repeat(990)+&n.to_string()).as_bytes());
        capture.response(b"HTTP/1.1 204 No Content\r\n\r\n");
        capture.finish();
    }
    let service = InspectorService::start(history, 0).await.unwrap();
    let url = service.bootstrap_url();
    let token = url.split("#token=").nth(1).unwrap();
    let auth = format!("Authorization: Bearer {token}\r\n");
    let first = request(&service, &auth, "/api/v1/requests?limit=100").await;
    let body = first.split("\r\n\r\n").nth(1).unwrap();
    assert!(body.len() <= 65536);
    assert!(!body.contains("CANARY"));
    let value: serde_json::Value = serde_json::from_str(body).unwrap();
    let items = value["items"].as_array().unwrap();
    assert!(!items.is_empty() && items.len() <= 100);
    assert!(items[0].get("request").is_none());
    let cursor = value["next_before"].as_u64().unwrap();
    let next = request(
        &service,
        &auth,
        &format!("/api/v1/requests?limit=100&before={cursor}"),
    )
    .await;
    let next: serde_json::Value =
        serde_json::from_str(next.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert!(next["items"][0]["sequence"].as_u64().unwrap() < cursor);
    assert!(request(&service, &auth, "/api/v1/requests?state=complete")
        .await
        .starts_with("HTTP/1.1 200"));
    assert!(request(&service, &auth, "/api/v1/requests?q=COOKIE_CANARY")
        .await
        .contains("\"items\":[]"));
    service.shutdown().await;
}

#[tokio::test]
async fn bind_failure_random_capability_and_no_request_body_fallback() {
    use tnl::inspection::BoundInspector;
    let bound = BoundInspector::bind(0).await.unwrap();
    assert_ne!(bound.addr().port(), 0);
    assert!(BoundInspector::bind(bound.addr().port()).await.is_err());
    let service = bound.start(Inspection::new(PreviewMode::Off));
    let other = InspectorService::start(Inspection::new(PreviewMode::Off), 0)
        .await
        .unwrap();
    assert_ne!(
        service.bootstrap_url().split("#token=").nth(1),
        other.bootstrap_url().split("#token=").nth(1)
    );
    for header in [
        "Content-Length: 1\r\n",
        "Transfer-Encoding: chunked\r\n",
        "Upgrade: websocket\r\n",
    ] {
        let response = request(&service, header, "/").await;
        assert!(response.starts_with("HTTP/1.1 400"));
    }
    let prefix = "GET / HTTP/1.1\r\nHost: ".to_string() + &service.addr().to_string() + "\r\nX: ";
    for size in [32768, 32769] {
        let head = format!("{prefix}{}\r\n\r\n", "x".repeat(size - prefix.len() - 4));
        let response = raw(&service, &head).await;
        if size == 32768 {
            assert!(response.starts_with("HTTP/1.1 200"));
        } else {
            assert!(response.is_empty());
        }
    }
    let mut slow = tokio::net::TcpStream::connect(service.addr())
        .await
        .unwrap();
    slow.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(4),
        slow.read_to_end(&mut bytes),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(bytes.is_empty());
    service.shutdown().await;
    other.shutdown().await;
}

#[tokio::test]
async fn real_service_sheds_excess_slow_header_clients() {
    let history = Inspection::new(PreviewMode::Off);
    let service = InspectorService::start(history.clone(), 0).await.unwrap();
    let mut clients = Vec::new();
    for _ in 0..4 {
        let mut socket = tokio::net::TcpStream::connect(service.addr())
            .await
            .unwrap();
        socket.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
        clients.push(socket);
        tokio::task::yield_now().await;
    }
    let response = raw(
        &service,
        &format!("GET / HTTP/1.1\r\nHost: {}\r\n\r\n", service.addr()),
    )
    .await;
    assert!(response.is_empty());
    let capture = history.admit(TunnelGeneration::new(), None).unwrap();
    drop(capture);
    tokio::time::timeout(std::time::Duration::from_secs(1), service.shutdown())
        .await
        .unwrap();
    drop(clients);
}

const USER_NAVIGATION: &str = "Sec-Fetch-Site: cross-site\r\nSec-Fetch-Mode: navigate\r\nSec-Fetch-Dest: document\r\nSec-Fetch-User: ?1\r\n";

#[tokio::test]
async fn cross_site_user_navigation_bootstraps_only_the_nonsensitive_root_document() {
    let history = Inspection::new(PreviewMode::Off);
    let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
    capture.request(b"GET /PRIVATE_PATH_CANARY HTTP/1.1\r\nHost: localhost\r\n\r\n");
    capture.response(b"HTTP/1.1 204 No Content\r\n\r\n");
    capture.finish();
    let service = InspectorService::start(history, 0).await.unwrap();
    let url = service.bootstrap_url();
    let token = url.split("#token=").nth(1).unwrap();
    // Exact headers observed by the parent in real Chrome, with no Origin.
    for origin in [
        String::new(),
        format!("Origin: http://{}\r\n", service.addr()),
    ] {
        let response = request(&service, &format!("{USER_NAVIGATION}{origin}"), "/").await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("Content-Type: text/html"));
        assert!(response.contains("src=\"/inspector.js\""));
        assert!(!response.contains(token));
        assert!(!response.contains("PRIVATE_PATH_CANARY"));
        assert!(!response.contains("Access-Control-Allow"));
    }
    service.shutdown().await;
}

#[tokio::test]
async fn cross_site_bootstrap_exception_requires_unique_exact_user_navigation_headers() {
    let service = InspectorService::start(Inspection::new(PreviewMode::Off), 0)
        .await
        .unwrap();
    for (name, value) in [
        ("Sec-Fetch-Mode", "navigate"),
        ("Sec-Fetch-Dest", "document"),
        ("Sec-Fetch-User", "?1"),
    ] {
        let field = format!("{name}: {value}\r\n");
        for headers in [
            USER_NAVIGATION.replace(&field, ""),
            USER_NAVIGATION.replace(&field, &format!("{name}: wrong\r\n")),
            format!("{USER_NAVIGATION}{field}"),
            format!(
                "{USER_NAVIGATION}{}: {value}\r\n",
                name.to_ascii_lowercase()
            ),
        ] {
            let response = request(&service, &headers, "/").await;
            assert!(
                response.starts_with("HTTP/1.1 403"),
                "field {name}: {response}"
            );
        }
    }
    for (field, replacement) in [
        ("Sec-Fetch-Mode: navigate", "Sec-Fetch-Mode: cors"),
        ("Sec-Fetch-Dest: document", "Sec-Fetch-Dest: iframe"),
        ("Sec-Fetch-Dest: document", "Sec-Fetch-Dest: script"),
        ("Sec-Fetch-User: ?1", "Sec-Fetch-User: ?0"),
        ("Sec-Fetch-Site: cross-site", "Sec-Fetch-Site: same-site"),
    ] {
        assert!(
            request(&service, &USER_NAVIGATION.replace(field, replacement), "/")
                .await
                .starts_with("HTTP/1.1 403")
        );
    }
    assert!(request(
        &service,
        &format!("{USER_NAVIGATION}Sec-Fetch-Site: cross-site\r\n"),
        "/"
    )
    .await
    .starts_with("HTTP/1.1 403"));
    service.shutdown().await;
}

#[tokio::test]
async fn cross_site_navigation_never_relaxes_api_script_authority_origin_or_framing() {
    let service = InspectorService::start(Inspection::new(PreviewMode::Off), 0)
        .await
        .unwrap();
    let url = service.bootstrap_url();
    let token = url.split("#token=").nth(1).unwrap();
    let headers = format!("{USER_NAVIGATION}Authorization: Bearer {token}\r\n");
    for target in [
        "/inspector.js",
        "/api/v1/stats",
        "/api/v1/requests",
        "/api/v1/requests/unknown",
        "/?",
        "/?token=ignored",
        "//",
        "/index.html",
    ] {
        assert!(
            request(&service, &headers, target)
                .await
                .starts_with("HTTP/1.1 403"),
            "target {target}"
        );
    }
    for method in ["POST", "HEAD", "OPTIONS"] {
        let response = raw(
            &service,
            &format!(
                "{method} / HTTP/1.1\r\nHost: {}\r\n{headers}\r\n",
                service.addr()
            ),
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 403"), "method {method}");
    }
    for origin in [
        "Origin: http://foreign.invalid\r\n".to_owned(),
        "Origin: null\r\n".to_owned(),
        format!(
            "Origin: http://{0}\r\nOrigin: http://{0}\r\n",
            service.addr()
        ),
    ] {
        assert!(request(&service, &format!("{headers}{origin}"), "/")
            .await
            .starts_with("HTTP/1.1 403"));
    }
    for host in [
        format!("Host: localhost:{}\r\n", service.addr().port()),
        "Host: 127.0.0.1:0\r\n".to_owned(),
        format!("Host: {0}\r\nHost: {0}\r\n", service.addr()),
    ] {
        assert!(
            raw(&service, &format!("GET / HTTP/1.1\r\n{host}{headers}\r\n"))
                .await
                .starts_with("HTTP/1.1 403")
        );
    }
    for framing in [
        "Content-Length: 1\r\n",
        "Content-Length: 0\r\nContent-Length: 0\r\n",
        "Transfer-Encoding: chunked\r\n",
        "Upgrade: websocket\r\n",
    ] {
        assert!(
            request(&service, &format!("{headers}{framing}"), "/")
                .await
                .starts_with("HTTP/1.1 400"),
            "framing {framing}"
        );
    }
    // Same-origin authenticated API requests remain available independently.
    assert!(request(
        &service,
        &format!("Authorization: Bearer {token}\r\nSec-Fetch-Site: same-origin\r\n"),
        "/api/v1/stats"
    )
    .await
    .starts_with("HTTP/1.1 200"));
    service.shutdown().await;
}
