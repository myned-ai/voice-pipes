use super::*;

/// One second of a 440 Hz tone at 24 kHz, the rate `OpenAI` and Gemini send.
fn one_second_at_24k() -> Vec<i16> {
    (0..24_000)
        .map(|i| {
            let t = f64::from(i) / 24_000.0;
            #[expect(clippy::cast_possible_truncation, reason = "bounded by the amplitude")]
            let s = ((t * 440.0 * std::f64::consts::TAU).sin() * 8_000.0) as i16;
            s
        })
        .collect()
}

/// The audio a downlink emits for `audio` sent in pieces of `size`, then
/// the end of the response.
fn downlinked(audio: &[i16], size: usize) -> Vec<i16> {
    let mut down = Downlink::new(16_000);
    let mut events = Vec::new();
    for piece in audio.chunks(size) {
        events.extend(down.on_event(RealtimeEvent::ResponseAudioDelta {
            samples: piece.to_vec(),
            sample_rate: 24_000,
            alignment: None,
        }));
    }
    events.extend(down.on_event(RealtimeEvent::ResponseCompleted { response_id: None }));
    assert!(matches!(
        events.last(),
        Some(RealtimeEvent::ResponseCompleted { .. })
    ));
    events
        .into_iter()
        .flat_map(|ev| match ev {
            RealtimeEvent::ResponseAudioDelta {
                samples,
                sample_rate,
                ..
            } => {
                assert_eq!(sample_rate, 16_000);
                samples
            }
            _ => Vec::new(),
        })
        .collect()
}

/// The regression: each delta was resampled on its own, so the output
/// length drifted with the delta size and the filter restarted at every
/// edge. However the audio is cut, the output is the same stream.
#[test]
fn audio_comes_out_the_same_however_the_provider_cuts_it() {
    let audio = one_second_at_24k();
    let whole = downlinked(&audio, audio.len());
    assert!(
        whole.len().abs_diff(16_000) <= 1,
        "one second at 16 kHz, got {} samples",
        whole.len()
    );
    for size in [1, 7, 1201] {
        assert_eq!(downlinked(&audio, size), whole, "pieces of {size}");
    }
}

/// A rate the resampler cannot take is refused as a recoverable error,
/// not a panic in the session's reader.
#[test]
fn audio_at_an_unsupported_rate_is_refused() {
    let mut down = Downlink::new(16_000);
    let events = down.on_event(RealtimeEvent::ResponseAudioDelta {
        samples: vec![1, 2, 3],
        sample_rate: 0,
        alignment: None,
    });
    assert!(
        matches!(
            events.as_slice(),
            [RealtimeEvent::Error {
                recoverable: true,
                ..
            }]
        ),
        "{events:?}"
    );
}

/// When the user barges in, what the filter held belongs to the reply
/// being cut off: the next reply starts clean, without it.
#[test]
fn a_barge_in_drops_the_held_audio() {
    let audio = one_second_at_24k();
    let mut down = Downlink::new(16_000);
    down.on_event(RealtimeEvent::ResponseAudioDelta {
        samples: audio[..2400].to_vec(),
        sample_rate: 24_000,
        alignment: None,
    });
    down.on_event(RealtimeEvent::InputSpeechStarted);
    let events = down.on_event(RealtimeEvent::ResponseCompleted { response_id: None });
    assert!(
        matches!(events.as_slice(), [RealtimeEvent::ResponseCompleted { .. }]),
        "no held audio after a barge-in: {events:?}"
    );
}

/// Audio already at the output rate passes through untouched.
#[test]
fn audio_at_the_output_rate_passes_through() {
    let mut down = Downlink::new(24_000);
    let samples = one_second_at_24k();
    let events = down.on_event(RealtimeEvent::ResponseAudioDelta {
        samples: samples.clone(),
        sample_rate: 24_000,
        alignment: None,
    });
    assert!(
        matches!(events.as_slice(), [RealtimeEvent::ResponseAudioDelta { samples: s, sample_rate: 24_000, .. }] if *s == samples)
    );
}
