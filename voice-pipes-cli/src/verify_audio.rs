//! `voice-pipes verify-audio` CLI subcommand handler.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use voice_pipes::config;
use voice_pipes::stt::SttProvider;
use voice_pipes_bench::audio_verify::AudioVerifier;
use voice_pipes_bench::audio_verify::golden::{GoldenSuite, approve_golden};

/// Arguments for the `voice-pipes verify-audio` subcommand.
pub struct VerifyAudioArgs {
    pub audio: Option<PathBuf>,
    pub golden: Option<PathBuf>,
    pub expected_text: Option<String>,
    pub stt_config: Option<PathBuf>,
    pub suite: Option<PathBuf>,
    pub json_out: Option<PathBuf>,
    pub json_stdout: bool,
    pub approve_to: Option<PathBuf>,
}

/// Run single-file or batch-suite audio verification and exit non-zero on failure.
pub async fn run_verify_audio(args: VerifyAudioArgs) -> Result<()> {
    let stt = load_optional_stt(args.stt_config.as_deref())?;

    if let Some(suite_path) = &args.suite {
        return run_suite(
            suite_path,
            stt.as_ref(),
            args.json_out.as_deref(),
            args.json_stdout,
        )
        .await;
    }

    let Some(audio_path) = &args.audio else {
        bail!("pass either `--audio <PATH>` or `--suite <GOLDEN_TOML>`");
    };

    let verifier = AudioVerifier::default();
    if let Some(dest) = &args.approve_to {
        let id = dest
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("golden_case");
        let approved = approve_golden(
            &verifier,
            audio_path,
            dest,
            id,
            args.expected_text.as_deref(),
            stt.as_ref(),
        )
        .await?;
        emit_json_if_requested(args.json_out.as_deref(), &approved)?;
        if args.json_stdout {
            println!("{}", serde_json::to_string_pretty(&approved)?);
        } else {
            println!("{}", approved.report);
            println!(
                "Approved golden WAV: {} (sha256={})",
                dest.display(),
                approved.sha256
            );
        }
        return Ok(());
    }

    let report = verifier
        .verify_wav(
            audio_path,
            args.expected_text.as_deref(),
            stt.as_ref(),
            args.golden.as_deref(),
        )
        .await?;

    emit_json_if_requested(args.json_out.as_deref(), &report)?;
    if args.json_stdout {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{report}");
    }

    if !report.passed() {
        bail!("audio verification failed for {}", audio_path.display());
    }
    Ok(())
}

async fn run_suite(
    suite_path: &Path,
    stt: Option<&Arc<dyn SttProvider>>,
    json_out: Option<&Path>,
    json_stdout: bool,
) -> Result<()> {
    let suite = GoldenSuite::load(suite_path)?;
    let base_dir = suite_path.parent().unwrap_or_else(|| Path::new("."));
    let report = suite.run(base_dir, stt).await?;

    emit_json_if_requested(json_out, &report)?;
    if json_stdout {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{report}");
    }

    if !report.passed {
        bail!(
            "golden suite failed: {}/{} cases passed",
            report.passed_count,
            report.total
        );
    }
    Ok(())
}

fn load_optional_stt(stt_config: Option<&Path>) -> Result<Option<Arc<dyn SttProvider>>> {
    let Some(path) = stt_config else {
        return Ok(None);
    };
    let raw = config::load(Some(path))
        .with_context(|| format!("failed to load STT config {}", path.display()))?;
    let cfg = config::merge(raw, config::CliOverrides::default());
    let provider = config::build_stt_provider(&cfg, None)?;
    Ok(Some(provider.into()))
}

fn emit_json_if_requested<T: serde::Serialize>(path: Option<&Path>, value: &T) -> Result<()> {
    let Some(out) = path else {
        return Ok(());
    };
    if let Some(parent) = out.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(value)?;
    std::fs::write(out, json).with_context(|| format!("failed to write {}", out.display()))?;
    Ok(())
}
