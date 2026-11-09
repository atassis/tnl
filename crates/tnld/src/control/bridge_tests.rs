use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use futures::{Sink, Stream};
use tokio::sync::{oneshot, Notify};

use super::*;

/// Both sinks permanently block sends without a timer or a socket buffer race.
struct BlockedEndpoint<M> {
    message: Option<M>,
    sending: Arc<Notify>,
    released: Option<oneshot::Sender<()>>,
}

impl<M> Drop for BlockedEndpoint<M> {
    fn drop(&mut self) {
        self.released.take().unwrap().send(()).ok();
    }
}

impl<M: Unpin> Stream for BlockedEndpoint<M> {
    type Item = Result<M, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.message
            .take()
            .map_or(Poll::Pending, |message| Poll::Ready(Some(Ok(message))))
    }
}

impl<M: Unpin> Sink<M> for BlockedEndpoint<M> {
    type Error = std::io::Error;

    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.sending.notify_one();
        Poll::Pending
    }

    fn start_send(self: Pin<&mut Self>, _: M) -> Result<(), Self::Error> {
        unreachable!("permanently blocked sink became ready")
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Pending
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Pending
    }
}

fn endpoint<M>(message: Option<M>) -> (BlockedEndpoint<M>, Arc<Notify>, oneshot::Receiver<()>) {
    let sending = Arc::new(Notify::new());
    let (released, release) = oneshot::channel();
    (
        BlockedEndpoint {
            message,
            sending: sending.clone(),
            released: Some(released),
        },
        sending,
        release,
    )
}

#[tokio::test(start_paused = true)]
async fn bridge_cancellation_releases_both_directions_while_send_is_blocked() {
    for from_axum in [true, false] {
        let (axum, axum_sending, mut axum_released) =
            endpoint(from_axum.then(|| axum::extract::ws::Message::Binary(vec![1].into())));
        let (client, client_sending, mut client_released) = endpoint(
            (!from_axum).then(|| async_tungstenite::tungstenite::Message::Binary(vec![2].into())),
        );
        let cancel = tokio_util::sync::CancellationToken::new();
        let bridge_cancel = cancel.clone();
        let bridge = tokio::spawn(async move {
            bridge_messages(axum, client, &bridge_cancel).await;
        });
        if from_axum {
            client_sending.notified().await;
        } else {
            axum_sending.notified().await;
        }
        assert!(axum_released.try_recv().is_err());
        assert!(client_released.try_recv().is_err());
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), bridge)
            .await
            .expect("bridge did not observe cancellation inside blocked send")
            .unwrap();
        axum_released
            .await
            .expect("axum socket owner retained after completion");
        client_released
            .await
            .expect("duplex owner retained after completion");
    }
}

#[tokio::test(start_paused = true)]
async fn failed_cleanup_forces_driver_and_joins_blocked_bridge_after_two_seconds() {
    for from_axum in [true, false] {
        let (termination, completion) = tnl_protocol::SessionTermination::pair();
        let driver_control = termination.clone();
        let driver = tokio::spawn(async move {
            // Model a physical driver stuck inside graceful poll_close until force.
            driver_control.close_requested().await;
            driver_control.forced().await;
            drop(completion);
        });
        let cancel = tokio_util::sync::CancellationToken::new();
        let (axum, axum_sending, axum_released) =
            endpoint(from_axum.then(|| axum::extract::ws::Message::Binary(vec![1].into())));
        let (client, client_sending, client_released) = endpoint(
            (!from_axum).then(|| async_tungstenite::tungstenite::Message::Binary(vec![2].into())),
        );
        let bridge_cancel = cancel.clone();
        let bridge = tokio::spawn(async move {
            bridge_messages(axum, client, &bridge_cancel).await;
        });
        if from_axum {
            client_sending.notified().await;
        } else {
            axum_sending.notified().await;
        }
        let mut cleanup = Box::pin(finish_transport(&termination, &cancel, true));
        assert!(futures::poll!(&mut cleanup).is_pending());
        tokio::time::advance(Duration::from_millis(1999)).await;
        assert!(futures::poll!(&mut cleanup).is_pending());
        assert!(!cancel.is_cancelled());
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::time::timeout(Duration::from_secs(1), cleanup)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), bridge)
            .await
            .unwrap()
            .unwrap();
        driver.await.unwrap();
        axum_released.await.unwrap();
        client_released.await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn healthy_driver_completion_does_not_cancel_bridge_queued_write_drain() {
    let (termination, completion) = tnl_protocol::SessionTermination::pair();
    let cancel = tokio_util::sync::CancellationToken::new();
    drop(completion);
    finish_transport(&termination, &cancel, false).await;
    assert!(
        !cancel.is_cancelled(),
        "driver flush to duplex is not real-socket flush"
    );
    assert!(futures::poll!(Box::pin(termination.forced())).is_pending());
}

#[test]
fn failed_cleanup_is_not_selected_for_business_or_policy_errors() {
    for code in [
        ErrorCode::TunnelAlreadyActive,
        ErrorCode::TunnelLost,
        ErrorCode::Unauthorized,
        ErrorCode::AccessPolicyMismatch,
    ] {
        let business = anyhow::anyhow!("server rejected request: {code:?}");
        assert!(!is_physical_session_failure(&business));
    }
    assert!(is_physical_session_failure(&ControlProtocolFailure.into()));
    assert!(is_physical_session_failure(
        &std::io::Error::from(std::io::ErrorKind::ConnectionReset).into()
    ));
    // Native fatal capacity and terminal inbound overflow are not policy denials.
    for cause in [
        tnl_protocol::OpenStreamError::Capacity,
        tnl_protocol::OpenStreamError::InboundOverflow,
    ] {
        assert!(is_physical_session_failure(&cause.into()));
    }
}
