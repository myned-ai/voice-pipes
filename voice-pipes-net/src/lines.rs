//! Newline-delimited streams (NDJSON, line-oriented protocols) split with a
//! bound, so a server that never sends a newline cannot grow a buffer
//! without limit.

/// A line longer than the splitter's limit arrived; the stream is not
/// trustworthy past this point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a line exceeded {limit} bytes without a newline")]
pub struct LineTooLong {
    /// The limit that was exceeded, in bytes.
    pub limit: usize,
}

/// Splits a byte stream into lines of at most `max_line_bytes` each.
///
/// Feed each chunk as it arrives to [`push`](Self::push); complete lines come
/// back without their `\n` (a `\r` before it is dropped too). Each byte is
/// scanned once, however the stream is chunked.
#[derive(Debug)]
pub struct BoundedLines {
    buf: Vec<u8>,
    scanned: usize,
    max_line_bytes: usize,
}

impl BoundedLines {
    /// A splitter refusing any line longer than `max_line_bytes`.
    pub fn new(max_line_bytes: usize) -> Self {
        Self {
            buf: Vec::new(),
            scanned: 0,
            max_line_bytes,
        }
    }

    /// Append `bytes` and return every line they completed, in order.
    ///
    /// # Errors
    /// [`LineTooLong`] when a line (complete or still pending) passes the
    /// limit.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, LineTooLong> {
        self.buf.extend_from_slice(bytes);
        let mut lines = Vec::new();
        let mut start = 0;
        while let Some(offset) = self.buf[self.scanned..].iter().position(|&b| b == b'\n') {
            let end = self.scanned + offset;
            if end - start > self.max_line_bytes {
                return Err(self.too_long());
            }
            let mut line = self.buf[start..end].to_vec();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            lines.push(line);
            start = end + 1;
            self.scanned = start;
        }
        self.buf.drain(..start);
        self.scanned = self.buf.len();
        if self.buf.len() > self.max_line_bytes {
            return Err(self.too_long());
        }
        Ok(lines)
    }

    /// The last line, when the stream ended without a newline after it,
    /// with a trailing carriage return dropped as [`push`](Self::push) drops
    /// it from every other line.
    pub fn finish(mut self) -> Option<Vec<u8>> {
        if self.buf.last() == Some(&b'\r') {
            self.buf.pop();
        }
        (!self.buf.is_empty()).then_some(self.buf)
    }

    fn too_long(&self) -> LineTooLong {
        LineTooLong {
            limit: self.max_line_bytes,
        }
    }
}

#[cfg(test)]
#[path = "tests/lines.rs"]
mod tests;
