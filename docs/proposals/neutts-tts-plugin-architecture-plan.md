# Technical Architecture & Implementation Plan: NeuTTS-Air Plugin (`voice-pipes-tts-neutts`)

**Status:** active

## 1. Executive Summary

This document specifies the technical design, runtime architecture, and implementation plan for supporting [Neuphonic's NeuTTS-Air](https://github.com/neuphonic/neutts) Speech Language Model (SLM) as a native CPU-optimized Text-to-Speech (TTS) plugin within Voice Pipes.

NeuTTS-Air is an open-source, on-device SLM released under the **Apache 2.0** license. It features instant zero-shot voice cloning (3–15 seconds of reference audio), sub-realtime generation on CPU, and an ultra-compact 24 kHz neural audio codec (**NeuCodec**). Integrating NeuTTS-Air provides a 100% local, zero-cloud-dependency voice synthesizer capable of meeting Voice Pipes' strict sub-500ms glass-to-glass latency and immediate barge-in requirements.

---

## 2. NeuTTS-Air Architecture & CPU Feasibility Analysis

### 2.1 Model Components & Dataflow
NeuTTS-Air synthesizes speech in two coupled stages:

```
+-------------------------------------------------------------------------+
|                              Text Input                                 |
|          ("Hello, I am running completely locally on CPU.")             |
+------------------------------------+------------------------------------+
                                     |
                                     v
+-------------------------------------------------------------------------+
|                  Phonemization & Text Normalization                     |
|    - espeak-ng IPA phonemes via `voice_pipes_sdk::phonemize`            |
|    - Subprocess limiter: `VP_PHONEMIZE_JOBS` & strict whitelist regex   |
+------------------------------------+------------------------------------+
                                     |
                                     v
+-------------------------------------------------------------------------+
|              Acoustic Token Generator (Language Model)                  |
|    - Architecture: Qwen2-based autoregressive transformer (~0.5B)       |
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
|    - Polyphase 3:2 downsampler: 24,000 Hz -> 16,000 Hz                  |
|    - Output Format: 16-bit signed mono PCM (`AudioFrame`)               |
|    - Chunking: 1600 samples (100ms) for ultra-low streaming latency     |
+-------------------------------------------------------------------------+
```

### 2.2 CPU Feasibility & Performance Profile
* **Memory Footprint**:
  * NeuTTS-Air Q4_K_M: ~280 MB.
  * NeuCodec ONNX: ~45 MB.
  * Total resident memory per session pool: **< 400 MB RAM**.
* **Real-Time Factor (RTF)**:
  * On modern x86/ARM CPUs (Ryzen 9, Intel Core Ultra, Apple M-series), NeuTTS GGUF generates **100+ acoustic tokens/sec**.
  * Since 1 token = 20ms audio (50 tokens = 1 second of speech), 100 tokens/sec translates to **RTF ~ 0.5 (2x faster than real time)**.
* **Time-To-First-Audio (TTFA)**:
  * Initial TTFA on CPU is **80–120 ms** (decoding begins after the first 5–10 acoustic tokens are emitted).
* **Licensing**:
  * **Apache 2.0**: Unrestricted open-source and commercial use.

---

## 3. Plugin Architecture: `voice-pipes-tts-neutts`

### 3.1 Crate Layout
```
plugins/tts/neutts/
├── Cargo.toml
├── README.md
└── src/
    ├── lib.rs              # Plugin entry point, TtsProvider, TtsFactory, register()
    ├── engine.rs           # Multi-threaded inference session and SessionPool wrapper
    ├── codec.rs            # NeuCodec ONNX decoder using voice-pipes-onnx
    ├── resample.rs         # 24 kHz -> 16 kHz polyphase downsampler
    ├── schema.rs           # TOML configuration deserialization and validation
    ├── voices.rs           # Voice reference embeddings & voice cloning profiles
    ├── wizard.rs           # Interactive setup wizard descriptors for voice-pipes-cli
    └── tests/
        └── neutts.rs       # Conformance tests & metrics verification
```

### 3.2 Configuration Specification (`pipeline.toml`)
```toml
[tts]
provider = "neutts"
# Path to NeuTTS-Air GGUF backbone file
model = "models/neutts-air-q4_k_m.gguf"
# Path to NeuCodec ONNX vocoder decoder
vocoder = "models/neucodec_decoder.onnx"
# Quantization: "q4_k_m" (default), "q8_0", or "fp16"
quantization = "q4_k_m"
# Number of concurrent synthesis engines in the session pool
pool_size = 2
# CPU threads allocated to ONNX / GGUF operations
threads = 4
# Voice profile: built-in preset or path to custom 3-second reference audio (.wav + .txt)
voice = "default"
# Optional reference audio for instant voice cloning:
# reference_audio = "models/neutts/voices/custom_agent.wav"
# reference_text = "models/neutts/voices/custom_agent.txt"

[tts.chunking]
mode = "punctuation"
max_chunk_chars = 120
```

### 3.3 Core Implementation Details

#### 1. Cancellation & Instant Barge-in
Voice Pipes guarantees immediate audio cutoff when the user starts speaking:
* The token generation loop checks `tx.is_closed()` before generating each new acoustic token.
* When barge-in is triggered (Silero VAD speech onset), the audio receiver is dropped, immediately aborting the GGUF generation loop in under 50ms and freeing the engine slot.

#### 2. Phonemization & Subprocess Safety
* Uses `voice_pipes_sdk::phonemize` (which enforces `VP_PHONEMIZE_JOBS` concurrency limits, input argument whitelisting regex, and 10s child execution timeouts to prevent process leaks).

#### 3. Resampling Seam
* NeuCodec outputs audio at 24,000 Hz.
* Voice Pipes standardizes on 16,000 Hz mono PCM (`TARGET_SAMPLE_RATE = 16_000`).
* Resampling is performed via an in-memory polyphase sinc filter (3:2 rational decimation) with minimal latency overhead (< 2ms for a 100ms audio chunk).
