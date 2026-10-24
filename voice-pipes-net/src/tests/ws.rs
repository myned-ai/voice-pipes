use std::time::Instant;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

use super::*;

/// A listener that takes the TCP connection and never answers the
/// upgrade: the connect gives up at its bound.
#[tokio::test]
async fn a_silent_endpoint_times_out_at_the_bound() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("ws://{}", listener.local_addr().expect("addr"));
    let held = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.expect("accept");
        tokio::time::sleep(Duration::from_secs(5)).await;
        drop(socket);
    });
    let started = Instant::now();
    let err = connect_websocket(url, 1024, Duration::from_millis(200))
        .await
        .expect_err("no upgrade answer");
    assert!(
        matches!(err, ConnectError::Timeout(t) if t == Duration::from_millis(200)),
        "{err:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    held.abort();
}

/// A connection opened with a message bound refuses a larger message.
#[tokio::test]
async fn an_open_connection_refuses_a_message_over_the_bound() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("ws://{}", listener.local_addr().expect("addr"));
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.expect("accept");
        let mut ws = tokio_tungstenite::accept_async(socket)
            .await
            .expect("upgrade");
        ws.send(Message::Text("small".into())).await.expect("send");
        ws.send(Message::Text("x".repeat(4096).into()))
            .await
            .expect("send");
        let _ = ws.next().await;
    });
    let (mut ws, _response) = connect_websocket(url, 1024, Duration::from_secs(5))
        .await
        .expect("connect");
    let first = ws.next().await.expect("a frame").expect("within the bound");
    assert_eq!(first, Message::Text("small".into()));
    assert!(
        matches!(ws.next().await, Some(Err(_))),
        "a 4 KiB message over a 1 KiB bound is refused"
    );
}

/// A refused connection is a handshake error, not a timeout.
#[tokio::test]
async fn a_refused_connection_is_a_handshake_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("ws://{}", listener.local_addr().expect("addr"));
    drop(listener);
    let err = connect_websocket(url, 1024, Duration::from_secs(5))
        .await
        .expect_err("nothing listens");
    assert!(matches!(err, ConnectError::Handshake(_)), "{err:?}");
}

/// The connection sends each frame as it is written: audio goes out in
/// 20 ms frames, and Nagle's algorithm would hold a small frame back until
/// the previous one is acknowledged.
#[tokio::test]
async fn a_connection_sends_without_delay() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("ws://{}", listener.local_addr().expect("addr"));
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.expect("accept");
        let mut ws = tokio_tungstenite::accept_async(socket)
            .await
            .expect("upgrade");
        let _ = ws.next().await;
    });
    let (ws, _response) = connect_websocket(url, 1024, Duration::from_secs(5))
        .await
        .expect("connect");
    let MaybeTlsStream::Plain(tcp) = ws.get_ref() else {
        panic!("a ws:// connection is plain TCP");
    };
    assert!(tcp.nodelay().expect("read TCP_NODELAY"));
}
