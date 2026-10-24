//! Loopback HTTP servers for provider tests: one copy of what each
//! provider's tests otherwise wrote for itself.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Read one request from `sock`: its head, lower-cased, and its body as
/// `Content-Length` gives it. `None` when the peer closes first.
pub async fn read_request(sock: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    let body_start = loop {
        let n = sock.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(tmp.get(..n)?);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
    };
    let head = String::from_utf8_lossy(buf.get(..body_start)?).to_ascii_lowercase();
    let len: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    let mut body = buf.get(body_start..)?.to_vec();
    while body.len() < len {
        let n = sock.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        body.extend_from_slice(tmp.get(..n)?);
    }
    Some((head, body))
}

/// Answer one request with `head` and then `chunks`, `gap` apart, as a
/// chunked body: a stream that lasts as long as its audio. Returns the
/// server's base URL.
///
/// # Panics
/// When no loopback port can be bound.
pub async fn drip_server(head: Vec<u8>, chunks: Vec<Vec<u8>>, gap: Duration) -> String {
    #[expect(
        clippy::expect_used,
        reason = "test support: no port is a broken test host"
    )]
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port");
    #[expect(
        clippy::expect_used,
        reason = "test support: a bound listener has an address"
    )]
    let addr = listener.local_addr().expect("the bound address");
    tokio::spawn(async move {
        let Ok((mut sock, _)) = listener.accept().await else {
            return;
        };
        let _ = read_request(&mut sock).await;
        let _ = sock
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await;
        for (i, chunk) in chunks.iter().enumerate() {
            let piece = if i == 0 {
                [head.as_slice(), chunk.as_slice()].concat()
            } else {
                chunk.clone()
            };
            let _ = sock
                .write_all(format!("{:x}\r\n", piece.len()).as_bytes())
                .await;
            let _ = sock.write_all(&piece).await;
            let _ = sock.write_all(b"\r\n").await;
            tokio::time::sleep(gap).await;
        }
        let _ = sock.write_all(b"0\r\n\r\n").await;
    });
    format!("http://{addr}")
}
