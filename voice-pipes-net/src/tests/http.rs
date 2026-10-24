use super::*;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Serve one HTTP response per connection, forever, from a local port.
async fn serve(response: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0u8; 2048];
            let _ = sock.read(&mut buf).await;
            let _ = sock.write_all(response.as_bytes()).await;
        }
    });
    format!("http://{addr}")
}

/// A redirect is returned to the caller, never followed: following one could
/// carry a custom key header to another host.
#[tokio::test]
async fn redirects_are_not_followed() {
    let base = serve(
        "HTTP/1.1 302 Found\r\nLocation: http://attacker.invalid/steal\r\nContent-Length: 0\r\n\r\n",
    )
    .await;
    let client = http_client(&HttpOptions::request(Duration::from_secs(5))).unwrap();
    let resp = client
        .get(format!("{base}/v1/tts"))
        .header("xi-api-key", "sk-test")
        .send()
        .await
        .expect("the redirect response itself");
    assert_eq!(resp.status(), reqwest::StatusCode::FOUND);
}

/// The whole-request timeout bounds a server that never answers.
#[tokio::test]
async fn a_request_timeout_bounds_a_silent_server() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (_sock, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    let client = http_client(&HttpOptions::request(Duration::from_millis(200))).unwrap();
    let t0 = std::time::Instant::now();
    let err = client
        .get(format!("http://{addr}/"))
        .send()
        .await
        .unwrap_err();
    assert!(err.is_timeout(), "{err}");
    assert!(t0.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn a_streaming_client_completes_a_request() {
    let base = serve("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let client = http_client(&HttpOptions::streaming(Duration::from_secs(5))).unwrap();
    assert!(client.get(base).send().await.unwrap().status().is_success());
}

/// A chunked response that sends `chunks` pieces `gap` apart, then, if
/// `stall` is set, stops sending without closing.
async fn drip(chunks: usize, gap: Duration, stall: bool) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let Ok((mut sock, _)) = listener.accept().await else {
            return;
        };
        let mut buf = [0u8; 2048];
        let _ = sock.read(&mut buf).await;
        let _ = sock
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await;
        for _ in 0..chunks {
            let _ = sock.write_all(b"4\r\naudi\r\n").await;
            tokio::time::sleep(gap).await;
        }
        if stall {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        let _ = sock.write_all(b"0\r\n\r\n").await;
    });
    format!("http://{addr}")
}

/// Read a body to the end, as a consumer of streamed audio does.
async fn read_all(client: &reqwest::Client, url: String) -> Result<usize, reqwest::Error> {
    let mut response = client.get(url).send().await?;
    let mut total = 0;
    while let Some(chunk) = response.chunk().await? {
        total += chunk.len();
    }
    Ok(total)
}

/// A whole-request timeout ends a stream that is still arriving: the
/// reason streamed synthesis must not use one.
#[tokio::test]
async fn a_request_timeout_cuts_off_a_stream_still_arriving() {
    let base = drip(10, Duration::from_millis(100), false).await;
    let client = http_client(&HttpOptions::request(Duration::from_millis(300))).unwrap();
    let err = read_all(&client, base).await.unwrap_err();
    assert!(err.is_timeout(), "{err}");
}

/// A stream that keeps arriving is read to the end however long it lasts,
/// as long as no gap exceeds the idle bound.
#[tokio::test]
async fn a_streaming_client_reads_a_long_stream_to_the_end() {
    let base = drip(10, Duration::from_millis(100), false).await;
    let client = http_client(&HttpOptions::streaming(Duration::from_millis(300))).unwrap();
    assert_eq!(read_all(&client, base).await.unwrap(), 40);
}

/// A stream that stops arriving fails at the idle bound, not never.
#[tokio::test]
async fn a_streaming_client_fails_a_stalled_stream_at_the_idle_bound() {
    let base = drip(2, Duration::from_millis(10), true).await;
    let client = http_client(&HttpOptions::streaming(Duration::from_millis(200))).unwrap();
    let t0 = std::time::Instant::now();
    let err = read_all(&client, base).await.unwrap_err();
    assert!(err.is_timeout(), "{err}");
    assert!(t0.elapsed() < Duration::from_secs(5));
}

/// An error body is read only to the limit, so a server answering an error
/// with an endless or huge body cannot hold the provider or its memory.
#[tokio::test]
async fn an_error_body_is_read_only_to_the_limit() {
    let body = "e".repeat(100 * 1024);
    let response: &'static str = Box::leak(
        format!(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_boxed_str(),
    );
    let base = serve(response).await;
    let client = http_client(&HttpOptions::request(Duration::from_secs(5))).unwrap();
    let resp = client.get(format!("{base}/v1/tts")).send().await.unwrap();
    let text = error_body(resp).await;
    assert_eq!(text.len(), ERROR_BODY_LIMIT);
    assert!(text.chars().all(|c| c == 'e'));
}

/// A body within the limit comes back whole.
#[tokio::test]
async fn a_whole_body_within_the_limit_is_returned() {
    let base = serve("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello").await;
    let client = http_client(&HttpOptions::request(Duration::from_secs(5))).unwrap();
    let resp = client.get(&base).send().await.unwrap();
    assert_eq!(limited_body(resp, 5).await.unwrap(), b"hello");
}

/// A body past the limit is refused, not truncated.
#[tokio::test]
async fn a_body_past_the_limit_is_refused() {
    let base = serve("HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n0123456789").await;
    let client = http_client(&HttpOptions::request(Duration::from_secs(5))).unwrap();
    let resp = client.get(&base).send().await.unwrap();
    assert!(matches!(
        limited_body(resp, 4).await,
        Err(BodyError::TooLarge { limit: 4 })
    ));
}

/// A connection that drops part way through the body is a read failure; the
/// part that arrived is never returned as if it were the whole body.
#[tokio::test]
async fn a_body_cut_off_mid_read_is_a_read_error() {
    let base = serve("HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n{\"partial\":").await;
    let client = http_client(&HttpOptions::request(Duration::from_secs(5))).unwrap();
    let resp = client.get(&base).send().await.unwrap();
    assert!(matches!(
        limited_body(resp, 4096).await,
        Err(BodyError::Read(_))
    ));
}

/// A credential header is marked sensitive: left out of debug output and
/// out of HTTP/2's compression table.
#[test]
fn a_credential_header_is_sensitive() {
    let header = sensitive_header("sk-secret").unwrap();
    assert!(header.is_sensitive());
    assert!(!format!("{header:?}").contains("sk-secret"));
    assert!(sensitive_header("bad\nvalue").is_err());
}
