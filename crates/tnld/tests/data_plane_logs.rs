//! Regression test: data-plane requests emit at least one observable tracing
//! event with host/method/path/status — so operators can see traffic.
//!
//! Captures `tracing` output via `tracing-subscriber`'s `make_writer` to an
//! `Arc<Mutex<Vec<u8>>>` and asserts on substrings after a request that
//! falls through to the unknown-host path (we have no yamux client in this
//! test, so we can only exercise the failure-side log here; the success-side
//! info! log lands once Phase 8 has the full e2e harness).

use std::sync::{Arc, Mutex};

use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config as TnldConfig;
use tnld::serve::spawn_server;
use tracing_subscriber::fmt::MakeWriter;

#[derive(Clone, Default)]
struct VecWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for VecWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for VecWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn data_plane_request_emits_tracing_output() {
    let sink = VecWriter::default();
    let _ = tracing_subscriber::fmt()
        .with_env_filter("tnld=debug")
        .with_writer(sink.clone())
        .try_init();

    let hash = hash_plaintext("tnl_TEST").unwrap();
    let tokens = TokensFile {
        tokens: vec![TokenEntry {
            name: "test".into(),
            hash,
        }],
    };
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), toml::to_string(&tokens).unwrap()).unwrap();
    let cfg = TnldConfig {
        listen: "127.0.0.1:0".into(),
        public_url: "http://test".into(),
        hostname_root: "t.example.com".into(),
        tokens_file: tmp.path().to_string_lossy().into_owned(),
        session_grace_sec: 30,
    };
    let handle = spawn_server(cfg).await.unwrap();
    let addr = handle.local_addr.to_string();

    // Send a request that hits the data-plane fallback (unknown host).
    let _ = reqwest::Client::new()
        .get(format!("http://{addr}/some/path"))
        .header("Host", "ghost.t.example.com")
        .send()
        .await
        .unwrap();

    // Give the subscriber a moment to flush.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let captured = String::from_utf8_lossy(&sink.0.lock().unwrap()).to_string();
    assert!(
        captured.contains("no tunnel registered") || captured.contains("data-plane request"),
        "expected data-plane tracing output, got: {captured:?}"
    );
}
