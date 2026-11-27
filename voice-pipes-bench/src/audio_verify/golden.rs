//! Golden-reference suite manifest (`golden.toml`), batch runner, and approval workflow.
//!
//! Enables repeatable regression verification across multiple voices, languages, and
//! pre-approved golden `.wav` baselines with optional SHA-256 integrity checks.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use voice_pipes::stt::SttProvider;

use super::{AudioVerifier, VerifyReport, VerifyThresholds};

/// A batch suite manifest loaded from a `golden.toml` file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldenSuite {
    /// Optional human-readable suite name.
    #[serde(default)]
    pub name: Option<String>,
    /// Suite-wide default thresholds (individual cases may override).
    #[serde(default)]
    pub thresholds: VerifyThresholds,
    /// Ordered list of audio verification cases.
    #[serde(rename = "case", default)]
    pub cases: Vec<GoldenCase>,
}

/// One verification case inside a [`GoldenSuite`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldenCase {
    /// Unique identifier for this test case (e.g. `pocket_alba_en_hello`).
    pub id: String,
    /// Candidate `.wav` file path (resolved relative to the manifest directory if relative).
    pub audio_path: PathBuf,
    /// Optional pre-approved golden `.wav` file path.
    #[serde(default)]
    pub golden_path: Option<PathBuf>,
    /// Optional expected SHA-256 hex digest of `golden_path` for tamper/drift detection.
    #[serde(default)]
    pub golden_sha256: Option<String>,
    /// Optional expected prompt text for Layer 2 round-trip STT pronunciation alignment.
    #[serde(default)]
    pub expected_text: Option<String>,
    /// Optional voice label (informational metadata).
    #[serde(default)]
    pub voice: Option<String>,
    /// Optional BCP-47 language tag (informational metadata).
    #[serde(default)]
    pub language: Option<String>,
    /// Optional per-case threshold overrides.
    #[serde(default)]
    pub thresholds: Option<VerifyThresholds>,
}

/// Verification result for one [`GoldenCase`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseReport {
    pub id: String,
    pub audio_path: PathBuf,
    pub golden_path: Option<PathBuf>,
    pub report: VerifyReport,
}

/// Aggregated result for an entire [`GoldenSuite`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteReport {
    pub name: Option<String>,
    pub passed: bool,
    pub total: usize,
    pub passed_count: usize,
    pub failed_count: usize,
    pub cases: Vec<CaseReport>,
}

impl fmt::Display for SuiteReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status = if self.passed { "PASS" } else { "FAIL" };
        let title = self.name.as_deref().unwrap_or("golden-suite");
        writeln!(
            f,
            "[{status}] {title}: {}/{} cases passed ({} failed)",
            self.passed_count, self.total, self.failed_count
        )?;
        for case in &self.cases {
            writeln!(f, "--- case: {} ---", case.id)?;
            write!(f, "{}", case.report)?;
        }
        Ok(())
    }
}

/// Result returned after approving a candidate `.wav` as a golden reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovedEntry {
    pub case: GoldenCase,
    pub sha256: String,
    pub report: VerifyReport,
}

impl GoldenSuite {
    /// Load and parse a `golden.toml` suite manifest from disk.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read golden suite {}", path.display()))?;
        let suite: Self = toml::from_str(&raw)
            .with_context(|| format!("failed to parse golden suite {}", path.display()))?;
        Ok(suite)
    }

    /// Execute all cases in the suite, resolving relative paths against `base_dir`.
    pub async fn run(
        &self,
        base_dir: &Path,
        stt: Option<&Arc<dyn SttProvider>>,
    ) -> Result<SuiteReport> {
        let mut results = Vec::with_capacity(self.cases.len());
        let mut passed_count = 0usize;

        for case in &self.cases {
            let audio_path = resolve_path(base_dir, &case.audio_path);
            let golden_path = case.golden_path.as_ref().map(|p| resolve_path(base_dir, p));

            if let (Some(gp), Some(expected_hash)) = (&golden_path, &case.golden_sha256) {
                let actual = sha256_file(gp)?;
                if !actual.eq_ignore_ascii_case(expected_hash) {
                    bail!(
                        "golden SHA-256 mismatch for case '{}': expected {expected_hash}, got {actual}",
                        case.id
                    );
                }
            }

            let thresholds = case
                .thresholds
                .clone()
                .unwrap_or_else(|| self.thresholds.clone());
            let verifier = AudioVerifier::new(thresholds);
            let report = verifier
                .verify_wav(
                    &audio_path,
                    case.expected_text.as_deref(),
                    stt,
                    golden_path.as_deref(),
                )
                .await
                .with_context(|| format!("case '{}' failed to run", case.id))?;

            if report.passed() {
                passed_count += 1;
            }
            results.push(CaseReport {
                id: case.id.clone(),
                audio_path,
                golden_path,
                report,
            });
        }

        let total = results.len();
        let failed_count = total.saturating_sub(passed_count);
        Ok(SuiteReport {
            name: self.name.clone(),
            passed: failed_count == 0,
            total,
            passed_count,
            failed_count,
            cases: results,
        })
    }
}

/// Verify `audio_path`, and if it passes all gates, copy it to `dest_golden` and return
/// a ready-to-serialize [`ApprovedEntry`] with its SHA-256 digest.
pub async fn approve_golden(
    verifier: &AudioVerifier,
    audio_path: &Path,
    dest_golden: &Path,
    id: &str,
    expected_text: Option<&str>,
    stt: Option<&Arc<dyn SttProvider>>,
) -> Result<ApprovedEntry> {
    let report = verifier
        .verify_wav(audio_path, expected_text, stt, None)
        .await?;
    if !report.passed() {
        bail!(
            "refusing to approve {}: verification failed:\n{report}",
            audio_path.display()
        );
    }
    if let Some(parent) = dest_golden.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }
    std::fs::copy(audio_path, dest_golden).with_context(|| {
        format!(
            "failed to copy {} to {}",
            audio_path.display(),
            dest_golden.display()
        )
    })?;
    let sha256 = sha256_file(dest_golden)?;
    let case = GoldenCase {
        id: id.to_string(),
        audio_path: audio_path.to_path_buf(),
        golden_path: Some(dest_golden.to_path_buf()),
        golden_sha256: Some(sha256.clone()),
        expected_text: expected_text.map(ToString::to_string),
        voice: None,
        language: None,
        thresholds: None,
    };
    Ok(ApprovedEntry {
        case,
        sha256,
        report,
    })
}

/// Compute lowercase hex SHA-256 digest of a file on disk.
pub fn sha256_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read {} for SHA-256", path.display()))?;
    let digest = Sha256::digest(&bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        use fmt::Write as _;
        let _ = write!(&mut hex, "{b:02x}");
    }
    Ok(hex)
}

fn resolve_path(base_dir: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base_dir.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn write_test_wav(path: &Path, samples: &[f32], sample_rate: u32) {
        let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
        let data_len = u32::try_from(samples.len() * 2).expect("len");
        let byte_rate = sample_rate * 2;
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
        bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&byte_rate.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes()); // block align
        bytes.extend_from_slice(&16u16.to_le_bytes()); // bits
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for &s in samples {
            let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        std::fs::write(path, bytes).expect("write wav");
    }

    fn synth_clean_speech_like(sr: u32, dur_s: f64) -> Vec<f32> {
        let n = (dur_s * f64::from(sr)) as usize;
        let mut out = Vec::with_capacity(n);
        let mut phase = 0.0f64;
        for i in 0..n {
            let t = (i as f64) / f64::from(sr);
            let f0 = 180.0 + 35.0 * (2.0 * PI * 1.4 * t).sin();
            phase += 2.0 * PI * f0 / f64::from(sr);
            let env = 0.55 + 0.45 * (2.0 * PI * 3.5 * t).sin();
            let s =
                0.22 * env * (phase.sin() + 0.5 * (2.0 * phase).sin() + 0.25 * (3.0 * phase).sin());
            out.push(s as f32);
        }
        out
    }

    #[tokio::test]
    async fn golden_suite_loads_runs_and_verifies_sha256() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cand_wav = tmp.path().join("cand.wav");
        let gold_wav = tmp.path().join("gold.wav");
        let clean = synth_clean_speech_like(24_000, 1.2);
        write_test_wav(&cand_wav, &clean, 24_000);
        write_test_wav(&gold_wav, &clean, 24_000);
        let hash = sha256_file(&gold_wav).expect("hash");

        let manifest_path = tmp.path().join("golden.toml");
        let manifest = format!(
            "name = \"smoke\"\n\n[[case]]\nid = \"clean_match\"\naudio_path = \"cand.wav\"\ngolden_path = \"gold.wav\"\ngolden_sha256 = \"{hash}\"\n"
        );
        std::fs::write(&manifest_path, manifest).expect("write toml");

        let suite = GoldenSuite::load(&manifest_path).expect("load");
        let report = suite.run(tmp.path(), None).await.expect("run");
        assert!(report.passed, "{report}");
        assert_eq!(report.passed_count, 1);
        assert_eq!(report.failed_count, 0);

        // Tampered hash must fail loudly.
        let mut bad_suite = suite.clone();
        bad_suite.cases[0].golden_sha256 = Some("0000".repeat(16));
        assert!(bad_suite.run(tmp.path(), None).await.is_err());
    }

    #[tokio::test]
    async fn approve_golden_accepts_clean_and_rejects_silence() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let clean_wav = tmp.path().join("clean.wav");
        let silent_wav = tmp.path().join("silent.wav");
        let dest = tmp.path().join("approved/ref.wav");

        write_test_wav(&clean_wav, &synth_clean_speech_like(24_000, 1.0), 24_000);
        write_test_wav(&silent_wav, &vec![0.0f32; 24_000], 24_000);

        let verifier = AudioVerifier::default();
        assert!(
            approve_golden(&verifier, &silent_wav, &dest, "bad", None, None)
                .await
                .is_err()
        );
        assert!(!dest.exists());

        let approved = approve_golden(&verifier, &clean_wav, &dest, "good", None, None)
            .await
            .expect("approve clean");
        assert!(dest.exists());
        assert_eq!(approved.sha256.len(), 64);
        assert!(approved.report.passed());
    }
}
