//! Integration test: registered tunnel whose session handle is not yet in
//! `session_handles` → 503 with X-Tnl-Component: daemon and Retry-After: 1.
//!
//! This exercises the second 503 branch in `data_plane::handler`:
//!   `state.session_handles.get(&session_id)` returns None.

use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config;
use tnld::serve::spawn_server_with_state;

fn make_cfg(tokens_file: String) -> Config {
    Config {
        listen: "127.0.0.1:0".into(),
        public_url: "http://test".into(),
        hostname_root: "t.example.com".into(),
        tokens_file,
        // Long grace window so GC never fires during this test.
        session_grace_sec: 300,
    }
}

fn make_tokens_file() -> tempfile::NamedTempFile {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let tf = TokensFile {
        tokens: vec![TokenEntry {
            name: "smoke".into(),
            hash: hash_plaintext("tnl_TEST").unwrap(),
        }],
    };
    std::fs::write(tmp.path(), toml::to_string(&tf).unwrap()).unwrap();
    tmp
}

#[tokio::test(flavor = "multi_thread")]
async fn registered_tunnel_no_session_handle_returns_503_daemon() {
    let tmp = make_tokens_file();
    let (handle, state) =
        spawn_server_with_state(make_cfg(tmp.path().to_string_lossy().into_owned()))
            .await
            .unwrap();
    let addr = handle.local_addr.to_string();

    // Register a session in the registry and create a tunnel, but deliberately
    // do NOT insert anything into `state.session_handles`.  The data-plane
    // handler will find the tunnel and a valid session_id, then fail to find a
    // session handle → 503 with component "daemon".
    let sess = state.registry.register_session("smoke");
    let _tunnel = state
        .registry
        .create_tunnel("orphan", &sess.id, "smoke")
        .unwrap();
    // session_handles intentionally left empty.

    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/"))
        .header("Host", "orphan.t.example.com")
        .header("Accept", "application/json")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 503);
    assert_eq!(
        resp.headers()
            .get("x-tnl-component")
            .and_then(|v| v.to_str().ok()),
        Some("daemon"),
    );
    assert_eq!(
        resp.headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok()),
        Some("1"),
    );

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "tunnel_disconnected");
}
