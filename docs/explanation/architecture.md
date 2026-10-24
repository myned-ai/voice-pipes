# Streaming Architecture — Implementation Status

**Status:** active
**Last verified:** 2026-09-06 (moved into the public tree; content unchanged)
**Owner:** Docs

Consolidated record of the voice-pipes streaming-first pipeline architecture.
It supersedes the original streaming design plans.

---

## Executive Summary

voice-pipes started as an end-to-end batch pipeline: every stage (mic capture,
resample, STT, LLM, TTS, resample, playback) waited for the previous stage to
produce its complete output before starting. Perceived latency was 4–7 seconds
from end-of-speech to first audio.

A 7-phase streaming plan was designed and executed. **Phases 1–6 are fully
implemented. Phase 7 is partially implemented** (AssemblyAI streaming STT done,
Deepgram and Vosk not implemented). The pipeline is now streaming-first: all
stages run as concurrent tokio tasks connected by bounded mpsc channels with
cancellation propagation.

**Result:** Time-to-first-audio dropped from ~4–7 seconds to ~500ms–1.5s.

---

## Architecture

```
┌──────────┐     ┌──────────┐     ┌──────────┐     ┌──────────┐     ┌──────────┐     ┌──────────┐
│ Mic/     │     │ VAD      │     │ STT      │     │ LLM      │     │ TTS      │     │ Playback │
│ Transport│────>│ Filter   │────>│ Task     │────>│ Task     │────>│ Task     │────>│ Task     │
│          │ ch  │          │ ch  │          │ ch  │          │ ch  │          │ ch  │          │
└──────────┘     └──────────┘     └──────────┘     └──────────┘     └──────────┘     └──────────┘
  AudioChunk       AudioChunk       Utterance       TextFragment      AudioChunk
                   +is_end_of                       +hints
                    utterance

 CancellationToken propagates through all tasks ──────────────────────────────────►
 TurnState (Listening/Processing/Speaking) updated atomically ─────────────────────►
```

Each box is a `tokio::spawn` task. Bounded `mpsc` channels provide natural
backpressure. Dropping any sender causes downstream receivers to complete
gracefully.

### Key types

| Type | Location | Purpose |
|------|----------|---------|
| `AudioChunk` | `src/pipeline.rs` | PCM audio flowing between stages (samples, sample_rate, channels, is_end_of_utterance) |
| `Utterance` | `src/pipeline.rs` | Transcribed text from STT to LLM (text, utterance_id, confidence) |
| `TextFragment` | `src/text/mod.rs` | Sentence-level text from LLM to TTS (text, is_end_of_response, synthesis hints) |
| `PipelineTurn` | `src/pipeline.rs` | Per-utterance session with its own `CancellationToken` |
| `TurnState` | `src/pipeline.rs` | Atomic state enum: Listening → Processing → Speaking |
| `SynthesisHints` | `src/tts/mod.rs` | Per-fragment TTS overrides (voice, language, speed, options) |

---

## Implemented Features

### Phase 1: LLM Token Streaming + Sentence Chunking ✅

**Impact:** Highest — cut perceived latency by 2–5× by enabling TTS to start
synthesizing while the LLM is still generating.

**What was built:**

- **`TextProcessor::process_stream()`** — streaming trait method on `TextProcessor`
  (`src/text/mod.rs`). Sends `TextFragment` values via mpsc channel. Accepts a
  `CancellationToken` for interruption. Default implementation wraps batch
  `process()` for backward compat.

- **Sentence boundary detection** — `split_first_sentence()` in the SDK's `sentence` module, so plugin voices chunk text as built-in ones do.
  Splits on `.`, `!`, `?`, `\n` followed by whitespace or end-of-string.
  Known limitation: splits on abbreviations like "Dr." (acceptable tradeoff).

- **`run_chain_stream()`** — `src/text/mod.rs`. For multi-processor chains, runs
  all but the last processor in batch mode, then streams the final processor's
  output. Forwards cancellation token.

- **LLM streaming** — `LlmProcessor::process_stream()` overrides the default.
  Accumulates tokens into a sentence buffer, flushes to the channel on sentence
  boundaries. Remaining buffer flushed with `is_end_of_response: true` at EOS.

- **OpenAI-compatible streaming** — The `openai-compat` text processor also
  implements `process_stream()` using SSE streaming from the external LLM server.

### Phase 2: Streaming Resampler ✅

**Impact:** Enabler for chunk-based pipeline — eliminates batch resampling
bottleneck.

**What was built:**

- **`StreamingResampler`** — `src/audio/resample.rs`. Stateful resampler that
  maintains a lookahead/history buffer and fractional position across chunk
  boundaries. Uses polyphase sinc interpolation.

- **`process_chunk(&mut self, &[i16]) -> Vec<i16>`** — processes a single 10–30ms
  chunk and returns resampled output. No artifacts at chunk boundaries.

- **`flush()`** — emits tail samples from the history buffer after the final chunk.

- Existing batch `resample()` function preserved for backward compat.

- Extensive tests verify streaming output matches batch output within ±1 LSB.

### Phase 3: Channel-Based Pipeline Orchestrator ✅

**Impact:** Structural backbone — all stages run concurrently with natural
backpressure.

**What was built:**

- **`src/pipeline.rs`** — pipeline module with message types, task functions,
  and orchestration logic.

- **Task functions:**
  - `stt_task()` — receives `AudioChunk`, buffers until end-of-utterance,
    calls `transcribe()` or `transcribe_stream()`, sends `Utterance` downstream.
  - `llm_task()` — receives `Utterance`, calls `run_chain_stream()`, sends
    `TextFragment` downstream. Uses `tokio::select!` with cancellation.
  - `tts_task()` — receives `TextFragment`, calls `synthesize_stream()`,
    resamples output per-chunk via `StreamingResampler`, sends `AudioChunk`
    downstream. Handles cancellation mid-synthesis.
  - `playback_task()` — receives `AudioChunk`, feeds into a persistent cpal
    output stream via a ring buffer. No open/close per utterance.

- **`process_utterance()`** — orchestrator function that spawns LLM, TTS, and
  audio forwarding tasks concurrently for a single utterance. Measures
  TTFT (time to first text fragment) and TTFA (time to first audio chunk).

- **Channel buffer sizing:**
  | Channel | Bound | Rationale |
  |---------|-------|-----------|
  | mic → VAD | 64 | ~1.3s of 20ms chunks, prevents mic data loss |
  | VAD → STT | 64 | Buffer for utterance accumulation |
  | STT → LLM | 4 | Few utterances queue; backpressure is fine |
  | LLM → TTS | 16 | ~16 sentences; LLM may outpace TTS |
  | TTS → playback | 32 | Buffer for smooth playback |

### Phase 4: Streaming Mic Input + VAD ✅

**Impact:** Eliminated the 1.5s silence-timeout delay from the latency budget.

**What was built:**

- **`mic_stream()`** — `src/audio/io.rs`. Continuous mic capture emitting
  fixed-size `AudioChunk` every ~20ms via mpsc channel. Runs on a blocking
  thread (cpal callbacks are sync). Now deprecated in favor of
  `mic_capture_raw()` + `vad_filter_task()`.

- **`vad_filter_task()`** — `src/pipeline.rs`. Standalone async task that
  receives raw audio chunks and segments them into utterances:
  - Pre-roll buffer (5 chunks) — captures audio before speech onset.
  - Onset confirmation (3 consecutive speech chunks) — avoids false triggers.
  - End-of-utterance detection when speech → silence transition exceeds threshold.
  - Sets `is_end_of_utterance: true` on the final chunk of each utterance.

- **`VadBackend` trait** — `voice-pipes-sdk/src/vad.rs`, built-ins in
  `src/vad/segmenter.rs`. Pluggable VAD:
  - Silero (plugin crate `voice-pipes-vad-silero`, CLI feature `vad-silero`) —
    Silero VAD v5 ONNX model. Processes 512-sample windows at 16kHz. Returns
    speech probability. The binary names it the backend `mode = "auto"` runs.
  - `EnergyVad` — Simple RMS energy-based fallback. No model dependency.
  - `PassthroughVad` — every chunk is speech, for providers with their own VAD.

- `record_utterance()` preserved for file-input and non-streaming modes.

### Phase 5: Streaming TTS Output ✅

**Impact:** First audio heard while LLM is still generating later sentences.

**What was built:**

- **`TtsProvider::synthesize_stream()`** — `src/tts/mod.rs`. Streaming trait
  method that sends `AudioFrame` chunks via mpsc channel. Default wraps batch
  `synthesize()`.

- **`SynthesisRequest`** — what `synthesize_stream()` takes: the text, optional
  `SynthesisHints` for per-fragment voice/language/speed overrides, and a
  word-timing channel for providers that report word timestamps.

- **Kokoro streaming** — `src/tts/kokoro/mod.rs`. Streams output in ~170ms
  chunks (`STREAM_CHUNK_SAMPLES = 4096` at 24kHz). Supports per-fragment
  voice and speed hints.

- **Piper streaming** — `src/tts/piper/mod.rs`. Spawns synthesis in a blocking
  task, sends frames chunk-by-chunk.

- **Per-fragment resampling** — TTS task creates a `StreamingResampler` when
  TTS output rate differs from playback device rate. Resamples each chunk
  incrementally, flushes tail at end-of-response.

- **Persistent playback stream** — `playback_task()` opens the cpal output
  device once and feeds a ring buffer from incoming chunks. Eliminates
  click/pop artifacts from repeated stream open/close. Outputs silence (0.0)
  on buffer underrun rather than crashing.

### Phase 6: Interruption Handling ✅

**Impact:** Natural conversation — user can interrupt agent mid-response.

**What was built:**

- **`CancellationToken` propagation** — from `tokio_util::sync::CancellationToken`.
  Each `PipelineTurn` has its own token. Calling `cancel()` propagates to all
  tasks that hold a clone.

- **`TurnState` atomic state machine** — `src/pipeline.rs`:
  ```
  Listening → Processing → Speaking → Listening
                                    ↗
  (interrupt at any point) → cancel
  ```

- **Per-task cancellation behavior:**
  - LLM task: `tokio::select!` between `cancel.cancelled()` and the generation
    loop. Stops token generation immediately on cancel.
  - TTS task: checks cancellation before each fragment synthesis. On cancel,
    drains remaining fragments without processing.
  - Playback task: on cancel, clears the ring buffer (stops audio immediately)
    and drains the audio channel.
  - `process_utterance()`: aborts spawned task handles on cancellation.

- **Interrupt detection**: VAD continues monitoring mic input during `Speaking`
  state. When speech is detected, the current turn's token is cancelled and a
  new turn begins with the incoming utterance.

### Phase 7: Streaming STT ⚠️ Partial

**What was built:**

- **`SttProvider::transcribe_stream()`** — `src/stt/mod.rs`. Trait method for
  streaming STT: receives `AudioChunk` via mpsc, sends `TranscriptionResult`
  (with `is_final` flag) via mpsc.

- **AssemblyAI streaming provider** ✅ — `src/stt/assemblyai/`. Full
  implementation with:
  - WebSocket client (`ws_client` submodule) connecting to AssemblyAI's
    real-time V3 API.
  - Message types for the protocol (`messages.rs`).
  - Turn detection support.
  - Configurable via `provider = "assemblyai"` with API key.
  - Feature-gated as `stt-assemblyai`.

- **Pipeline integration** — `stt_task()` in `src/pipeline.rs` spawns a
  `transcribe_stream` worker internally. Sends audio chunks to the STT provider
  and receives transcription results incrementally. Applies a "flush trick"
  (500ms silence) to force right-context evaluation in encoder models.

- **Parakeet** — batch-only (`transcribe_stream` not implemented). Remains the
  default local STT with VAD-segmented batch processing.

**What was NOT built:**

| Provider | Status | Notes |
|----------|--------|-------|
| Deepgram streaming | ❌ Not implemented | Was planned as Option C. No `src/stt/deepgram/` module exists. |
| Vosk local streaming | ❌ Not implemented | Was planned as Option D. Lower accuracy than Parakeet. |
| Whisper.cpp | ❌ Not implemented | Was planned as Option B. Not needed given Parakeet accuracy. |

---

## Beyond the Original Streaming Plan

Several capabilities were built that go beyond the original 7-phase plan:

### WebSocket Transport Layer
- Full WebSocket server in `src/transport/ws/` — binary frames for audio, JSON
  text frames for control messages.
- `TransportSession` trait with connection lifecycle, health monitoring, and
  reconnection logic.
- Bridge between transport and pipeline channels (`src/transport/bridge.rs`).
- Enables remote clients to connect to the streaming pipeline.

### gRPC Split-Mode (Component Server)
- Proto definitions for `SttService`, `TtsService`, and `TextProcessorService`
  with both batch and streaming RPCs.
- Client providers: `GrpcSttProvider`, `GrpcTtsProvider`, `GrpcTextProcessor`
  that implement the standard traits but forward calls over gRPC.
- Feature-gated: `stt-grpc`, `tts-grpc`, `text-grpc`, `component-server`.
- Allows running STT, TTS, and LLM as separate services connected via gRPC,
  enabling independent scaling per pipeline stage.

### Audit/Tap System
- Pipeline observation points (`AuditTap`, `TapSide`) for mid-stream telemetry.
- `StreamingAuditSink` for real-time event streaming to external consumers.
- `AuditSink` with `AuditRecord` for persistent audit trail storage.
- Captures pipeline events (`TraceEvent`) for offline analysis.

### Admin API
- REST endpoints for orchestrator control, worker debug, and internal admin.
- Built on Axum + Tower.

### OpenTelemetry Integration
- `src/telemetry.rs` — feature-gated `telemetry` feature.
- Distributed tracing for pipeline stages.

### Synthesis Hints
- `SynthesisHints` type enables per-fragment TTS configuration (voice,
  language, speed, arbitrary options). The LLM can annotate `TextFragment`
  values with hints that the TTS engine uses for that specific sentence.

---

## Latency Budget

| Stage | Before (batch) | After (streaming) | Phase |
|-------|----------------|--------------------|-------|
| Mic capture → VAD | ~1500ms (silence timeout) | ~200ms (VAD endpointing) | 4 |
| Resample (in) | < 5ms | < 1ms per chunk | 2 |
| STT | 200–500ms | 200–500ms batch, 50–200ms streaming (AssemblyAI) | 7 |
| **LLM first token** | **2000–5000ms (full gen)** | **50–200ms** | **1** |
| TTS (first sentence) | Waits for full LLM output | Starts after 1st sentence (~200ms gen) | 1 + 5 |
| Resample (out) | < 5ms | < 1ms per chunk | 2 |
| **Time to first audio** | **~4–7 seconds** | **~500ms–1.5s** | **All** |

---

## Future Work

Items from the original plan that remain unimplemented, plus new directions:

### Streaming STT Providers
- **Deepgram** — cloud streaming STT via WebSocket. Would provide a second
  cloud STT option alongside AssemblyAI with potentially different
  accuracy/latency characteristics.
- **Vosk** — local streaming STT via Kaldi. Only if offline streaming is a
  hard requirement. Accuracy is below Parakeet/Whisper.
- **Whisper.cpp** — alternative local batch STT via `whisper-rs`. Only if
  Parakeet accuracy proves insufficient.

### Echo Cancellation
- Currently, the mic may pick up the agent's TTS output during playback and
  trigger a false interrupt. A basic AEC (acoustic echo cancellation) stage
  before VAD would improve interrupt reliability. The original Phase 6 plan
  noted this as future work.

### True Sub-Sentence TTS Streaming
- Current streaming is sentence-level: each sentence is synthesized as a
  complete unit. For even lower latency, VITS/Kokoro could potentially output
  audio in overlapping sub-utterance windows. This is complex and the
  sentence-level approach is sufficient for most use cases.

### Parakeet Streaming Mode
- Parakeet TDT uses full-sequence self-attention and cannot do true
  frame-by-frame streaming. A pseudo-streaming approach (buffer-and-recompute
  with expanding windows) was explored but not productionized. Would reduce
  local STT latency without requiring a cloud provider.
