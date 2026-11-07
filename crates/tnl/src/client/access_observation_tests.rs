//! Composition boundary: real protected control + data plane + raw consumer.
use super::*;
use crate::inspection::{
    CaptureContext, ConsumerFactory, Inspection, Observation, ObservationConsumer, PreviewMode,
    TunnelGeneration,
};
use crate::replay::{AvailabilityState, ReplayController};
use std::sync::Mutex;

struct SeenHead {
    context: CaptureContext,
    request_id: String,
    authorization: Option<String>,
}
struct BoundaryFactory {
    replay: ReplayController,
    heads: Arc<Mutex<Vec<SeenHead>>>,
}
struct BoundaryConsumer {
    context: CaptureContext,
    raw: Box<dyn ObservationConsumer>,
    heads: Arc<Mutex<Vec<SeenHead>>>,
}
impl ConsumerFactory for BoundaryFactory {
    fn admit(&self, context: CaptureContext) -> Option<Box<dyn ObservationConsumer>> {
        Some(Box::new(BoundaryConsumer {
            context,
            raw: self.replay.admit(context)?,
            heads: self.heads.clone(),
        }))
    }
}
impl ObservationConsumer for BoundaryConsumer {
    fn observe(&mut self, event: &Observation<'_>) {
        if let Observation::RequestHead { headers, .. } = event {
            let values = |name: &str| -> Vec<_> {
                headers
                    .iter()
                    .filter(|h| h.name.eq_ignore_ascii_case(name))
                    .map(|h| std::str::from_utf8(h.value).unwrap().to_owned())
                    .collect()
            };
            let ids = values("x-tnl-request-id");
            assert_eq!(ids.len(), 1);
            let auth = values("authorization");
            assert!(auth.len() <= 1);
            let mut heads = self.heads.lock().unwrap();
            assert!(heads.len() < 2);
            heads.push(SeenHead {
                context: self.context,
                request_id: ids[0].clone(),
                authorization: auth.into_iter().next(),
            });
        }
        self.raw.observe(event);
    }
}

async fn connect(fixture: &Fixture, options: &TunnelOptions) -> ConnectedSession {
    if options.access.is_some() {
        return connect_and_create_with_connector(
            &fixture.endpoint,
            "tnl_TEST",
            Some("guarded"),
            options,
            Some(fixture.connector.clone()),
        )
        .await
        .unwrap();
    }
    // Public control uses the same certificate-verified test edge, not a
    // plaintext shortcut or a verifier-disabled TLS connector.
    let (session, mut control, data_stream_protocol) = dial_and_open_control_with_connector(
        &fixture.endpoint,
        "tnl_TEST",
        Some(fixture.connector.clone()),
    )
    .await
    .unwrap();
    write_msg(
        &mut control,
        &ControlMsg::CreateTunnel(CreateTunnelReq {
            subdomain: Some("guarded".into()),
        }),
    )
    .await
    .unwrap();
    let ControlMsg::TunnelCreated(ack) = read_msg(&mut control).await.unwrap() else {
        panic!("expected public ACK");
    };
    ConnectedSession {
        session,
        control,
        data_stream_protocol,
        tunnel_id: ack.tunnel_id,
        hostname: ack.hostname,
        subdomain: ack.subdomain,
    }
}

fn response_id(headers: &http::HeaderMap) -> String {
    assert_eq!(headers.get_all("x-tnl-request-id").iter().count(), 1);
    let id = headers["x-tnl-request-id"].to_str().unwrap().to_owned();
    assert_ne!(id, "00000000000000000000000000");
    assert!(id.parse::<ulid::Ulid>().is_ok());
    id
}

async fn assert_capture(
    fixture: &mut Fixture,
    history: &Inspection,
    heads: &Arc<Mutex<Vec<SeenHead>>>,
    logs: &mut tokio::sync::mpsc::Receiver<crate::inspector::LogLine>,
    id: &str,
    authorization: Option<&str>,
    status: u16,
) -> crate::inspection::InspectionRecord {
    let backend = fixture.seen.recv().await.unwrap();
    let id_line = format!("x-tnl-request-id: {id}\r\n");
    assert!(backend
        .to_ascii_lowercase()
        .contains(&id_line.to_ascii_lowercase()));
    let log = logs.recv().await.unwrap();
    assert_eq!(log.req_id.to_string(), id);
    assert_eq!(log.status, Some(status));
    let record = history
        .search("")
        .into_iter()
        .find(|r| {
            r.request_id
                .is_some_and(|request_id| request_id.to_string() == id)
        })
        .unwrap();
    assert_eq!(record.visitor_source, Some("peer"));
    assert_eq!(record.status, Some(status));
    let heads = heads.lock().unwrap();
    let head = heads
        .iter()
        .find(|h| h.context.capture_id == record.capture_id)
        .unwrap();
    assert_eq!(head.request_id, id);
    assert_eq!(head.context.request_id.unwrap().to_string(), id);
    assert_eq!(head.context.backend, Some(fixture.backend));
    assert_eq!(head.authorization.as_deref(), authorization);
    drop(heads);
    if let Some(authorization) = authorization {
        assert!(backend.contains(authorization));
    } else {
        assert!(!backend.to_ascii_lowercase().contains("authorization:"));
    }
    assert!(!backend.contains("VISITOR_SENTINEL"));
    assert!(!serde_json::to_string(&record).unwrap().contains("SENTINEL"));
    record
}

#[allow(clippy::too_many_lines)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_access_http_ws_ids_and_raw_capture_obey_credential_boundary() {
    tokio::time::timeout(Duration::from_secs(30), async {
        for mode in 0..3 {
            let mut fixture = fixture().await;
            let options = if mode == 2 {
                TunnelOptions::default()
            } else {
                options(mode == 0)
            };
            let connected = connect(&fixture, &options).await;
            assert_eq!(
                connected.data_stream_protocol,
                DataStreamProtocol::Http1MetadataV1
            );
            let generation = TunnelGeneration::new();
            let replay = ReplayController::new(true, 65535).unwrap();
            replay.activate(generation);
            let heads = Arc::new(Mutex::new(Vec::new()));
            let history = Inspection::with_consumer(
                PreviewMode::Off,
                Some(Arc::new(BoundaryFactory {
                    replay: replay.clone(),
                    heads: heads.clone(),
                })),
            );
            history.activate(generation);
            let (tx, mut logs) = tokio::sync::mpsc::channel(4);
            let mut ctx = crate::forwarder::ForwardCtx::new(
                "guarded".into(),
                Some(tx),
                env!("CARGO_PKG_VERSION"),
            );
            ctx.data_stream_protocol = connected.data_stream_protocol;
            ctx.inspection = Some((history.clone(), generation));
            let backend = fixture.backend;
            let accept = tokio::spawn(async move {
                let _control = connected.control;
                let _ = run_accept_loop(
                    connected.session,
                    crate::target::Target::Explicit(backend),
                    ctx,
                )
                .await;
            });
            if mode == 0 {
                let denied = reqwest::Client::new()
                    .get(format!("http://{}/denied", fixture.server.local_addr))
                    .header("host", "guarded.t.x")
                    .send()
                    .await
                    .unwrap();
                assert_eq!(denied.status(), 401);
                assert_eq!(fixture.count.load(Ordering::SeqCst), 0);
                assert!(history.search("").is_empty());
                assert!(heads.lock().unwrap().is_empty());
            }
            let credential = if mode == 0 {
                "Basic dXNlcjpWSVNJVE9SX1NFTlRJTkVM"
            } else {
                "Bearer APP_SENTINEL"
            };
            let expected_auth = (mode != 0).then_some(credential);
            let response = reqwest::Client::new()
                .get(format!("http://{}/app", fixture.server.local_addr))
                .header("host", "guarded.t.x")
                .header("authorization", credential)
                .header("connection", "close, X-Tnl-Request-Id")
                .header("x-tnl-request-id", "visitor-forgery")
                .header("X-Tnl-Request-Id", "00000000000000000000000000")
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            let http_id = response_id(response.headers());
            assert_eq!(response.text().await.unwrap(), "ok");
            let http = assert_capture(
                &mut fixture,
                &history,
                &heads,
                &mut logs,
                &http_id,
                expected_auth,
                200,
            )
            .await;
            assert_eq!(
                replay.availability(http.capture_id).state,
                AvailabilityState::Eligible
            );
            let request = http::Request::builder()
                .uri(format!("ws://{}/socket", fixture.server.local_addr))
                .header("host", "guarded.t.x")
                .header("authorization", credential)
                .header("connection", "upgrade, X-Tnl-Request-Id")
                .header("upgrade", "websocket")
                .header("sec-websocket-key", generate_key())
                .header("sec-websocket-version", "13")
                .header("x-tnl-request-id", "visitor-forgery")
                .header("X-Tnl-Request-Id", "00000000000000000000000000")
                .body(())
                .unwrap();
            let (mut ws, response) = async_tungstenite::tokio::connect_async(request)
                .await
                .unwrap();
            assert_eq!(response.status(), 101);
            let ws_id = response_id(response.headers());
            assert_ne!(ws_id, http_id);
            let payload =
                async_tungstenite::tungstenite::Message::Binary(vec![0, 255, 128, 1].into());
            ws.send(payload.clone()).await.unwrap();
            assert_eq!(ws.next().await.unwrap().unwrap(), payload);
            ws.close(None).await.unwrap();
            drop(ws);
            let upgrade = assert_capture(
                &mut fixture,
                &history,
                &heads,
                &mut logs,
                &ws_id,
                expected_auth,
                101,
            )
            .await;
            assert_eq!(upgrade.state, crate::inspection::State::Upgraded);
            assert_eq!(
                replay.availability(upgrade.capture_id).state,
                AvailabilityState::Unavailable
            );
            assert_eq!(heads.lock().unwrap().len(), 2);
            accept.abort();
            drop(fixture);
        }
    })
    .await
    .expect("combined access/identity/capture exchange stalled");
}
