use std::time::Duration;

use tnl_protocol::request_metadata::{
    DataStreamProtocol, DATA_STREAM_PROTOCOL_HEADER, HTTP1_METADATA_V1,
};
use tnl_protocol::{ControlMsg, Session as _, TunnelCreatedResp};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A legacy-compatible daemon fixture with controlled upgrade echoes. It accepts
/// both create and reattach, so every connection must choose its own format.
#[allow(clippy::result_large_err)] // tungstenite requires this callback error type.
async fn server(echoes: Vec<Vec<&'static str>>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        for echo in echoes {
            let (socket, _) = listener.accept().await.unwrap();
            let ws = async_tungstenite::tokio::accept_hdr_async(
                socket,
                move |req: &http::Request<()>, mut response: http::Response<()>| {
                    response.headers_mut().insert(
                        tnl_protocol::control::CONTROL_PROTOCOL_HEADER,
                        http::HeaderValue::from_static(
                            tnl_protocol::control::CONTROL_PROTOCOL_VERSION,
                        ),
                    );
                    assert_eq!(
                        req.headers()
                            .get_all(DATA_STREAM_PROTOCOL_HEADER)
                            .iter()
                            .count(),
                        1
                    );
                    assert_eq!(
                        req.headers()[DATA_STREAM_PROTOCOL_HEADER],
                        HTTP1_METADATA_V1
                    );
                    assert_eq!(req.headers()["authorization"], "Bearer tnl_TEST");
                    for value in &echo {
                        response
                            .headers_mut()
                            .append(DATA_STREAM_PROTOCOL_HEADER, value.parse().unwrap());
                    }
                    Ok(response)
                },
            )
            .await
            .unwrap();
            tokio::spawn(async move {
                let mut session = tnl_protocol::transport::server_session_from_ws_generic(ws);
                let Ok(Some(mut control)) = session.accept_stream().await else {
                    return;
                };
                let n = control.read_u32().await.unwrap();
                let mut bytes = vec![0; n as usize];
                control.read_exact(&mut bytes).await.unwrap();
                let msg: ControlMsg = serde_json::from_slice(&bytes).unwrap();
                assert!(matches!(
                    msg,
                    ControlMsg::CreateTunnel(_) | ControlMsg::ReattachTunnel(_)
                ));
                let reply = serde_json::to_vec(&ControlMsg::TunnelCreated(TunnelCreatedResp {
                    tunnel_id: ulid::Ulid::nil(),
                    hostname: "demo.t.x".into(),
                    subdomain: "demo".into(),
                }))
                .unwrap();
                control
                    .write_u32(u32::try_from(reply.len()).unwrap())
                    .await
                    .unwrap();
                control.write_all(&reply).await.unwrap();
                control.flush().await.unwrap();
                // Keep the session/control alive until the client drops them.
                let _ = control.read_u8().await;
            });
        }
    });
    (endpoint, task)
}

#[tokio::test]
async fn client_metadata_negotiation_create_and_reattach_reselect_per_connection() {
    let (endpoint, task) = server(vec![vec![], vec![HTTP1_METADATA_V1], vec![]]).await;
    let first = tnl::client::connect_and_create(&endpoint, "tnl_TEST", "demo")
        .await
        .unwrap();
    assert_eq!(first.data_stream_protocol, DataStreamProtocol::RawHttp1);
    let second = tnl::client::connect_and_reattach(&endpoint, "tnl_TEST", first.tunnel_id, "demo")
        .await
        .unwrap();
    assert_eq!(
        second.data_stream_protocol,
        DataStreamProtocol::Http1MetadataV1
    );
    let third = tnl::client::connect_and_reattach(&endpoint, "tnl_TEST", first.tunnel_id, "demo")
        .await
        .unwrap();
    assert_eq!(third.data_stream_protocol, DataStreamProtocol::RawHttp1);
    task.await.unwrap();
}

#[tokio::test]
async fn client_metadata_negotiation_invalid_echo_is_connection_error() {
    for echo in [
        vec![""],
        vec!["future"],
        vec![HTTP1_METADATA_V1, HTTP1_METADATA_V1],
        vec!["http1-metadata-v1, future"],
    ] {
        let (endpoint, task) = server(vec![echo]).await;
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            tnl::client::connect_and_create(&endpoint, "tnl_TEST", "demo"),
        )
        .await
        .unwrap();
        assert!(result
            .err()
            .unwrap()
            .to_string()
            .contains("invalid data stream protocol echo"));
        task.await.unwrap();
    }
}

#[tokio::test]
#[allow(clippy::result_large_err)]
async fn control_version_echo_is_required_before_any_yamux_or_registration_bytes() {
    use futures::StreamExt as _;
    for echoes in [vec![], vec!["future"], vec!["heartbeat-v1", "heartbeat-v1"]] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = async_tungstenite::tokio::accept_hdr_async(
                socket,
                move |_: &http::Request<()>, mut response: http::Response<()>| {
                    for echo in &echoes {
                        response.headers_mut().append(
                            tnl_protocol::control::CONTROL_PROTOCOL_HEADER,
                            echo.parse().unwrap(),
                        );
                    }
                    Ok(response)
                },
            )
            .await
            .unwrap();
            // Rejection must happen before constructing the yamux session (even
            // its bootstrap ping would be evidence of mutation past the echo).
            matches!(
                ws.next().await,
                Some(Ok(async_tungstenite::tungstenite::Message::Binary(_)))
            )
        });
        let error = tnl::client::connect_and_create(&endpoint, "tnl_TEST", "demo")
            .await
            .unwrap_err();
        let sent_data = server.await.unwrap();
        assert!(!sent_data, "incompatible echo sent control transport bytes");
        assert!(
            error.to_string().contains("incompatible control protocol"),
            "{error}"
        );
    }
}

#[tokio::test]
async fn public_registration_rejects_changed_requested_name_or_reattach_identity() {
    let (endpoint, task) = server(vec![vec![]]).await;
    assert!(
        tnl::client::connect_and_create(&endpoint, "tnl_TEST", "other")
            .await
            .is_err(),
        "create silently changed the requested name"
    );
    task.await.unwrap();
    let (endpoint, task) = server(vec![vec![]]).await;
    assert!(
        tnl::client::connect_and_reattach(&endpoint, "tnl_TEST", ulid::Ulid::new(), "demo")
            .await
            .is_err(),
        "reattach silently changed tunnel identity"
    );
    task.await.unwrap();
}
