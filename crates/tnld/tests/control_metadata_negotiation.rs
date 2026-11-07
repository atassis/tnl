use tnl_protocol::request_metadata::{DATA_STREAM_PROTOCOL_HEADER, HTTP1_METADATA_V1};
use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_metadata_negotiation_requires_one_matching_authenticated_offer() {
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
    let handle = tnld::serve::spawn_server(Config {
        listen: "127.0.0.1:0".into(),
        public_url: "http://x".into(),
        hostname_root: "t.x".into(),
        tokens_file: file.path().to_string_lossy().into_owned(),
        session_grace_sec: 30,
        trusted_proxies: vec![],
    })
    .await
    .unwrap();
    for (offers, echo) in [
        (vec![HTTP1_METADATA_V1], true),
        (vec![], false),
        (vec!["future"], false),
        (vec![HTTP1_METADATA_V1, HTTP1_METADATA_V1], false),
        (vec![""], false),
        (vec!["http1-metadata-v1, future"], false),
    ] {
        let mut req = http::Request::builder()
            .uri(format!("ws://{}/control", handle.local_addr))
            .header("Host", handle.local_addr.to_string())
            .header("Authorization", "Bearer tnl_TEST")
            .header("Upgrade", "websocket")
            .header("Connection", "Upgrade")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                async_tungstenite::tungstenite::handshake::client::generate_key(),
            );
        for offer in offers {
            req = req.header(DATA_STREAM_PROTOCOL_HEADER, offer);
        }
        let (_ws, response) = async_tungstenite::tokio::connect_async(req.body(()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), http::StatusCode::SWITCHING_PROTOCOLS);
        assert_eq!(
            response
                .headers()
                .get(DATA_STREAM_PROTOCOL_HEADER)
                .map(|v| v.to_str().unwrap()),
            echo.then_some(HTTP1_METADATA_V1)
        );
    }
    let response = reqwest::Client::new()
        .get(format!("http://{}/control", handle.local_addr))
        .header(DATA_STREAM_PROTOCOL_HEADER, HTTP1_METADATA_V1)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), http::StatusCode::UNAUTHORIZED);
    assert!(!response.headers().contains_key(DATA_STREAM_PROTOCOL_HEADER));
    handle.join.abort();
}
