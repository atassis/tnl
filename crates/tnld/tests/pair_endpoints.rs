use reqwest::StatusCode;
use serde_json::json;
use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config as TnldConfig;
use tnld::serve::spawn_server;

async fn spawn() -> (String, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let tf = TokensFile {
        tokens: vec![TokenEntry {
            name: "admin".into(),
            hash: hash_plaintext("tnl_ADMIN").unwrap(),
        }],
    };
    std::fs::write(tmp.path(), toml::to_string(&tf).unwrap()).unwrap();
    let cfg = TnldConfig {
        listen: "127.0.0.1:0".into(),
        public_url: "http://test.example".into(),
        hostname_root: "t.example.com".into(),
        tokens_file: tmp.path().to_string_lossy().into_owned(),
    };
    let h = spawn_server(cfg).await.unwrap();
    (h.local_addr.to_string(), tmp)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_pair_returns_code_and_invite_url() {
    let (addr, _t) = spawn().await;
    let c = reqwest::Client::new();
    let r = c
        .post(format!("http://{addr}/pair"))
        .bearer_auth("tnl_ADMIN")
        .json(&json!({"name": "laptop", "expires_in_sec": 60}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v: serde_json::Value = r.json().await.unwrap();
    assert!(v["code"].as_str().unwrap().contains('-'));
    assert!(v["invite_url"]
        .as_str()
        .unwrap()
        .starts_with("http://test.example/invite/"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pair_requires_bearer() {
    let (addr, _t) = spawn().await;
    let r = reqwest::Client::new()
        .post(format!("http://{addr}/pair"))
        .json(&json!({"name": "x", "expires_in_sec": 60}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redeem_returns_token_and_endpoint() {
    let (addr, _t) = spawn().await;
    let c = reqwest::Client::new();
    let r = c
        .post(format!("http://{addr}/pair"))
        .bearer_auth("tnl_ADMIN")
        .json(&json!({"name": "laptop", "expires_in_sec": 60}))
        .send()
        .await
        .unwrap();
    let code = r.json::<serde_json::Value>().await.unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();
    let r = c
        .post(format!("http://{addr}/pair/redeem"))
        .json(&json!({"code": code}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["name"], "laptop");
    assert_eq!(v["endpoint"], "http://test.example");
    assert!(v["token"].as_str().unwrap().starts_with("tnl_"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redeem_unknown_404() {
    let (addr, _t) = spawn().await;
    let r = reqwest::Client::new()
        .post(format!("http://{addr}/pair/redeem"))
        .json(&json!({"code": "ZZ-99-AA"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        r.json::<serde_json::Value>().await.unwrap()["error"],
        "pair_not_found"
    );
}
