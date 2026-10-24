use super::*;
use crate::transport::{MessageRole, PipelineMessage};

#[tokio::test]
async fn send_recv_client_to_server() {
    let (client, mut server) = channel_pair();

    let msg = ClientMessage::Message(PipelineMessage {
        kind: "text".into(),
        text: Some("hello".into()),
        payload: None,
        binary: None,
        role: MessageRole::User,
        turn_id: None,
        message_id: "1".into(),
    });
    client.send_client(msg).await.unwrap();

    let received = server.recv().await.unwrap().unwrap();
    match received {
        ClientMessage::Message(m) => assert_eq!(m.text.as_deref(), Some("hello")),
        other => panic!("expected Message, got {other:?}"),
    }
}

#[tokio::test]
async fn send_recv_server_to_client() {
    let (client, server) = channel_pair();

    let msg = ServerMessage::Message(PipelineMessage {
        kind: "text".into(),
        text: Some("world".into()),
        payload: None,
        binary: None,
        role: MessageRole::Assistant,
        turn_id: None,
        message_id: "2".into(),
    });
    server.send(msg).await.unwrap();

    let received = client.recv_server().await.unwrap().unwrap();
    match received {
        ServerMessage::Message(m) => assert_eq!(m.text.as_deref(), Some("world")),
        other => panic!("expected Message, got {other:?}"),
    }
}

#[tokio::test]
async fn close_signals_none() {
    let (client, mut server) = channel_pair();
    client.close("done").await.unwrap();

    let received = server.recv().await.unwrap();
    assert!(received.is_none());
}

#[tokio::test]
async fn send_after_close_fails() {
    let (client, _server) = channel_pair();
    client.close("done").await.unwrap();

    let result = client
        .send_client(ClientMessage::Control(
            crate::transport::ClientControl::Cancel,
        ))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn metadata_matches() {
    let (client, server) = channel_pair();
    assert_eq!(client.metadata().transport_type, TransportType::Direct);
    assert_eq!(server.metadata().transport_type, TransportType::Direct);
    assert_eq!(client.metadata().session_id, server.metadata().session_id);
}
