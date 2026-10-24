# A Composable VAD Architecture for Voice Agents

**Status:** active
**Last verified:** 2026-09-06 (moved into the public tree; content unchanged)
**Owner:** Docs

## The hidden coupling

The first version of our barge-in code looked like this:

```rust
async fn interrupt_monitor(
    mic_rx: Receiver<AudioChunk>,
    turn_state: SharedTurnState,
    cancel: CancellationToken,
    settings: BargeInSettings,
) {
    while let Some(chunk) = mic_rx.recv().await {
        if get_turn_state(&turn_state) != TurnState::Speaking {
            continue;
        }
        if compute_rms(&chunk.samples) > settings.speech_threshold() {
            cancel.cancel();
        }
    }
}
```

One function. RMS. Hardcoded. It worked in the demo room. It missed one in three interruptions in real rooms. We knew, conceptually, that:

- AssemblyAI emits a `partial_transcript` event the instant it detects speech — usually 80 ms before our RMS would trip.
- The user might want to press a "Stop" button in the UI, which should also count as an interruption.
- A future Silero ONNX VAD would be more accurate than RMS.

But there was no place to *put* those signals. The cancellation was hardwired into an `interrupt_monitor` that only knew about RMS. Adding a second source meant a second `interrupt_monitor`, racing into the same `CancellationToken`, with no shared gating logic. And if either one's "is barge-in allowed right now?" check drifted from the other, we'd get inconsistent behavior.

## The trait

The fix is to stop thinking about "the interrupt monitor" and start thinking about "any number of speech-event sources funneling into one channel."

```rust
pub enum VadEvent {
    SpeechStarted { source: String, timestamp: Duration },
    SpeechStopped { source: String, timestamp: Duration },
}

#[async_trait]
pub trait VadEngine: Send + Sync {
    fn name(&self) -> &'static str;
    async fn start(&mut self, tx: mpsc::Sender<VadEvent>);
    fn as_component(&self) -> Option<Arc<dyn Component>> { None }
}
```

That's it. Every detector implements `VadEngine`. The orchestrator owns the receiving end of one `mpsc::Receiver<VadEvent>` and doesn't care where events came from. `as_component` is the optional hook that registers the engine with the component metrics registry — telemetry comes for free.

## Shared gating

The single most important rule is that the "should barge-in fire right now?" decision is shared across engines:

```rust
pub fn should_evaluate_barge_in(
    state: TurnState,
    settings: &BargeInSettings,
) -> bool {
    match state {
        TurnState::Speaking | TurnState::Draining => true,
        TurnState::Processing => settings.barge_in_during_processing(),
        TurnState::Listening => false,
    }
}
```

Every engine calls this before emitting. Three properties matter:

1. **It's exhaustive.** A new `TurnState` variant won't compile until every engine reconsiders it. We added `Draining` after the fact — the compiler walked us to every gating site.
2. **`Draining` allows barge-in.** This is the state where the server is done generating but the client is still playing. Without it, users who cut in at the end of a sentence get ignored.
3. **`Processing` is configurable.** Allowing barge-in during LLM generation makes the agent feel snappier; disallowing prevents accidental cancellation from coughs.

## The three production engines

### `RmsVadEngine` — the fast path

Local RMS over PCM. Sub-20 ms reaction time. False-positives on transient noise. Independent of any provider.

```rust
let rms = compute_rms(&chunk.samples);
if rms > self.settings.speech_threshold() {
    speech_chunks += 1;
    if speech_chunks >= self.settings.min_chunks() {
        tx.send(VadEvent::SpeechStarted { source: "rms".into(), .. }).await?;
    }
}
```

Useful as a hyper-fast trigger. Useless on quiet users.

### `ProviderVadEngine` — the accurate path

Adapts an STT provider's native speech-start signal. The non-obvious move here is to *not* depend on the STT crate's types directly:

```rust
#[async_trait]
pub trait SpeechSignalSource: Send + Sync {
    async fn next_speech_event(&mut self) -> Option<SpeechSignal>;
}

pub struct SttResultSignalSource { /* adapts an STT result receiver */ }
impl SpeechSignalSource for SttResultSignalSource { … }

pub struct ProviderVadEngine {
    source: Option<Box<dyn SpeechSignalSource>>,
    …
}
```

`ProviderVadEngine` knows nothing about AssemblyAI, OpenAI Realtime, or Deepgram. It consumes a `dyn SpeechSignalSource`. Each STT provider ships its own adapter. Adding a new provider doesn't touch the VAD code.

This is the same pattern as the metrics extras: a typed boundary inside the crate, an open boundary at the seam. STT providers can't pollute VAD; VAD can't reach into STT internals.

### `ClientVadEngine` — the zero-RTT path

Adapts a `{action: "cancel"}` WebSocket message from the UI. When the user clicks the stop button, the client *also* flushes its audio locally before the server even knows. The engine just translates the wire message into a `VadEvent::SpeechStarted { source: "client" }`.

## The composite — first to fire wins

Three engines, one cancellation token. The composition is trivial because the trait is right:

```rust
pub struct CompositeVadEngine {
    engines: Vec<Box<dyn VadEngine>>,
}

#[async_trait]
impl VadEngine for CompositeVadEngine {
    fn name(&self) -> &'static str { "composite" }

    async fn start(&mut self, tx: mpsc::Sender<VadEvent>) {
        for engine in self.engines.drain(..) {
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut e = engine;
                e.start(tx).await;
            });
        }
    }
}
```

Every engine runs concurrently, every engine sends into the same `tx`. The orchestrator's receiver gets the *first* event from *any* engine. Whichever wins, wins.

The `source` field on `VadEvent` is purely for observability — once the orchestrator receives an event, it cancels the token unconditionally. Late events from other engines for the same turn arrive after `state == Listening` and are dropped by `should_evaluate_barge_in`.

## Why this matters

The composite gives you something no single VAD can: **the minimum of all detector latencies, and the union of their recall.**

| Configuration | p50 trip latency | Misses (quiet user) | False positives |
|--|--|--|--|
| RMS only | 30 ms | 30% | Medium |
| Provider only | 110 ms | 5% | Low |
| Client only | 5 ms (when used) | N/A | None |
| **Composite (RMS + Provider + Client)** | **30 ms** | **5%** | **Medium** |

You give up nothing on speed (RMS still wins races on loud interruptions) and get the recall of the provider VAD on quiet users. The client button is the cherry on top: a manual override that's effectively zero-latency because it doesn't even need a server round-trip.

## What we'd do differently

Two regrets:

1. **We didn't make the engine list config-driven from day one.** Hardcoding the composite's children meant a recompile to test "RMS + Client without Provider." The trait was right; the wiring was lazy.
2. **We treated `SpeechStopped` as cosmetic.** It shouldn't be — it's the right primitive for end-of-turn detection too. We currently route end-of-turn through STT's `end_of_turn` flag separately, which means two parallel "is the user done?" pipelines. Unifying them under `VadEvent::SpeechStopped` from a `ProviderVadEngine` would eliminate a class of off-by-one bugs we still occasionally see.

## Takeaways

1. **Make speech detection a trait, not a function.** The first time you want a second detector, you'll already have shipped the wrong abstraction.
2. **Decouple the engine from its data source.** `ProviderVadEngine` knows nothing about STT; it consumes a `SpeechSignalSource`. Each provider ships its own adapter.
3. **Share gating logic across engines.** One `should_evaluate_barge_in` function, exhaustive `match` on `TurnState`, called from every engine.
4. **Compose with first-to-fire.** Concurrent engines into one channel. You stop choosing between latency and accuracy and get the minimum of both.
5. **Treat manual UI cancel as a VAD source.** It's not a special case; it's the zero-RTT engine in the composite.

The pattern generalizes beyond VAD. Anywhere you have multiple ways to detect "the same kind of event" — endpointing, content-policy violations, agent handoff signals — the same trait + composite shape applies. Build the bus, then plug detectors in.
