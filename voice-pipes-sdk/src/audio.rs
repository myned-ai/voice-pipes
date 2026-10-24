//! Core audio frame type shared between the SDK and the framework, and the
//! byte-stream helpers a provider that receives audio over the network needs:
//! [`PcmFramer`] for 16-bit PCM that arrives in arbitrary chunks, and
//! [`WavStripper`] for a stream that may start with a WAV header, and
//! [`resample`] for sample-rate conversion.

use serde::Serialize;

pub mod resample;

/// A captured audio buffer — the unit passed to [`super::stt::SttProvider::transcribe`].
#[derive(Debug, Clone, Serialize)]
pub struct AudioFrame {
    /// PCM i16 samples (interleaved when `channels > 1`).
    pub samples: Vec<i16>,
    /// Sample rate in Hz (typically 16 000 for STT providers).
    pub sample_rate: u32,
    /// Number of audio channels (1 = mono, 2 = stereo).
    pub channels: u16,
}

// ── PCM framing ───────────────────────────────────────────────

/// Turns a stream of 16-bit little-endian mono PCM bytes, chunked however the
/// network delivered it, into [`AudioFrame`]s of a fixed size.
///
/// A sample split across two chunks is carried over; complete frames are
/// taken with [`take_frames`](Self::take_frames) as they fill, and the short
/// last one with [`finish`](Self::finish).
///
/// ```
/// use voice_pipes_sdk::audio::PcmFramer;
///
/// let mut pcm = PcmFramer::new(24_000, 2);
/// pcm.push(&[1, 0, 2]); // one sample and half of the next
/// pcm.push(&[0, 3, 0]);
/// let frames = pcm.take_frames();
/// assert_eq!(frames[0].samples, vec![1, 2]);
/// assert_eq!(pcm.finish().unwrap().samples, vec![3]);
/// assert_eq!(pcm.samples_out(), 3);
/// ```
#[derive(Debug)]
pub struct PcmFramer {
    carry: Option<u8>,
    pending: Vec<i16>,
    frame_samples: usize,
    sample_rate: u32,
    samples_out: usize,
    delivery_wait: std::time::Duration,
}

impl PcmFramer {
    /// Frames of `frame_samples` samples (at least one) at `sample_rate`.
    pub fn new(sample_rate: u32, frame_samples: usize) -> Self {
        let frame_samples = frame_samples.max(1);
        Self {
            carry: None,
            pending: Vec::with_capacity(frame_samples),
            frame_samples,
            sample_rate,
            samples_out: 0,
            delivery_wait: std::time::Duration::ZERO,
        }
    }

    /// Decode `bytes`, and return the samples this call completed, for a
    /// caller that inspects audio as it arrives (a silence probe, say).
    pub fn push(&mut self, bytes: &[u8]) -> &[i16] {
        let start = self.pending.len();
        let mut rest = bytes;
        if let Some(low) = self.carry
            && let [high, tail @ ..] = rest
        {
            self.pending.push(i16::from_le_bytes([low, *high]));
            self.carry = None;
            rest = tail;
        }
        let (pairs, remainder) = rest.as_chunks::<2>();
        self.pending
            .extend(pairs.iter().map(|pair| i16::from_le_bytes(*pair)));
        if let [byte] = *remainder {
            self.carry = Some(byte);
        }
        &self.pending[start..]
    }

    /// Every complete frame decoded so far, in order.
    pub fn take_frames(&mut self) -> Vec<AudioFrame> {
        let complete = self.pending.len() / self.frame_samples * self.frame_samples;
        if complete == 0 {
            return Vec::new();
        }
        let rest = self.pending.split_off(complete);
        let whole = std::mem::replace(&mut self.pending, rest);
        self.samples_out += whole.len();
        whole
            .chunks(self.frame_samples)
            .map(|chunk| self.frame(chunk.to_vec()))
            .collect()
    }

    /// The samples left over at the end of the stream, as one short frame.
    /// A dangling odd byte is half a sample and is dropped.
    pub fn finish(&mut self) -> Option<AudioFrame> {
        self.carry = None;
        if self.pending.is_empty() {
            return None;
        }
        let rest = std::mem::take(&mut self.pending);
        self.samples_out += rest.len();
        Some(self.frame(rest))
    }

    /// Send every complete frame on `tx`. `false` once the receiver has gone,
    /// so the caller can stop reading a stream nobody will play. Tracks
    /// time spent waiting on channel backpressure in [`delivery_wait`](Self::delivery_wait).
    pub async fn send_frames(&mut self, tx: &tokio::sync::mpsc::Sender<AudioFrame>) -> bool {
        for frame in self.take_frames() {
            let t0 = std::time::Instant::now();
            let res = tx.send(frame).await;
            self.delivery_wait += t0.elapsed();
            if res.is_err() {
                return false;
            }
        }
        true
    }

    /// Send the last short frame, if any, at the end of the stream. `false`
    /// when the receiver has gone. Tracks channel backpressure in [`delivery_wait`](Self::delivery_wait).
    pub async fn send_rest(&mut self, tx: &tokio::sync::mpsc::Sender<AudioFrame>) -> bool {
        match self.finish() {
            Some(frame) => {
                let t0 = std::time::Instant::now();
                let res = tx.send(frame).await.is_ok();
                self.delivery_wait += t0.elapsed();
                res
            }
            None => true,
        }
    }

    /// Cumulative time spent waiting on channel delivery backpressure while sending frames.
    pub fn delivery_wait(&self) -> std::time::Duration {
        self.delivery_wait
    }

    /// Samples handed out in frames so far.
    pub fn samples_out(&self) -> usize {
        self.samples_out
    }

    /// Samples decoded so far, handed out or not.
    pub fn samples_in(&self) -> usize {
        self.samples_out + self.pending.len()
    }

    fn frame(&self, samples: Vec<i16>) -> AudioFrame {
        AudioFrame {
            samples,
            sample_rate: self.sample_rate,
            channels: 1,
        }
    }
}

// ── WAV headers ───────────────────────────────────────────────

/// Longest WAV header searched for its `data` chunk. Real headers are 44
/// bytes, or a few hundred with metadata chunks.
pub const MAX_WAV_HEADER_BYTES: usize = 4096;

/// A stream that should have started with a WAV header did not have a usable
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WavError {
    /// The stream does not start with `RIFF....WAVE`, and a header was required.
    #[error("the audio stream is not WAV")]
    NotWav,
    /// No `data` chunk within [`MAX_WAV_HEADER_BYTES`].
    #[error("no WAV data chunk within {MAX_WAV_HEADER_BYTES} bytes")]
    NoDataChunk,
    /// The stream ended inside the WAV header, before any audio.
    #[error("the audio stream ended inside its WAV header")]
    Truncated,
}

/// A `data` chunk size at or above this is a placeholder, not a length: a
/// server streaming audio of unknown length writes 0 or a value near
/// `u32::MAX` there. 2 GiB is over twelve hours of 24 kHz speech.
const UNKNOWN_DATA_SIZE: u32 = 0x8000_0000;

/// Removes a WAV header from the front of a streamed response, however it is
/// chunked, and passes the PCM after it through.
///
/// [`new`](Self::new) also accepts a stream without a header (raw PCM passes
/// through unchanged), for servers that may send either;
/// [`requiring_header`](Self::requiring_header) refuses one, for a server
/// documented to always send WAV. The header's chunks are walked by their
/// declared sizes, so metadata chunks before `data` are skipped; a stream
/// whose `data` chunk does not start within [`MAX_WAV_HEADER_BYTES`] is an
/// error rather than a growing buffer. A `data` chunk with a real declared
/// size ends there, so a chunk after it is not played as audio; a
/// placeholder size (0, or a streaming server's near-`u32::MAX`) runs to the
/// end of the stream.
#[derive(Debug)]
pub struct WavStripper {
    state: WavState,
    require_header: bool,
}

#[derive(Debug)]
enum WavState {
    Probing(Vec<u8>),
    /// In the `data` chunk; `remaining` is what its declared size has left,
    /// `None` when the size was a placeholder.
    Wav {
        remaining: Option<u64>,
    },
    Raw,
}

impl Default for WavStripper {
    fn default() -> Self {
        Self::new()
    }
}

impl WavStripper {
    /// Strip a header when there is one; pass raw PCM through otherwise.
    pub fn new() -> Self {
        Self {
            state: WavState::Probing(Vec::new()),
            require_header: false,
        }
    }

    /// Strip the header, and refuse a stream that has none.
    pub fn requiring_header() -> Self {
        Self {
            require_header: true,
            ..Self::new()
        }
    }

    /// The PCM bytes `bytes` carries: empty while the header is still
    /// arriving, the bytes after it once it has, and nothing past the end of
    /// a `data` chunk whose size was declared.
    ///
    /// # Errors
    /// [`WavError`] when the stream cannot be a WAV this stripper accepts.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<u8>, WavError> {
        let buf = match &mut self.state {
            WavState::Raw => return Ok(bytes.to_vec()),
            WavState::Wav { remaining } => return Ok(take_declared(remaining, bytes)),
            WavState::Probing(buf) => buf,
        };
        buf.extend_from_slice(bytes);
        if buf.len() < 12 {
            return Ok(Vec::new());
        }
        if &buf[0..4] != b"RIFF" || &buf[8..12] != b"WAVE" {
            if self.require_header {
                return Err(WavError::NotWav);
            }
            let pcm = std::mem::take(buf);
            self.state = WavState::Raw;
            return Ok(pcm);
        }
        match data_chunk(buf) {
            Some((offset, size)) => {
                let mut remaining =
                    (size != 0 && size < UNKNOWN_DATA_SIZE).then_some(u64::from(size));
                let pcm = take_declared(&mut remaining, &buf[offset..]);
                self.state = WavState::Wav { remaining };
                Ok(pcm)
            }
            None if buf.len() > MAX_WAV_HEADER_BYTES => Err(WavError::NoDataChunk),
            None => Ok(Vec::new()),
        }
    }

    /// Whether a WAV header was found and removed.
    pub fn stripped_header(&self) -> bool {
        matches!(self.state, WavState::Wav { .. })
    }

    /// At the end of the stream: bytes still held while deciding, which are
    /// PCM when the stream was too short to carry a header and none was
    /// required.
    ///
    /// # Errors
    /// [`WavError::Truncated`] when the stream ended inside a WAV header (it
    /// began `RIFF....WAVE` but its `data` chunk never came), or, when a
    /// header was required, after fewer bytes than one. A stream that sent
    /// nothing at all is not an error here; the caller decides what no
    /// audio means.
    pub fn finish(self) -> Result<Vec<u8>, WavError> {
        match self.state {
            WavState::Probing(buf) if buf.is_empty() => Ok(buf),
            WavState::Probing(buf) if buf.len() < 12 && !self.require_header => Ok(buf),
            WavState::Probing(_) => Err(WavError::Truncated),
            WavState::Wav { .. } | WavState::Raw => Ok(Vec::new()),
        }
    }
}

/// The part of `bytes` inside the `data` chunk, counting down what its
/// declared size has left; all of it when the size is unknown.
fn take_declared(remaining: &mut Option<u64>, bytes: &[u8]) -> Vec<u8> {
    match remaining {
        None => bytes.to_vec(),
        Some(left) => {
            let take = usize::try_from(*left).map_or(bytes.len(), |l| l.min(bytes.len()));
            *left -= take as u64;
            bytes[..take].to_vec()
        }
    }
}

/// Where the `data` chunk's samples start and its declared size, walking the
/// chunks after the 12-byte RIFF header by their declared sizes (padded to
/// even, as RIFF requires). `None` until the `data` chunk's own header has
/// arrived, or when the walk leaves the header limit.
fn data_chunk(buf: &[u8]) -> Option<(usize, u32)> {
    let mut pos = 12usize;
    while pos <= MAX_WAV_HEADER_BYTES {
        let header = buf.get(pos..pos.checked_add(8)?)?;
        let (id, size) = header.split_at(4);
        let size = u32::from_le_bytes(size.try_into().ok()?);
        if id == b"data" {
            return Some((pos + 8, size));
        }
        let size = size as usize;
        pos = pos
            .saturating_add(8)
            .saturating_add(size)
            .saturating_add(size & 1);
    }
    None
}

/// 16-bit PCM as `f32` in `[-1.0, 1.0)`, the scale ONNX audio models take.
pub fn i16_to_f32(samples: &[i16]) -> Vec<f32> {
    samples.iter().map(|&s| f32::from(s) / 32768.0).collect()
}

/// 16-bit PCM as the little-endian bytes a network API takes (raw
/// `pcm_s16le`), two per sample.
///
/// ```
/// use voice_pipes_sdk::audio::i16_to_le_bytes;
///
/// assert_eq!(i16_to_le_bytes(&[1, -2]), vec![0x01, 0x00, 0xFE, 0xFF]);
/// ```
pub fn i16_to_le_bytes(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

/// `f32` audio, as an ONNX speech model produces it, as 16-bit PCM: each
/// sample clamped to `[-1.0, 1.0]` and scaled by `i16::MAX`, so a model
/// that overshoots clips rather than wraps, and NaN is silence.
///
/// ```
/// use voice_pipes_sdk::audio::f32_to_i16;
///
/// assert_eq!(f32_to_i16(&[0.0, 1.0, -2.0]), vec![0, i16::MAX, -i16::MAX]);
/// ```
pub fn f32_to_i16(samples: &[f32]) -> Vec<i16> {
    samples.iter().map(|&s| sample_to_i16(s)).collect()
}

/// One sample of [`f32_to_i16`].
#[expect(
    clippy::cast_possible_truncation,
    reason = "clamped to the i16 range first, and a float to int cast saturates"
)]
fn sample_to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16
}

/// The duration in seconds of `samples` mono or interleaved PCM samples at `rate` Hz with `channels`.
/// A rate of 0 returns 0.0 s.
///
/// ```
/// use voice_pipes_sdk::audio::audio_duration_seconds;
///
/// assert_eq!(audio_duration_seconds(32_000, 16_000, 1), 2.0);
/// assert_eq!(audio_duration_seconds(32_000, 16_000, 2), 1.0);
/// ```
#[must_use]
pub fn audio_duration_seconds(samples: u64, rate: u32, channels: u16) -> f64 {
    let ch = u32::from(channels.max(1));
    if rate == 0 {
        0.0
    } else {
        samples as f64 / (f64::from(rate) * f64::from(ch))
    }
}

/// The duration in milliseconds of `samples` mono or interleaved PCM samples at `rate` Hz with `channels`.
///
/// ```
/// use voice_pipes_sdk::audio::audio_duration_ms;
///
/// assert_eq!(audio_duration_ms(16_000, 16_000, 1), 1000.0);
/// assert_eq!(audio_duration_ms(32_000, 16_000, 2), 1000.0);
/// ```
#[must_use]
pub fn audio_duration_ms(samples: u64, rate: u32, channels: u16) -> f64 {
    audio_duration_seconds(samples, rate, channels) * 1000.0
}

/// The number of samples `ms` milliseconds of audio at `rate` Hz holds,
/// rounded down: how many samples to buffer for a duration a setting gives.
///
/// A negative or NaN duration is 0 samples, and one past `usize` is
/// `usize::MAX`; the conversion saturates rather than panics.
///
/// ```
/// use voice_pipes_sdk::audio::samples_in_ms;
///
/// assert_eq!(samples_in_ms(16_000, 250.0), 4_000);
/// assert_eq!(samples_in_ms(16_000, -1.0), 0);
/// ```
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a float to int cast saturates and maps NaN to 0; it cannot panic"
)]
pub fn samples_in_ms(rate: u32, ms: f32) -> usize {
    (f64::from(rate) * f64::from(ms) / 1000.0) as usize
}

/// Interleaved samples averaged down to one channel.
///
/// `channels` of `0` or `1` is mono and comes back unchanged: a frame that
/// arrived over the network with no channel count set is read as mono
/// rather than divided by zero. A short last frame is averaged over the
/// full channel count.
///
/// ```
/// use voice_pipes_sdk::audio::to_mono;
///
/// assert_eq!(to_mono(&[0.5, -0.5, 1.0, 0.0], 2), vec![0.0, 0.5]);
/// assert_eq!(to_mono(&[1.0], 0), vec![1.0]);
/// ```
pub fn to_mono(samples: &[f32], channels: u16) -> Vec<f32> {
    if channels <= 1 {
        return samples.to_vec();
    }
    let width = f32::from(channels);
    samples
        .chunks(usize::from(channels))
        .map(|frame| frame.iter().sum::<f32>() / width)
        .collect()
}

/// PCM16 of `channels` channels at `from_rate`, as mono `f32` at `to_rate`:
/// what a local recogniser hands its model. Converted with [`i16_to_f32`],
/// mixed with [`to_mono`], then resampled with [`resample::resample_f32`]
/// only when the rates differ.
///
/// # Errors
/// As [`resample::resample_f32`]: a rate it cannot convert.
///
/// ```
/// use voice_pipes_sdk::audio::mono_f32_at;
///
/// let stereo = [i16::MAX, i16::MIN, 0, 0];
/// let mono = mono_f32_at(&stereo, 2, 16_000, 16_000).unwrap();
/// assert_eq!(mono.len(), 2);
/// assert!(mono[0].abs() < 1e-4 && mono[1] == 0.0);
/// assert_eq!(mono_f32_at(&[0; 480], 1, 48_000, 16_000).unwrap().len(), 160);
/// ```
pub fn mono_f32_at(
    samples: &[i16],
    channels: u16,
    from_rate: u32,
    to_rate: u32,
) -> Result<Vec<f32>, resample::ResampleError> {
    let mono = to_mono(&i16_to_f32(samples), channels);
    if from_rate == to_rate {
        Ok(mono)
    } else {
        resample::resample_f32(&mono, from_rate, to_rate)
    }
}

#[cfg(test)]
#[path = "tests/audio.rs"]
mod tests;
