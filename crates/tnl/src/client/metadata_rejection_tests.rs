//! Observe intentional pre-registration rejection without manufacturing a yamux
//! session whose bootstrap writes could race the client's socket drop.
use async_tungstenite::tungstenite::{error::ProtocolError, Error, Message};
use futures::{Stream, StreamExt as _};

#[derive(Default)]
struct NoRegistration {
    pending: Vec<u8>,
    received: usize,
}

impl NoRegistration {
    fn receive(&mut self, bytes: &[u8], invalid_echo: bool) -> Result<(), &'static str> {
        self.received += bytes.len();
        if self.received > 4096 || (invalid_echo && !bytes.is_empty()) {
            return Err("unexpected transport bytes after rejected metadata echo");
        }
        self.pending.extend_from_slice(bytes);
        while self.pending.len() >= 12 {
            let header = &self.pending[..12];
            let flags = u16::from_be_bytes([header[2], header[3]]);
            let id = u32::from_be_bytes(header[4..8].try_into().unwrap());
            let value = u32::from_be_bytes(header[8..12].try_into().unwrap());
            // Only payload-free bootstrap/teardown for the CLI's even control
            // stream is permitted. Data length is checked at its HEADER, so
            // truncated/undelivered registration payload cannot escape detection.
            let permitted = header[0] == 0
                && match header[1] {
                    0 => id == 2 && matches!(flags, 0 | 1 | 4 | 5 | 8) && value == 0,
                    1 => id == 2 && matches!(flags, 0 | 1) && value <= 256 * 1024,
                    2 => id == 0 && flags == 1,
                    3 => id == 0 && flags == 0 && value == 0,
                    _ => false,
                };
            if !permitted {
                return Err("unexpected yamux header or registration payload");
            }
            self.pending.drain(..12);
        }
        Ok(())
    }

    fn finish(&self) -> Result<(), &'static str> {
        if self.pending.is_empty() {
            Ok(())
        } else {
            Err("rejected metadata left a partial yamux header")
        }
    }
}

pub(super) async fn observe(
    mut ws: impl Stream<Item = Result<Message, Error>> + Unpin,
    invalid_echo: bool,
) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut observer = NoRegistration::default();
        loop {
            match ws.next().await {
                Some(Ok(Message::Binary(bytes))) => observer.receive(&bytes, invalid_echo).unwrap(),
                Some(
                    Ok(Message::Close(_))
                    | Err(
                        Error::ConnectionClosed
                        | Error::Protocol(ProtocolError::ResetWithoutClosingHandshake),
                    ),
                )
                | None => break,
                Some(Err(Error::Io(error)))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
                    ) =>
                {
                    break
                }
                _ => panic!("unexpected WebSocket message/error during metadata rejection"),
            }
        }
        observer.finish().unwrap();
    })
    .await
    .expect("metadata rejection did not release its socket");
}

#[test]
fn rejection_observer_accepts_fragmented_bootstrap_but_rejects_payload_and_partial_headers() {
    let ping = [0, 2, 0, 1, 0, 0, 0, 0, 0, 0, 0, 42];
    let reset = [0, 0, 0, 8, 0, 0, 0, 2, 0, 0, 0, 0];
    let goaway = [0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let mut observer = NoRegistration::default();
    for byte in ping.into_iter().chain(reset).chain(goaway) {
        observer.receive(&[byte], false).unwrap();
    }
    observer.finish().unwrap();
    // Negative control: even a declared registration with NO delivered payload
    // must be rejected, as must any transport bytes after an invalid echo.
    let registration = [0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1];
    assert!(NoRegistration::default()
        .receive(&registration, false)
        .is_err());
    assert!(NoRegistration::default().receive(&ping, true).is_err());
    for bad in [
        [1, 2, 0, 1, 0, 0, 0, 0, 0, 0, 0, 42],
        [0, 2, 0, 8, 0, 0, 0, 0, 0, 0, 0, 42],
        [0, 0, 0, 8, 0, 0, 0, 3, 0, 0, 0, 0],
    ] {
        assert!(NoRegistration::default().receive(&bad, false).is_err());
    }
    let mut partial = NoRegistration::default();
    partial.receive(&ping[..3], false).unwrap();
    assert!(partial.finish().is_err());
}
