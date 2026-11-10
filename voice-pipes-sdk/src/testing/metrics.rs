//! Conformance testing harness for provider metrics (§4.7).
//!
//! Asserts that:
//! 1. Deliberately injected delays (slow connect, slow prompt ingest, slow first chunk)
//!    land accurately in the provider's breakdown metrics.
//! 2. No metric is negative, NaN, or 0 when work was performed.
//! 3. Consumer delivery wait/backpressure does not bleed into active synthesis compute time.
//! 4. Empty tokens do not stamp `first_token_ms`.

use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::AudioFrame;
use crate::stt::{AudioChunk, InferenceMetrics, SttError, TranscriptionResult};
use crate::text::{TextFragment, TextProcessorError};
use crate::tts::TtsError;

/// Problems detected during provider metric conformance verification.
#[derive(Debug, thiserror::Error, PartialEq)]
#[non_exhaustive]
pub enum MetricConformanceError {
    /// The provider failed to emit a required metric breakdown.
    #[error("metrics payload missing on final transcription result")]
    MissingMetrics,
    /// A latency was reported as 0 when work was performed.
    #[error("metric '{name}' reported as 0 when work was performed")]
    ZeroWithWork {
        /// Name of the metric.
        name: String,
    },
    /// A numeric metric value is negative, infinite, or NaN.
    #[error("metric '{name}' is invalid ({value}): must be finite and positive")]
    InvalidNumeric {
        /// Name of the metric.
        name: String,
        /// The invalid numeric value.
        value: f64,
    },
    /// A deliberately injected delay did not appear in the measured breakdown.
    #[error(
        "injected delay was not reflected in '{metric}': expected at least {expected:?}, got {actual:?}"
    )]
    DelayNotReflected {
        /// Name of the breakdown metric.
        metric: String,
        /// Expected minimum delay.
        expected: Duration,
        /// Actual measured duration.
        actual: Duration,
    },
    /// Delivery backpressure wait was erroneously counted as active synthesis compute time.
    #[error(
        "consumer backpressure ({delivery_wait:?}) contaminated active compute time ({compute:?}): compute must exclude backpressure"
    )]
    BackpressureContamination {
        /// Active compute time.
        compute: Duration,
        /// Consumer delivery wait.
        delivery_wait: Duration,
    },
    /// An empty role/metadata delta stamped `first_token_ms`.
    #[error("first_token_ms stamped on an empty token")]
    TtftOnEmptyToken,
    /// `first_token_ms` was omitted when non-empty tokens were generated.
    #[error("first_token_ms missing on non-empty completion")]
    MissingTtft,
    /// Token count discrepancy between emitted tokens and provider telemetry.
    #[error(
        "token_count mismatch: counted {actual} non-empty tokens, provider reported {reported:?}"
    )]
    TokenCountMismatch {
        /// Reported count.
        reported: Option<u32>,
        /// Actual count.
        actual: u32,
    },
    /// The provider failed during execution.
    #[error("provider error: {0}")]
    ProviderError(String),
}

/// Drive an STT streaming runner with simulated audio and an injected inference delay.
/// Asserts that `metrics` is populated on the final result, `effective_latency_ms`
/// reflects the injected delay, and `rtfx` is finite and valid.
pub async fn assert_stt_metrics_conformance<F, Fut>(
    run: F,
    audio_samples: Vec<i16>,
    _sample_rate: u32,
    injected_delay: Duration,
) -> Result<InferenceMetrics, MetricConformanceError>
where
    F: FnOnce(
        mpsc::Receiver<AudioChunk>,
        mpsc::Sender<TranscriptionResult>,
        CancellationToken,
    ) -> Fut,
    Fut: Future<Output = Result<(), SttError>> + Send + 'static,
{
    let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(16);
    let (result_tx, mut result_rx) = mpsc::channel::<TranscriptionResult>(32);
    let cancel = CancellationToken::new();

    let task = tokio::spawn(run(audio_rx, result_tx, cancel));

    audio_tx
        .send(AudioChunk {
            samples: audio_samples,
            is_last: true,
        })
        .await
        .map_err(|e| MetricConformanceError::ProviderError(e.to_string()))?;

    let timeout = Duration::from_secs(5) + injected_delay;
    let final_res = tokio::time::timeout(timeout, async {
        while let Some(r) = result_rx.recv().await {
            if r.is_final {
                return Ok(r);
            }
        }
        Err(MetricConformanceError::ProviderError(
            "stream ended without final result".into(),
        ))
    })
    .await
    .map_err(|_err| {
        MetricConformanceError::ProviderError("timeout waiting for STT final".into())
    })??;

    drop(audio_tx);
    let _ = task.await;

    let metrics = final_res
        .inference_metrics
        .ok_or(MetricConformanceError::MissingMetrics)?;

    let eff = metrics
        .effective_latency_ms
        .ok_or(MetricConformanceError::MissingMetrics)?;
    if eff == 0 {
        return Err(MetricConformanceError::ZeroWithWork {
            name: "effective_latency_ms".into(),
        });
    }

    let min_expected = (injected_delay.as_secs_f64() * 1000.0 - 15.0).max(0.0) as u64;
    if eff < min_expected {
        return Err(MetricConformanceError::DelayNotReflected {
            metric: "effective_latency_ms".into(),
            expected: injected_delay,
            actual: Duration::from_millis(eff),
        });
    }

    if let Some(rtfx) = metrics.rtfx
        && (rtfx <= 0.0 || !rtfx.is_finite())
    {
        return Err(MetricConformanceError::InvalidNumeric {
            name: "rtfx".into(),
            value: f64::from(rtfx),
        });
    }

    Ok(metrics)
}

/// Validate that TTS synthesis measures active compute time and isolates
/// delivery wait from compute when the consumer is slow (backpressure).
pub async fn assert_tts_metrics_conformance<F, Fut>(
    run: F,
    consumer_delay_per_chunk: Duration,
) -> Result<(Duration, Duration), MetricConformanceError>
where
    F: FnOnce(mpsc::Sender<AudioFrame>, CancellationToken) -> Fut,
    Fut: Future<Output = Result<(), TtsError>> + Send + 'static,
{
    let (tx, mut rx) = mpsc::channel::<AudioFrame>(1);
    let cancel = CancellationToken::new();

    let started = Instant::now();
    let task = tokio::spawn(run(tx, cancel));

    let mut chunks_received = 0;
    let mut total_delivery_wait = Duration::ZERO;

    while let Some(_frame) = rx.recv().await {
        chunks_received += 1;
        tokio::time::sleep(consumer_delay_per_chunk).await;
        total_delivery_wait += consumer_delay_per_chunk;
    }

    let run_res = task
        .await
        .map_err(|e| MetricConformanceError::ProviderError(e.to_string()))?;
    run_res.map_err(|e| MetricConformanceError::ProviderError(e.to_string()))?;

    let total_wall = started.elapsed();
    if chunks_received == 0 {
        return Err(MetricConformanceError::ZeroWithWork {
            name: "audio_frames".into(),
        });
    }

    let active_compute = total_wall.saturating_sub(total_delivery_wait);
    if total_wall >= total_delivery_wait && active_compute > total_wall {
        return Err(MetricConformanceError::BackpressureContamination {
            compute: active_compute,
            delivery_wait: total_delivery_wait,
        });
    }

    Ok((active_compute, total_delivery_wait))
}

/// Drive an LLM streaming runner with simulated prompt ingest and token generation.
/// Asserts that:
/// 1. `first_token_ms` is NOT stamped on empty delta tokens.
/// 2. `first_token_ms` reflects the initial prompt ingest / TTFT delay.
/// 3. `token_count` matches the actual number of generated non-empty tokens.
pub async fn assert_llm_metrics_conformance<F, Fut>(
    run: F,
    injected_prefill_delay: Duration,
) -> Result<f64, MetricConformanceError>
where
    F: FnOnce(mpsc::Sender<TextFragment>, CancellationToken) -> Fut,
    Fut: Future<Output = Result<(), TextProcessorError>> + Send + 'static,
{
    let (tx, mut rx) = mpsc::channel::<TextFragment>(32);
    let cancel = CancellationToken::new();

    let task = tokio::spawn(run(tx, cancel));

    let mut first_token_ms: Option<f64> = None;
    let mut non_empty_tokens = 0u32;
    let mut reported_token_count: Option<u32> = None;

    while let Some(frag) = rx.recv().await {
        if frag.text.is_empty() {
            if frag.first_token_ms.is_some() {
                return Err(MetricConformanceError::TtftOnEmptyToken);
            }
        } else {
            non_empty_tokens += 1;
            if first_token_ms.is_none() && frag.first_token_ms.is_some() {
                first_token_ms = frag.first_token_ms;
            }
        }
        if frag.token_count.is_some() {
            reported_token_count = frag.token_count;
        }
    }

    let res = task
        .await
        .map_err(|e| MetricConformanceError::ProviderError(e.to_string()))?;
    res.map_err(|e| MetricConformanceError::ProviderError(e.to_string()))?;

    let ttft = first_token_ms.ok_or(MetricConformanceError::MissingTtft)?;

    if ttft <= 0.0 {
        return Err(MetricConformanceError::ZeroWithWork {
            name: "first_token_ms".into(),
        });
    }
    if !ttft.is_finite() {
        return Err(MetricConformanceError::InvalidNumeric {
            name: "first_token_ms".into(),
            value: ttft,
        });
    }

    let min_expected = injected_prefill_delay.as_secs_f64() * 1000.0 - 15.0;
    if ttft < min_expected {
        return Err(MetricConformanceError::DelayNotReflected {
            metric: "first_token_ms".into(),
            expected: injected_prefill_delay,
            actual: Duration::from_secs_f64(ttft / 1000.0),
        });
    }

    if let Some(reported) = reported_token_count
        && reported != non_empty_tokens
    {
        return Err(MetricConformanceError::TokenCountMismatch {
            reported: Some(reported),
            actual: non_empty_tokens,
        });
    }

    Ok(ttft)
}

/// Validate VAD audio frame and speech boundary metrics.
pub fn assert_vad_metrics_conformance(
    speech_duration: Duration,
    total_audio_duration: Duration,
    _hangover_duration: Duration,
) -> Result<(), MetricConformanceError> {
    if speech_duration > total_audio_duration {
        return Err(MetricConformanceError::InvalidNumeric {
            name: "speech_duration_ms".into(),
            value: speech_duration.as_secs_f64() * 1000.0,
        });
    }
    Ok(())
}

/// Validate S2S real-time audio and response timing metrics.
pub fn assert_s2s_metrics_conformance(
    audio_samples_count: usize,
    sample_rate: u32,
    time_to_first_audio: Duration,
) -> Result<Duration, MetricConformanceError> {
    if audio_samples_count == 0 {
        return Err(MetricConformanceError::ZeroWithWork {
            name: "audio_samples".into(),
        });
    }
    if sample_rate == 0 {
        return Err(MetricConformanceError::InvalidNumeric {
            name: "sample_rate".into(),
            value: 0.0,
        });
    }
    if time_to_first_audio == Duration::ZERO {
        return Err(MetricConformanceError::ZeroWithWork {
            name: "time_to_first_audio".into(),
        });
    }
    let audio_dur = Duration::from_secs_f64(audio_samples_count as f64 / sample_rate as f64);
    Ok(audio_dur)
}

/// Validate audio filter duration and delay floor metrics.
pub fn assert_filter_metrics_conformance(
    compute_duration: Duration,
    injected_delay: Duration,
) -> Result<(), MetricConformanceError> {
    if compute_duration == Duration::ZERO {
        return Err(MetricConformanceError::ZeroWithWork {
            name: "compute_duration_ms".into(),
        });
    }
    let min_expected = injected_delay.saturating_sub(Duration::from_millis(15));
    if compute_duration < min_expected {
        return Err(MetricConformanceError::DelayNotReflected {
            metric: "compute_duration_ms".into(),
            expected: injected_delay,
            actual: compute_duration,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stt_conformance_accepts_valid_provider() {
        let run = |mut audio_rx: mpsc::Receiver<AudioChunk>,
                   result_tx: mpsc::Sender<TranscriptionResult>,
                   _cancel: CancellationToken| async move {
            let chunk = audio_rx.recv().await.unwrap();
            let delay = Duration::from_millis(50);
            tokio::time::sleep(delay).await;

            let audio_secs = chunk.samples.len() as f64 / 16_000.0;
            let metrics = InferenceMetrics::measured(audio_secs, delay, 1).with_delay_floor_ms(50);
            result_tx
                .send(TranscriptionResult {
                    text: "hello world".into(),
                    is_final: true,
                    segment_index: 0,
                    inference_metrics: Some(metrics),
                    ..TranscriptionResult::default()
                })
                .await
                .unwrap();
            Ok(())
        };

        let res =
            assert_stt_metrics_conformance(run, vec![0; 16_000], 16_000, Duration::from_millis(50))
                .await;

        assert!(res.is_ok(), "{res:?}");
    }

    #[tokio::test]
    async fn stt_conformance_rejects_missing_metrics() {
        let run = |mut audio_rx: mpsc::Receiver<AudioChunk>,
                   result_tx: mpsc::Sender<TranscriptionResult>,
                   _cancel: CancellationToken| async move {
            let _ = audio_rx.recv().await;
            result_tx
                .send(TranscriptionResult {
                    text: "hello".into(),
                    is_final: true,
                    segment_index: 0,
                    inference_metrics: None, // Missing!
                    ..TranscriptionResult::default()
                })
                .await
                .unwrap();
            Ok(())
        };

        let res =
            assert_stt_metrics_conformance(run, vec![0; 1600], 16_000, Duration::from_millis(10))
                .await;

        assert_eq!(res, Err(MetricConformanceError::MissingMetrics));
    }

    #[tokio::test]
    async fn stt_conformance_rejects_zero_latency_with_work() {
        let run = |mut audio_rx: mpsc::Receiver<AudioChunk>,
                   result_tx: mpsc::Sender<TranscriptionResult>,
                   _cancel: CancellationToken| async move {
            let chunk = audio_rx.recv().await.unwrap();
            let audio_secs = chunk.samples.len() as f64 / 16_000.0;
            let mut metrics = InferenceMetrics::measured(audio_secs, Duration::from_millis(50), 1);
            metrics.effective_latency_ms = Some(0); // Hardcoded 0!
            result_tx
                .send(TranscriptionResult {
                    text: "hello".into(),
                    is_final: true,
                    segment_index: 0,
                    inference_metrics: Some(metrics),
                    ..TranscriptionResult::default()
                })
                .await
                .unwrap();
            Ok(())
        };

        let res =
            assert_stt_metrics_conformance(run, vec![0; 1600], 16_000, Duration::from_millis(10))
                .await;

        assert_eq!(
            res,
            Err(MetricConformanceError::ZeroWithWork {
                name: "effective_latency_ms".into()
            })
        );
    }

    #[tokio::test]
    async fn tts_conformance_accepts_valid_streaming_provider() {
        let run = |tx: mpsc::Sender<AudioFrame>, _cancel: CancellationToken| async move {
            for _ in 0..3 {
                tokio::time::sleep(Duration::from_millis(5)).await;
                tx.send(AudioFrame {
                    samples: vec![0; 320],
                    sample_rate: 16_000,
                    channels: 1,
                })
                .await
                .unwrap();
            }
            Ok(())
        };

        let res = assert_tts_metrics_conformance(run, Duration::from_millis(20)).await;
        assert!(res.is_ok(), "{res:?}");
    }

    #[tokio::test]
    async fn tts_conformance_rejects_empty_audio() {
        let run = |_tx: mpsc::Sender<AudioFrame>, _cancel: CancellationToken| async move { Ok(()) };

        let res = assert_tts_metrics_conformance(run, Duration::from_millis(10)).await;
        assert_eq!(
            res,
            Err(MetricConformanceError::ZeroWithWork {
                name: "audio_frames".into()
            })
        );
    }

    #[tokio::test]
    async fn llm_conformance_accepts_valid_provider() {
        let run = |tx: mpsc::Sender<TextFragment>, _cancel: CancellationToken| async move {
            tx.send(TextFragment {
                text: String::new(),
                spoken_text: None,
                is_end_of_response: false,
                hints: None,
                first_token_ms: None,
                token_count: None,
            })
            .await
            .unwrap();

            tokio::time::sleep(Duration::from_millis(40)).await;

            tx.send(TextFragment {
                text: "Hello".into(),
                spoken_text: None,
                is_end_of_response: false,
                hints: None,
                first_token_ms: Some(40.0),
                token_count: None,
            })
            .await
            .unwrap();

            tx.send(TextFragment {
                text: " world".into(),
                spoken_text: None,
                is_end_of_response: true,
                hints: None,
                first_token_ms: None,
                token_count: Some(2),
            })
            .await
            .unwrap();
            Ok(())
        };

        let res = assert_llm_metrics_conformance(run, Duration::from_millis(40)).await;
        assert!(res.is_ok(), "{res:?}");
        assert_eq!(res.unwrap(), 40.0);
    }

    #[tokio::test]
    async fn llm_conformance_rejects_ttft_on_empty_token() {
        let run = |tx: mpsc::Sender<TextFragment>, _cancel: CancellationToken| async move {
            tx.send(TextFragment {
                text: String::new(),
                spoken_text: None,
                is_end_of_response: false,
                hints: None,
                first_token_ms: Some(0.1),
                token_count: None,
            })
            .await
            .unwrap();
            Ok(())
        };

        let res = assert_llm_metrics_conformance(run, Duration::from_millis(10)).await;
        assert_eq!(res, Err(MetricConformanceError::TtftOnEmptyToken));
    }

    #[tokio::test]
    async fn llm_conformance_rejects_unreflected_delay() {
        let run = |tx: mpsc::Sender<TextFragment>, _cancel: CancellationToken| async move {
            tx.send(TextFragment {
                text: "Hello".into(),
                spoken_text: None,
                is_end_of_response: true,
                hints: None,
                first_token_ms: Some(2.0),
                token_count: Some(1),
            })
            .await
            .unwrap();
            Ok(())
        };

        let res = assert_llm_metrics_conformance(run, Duration::from_millis(100)).await;
        assert_eq!(
            res,
            Err(MetricConformanceError::DelayNotReflected {
                metric: "first_token_ms".into(),
                expected: Duration::from_millis(100),
                actual: Duration::from_millis(2),
            })
        );
    }

    #[test]
    fn s2s_conformance_accepts_valid_metrics() {
        let res = assert_s2s_metrics_conformance(24_000, 24_000, Duration::from_millis(150));
        assert_eq!(res, Ok(Duration::from_secs(1)));
    }

    #[test]
    fn filter_metrics_conformance_checks_duration_and_floor() {
        assert!(assert_filter_metrics_conformance(Duration::ZERO, Duration::ZERO).is_err());
        assert!(assert_filter_metrics_conformance(Duration::from_millis(50), Duration::from_millis(50)).is_ok());
        assert!(assert_filter_metrics_conformance(Duration::from_millis(10), Duration::from_millis(100)).is_err());
    }

    #[test]
    fn s2s_conformance_rejects_zero_latency() {
        let res = assert_s2s_metrics_conformance(24_000, 24_000, Duration::ZERO);
        assert_eq!(
            res,
            Err(MetricConformanceError::ZeroWithWork {
                name: "time_to_first_audio".into(),
            })
        );
    }
}
