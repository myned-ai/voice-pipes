# Adding a new pipeline component

**Status:** active
**Last verified:** 2026-09-06 (moved into the public tree; content unchanged)
**Owner:** Docs

In voice-pipes, every "component" is anything that implements the [`Component`](../../voice-pipes/src/component/mod.rs) trait and gets registered into the worker's `ComponentRegistry`. Once registered, it shows up automatically on:

- `/debug/components` (worker) and `/api/v1/components` (orchestrator)
- the **Pipelines & Component Health** dashboard card (one pill per component, color-coded)
- the auto-degrader loop (Healthy ↔ Degraded based on rolling telemetry)
- the per-turn metrics table — if you also implement `turn_descriptor()`

There are two tracks depending on what your component does. Pick one.

> **Processing the agent's audio instead?** You may not need a new component
> at all. Anything that runs on outbound TTS audio plugs into the existing
> `[[audio_post_processor]]` table: `mode = "inline"` for processors that
> mutate samples or attach per-chunk `audio_meta.extras`, and
> `mode = "annotate"` for services that derive metadata — blendshapes,
> visemes, phonemes, emotion — off the critical path over a streaming gRPC
> stream ([AUDIO-ANNOTATORS.md](../guides/audio-annotators.md)). Both are gRPC-facing,
> so the service can be in any language; annotators surface in the per-turn
> metrics table as `annotator:<name>` automatically.

---

## Track A — A new provider for an existing stage (STT / TTS / LLM / VAD)

Use this when you want to plug, say, a new TTS engine in. You implement the existing trait (`TtsProvider`, `SttProvider`, `TextProcessor`, `VadEngine`) and it gets wired through the existing config factories.

### 1. Implement the stage trait + Component

```rust
// voice-pipes/src/tts/my_tts/mod.rs
use std::sync::Arc;
use std::time::{Duration, Instant};
use crate::component::{Component, ComponentCore, ComponentKind, ComponentTelemetry, HealthStatus};

pub struct MyTtsProvider {
    core: Arc<ComponentCore>,
    // ...your provider state...
}

impl MyTtsProvider {
    pub fn new(/* ... */) -> Self {
        Self {
            core: Arc::new(ComponentCore::new("tts:mytts", ComponentKind::Tts)),
        }
    }
}

#[async_trait::async_trait]
impl crate::tts::TtsProvider for MyTtsProvider {
    // ...required trait methods...

    // Hand the registry an Arc<dyn Component>. Most providers do this
    // by wrapping their ComponentCore in a tiny adapter struct.
    fn as_component(&self) -> Option<Arc<dyn Component>> {
        // ComponentCore does not implement Component itself — wrap it in
        // CoreComponent, which does. Build a custom adapter struct only when
        // you need more than the default behavior (e.g. a custom error_policy()).
        Some(Arc::new(crate::component::CoreComponent::new(self.core.clone())))
    }
}
```

`ComponentCore` already gives you everything `Component` needs — id, kind, atomic counters, a bounded latency histogram, `last_error`, an `extras` map, and `set_health()`. Call its helpers from your hot path:

| Hot-path event | Method |
|----------------|--------|
| Successful op | `core.record_ok(latency)` |
| Error | `core.record_error(err)` |
| Timeout | `core.record_timeout()` |
| Custom field (e.g. tokens/s) | `core.set_extra("tokens_per_sec", json!(42.5))` |
| Manual health flip | `core.mark_degraded("reason")` / `mark_unhealthy(...)` / `mark_healthy()` |

The auto-degrader will toggle `Healthy ↔ Degraded` for you based on rolling error rate / p99 latency, so don't over-call `mark_*` from inside the request loop.

### 2. Wire into the config factory

Add a `match` arm in `config::build_*_provider` ([config/factories.rs](../../voice-pipes/src/config/factories.rs)) for your new `provider` string. Existing arms (parakeet, assemblyai, piper, kokoro, etc.) are the template.

### 3. Done — registration is automatic

`bootstrap_transport` already calls `provider.as_component()` and registers what comes back ([commands/serve.rs](../../voice-pipes/src/commands/serve.rs)). No further changes needed.

---

## Track B — A genuinely new stage / sidecar component

Use this when your component is not a drop-in for STT/TTS/LLM/VAD — e.g. a moderation filter, a session memory cache, a redaction stage, an analytics tap, etc.

### 1. Pick a `ComponentKind`

Either reuse `ComponentKind::Other` or, if you expect the dashboard to group several similar things, add a variant. It's a simple `serde(rename_all = "snake_case")` enum — adding a variant requires updating the `Display` impl and any exhaustive `match`es. Frontend `ComponentKind` union (`voice-pipes-admin/frontend/src/components/PipelinesHealth.tsx`) needs the same string added.

### 2. Define your component type

Same pattern as Track A — embed `Arc<ComponentCore>` and either implement `Component` directly or expose `core` via an adapter:

```rust
pub struct ModerationFilter {
    core: Arc<ComponentCore>,
    // ...
}

impl ModerationFilter {
    pub fn new() -> Self {
        Self { core: Arc::new(ComponentCore::new("moderation:openai", ComponentKind::Other)) }
    }

    pub fn check(&self, text: &str) -> Result<(), ModerationError> {
        let started = Instant::now();
        match self.do_check(text) {
            Ok(()) => { self.core.record_ok(started.elapsed()); Ok(()) }
            Err(e) => { self.core.record_error(&e); Err(e) }
        }
    }
}

// Wrap the core in CoreComponent, which implements Component.
impl ModerationFilter {
    pub fn as_component(&self) -> Arc<dyn Component> {
        Arc::new(crate::component::CoreComponent::new(self.core.clone()))
    }
}
```

### 3. Register at boot

In `bootstrap_transport` (or wherever you instantiate it), add one line:

```rust
let moderation = Arc::new(ModerationFilter::new());
state.component_registry.register(moderation.as_component());
```

That's the entire integration. From this moment the component is visible everywhere.

---

## Optional — opt into the per-turn metrics table

If your component produces per-turn measurements (latencies, counters, token rates), implement `Component::turn_descriptor()` so the descriptor-driven `<TurnComponentMetricsTable>` on the playground + session-detail pages can render typed rows for it without any frontend changes:

```rust
use crate::pipeline::{ComponentMetricDescriptor, MetricFieldDescriptor, MetricUnit};

fn turn_descriptor(&self) -> Option<ComponentMetricDescriptor> {
    Some(ComponentMetricDescriptor {
        component_id: self.core.id().to_string(),
        kind: self.core.kind(),
        label: "Moderation".into(),
        fields: vec![
            MetricFieldDescriptor { key: "check_ms".into(), label: "Check".into(),
                                    unit: MetricUnit::Ms, format: None },
            MetricFieldDescriptor { key: "flagged".into(), label: "Flagged".into(),
                                    unit: MetricUnit::Count, format: None },
        ],
    })
}
```

Then, while emitting the per-turn `TurnComponentMetrics`, populate the matching keys in `extras` (see [`pipeline/extras.rs`](../../voice-pipes/src/pipeline/extras.rs) for how STT/LLM/TTS do it). The pipeline orchestration layer (`pipeline/orchestrate.rs` + `transport/serve/setup.rs`) already collects `turn_descriptor()` from every registered component and sends them in `SessionInfo.metric_descriptors`. The frontend will render rows automatically.

---

## What "free" means once registered

You get all of these without touching frontend or BFF code:

| Surface | Where |
|---------|-------|
| `GET /debug/components` per worker | `voice-pipes/src/api/worker_debug.rs` |
| `GET /api/v1/components` orchestrator-wide rollup | `voice-pipes/src/api/orchestrator/handlers.rs` |
| Worst-of aggregate health on the worker | `state.component_registry.aggregate_health()` |
| Auto-degrade on rolling error rate / p99 latency | `AutoDegrader` in `voice-pipes/src/component/mod.rs` |
| Pipeline card on the dashboard with a colored pill (kind + id + state, hover for reason / since / processed / errors / last_error) | `voice-pipes-admin/frontend/src/components/PipelinesHealth.tsx` |
| Per-turn typed-field rows on playground & session-detail (only if you implement `turn_descriptor()`) | `voice-pipes-admin/frontend/src/components/metrics/TurnComponentMetricsTable.tsx` |

---

## Quick checklist

1. Embed `Arc<ComponentCore>` in your type, with a stable id like `"kind:provider"`.
2. Call `core.record_ok / record_error / record_timeout` on every operation.
3. Either implement `Component` directly or return `core` from `as_component()`.
4. Register once at boot via `state.component_registry.register(...)`.
5. (Optional) Implement `turn_descriptor()` and populate matching `extras` keys.

That's it — nothing else needs to change for the new component to surface in the dashboard, in `/api/v1/components`, in the worker heartbeat rollup, in the auto-degrader, and (optionally) in the per-turn metrics table.
