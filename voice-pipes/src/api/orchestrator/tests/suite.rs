use crate::api::orchestrator::pipeline_graph::build_pipeline_graph;
use crate::config::PipelineConfig;

fn default_config_toml() -> &'static str {
    r#"
[pipeline]
name = "test-pipeline"
log_format = "compact"

[pipeline.audit]
enabled = false

[pipeline.audit_tap]
enabled = false
side = "both"

[stt]
provider = "parakeet-tdt"
model_dir = "models/parakeet-tdt"

[tts]
provider = "piper"
model = "models/en_US-lessac-medium.onnx"

[output]
sample_rate = 48000
"#
}

fn default_config() -> PipelineConfig {
    toml::from_str::<PipelineConfig>(default_config_toml()).expect("parse test config")
}

/// A worker config with a field this build does not know is refused with a
/// reason that names the field and the fix, not turned into an empty graph
/// the console would show as "no workers".
#[test]
fn a_worker_config_from_a_newer_build_is_refused_with_the_reason() {
    use crate::api::orchestrator::handlers::parse_worker_pipeline_config;
    let mut raw = serde_json::to_value(default_config()).expect("config as JSON");
    raw["pipeline"]["field_from_a_newer_build"] = serde_json::json!(300);

    let err = parse_worker_pipeline_config("worker-x", Some(&raw)).expect_err("unknown field");
    assert!(err.contains("worker-x"), "{err}");
    assert!(err.contains("field_from_a_newer_build"), "{err}");
    assert!(
        err.contains("Rebuild and restart the orchestrator"),
        "{err}"
    );

    let cfg = serde_json::to_value(default_config()).expect("config as JSON");
    assert!(
        parse_worker_pipeline_config("worker-x", Some(&cfg))
            .expect("a config of this build parses")
            .is_some()
    );
    assert!(
        parse_worker_pipeline_config("worker-x", None)
            .expect("no config is not an error")
            .is_none()
    );
}

#[test]
fn pipeline_graph_contains_vad_node() {
    let cfg = default_config();
    let graph = build_pipeline_graph(Some(&cfg));

    let vad_node = graph.nodes.iter().find(|n| n.id == "vad");
    assert!(vad_node.is_some(), "pipeline graph must contain a vad node");

    let vad = vad_node.unwrap();
    assert_eq!(vad.node_type, "vad");
    assert!(vad.label.contains("VAD"));
}

#[test]
fn pipeline_graph_vad_edges() {
    let cfg = default_config();
    let graph = build_pipeline_graph(Some(&cfg));

    // wav_input -> vad
    assert!(
        graph
            .edges
            .iter()
            .any(|e| e.from == "wav_input" && e.to == "vad"),
        "edge wav_input->vad must exist"
    );
    // vad -> stt_resample
    assert!(
        graph
            .edges
            .iter()
            .any(|e| e.from == "vad" && e.to == "stt_resample"),
        "edge vad->stt_resample must exist"
    );
    // No direct wav_input -> stt_resample (VAD is in between)
    assert!(
        !graph
            .edges
            .iter()
            .any(|e| e.from == "wav_input" && e.to == "stt_resample"),
        "wav_input must NOT connect directly to stt_resample"
    );
}

#[test]
fn pipeline_graph_silero_vad_metadata() {
    let cfg = toml::from_str::<PipelineConfig>(
        r#"
[pipeline]
name = "test"
log_format = "compact"

[pipeline.audit]
enabled = false

[pipeline.audit_tap]
enabled = false
side = "both"

[stt]
provider = "parakeet-tdt"

[tts]
provider = "piper"

[vad]
mode = "silero"
model = "models/silero_vad.onnx"
speech_prob = 0.6
"#,
    )
    .expect("parse config");

    let graph = build_pipeline_graph(Some(&cfg));
    let vad = graph.nodes.iter().find(|n| n.id == "vad").unwrap();

    assert_eq!(vad.label, "VAD");
    assert_eq!(vad.provider.as_deref(), Some("silero"));

    let meta = &vad.metadata;
    assert_eq!(meta["mode"], "silero");
    assert_eq!(meta["model"], "models/silero_vad.onnx");
    // speech_prob resolves through effective_speech_prob()
    let sp = meta["speech_prob"].as_f64().unwrap();
    assert!(
        (sp - 0.6).abs() < 1e-6,
        "speech_prob should be ~0.6, got {sp}"
    );
}

#[test]
fn pipeline_graph_node_count() {
    let cfg = default_config();
    let graph = build_pipeline_graph(Some(&cfg));

    // 8 nodes: wav_input, vad, stt_resample, stt, turn_detector, tts, output_resample, wav_output
    assert_eq!(
        graph.nodes.len(),
        8,
        "expected 8 pipeline nodes including vad and turn_detector"
    );
    // 7 edges
    assert_eq!(graph.edges.len(), 7, "expected 7 pipeline edges");
}

/// Configurations with `[realtime_provider]` MUST surface a
/// compact 3-node graph (Mic → Realtime → Speaker) instead of the
/// classic STT/LLM/TTS chain. Regression guard against the
/// previous behaviour, which would default to bogus
/// `parakeet-tdt` STT + `piper` TTS nodes for realtime workers.
#[test]
fn pipeline_graph_realtime_topology() {
    let cfg = toml::from_str::<PipelineConfig>(
        r#"
[pipeline]
name = "rt-test"
log_format = "compact"

[pipeline.audit]
enabled = false

[pipeline.audit_tap]
enabled = false
side = "both"

[realtime_provider]
provider = "openai-realtime"
model = "gpt-realtime"
voice = "alloy"
vad = "server_vad"
instructions = "be brief"

[transport]
enabled = true
"#,
    )
    .expect("parse realtime config");

    let graph = build_pipeline_graph(Some(&cfg));

    // 3 nodes exactly: input, realtime, output. No ghost
    // STT/LLM/TTS components.
    assert_eq!(graph.nodes.len(), 3, "realtime topology must be 3 nodes");
    let ids: Vec<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids, vec!["wav_input", "realtime", "wav_output"]);
    assert!(graph.nodes.iter().all(|n| n.id != "stt" && n.id != "tts"));

    let rt = graph.nodes.iter().find(|n| n.id == "realtime").unwrap();
    assert_eq!(rt.node_type, "realtime");
    assert_eq!(rt.provider.as_deref(), Some("openai-realtime"));
    assert_eq!(rt.metadata["model"], "gpt-realtime");
    assert_eq!(rt.metadata["voice"], "alloy");
    assert_eq!(rt.metadata["vad"], "server_vad");

    // 2 edges: input → realtime → output.
    assert_eq!(graph.edges.len(), 2);
    assert!(
        graph
            .edges
            .iter()
            .any(|e| e.from == "wav_input" && e.to == "realtime")
    );
    assert!(
        graph
            .edges
            .iter()
            .any(|e| e.from == "realtime" && e.to == "wav_output")
    );

    // Audit tap is meaningless for realtime (no resamplers) -> disabled.
    assert!(!graph.audit_tap.enabled);
}

/// `[[audio_post_processor]]` entries must appear as `post` nodes
/// spliced between the output resampler and the output — before this,
/// the Live Pipeline graph silently hid configured post-processors
/// (e.g. the gRPC blendshape service).
#[test]
fn pipeline_graph_includes_audio_post_processors() {
    let mut toml_src = default_config_toml().to_string();
    toml_src.push_str(
        r#"
[[audio_post_processor]]
type = "grpc"
endpoint = "http://localhost:50070"
"#,
    );
    let cfg = toml::from_str::<PipelineConfig>(&toml_src).expect("parse config");
    let graph = build_pipeline_graph(Some(&cfg));

    let post = graph
        .nodes
        .iter()
        .find(|n| n.id == "post_grpc")
        .expect("pipeline graph must contain the audio post-processor node");
    assert_eq!(post.node_type, "post");
    assert_eq!(post.provider.as_deref(), Some("grpc"));
    // Raw options surface as display-only metadata.
    assert_eq!(post.metadata["endpoint"], "http://localhost:50070");
    assert!(post.param_schema.is_empty());

    // Spliced into the audio chain: resample -> post -> output, and the
    // old direct resample -> output edge is gone.
    assert!(
        graph
            .edges
            .iter()
            .any(|e| e.from == "output_resample" && e.to == "post_grpc")
    );
    assert!(
        graph
            .edges
            .iter()
            .any(|e| e.from == "post_grpc" && e.to == "wav_output")
    );
    assert!(
        !graph
            .edges
            .iter()
            .any(|e| e.from == "output_resample" && e.to == "wav_output")
    );
}

/// Post-processors run in realtime S2S sessions too — the compact
/// topology must show them between the realtime node and the output.
#[test]
fn pipeline_graph_realtime_includes_audio_post_processors() {
    let cfg = toml::from_str::<PipelineConfig>(
        r#"
[pipeline]
name = "rt-post"
log_format = "compact"

[pipeline.audit]
enabled = false

[pipeline.audit_tap]
enabled = false
side = "both"

[realtime_provider]
provider = "openai-realtime"
model = "gpt-realtime"

[[audio_post_processor]]
type = "grpc"
endpoint = "http://localhost:50070"
"#,
    )
    .expect("parse realtime config");
    let graph = build_pipeline_graph(Some(&cfg));

    let ids: Vec<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["wav_input", "realtime", "post_grpc", "wav_output"]
    );
    assert!(
        graph
            .edges
            .iter()
            .any(|e| e.from == "realtime" && e.to == "post_grpc")
    );
    assert!(
        graph
            .edges
            .iter()
            .any(|e| e.from == "post_grpc" && e.to == "wav_output")
    );
    assert!(
        !graph
            .edges
            .iter()
            .any(|e| e.from == "realtime" && e.to == "wav_output")
    );
}

// ── scan_configs (list_configs helper) ──────────────────

use crate::config::discover::scan_configs;
use std::fs;
use tempfile::TempDir;

fn write(p: &std::path::Path, body: &str) {
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(p, body).unwrap();
}

#[test]
fn scan_configs_returns_only_toml_sorted() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(&root.join("b.toml"), "");
    write(&root.join("a.toml"), "");
    write(&root.join("readme.md"), "ignore me");
    write(&root.join("notes.txt"), "ignore me");

    let entries = scan_configs(root, 2);
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["a", "b"], "sorted by path, .toml only");
    // Stems strip extension; paths use forward slashes.
    for e in &entries {
        assert!(!e.path.contains('\\'), "path uses forward slashes");
        assert!(
            std::path::Path::new(&e.path)
                .extension()
                .is_some_and(|x| x == "toml")
        );
    }
}

#[test]
fn scan_configs_recurses_within_depth_limit() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(&root.join("top.toml"), "");
    write(&root.join("sub1").join("mid.toml"), "");
    write(&root.join("sub1").join("sub2").join("deep.toml"), "");
    // depth_limit=2 means root(0) -> sub1(1) -> sub2(2) all visited.
    // Names follow path-sorted order: sub1/mid < sub1/sub2/deep < top.
    let entries = scan_configs(root, 2);
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["mid", "deep", "top"]);
}

#[test]
fn scan_configs_respects_depth_limit() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(&root.join("top.toml"), "");
    write(&root.join("a").join("b").join("c").join("nope.toml"), "");

    // depth_limit=1 means only root(0) and direct children(1) -- nope.toml is at depth 3.
    let entries = scan_configs(root, 1);
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["top"]);
}

#[test]
fn scan_configs_skips_hidden_entries() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(&root.join("visible.toml"), "");
    write(&root.join(".hidden.toml"), "");
    write(&root.join(".cache").join("inside.toml"), "");

    let entries = scan_configs(root, 2);
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["visible"],
        "hidden files and hidden dirs are excluded"
    );
}

#[test]
fn scan_configs_missing_root_returns_empty() {
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("does-not-exist");
    let entries = scan_configs(&missing, 2);
    assert!(entries.is_empty());
}

/// An absolute scan root must not leak the host directory layout:
/// two workers scanning the same tree under different absolute
/// prefixes must advertise identical `ConfigEntry.path`s (the
/// orchestrator aggregates fleet-wide keyed by path).
#[test]
fn scan_configs_absolute_root_emits_host_agnostic_paths() {
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();
    for base in [dir_a.path(), dir_b.path()] {
        let root = base.join("examples");
        write(&root.join("a.toml"), "");
        write(&root.join("avatar").join("b.toml"), "");
    }

    let a = scan_configs(&dir_a.path().join("examples"), 2);
    let b = scan_configs(&dir_b.path().join("examples"), 2);
    let paths_a: Vec<&str> = a.iter().map(|e| e.path.as_str()).collect();
    let paths_b: Vec<&str> = b.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        paths_a, paths_b,
        "same tree under different absolute roots must yield identical entries"
    );
    assert_eq!(paths_a, vec!["examples/a.toml", "examples/avatar/b.toml"]);
    for p in paths_a {
        assert!(
            !p.contains(':') && !p.starts_with('/'),
            "no drive/root prefix may leak into the payload: {p}"
        );
    }
}

/// A relative root passes through verbatim — the emitted path stays
/// resolvable from the worker CWD (`--config examples/...`), which
/// is the `Variant.config_path` loadability contract. Exercises the
/// exact production call (`scan_configs(Path::new("examples"), 2)`):
/// cargo runs unit tests with CWD = the crate root, which contains
/// the real `examples/` tree.
#[test]
fn scan_configs_relative_root_keeps_cwd_resolvable_prefix() {
    let entries = scan_configs(std::path::Path::new("examples"), 2);
    assert!(
        !entries.is_empty(),
        "the repo's examples/ tree must contain configs"
    );
    for e in &entries {
        assert!(
            e.path.starts_with("examples/"),
            "relative root prefix preserved verbatim: {}",
            e.path
        );
        assert!(!e.path.contains('\\'), "`/` separators only: {}", e.path);
        assert!(
            std::path::Path::new(&e.path).exists(),
            "emitted path must resolve from the worker CWD: {}",
            e.path
        );
    }
    // Nested dirs are preserved (examples/ has at least one subdir).
    assert!(
        entries.iter().any(|e| e.path.matches('/').count() >= 2),
        "expected at least one nested entry, got: {:?}",
        entries.iter().map(|e| &e.path).collect::<Vec<_>>()
    );
}

/// The graph shows the rate a TTS provider reports for its config, and
/// nothing for a provider this build cannot ask, never a guessed rate.
#[test]
fn pipeline_graph_takes_the_tts_rate_from_the_provider() {
    let unknown: PipelineConfig =
        toml::from_str("[pipeline]\nname = \"g\"\n[tts]\nprovider = \"not-in-this-build\"\n")
            .unwrap();
    let graph = build_pipeline_graph(Some(&unknown));
    let tts_out = graph
        .edges
        .iter()
        .find(|e| e.from == "tts")
        .expect("tts output edge");
    assert_eq!(tts_out.sample_rate, None);

    let pinned: PipelineConfig = toml::from_str(
        "[pipeline]\nname = \"g\"\n[tts]\nprovider = \"not-in-this-build\"\n[output]\nsample_rate = 16000\n",
    )
    .unwrap();
    let graph = build_pipeline_graph(Some(&pinned));
    assert!(
        graph
            .edges
            .iter()
            .any(|e| e.to == "wav_output" && e.sample_rate == Some(16_000)),
        "a configured output rate still shows"
    );
}

#[test]
fn pipeline_graph_raw_toml_rendered_and_redacted() {
    let raw = r#"
[pipeline]
name = "secure-pipeline"

[stt]
provider = "deepgram"
api_key = "super-secret-key-12345"
"#;
    let cfg: PipelineConfig = toml::from_str(raw).expect("valid pipeline config");
    let graph = build_pipeline_graph(Some(&cfg));

    let rendered = graph.raw_toml.expect("raw_toml should be populated");
    assert!(!rendered.contains("super-secret-key-12345"), "secret must be redacted");
    assert!(
        rendered.contains(crate::config::redact::REDACTED),
        "secret must be replaced with REDACTED placeholder"
    );
    assert!(rendered.contains("name = 'secure-pipeline'") || rendered.contains("name = \"secure-pipeline\""));
}
