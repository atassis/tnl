use tnl_protocol::request_metadata::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn metadata() -> RequestMetadataV1 {
    RequestMetadataV1 {
        request_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        visitor: VisitorIdentity {
            visitor_ip: "2001:db8::7".parse().unwrap(),
            peer_ip: "127.0.0.1".parse().unwrap(),
            source: VisitorIpSource::XForwardedFor,
        },
    }
}

#[tokio::test]
async fn request_metadata_exact_frame_and_http_boundary() {
    let value = metadata();
    let mut bytes = Vec::new();
    write_request_metadata(&mut bytes, &value).await.unwrap();
    let json = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        &bytes[..4],
        &u32::try_from(json.len()).unwrap().to_be_bytes()
    );
    assert_eq!(&bytes[4..], json);
    bytes.extend_from_slice(b"POST / HTTP/1.1\r\n\r\nbody");
    let mut reader = bytes.as_slice();
    assert_eq!(read_request_metadata(&mut reader).await.unwrap(), value);
    assert_eq!(reader, b"POST / HTTP/1.1\r\n\r\nbody");
}

#[tokio::test]
async fn request_metadata_fragmented_frame() {
    let (mut writer, mut reader) = tokio::io::duplex(8);
    let value = metadata();
    let expected = value.clone();
    let task = tokio::spawn(async move {
        let mut frame = Vec::new();
        write_request_metadata(&mut frame, &value).await.unwrap();
        for byte in frame {
            writer.write_all(&[byte]).await.unwrap();
        }
    });
    assert_eq!(read_request_metadata(&mut reader).await.unwrap(), expected);
    task.await.unwrap();
}

#[tokio::test]
async fn request_metadata_rejects_lengths_before_payload_read() {
    for n in [0u32, 1025, u32::MAX] {
        let frame = n.to_be_bytes();
        assert!(matches!(
            read_request_metadata(&mut frame.as_slice()).await,
            Err(RequestMetadataError::Length(_))
        ));
    }
    for frame in [vec![], vec![0, 0], vec![0, 0, 0, 2, b'{']] {
        assert!(read_request_metadata(&mut frame.as_slice()).await.is_err());
    }
}

#[tokio::test]
async fn request_metadata_rejects_invalid_typed_payloads() {
    let good = serde_json::to_string(&metadata()).unwrap();
    let invalid = [
        "{}".into(),
        "not json".into(),
        good.replace("2001:db8::7", "not-ip"),
        good.replace("x_forwarded_for", "unknown"),
        format!(
            "{{\"visitor\":{},{}",
            serde_json::to_string(&metadata().visitor).unwrap(),
            &good[1..]
        ),
        good.replace("\"source\":", "\"source\":\"peer\",\"source\":"),
        good.replace("01ARZ3NDEKTSV4RRFFQ69G5FAV", "invalid-ulid"),
    ];
    for json in invalid {
        let mut frame = u32::try_from(json.len()).unwrap().to_be_bytes().to_vec();
        frame.extend_from_slice(json.as_bytes());
        assert!(
            read_request_metadata(&mut frame.as_slice()).await.is_err(),
            "{json}"
        );
    }
    let base = good.replace("{\"request_id\"", "{\"padding\":\"\",\"request_id\"");
    let padding = "a".repeat(MAX_REQUEST_METADATA_BYTES - base.len());
    let additive = base.replace("\"padding\":\"\"", &format!("\"padding\":\"{padding}\""));
    assert_eq!(additive.len(), MAX_REQUEST_METADATA_BYTES);
    let mut frame = u32::try_from(additive.len())
        .unwrap()
        .to_be_bytes()
        .to_vec();
    frame.extend_from_slice(additive.as_bytes());
    assert_eq!(
        read_request_metadata(&mut frame.as_slice()).await.unwrap(),
        metadata()
    );
}

#[tokio::test]
async fn request_metadata_reader_does_not_consume_http() {
    let mut frame = Vec::new();
    write_request_metadata(&mut frame, &metadata())
        .await
        .unwrap();
    frame.extend_from_slice(b"HTTP");
    let mut reader = frame.as_slice();
    read_request_metadata(&mut reader).await.unwrap();
    let mut rest = String::new();
    reader.read_to_string(&mut rest).await.unwrap();
    assert_eq!(rest, "HTTP");
}

#[tokio::test]
async fn request_metadata_rejects_noncanonical_or_nil_request_ids() {
    let visitor = serde_json::to_string(&metadata().visitor).unwrap();
    for id in [
        "null",
        "42",
        "[]",
        "\"invalid\"",
        "\"00000000000000000000000000\"",
        "\"01arz3ndektsv4rrffq69g5fav\"",
        "\"81ARZ3NDEKTSV4RRFFQ69G5FAV\"",
        "\"01ARZ3NDEKTSV4RRFFQ69G5FAI\"",
        "\" 01ARZ3NDEKTSV4RRFFQ69G5FAV\"",
        "\"1ARZ3NDEKTSV4RRFFQ69G5FAV\"",
    ] {
        let json = format!("{{\"request_id\":{id},\"visitor\":{visitor}}}");
        let mut frame = u32::try_from(json.len()).unwrap().to_be_bytes().to_vec();
        frame.extend_from_slice(json.as_bytes());
        assert!(
            read_request_metadata(&mut frame.as_slice()).await.is_err(),
            "accepted {id}"
        );
    }
    for json in [
        format!("{{\"visitor\":{visitor}}}"),
        format!("{{\"request_id\":\"01ARZ3NDEKTSV4RRFFQ69G5FAV\",\"request_id\":\"01ARZ3NDEKTSV4RRFFQ69G5FAV\",\"visitor\":{visitor}}}"),
    ] {
        let mut frame = u32::try_from(json.len()).unwrap().to_be_bytes().to_vec();
        frame.extend_from_slice(json.as_bytes());
        assert!(read_request_metadata(&mut frame.as_slice()).await.is_err());
    }
    let mut invalid = metadata();
    invalid.request_id = ulid::Ulid::nil();
    let mut bytes = Vec::new();
    assert!(write_request_metadata(&mut bytes, &invalid).await.is_err());
    assert!(
        bytes.is_empty(),
        "invalid typed values must fail before writing"
    );
}
