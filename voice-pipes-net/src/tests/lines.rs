use super::*;

/// Lines split the same way however the stream is chunked, with `\r\n`
/// accepted, and the unterminated tail handed back at the end.
#[test]
fn lines_come_back_whole_whatever_the_chunking() {
    let stream = b"{\"a\":1}\r\n{\"b\":2}\n\n{\"c\":3}";
    let (got, tail) = voice_pipes_sdk::testing::chunking_invariant(stream, |chunks| {
        let mut lines = BoundedLines::new(64);
        let mut got = Vec::new();
        for chunk in chunks {
            got.extend(lines.push(chunk).unwrap());
        }
        (got, lines.finish())
    });
    assert_eq!(
        got,
        vec![b"{\"a\":1}".to_vec(), b"{\"b\":2}".to_vec(), Vec::new()]
    );
    assert_eq!(tail, Some(b"{\"c\":3}".to_vec()));
}

/// A server that never sends a newline is cut off at the limit instead of
/// growing the buffer, and so is a complete line over the limit.
#[test]
fn a_line_past_the_limit_is_refused() {
    let mut lines = BoundedLines::new(8);
    assert_eq!(lines.push(b"12345678").unwrap(), Vec::<Vec<u8>>::new());
    assert_eq!(lines.push(b"9"), Err(LineTooLong { limit: 8 }));

    let mut lines = BoundedLines::new(8);
    assert_eq!(
        lines.push(b"123456789\nok\n"),
        Err(LineTooLong { limit: 8 })
    );

    let mut lines = BoundedLines::new(8);
    assert_eq!(
        lines.push(b"12345678\n").unwrap(),
        vec![b"12345678".to_vec()]
    );
    assert_eq!(lines.finish(), None);
}

/// The last line, unterminated, loses a trailing carriage return as every
/// terminated line does: a CRLF stream that ends without its final
/// newline gives the same text.
#[test]
fn the_last_line_drops_a_trailing_carriage_return() {
    let mut lines = BoundedLines::new(64);
    assert_eq!(lines.push(b"a\r\nb\r").unwrap(), vec![b"a".to_vec()]);
    assert_eq!(lines.finish(), Some(b"b".to_vec()));

    // Nothing but the carriage return is no line at all.
    let mut lines = BoundedLines::new(64);
    assert_eq!(lines.push(b"\r").unwrap(), Vec::<Vec<u8>>::new());
    assert_eq!(lines.finish(), None);
}
