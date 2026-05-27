use std::time::Duration;

use tnl::peek::{peek_request_head, RequestHeadProbe};
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn captures_full_head_with_blank_line() {
    let (mut a, mut b) = tokio::io::duplex(8192);
    tokio::spawn(async move {
        a.write_all(b"GET / HTTP/1.1\r\nHost: x\r\nAccept: application/json\r\n\r\n")
            .await
            .unwrap();
        a.write_all(b"body bytes follow").await.unwrap();
    });
    let probe = peek_request_head(&mut b, 4096, Duration::from_secs(1))
        .await
        .unwrap();
    match probe {
        RequestHeadProbe::Complete { head, leftover } => {
            assert!(
                head.ends_with(b"\r\n\r\n"),
                "head: {:?}",
                String::from_utf8_lossy(&head)
            );
            assert!(head.contains_subslice(b"Accept: application/json"));
            assert_eq!(leftover, b"body bytes follow");
        }
        other => panic!("expected Complete, got {other:?}"),
    }
}

#[tokio::test]
async fn cap_exceeded_returns_capped() {
    let (mut a, mut b) = tokio::io::duplex(8192);
    tokio::spawn(async move {
        // 200 bytes of garbage with no blank line.
        let chunk = vec![b'X'; 200];
        a.write_all(&chunk).await.unwrap();
    });
    let probe = peek_request_head(&mut b, 128, Duration::from_millis(200))
        .await
        .unwrap();
    match probe {
        RequestHeadProbe::Capped(buf) => assert_eq!(buf.len(), 128),
        other => panic!("expected Capped, got {other:?}"),
    }
}

#[tokio::test]
async fn eof_before_blank_line_returns_eof() {
    let (mut a, mut b) = tokio::io::duplex(8192);
    tokio::spawn(async move {
        a.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n").await.unwrap();
        drop(a);
    });
    let probe = peek_request_head(&mut b, 4096, Duration::from_secs(1))
        .await
        .unwrap();
    assert!(matches!(probe, RequestHeadProbe::Eof(_)), "got {probe:?}");
}

// Tiny inherent-extension to make the `contains_subslice` call above readable
// without pulling memchr.
trait ContainsSubslice {
    fn contains_subslice(&self, needle: &[u8]) -> bool;
}
impl ContainsSubslice for Vec<u8> {
    fn contains_subslice(&self, needle: &[u8]) -> bool {
        self.windows(needle.len()).any(|w| w == needle)
    }
}
