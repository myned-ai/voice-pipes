# Technical Architecture & Implementation Plan: NeuTTS Plugin (`voice-pipes-tts-neutts`)

## 1. Executive Summary

This document specifies the technical design, runtime architecture, and phased implementation plan for supporting [Neuphonic's NeuTTS](https://github.com/neuphonic/neutts) family of Speech Language Models (SLMs) as a native CPU-optimized Text-to-Speech (TTS) plugin within Voice Pipes.

NeuTTS is an open-source, on-device SLM featuring instant voice cloning, sub-realtime generation on CPU, and an ultra-compact neural audio codec (**NeuCodec**). Integrating NeuTTS into Voice Pipes gives users a state-of-the-art, 100% local, zero-cloud-dependency voice synthesizer capable of meeting Voice Pipes' strict sub-500ms glass-to-glass latency and immediate barge-in requirements.

---

## 2. NeuTTS Architecture & CPU Feasibility Analysis

### 2.1 Model Components & Dataflow
NeuTTS decomposes speech synthesis into two distinct stages:

```
+-------------------------------------------------------------------------+
|                              Text Input                                 |
|          ("Hello, I am running completely locally on CPU.")             |
+------------------------------------+------------------------------------+
                                     |
                                     v
+-------------------------------------------------------------------------+
|                  Phonemization & Text Normalization                     |
|    - NeuTTS-Air / Nano: espeak-ng IPA phonemes via voice-pipes-sdk      |
|    - NeuTTS-2E: Direct text tokens (no external phonemizer needed)      |
+------------------------------------+------------------------------------+
                                     |
                                     v
+-------------------------------------------------------------------------+
|              Acoustic Token Generator (Language Model)                  |
|    - Architecture: Qwen2-based autoregressive transformer (~0.5B / ~0.12B|
|    - Quantization: GGUF Q4_K_M (~280 MB RAM) / Q8_0                     |
|    - Conditioning: 3s reference audio tokens + prompt transcript        |
|    - Output: Discrete acoustic codes @ 50 Hz (1 token = 20ms audio)     |
+------------------------------------+------------------------------------+
                                     | (Streamed token-by-token)
                                     v
+-------------------------------------------------------------------------+
|                     Neural Codec Decoder (NeuCodec)                     |
|    - Architecture: Single-codebook FSQ convolutional neural vocoder     |
|    - Runtime: ONNX Runtime (CPU Execution Provider) via voice-pipes-onnx|
|    - Native Sample Rate: 24,000 Hz (0.8 kbps bitrate)                   |
+------------------------------------+------------------------------------+
                                     |
                                     v
+-------------------------------------------------------------------------+
|                    Resampling & Voice Pipes Framing                     |
|    - Polyphase / sinc downsampling: 24,000 Hz -> 16,000 Hz              |
|    - Output Format: 16-bit signed mono PCM (`AudioFrame`)               |
|    - Chunking: 4096 samples (~170ms) / 1600 samples (100ms) for stream |
+-------------------------------------------------------------------------+
```

### 2.2 CPU Feasibility & Performance Profile
* **Memory Footprint**:
  * NeuTTS-Air Q4_K_M: ~280 MB.
  * NeuCodec ONNX: ~45 MB.
  * Total resident memory per session pool: **< 400 MB RAM**.
* **Real-Time Factor (RTF)**:
  * On modern x86/ARM desktop CPUs (Ryzen 9, Intel Core Ultra, Apple Silicon M-series), NeuTTS GGUF generates **100+ acoustic tokens/sec**.
  * Since 1 token = 20ms audio (50 tokens = 1 second of speech), 100 tokens/sec translates to **RTF ~ 0.5 (2x faster than real time)**.
  * On edge hardware (Raspberry Pi 5, N100 mini PCs), NeuTTS-Nano hits **RTF ~ 0.8–0.9**, comfortably sustaining uninterrupted real-time streaming playback.
* **Time-To-First-Audio (TTFA)**:
  * Because token generation is autoregressive at 50 Hz, the vocoder can decode the first audio chunk as soon as the first 5–10 tokens (~100–200ms of audio) are emitted.
  * Initial TTFA on CPU is estimated at **80–130 ms**, well below the budget for conversational voice pipelines.

---

## 3. Plugin Architecture: `voice-pipes-tts-neutts`

### 3.1 Crate Layout
Following Voice Pipes workspace rules and SDK isolation requirements:
```
plugins/tts/neutts/
├── Cargo.toml
├── README.md
└── src/
    ├── lib.rs              # Plugin entry point, TtsProvider, TtsFactory, register()
    ├── engine.rs           # Multi-threaded inference session and SessionPool wrapper
    ├── backbone/
    │   ├── mod.rs          # Trait for acoustic model backend (GGUF / Sidecar)
    │   ├── candle_gguf.rs  # In-process GGUF runner using candle-transformers
    │   └── sidecar.rs      # High-speed local sidecar proxy (Phase 1)
    ├── codec.rs            # NeuCodec ONNX decoder using voice-pipes-onnx
    ├── resampler.rs        # 24 kHz -> 16 kHz high-fidelity sinc downsampler
    ├── schema.rs           # TOML configuration deserialization and validation
    ├── voices.rs           # Embedded voice reference embeddings & voice cloning profiles
    └── wizard.rs           # Interactive setup wizard descriptors for voice-pipes-cli
```

### 3.2 Configuration Specification (`pipeline.toml`)
```toml
[tts]
provider = "neutts"
# Variant: "air" (default, highest quality), "nano" (fastest, edge), or "2e" (emotional)
model = "air"
# Quantization: "q4_k_m" (default), "q8_0", or "fp16"
quantization = "q4_k_m"
# Number of concurrent synthesis engines in the session pool
pool_size = 2
# CPU threads allocated to ONNX / GGUF operations
threads = 4
# Voice profile: built-in preset or path to custom 3-second reference audio (.wav + .txt)
voice = "default_en"
# Optional reference audio for instant voice cloning:
# reference_audio = "models/neutts/voices/custom_agent.wav"
# reference_text = "models/neutts/voices/custom_agent.txt"

# Emotional mode (supported when model = "2e")
# emotion = "happy"  # neutral, happy, sad, angry, surprised, fearful, disgusted

[tts.chunking]
mode = "punctuation"
max_chunk_chars = 120
```

### 3.3 Core Implementation Details

#### 1. Trait Implementation (`TtsProvider`)
```rust
#[async_trait]
impl TtsProvider for NeuTts {
    async fn synthesize(&self, text: &str) -> Result<AudioFrame, TtsError> {
        let req = SynthesisRequest::new(text);
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        self.synthesize_stream(req, tx).await?;
        
        let mut samples = Vec::new();
        while let Some(frame) = rx.recv().await {
            samples.extend_from_slice(&frame.data);
        }
        Ok(AudioFrame::new(samples, 16_000))
    }

    async fn synthesize_stream(
        &self,
        req: SynthesisRequest<'_>,
        tx: tokio::sync::mpsc::Sender<AudioFrame>,
    ) -> Result<(), TtsError> {
        let checkout = self.pool.checkout_within(Duration::from_millis(500))
            .map_err(|e| TtsError::busy(format!("NeuTTS pool exhausted: {e}")))?;

        // Run synthesis on blocking threadpool with cancellation check
        tokio::task::spawn_blocking(move || {
            checkout.generate_streaming(req.text, req.hints, tx)
        }).await.map_err(|e| TtsError::InferenceFailed(e.to_string()))??;

        Ok(())
    }
}
```

#### 2. Cancellation & Instant Barge-in (Interruption Handling)
Voice Pipes guarantees immediate audio cutoff when the user starts speaking. In NeuTTS:
* The token generation loop checks `tx.is_closed()` before computing each new acoustic token.
* If the downstream audio sink is dropped (barge-in event triggered by Silero VAD / Smart Turn), the generation loop aborts immediately, saving CPU cycles.
* The session slot is returned to `SessionPool` instantly without lingering lockup.

#### 3. Phonemization & Subprocess Safety
* For NeuTTS-Air and Nano: Use `voice_pipes_sdk::phonemize` (which enforces `VP_PHONEMIZE_JOBS` concurrency limits, input argument whitelisting regex, and 10s child execution timeouts to prevent process leaks).
* For NeuTTS-2E: Phonemizer is bypassed entirely, speeding up cold-start turn time by ~20ms.

#### 4. Resampling Seam
* NeuCodec outputs audio at 24,000 Hz.
* Voice Pipes requires 16,000 Hz mono PCM (`TARGET_SAMPLE_RATE = 16_000`).
* Resampling is performed via an in-memory polyphase sinc filter (3:2 rational decimation) with minimal latency overhead (< 2ms for a 100ms audio chunk).

---

## 4. Phased Implementation Roadmap

```
                                IMPLEMENTATION TIMELINE
====================================================================================
Phase 1 (Week 1)          Phase 2 (Week 2)          Phase 3 (Week 3)
Sidecar Adapter           Native In-Process         Benchmarking & Demo
====================================================================================
[ Python/C++ Sidecar ] -> [ Candle / ONNX Rust ] -> [ voice-pipes-bench Suite ]
[ Streaming Unix/TCP ]    [ Zero Python Dep    ]    [ Admin Console UI Demo   ]
[ Benchmark Baseline ]    [ SessionPool Safety ]    [ Outreach & Case Study   ]
```

### Phase 1: High-Performance Streaming Sidecar (Rapid Prototyping & Validation)
* Build a local micro-service (Python / C++ with `neutts[all]`) that streams raw PCM frames over a Unix domain socket or localhost TCP stream.
* Implement `voice-pipes-tts-neutts` as a client communicating with this sidecar (similar to `voice-pipes-tts-pocket`).
* **Goal**: Establish immediate baseline benchmarks for latency, RTF, and voice quality within 48 hours without getting blocked by raw weight conversion or GGUF tensor format nuances.

### Phase 2: Fully Native In-Process Rust Plugin (Production Grade)
* Implement the pure-Rust inference pipeline:
  1. Qwen2 backbone using `candle-transformers` / GGUF reader with SIMD AVX2/AVX-512/NEON optimizations.
  2. NeuCodec ONNX model loaded via `voice_pipes_onnx::SessionPool`.
  3. Polyphase resampler in Rust (`rubato` or custom decimation filter).
* Ensure strict adherence to Voice Pipes workspace rules: `#![forbid(unsafe_code)]`, zero unwrap/expect in production code, `-D warnings`.

### Phase 3: Benchmarking, Documentation & Admin Console Integration
* Add NeuTTS to `voice-pipes-bench` automated benchmarking suite.
* Measure:
  * Time-to-First-Audio (TTFA) across text lengths (10, 50, 150 words).
  * Real-Time Factor (RTF and RTFx) on 1, 4, 16 concurrent streams.
  * Barge-in interruption recovery latency.
* Add wizard configuration to `voice-pipes-cli`:
  * `voice-pipes init` -> interactive selection of NeuTTS.
* Build a live playground preset in `voice-pipes-admin`.
