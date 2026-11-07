#![cfg(unix)]
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)] // Real binary -> daemon -> backend -> authenticated detail.
async fn real_cli_tunnel_body_policy_defaults_off_and_json_requires_explicit_opt_in() {
    let tokens = tempfile::NamedTempFile::new().unwrap();
    let tf = tnld::auth::TokensFile {
        tokens: vec![tnld::auth::TokenEntry {
            name: "fixture".into(),
            hash: tnld::auth::hash_plaintext("tnl_U").unwrap(),
        }],
    };
    std::fs::write(tokens.path(), toml::to_string(&tf).unwrap()).unwrap();
    let daemon = tnld::serve::spawn_server(tnld::config::Config {
        listen: "127.0.0.1:0".into(),
        public_url: "http://x".into(),
        hostname_root: "t.x".into(),
        tokens_file: tokens.path().to_string_lossy().into_owned(),
        trusted_proxies: vec![],
        session_grace_sec: 30,
    })
    .await
    .unwrap();
    let cfg = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        cfg.path(),
        format!(
            "endpoint = \"http://{}\"\ntoken = \"tnl_U\"\n",
            daemon.local_addr
        ),
    )
    .unwrap();
    let client = reqwest::Client::new();
    for json in [false, true] {
        let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = backend.local_addr().unwrap().to_string();
        let name = if json { "jsondemo" } else { "offdemo" };
        let mut args = vec!["http", &target, name, "--inspect", "--quiet", "--json"];
        if json {
            args.extend(["--inspect-bodies", "json"]);
        }
        let mut child = Child(
            Command::new(env!("CARGO_BIN_EXE_tnl"))
                .args(args)
                .env("TNL_CONFIG", cfg.path())
                .env_remove("TNL_ENDPOINT")
                .env_remove("TNL_TOKEN")
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut stderr = BufReader::new(child.0.stderr.take().unwrap());
        let mut line = String::new();
        stderr.read_line(&mut line).unwrap();
        let mut url = url::Url::parse(
            line.split_whitespace()
                .find(|s| s.starts_with("http://"))
                .unwrap(),
        )
        .unwrap();
        let token = url
            .fragment()
            .unwrap()
            .strip_prefix("token=")
            .unwrap()
            .to_owned();
        url.set_fragment(None);
        let base = url.to_string();
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                let value: serde_json::Value = client
                    .get(format!("{base}api/v1/stats"))
                    .bearer_auth(&token)
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                if value["tunnel"]["connected"] == true {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let body = br#"{"password":"REQUEST_BODY_CANARY","ok":true}"#;
        let response_body = br#"{"token":"RESPONSE_BODY_CANARY","ok":true}"#;
        let server = tokio::spawn(async move {
            let (mut socket, _) = backend.accept().await.unwrap();
            let mut wire = Vec::new();
            loop {
                let mut buf = [0; 1024];
                let n = socket.read(&mut buf).await.unwrap();
                assert_ne!(n, 0);
                wire.extend_from_slice(&buf[..n]);
                if let Some(end) = wire.windows(4).position(|w| w == b"\r\n\r\n") {
                    if wire.len() >= end + 4 + body.len() {
                        break;
                    }
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                response_body.len()
            )
            .into_bytes();
            let response = [response, response_body.to_vec()].concat();
            socket.write_all(&response).await.unwrap();
            (wire.len(), response.len())
        });
        let response = client
            .post(format!("http://{}/?token=QUERY_CANARY", daemon.local_addr))
            .header("Host", format!("{name}.t.x"))
            .header("Content-Type", "application/json")
            .header("Authorization", "HEADER_CANARY")
            .body(body.as_slice())
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        response.bytes().await.unwrap();
        let counts = server.await.unwrap();
        let detail = tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                let list: serde_json::Value = client
                    .get(format!("{base}api/v1/requests"))
                    .bearer_auth(&token)
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                if let Some(id) = list["items"][0]["capture_id"].as_str() {
                    let detail: serde_json::Value = client
                        .get(format!("{base}api/v1/requests/{id}"))
                        .bearer_auth(&token)
                        .send()
                        .await
                        .unwrap()
                        .json()
                        .await
                        .unwrap();
                    if detail["state"] != "active" {
                        break detail;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(detail["state"], "complete");
        assert_eq!(detail["response_source"], "backend");
        assert_eq!(detail["bytes_to_backend"], counts.0);
        assert_eq!(detail["bytes_from_backend"], counts.1);
        assert!(detail["tunnel_id"].is_string());
        assert!(detail["request_id"].is_string());
        assert_eq!(detail["visitor"]["display_ip"], "[REDACTED]");
        assert_eq!(detail["replay"]["eligible"], false);
        for message in ["request", "response"] {
            assert_eq!(
                detail[message]["body"]["mode"],
                if json { "json" } else { "off" }
            );
            if json {
                assert_eq!(detail[message]["body"]["state"], "available");
                assert_eq!(detail[message]["body"]["redacted"], true);
            } else {
                assert!(detail[message]["body"]["text"].is_null());
                assert_eq!(detail[message]["body"]["state"], "disabled");
            }
        }
        assert!(!detail.to_string().contains("CANARY"));
        drop(child);
    }
    daemon.join.abort();
}
