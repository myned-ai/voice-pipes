use super::*;

#[test]
fn parse_valid_chunk() {
    let line =
        r#"data: {"id":"chatcmpl-1","choices":[{"delta":{"content":"Hi"},"finish_reason":null}]}"#;
    let chunk = parse_sse_line(line).unwrap().into_chunk().unwrap();
    assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("Hi"));
}

#[test]
fn parse_done_sentinel() {
    assert!(matches!(
        parse_sse_line("data: [DONE]").unwrap(),
        SseLine::Done
    ));
}

#[test]
fn parse_empty_line() {
    assert!(matches!(parse_sse_line("").unwrap(), SseLine::Skip));
    assert!(matches!(parse_sse_line("  ").unwrap(), SseLine::Skip));
}

#[test]
fn parse_comment_line() {
    assert!(matches!(
        parse_sse_line(": keep-alive").unwrap(),
        SseLine::Skip
    ));
}

#[test]
fn parse_event_line() {
    assert!(matches!(
        parse_sse_line("event: message").unwrap(),
        SseLine::Skip
    ));
}

#[test]
fn parse_invalid_json() {
    let result = parse_sse_line("data: {invalid}");
    assert!(result.is_err());
}

#[test]
fn parse_chunk_with_finish_reason() {
    let line = r#"data: {"id":"chatcmpl-1","choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":10,"total_tokens":15}}"#;
    let chunk = parse_sse_line(line).unwrap().into_chunk().unwrap();
    assert_eq!(chunk.choices[0].finish_reason.as_deref(), Some("stop"));
    assert!(chunk.choices[0].delta.content.is_none());
    assert!(chunk.usage.is_some());
}

#[test]
fn parse_data_with_extra_whitespace() {
    let line = "data:   {\"id\":\"x\",\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}  ";
    let chunk = parse_sse_line(line).unwrap().into_chunk().unwrap();
    assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("ok"));
}

/// However the stream is chunked, the same lines come out: a multi-byte
/// character split across chunks, `\r\n` line ends and blank separator lines
/// included.
#[test]
fn sse_lines_are_the_same_whatever_the_chunking() {
    let stream = "data: {\"a\":\"日本語 🎤 é\"}\r\n\ndata: [DONE]\n".as_bytes();
    let lines = voice_pipes_sdk::testing::chunking_invariant(stream, |chunks| {
        let mut reader = SseReader::new(1024);
        chunks
            .iter()
            .flat_map(|chunk| reader.push(chunk).expect("within the bound"))
            .collect::<Vec<_>>()
    });
    assert_eq!(lines, ["data: {\"a\":\"日本語 🎤 é\"}", "", "data: [DONE]"]);
}

/// Invalid bytes decode as U+FFFD within their line; they are not carried
/// into the next one.
#[test]
fn invalid_bytes_are_replaced_within_their_line() {
    let mut reader = SseReader::new(1024);
    assert_eq!(reader.push(b"ok\xFFok\n").unwrap(), ["ok\u{FFFD}ok"]);
    assert_eq!(reader.push(&[0xE6, 0x97, b'\n']).unwrap(), ["\u{FFFD}"]);
    assert_eq!(reader.push(b"x\n").unwrap(), ["x"]);
}

/// A stream that goes past the bound without a line end is refused.
#[test]
fn a_line_past_the_bound_is_refused() {
    let mut reader = SseReader::new(8);
    assert!(reader.push(b"12345678").unwrap().is_empty());
    assert!(reader.push(b"9").is_err());
}

/// A vendor error sent in the stream is its own kind of line, carrying
/// the vendor's message, not a parse failure to be skipped.
#[test]
fn an_error_frame_is_an_error_line() {
    let line = r#"data: {"error":{"message":"The server had an error","type":"server_error"}}"#;
    match parse_sse_line(line) {
        Ok(SseLine::Error(e)) => assert_eq!(e.message, "The server had an error"),
        other => panic!("expected an error line, got {other:?}"),
    }
}

/// A line that is neither a chunk nor an error is refused without its
/// text: a malformed chunk can carry the reply, which stays out of logs.
#[test]
fn a_parse_failure_does_not_carry_the_line() {
    let err = parse_sse_line(r#"data: {"choices": "my secret reply"}"#).unwrap_err();
    assert!(!err.to_string().contains("secret"), "{err}");
}
