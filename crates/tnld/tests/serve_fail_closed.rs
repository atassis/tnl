use std::io::Write;

/// `tnld serve` (via `spawn_server`) must refuse to start when the token store
/// is empty, pointing the operator at `tnld init`.
#[tokio::test]
async fn serve_refuses_to_start_with_empty_token_store() {
    let mut tokens = tempfile::NamedTempFile::new().unwrap();
    writeln!(tokens, "tokens = []").unwrap();

    let cfg = tnld::config::Config {
        listen: "127.0.0.1:0".into(),
        public_url: "https://api.tnl.example.com".into(),
        hostname_root: "t.example.com".into(),
        tokens_file: tokens.path().display().to_string(),
        session_grace_sec: 30,
    };

    let err = tnld::serve::spawn_server(cfg).await.unwrap_err();
    assert!(err.to_string().contains("tnld init"), "got: {err}");
}
