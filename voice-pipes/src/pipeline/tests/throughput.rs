use super::*;
use std::time::Duration;

#[test]
fn output_rtf_and_overhead_math() {
    let mut o = OutputThroughput::default();
    assert!(o.finish().is_none(), "no audio → no component");
    // Fake the clocks directly: 1000 ms of audio delivered over 600 ms
    // by the source and 1200 ms at the emit side.
    let t0 = Instant::now();
    o.first_source = Some(t0);
    o.last_source = Some(t0 + Duration::from_millis(600));
    o.first_emit = Some(t0 + Duration::from_millis(50));
    o.last_emit = Some(t0 + Duration::from_millis(1250));
    o.audio_ms = 1000;
    assert_eq!(o.emit_rtf(), Some(1.2));
    assert_eq!(o.source_rtf(), Some(0.6));
    assert_eq!(o.chain_overhead_ms(), 600);
    let c = o.finish().unwrap();
    assert_eq!(c.component_id, "output");
    assert_eq!(c.kind, "output");
    assert_eq!(c.extras["emit_rtf"], 1.2);
    assert_eq!(c.extras["source_rtf"], 0.6);
    assert_eq!(c.extras["chain_overhead_ms"], 600);
    assert_eq!(c.extras["audio_output_ms"], 1000);
    assert_eq!(c.latency_ms, Some(1200));
}

#[test]
fn output_ignores_empty_chunks_and_handles_interrupted_turns() {
    let mut o = OutputThroughput::default();
    o.on_source(0); // sentinel
    assert!(o.finish().is_none());
    o.on_source(40);
    o.on_emit();
    // Interrupted after one chunk: still a component over the span so far.
    let c = o.finish().expect("component for the delivered span");
    assert_eq!(c.extras["audio_output_ms"], 40);
    assert_eq!(o.chain_overhead_ms(), 0);
}

#[test]
fn chain_stats_percentiles_budget_ratio_and_components() {
    let mut s = ChainStats::default();
    for l in [10u64, 20, 30, 40, 50] {
        assert!(s.record("grpc", l, 40, CallOutcome::Ok).is_none());
    }
    s.record("grpc", 2000, 40, CallOutcome::Timeout);
    s.record("noop", 1, 40, CallOutcome::Ok);
    let comps = s.components();
    assert_eq!(
        comps.len(),
        2,
        "one component per processor, first-call order"
    );
    let g = &comps[0];
    assert_eq!(g.component_id, "audio_post:grpc");
    assert_eq!(g.kind, "audio_post");
    assert_eq!(g.provider.as_deref(), Some("grpc"));
    assert_eq!(g.extras["calls"], 6);
    assert_eq!(g.extras["latency_p50_ms"], 40);
    assert_eq!(g.extras["latency_max_ms"], 2000);
    assert_eq!(g.extras["timeouts"], 1);
    assert_eq!(g.extras["failures"], 0);
    // mean latency (2150/6 = 358.3) ÷ mean chunk (40) = 8.96
    assert_eq!(g.extras["budget_ratio"], 8.96);
    assert_eq!(comps[1].component_id, "audio_post:noop");
}

#[test]
fn slowness_warning_fires_after_ten_consecutive_slow_chunks_once_per_turn() {
    let mut s = ChainStats::default();
    for _ in 0..9 {
        assert!(s.record("grpc", 58, 41, CallOutcome::Ok).is_none());
    }
    let msg = s
        .record("grpc", 58, 41, CallOutcome::Ok)
        .expect("10th consecutive slow chunk warns");
    assert!(msg.contains("\"grpc\" cannot keep up"));
    assert!(msg.contains("p50 58 ms per 41 ms chunk"));
    assert!(msg.contains("budget_ratio 1.41"));
    // Keeps being slow at the same ratio: no repeat.
    for _ in 0..20 {
        assert!(s.record("grpc", 58, 41, CallOutcome::Ok).is_none());
    }
    // Ratio doubles → warns again.
    let mut again = None;
    for _ in 0..200 {
        again = s.record("grpc", 1000, 41, CallOutcome::Ok).or(again);
    }
    assert!(again.is_some(), "warns again once the ratio has doubled");
    // A fast chunk resets the streak.
    let mut fresh = ChainStats::default();
    for i in 0..30 {
        let latency = if i == 9 { 1 } else { 58 };
        assert!(fresh.record("grpc", latency, 41, CallOutcome::Ok).is_none() || i >= 19);
    }
}

#[test]
fn descriptors_carry_health_semantics() {
    let out = ComponentMetricDescriptor::output();
    assert_eq!(out.component_id, "output");
    let emit = out.fields.iter().find(|f| f.key == "emit_rtf").unwrap();
    let h = emit.health.as_ref().unwrap();
    assert_eq!(h.budget, Some(0.9));
    assert_eq!(h.on_critical_path, Some(true));
    let bands = h.severity_bands.unwrap();
    assert!((bands.warn - 1.0).abs() < 1e-9);
    assert!((bands.severe - 1.2 / 0.9).abs() < 1e-9);
    let post = ComponentMetricDescriptor::audio_post("grpc");
    assert_eq!(post.component_id, "audio_post:grpc");
    let ratio = post
        .fields
        .iter()
        .find(|f| f.key == "budget_ratio")
        .unwrap();
    assert_eq!(ratio.health.as_ref().unwrap().budget, Some(0.8));
}
