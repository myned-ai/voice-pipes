# Beyond TTFT — The Metrics That Actually Predict Whether Your Voice Agent Feels Alive

**Status:** active
**Last verified:** 2026-09-06 (moved into the public tree; content unchanged)
**Owner:** Docs

## The number everyone publishes is the wrong one

Open any voice-AI vendor's pricing page and you'll see a single latency number. "Sub-300 ms response time." "200 ms TTFT." It's always one number, it's always a median, and it tells you almost nothing about whether the agent will feel good in production.

The actual experience of a voice agent is governed by *which stage was slow*, *how often it was slow*, and *whether your metric noticed*. After instrumenting our pipeline at the per-component level, we now believe three things:

1. **End-to-end latency is a useless top-level KPI.** It conflates LLM verbosity, network, audio pacing, and pipeline stages you can actually fix.
2. **p50 of any stage will lie to you.** What kills perceived quality is p99 of the *worst* stage on any given turn.
3. **You cannot add metrics to a closed schema fast enough.** If adding a translator, a content filter, or a RAG step requires a 9-file shotgun edit to the wire format, the team will stop measuring.

This is the design we landed on after the third schema-edit shotgun.

## What a turn actually looks like

For a single user question, here's what's happening in time order:

```
t=0     User stops talking (last audio chunk silence-edge detected)
t=80    VAD/STT commits "end of utterance"
t=120   STT emits final transcript
t=125   LLM request sent
t=380   LLM emits first token             ← traditional "TTFT"
t=410   First token reaches TTS
t=520   TTS emits first audio frame       ← traditional "TTFB"
t=560   First audio frame on user's speakers
t=2400  LLM emits last token
t=2700  TTS finishes synthesizing
t=3800  Last audio frame finishes playing
```

The numbers vendors publish are usually `t=560` (TTFB) or `t=380` (TTFT). Neither tells you:

- **EOU delay** (`t=80 − t=0`): how long after silence the pipeline committed. This is the "did I finish talking?" pause that makes agents feel laggy *before* they even start replying.
- **STT latency** (`t=120 − t=80`): how long final transcription took after commit.
- **LLM TTFT** (`t=380 − t=125`): the model's actual first-token time, isolated from STT.
- **TTS TTFB-internal** (`t=520 − t=410`): TTS first-audio measured from *its* input, not from utterance start. This is the only honest TTS comparison.
- **LLM generation rate**: tokens/second over `t=380 → t=2400`.
- **Audio tail** (`t=3800 − t=2700`): how long the client kept playing after the server stopped emitting. This is your `Draining` window — the one that breaks barge-in if you don't model it.

A naïve "300 ms TTFT" can hide a 200 ms EOU delay (your endpointing is bad), 50 ms STT (fine), 50 ms LLM TTFT (great). Or it can hide 20 ms EOU, 20 ms STT, 260 ms LLM TTFT. Same number, completely different optimization targets.

## p50 lies; p99-of-worst-stage tells the truth

Suppose you have three stages, each with this distribution per turn:

| Stage | p50 | p99 |
|-----|-----|-----|
| STT | 80 ms | 600 ms |
| LLM TTFT | 250 ms | 1200 ms |
| TTS TTFB | 90 ms | 800 ms |

Naïve aggregate p50 looks like 420 ms. Lovely.

But the p99 of the *user-perceived first response* isn't the sum of p99s, and it isn't the p99 of the sum. It's `max` of the per-turn worst stage, because on any given turn the user notices the slowest one. About 3% of turns will have *one* stage in its tail. Those are the turns the user remembers.

The metric that correlates with churn, in our admittedly small sample, is:

```
worst_stage_p99 = max over stages of (per-stage p99)
```

If any stage has a p99 above ~1.2 s, users start repeating themselves. It does not matter if your end-to-end p50 is excellent.

This implies two things:

1. **Track p99 per stage, not aggregate.** Aggregates hide the dimension you need to fix.
2. **Tail-cut the worst stage first.** Don't optimize LLM TTFT from 250→200 ms when STT p99 is 600 ms.

## The schema that doesn't fight you

Every metrics system starts as named fields:

```rust
struct TurnMetrics {
    ttft_ms: Option<u64>,
    ttfb_ms: Option<u64>,
    stt_latency_ms: Option<u64>,
    llm_generation_ms: Option<u64>,
    tts_ttfb_ms: Option<u64>,
    eou_delay_ms: Option<u64>,
}
```

This works for exactly as long as the pipeline has fixed stages. Then someone adds a translator. Then a content filter. Then a RAG retriever. Each addition is a 9-file edit: pipeline struct, transport enum, serialization tests, wire schema, frontend types, table component, aggregator, audit log, benchmark runner.

We replaced all of it with a single open-ended slice:

```rust
pub struct TurnMetricsSnapshot {
    pub utterance_id: u32,
    pub total_ms: u64,
    pub interrupted: bool,
    pub components: Vec<TurnComponentMetrics>,
}

pub struct TurnComponentMetrics {
    pub component_id: String,        // "stt:assemblyai"
    pub kind: String,                // "stt" | "llm" | "tts" | "vad" | …
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>, // None = span open at snapshot time
    pub latency_ms: Option<u64>,
    pub provider: Option<String>,
    pub cold_start: bool,
    pub error: Option<String>,
    pub extras: serde_json::Map<String, serde_json::Value>,
}
```

Three properties make this work:

1. **`kind` is a string, not an enum.** A new component kind never breaks the wire. An enum variant addition would.
2. **`finished_at_ms: Option`** lets an interrupted component appear in the snapshot with no end time. That's signal, not a bug — it tells you exactly which stage was running when the user barged in.
3. **`extras` is open.** Each component owns a typed `*Extras` struct internally (`TtsExtras { ttfb_ms, rtf, sample_rate, … }`) but serializes through the same map. Producers don't sprinkle untyped `set("rtf", …)` calls — that's how key drift starts (`tokens_per_sec` vs `tps` vs `tokens_s`).

## RAII spans, central clock

The orchestrator owns turn lifetime, so it owns the clock. Components don't track their own start times — that's how you end up with two stages whose timestamps disagree by 30 ms and a metric that's mysteriously negative.

At the trait seams (`Utterance.committed_at`, first LLM fragment, first raw TTS audio chunk, and first post-processed emitted audio chunk), the engine records stack-only `Option<Instant>` timestamps on a single monotonic clock. From those timestamps it materializes contiguous telescoping stage latencies (`llm_wait_ms + tts_wait_ms + output_wait_ms == commit_to_first_audio_ms`) and exact component span windows (`started_at_ms`, `finished_at_ms`, `latency_ms`) via `ComponentSpan::finish_with_window`:

```rust
let mut span = recorder.begin(self.id(), self.kind().as_str());
span.set_provider("assemblyai");
span.extras_typed(&SttExtras { processing_ms, endpointing_ms });
// span.finish_with_window(started_at_ms, finished_at_ms) stamps exact seam windows
```

`begin()` / `finish_with_window()` stamps `started_at_ms` and `finished_at_ms` from the orchestrator's `commit_start` clock. `latency_ms` is computed from the same window. Insertion order in the `components` vec matches begin order, which matches pipeline order — no sort, no `Option<u64>` ordering hack. Provider-reported numbers (`connect_ms`, `prompt_ingest_ms`, `first_token_ms`, `compute_ms`) are breakdowns *inside* those engine-measured stage spans, never replacements for them.

## Descriptors: the part most teams skip

A generic snapshot is useless if the frontend has to hardcode "ttfb_ms" → "Time to first byte" and a unit of milliseconds. That's the closed-surface problem one level down.

Each component declares a descriptor at session-build time:

```rust
pub struct ComponentMetricDescriptor {
    pub component_id: String,
    pub kind: String,
    pub label: String,                // "Translator (en→es)"
    pub fields: Vec<MetricFieldDescriptor>,
}

pub struct MetricFieldDescriptor {
    pub key: String,                  // matches an extras key
    pub label: String,                // "Time to first translation"
    pub unit: MetricUnit,             // Ms | Percent | Ratio | TokensPerSec | …
    pub format: Option<String>,
}
```

The orchestrator collects descriptors once and ships them on `SessionReady`. The frontend renders one generic `<TurnComponentMetricsTable>` driven entirely by descriptors. New component → declares descriptor → table shows it with correct labels and units. **Zero frontend changes.**

This is the single most leveraged decision in the design. Without it, every new component still costs a frontend PR.

## What we actually watch

After all that, the dashboard we look at every day is short:

| Metric | Why |
|------|-----|
| `eou_delay` p50 / p99 | "Did I finish talking?" pause. Most-noticed by users. |
| Worst-stage p99 per turn | The actual quality killer. |
| Per-stage p99 (STT / LLM TTFT / TTS TTFB) | Where to invest. |
| LLM tokens/sec p50 | Predicts whether long answers feel slow. |
| `Draining` duration p99 | If high, your barge-in window is wide and your interruption UX will suffer. |
| `interrupted_rate` | High = users frustrated, OR good barge-in working as designed. Read alongside session length. |

Notably absent: aggregate end-to-end latency. We stopped looking at it. It moved when LLM verbosity moved and told us nothing about the pipeline.

## Takeaways

1. **End-to-end latency is a vanity metric.** Stage-level latencies tell you what to fix.
2. **Track p99 per stage and `max` across stages per turn.** That's the number that correlates with user behavior.
3. **Schema must be open.** A `Vec<TurnComponentMetrics>` with `kind: String` and a typed-but-serialized `extras` map removes the schema-edit tax that kills measurement culture.
4. **Ship descriptors with metrics.** Otherwise the frontend becomes the new closed surface.
5. **Central clock, RAII spans.** Components don't get to time themselves.

The system you can't see, you can't tune. The system you tune by p50 will surprise you in production. The system you tune by per-stage p99 will feel like a phone call.
