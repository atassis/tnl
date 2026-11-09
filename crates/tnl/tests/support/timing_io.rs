use tokio::io::{AsyncRead, AsyncReadExt};

pub async fn read_head(reader: &mut (impl AsyncRead + Unpin)) -> Vec<u8> {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        assert!(head.len() < 8192, "fixture head overflow");
        head.push(reader.read_u8().await.unwrap());
    }
    head
}
