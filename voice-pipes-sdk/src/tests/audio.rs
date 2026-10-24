use super::*;

fn le(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

fn wav_header(extra_chunk: Option<(&[u8; 4], &[u8])>) -> Vec<u8> {
    let mut h = Vec::new();
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&0u32.to_le_bytes());
    h.extend_from_slice(b"WAVE");
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&[1, 0, 1, 0]);
    h.extend_from_slice(&24_000u32.to_le_bytes());
    h.extend_from_slice(&48_000u32.to_le_bytes());
    h.extend_from_slice(&[2, 0, 16, 0]);
    if let Some((id, body)) = extra_chunk {
        h.extend_from_slice(id);
        h.extend_from_slice(&(body.len() as u32).to_le_bytes());
        h.extend_from_slice(body);
        if body.len() % 2 == 1 {
            h.push(0);
        }
    }
    h.extend_from_slice(b"data");
    h.extend_from_slice(&u32::MAX.to_le_bytes());
    h
}

/// However the bytes are chunked, the same samples come out, in frames of
/// the configured size and one short last frame.
#[test]
fn pcm_frames_the_same_whatever_the_chunking() {
    let samples: Vec<i16> = (0..25).map(|i| i * 1000 - 12_000).collect();
    let bytes = le(&samples);
    let (framed, pushed) = crate::testing::chunking_invariant(&bytes, |chunks| {
        let mut pcm = PcmFramer::new(16_000, 10);
        let mut out = Vec::new();
        let mut pushed = Vec::new();
        for chunk in chunks {
            pushed.extend_from_slice(pcm.push(chunk));
            for f in pcm.take_frames() {
                assert_eq!(f.samples.len(), 10);
                assert_eq!(f.sample_rate, 16_000);
                out.extend(f.samples);
            }
        }
        let last = pcm.finish().unwrap();
        assert_eq!(last.samples.len(), 5);
        out.extend(last.samples);
        assert_eq!(pcm.samples_out(), 25);
        (out, pushed)
    });
    assert_eq!(framed, samples);
    assert_eq!(pushed, samples, "push reports each sample once");
}

/// A dangling odd byte at the end is half a sample and is dropped; an empty
/// push changes nothing.
#[test]
fn a_dangling_byte_is_dropped_at_the_end() {
    let mut pcm = PcmFramer::new(24_000, 4);
    assert!(pcm.push(&[]).is_empty());
    assert_eq!(pcm.push(&[7]), &[] as &[i16]);
    assert_eq!(pcm.push(&[0, 9]), &[7]);
    assert_eq!(pcm.finish().unwrap().samples, vec![7]);
    assert!(pcm.finish().is_none());
}

/// A header is removed however it is chunked, metadata chunks before `data`
/// (odd-sized ones padded) included, and the PCM after it passes through.
#[test]
fn a_wav_header_is_stripped_whatever_the_chunking() {
    let mut stream = wav_header(Some((b"LIST", b"odd")));
    let pcm = le(&[1, -2, 3]);
    stream.extend_from_slice(&pcm);
    let out = crate::testing::chunking_invariant(&stream, strip_required);
    assert_eq!(out, pcm);
}

/// What a header-requiring stripper passes of `chunks`, fed in order.
fn strip_required(chunks: &[&[u8]]) -> Vec<u8> {
    let mut wav = WavStripper::requiring_header();
    let mut out = Vec::new();
    for chunk in chunks {
        out.extend(wav.feed(chunk).unwrap());
    }
    assert!(wav.stripped_header());
    out
}

/// Raw PCM passes through a lenient stripper and is refused by a strict one;
/// a stream shorter than a header is PCM only when no header was required.
#[test]
fn raw_pcm_passes_unless_a_header_is_required() {
    let raw = le(&[5, 6, 7, 8, 9, 10]);
    let mut lenient = WavStripper::new();
    assert_eq!(lenient.feed(&raw).unwrap(), raw);
    assert!(!lenient.stripped_header());
    assert_eq!(lenient.feed(&[1, 2]).unwrap(), vec![1, 2]);

    assert_eq!(
        WavStripper::requiring_header().feed(&raw),
        Err(WavError::NotWav)
    );

    let mut short = WavStripper::new();
    assert!(short.feed(&[1, 0, 2]).unwrap().is_empty());
    assert_eq!(short.finish(), Ok(vec![1, 0, 2]));
    let mut short = WavStripper::requiring_header();
    assert!(short.feed(&[1, 0, 2]).unwrap().is_empty());
    assert_eq!(short.finish(), Err(WavError::Truncated));
    assert_eq!(WavStripper::requiring_header().finish(), Ok(Vec::new()));
}

/// A stream that ends inside its WAV header is an error at `finish`, not a
/// success with no audio, whether or not a header was required.
#[test]
fn a_stream_ending_inside_its_header_is_an_error() {
    let header = wav_header(None);
    for cut in [12, 20, header.len() - 1] {
        for mut wav in [WavStripper::new(), WavStripper::requiring_header()] {
            assert!(wav.feed(&header[..cut]).unwrap().is_empty());
            assert_eq!(wav.finish(), Err(WavError::Truncated), "cut at {cut}");
        }
    }
    let mut whole = WavStripper::new();
    assert!(whole.feed(&header).unwrap().is_empty());
    assert!(whole.stripped_header());
    assert_eq!(whole.finish(), Ok(Vec::new()));
}

/// A `data` chunk with a real declared size ends there, however the stream
/// is chunked: a chunk after it is not audio. A placeholder size runs to the
/// end of the stream.
#[test]
fn a_declared_data_size_ends_the_audio() {
    let pcm = le(&[1, -2, 3, -4]);
    let mut stream = wav_header(None);
    let at = stream.len() - 4;
    stream[at..].copy_from_slice(&(pcm.len() as u32).to_le_bytes());
    stream.extend_from_slice(&pcm);
    stream.extend_from_slice(b"LIST\x04\0\0\0junk");
    assert_eq!(
        crate::testing::chunking_invariant(&stream, strip_required),
        pcm
    );

    for placeholder in [0u32, u32::MAX, 0x7FFF_FFFF + 1] {
        let mut stream = wav_header(None);
        let at = stream.len() - 4;
        stream[at..].copy_from_slice(&placeholder.to_le_bytes());
        stream.extend_from_slice(&pcm);
        let mut wav = WavStripper::new();
        assert_eq!(wav.feed(&stream).unwrap(), pcm, "size {placeholder:#x}");
        assert_eq!(wav.feed(&[9, 9]).unwrap(), vec![9, 9]);
    }
}

/// A broken header never panics and never grows without bound: chunk sizes
/// that point past the limit, a header that never reaches `data`, and a
/// `data` tag whose size has not arrived.
#[test]
fn a_malformed_header_is_an_error_not_a_panic() {
    let mut huge = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
    huge.extend_from_slice(&u32::MAX.to_le_bytes());
    let mut wav = WavStripper::new();
    assert!(wav.feed(&huge).unwrap().is_empty());
    assert_eq!(
        wav.feed(&[0u8; MAX_WAV_HEADER_BYTES]),
        Err(WavError::NoDataChunk)
    );

    let mut endless = b"RIFF\0\0\0\0WAVE".to_vec();
    for _ in 0..600 {
        endless.extend_from_slice(b"junk");
        endless.extend_from_slice(&0u32.to_le_bytes());
    }
    assert_eq!(
        WavStripper::new().feed(&endless),
        Err(WavError::NoDataChunk)
    );

    let mut wav = WavStripper::new();
    assert!(wav.feed(b"RIFF\0\0\0\0WAVEdata").unwrap().is_empty());
    assert_eq!(wav.feed(&[0, 0, 0, 0, 4, 0]).unwrap(), vec![4, 0]);
}

/// Frames go out on the channel as they fill; a closed receiver is reported
/// so the provider can stop reading.
#[tokio::test]
async fn frames_are_sent_until_the_receiver_goes() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let mut pcm = PcmFramer::new(8_000, 2);
    pcm.push(&le(&[1, 2, 3]));
    assert!(pcm.send_frames(&tx).await);
    assert_eq!(rx.recv().await.unwrap().samples, vec![1, 2]);
    assert!(pcm.send_rest(&tx).await);
    assert_eq!(rx.recv().await.unwrap().samples, vec![3]);

    drop(rx);
    pcm.push(&le(&[4, 5]));
    assert!(!pcm.send_frames(&tx).await);
}

#[test]
fn to_mono_reads_zero_channels_as_mono() {
    assert_eq!(to_mono(&[1.0], 0), vec![1.0]);
    assert_eq!(to_mono(&[0.25, 0.75], 1), vec![0.25, 0.75]);
}

#[test]
fn to_mono_averages_each_frame() {
    let mono = to_mono(&[0.4, 0.6, -0.2, 0.8], 2);
    assert_eq!(mono.len(), 2);
    assert!((mono[0] - 0.5).abs() < 1e-6);
    assert!((mono[1] - 0.3).abs() < 1e-6);
    assert!(to_mono(&[], 2).is_empty());
}

/// Samples become two little-endian bytes each, and read back as the same
/// samples, the extremes included.
#[test]
fn i16_to_le_bytes_round_trips() {
    let samples = [0, 1, -1, i16::MAX, i16::MIN];
    let bytes = i16_to_le_bytes(&samples);
    assert_eq!(bytes.len(), samples.len() * 2);
    assert_eq!(&bytes[..4], &[0x00, 0x00, 0x01, 0x00]);
    let back: Vec<i16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| i16::from_le_bytes(*c))
        .collect();
    assert_eq!(back, samples);
    assert!(i16_to_le_bytes(&[]).is_empty());
}

/// Model output in range scales to the full i16 range.
#[test]
fn f32_to_i16_converts_correctly() {
    assert_eq!(f32_to_i16(&[0.0, 1.0, -1.0]), vec![0, i16::MAX, -i16::MAX]);
    assert_eq!(f32_to_i16(&[0.5]), vec![16_383]);
    assert!(f32_to_i16(&[]).is_empty());
}

/// Output past the range clips instead of wrapping; NaN is silence.
#[test]
fn f32_to_i16_clamps() {
    assert_eq!(
        f32_to_i16(&[2.0, -2.0, f32::INFINITY, f32::NAN]),
        vec![i16::MAX, -i16::MAX, i16::MAX, 0]
    );
}

/// A duration's samples, rounded down; nonsense durations saturate.
#[test]
fn samples_in_ms_counts_whole_samples_and_saturates() {
    assert_eq!(samples_in_ms(16_000, 80.0), 1_280);
    assert_eq!(samples_in_ms(16_000, 8_000.0), 128_000);
    assert_eq!(samples_in_ms(22_050, 1.0), 22);
    assert_eq!(samples_in_ms(16_000, 0.0), 0);
    assert_eq!(samples_in_ms(16_000, -5.0), 0);
    assert_eq!(samples_in_ms(16_000, f32::NAN), 0);
    assert_eq!(samples_in_ms(u32::MAX, f32::MAX), usize::MAX);
}

/// PCM16 maps onto [-1, 1]: the extremes land on the ends and zero stays
/// zero; nothing in gives nothing out.
#[test]
fn i16_to_f32_spans_the_unit_range() {
    let out = i16_to_f32(&[0, i16::MAX, i16::MIN]);
    assert_eq!(out[0], 0.0);
    assert!((out[1] - 1.0).abs() < 0.001);
    assert!((out[2] + 1.0).abs() < 0.001);
    assert!(i16_to_f32(&[]).is_empty());
}
