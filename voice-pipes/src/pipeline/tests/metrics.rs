use super::*;
use serde_json::json;

// ── Descriptor health metadata ──

#[test]
fn descriptor_field_deserializes_without_health_field() {
    // Old wire payloads (pre-health) must still deserialize — the field is
    // additive & optional. This is the core back-compat guarantee.
    let old = json!({ "key": "ttft_ms", "label": "TTFT", "unit": "ms" });
    let f: MetricFieldDescriptor = serde_json::from_value(old).unwrap();
    assert_eq!(f.key, "ttft_ms");
    assert!(f.health.is_none());
    assert!(f.format.is_none());
}

#[test]
fn descriptor_health_roundtrips() {
    let f =
        MetricFieldDescriptor::new("ttfb_ms", "TTFB", MetricUnit::Ms).latency(400.0, Some(true));
    let wire = serde_json::to_value(&f).unwrap();
    // Health present with the expected snake_case wire form.
    assert_eq!(wire["health"]["role"], "latency");
    assert_eq!(wire["health"]["direction"], "lower_is_better");
    assert_eq!(wire["health"]["budget"], 400.0);
    assert_eq!(wire["health"]["on_critical_path"], true);
    let back: MetricFieldDescriptor = serde_json::from_value(wire).unwrap();
    assert_eq!(back, f);
}

#[test]
fn diagnostic_field_serializes_without_budget() {
    let f = MetricFieldDescriptor::new("chars", "Characters", MetricUnit::Count).diagnostic();
    let wire = serde_json::to_value(&f).unwrap();
    assert_eq!(wire["health"]["role"], "diagnostic");
    // No budget key emitted (skip_serializing_if None).
    assert!(wire["health"].get("budget").is_none());
}

#[test]
fn canonical_descriptors_declare_critical_path_latencies() {
    // The perceived-latency metrics must be latency + on_critical_path so the
    // generic engine judges them; the overlap-conditional STT processing must
    // declare on_critical_path: None (engine decides).
    let expect = |d: &ComponentMetricDescriptor, key: &str| {
        d.fields
            .iter()
            .find(|f| f.key == key)
            .unwrap()
            .health
            .clone()
            .unwrap()
    };
    let stt = ComponentMetricDescriptor::stt_default();
    assert_eq!(expect(&stt, "endpointing_ms").on_critical_path, Some(true));
    assert_eq!(
        expect(&stt, "processing_ms").role,
        Some(MetricRole::Latency)
    );
    assert_eq!(expect(&stt, "processing_ms").on_critical_path, None); // conditional
    assert_eq!(expect(&stt, "stt_rtfx").role, Some(MetricRole::Diagnostic));

    let llm = ComponentMetricDescriptor::llm_default();
    assert_eq!(expect(&llm, "ttft_ms").on_critical_path, Some(true));
    assert_eq!(
        expect(&llm, "generation_ms").role,
        Some(MetricRole::Diagnostic)
    );

    let tts = ComponentMetricDescriptor::tts_default();
    assert_eq!(expect(&tts, "ttfb_ms").on_critical_path, Some(true));
    assert_eq!(expect(&tts, "rtf").role, Some(MetricRole::Quality));

    let rt = ComponentMetricDescriptor::realtime_default();
    assert_eq!(expect(&rt, "ttfb_ms").on_critical_path, Some(true));
    // Input audio quality: advertised, never judged (no budget yet).
    for key in crate::audio::input_meter::EXTRAS_KEYS {
        let h = expect(&rt, key);
        assert_eq!(h.role, Some(MetricRole::Diagnostic), "{key}");
        assert_eq!(h.budget, None, "{key}");
    }
}

#[test]
fn severity_bands_default() {
    let b = SeverityBands::default();
    assert!(b.warn < b.moderate && b.moderate < b.severe);
}

#[test]
fn snapshot_starts_empty() {
    let r = TurnRecorder::new();
    let snap = r.snapshot(7, false, None);
    assert_eq!(snap.utterance_id, 7);
    assert!(!snap.interrupted);
    assert!(snap.components.is_empty());
}

#[test]
fn span_records_id_kind_and_order() {
    let r = TurnRecorder::new();
    r.begin("stt:parakeet", "stt").finish();
    r.begin("llm:openai", "llm").finish();
    r.begin("tts:piper", "tts").finish();
    let snap = r.snapshot(1, false, None);
    let kinds: Vec<&str> = snap.components.iter().map(|c| c.kind.as_str()).collect();
    assert_eq!(kinds, vec!["stt", "llm", "tts"]);
    let ids: Vec<&str> = snap
        .components
        .iter()
        .map(|c| c.component_id.as_str())
        .collect();
    assert_eq!(ids, vec!["stt:parakeet", "llm:openai", "tts:piper"]);
}

#[test]
fn finish_stamps_latency_and_finished_at() {
    let r = TurnRecorder::new();
    let span = r.begin("tts:piper", "tts");
    std::thread::sleep(std::time::Duration::from_millis(5));
    span.finish();
    let snap = r.snapshot(1, false, None);
    let c = &snap.components[0];
    assert!(c.finished_at_ms.is_some());
    assert!(c.latency_ms.unwrap() >= 5);
}

#[test]
fn drop_finishes_span_when_not_called_explicitly() {
    let r = TurnRecorder::new();
    {
        let _span = r.begin("tts:piper", "tts");
        std::thread::sleep(std::time::Duration::from_millis(2));
    };
    let snap = r.snapshot(1, false, None);
    assert!(snap.components[0].finished_at_ms.is_some());
}

#[test]
fn finish_is_idempotent() {
    let r = TurnRecorder::new();
    let mut span = r.begin("tts:piper", "tts");
    span.set_provider("piper");
    span.finish_in_place();
    let first = r.snapshot(1, false, None).components[0].finished_at_ms;
    std::thread::sleep(std::time::Duration::from_millis(2));
    span.finish_in_place();
    let second = r.snapshot(1, false, None).components[0].finished_at_ms;
    assert_eq!(first, second);
}

#[test]
fn set_provider_cold_start_and_error_round_trip() {
    let r = TurnRecorder::new();
    let mut span = r.begin("tts:piper", "tts");
    span.set_provider("piper");
    span.set_cold_start(true);
    span.set_error("synth failed");
    span.finish();
    let snap = r.snapshot(1, false, None);
    let c = &snap.components[0];
    assert_eq!(c.provider.as_deref(), Some("piper"));
    assert!(c.cold_start);
    assert_eq!(c.error.as_deref(), Some("synth failed"));
}

#[test]
fn extras_mut_writes_freeform_keys() {
    let r = TurnRecorder::new();
    let mut span = r.begin("custom:thing", "thing");
    span.extras_mut(|m| {
        m.insert("foo".into(), json!(42));
    });
    span.finish();
    let snap = r.snapshot(1, false, None);
    assert_eq!(snap.components[0].extras.get("foo"), Some(&json!(42)));
}

#[test]
fn extras_typed_serialises_struct_into_map() {
    #[derive(Serialize)]
    struct LlmExtras {
        ttft_ms: u64,
        generation_ms: u64,
    }

    let r = TurnRecorder::new();
    let mut span = r.begin("llm:openai", "llm");
    span.extras_typed(&LlmExtras {
        ttft_ms: 211,
        generation_ms: 614,
    });
    span.finish();
    let snap = r.snapshot(1, false, None);
    assert_eq!(snap.components[0].extras.get("ttft_ms"), Some(&json!(211)));
    assert_eq!(
        snap.components[0].extras.get("generation_ms"),
        Some(&json!(614))
    );
}

#[test]
fn helpers_pull_well_known_fields_from_extras() {
    let r = TurnRecorder::new();
    let mut llm = r.begin("llm:openai", "llm");
    llm.extras_mut(|m| {
        m.insert("ttft_ms".into(), json!(211));
        m.insert("generation_ms".into(), json!(614));
    });
    llm.finish();
    let mut tts = r.begin("tts:piper", "tts");
    tts.extras_mut(|m| {
        m.insert("ttfb_ms".into(), json!(88));
    });
    tts.finish();
    let snap = r.snapshot(1, false, None);
    assert_eq!(snap.ttft_ms(), Some(211));
    assert_eq!(snap.ttfb_ms(), Some(88));
    assert_eq!(snap.llm_generation_ms(), Some(614));
    assert_eq!(snap.stt_latency_ms(), None);
}

#[test]
fn component_by_kind_finds_first_match() {
    let r = TurnRecorder::new();
    r.begin("a:1", "audio_post").finish();
    r.begin("a:2", "audio_post").finish();
    let snap = r.snapshot(1, false, None);
    assert_eq!(
        snap.component_by_kind("audio_post")
            .map(|c| c.component_id.as_str()),
        Some("a:1")
    );
}

#[test]
fn open_span_at_snapshot_has_no_finished_at() {
    let r = TurnRecorder::new();
    let _span = r.begin("tts:piper", "tts");
    let snap = r.snapshot(1, true, None);
    assert!(snap.interrupted);
    assert!(snap.components[0].finished_at_ms.is_none());
    assert!(snap.components[0].latency_ms.is_none());
}

#[test]
fn metric_unit_serialises_to_snake_case() {
    let v = serde_json::to_value(MetricUnit::TokensPerSec).unwrap();
    assert_eq!(v, json!("tokens_per_sec"));
    assert_eq!(serde_json::to_value(MetricUnit::Ms).unwrap(), json!("ms"));
}

#[test]
fn descriptor_round_trips_through_serde() {
    let d = ComponentMetricDescriptor {
        component_id: "tts:piper".into(),
        kind: "tts".into(),
        label: "TTS (Piper)".into(),
        fields: vec![MetricFieldDescriptor::new(
            "ttfb_ms",
            "Time to first byte",
            MetricUnit::Ms,
        )],
    };
    let s = serde_json::to_string(&d).unwrap();
    let back: ComponentMetricDescriptor = serde_json::from_str(&s).unwrap();
    assert_eq!(d, back);
}

#[test]
fn cold_start_and_empty_extras_are_skipped_from_json() {
    let r = TurnRecorder::new();
    r.begin("stt:parakeet", "stt").finish();
    let snap = r.snapshot(1, false, None);
    let json = serde_json::to_value(&snap).unwrap();
    let comp = &json["components"][0];
    assert!(comp.get("cold_start").is_none());
    assert!(comp.get("extras").is_none());
    assert!(comp.get("error").is_none());
    assert!(comp.get("provider").is_none());
}

#[test]
fn all_registered_descriptor_fields_have_non_empty_anchors_and_owner() {
    let descriptors = all_canonical_descriptors();
    assert!(
        !descriptors.is_empty(),
        "all_canonical_descriptors must not be empty"
    );

    let mut missing = Vec::new();
    for d in &descriptors {
        for f in &d.fields {
            if f.start_anchor.as_ref().is_none_or(|s| s.trim().is_empty()) {
                missing.push(format!("{}.{} missing start_anchor", d.component_id, f.key));
            }
            if f.end_anchor.as_ref().is_none_or(|s| s.trim().is_empty()) {
                missing.push(format!("{}.{} missing end_anchor", d.component_id, f.key));
            }
            if f.owner.as_ref().is_none_or(|s| s.trim().is_empty()) {
                missing.push(format!("{}.{} missing owner", d.component_id, f.key));
            }
            if f.includes.as_ref().is_none_or(|s| s.trim().is_empty()) {
                missing.push(format!("{}.{} missing includes", d.component_id, f.key));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "The following descriptor fields lack non-empty anchors, owner, or includes:\n{}",
        missing.join("\n")
    );
}
