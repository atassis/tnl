use std::{
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tnl_protocol::{
    access::{BasicVerifier, SecretVerifier, VisitorAccessPolicyV1, VisitorRateLimit},
    request_metadata::DataStreamProtocol,
    Session, Stream,
};
use tnld::{
    access::CompiledAccess,
    serve::{spawn_server_with_state, SessionHandle},
};

#[derive(Debug)]
struct CountedSession(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Session for CountedSession {
    fn termination(&self) -> tnl_protocol::transport::SessionTermination {
        panic!("open-only data-plane fixture does not run a physical session owner")
    }
    async fn open_stream(&mut self) -> anyhow::Result<Pin<Box<dyn Stream>>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("counted open failure")
    }
    async fn accept_stream(&mut self) -> anyhow::Result<Option<Pin<Box<dyn Stream>>>> {
        Ok(None)
    }
    async fn ping(&mut self) -> anyhow::Result<Duration> {
        Ok(Duration::ZERO)
    }
    async fn close(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn every_access_denial_is_before_stream_open_and_open_failure_releases_slots() {
    let tokens = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tokens.path(), "tokens = []").unwrap();
    let (server, state) = spawn_server_with_state(tnld::config::Config {
        listen: "127.0.0.1:0".into(),
        public_url: "http://test".into(),
        hostname_root: "t.x".into(),
        tokens_file: tokens.path().to_str().unwrap().into(),
        trusted_proxies: vec![],
        session_grace_sec: 30,
    })
    .await
    .unwrap();
    let session = state.registry.register_session("owner");
    let opens = Arc::new(AtomicUsize::new(0));
    state.session_handles.insert(
        session.id.clone(),
        SessionHandle::new(
            &session,
            Arc::new(tokio::sync::Mutex::new(Box::new(CountedSession(
                opens.clone(),
            )))),
            DataStreamProtocol::Http1MetadataV1,
        ),
    );
    let basic = BasicVerifier::new(
        "user".into(),
        SecretVerifier::new(tnld::auth::hash_plaintext("visitor").unwrap()).unwrap(),
    )
    .unwrap();
    let policies = [
        (
            "basic",
            VisitorAccessPolicyV1::new(Some(basic), &[], None, None).unwrap(),
            401,
        ),
        (
            "ip",
            VisitorAccessPolicyV1::new(None, &["198.51.100.0/24".into()], None, None).unwrap(),
            403,
        ),
        (
            "rate",
            VisitorAccessPolicyV1::new(
                None,
                &[],
                Some(VisitorRateLimit {
                    requests_per_second: 1,
                    burst: 1,
                }),
                None,
            )
            .unwrap(),
            429,
        ),
        (
            "concurrent",
            VisitorAccessPolicyV1::new(None, &[], None, Some(1)).unwrap(),
            429,
        ),
    ];
    let mut held = Vec::new();
    for (name, policy, status) in policies {
        let access = Arc::new(CompiledAccess::new(Arc::new(policy)).unwrap());
        if matches!(name, "rate" | "concurrent") {
            held.push(
                access
                    .admit(None, &mut http::HeaderMap::new())
                    .await
                    .unwrap(),
            );
        }
        state
            .registry
            .create_tunnel_bound(
                name,
                &session.id,
                "owner",
                DataStreamProtocol::Http1MetadataV1,
                Some(access),
            )
            .unwrap();
        let response = reqwest::Client::new()
            .post(format!(
                "http://{}/denied?QUERY_SENTINEL",
                server.local_addr
            ))
            .header("Host", format!("{name}.t.x"))
            .header("Authorization", "Bearer APP_SENTINEL")
            .header("X-Forwarded-For", "198.51.100.1")
            .body("BODY_SENTINEL")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status);
        assert_eq!(response.headers()["x-tnl-component"], "access");
        if status == 429 {
            assert_eq!(response.headers()["retry-after"], "1");
        }
        assert!(!response.text().await.unwrap().contains("SENTINEL"));
        assert_eq!(opens.load(Ordering::SeqCst), 0);
    }
    drop(held);
    for _ in 0..2 {
        let response = reqwest::Client::new()
            .get(format!("http://{}/open", server.local_addr))
            .header("Host", "concurrent.t.x")
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            502,
            "failed open must release the concurrency slot"
        );
    }
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    server.join.abort();
}

#[tokio::test]
async fn trusted_proxy_invalid_or_missing_forwarding_is_unavailable_not_proxy_allowlist_bypass() {
    let tokens = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tokens.path(), "tokens = []").unwrap();
    let (server, state) = spawn_server_with_state(tnld::config::Config {
        listen: "127.0.0.1:0".into(),
        public_url: "http://test".into(),
        hostname_root: "t.x".into(),
        tokens_file: tokens.path().to_str().unwrap().into(),
        trusted_proxies: vec!["127.0.0.0/8".into()],
        session_grace_sec: 30,
    })
    .await
    .unwrap();
    let session = state.registry.register_session("owner");
    let opens = Arc::new(AtomicUsize::new(0));
    state.session_handles.insert(
        session.id.clone(),
        SessionHandle::new(
            &session,
            Arc::new(tokio::sync::Mutex::new(Box::new(CountedSession(
                opens.clone(),
            )))),
            DataStreamProtocol::Http1MetadataV1,
        ),
    );
    let access = Arc::new(
        CompiledAccess::new(Arc::new(
            VisitorAccessPolicyV1::new(None, &["127.0.0.0/8".into()], None, None).unwrap(),
        ))
        .unwrap(),
    );
    state
        .registry
        .create_tunnel_bound(
            "guarded",
            &session.id,
            "owner",
            DataStreamProtocol::Http1MetadataV1,
            Some(access),
        )
        .unwrap();
    for forwarded in [None, Some("invalid"), Some("127.0.0.2")] {
        let mut request = reqwest::Client::new()
            .get(format!("http://{}/", server.local_addr))
            .header("Host", "guarded.t.x");
        if let Some(value) = forwarded {
            request = request.header("x-forwarded-for", value);
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), 503);
        assert_eq!(response.headers()["retry-after"], "1");
        assert_eq!(opens.load(Ordering::SeqCst), 0);
    }
    server.join.abort();
}
