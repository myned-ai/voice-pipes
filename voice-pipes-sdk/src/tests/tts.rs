use super::*;

/// Feed `(text, start, end)` entries to one grouper and flush it.
fn grouped(grouper: &mut WordGrouper, entries: &[(&str, u64, u64)]) -> Vec<WordBoundary> {
    let mut out = Vec::new();
    for &(ch, start, end) in entries {
        grouper.push(ch, start, end, &mut out);
    }
    out
}

/// Whitespace ends a word, punctuation stays on it, and a word's span runs
/// from its first character's start to its last one's end.
#[test]
fn words_group_on_whitespace_with_punctuation_attached() {
    let mut g = WordGrouper::new();
    let mut out = grouped(
        &mut g,
        &[
            ("H", 0, 50),
            ("i", 50, 100),
            (",", 100, 150),
            (" ", 150, 200),
            ("y", 200, 250),
            ("o", 250, 300),
            ("u", 300, 350),
            ("!", 350, 400),
        ],
    );
    g.flush(&mut out);
    let words: Vec<_> = out
        .iter()
        .map(|w| (w.word.as_str(), w.start_ms, w.duration_ms))
        .collect();
    assert_eq!(words, [("Hi,", 0, 150), ("you!", 200, 200)]);
}

/// A word whose characters arrive across two calls is one boundary.
#[test]
fn a_word_split_across_pushes_is_stitched() {
    let mut g = WordGrouper::new();
    let first = grouped(&mut g, &[("h", 0, 100), ("e", 100, 200), ("l", 200, 300)]);
    assert!(
        first.is_empty(),
        "no whitespace yet: the word is still open"
    );
    let mut out = grouped(&mut g, &[("l", 300, 400), ("o", 400, 500), (" ", 500, 550)]);
    g.flush(&mut out);
    assert_eq!(out.len(), 1);
    assert_eq!(
        (out[0].word.as_str(), out[0].start_ms, out[0].duration_ms),
        ("hello", 0, 500)
    );
}

/// Empty entries and whitespace runs end a word without making one; a
/// flush with nothing open adds nothing; times past `u32` saturate.
#[test]
fn empty_entries_and_whitespace_runs_end_words_and_times_saturate() {
    let mut g = WordGrouper::new();
    let mut out = grouped(
        &mut g,
        &[("a", 0, 10), ("", 10, 20), ("  ", 20, 30), ("b", 30, 40)],
    );
    g.flush(&mut out);
    g.flush(&mut out);
    assert_eq!(
        out.iter().map(|w| w.word.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    );

    let mut late = grouped(&mut g, &[("z", u64::MAX - 1, u64::MAX)]);
    g.flush(&mut late);
    assert_eq!((late[0].start_ms, late[0].duration_ms), (u32::MAX, 1));
}

#[test]
fn transient_classification() {
    assert!(TtsError::RateLimited { retry_after: None }.is_transient());
    assert!(TtsError::NetworkError("x".into()).is_transient());
    assert!(TtsError::InferenceFailed("x".into()).is_transient());
    assert!(
        TtsError::ApiError {
            status: 500,
            message: "x".into()
        }
        .is_transient()
    );
    assert!(!TtsError::ConfigError("x".into()).is_transient());
    assert!(!TtsError::ModelLoadFailed("x".into()).is_transient());
    assert!(!TtsError::AuthError("x".into()).is_transient());
    assert!(!TtsError::PhonemizationFailed("x".into()).is_transient());
}

#[test]
fn display_includes_detail() {
    let e = TtsError::ApiError {
        status: 429,
        message: "slow down".into(),
    };
    assert_eq!(e.to_string(), "API error (429): slow down");
}

/// A hosted API's status maps to the error the retry and health logic
/// expects, with the vendor named and a long body cut short.
#[test]
fn http_statuses_map_to_tts_errors() {
    use super::TtsError;
    assert!(matches!(
        TtsError::from_http_status("Acme", 401, "bad key"),
        TtsError::AuthError(m) if m == "Acme: bad key"
    ));
    assert!(matches!(
        TtsError::from_http_status("Acme", 403, "no"),
        TtsError::AuthError(_)
    ));
    assert!(matches!(
        TtsError::from_http_status("Acme", 429, ""),
        TtsError::RateLimited { .. }
    ));
    assert!(matches!(
        TtsError::from_http_status("Acme", 422, "voice?"),
        TtsError::ConfigError(_)
    ));
    assert!(matches!(
        TtsError::from_http_status("Acme", 503, ""),
        TtsError::InferenceFailed(m) if m == "Acme server error (503): Acme HTTP 503"
    ));
    match TtsError::from_http_status("Acme", 418, &"x".repeat(2000)) {
        TtsError::ApiError { status, message } => {
            assert_eq!(status, 418);
            assert_eq!(message.len(), 512);
        }
        other => panic!("{other:?}"),
    }
    assert!(TtsError::from_http_status("Acme", 500, "").is_transient());
}

/// A request the provider rejects fails the same way on every attempt, so
/// it is never retried: an unknown voice or model, a teapot, a bad key.
#[test]
fn client_errors_are_not_transient() {
    for status in [400, 401, 403, 404, 410, 418, 422] {
        let e = TtsError::from_http_status("Acme", status, "no");
        assert!(!e.is_transient(), "{status}: {e:?}");
    }
    assert!(matches!(
        TtsError::from_http_status("Acme", 404, "voice not found"),
        TtsError::ConfigError(m) if m == "Acme: voice not found"
    ));
    for status in [408, 429, 500, 502, 503] {
        let e = TtsError::from_http_status("Acme", status, "");
        assert!(e.is_transient(), "{status}: {e:?}");
    }
    let unknown = TtsError::ApiError {
        status: 418,
        message: String::new(),
    };
    assert!(!unknown.is_transient());
}

/// A default session config names real rates: a provider built from it
/// must not be handed zero, which no audio has and no resampler converts.
#[test]
fn the_default_session_config_has_real_rates() {
    let cfg = TtsSessionConfig::default();
    assert_eq!(cfg.input_sample_rate, 16_000);
    assert_eq!(cfg.output_sample_rate, 16_000);
}

/// More frames than the channel holds: the stream and the collection run
/// together, so nothing waits on itself, and every sample arrives in order.
#[tokio::test]
async fn collect_stream_gathers_a_long_stream() {
    let frame = collect_stream(16_000, |tx| async move {
        for i in 0..200_i16 {
            tx.send(AudioFrame {
                samples: vec![i; 10],
                sample_rate: 16_000,
                channels: 1,
            })
            .await
            .map_err(|_gone| TtsError::InferenceFailed("receiver gone".into()))?;
        }
        Ok(())
    })
    .await
    .expect("collected");
    assert_eq!(frame.samples.len(), 2000);
    assert_eq!(frame.samples[1999], 199);
    assert_eq!(frame.sample_rate, 16_000);
}

#[tokio::test]
async fn collect_stream_reports_the_stream_error() {
    let err = collect_stream(16_000, |_tx| async {
        Err::<(), _>(TtsError::InferenceFailed("no audio".into()))
    })
    .await
    .expect_err("the stream failed");
    assert!(err.to_string().contains("no audio"), "{err}");
}

#[tokio::test]
async fn recorded_counts_successes_and_errors() {
    let core =
        crate::component::ComponentCore::new("tts:test", crate::component::ComponentKind::Tts);
    let _ = recorded(&core, async { Ok::<_, TtsError>(()) }).await;
    let _ = recorded(&core, async {
        Err::<(), _>(TtsError::InferenceFailed("x".into()))
    })
    .await;
    let telemetry = core.telemetry();
    assert_eq!(telemetry.processed_total, 1);
    assert_eq!(telemetry.errors_total, 1);
}

#[tokio::test]
async fn a_cancelled_synthesis_is_not_recorded() {
    let core =
        crate::component::ComponentCore::new("tts:test", crate::component::ComponentKind::Tts);
    let cancel = tokio_util::sync::CancellationToken::new();
    let _ = recorded_unless_cancelled(&core, &cancel, async { Ok::<_, TtsError>(()) }).await;
    assert_eq!(core.telemetry().processed_total, 1);

    // Cancelled while it ran: neither its success nor an error counts.
    let result = recorded_unless_cancelled(&core, &cancel, async {
        cancel.cancel();
        Ok::<_, TtsError>(())
    })
    .await;
    assert!(result.is_ok());
    let _ = recorded_unless_cancelled(&core, &cancel, async {
        Err::<(), _>(TtsError::InferenceFailed("x".into()))
    })
    .await;
    let telemetry = core.telemetry();
    assert_eq!(telemetry.processed_total, 1);
    assert_eq!(telemetry.errors_total, 0);
}

#[test]
fn the_requested_voice_is_the_hint_or_the_configured_one() {
    let cell = voice_cell("alloy".into());
    assert_eq!(requested_voice(None, &cell), "alloy");
    let hints = SynthesisHints {
        voice: Some("echo".into()),
        ..SynthesisHints::default()
    };
    assert_eq!(requested_voice(Some(&hints), &cell), "echo");
    cell.store(std::sync::Arc::new("nova".into()));
    assert_eq!(requested_voice(None, &cell), "nova");
}

#[test]
fn a_store_voice_cell_is_adopted() {
    let own = voice_cell("alloy".into());
    assert!(std::sync::Arc::ptr_eq(
        &adopt_voice(None, std::sync::Arc::clone(&own)),
        &own
    ));
    let store = crate::params::AtomicParamStore::builder()
        .string("voice", "echo")
        .build();
    let adopted = adopt_voice(Some(&store), own);
    assert_eq!(*adopted.load_full(), "echo");
}
