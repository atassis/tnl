//! Synthetic pending dial models DNS/TCP delay at the actual connect await.
//! It does not assert cancellation of Tokio's blocking OS resolver worker.
use super::*;
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

#[allow(clippy::result_large_err)]
async fn boundary_case(boundary: ConnectBoundary, shutdown: bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let probe = Arc::new(ConnectBoundaryProbe::default());
    let (finish, finished) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        if boundary == ConnectBoundary::Dial {
            finished.await.unwrap();
            assert!(
                futures::poll!(Box::pin(listener.accept())).is_pending(),
                "cancelled resolver/dial initiated a late connection"
            );
        } else {
            let (socket, _) = listener.accept().await.unwrap();
            let ws = async_tungstenite::tokio::accept_hdr_async(
                socket,
                |_: &http::Request<()>, mut response: http::Response<()>| {
                    response.headers_mut().insert(
                        tnl_protocol::control::CONTROL_PROTOCOL_HEADER,
                        http::HeaderValue::from_static(
                            tnl_protocol::control::CONTROL_PROTOCOL_VERSION,
                        ),
                    );
                    Ok(response)
                },
            )
            .await
            .unwrap();
            let mut session = tnl_protocol::server_session_from_ws_generic(ws);
            assert!(
                session.accept_stream().await.unwrap().is_none(),
                "cancelled open produced a late control stream"
            );
        }
    });
    let (signal, signalled) = tokio::sync::oneshot::channel();
    let connect = tokio::spawn(TEST_CONNECT_BOUNDARY.scope(
        (boundary, probe.clone()),
        async move {
            if shutdown {
                crate::reconnect::run_until_shutdown(
                    &endpoint,
                    "tnl_TEST",
                    Some("demo"),
                    crate::target::Target::LocalhostPort(1),
                    crate::host_header::HostHeader::Preserve,
                    &TunnelOptions::default(),
                    crate::reconnect::Hooks::default(),
                    async {
                        signalled.await?;
                        Ok(())
                    },
                )
                .await
            } else {
                connect_and_create(&endpoint, "tnl_TEST", "demo")
                    .await
                    .map(|_| ())
            }
        },
    ));
    probe.entered.cancelled().await;
    tokio::time::pause();
    if shutdown {
        signal.send(()).unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(2)).await;
    } else {
        tokio::time::advance(Duration::from_secs(20)).await;
    }
    tokio::time::resume();
    let result = tokio::time::timeout(Duration::from_secs(3), connect)
        .await
        .expect("actual connect/run did not honor its lifecycle deadline")
        .unwrap();
    if shutdown {
        result.unwrap();
    } else {
        assert!(matches!(
            result
                .unwrap_err()
                .downcast_ref::<control::LifecycleDeadline>(),
            Some(control::LifecycleDeadline::Attempt)
        ));
    }
    assert!(probe.dropped.is_cancelled());
    probe.release.cancel();
    tokio::task::yield_now().await;
    assert!(
        !probe.continued.load(Ordering::SeqCst),
        "late resolver/open continuation ran after cancellation"
    );
    if boundary == ConnectBoundary::Open {
        let termination = probe.termination.lock().unwrap().clone().unwrap();
        termination.completed().await;
    }
    let _ = finish.send(());
    server.await.unwrap();
}

#[tokio::test]
async fn dial_and_control_open_deadlines_drop_pending_work_before_any_late_registration() {
    for boundary in [ConnectBoundary::Dial, ConnectBoundary::Open] {
        boundary_case(boundary, false).await;
    }
}

#[tokio::test]
async fn shutdown_during_dial_or_open_observes_cleanup_and_prevents_late_registration() {
    for boundary in [ConnectBoundary::Dial, ConnectBoundary::Open] {
        boundary_case(boundary, true).await;
    }
}
