use crate::api::param_schema::{ComponentParamSchema, ParamDescriptor, ParamValueType};
use crate::api::types::{PipelineAuditTap, PipelineEdge, PipelineGraphResponse, PipelineNode};
use crate::config::PipelineConfig;

// ── Fixed-node schemas ───────────────────────────────────────
//
// These nodes have no config struct, so we define their schemas inline.
// Their parameters are hardcoded constants (compile-time choices) that
// are display-only.

fn sinc_resample_schema() -> Vec<ParamDescriptor> {
    vec![
        ParamDescriptor::new(
            "sinc_zeros",
            "Sinc zeros",
            "Number of zero-crossings in the windowed-sinc interpolation \
                          filter. Higher values improve quality at the cost of CPU.",
            ParamValueType::Int,
        ),
        ParamDescriptor::new(
            "table_oversampling",
            "Table oversampling",
            "Oversampling factor for the pre-computed sinc lookup table. \
                          Higher values reduce interpolation aliasing.",
            ParamValueType::Int,
        ),
    ]
}

fn sinc_resample_values() -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("sinc_zeros".into(), serde_json::json!(8));
    m.insert("table_oversampling".into(), serde_json::json!(64));
    m
}

/// Nodes for the configured `[[audio_post_processor]]` chain, plus their ids
/// in chain order. Post-processors run on the ALREADY output-resampled audio
/// (the resampler lives inside `tts_task`; `post_audio::run_chain` runs right
/// before the chunk is handed to the transport), so callers splice these
/// between the output resample stage (or the realtime node) and the output.
///
/// Metadata is the raw `options` map, display-only: `AudioPostProcessorConfig`
/// has no `ComponentParamSchema` impl yet, so there is no mutable-param schema
/// to advertise.
fn audio_post_nodes(cfg: &PipelineConfig) -> (Vec<PipelineNode>, Vec<String>) {
    let list = &cfg.audio_post_processor;
    let mut nodes = Vec::with_capacity(list.len());
    let mut ids = Vec::with_capacity(list.len());
    for (i, pp) in list.iter().enumerate() {
        let id = if list.len() == 1 {
            format!("post_{}", pp.processor_type)
        } else {
            format!("post_{}_{i}", pp.processor_type)
        };
        let mut meta: serde_json::Map<String, serde_json::Value> = pp
            .options
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        // `inline` runs on the chunk before transport; `annotate` is teed off
        // the audio path — worth seeing on the node.
        meta.insert(
            "mode".to_string(),
            serde_json::to_value(pp.mode).unwrap_or(serde_json::Value::Null),
        );
        nodes.push(PipelineNode {
            id: id.clone(),
            node_type: "post".to_string(),
            label: "Audio Post".to_string(),
            provider: Some(pp.processor_type.clone()),
            input_sample_rate: None,
            output_sample_rate: None,
            algorithm: None,
            metadata: serde_json::Value::Object(meta),
            param_schema: vec![],
        });
        ids.push(id);
    }
    (nodes, ids)
}

/// Append the audio edges `from → post_0 → … → post_n → wav_output`
/// (or `from → wav_output` when no post-processors are configured).
fn push_post_chain_edges(
    edges: &mut Vec<PipelineEdge>,
    from: &str,
    post_ids: &[String],
    output_rate: Option<u32>,
) {
    let mut prev = from;
    for pid in post_ids {
        edges.push(PipelineEdge {
            from: prev.to_string(),
            to: pid.clone(),
            data_type: "audio".into(),
            sample_rate: output_rate,
        });
        prev = pid;
    }
    edges.push(PipelineEdge {
        from: prev.to_string(),
        to: "wav_output".into(),
        data_type: "audio".into(),
        sample_rate: output_rate,
    });
}

pub fn build_pipeline_graph(cfg: Option<&PipelineConfig>) -> PipelineGraphResponse {
    let Some(cfg) = cfg else {
        return PipelineGraphResponse {
            name: "unknown".to_string(),
            nodes: vec![],
            edges: vec![],
            audit_tap: PipelineAuditTap {
                enabled: false,
                side: None,
                tapped_nodes: vec![],
            },
            raw_toml: None,
        };
    };

    // ── Realtime S2S short-circuit ─────────────────────────
    //
    // When the worker is configured with `[realtime_provider]`, the
    // classic STT → LLM → TTS chain is bypassed and the conversation
    // runs end-to-end inside a single remote model. Rendering the
    // cascade graph would show ghost STT/TTS components (defaulted to
    // `parakeet-tdt` / `piper`) that the worker doesn't actually use
    // — confusing the user and exposing fake "providers" on the
    // dashboard. Emit a compact, accurate topology instead:
    //
    //     Mic (WebSocket) ──► Realtime (S2S) ──► Speaker (WebSocket)
    //
    // The realtime provider handles VAD, transcription, generation
    // and synthesis internally; surface the operator-relevant knobs
    // (model, voice, vad mode, instructions) as node metadata so the
    // UI can still display them.
    if let Some(rt) = cfg.realtime_provider.as_ref() {
        return build_realtime_pipeline_graph(cfg, rt);
    }

    let name = cfg
        .pipeline
        .name
        .clone()
        .unwrap_or_else(|| "voice-pipeline".to_string());
    let stt_provider = crate::config::stt_provider_name(cfg).unwrap_or_else(|| "none".to_string());
    let tts_provider = crate::config::tts_provider_name(cfg).unwrap_or_else(|| "none".to_string());
    // The graph does not build the recogniser to ask its rate, so it shows
    // none rather than assume one.
    let stt_rate: Option<u32> = None;
    // The provider knows its own rate; unknown stays unknown rather than a
    // guess drawn with confidence.
    let tts_rate = cfg.tts.as_ref().and_then(|t| {
        crate::tts::lookup_tts_factory(&t.provider).and_then(|f| f.output_sample_rate(t))
    });
    let output_rate = cfg.output.as_ref().and_then(|o| o.sample_rate).or(tts_rate);
    let has_transport = cfg.transport.as_ref().is_some_and(|t| t.enabled);

    let vad_mode = cfg.vad.as_ref().map_or("auto", |v| v.mode.as_str());

    // ── Derive metadata + schema from each component's own impl ─────────────

    let (vad_schema, vad_meta) = cfg
        .vad
        .as_ref()
        .map_or((vec![], serde_json::json!({})), |v| {
            (
                v.param_schema(),
                serde_json::Value::Object(v.current_values()),
            )
        });

    let (stt_schema, stt_meta) = cfg
        .stt
        .as_ref()
        .map_or((vec![], serde_json::json!({})), |s| {
            (
                s.param_schema(),
                serde_json::Value::Object(s.current_values()),
            )
        });

    let (tts_schema, tts_meta) = cfg
        .tts
        .as_ref()
        .map_or((vec![], serde_json::json!({})), |t| {
            (
                t.param_schema(),
                serde_json::Value::Object(t.current_values()),
            )
        });

    let (turn_schema, turn_meta, turn_mode) =
        cfg.turn_detection
            .as_ref()
            .map_or((vec![], serde_json::json!({}), "silence"), |td| {
                (
                    td.param_schema(),
                    serde_json::Value::Object(td.current_values()),
                    td.mode.as_str(),
                )
            });

    let sinc_schema = sinc_resample_schema();
    let sinc_values = sinc_resample_values();

    let mut nodes = vec![
        PipelineNode {
            id: "wav_input".to_string(),
            node_type: "input".to_string(),
            label: if has_transport {
                "Mic (WebSocket)".to_string()
            } else {
                "WAV Input".to_string()
            },
            provider: has_transport.then(|| "websocket".to_string()),
            input_sample_rate: None,
            output_sample_rate: Some(48_000), // browser mic default
            algorithm: None,
            metadata: serde_json::json!({}),
            param_schema: vec![],
        },
        PipelineNode {
            id: "vad".to_string(),
            node_type: "vad".to_string(),
            label: "VAD".to_string(),
            provider: Some(vad_mode.to_string()),
            input_sample_rate: Some(48_000),
            output_sample_rate: Some(48_000),
            algorithm: None,
            metadata: vad_meta,
            param_schema: vad_schema,
        },
        PipelineNode {
            id: "stt_resample".to_string(),
            node_type: "resample".to_string(),
            label: "Resample -> STT".to_string(),
            provider: None,
            input_sample_rate: Some(48_000),
            output_sample_rate: stt_rate,
            algorithm: Some("windowed-sinc".to_string()),
            metadata: serde_json::Value::Object(sinc_values.clone()),
            param_schema: sinc_schema.clone(),
        },
        PipelineNode {
            id: "stt".to_string(),
            node_type: "stt".to_string(),
            label: "STT".to_string(),
            provider: Some(stt_provider.clone()),
            input_sample_rate: stt_rate,
            output_sample_rate: None,
            algorithm: None,
            metadata: stt_meta,
            param_schema: stt_schema,
        },
        PipelineNode {
            id: "turn_detector".to_string(),
            node_type: "turn_detector".to_string(),
            label: "Turn Detection".to_string(),
            provider: Some(turn_mode.to_string()),
            input_sample_rate: None,
            output_sample_rate: None,
            algorithm: None,
            metadata: turn_meta,
            param_schema: turn_schema,
        },
    ];

    // Add text processor nodes (LLM, echo, api, etc.)
    let text_processors = &cfg.text_processor;
    let mut tp_ids: Vec<String> = Vec::new();
    for (i, tp) in text_processors.iter().enumerate() {
        let tp_id = if text_processors.len() == 1 {
            tp.processor_type.clone()
        } else {
            format!("{}_{}", tp.processor_type, i)
        };
        let label = "LLM".to_string();
        let provider = Some(tp.processor_type.clone());
        // Schema and values come entirely from the component's own impl
        let tp_schema = tp.param_schema();
        let tp_meta = serde_json::Value::Object(tp.current_values());
        nodes.push(PipelineNode {
            id: tp_id.clone(),
            node_type: "llm".to_string(),
            label,
            provider,
            input_sample_rate: None,
            output_sample_rate: None,
            algorithm: None,
            metadata: tp_meta,
            param_schema: tp_schema,
        });
        tp_ids.push(tp_id);
    }

    nodes.extend([
        PipelineNode {
            id: "tts".to_string(),
            node_type: "tts".to_string(),
            label: "TTS".to_string(),
            provider: Some(tts_provider.clone()),
            input_sample_rate: None,
            output_sample_rate: tts_rate,
            algorithm: None,
            metadata: tts_meta,
            param_schema: tts_schema,
        },
        PipelineNode {
            id: "output_resample".to_string(),
            node_type: "resample".to_string(),
            label: "Resample -> Output".to_string(),
            provider: None,
            input_sample_rate: tts_rate,
            output_sample_rate: output_rate,
            algorithm: Some("windowed-sinc".to_string()),
            metadata: serde_json::Value::Object(sinc_values),
            param_schema: sinc_schema,
        },
    ]);

    // Audio post-processors run on the resampled output audio, right
    // before transport.
    let (post_nodes, post_ids) = audio_post_nodes(cfg);
    nodes.extend(post_nodes);

    nodes.push(PipelineNode {
        id: "wav_output".to_string(),
        node_type: "output".to_string(),
        label: if has_transport {
            "Speaker (WebSocket)".to_string()
        } else {
            "WAV Output".to_string()
        },
        provider: has_transport.then(|| "websocket".to_string()),
        input_sample_rate: output_rate,
        output_sample_rate: output_rate,
        algorithm: None,
        metadata: serde_json::json!({}),
        param_schema: vec![],
    });

    // Build edges: input -> vad -> resample -> stt -> [text_processors...] -> tts -> resample -> output
    let mut edges = vec![
        PipelineEdge {
            from: "wav_input".into(),
            to: "vad".into(),
            data_type: "audio".into(),
            sample_rate: None,
        },
        PipelineEdge {
            from: "vad".into(),
            to: "stt_resample".into(),
            data_type: "audio".into(),
            sample_rate: None,
        },
        PipelineEdge {
            from: "stt_resample".into(),
            to: "stt".into(),
            data_type: "audio".into(),
            sample_rate: stt_rate,
        },
    ];
    // STT -> turn detection -> first text processor (or TTS if none)
    let first_tp = tp_ids.first().cloned().unwrap_or_else(|| "tts".to_string());
    edges.push(PipelineEdge {
        from: "stt".into(),
        to: "turn_detector".into(),
        data_type: "text".into(),
        sample_rate: None,
    });
    edges.push(PipelineEdge {
        from: "turn_detector".into(),
        to: first_tp.clone(),
        data_type: "text".into(),
        sample_rate: None,
    });
    // Chain text processors together
    for w in tp_ids.windows(2) {
        edges.push(PipelineEdge {
            from: w[0].clone(),
            to: w[1].clone(),
            data_type: "text".into(),
            sample_rate: None,
        });
    }
    // Last text processor -> TTS
    if let Some(last_tp) = tp_ids.last() {
        edges.push(PipelineEdge {
            from: last_tp.clone(),
            to: "tts".into(),
            data_type: "text".into(),
            sample_rate: None,
        });
    }
    edges.push(PipelineEdge {
        from: "tts".into(),
        to: "output_resample".into(),
        data_type: "audio".into(),
        sample_rate: tts_rate,
    });
    push_post_chain_edges(&mut edges, "output_resample", &post_ids, output_rate);

    let tap = &cfg.pipeline.audit_tap;
    let tapped_nodes = if tap.enabled {
        vec!["stt_resample".to_string(), "output_resample".to_string()]
    } else {
        vec![]
    };

    PipelineGraphResponse {
        name,
        nodes,
        edges,
        audit_tap: PipelineAuditTap {
            enabled: tap.enabled,
            side: tap.side.clone(),
            tapped_nodes,
        },
        raw_toml: render_redacted_toml(cfg),
    }
}

fn render_redacted_toml(cfg: &PipelineConfig) -> Option<String> {
    let mut val = serde_json::to_value(cfg).ok()?;
    crate::config::redact::redact_secrets(&mut val);
    prune_nulls(&mut val);
    toml::to_string_pretty(&val).ok()
}

fn prune_nulls(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.retain(|_, v| !v.is_null());
            for v in map.values_mut() {
                prune_nulls(v);
            }
        }
        serde_json::Value::Array(items) => {
            for v in items.iter_mut() {
                prune_nulls(v);
            }
        }
        _ => {}
    }
}

// ── Realtime S2S topology builder ────────────────────────────
//
// Produces a 3-node graph for sessions backed by an
// `[realtime_provider]` block. The intermediate Realtime node holds
// everything the operator might want to inspect (provider, model,
// voice, VAD mode, instructions) without faking STT/LLM/TTS
// components that don't exist in this configuration.

fn realtime_output_emit_rate(rt: &crate::config::RealtimeProviderConfig) -> u32 {
    // An invalid setting fails validation before a session starts; the
    // graph shows the default meanwhile.
    rt.output_emit_rate()
        .unwrap_or(crate::config::DEFAULT_OUTPUT_EMIT_RATE)
}

fn build_realtime_pipeline_graph(
    cfg: &PipelineConfig,
    rt: &crate::config::RealtimeProviderConfig,
) -> PipelineGraphResponse {
    let name = cfg
        .pipeline
        .name
        .clone()
        .unwrap_or_else(|| "voice-pipeline".to_string());
    let has_transport = cfg.transport.as_ref().is_some_and(|t| t.enabled);
    // The provider's own input rate is known to its protocol, which the
    // graph does not build; it is shown as unknown rather than guessed.
    let native_in: Option<u32> = None;
    let emit_rate = realtime_output_emit_rate(rt);
    let output_rate = cfg
        .output
        .as_ref()
        .and_then(|o| o.sample_rate)
        .unwrap_or(emit_rate);

    // Operator-relevant metadata, surfaced as a JSON object so the
    // existing `metadata` rendering in the UI picks each field up
    // automatically. We use `current_values()` so that full text is passed
    // to the UI for editing, relying on the frontend to visually truncate.
    let mut meta = rt.current_values();
    meta.insert(
        "provider".into(),
        serde_json::Value::String(rt.provider.clone()),
    );
    meta.insert(
        "input_sample_rate".into(),
        serde_json::Value::from(native_in),
    );
    meta.insert(
        "output_sample_rate".into(),
        serde_json::Value::from(emit_rate),
    );

    let mut nodes = vec![
        PipelineNode {
            id: "wav_input".to_string(),
            node_type: "input".to_string(),
            label: if has_transport {
                "Mic (WebSocket)".to_string()
            } else {
                "WAV Input".to_string()
            },
            provider: has_transport.then(|| "websocket".to_string()),
            input_sample_rate: None,
            output_sample_rate: Some(48_000),
            algorithm: None,
            metadata: serde_json::json!({}),
            param_schema: vec![],
        },
        PipelineNode {
            id: "realtime".to_string(),
            node_type: "realtime".to_string(),
            label: "Realtime (S2S)".to_string(),
            provider: Some(rt.provider.clone()),
            input_sample_rate: native_in,
            output_sample_rate: Some(emit_rate),
            algorithm: None,
            metadata: serde_json::Value::Object(meta),
            param_schema: rt.param_schema(),
        },
    ];

    // Audio post-processors run on provider audio in the realtime runner
    // too — surface them just like in the classic topology.
    let (post_nodes, post_ids) = audio_post_nodes(cfg);
    nodes.extend(post_nodes);

    nodes.push(PipelineNode {
        id: "wav_output".to_string(),
        node_type: "output".to_string(),
        label: if has_transport {
            "Speaker (WebSocket)".to_string()
        } else {
            "WAV Output".to_string()
        },
        provider: has_transport.then(|| "websocket".to_string()),
        input_sample_rate: Some(output_rate),
        output_sample_rate: Some(output_rate),
        algorithm: None,
        metadata: serde_json::json!({}),
        param_schema: vec![],
    });

    let mut edges = vec![PipelineEdge {
        from: "wav_input".into(),
        to: "realtime".into(),
        data_type: "audio".into(),
        sample_rate: native_in,
    }];
    push_post_chain_edges(&mut edges, "realtime", &post_ids, Some(output_rate));

    PipelineGraphResponse {
        name,
        nodes,
        edges,
        // Realtime sessions don't use the audit-tap (no internal
        // resample stages to tap); always disabled.
        audit_tap: PipelineAuditTap {
            enabled: false,
            side: None,
            tapped_nodes: vec![],
        },
        raw_toml: render_redacted_toml(cfg),
    }
}
