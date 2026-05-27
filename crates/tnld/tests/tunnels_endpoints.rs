use reqwest::StatusCode;
use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config as TnldConfig;
use tnld::serve::spawn_server;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tunnels_endpoints_basic_shape() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let tf = TokensFile {
        tokens: vec![TokenEntry {
            name: "alice".into(),
            hash: hash_plaintext("tnl_A").unwrap(),
        }],
    };
    std::fs::write(tmp.path(), toml::to_string(&tf).unwrap()).unwrap();
    let cfg = TnldConfig {
        listen: "127.0.0.1:0".into(),
        public_url: "http://x".into(),
        hostname_root: "t.x".into(),
        tokens_file: tmp.path().to_string_lossy().into_owned(),
        session_grace_sec: 30,
    };
    let h = spawn_server(cfg).await.unwrap();
    let addr = h.local_addr.to_string();

    // No tunnels yet — empty list.
    let r = reqwest::Client::new()
        .get(format!("http://{addr}/tunnels"))
        .bearer_auth("tnl_A")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v.as_array().unwrap().len(), 0);

    // Delete unknown → 404.
    let r = reqwest::Client::new()
        .delete(format!("http://{addr}/tunnels/ghost"))
        .bearer_auth("tnl_A")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);

    // Missing bearer → 401.
    let r = reqwest::Client::new()
        .get(format!("http://{addr}/tunnels"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}
