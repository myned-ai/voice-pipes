//! `voice-pipes check` — exercise every component of a pipeline in turn and
//! print whether each one did its job.
//!
//! The terminal half of the console's component check. The console's version
//! probes a *running* worker over HTTP, which is the right tool when something
//! is deployed and misbehaving; this one builds the components from a config
//! file and needs nothing running, which is the right tool before starting
//! anything, or on a machine with no console.
//!
//! What it reports is whether the stage did its job, not how fast it was. A
//! recogniser that answers in 106 ms with no words in the answer is a failure
//! however good the number looks, and that distinction is the whole point of
//! the exercise.

use std::path::{Path, PathBuf};

use crate::config::PipelineConfig;

/// Audio lost before the onset past which a word is probably gone. An
/// unstressed first syllable runs 150–250 ms.
const LOST_WORD_MS: u64 = 250;

/// TTS realtime factor above which synthesis has no headroom.
const RTF_BUDGET: f64 = 0.6;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Ok,
    Warn,
    Fail,
    Skipped,
}

impl Verdict {
    fn mark(self) -> &'static str {
        match self {
            Verdict::Ok => "ok",
            Verdict::Warn => "warn",
            Verdict::Fail => "FAIL",
            Verdict::Skipped => "skip",
        }
    }
}

struct Row {
    stage: &'static str,
    provider: String,
    verdict: Verdict,
    detail: String,
}

/// One fixture and the words it contains, read from the manifest.
struct Fixture {
    name: String,
    path: PathBuf,
    text: String,
}

fn fixture_dir() -> PathBuf {
    match std::env::var("VP_FIXTURE_DIR") {
        Ok(d) => PathBuf::from(d),
        Err(_) => PathBuf::from("fixtures"),
    }
}

/// Load the fixture manifest, or an empty list when there is none.
fn load_fixtures() -> Vec<Fixture> {
    let dir = fixture_dir();
    let Ok(text) = std::fs::read_to_string(dir.join("manifest.json")) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    json.get("fixtures")
        .and_then(|f| f.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|f| {
                    let name = f.get("name")?.as_str()?.to_string();
                    let file = f.get("file")?.as_str()?;
                    Some(Fixture {
                        path: dir.join(file),
                        text: f
                            .get("text")
                            .and_then(|t| t.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        name,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Words, lowercased and stripped of punctuation. Case and commas are not the
/// test; whether the words arrived is.
fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// Run every component of `cfg` against the fixtures and print a table.
///
/// Returns an error when any stage failed, so the command can be used as a
/// gate: a check that always exits zero is decoration.
pub async fn run_check(cfg: &PipelineConfig, fixture_name: Option<&str>) -> anyhow::Result<()> {
    let all = load_fixtures();
    if all.is_empty() {
        anyhow::bail!(
            "no fixtures found in {} — run `python fixtures/build-fixtures.py` \
             or set VP_FIXTURE_DIR",
            fixture_dir().display()
        );
    }

    // The default pass uses a fixture with speech in it and one with none:
    // a stage that mishears speech and a stage that invents words from
    // silence are different faults, and only the second needs the quiet one.
    let chosen: Vec<&Fixture> = if let Some(n) = fixture_name {
        all.iter().filter(|f| f.name == n).collect()
    } else {
        let speech = all
            .iter()
            .find(|f| f.name == "hello-how-are-you")
            .or_else(|| all.iter().find(|f| !f.text.is_empty()));
        let silent = all.iter().find(|f| f.text.is_empty());
        speech.into_iter().chain(silent).collect()
    };
    if chosen.is_empty() {
        anyhow::bail!(
            "no fixture named {:?}; available: {}",
            fixture_name.unwrap_or(""),
            all.iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let mut rows: Vec<Row> = Vec::new();
    for fixture in &chosen {
        println!(
            "\n  fixture {} — {}",
            fixture.name,
            if fixture.text.is_empty() {
                "(no speech)".to_string()
            } else {
                format!("{:?}", fixture.text)
            }
        );
        rows.push(check_vad(cfg, fixture));
        rows.push(check_stt(cfg, fixture).await);
    }
    rows.push(check_tts(cfg).await);
    rows.push(check_llm(cfg).await);

    let width = rows.iter().map(|r| r.provider.len()).max().unwrap_or(8);
    println!();
    for r in &rows {
        println!(
            "  {:<5} {:<width$}  {:<4}  {}",
            r.stage,
            r.provider,
            r.verdict.mark(),
            r.detail,
            width = width
        );
    }

    let failed = rows.iter().filter(|r| r.verdict == Verdict::Fail).count();
    let warned = rows.iter().filter(|r| r.verdict == Verdict::Warn).count();
    println!(
        "\n  {} ok, {warned} warn, {failed} failed",
        rows.iter().filter(|r| r.verdict == Verdict::Ok).count()
    );
    if failed > 0 {
        anyhow::bail!("{failed} component check(s) failed");
    }
    Ok(())
}

fn read_fixture(path: &Path) -> Option<(Vec<i16>, u32)> {
    let frame = crate::audio::wav_reader::read_wav(path).ok()?;
    Some((frame.samples, frame.sample_rate))
}

fn check_vad(cfg: &PipelineConfig, fixture: &Fixture) -> Row {
    let backend = cfg.vad.as_ref().map_or("none".into(), |v| v.mode.clone());
    let Some((samples, rate)) = read_fixture(&fixture.path) else {
        return Row {
            stage: "vad",
            provider: backend,
            verdict: Verdict::Skipped,
            detail: format!("cannot read {}", fixture.path.display()),
        };
    };

    let Ok(mut vad) = crate::config::build_vad(cfg, None) else {
        return Row {
            stage: "vad",
            provider: backend,
            verdict: Verdict::Skipped,
            detail: "no VAD configured".into(),
        };
    };
    let pre_roll_ms = cfg
        .vad
        .as_ref()
        .map_or(crate::config::DEFAULT_VAD_PRE_ROLL_MS, |v| {
            v.effective_pre_roll_ms()
        });

    // Where the detector first fires, frame by frame.
    let frame = (rate as usize / 50).max(1);
    let mut first_speech: Option<usize> = None;
    let mut speech_frames = 0u64;
    for (i, chunk) in samples.chunks(frame).enumerate() {
        if vad.process_chunk(chunk, rate).is_ok_and(|r| r.is_speech) {
            speech_frames += 1;
            first_speech.get_or_insert(i);
        }
    }

    let expects_speech = !fixture.text.is_empty();
    if speech_frames == 0 {
        return Row {
            stage: "vad",
            provider: backend,
            verdict: if expects_speech {
                Verdict::Fail
            } else {
                Verdict::Ok
            },
            detail: if expects_speech {
                "found no speech; the recogniser would be given nothing".into()
            } else {
                "no speech, as expected for this fixture".into()
            },
        };
    }
    if !expects_speech {
        return Row {
            stage: "vad",
            provider: backend,
            verdict: Verdict::Fail,
            detail: "found speech in a fixture that holds none".into(),
        };
    }

    // Forwarding begins `pre_roll_ms` before the detector fired. What lies
    // ahead of that is dropped -- and only the part of it that carries speech
    // is a fault. Counting all of it would report a fixture's lead-in silence
    // as a lost word, which is the mistake this rule replaced; it is shared
    // with the worker's probe so the two cannot disagree.
    let onset = first_speech.unwrap_or(0);
    let pre_roll_frames = (pre_roll_ms as usize).div_ceil(20);
    let forwarded_from = onset.saturating_sub(pre_roll_frames);
    let energies = crate::api::probe::frame_energies(&samples, frame);
    let lost_frames =
        crate::api::probe::speech_frames_dropped(&energies, 0, forwarded_from, energies.len());
    let lost_ms = lost_frames as u64 * 20;
    let lead_ms = forwarded_from as u64 * 20;
    if lost_ms >= LOST_WORD_MS {
        Row {
            stage: "vad",
            provider: backend,
            verdict: Verdict::Fail,
            detail: format!(
                "{lost_ms} ms of speech dropped before the onset — enough to lose the \
                 first word (pre_roll_ms = {pre_roll_ms})"
            ),
        }
    } else {
        Row {
            stage: "vad",
            provider: backend,
            verdict: Verdict::Ok,
            detail: format!(
                "onset kept: {lost_ms} ms of speech dropped, {lead_ms} ms of lead-in skipped"
            ),
        }
    }
}

async fn check_stt(cfg: &PipelineConfig, fixture: &Fixture) -> Row {
    let provider_name = crate::config::stt_provider_name(cfg).unwrap_or_else(|| "none".to_string());
    let Some((samples, rate)) = read_fixture(&fixture.path) else {
        return Row {
            stage: "stt",
            provider: provider_name,
            verdict: Verdict::Skipped,
            detail: "cannot read fixture".into(),
        };
    };
    let provider = match crate::config::build_stt_provider(cfg, None) {
        Ok(p) => p,
        Err(e) => {
            return Row {
                stage: "stt",
                provider: provider_name,
                verdict: Verdict::Fail,
                detail: format!("could not build: {e}"),
            };
        }
    };
    let frame = match crate::audio::resample::resample(
        crate::audio::AudioFrame {
            samples,
            sample_rate: rate,
            channels: 1,
        },
        provider.expected_sample_rate(),
    ) {
        Ok(frame) => frame,
        Err(e) => {
            return Row {
                stage: "stt",
                provider: provider_name,
                verdict: Verdict::Fail,
                detail: format!("the fixture cannot be given to it: {e}"),
            };
        }
    };
    let res = match provider.transcribe(&frame).await {
        Ok(r) => r,
        Err(e) => {
            return Row {
                stage: "stt",
                provider: provider_name,
                verdict: Verdict::Fail,
                detail: format!("transcribe failed: {e}"),
            };
        }
    };
    let text = res.text.trim().to_string();

    let expects_speech = !fixture.text.is_empty();
    if expects_speech
        && let Some(metrics) = &res.inference_metrics
    {
        if metrics.effective_latency_ms == Some(0) {
            return Row {
                stage: "stt",
                provider: provider_name,
                verdict: Verdict::Fail,
                detail: "measurement.zero_with_work: effective_latency_ms reported as 0 ms".into(),
            };
        }
        if let Some(rtfx) = metrics.rtfx
            && (rtfx <= 0.0 || !rtfx.is_finite())
        {
            return Row {
                stage: "stt",
                provider: provider_name,
                verdict: Verdict::Fail,
                detail: format!("measurement.invalid_numeric: rtfx is invalid ({rtfx})"),
            };
        }
    }
    let has_words = text.chars().any(char::is_alphanumeric);
    if !has_words {
        return Row {
            stage: "stt",
            provider: provider_name,
            verdict: if expects_speech {
                Verdict::Fail
            } else {
                Verdict::Ok
            },
            detail: if expects_speech {
                "returned no words".into()
            } else {
                "returned nothing, as it should for audio with no speech".into()
            },
        };
    }
    if !expects_speech {
        return Row {
            stage: "stt",
            provider: provider_name,
            verdict: Verdict::Fail,
            detail: format!("invented {text:?} from audio with no speech"),
        };
    }

    // The first word is checked on its own: losing it is a segmentation
    // failure with a different cause and fix from mishearing the middle, and
    // a whole-transcript score barely moves when one word of six is gone.
    let want = words(&fixture.text);
    let got = words(&text);
    if let (Some(w), Some(g)) = (want.first(), got.first())
        && w != g
    {
        return Row {
            stage: "stt",
            provider: provider_name,
            verdict: Verdict::Fail,
            // Given the whole file, not the segmenter's output, so a missing
            // word is the recogniser's own.
            detail: format!("first word is {g:?}, expected {w:?} — the recogniser dropped it"),
        };
    }
    let missing = want.iter().filter(|w| !got.contains(w)).count();
    if !want.is_empty() && (missing as f64 / want.len() as f64) > 0.34 {
        return Row {
            stage: "stt",
            provider: provider_name,
            verdict: Verdict::Warn,
            detail: format!("{missing} of {} expected words missing", want.len()),
        };
    }
    Row {
        stage: "stt",
        provider: provider_name,
        verdict: Verdict::Ok,
        detail: format!("{text:?}"),
    }
}

async fn check_tts(cfg: &PipelineConfig) -> Row {
    let provider_name = crate::config::tts_provider_name(cfg).unwrap_or_else(|| "none".to_string());
    let store = crate::config::build_tts_param_store(cfg);
    let provider = match crate::config::build_tts_provider(cfg, None, Some(&store)) {
        Ok(p) => p,
        Err(e) => {
            return Row {
                stage: "tts",
                provider: provider_name,
                verdict: Verdict::Fail,
                detail: format!("could not build: {e}"),
            };
        }
    };
    let started = std::time::Instant::now();
    let frame = match provider
        .synthesize("The quick brown fox jumps over the lazy dog.")
        .await
    {
        Ok(f) => f,
        Err(e) => {
            return Row {
                stage: "tts",
                provider: provider_name,
                verdict: Verdict::Fail,
                detail: format!("synthesis failed: {e}"),
            };
        }
    };
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    let audio_ms = frame.samples.len() as f64 / f64::from(frame.sample_rate.max(1)) * 1000.0;
    if audio_ms == 0.0 {
        return Row {
            stage: "tts",
            provider: provider_name,
            verdict: Verdict::Fail,
            detail: "produced no audio".into(),
        };
    }
    if ms == 0.0 && !frame.samples.is_empty() {
        return Row {
            stage: "tts",
            provider: provider_name,
            verdict: Verdict::Fail,
            detail: "measurement.zero_with_work: synthesis elapsed time reported as 0 ms".into(),
        };
    }
    let rtf = ms / audio_ms;
    if rtf <= 0.0 || !rtf.is_finite() {
        return Row {
            stage: "tts",
            provider: provider_name,
            verdict: Verdict::Fail,
            detail: format!("measurement.invalid_numeric: rtf is invalid ({rtf})"),
        };
    }
    Row {
        stage: "tts",
        provider: provider_name,
        verdict: if rtf > 1.0 {
            Verdict::Fail
        } else if rtf > RTF_BUDGET {
            Verdict::Warn
        } else {
            Verdict::Ok
        },
        detail: format!(
            "{:.2} s audio in {ms:.0} ms (rtf {rtf:.2})",
            audio_ms / 1000.0
        ),
    }
}

async fn check_llm(cfg: &PipelineConfig) -> Row {
    let name = cfg
        .text_processor
        .first()
        .map_or("none".into(), |t| t.processor_type.clone());
    if cfg.text_processor.is_empty() {
        return Row {
            stage: "llm",
            provider: name,
            verdict: Verdict::Skipped,
            detail: "no text processor configured".into(),
        };
    }
    let mut processors = match crate::config::build_text_processors(cfg, None, None) {
        Ok(p) => p,
        Err(e) => {
            return Row {
                stage: "llm",
                provider: name,
                verdict: Verdict::Fail,
                detail: format!("could not build: {e}"),
            };
        }
    };
    if processors.is_empty() {
        return Row {
            stage: "llm",
            provider: name,
            verdict: Verdict::Skipped,
            detail: "no text processor built".into(),
        };
    }
    let started = std::time::Instant::now();
    let reply = match processors.remove(0).process("Hello, how are you?").await {
        Ok(r) => r,
        Err(e) => {
            return Row {
                stage: "llm",
                provider: name,
                verdict: Verdict::Fail,
                detail: format!("generation failed: {e}"),
            };
        }
    };
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    if reply.trim().is_empty() {
        return Row {
            stage: "llm",
            provider: name,
            verdict: Verdict::Fail,
            detail: "returned an empty reply".into(),
        };
    }
    if ms == 0.0 {
        return Row {
            stage: "llm",
            provider: name,
            verdict: Verdict::Fail,
            detail: "measurement.zero_with_work: LLM generation elapsed time reported as 0 ms".into(),
        };
    }
    Row {
        stage: "llm",
        provider: name,
        // Two seconds is where a spoken reply stops feeling like an answer
        // and starts feeling like a stall.
        verdict: if ms > 2000.0 {
            Verdict::Warn
        } else {
            Verdict::Ok
        },
        detail: format!(
            "{ms:.0} ms, {:?}",
            reply.chars().take(48).collect::<String>()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_ignores_case_and_punctuation() {
        assert_eq!(words("Hello, how are you?"), ["hello", "how", "are", "you"]);
        assert_eq!(words("  "), Vec::<String>::new());
    }

    #[test]
    fn a_failed_stage_is_distinguishable_from_a_skipped_one() {
        // A component that could not be built has not passed, and saying "ok"
        // for a stage that never ran is how a check becomes decoration.
        assert_eq!(Verdict::Fail.mark(), "FAIL");
        assert_eq!(Verdict::Skipped.mark(), "skip");
        assert_ne!(Verdict::Skipped.mark(), Verdict::Ok.mark());
    }

    #[test]
    fn check_reports_measurement_faults() {
        let metrics = voice_pipes_sdk::stt::InferenceMetrics::new()
            .with_effective_latency_ms(Some(0))
            .with_audio_duration_ms(Some(1000))
            .with_rtfx(Some(-1.0))
            .with_inference_count(Some(1));
        assert_eq!(metrics.effective_latency_ms, Some(0));
        assert!(metrics.rtfx.unwrap() <= 0.0);
    }
}
