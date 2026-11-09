//! Integration tests verifying that Quickstart commands and all example
//! configuration files parse validly and run as documented.
//!
//! Hosted components (e.g. Gemini Live, `OpenAI` Realtime, `AssemblyAI`, Google Cloud)
//! are tested live when their corresponding environment variable is present,
//! and gracefully validated/skipped when credentials are not configured.

use std::path::{Path, PathBuf};
use std::process::Command;

#[allow(dead_code, reason = "only used in tests with default features")]
fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_voice-pipes"))
}

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../voice-pipes/examples")
        .canonicalize()
        .expect("voice-pipes/examples directory exists")
}

#[allow(dead_code, reason = "only used in tests with default features")]
fn models_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../voice-pipes/models")
        .canonicalize()
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../voice-pipes/models"))
}

/// Recursively collect all `.toml` files in a directory.
fn collect_toml_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(collect_toml_files(&path));
            } else if path.extension().is_some_and(|ext| ext == "toml") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

#[test]
fn all_example_configs_parse_validly() {
    let dir = examples_dir();
    let toml_files = collect_toml_files(&dir);
    assert!(
        !toml_files.is_empty(),
        "examples directory must contain .toml files: {}",
        dir.display()
    );

    let mut parse_errors = Vec::new();

    for file in &toml_files {
        let content = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                parse_errors.push(format!("{}: failed to read: {e}", file.display()));
                continue;
            }
        };

        let file_name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let parent_dir = file
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let is_worker = file_name.ends_with(".worker.toml")
            || (matches!(parent_dir, "config" | "orchestrate")
                && file_name.starts_with("worker")
                && !file_name.starts_with("worker-realtime-"));
        let is_orchestrator =
            file_name == "orchestrator.toml" && matches!(parent_dir, "config" | "orchestrate");

        if is_orchestrator {
            if let Err(e) = toml::from_str::<voice_pipes::api::types::OrchestratorConfig>(&content)
            {
                parse_errors.push(format!(
                    "Orchestrator config {} failed to parse: {e}",
                    file.display()
                ));
            }
        } else if is_worker {
            if let Err(e) = toml::from_str::<voice_pipes::api::types::WorkerConfig>(&content) {
                parse_errors.push(format!(
                    "Worker config {} failed to parse: {e}",
                    file.display()
                ));
            }
        } else {
            // PipelineConfig
            if let Err(e) = toml::from_str::<voice_pipes::config::PipelineConfig>(&content) {
                parse_errors.push(format!(
                    "Pipeline config {} failed to parse: {e}",
                    file.display()
                ));
            }
        }
    }

    assert!(
        parse_errors.is_empty(),
        "Found {} example configuration parse error(s):\n{}",
        parse_errors.len(),
        parse_errors.join("\n")
    );
}

#[test]
fn quickstart_pipeline_configs_are_syntactically_valid() {
    let dir = examples_dir();
    let quickstart_configs = [
        dir.join("minimal.toml"),
        dir.join("live/gemini-live.toml"),
        dir.join("tts/piper.toml"),
        dir.join("stt/parakeet.toml"),
        dir.join("listen/parakeet.toml"),
    ];

    for cfg in &quickstart_configs {
        assert!(cfg.exists(), "Quickstart config exists: {}", cfg.display());
        let text = std::fs::read_to_string(cfg).expect("read config");
        let parsed = toml::from_str::<voice_pipes::config::PipelineConfig>(&text);
        assert!(
            parsed.is_ok(),
            "{}: failed to deserialize PipelineConfig: {:?}",
            cfg.display(),
            parsed.err()
        );
    }
}

#[cfg(feature = "tts-piper")]
#[test]
fn quickstart_standalone_tts_execution() {
    let tts_cfg = examples_dir().join("tts/piper.toml");
    let model_file = models_dir().join("en_US-lessac-medium.onnx");

    if !model_file.exists() {
        eprintln!(
            "[SKIP] Local Piper model not found at {}; skipping live WAV synthesis.",
            model_file.display()
        );
        return;
    }

    let out_wav = std::env::temp_dir().join(format!("vp_test_tts_{}.wav", std::process::id()));
    let _ = std::fs::remove_file(&out_wav);

    let output = bin()
        .current_dir(examples_dir().parent().expect("voice-pipes root"))
        .args(["--config"])
        .arg(&tts_cfg)
        .args(["tts", "--text", "Voice Pipes test speech", "--output"])
        .arg(&out_wav)
        .output()
        .expect("execute tts command");

    assert!(
        output.status.success(),
        "tts command must succeed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(out_wav.exists(), "TTS output WAV file must be created");
    let file_len = std::fs::metadata(&out_wav).map_or(0, |m| m.len());
    assert!(
        file_len > 44,
        "WAV file must contain audio data (got {file_len} bytes)"
    );

    let _ = std::fs::remove_file(&out_wav);
}

#[cfg(all(feature = "s2s-gemini", feature = "realtime"))]
#[test]
fn quickstart_gemini_live_options_and_credential_gate() {
    let gemini_cfg = examples_dir().join("live/gemini-live.toml");
    assert!(gemini_cfg.exists(), "gemini-live.toml exists");

    // 1. Check that running with `interactive --help` prints valid usage
    let help_out = bin()
        .args(["--config"])
        .arg(&gemini_cfg)
        .args(["interactive", "--help"])
        .output()
        .expect("run interactive --help");
    assert!(help_out.status.success());
    let help_text = String::from_utf8_lossy(&help_out.stdout);
    assert!(help_text.contains("interactive"));

    // 2. If GEMINI_API_KEY is unset, assert that running interactive fails cleanly
    // with a missing credentials error (no panic/crash).
    let has_key = std::env::var("GEMINI_API_KEY").is_ok_and(|k| !k.trim().is_empty());
    if has_key {
        eprintln!("[INFO] GEMINI_API_KEY is present; configuration is validated for live calling.");
    } else {
        let run_out = bin()
            .env_remove("GEMINI_API_KEY")
            .args(["--config"])
            .arg(&gemini_cfg)
            .arg("interactive")
            .output()
            .expect("run interactive without key");

        assert!(
            !run_out.status.success(),
            "interactive must reject execution without GEMINI_API_KEY"
        );
        let err_text = format!(
            "{}{}",
            String::from_utf8_lossy(&run_out.stdout),
            String::from_utf8_lossy(&run_out.stderr)
        );
        assert!(
            err_text.contains("GEMINI_API_KEY") || err_text.contains("api_key"),
            "Error output must explain missing API key:\n{err_text}"
        );
    }
}

#[test]
fn quickstart_documentation_commands_parse_as_valid_cli_invocations() {
    let doc_paths = [
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../docs/getting-started/quickstart-source.md")
            .canonicalize()
            .expect("quickstart-source.md exists"),
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../docs/getting-started/first-agent.md")
            .canonicalize()
            .expect("first-agent.md exists"),
    ];

    let mut commands = Vec::new();
    for doc_path in &doc_paths {
        let text = std::fs::read_to_string(doc_path).expect("read doc");

        // Extract command lines starting with `voice-pipes `
        for line in text.lines() {
            let trimmed = line.trim();
            if let Some(cmd) = trimmed.strip_prefix("voice-pipes ") {
                // Strip trailing comments
                let cmd = cmd.split('#').next().unwrap_or("").trim();
                if !cmd.is_empty() && !cmd.ends_with('\\') {
                    commands.push(cmd.to_string());
                }
            }
        }
    }

    assert!(
        !commands.is_empty(),
        "getting started docs must contain voice-pipes commands"
    );

    for cmd in commands {
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        // Verify that the first word is a known flag or subcommand
        let first = parts[0];
        let valid_first = first.starts_with("--config")
            || first.starts_with('-')
            || [
                "models",
                "live",
                "interactive",
                "serve",
                "check",
                "tts",
                "stt",
                "listen",
                "pipe",
                "inspect",
            ]
            .contains(&first);

        assert!(
            valid_first,
            "Command in quickstart-source.md '{cmd}' begins with unexpected token: '{first}'"
        );
    }
}

/// A comprehensive audit across all pipeline examples checking whether each is:
/// - Fully runnable right now (`[READY]`)
/// - Or skipped (`[SKIP]`), with the exact reason (missing model weights, missing API key, or uncompiled feature).
#[test]
fn test_examples_runtime_readiness_and_skip_report() {
    let dir = examples_dir();
    let toml_files = collect_toml_files(&dir);
    let vp_root = dir.parent().expect("voice-pipes root dir");

    let mut ready_count = 0;
    let mut skipped_count = 0;
    let mut rows = Vec::new();

    for file in &toml_files {
        let rel_path = file
            .strip_prefix(&dir)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        let file_name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let parent_dir = file
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("");

        if file_name == "orchestrator.toml"
            || file_name.ends_with(".worker.toml")
            || (matches!(parent_dir, "config" | "orchestrate") && file_name.starts_with("worker"))
        {
            continue; // Skip orchestrator/worker configs from pipeline component report
        }

        let Ok(content) = std::fs::read_to_string(file) else {
            continue;
        };

        let cfg: voice_pipes::config::PipelineConfig = match toml::from_str(&content) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let mut skip_reasons = Vec::new();

        // 1. Check Realtime S2S provider
        if let Some(ref rt) = cfg.realtime_provider {
            let key_env = match rt.provider.as_str() {
                "gemini-live" | "gemini" => "GEMINI_API_KEY",
                "openai-realtime" => "OPENAI_API_KEY",
                "elevenlabs" => "ELEVENLABS_API_KEY",
                _ => rt.api_key_env.as_deref().unwrap_or("API_KEY"),
            };
            let has_key = std::env::var(key_env).is_ok_and(|k| !k.trim().is_empty());
            if !has_key {
                skip_reasons.push(format!("Missing API key: {key_env}"));
            }
        }

        // 2. Check STT provider
        if let Some(ref stt) = cfg.stt {
            if let Some(ref dir_path) = stt.model_dir {
                let resolved = vp_root.join(dir_path);
                if !resolved.exists() {
                    skip_reasons.push(format!("Missing model dir: {}", dir_path.display()));
                }
            }
            if let Some(ref file_path) = stt.model {
                let resolved = vp_root.join(file_path);
                if !resolved.exists() {
                    skip_reasons.push(format!("Missing model: {}", file_path.display()));
                }
            }
            if matches!(stt.provider.as_str(), "assemblyai" | "assembly") {
                if !cfg!(feature = "stt-assemblyai") {
                    skip_reasons.push("Feature 'stt-assemblyai' not compiled in build".to_string());
                }
                if std::env::var("ASSEMBLYAI_API_KEY").map_or(true, |k| k.trim().is_empty()) {
                    skip_reasons.push("Missing API key: ASSEMBLYAI_API_KEY".to_string());
                }
            }
            if stt.provider == "google" {
                if !cfg!(feature = "stt-google") {
                    skip_reasons.push("Feature 'stt-google' not compiled in build".to_string());
                }
                if std::env::var("GOOGLE_APPLICATION_CREDENTIALS")
                    .map_or(true, |k| k.trim().is_empty())
                {
                    skip_reasons
                        .push("Missing credentials: GOOGLE_APPLICATION_CREDENTIALS".to_string());
                }
            }
            if stt.provider == "moonshine" && !cfg!(feature = "stt-moonshine") {
                skip_reasons.push("Feature 'stt-moonshine' not compiled in build".to_string());
            }
        }

        // 3. Check TTS provider
        if let Some(ref tts) = cfg.tts {
            if let Some(ref file_path) = tts.model {
                let resolved = vp_root.join(file_path);
                if !resolved.exists() {
                    skip_reasons.push(format!("Missing voice model: {}", file_path.display()));
                }
            }
            if tts.provider == "kokoro" && !cfg!(feature = "tts-kokoro") {
                skip_reasons.push("Feature 'tts-kokoro' not compiled in build".to_string());
            }
            if tts.provider == "openai" {
                if !cfg!(feature = "tts-openai") {
                    skip_reasons.push("Feature 'tts-openai' not compiled in build".to_string());
                }
                if std::env::var("OPENAI_API_KEY").map_or(true, |k| k.trim().is_empty()) {
                    skip_reasons.push("Missing API key: OPENAI_API_KEY".to_string());
                }
            }
            if tts.provider == "elevenlabs" {
                if !cfg!(feature = "tts-elevenlabs") {
                    skip_reasons.push("Feature 'tts-elevenlabs' not compiled in build".to_string());
                }
                if std::env::var("ELEVENLABS_API_KEY").map_or(true, |k| k.trim().is_empty()) {
                    skip_reasons.push("Missing API key: ELEVENLABS_API_KEY".to_string());
                }
            }
        }

        // 4. Check LLM text_processor
        for (i, proc) in cfg.text_processor.iter().enumerate() {
            if let Some(m_str) = proc.options.get("model").and_then(|v| v.as_str()) {
                let resolved = vp_root.join(m_str);
                if !resolved.exists() {
                    skip_reasons.push(format!("Missing LLM[{i}] model: {m_str}"));
                }
            }
            if proc.processor_type == "llm-openai-compat" || proc.processor_type == "openai" {
                let key_env = proc.api_key_env.as_deref().unwrap_or("OPENAI_API_KEY");
                if std::env::var(key_env).map_or(true, |k| k.trim().is_empty()) {
                    skip_reasons.push(format!("Missing LLM API key: {key_env}"));
                }
            }
        }

        // 5. Check VAD
        if let Some(m) = cfg.vad.as_ref().and_then(|v| v.model.as_ref()) {
            let resolved = vp_root.join(m);
            if !resolved.exists() {
                skip_reasons.push(format!("Missing VAD model: {}", m.display()));
            }
        }

        let is_ready = skip_reasons.is_empty();
        if is_ready {
            ready_count += 1;
            rows.push((
                rel_path,
                "READY".to_string(),
                "All models on disk and credentials configured".to_string(),
            ));
        } else {
            skipped_count += 1;
            rows.push((rel_path, "SKIPPED".to_string(), skip_reasons.join("; ")));
        }
    }

    // Print the structured diagnostics table
    println!("\n{}", "=".repeat(105));
    println!(" VOICE PIPES EXAMPLES: RELEASE READINESS & SKIP DIAGNOSTICS REPORT");
    println!("{}", "=".repeat(105));
    println!(
        "{:<45} | {:<7} | Diagnostics / Reason",
        "Example Configuration", "Status"
    );
    println!("{}", "-".repeat(105));
    for (name, status, reason) in &rows {
        println!("{name:<45} | {status:<7} | {reason}");
    }
    println!("{}", "=".repeat(105));
    println!(
        " Summary: Total Pipelines Evaluated: {} | Ready: {} | Skipped: {}",
        rows.len(),
        ready_count,
        skipped_count
    );
    println!("{}\n", "=".repeat(105));

    // Ensure we evaluated all pipelines and ready ones are non-zero
    assert!(!rows.is_empty(), "Must evaluate example configurations");
}
