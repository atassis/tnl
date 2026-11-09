//! Sole owner of control exchanges; a cancelled decoder is never reused.
use anyhow::Context as _;
use std::{pin::Pin, time::Duration};
use tnl_protocol::{control::MAX_CONTROL_MESSAGE_BYTES, ClosingReason, ControlMsg, ErrorCode};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// Write a length-prefixed JSON `ControlMsg` to the control stream.
pub(super) async fn write_msg(
    ctrl: &mut Pin<Box<dyn tnl_protocol::Stream>>,
    msg: &ControlMsg,
) -> anyhow::Result<()> {
    let payload = zeroize::Zeroizing::new(serde_json::to_vec(msg)?);
    if payload.len() > MAX_CONTROL_MESSAGE_BYTES {
        return Err(ControlError::Framing.into());
    }
    let len = u32::try_from(payload.len())
        .context("payload too large")?
        .to_be_bytes();
    ctrl.write_all(&len).await?;
    ctrl.write_all(&payload).await?;
    ctrl.flush().await?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error("control response exceeds size bound")]
    Framing,
    #[error("invalid control response schema")]
    Schema,
    #[error("daemon sent an unexpected control response")]
    Acknowledgment,
    #[error("daemon rejected the control request ({0:?})")]
    Server(ErrorCode),
}

/// Read a length-prefixed JSON `ControlMsg` from the control stream.
pub(super) async fn read_msg(
    ctrl: &mut Pin<Box<dyn tnl_protocol::Stream>>,
) -> anyhow::Result<ControlMsg> {
    let mut lenbuf = [0u8; 4];
    ctrl.read_exact(&mut lenbuf).await?;
    let n = u32::from_be_bytes(lenbuf) as usize;
    if n > MAX_CONTROL_MESSAGE_BYTES {
        return Err(ControlError::Framing.into());
    }
    let mut respbuf = zeroize::Zeroizing::new(vec![0u8; n]);
    ctrl.read_exact(&mut respbuf).await?;
    serde_json::from_slice(&respbuf).map_err(|_| ControlError::Schema.into())
}

pub async fn close_control(control: &mut Pin<Box<dyn tnl_protocol::Stream>>) -> anyhow::Result<()> {
    write_msg(control, &ControlMsg::Close).await?;
    match read_msg(control).await? {
        ControlMsg::Closing {
            reason: ClosingReason::ClientShutdown | ClosingReason::RemoteStop,
        } => Ok(()),
        ControlMsg::Error { code, .. } => Err(ControlError::Server(code).into()),
        _ => Err(ControlError::Acknowledgment.into()),
    }
}

pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum LifecycleDeadline {
    #[error("connection attempt deadline exceeded")]
    Attempt,
    #[error("control heartbeat deadline exceeded")]
    Heartbeat,
}

pub enum ControlEnd {
    RemoteStop,
    Shutdown { acknowledged: bool, unhealthy: bool },
}

pub struct ControlOwner {
    stream: Pin<Box<dyn tnl_protocol::Stream>>,
    tunnel_id: ulid::Ulid,
}

impl ControlOwner {
    pub fn new(stream: Pin<Box<dyn tnl_protocol::Stream>>, tunnel_id: ulid::Ulid) -> Self {
        Self { stream, tunnel_id }
    }

    pub async fn run(
        mut self,
        shutdown: tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<ControlEnd> {
        let mut sequence = 0u64;
        loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => return Ok(self.close().await),
                () = tokio::time::sleep(HEARTBEAT_INTERVAL) => {},
            }
            let tunnel_id = self.tunnel_id;
            let (result, shutdown_deadline) = {
                let exchange = tokio::time::timeout(EXCHANGE_TIMEOUT, async {
                    write_msg(
                        &mut self.stream,
                        &ControlMsg::Heartbeat {
                            tunnel_id,
                            sequence,
                        },
                    )
                    .await?;
                    match read_msg(&mut self.stream).await? {
                        ControlMsg::HeartbeatAck {
                            tunnel_id: id,
                            sequence: ack,
                        } if id == tunnel_id && ack == sequence => Ok(None),
                        ControlMsg::Closing {
                            reason: ClosingReason::RemoteStop,
                        } => Ok(Some(ControlEnd::RemoteStop)),
                        ControlMsg::Error { code, .. } => Err(ControlError::Server(code).into()),
                        _ => Err(ControlError::Acknowledgment.into()),
                    }
                });
                tokio::pin!(exchange);
                tokio::select! {
                    biased;
                    result = &mut exchange => (result, None),
                    () = shutdown.cancelled() => {
                        let deadline = tokio::time::Instant::now() + SHUTDOWN_TIMEOUT;
                        // Retain this decoder across shutdown, even after a partial
                        // prefix/payload. Expiry abandons the entire stream.
                        match tokio::time::timeout_at(deadline, &mut exchange).await {
                            Ok(result) => (result, Some(deadline)),
                            Err(_) => return Ok(ControlEnd::Shutdown { acknowledged: false, unhealthy: true }),
                        }
                    }
                }
            };
            let response = result
                .map_err(|_| anyhow::Error::new(LifecycleDeadline::Heartbeat))
                .and_then(|response| response);
            match response {
                Ok(Some(end)) => return Ok(end),
                Ok(None) => {
                    if let Some(deadline) = shutdown_deadline {
                        return Ok(self.close_at(deadline).await);
                    }
                }
                Err(error) if shutdown_deadline.is_some() => {
                    return Ok(ControlEnd::Shutdown {
                        acknowledged: false,
                        unhealthy: crate::reconnect::is_physical_session_failure(&error),
                    })
                }
                Err(error) => return Err(error),
            }
            sequence = sequence
                .checked_add(1)
                .ok_or(ControlError::Acknowledgment)?;
        }
    }

    async fn close(&mut self) -> ControlEnd {
        self.close_at(tokio::time::Instant::now() + SHUTDOWN_TIMEOUT)
            .await
    }

    async fn close_at(&mut self, deadline: tokio::time::Instant) -> ControlEnd {
        let result = tokio::time::timeout_at(deadline, close_control(&mut self.stream)).await;
        let acknowledged = matches!(result, Ok(Ok(())));
        let unhealthy = match result {
            Err(_) => true,
            Ok(Err(error)) => crate::reconnect::is_physical_session_failure(&error),
            Ok(Ok(())) => false,
        };
        ControlEnd::Shutdown {
            acknowledged,
            unhealthy,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(msg: &ControlMsg) -> Vec<u8> {
        let payload = serde_json::to_vec(msg).unwrap();
        let mut bytes = u32::try_from(payload.len()).unwrap().to_be_bytes().to_vec();
        bytes.extend(payload);
        bytes
    }

    // The IO is real duplex framing, not a pre-decoded message mock. Polling
    // explicitly at each boundary proves a prefix/payload was consumed before
    // shutdown transitions the owner.
    async fn partial_shutdown(split: usize) {
        let (client, peer) = tokio::io::duplex(1024);
        let tunnel_id = ulid::Ulid::new();
        let stop = tokio_util::sync::CancellationToken::new();
        let run = ControlOwner::new(Box::pin(client), tunnel_id).run(stop.clone());
        tokio::pin!(run);
        assert!(futures::poll!(&mut run).is_pending());
        tokio::time::advance(HEARTBEAT_INTERVAL).await;
        assert!(futures::poll!(&mut run).is_pending());
        let mut stream: Pin<Box<dyn tnl_protocol::Stream>> = Box::pin(peer);
        assert_eq!(
            read_msg(&mut stream).await.unwrap(),
            ControlMsg::Heartbeat {
                tunnel_id,
                sequence: 0
            }
        );
        let ack = frame(&ControlMsg::HeartbeatAck {
            tunnel_id,
            sequence: 0,
        });
        stream.write_all(&ack[..split]).await.unwrap();
        stream.flush().await.unwrap();
        assert!(futures::poll!(&mut run).is_pending());
        stop.cancel();
        assert!(futures::poll!(&mut run).is_pending());
        // A second reader/Close at this point would corrupt the outstanding ACK.
        stream.write_all(&ack[split..]).await.unwrap();
        stream.flush().await.unwrap();
        assert!(futures::poll!(&mut run).is_pending());
        assert_eq!(read_msg(&mut stream).await.unwrap(), ControlMsg::Close);
        write_msg(
            &mut stream,
            &ControlMsg::Closing {
                reason: ClosingReason::ClientShutdown,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            run.await.unwrap(),
            ControlEnd::Shutdown {
                acknowledged: true,
                unhealthy: false
            }
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_preserves_partial_prefix_decoder() {
        partial_shutdown(2).await;
    }
    #[tokio::test(start_paused = true)]
    async fn shutdown_preserves_partial_payload_decoder() {
        partial_shutdown(11).await;
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeat_bounds_entire_write_flush_and_read_exchange() {
        for capacity in [1, 1024] {
            let (client, _peer) = tokio::io::duplex(capacity);
            let run = ControlOwner::new(Box::pin(client), ulid::Ulid::new())
                .run(tokio_util::sync::CancellationToken::new());
            tokio::pin!(run);
            assert!(futures::poll!(&mut run).is_pending());
            tokio::time::advance(HEARTBEAT_INTERVAL).await;
            assert!(futures::poll!(&mut run).is_pending());
            tokio::time::advance(EXCHANGE_TIMEOUT).await;
            assert!(matches!(
                run.await.err().unwrap().downcast_ref::<LifecycleDeadline>(),
                Some(LifecycleDeadline::Heartbeat)
            ));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_deadline_abandons_partial_frame_without_close_or_second_read() {
        let (client, mut peer) = tokio::io::duplex(1024);
        let stop = tokio_util::sync::CancellationToken::new();
        let run = ControlOwner::new(Box::pin(client), ulid::Ulid::new()).run(stop.clone());
        tokio::pin!(run);
        assert!(futures::poll!(&mut run).is_pending());
        tokio::time::advance(HEARTBEAT_INTERVAL).await;
        assert!(futures::poll!(&mut run).is_pending());
        let mut heartbeat = vec![0; 1024];
        assert!(peer.read(&mut heartbeat).await.unwrap() > 0);
        peer.write_all(&[0, 0]).await.unwrap();
        assert!(futures::poll!(&mut run).is_pending());
        stop.cancel();
        assert!(futures::poll!(&mut run).is_pending());
        tokio::time::advance(SHUTDOWN_TIMEOUT).await;
        assert!(matches!(
            run.await.unwrap(),
            ControlEnd::Shutdown {
                acknowledged: false,
                unhealthy: true
            }
        ));
        assert_eq!(
            peer.read(&mut heartbeat).await.unwrap(),
            0,
            "abandonment sent an unsafe Close"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn mismatched_identity_sequence_schema_and_oversize_are_sanitized_terminal_failures() {
        let id = ulid::Ulid::new();
        let replies = [
            frame(&ControlMsg::HeartbeatAck {
                tunnel_id: ulid::Ulid::new(),
                sequence: 0,
            }),
            frame(&ControlMsg::HeartbeatAck {
                tunnel_id: id,
                sequence: 1,
            }),
            [4u32.to_be_bytes().as_slice(), b"SECR"].concat(),
            u32::MAX.to_be_bytes().to_vec(),
        ];
        for bytes in replies {
            let (client, mut peer) = tokio::io::duplex(1024);
            let run = ControlOwner::new(Box::pin(client), id)
                .run(tokio_util::sync::CancellationToken::new());
            tokio::pin!(run);
            assert!(futures::poll!(&mut run).is_pending());
            tokio::time::advance(HEARTBEAT_INTERVAL).await;
            assert!(futures::poll!(&mut run).is_pending());
            let mut heartbeat = [0; 1024];
            assert!(peer.read(&mut heartbeat).await.unwrap() > 0);
            peer.write_all(&bytes).await.unwrap();
            let error = run.await.err().unwrap();
            assert!(!format!("{error:#}").contains("SECR"));
            assert!(crate::reconnect::is_physical_session_failure(&error));
            let mut remaining = [0; 1];
            assert_eq!(peer.read(&mut remaining).await.unwrap(), 0);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn only_one_heartbeat_outstanding_and_exact_sequence_advances_after_ack() {
        let (client, peer) = tokio::io::duplex(1024);
        let mut peer: Pin<Box<dyn tnl_protocol::Stream>> = Box::pin(peer);
        let id = ulid::Ulid::new();
        let stop = tokio_util::sync::CancellationToken::new();
        let run = ControlOwner::new(Box::pin(client), id).run(stop.clone());
        tokio::pin!(run);
        assert!(futures::poll!(&mut run).is_pending());
        for sequence in 0..3 {
            tokio::time::advance(HEARTBEAT_INTERVAL).await;
            assert!(futures::poll!(&mut run).is_pending());
            assert_eq!(
                read_msg(&mut peer).await.unwrap(),
                ControlMsg::Heartbeat {
                    tunnel_id: id,
                    sequence
                }
            );
            let mut byte = [0; 1];
            {
                let read = peer.read(&mut byte);
                tokio::pin!(read);
                assert!(
                    futures::poll!(&mut read).is_pending(),
                    "overlapping heartbeat"
                );
            }
            write_msg(
                &mut peer,
                &ControlMsg::HeartbeatAck {
                    tunnel_id: id,
                    sequence,
                },
            )
            .await
            .unwrap();
            assert!(futures::poll!(&mut run).is_pending());
        }
        stop.cancel();
        assert!(futures::poll!(&mut run).is_pending());
        assert_eq!(read_msg(&mut peer).await.unwrap(), ControlMsg::Close);
        write_msg(
            &mut peer,
            &ControlMsg::Closing {
                reason: ClosingReason::ClientShutdown,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            run.await.unwrap(),
            ControlEnd::Shutdown {
                acknowledged: true,
                ..
            }
        ));
    }

    struct BlockedFlush(tokio::io::DuplexStream);
    impl tokio::io::AsyncRead for BlockedFlush {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buffer: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_read(cx, buffer)
        }
    }
    impl tokio::io::AsyncWrite for BlockedFlush {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            Pin::new(&mut self.0).poll_write(cx, bytes)
        }
        fn poll_flush(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Pending
        }
        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_shutdown(cx)
        }
    }

    #[tokio::test(start_paused = true)]
    async fn blocked_flush_is_inside_the_same_heartbeat_exchange_deadline() {
        let (client, peer) = tokio::io::duplex(1024);
        let id = ulid::Ulid::new();
        let run = ControlOwner::new(Box::pin(BlockedFlush(client)), id)
            .run(tokio_util::sync::CancellationToken::new());
        tokio::pin!(run);
        assert!(futures::poll!(&mut run).is_pending());
        tokio::time::advance(HEARTBEAT_INTERVAL).await;
        assert!(futures::poll!(&mut run).is_pending());
        let mut peer: Pin<Box<dyn tnl_protocol::Stream>> = Box::pin(peer);
        assert_eq!(
            read_msg(&mut peer).await.unwrap(),
            ControlMsg::Heartbeat {
                tunnel_id: id,
                sequence: 0
            }
        );
        // Even a ready valid reply cannot rescue an exchange whose flush stalls.
        write_msg(
            &mut peer,
            &ControlMsg::HeartbeatAck {
                tunnel_id: id,
                sequence: 0,
            },
        )
        .await
        .unwrap();
        tokio::time::advance(EXCHANGE_TIMEOUT).await;
        assert!(run.await.err().unwrap().is::<LifecycleDeadline>());
    }

    #[tokio::test(start_paused = true)]
    async fn finishing_pending_heartbeat_does_not_reset_shutdown_cleanup_budget() {
        let (client, peer) = tokio::io::duplex(1024);
        let id = ulid::Ulid::new();
        let stop = tokio_util::sync::CancellationToken::new();
        let run = ControlOwner::new(Box::pin(client), id).run(stop.clone());
        tokio::pin!(run);
        assert!(futures::poll!(&mut run).is_pending());
        tokio::time::advance(HEARTBEAT_INTERVAL).await;
        assert!(futures::poll!(&mut run).is_pending());
        let mut peer: Pin<Box<dyn tnl_protocol::Stream>> = Box::pin(peer);
        read_msg(&mut peer).await.unwrap();
        stop.cancel();
        assert!(futures::poll!(&mut run).is_pending());
        tokio::time::advance(Duration::from_secs(1)).await;
        write_msg(
            &mut peer,
            &ControlMsg::HeartbeatAck {
                tunnel_id: id,
                sequence: 0,
            },
        )
        .await
        .unwrap();
        assert!(futures::poll!(&mut run).is_pending());
        assert_eq!(read_msg(&mut peer).await.unwrap(), ControlMsg::Close);
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(matches!(
            run.await.unwrap(),
            ControlEnd::Shutdown {
                acknowledged: false,
                unhealthy: true
            }
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn business_rejection_during_close_does_not_authorize_failed_physical_cleanup() {
        let (client, peer) = tokio::io::duplex(1024);
        let stop = tokio_util::sync::CancellationToken::new();
        stop.cancel();
        let run = ControlOwner::new(Box::pin(client), ulid::Ulid::new()).run(stop);
        tokio::pin!(run);
        assert!(futures::poll!(&mut run).is_pending());
        let mut peer: Pin<Box<dyn tnl_protocol::Stream>> = Box::pin(peer);
        assert_eq!(read_msg(&mut peer).await.unwrap(), ControlMsg::Close);
        write_msg(
            &mut peer,
            &ControlMsg::Error {
                code: ErrorCode::Unauthorized,
                message: "SERVER_SECRET".into(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            run.await.unwrap(),
            ControlEnd::Shutdown {
                acknowledged: false,
                unhealthy: false
            }
        ));
    }
}
