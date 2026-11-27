//! Automated 3-layer Audio Quality & Golden-Reference Verifier.
//!
//! Eliminates manual sound checks by grading synthesised audio on:
//! 1. **Acoustic & Noise Sanity** ([`acoustic`]): RMS, clipping, DC offset, speech-band
//!    energy, spectral flatness, segmental `SNR`, `HNR`, and digital crackle/click/pop
//!    detection (including 80 ms `Mimi` / 50 ms pacer chunk-boundary attribution).
//! 2. **Voice Intonation & Dynamics** ([`prosody`]): `YIN` `F0` pitch contour, semitone
//!    expressiveness, jitter, dynamic range, crest factor, shimmer, and syllable rate.
//! 3. **Pronunciation & Golden Comparison** ([`alignment`], [`mel_dtw`]): punctuation-normalized
//!    round-trip `STT` `WER`/`CER` + exact missing/substituted word diff, plus `DTW`-aligned
//!    Log-Mel cosine similarity, `MCD` (dB), `LTAS` timbre match, `F0` RMSE, and RMS correlation.

pub mod acoustic;
pub mod alignment;
pub mod golden;
pub mod mel_dtw;
pub mod prosody;

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use voice_pipes::audio::wav_reader::read_wav;
use voice_pipes::stt::SttProvider;

use acoustic::{AcousticMetrics, analyze_acoustics};
use alignment::{PronunciationReport, align_pronunciation};
use mel_dtw::{GoldenComparison, compare_to_golden};
use prosody::{DynamicsMetrics, IntonationMetrics, analyze_prosody};

/// Severity of a failed verification gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueSeverity {
    Fail,
    Warn,
}

/// One diagnostic gate failure or warning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyIssue {
    pub severity: IssueSeverity,
    pub check: String,
    pub message: String,
}

/// Configurable pass/fail thresholds for [`AudioVerifier`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VerifyThresholds {
    pub min_rms_dbfs: f64,
    pub max_rms_dbfs: f64,
    pub max_clipping_pct: f64,
    pub min_speech_band_ratio: f64,
    pub max_high_freq_ratio: f64,
    pub max_spectral_flatness: f64,
    pub min_hnr_db: f64,
    pub max_crackles_per_sec: f64,
    pub max_boundary_crackles: usize,
    pub max_wer: f64,
    pub min_word_recall: f64,
    pub min_mel_cosine: f64,
    pub max_mcd_db: f64,
}

impl Default for VerifyThresholds {
    fn default() -> Self {
        Self {
            min_rms_dbfs: -35.0,
            max_rms_dbfs: -5.0,
            max_clipping_pct: 0.5,
            min_speech_band_ratio: 0.50,
            max_high_freq_ratio: 0.25,
            max_spectral_flatness: 0.35,
            min_hnr_db: 6.0,
            max_crackles_per_sec: 2.0,
            max_boundary_crackles: 0,
            max_wer: 0.15,
            min_word_recall: 0.85,
            min_mel_cosine: 0.85,
            max_mcd_db: 9.0,
        }
    }
}

/// Full structured verification report across all enabled layers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyReport {
    pub passed: bool,
    pub acoustics: AcousticMetrics,
    pub intonation: IntonationMetrics,
    pub dynamics: DynamicsMetrics,
    pub pronunciation: Option<PronunciationReport>,
    pub golden: Option<GoldenComparison>,
    pub issues: Vec<VerifyIssue>,
}

impl VerifyReport {
    pub fn passed(&self) -> bool {
        self.passed
    }
}

impl fmt::Display for VerifyReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status = if self.passed { "PASS" } else { "FAIL" };
        let a = &self.acoustics;
        let i = &self.intonation;
        let d = &self.dynamics;
        writeln!(
            f,
            "[{status}] dur={:.2}s sr={}Hz RMS={:.1}dBFS clip={:.2}% SNR={:.1}dB HNR={:.1}dB",
            a.duration_s, a.sample_rate, a.rms_dbfs, a.clipping_pct, a.segmental_snr_db, a.hnr_db
        )?;
        writeln!(
            f,
            "  spectrum: speech_band={:.1}% hf(>6kHz)={:.1}% flatness={:.3} active={:.1}% lead_sil={:.0}ms",
            a.speech_band_ratio * 100.0,
            a.high_freq_ratio * 100.0,
            a.spectral_flatness,
            a.active_speech_ratio * 100.0,
            a.leading_silence_ms,
        )?;
        writeln!(
            f,
            "  crackle:  count={} ({:.2}/s) boundary_seams={} worst_mag={:.3}",
            a.crackle.crackle_count,
            a.crackle.crackles_per_sec,
            a.crackle.boundary_crackle_count,
            a.crackle.worst_crackle_magnitude,
        )?;
        writeln!(
            f,
            "  prosody:  F0={:?}Hz std={:.2}st range={:.2}st jitter={:.2}% dyn={:.1}dB shimmer={:.2}% rate={:.2}Hz",
            i.median_f0_hz.map(|v| (v * 10.0).round() / 10.0),
            i.f0_std_semitones,
            i.f0_range_semitones,
            i.jitter_pct,
            d.dynamic_range_db,
            d.shimmer_pct,
            d.syllable_rate_hz,
        )?;
        if let Some(p) = &self.pronunciation {
            writeln!(
                f,
                "  speech:   WER={:.3} CER={:.3} recall={}/{} ({:.1}%) missing={:?} sub={:?}",
                p.wer,
                p.cer,
                p.matched_words,
                p.expected_word_count,
                p.word_recall * 100.0,
                p.missing_words,
                p.substituted_words,
            )?;
        }
        if let Some(g) = &self.golden {
            writeln!(
                f,
                "  golden:   mel_cos={:.3} MCD={:.2}dB LTAS_cos={:.3} dur_ratio={:.2} F0_RMSE={:?}st env_corr={:.3}",
                g.mel_cosine_similarity,
                g.mcd_db,
                g.ltas_cosine_similarity,
                g.duration_ratio,
                g.f0_rmse_semitones.map(|v| (v * 100.0).round() / 100.0),
                g.rms_envelope_correlation,
            )?;
        }
        for issue in &self.issues {
            writeln!(
                f,
                "  {:?} [{}]: {}",
                issue.severity, issue.check, issue.message
            )?;
        }
        Ok(())
    }
}

/// Builder/runner for automated audio verification.
#[derive(Debug, Clone, Default)]
pub struct AudioVerifier {
    thresholds: VerifyThresholds,
}

impl AudioVerifier {
    pub fn new(thresholds: VerifyThresholds) -> Self {
        Self { thresholds }
    }

    /// Verify mono `f32` samples (`[-1.0, 1.0]`), optionally comparing against a golden waveform
    /// and/or an already-obtained STT transcript.
    pub fn verify_samples(
        &self,
        samples: &[f32],
        sample_rate: u32,
        expected_and_heard: Option<(&str, &str)>,
        golden: Option<(&[f32], u32)>,
    ) -> VerifyReport {
        let acoustics = analyze_acoustics(samples, sample_rate);
        let (intonation, dynamics) = analyze_prosody(samples, sample_rate);
        let pronunciation = expected_and_heard.map(|(exp, heard)| align_pronunciation(exp, heard));
        let golden_cmp = golden.map(|(g_samples, g_sr)| {
            let (g_int, g_dyn) = analyze_prosody(g_samples, g_sr);
            compare_to_golden(
                samples,
                sample_rate,
                &intonation,
                &dynamics,
                g_samples,
                g_sr,
                &g_int,
                &g_dyn,
            )
        });

        let issues = evaluate_gates(
            &self.thresholds,
            &acoustics,
            pronunciation.as_ref(),
            golden_cmp.as_ref(),
        );
        let passed = !issues.iter().any(|i| i.severity == IssueSeverity::Fail);
        VerifyReport {
            passed,
            acoustics,
            intonation,
            dynamics,
            pronunciation,
            golden: golden_cmp,
            issues,
        }
    }

    /// Verify a `.wav` file on disk (optionally against a golden `.wav` and/or local [`SttProvider`]).
    pub async fn verify_wav(
        &self,
        audio_path: &Path,
        expected_text: Option<&str>,
        stt: Option<&Arc<dyn SttProvider>>,
        golden_path: Option<&Path>,
    ) -> anyhow::Result<VerifyReport> {
        let frame = read_wav(audio_path)?;
        let samples = i16_to_mono_f32(&frame.samples, usize::from(frame.channels.max(1)));

        let heard_owned = if let (Some(_), Some(provider)) = (expected_text, stt) {
            let res = provider
                .transcribe(&frame)
                .await
                .map_err(|e| anyhow::anyhow!("round-trip STT failed: {e}"))?;
            Some(res.text)
        } else {
            None
        };

        let golden_loaded = if let Some(gp) = golden_path {
            let gf = read_wav(gp)?;
            let gs = i16_to_mono_f32(&gf.samples, usize::from(gf.channels.max(1)));
            Some((gs, gf.sample_rate))
        } else {
            None
        };

        let expected_and_heard = match (expected_text, heard_owned.as_deref()) {
            (Some(exp), Some(hrd)) => Some((exp, hrd)),
            _ => None,
        };
        let golden_ref = golden_loaded.as_ref().map(|(s, sr)| (s.as_slice(), *sr));

        Ok(self.verify_samples(&samples, frame.sample_rate, expected_and_heard, golden_ref))
    }
}

fn i16_to_mono_f32(samples: &[i16], channels: usize) -> Vec<f32> {
    let ch = channels.max(1);
    let mut out = Vec::with_capacity(samples.len() / ch);
    for frame in samples.chunks(ch) {
        let sum: f32 = frame.iter().map(|&s| f32::from(s) / 32768.0).sum();
        out.push(sum / (frame.len() as f32));
    }
    out
}

fn evaluate_gates(
    t: &VerifyThresholds,
    a: &AcousticMetrics,
    pron: Option<&PronunciationReport>,
    gold: Option<&GoldenComparison>,
) -> Vec<VerifyIssue> {
    let mut issues = Vec::new();
    let mut fail = |check: &str, msg: String| {
        issues.push(VerifyIssue {
            severity: IssueSeverity::Fail,
            check: check.to_string(),
            message: msg,
        });
    };

    if a.rms_dbfs < t.min_rms_dbfs || a.rms_dbfs > t.max_rms_dbfs {
        fail(
            "rms_dbfs",
            format!(
                "RMS {:.1} dBFS outside [{:.1}, {:.1}]",
                a.rms_dbfs, t.min_rms_dbfs, t.max_rms_dbfs
            ),
        );
    }
    if a.clipping_pct > t.max_clipping_pct {
        fail(
            "clipping",
            format!(
                "clipping {:.2}% > {:.2}%",
                a.clipping_pct, t.max_clipping_pct
            ),
        );
    }
    if a.speech_band_ratio < t.min_speech_band_ratio {
        fail(
            "speech_band",
            format!(
                "speech-band energy {:.1}% < {:.1}%",
                a.speech_band_ratio * 100.0,
                t.min_speech_band_ratio * 100.0
            ),
        );
    }
    if a.high_freq_ratio > t.max_high_freq_ratio {
        fail(
            "high_freq_noise",
            format!(
                "HF (>6kHz) energy {:.1}% > {:.1}%",
                a.high_freq_ratio * 100.0,
                t.max_high_freq_ratio * 100.0
            ),
        );
    }
    if a.spectral_flatness > t.max_spectral_flatness {
        fail(
            "spectral_flatness",
            format!(
                "spectral flatness {:.3} > {:.3} (noise/screeching)",
                a.spectral_flatness, t.max_spectral_flatness
            ),
        );
    }
    if a.hnr_db < t.min_hnr_db {
        fail(
            "hnr",
            format!("HNR {:.1} dB < {:.1} dB", a.hnr_db, t.min_hnr_db),
        );
    }
    if a.crackle.crackles_per_sec > t.max_crackles_per_sec {
        fail(
            "crackle",
            format!(
                "{} crackles ({:.2}/s > {:.2}/s)",
                a.crackle.crackle_count, a.crackle.crackles_per_sec, t.max_crackles_per_sec
            ),
        );
    }
    if a.crackle.boundary_crackle_count > t.max_boundary_crackles {
        fail(
            "boundary_crackle",
            format!(
                "{} chunk-boundary crackles detected",
                a.crackle.boundary_crackle_count
            ),
        );
    }
    evaluate_reference_gates(t, pron, gold, &mut issues);
    issues
}

fn evaluate_reference_gates(
    t: &VerifyThresholds,
    pron: Option<&PronunciationReport>,
    gold: Option<&GoldenComparison>,
    issues: &mut Vec<VerifyIssue>,
) {
    let mut fail = |check: &str, msg: String| {
        issues.push(VerifyIssue {
            severity: IssueSeverity::Fail,
            check: check.to_string(),
            message: msg,
        });
    };
    if let Some(p) = pron {
        if p.wer > t.max_wer {
            fail(
                "wer",
                format!(
                    "WER {:.3} > {:.3} (missing: {:?})",
                    p.wer, t.max_wer, p.missing_words
                ),
            );
        }
        if p.word_recall < t.min_word_recall {
            fail(
                "word_recall",
                format!(
                    "word recall {:.1}% < {:.1}%",
                    p.word_recall * 100.0,
                    t.min_word_recall * 100.0
                ),
            );
        }
    }
    if let Some(g) = gold {
        if g.mel_cosine_similarity < t.min_mel_cosine {
            fail(
                "golden_mel_cosine",
                format!(
                    "mel cosine {:.3} < {:.3}",
                    g.mel_cosine_similarity, t.min_mel_cosine
                ),
            );
        }
        if g.mcd_db > t.max_mcd_db {
            fail(
                "golden_mcd",
                format!("MCD {:.2} dB > {:.2} dB", g.mcd_db, t.max_mcd_db),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_voiced_speech(sr: u32, secs: f64) -> Vec<f32> {
        let n = (f64::from(sr) * secs) as usize;
        let mut out = Vec::with_capacity(n);
        let mut phase = 0.0_f64;
        for i in 0..n {
            let t = (i as f64) / f64::from(sr);
            // Smooth pitch modulation 180..240 Hz (expressive intonation) + 4 Hz syllable envelope.
            let f0 = 210.0 + 30.0 * (std::f64::consts::TAU * 1.2 * t).sin();
            phase += std::f64::consts::TAU * f0 / f64::from(sr);
            let env = 0.15 + 0.25 * (0.5 + 0.5 * (std::f64::consts::TAU * 4.0 * t).sin());
            let harmonic =
                0.6 * phase.sin() + 0.3 * (2.0 * phase).sin() + 0.1 * (3.0 * phase).sin();
            out.push((env * harmonic) as f32);
        }
        out
    }

    #[test]
    fn clean_voiced_signal_passes_and_reports_intonation_and_dynamics() {
        let sr = 24_000;
        let sig = synthetic_voiced_speech(sr, 2.0);
        let rep = AudioVerifier::default().verify_samples(&sig, sr, None, Some((&sig, sr)));
        assert!(rep.passed(), "{rep}");
        assert!(rep.acoustics.hnr_db > 15.0, "HNR={}", rep.acoustics.hnr_db);
        assert_eq!(rep.acoustics.crackle.crackle_count, 0);
        assert!(
            rep.intonation
                .median_f0_hz
                .is_some_and(|f0| (170.0..=250.0).contains(&f0)),
            "{:?}",
            rep.intonation.median_f0_hz
        );
        assert!(rep.intonation.f0_std_semitones > 1.0);
        assert!(rep.golden.as_ref().unwrap().mcd_db < 0.01);
        assert!(rep.golden.as_ref().unwrap().mel_cosine_similarity > 0.999);
    }

    #[test]
    fn crackle_detector_catches_chunk_boundary_pops() {
        let sr = 24_000;
        let mut sig = synthetic_voiced_speech(sr, 1.0);
        // Inject a sharp digital pop right at 80 ms (1920 samples = Mimi frame seam).
        sig[1920] = 0.95;
        sig[1921] = -0.95;
        let rep = AudioVerifier::default().verify_samples(&sig, sr, None, None);
        assert!(!rep.passed(), "boundary crackle must fail verification");
        assert!(rep.acoustics.crackle.crackle_count >= 1);
        assert!(rep.acoustics.crackle.boundary_crackle_count >= 1);
    }

    #[test]
    fn noise_and_silence_are_rejected_immediately() {
        let sr = 24_000;
        let silence = vec![0.0_f32; 24_000];
        assert!(
            !AudioVerifier::default()
                .verify_samples(&silence, sr, None, None)
                .passed()
        );

        // Deterministic pseudo-white noise (screech/static stand-in).
        let mut state = 0x1234_5678_9ABC_DEF0_u64;
        let noise: Vec<f32> = (0..24_000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                ((state >> 40) as f32 / (1u32 << 24) as f32) * 0.8 - 0.4
            })
            .collect();
        let rep = AudioVerifier::default().verify_samples(&noise, sr, None, None);
        assert!(!rep.passed(), "white noise must fail verification");
        assert!(rep.acoustics.spectral_flatness > 0.5);
    }

    #[test]
    fn pronunciation_alignment_reports_missing_and_substituted_words() {
        let p = align_pronunciation(
            "Hello! Welcome to Voice Pipes running natively in pure Rust.",
            "hello welcome to voice pipes running in clean rust",
        );
        assert_eq!(p.missing_words, vec!["natively"]);
        assert_eq!(p.substituted_words, vec![("pure".into(), "clean".into())]);
        assert_eq!(p.matched_words, 8);
    }

    #[tokio::test]
    async fn inspect_pocket_candle_alba_wav_if_present() {
        let wav = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("target/pocket_candle_alba.wav");
        if !wav.exists() {
            return;
        }
        let rep = AudioVerifier::default()
            .verify_wav(&wav, None, None, None)
            .await
            .expect("verify pocket_candle_alba.wav");
        println!("\n=== AudioVerifier report for pocket_candle_alba.wav ===\n{rep}");
        assert!(rep.passed(), "{rep}");
    }
}
