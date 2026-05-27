//! Regression test for the alpha-deploy bug:
//!     async-tungstenite was compiled without a TLS feature, so
//!     `connect_async("wss://...")` failed with
//!     `URL error: TLS support not compiled in`.
//!
//! This test stands up a minimal rustls-backed TCP listener that accepts
//! a TLS handshake (cert is self-signed via rcgen), then verifies that
//! `tnl::client::connect_and_create`-style dial of `wss://` makes it past
//! the TLS layer. We assert specifically that the failure is NOT the
//! "TLS support not compiled in" symptom.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wss_dial_does_not_report_tls_feature_missing() {
    // Install the aws_lc_rs crypto provider (rustls 0.23 requires an explicit
    // provider; error means already installed, which is fine in test context).
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    // 1) Generate a self-signed cert for "localhost".
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = CertificateDer::from(cert.cert.der().to_vec());
    let key_der = PrivateKeyDer::try_from(cert.key_pair.serialize_der()).unwrap();

    // 2) Bind a TCP listener; spawn a task that accepts ONE TLS handshake then drops.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();

    let server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));

    tokio::spawn(async move {
        if let Ok((sock, _)) = listener.accept().await {
            let Ok(mut tls) = acceptor.accept(sock).await else {
                return;
            };
            // Read any bytes, drop the connection.
            let mut buf = [0u8; 256];
            let _ = tls.read(&mut buf).await;
            let _ = tls.shutdown().await;
        }
    });

    // 3) Build a client root store that trusts our self-signed cert.
    //    (We don't pass the cfg into connect_async — async-tungstenite uses its
    //     bundled webpki-roots. The cert won't validate; we accept ANY failure
    //     except the specific "TLS support not compiled in" one.)
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert_der).unwrap();
    let _client_cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    let url = format!("wss://localhost:{}/control", addr.port());
    let req = http::Request::builder()
        .method("GET")
        .uri(&url)
        .header("Host", format!("localhost:{}", addr.port()))
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header(
            "Sec-WebSocket-Key",
            async_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .header("Sec-WebSocket-Version", "13")
        .body(())
        .unwrap();

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        async_tungstenite::tokio::connect_async(req),
    )
    .await
    .expect("dial should not hang");

    match result {
        Ok(_) => {
            // Unexpected success (we used webpki-roots which don't trust self-signed),
            // but it means TLS works — also fine for this regression test.
        }
        Err(e) => {
            let msg = format!("{e:#}").to_lowercase();
            assert!(
                !msg.contains("tls support not compiled in"),
                "TLS feature missing on async-tungstenite — regression: {e:#}"
            );
        }
    }
}
