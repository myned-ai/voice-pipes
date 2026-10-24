//! Binary-level smoke tests: run the compiled `voice-pipes` binary and
//! assert on exit codes and output. `CARGO_BIN_EXE_*` is provided by Cargo
//! for integration tests — no extra dependencies needed.

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_voice-pipes"))
}

#[test]
fn help_lists_subcommands() {
    let out = bin().arg("--help").output().expect("run --help");
    assert!(out.status.success(), "--help must exit 0");
    let text = String::from_utf8_lossy(&out.stdout);
    for cmd in [
        "tts",
        "stt",
        "pipe",
        "serve",
        "orchestrate",
        "validate-config",
    ] {
        assert!(text.contains(cmd), "--help must list `{cmd}`:\n{text}");
    }
}

#[test]
fn version_prints() {
    let out = bin().arg("--version").output().expect("run --version");
    assert!(out.status.success(), "--version must exit 0");
    assert!(String::from_utf8_lossy(&out.stdout).contains("voice-pipes"));
}

// The shipped config uses the SQLite session store; without it the binary
// stops on that first (see the next test), so the token check is not reached.
#[cfg(feature = "session-sqlite")]
#[test]
fn orchestrate_with_shipped_config_refuses_to_start_without_tokens() {
    // Deploy/docker/config/orchestrator.toml sets `[auth] required =
    // true`. With no VP_* token in the environment the process must exit
    // non-zero naming the missing variable, before binding any port.
    let cfg = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../voice-pipes/deploy/docker/config/orchestrator.toml")
        .canonicalize()
        .expect("shipped orchestrator.toml exists");
    let dir = std::env::temp_dir().join(format!("vp-cli-auth-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");

    let out = bin()
        .current_dir(&dir)
        .env_remove("VP_ADMIN_TOKEN")
        .env_remove("VP_INTERNAL_TOKEN")
        .env_remove("VP_CLIENT_TOKEN")
        .args(["orchestrate", "--orchestrator-config"])
        .arg(&cfg)
        .output()
        .expect("run orchestrate");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        !out.status.success(),
        "orchestrate must refuse to start without tokens when [auth] required = true"
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("VP_ADMIN_TOKEN") && text.contains("required"),
        "startup error must name the missing token variable:\n{text}"
    );
}

/// `serve` validates its config like every other command: an invalid
/// value exits non-zero with the validation message before any port is
/// bound.
#[test]
fn serve_refuses_an_invalid_config() {
    let dir = std::env::temp_dir().join(format!("vp-cli-serve-invalid-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let cfg = dir.join("bad.toml");
    std::fs::write(
        &cfg,
        "[output]\nsample_rate = 0\n[transport]\nenabled = true\nport = 0\n",
    )
    .expect("write config");

    let out = bin()
        .current_dir(&dir)
        .arg("--config")
        .arg(&cfg)
        .arg("serve")
        .output()
        .expect("run serve");
    let _ = std::fs::remove_dir_all(&dir);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "serve must refuse an invalid config:\n{combined}"
    );
    assert!(
        combined.contains("output.sample_rate") && combined.contains("transport.port"),
        "the validation errors must be named:\n{combined}"
    );
}

#[test]
fn validate_config_rejects_missing_file() {
    let out = bin()
        .args(["validate-config", "definitely-does-not-exist.toml"])
        .output()
        .expect("run validate-config");
    assert!(
        !out.status.success(),
        "validating a nonexistent file must exit non-zero"
    );
}

#[test]
fn validate_config_reports_missing_piper_model() {
    // Regression guard for the factory-panic fix: a piper TTS config without
    // `model` must yield a validation ERROR (clean non-zero exit + message),
    // never a crash.
    let dir = std::env::temp_dir().join(format!("vp-cli-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let cfg = dir.join("piper-missing-model.toml");
    std::fs::write(&cfg, "[tts]\nprovider = \"piper\"\n").expect("write config");

    let out = bin()
        .arg("validate-config")
        .arg(&cfg)
        .output()
        .expect("run validate-config");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "missing tts.model must fail validation:\n{combined}"
    );
    assert!(
        combined.contains("model"),
        "report should mention the missing model:\n{combined}"
    );

    let _ = std::fs::remove_file(&cfg);
    let _ = std::fs::remove_dir(&dir);
}

/// A config that asks for a store the binary was built without is refused
/// at startup with the feature's name, rather than falling back to a store
/// that loses history.
#[cfg(not(feature = "session-sqlite"))]
#[test]
fn orchestrate_names_the_missing_sqlite_feature() {
    let cfg = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../voice-pipes/deploy/docker/config/orchestrator.toml")
        .canonicalize()
        .expect("shipped orchestrator.toml exists");
    let dir = std::env::temp_dir().join(format!("vp-cli-sqlite-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let out = bin()
        .current_dir(&dir)
        .env("VP_ADMIN_TOKEN", "t-admin")
        .env("VP_INTERNAL_TOKEN", "t-internal")
        .env("VP_CLIENT_TOKEN", "t-client")
        .args(["orchestrate", "--orchestrator-config"])
        .arg(&cfg)
        .output()
        .expect("run orchestrate");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        !out.status.success(),
        "orchestrate must not start without its session store"
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("session-sqlite"),
        "the error must name the feature: {text}"
    );
}

/// A value the flag does not take is refused by the parser, naming the ones
/// it does, before anything runs.
#[test]
fn a_flag_value_outside_its_set_is_refused_with_the_choices() {
    let out = bin()
        .args(["inspect", "--kind", "speakers"])
        .output()
        .expect("run inspect");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("possible values"), "{err}");
    assert!(err.contains("realtime"), "{err}");
}

/// The kinds keep their long spellings as aliases.
#[test]
fn inspect_takes_a_kind_by_its_alias() {
    let out = bin()
        .args(["inspect", "--kind", "turn_detection", "--format", "json"])
        .output()
        .expect("run inspect");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert!(
        rows.as_array()
            .is_some_and(|r| r.iter().all(|row| row["kind"] == "turn")),
        "{rows}"
    );
}

/// A command's machine-readable output is all that is on stdout: logs go
/// to stderr, so `--format json` can be piped straight into a parser,
/// valid config or not.
#[test]
fn json_output_is_the_only_thing_on_stdout() {
    let dir = std::env::temp_dir().join(format!("vp-cli-json-stdout-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let good = dir.join("good.toml");
    let bad = dir.join("bad.toml");
    std::fs::write(&good, "[tts]\nprovider = \"piper\"\nmodel = \"m.onnx\"\n").expect("write");
    std::fs::write(&bad, "[output]\nsample_rate = 0\n").expect("write");
    for cfg in [&good, &bad] {
        let out = bin()
            .env("RUST_LOG", "voice_pipes=debug")
            .args(["validate-config", "--format", "json"])
            .arg(cfg)
            .output()
            .expect("run validate-config");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(&stdout);
        assert!(
            parsed.is_ok(),
            "{}: stdout is not one JSON document:\n{stdout}",
            cfg.display()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A directory holding a `voice-pipes.toml` that does not parse.
fn dir_with_broken_config(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("vp-cli-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::write(dir.join("voice-pipes.toml"), "[stt\nprovider = ").expect("write config");
    dir
}

#[test]
fn inspect_ignores_a_broken_config_in_the_working_directory() {
    let dir = dir_with_broken_config("inspect");
    let out = bin()
        .current_dir(&dir)
        .args(["inspect", "--kind", "stt"])
        .output()
        .expect("run inspect");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "inspect must not read the pipeline config:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn validate_config_checks_its_own_file_not_the_one_in_the_working_directory() {
    let dir = dir_with_broken_config("validate");
    let good = dir.join("good.toml");
    std::fs::write(&good, "[pipeline]\n").expect("write good config");
    let out = bin()
        .current_dir(&dir)
        .arg("validate-config")
        .arg(&good)
        .output()
        .expect("run validate-config");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "validate-config must validate the file it was given:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_config_that_does_not_parse_is_named_in_the_error() {
    let dir = dir_with_broken_config("named");
    let out = bin()
        .current_dir(&dir)
        .args(["text", "--input", "hello"])
        .output()
        .expect("run text");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // The log lines before the error name the file too; the error itself
    // has to, for a caller that keeps only the error.
    let error = all.split("ERROR").nth(1).unwrap_or(&all);
    assert!(
        error.contains("voice-pipes.toml"),
        "the error does not name the file:\n{all}"
    );
}

/// `[pipeline] log_format` decides the log format when the flag is not
/// given, and the flag wins when it is.
#[test]
fn the_configs_log_format_applies_unless_the_flag_says_otherwise() {
    let dir = std::env::temp_dir().join(format!("vp-cli-logfmt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::write(
        dir.join("voice-pipes.toml"),
        "[pipeline]\nlog_format = \"json\"\n",
    )
    .expect("write config");
    let run = |extra: &[&str]| {
        let out = bin()
            .current_dir(&dir)
            .args(extra)
            .args(["text", "--input", "hi"])
            .output()
            .expect("run text");
        String::from_utf8_lossy(&out.stderr).to_string()
    };
    let from_config = run(&[]);
    let from_flag = run(&["--log-format", "compact"]);
    let _ = std::fs::remove_dir_all(&dir);
    let first = |s: &str| s.lines().next().unwrap_or_default().to_string();
    assert!(
        first(&from_config).starts_with('{'),
        "not JSON:\n{from_config}"
    );
    assert!(
        !first(&from_flag).starts_with('{'),
        "the flag was ignored:\n{from_flag}"
    );
}
