use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn gateway_auth_rotates_keys_and_preserves_stream_and_status() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = upstream.local_addr().unwrap();
    let (sent, mut received) = tokio::sync::mpsc::channel(4);
    let server = tokio::spawn(async move {
        for _ in 0..3 {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut byte = [0]; stream.read_exact(&mut byte).await.unwrap(); request.push(byte[0]);
                if request.ends_with(b"\r\n\r\n") { break; }
            }
            sent.send(String::from_utf8(request).unwrap()).await.unwrap();
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n9\r\ndata: a\n\n\r\n0\r\n\r\n").await.unwrap();
        }
    });
    let gateway = GatewayServer::start(format!("http://{address}/v1"), Protocol::OpenaiChat,
        vec![Secret::new("first-key"), Secret::new("second-key")], vec![("X-Provider".into(), Secret::new("header-secret"))], AuthMode::ApiKey).await.unwrap();
    let client = reqwest::Client::new();
    let url = format!("{}/chat/completions", gateway.base_url());
    assert_eq!(client.post(&url).send().await.unwrap().status().as_u16(), 401);
    for expected in ["first-key", "second-key", "first-key"] {
        let response = client.post(&url).bearer_auth(gateway.token()).send().await.unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let mut chunks = response.bytes_stream();
        let first = tokio::time::timeout(std::time::Duration::from_secs(2), chunks.next()).await.unwrap().unwrap().unwrap();
        assert_eq!(first.as_ref(), b"data: a\n\n");
        let request = received.recv().await.unwrap().to_lowercase();
        assert!(request.starts_with("post /v1/chat/completions "));
        assert!(request.contains(&format!("authorization: bearer {expected}")));
        assert!(request.contains("x-provider: header-secret"));
        assert!(!request.contains(gateway.token()));
    }
    server.await.unwrap();
    let address = gateway.base_url().split('/').nth(2).unwrap().to_owned();
    drop(gateway);
    tokio::task::yield_now().await;
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
}
