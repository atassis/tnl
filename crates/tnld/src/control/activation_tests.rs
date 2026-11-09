use std::{
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{Context, Poll},
    time::Duration,
};

use futures::{Sink, Stream};
use tnl_protocol::{CreateTunnelReq, Session as _};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    sync::{mpsc, oneshot, Notify},
};

use super::*;

type Message = axum::extract::ws::Message;

pub(super) struct Socket {
    pub(super) incoming: mpsc::UnboundedReceiver<Message>,
    pub(super) outgoing: mpsc::UnboundedSender<Message>,
    pub(super) blocked: Arc<AtomicBool>,
    pub(super) sending: Arc<Notify>,
    pub(super) released: Option<oneshot::Sender<()>>,
}

impl Drop for Socket {
    fn drop(&mut self) {
        self.released.take().unwrap().send(()).ok();
    }
}

impl Stream for Socket {
    type Item = Result<Message, std::io::Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.incoming.poll_recv(cx).map(|message| message.map(Ok))
    }
}

impl Sink<Message> for Socket {
    type Error = std::io::Error;
    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.blocked.load(Ordering::SeqCst) {
            self.sending.notify_one();
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }
    fn start_send(self: Pin<&mut Self>, message: Message) -> Result<(), Self::Error> {
        self.outgoing
            .send(message)
            .map_err(|_| std::io::ErrorKind::BrokenPipe.into())
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Pending
    }
}

async fn write_control(control: &mut Pin<Box<dyn tnl_protocol::Stream>>, message: &ControlMsg) {
    let payload = serde_json::to_vec(message).unwrap();
    control
        .write_u32(u32::try_from(payload.len()).unwrap())
        .await
        .unwrap();
    control.write_all(&payload).await.unwrap();
    control.flush().await.unwrap();
}

pub(super) fn state() -> AppState {
    let tokens = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tokens.path(), "tokens = []").unwrap();
    AppState {
        trusted_proxies: Arc::new(crate::visitor_ip::TrustedProxies::from_cidrs(&[]).unwrap()),
        tokens: Arc::new(crate::auth::TokenStore::load(tokens.path()).unwrap()),
        registry: Arc::new(Registry::new("t.x")),
        session_handles: Arc::new(dashmap::DashMap::new()),
        pair_registry: Arc::new(crate::pair::PairRegistry::new()),
        public_url: "http://x".into(),
        session_grace_sec: 30,
    }
}

pub(super) async fn relay_client_bytes(
    relay_io: tokio::io::DuplexStream,
    relay_incoming: mpsc::UnboundedSender<Message>,
    mut sent: mpsc::UnboundedReceiver<Message>,
    relay_stop: tokio_util::sync::CancellationToken,
) {
    let (mut read, mut write) = tokio::io::split(relay_io);
    let upload = async {
        let mut bytes = [0; 8192];
        loop {
            let n = read.read(&mut bytes).await.unwrap();
            if n == 0 {
                break;
            }
            if relay_incoming
                .send(Message::Binary(bytes[..n].to_vec().into()))
                .is_err()
            {
                break;
            }
        }
    };
    let download = async {
        while let Some(Message::Binary(bytes)) = sent.recv().await {
            if write.write_all(&bytes).await.is_err() {
                break;
            }
        }
    };
    tokio::select! { () = upload => {}, () = download => {}, () = relay_stop.cancelled() => {} }
}

#[derive(Clone, Copy)]
enum PhysicalFault {
    Protocol,
    InboundOverflow,
    NativeCapacity,
}

#[tokio::test(start_paused = true)]
async fn established_control_eof_from_physical_protocol_failure_releases_blocked_bridge() {
    established_failure_releases_blocked_bridge(PhysicalFault::Protocol).await;
}

#[tokio::test(start_paused = true)]
async fn established_inbound_overflow_releases_blocked_bridge() {
    established_failure_releases_blocked_bridge(PhysicalFault::InboundOverflow).await;
}

#[tokio::test(start_paused = true)]
async fn native_fatal_capacity_releases_blocked_bridge() {
    established_failure_releases_blocked_bridge(PhysicalFault::NativeCapacity).await;
}

#[allow(clippy::too_many_lines)]
async fn established_failure_releases_blocked_bridge(fault: PhysicalFault) {
    let state = state();
    let (incoming, receiver) = mpsc::unbounded_channel();
    let (outgoing, sent) = mpsc::unbounded_channel();
    let blocked = Arc::new(AtomicBool::new(false));
    let sending = Arc::new(Notify::new());
    let (released, mut release) = oneshot::channel();
    let socket = Socket {
        incoming: receiver,
        outgoing,
        blocked: blocked.clone(),
        sending: sending.clone(),
        released: Some(released),
    };
    let (daemon_ws, bridge_ws) = websocket_bridge_pair().await;
    let daemon_state = state.clone();
    let daemon = tokio::spawn(async move {
        let cancel = tokio_util::sync::CancellationToken::new();
        let token = AuthedToken {
            name: "owner".into(),
        };
        tokio::join!(
            run_transport_session(
                daemon_ws,
                &daemon_state,
                &token,
                DataStreamProtocol::RawHttp1,
                &cancel
            ),
            bridge_messages(socket, bridge_ws, &cancel),
        );
    });
    let (client_io, relay_io) = tokio::io::duplex(64 * 1024);
    let relay_incoming = incoming.clone();
    let stop_relay = tokio_util::sync::CancellationToken::new();
    let relay_stop = stop_relay.clone();
    let relay = tokio::spawn(relay_client_bytes(
        relay_io,
        relay_incoming,
        sent,
        relay_stop,
    ));
    let mut client = tnl_protocol::transport::YamuxSession::new_server(client_io);
    let mut control = client.open_stream().await.unwrap();
    write_control(
        &mut control,
        &ControlMsg::CreateTunnel(CreateTunnelReq {
            subdomain: Some("activation".into()),
        }),
    )
    .await;
    let length = control.read_u32().await.unwrap();
    let mut payload = vec![0; length as usize];
    control.read_exact(&mut payload).await.unwrap();
    let ControlMsg::TunnelCreated(created) = serde_json::from_slice(&payload).unwrap() else {
        panic!("fixture did not register");
    };
    let session_id = state
        .registry
        .current_session_id(&created.tunnel_id.to_string())
        .unwrap();
    let handle = state.session_handles.get(&session_id).unwrap().clone();
    let termination = handle.session.lock().await.termination();
    // Keep both ends' native active-stream count at the default 512 (one
    // control + 511 data), without retaining undelivered inbound streams or
    // stalling at the 256 un-ACK'd limit. No injected config or fake error.
    let mut held = Vec::new();
    if matches!(fault, PhysicalFault::NativeCapacity) {
        for _ in 0..511 {
            let mut outgoing = handle.session.lock().await.open_stream().await.unwrap();
            outgoing.write_all(b"x").await.unwrap();
            let mut incoming = client.accept_stream().await.unwrap().unwrap();
            incoming.read_exact(&mut [0; 1]).await.unwrap();
            incoming.write_all(b"a").await.unwrap();
            outgoing.read_exact(&mut [0; 1]).await.unwrap();
            held.push((outgoing, incoming));
        }
    }
    // Block only after a real control exchange completed. This ACK is queued
    // into the bridge while the control owner returns to reading at a boundary.
    blocked.store(true, Ordering::SeqCst);
    write_control(
        &mut control,
        &ControlMsg::Heartbeat {
            tunnel_id: created.tunnel_id,
            sequence: 1,
        },
    )
    .await;
    sending.notified().await;
    assert!(release.try_recv().is_err());
    let expected = match fault {
        PhysicalFault::Protocol => {
            incoming.send(Message::Binary(vec![1; 12].into())).unwrap();
            tnl_protocol::OpenStreamError::Protocol
        }
        PhysicalFault::InboundOverflow => {
            let mut bytes = Vec::new();
            // Control already used even stream ID 2. The 257th undelivered
            // incoming stream exceeds the transport delivery bound, not 512.
            for id in (4u32..).step_by(2).take(257) {
                bytes.extend_from_slice(&[0, 0, 0, 1]);
                bytes.extend_from_slice(&id.to_be_bytes());
                bytes.extend_from_slice(&[0; 4]);
            }
            incoming.send(Message::Binary(bytes.into())).unwrap();
            tnl_protocol::OpenStreamError::InboundOverflow
        }
        PhysicalFault::NativeCapacity => {
            let error = handle
                .session
                .lock()
                .await
                .open_stream()
                .await
                .err()
                .unwrap();
            assert_eq!(
                error.downcast_ref::<tnl_protocol::OpenStreamError>(),
                Some(&tnl_protocol::OpenStreamError::Capacity)
            );
            tnl_protocol::OpenStreamError::Capacity
        }
    };
    tokio::time::timeout(Duration::from_secs(3), daemon).await
        .expect("physical failure was misclassified as healthy control EOF, retaining the blocked socket").unwrap();
    assert!(
        release.try_recv().is_ok(),
        "bridge completion retained real-socket owner"
    );
    assert!(
        futures::poll!(Box::pin(termination.completed())).is_ready(),
        "driver IO retained after upgrade completion"
    );
    assert!(!state.session_handles.contains_key(&session_id));
    assert_eq!(termination.close_and_observe_cause().await, expected);
    drop(held);
    stop_relay.cancel();
    relay.await.unwrap();
}
