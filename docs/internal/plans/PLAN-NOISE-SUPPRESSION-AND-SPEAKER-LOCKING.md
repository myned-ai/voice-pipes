# Pure-Rust Noise Suppression (DeepFilterNet3) and Primary Speaker Locking (Target Speaker Extraction)

**Status:** Planned  
**Target Branch:** `feat/noise-suppression-speaker-locking`  
**Last updated:** 2026-11-10  
**Owner:** Core Engine + Audio Ingress Pipeline  

---

## Quick-Start Prompt for Implementation Session

Copy and paste the prompt below into a new coding session to execute this implementation:

```text
Implement `docs/internal/plans/PLAN-NOISE-SUPPRESSION-AND-SPEAKER-LOCKING.md` in full (Phases A through F, plus all verification tests and doc updates) on branch `feat/noise-suppression-speaker-locking`. Follow every invariant in `AGENTS.md`: `#![forbid(unsafe_code)]` across all crates, zero warnings under `cargo clippy --workspace -- -D warnings`, no `unwrap_used` / `expect_used` / `indexing_slicing` in non-test Rust code, depend ONLY on `voice-pipes-sdk` in plugins, adhere strictly to Rule 7 Metric Integrity (one definition in `docs/reference/metrics-glossary.md`, conformance fakes, missing as null), and pass `cargo test -p voice-pipes-cli --test workspace_rules`.
```

---

## 1. Problem Statement & Motivation

### The Industry Status Quo
In real-time conversational voice agents (LiveKit Agents, Daily/Pipecat, Vapi, Retell):
1. **Proprietary Commercial Licensing**: High-quality noise cancellation and background voice isolation rely on **Krisp**. Krisp requires a closed-source C++ dynamic library, private enterprise B2B licensing, closed SDK distribution, and per-minute or per-seat billing.
2. **Cloud or Browser WebRTC APM Limitation**: Fallback WebRTC APM (Acoustic Processing Module) runs either in-browser or via cloud WebRTC gateways. Traditional spectral subtraction in WebRTC fails on non-stationary noise (door slams, dogs, typing, street sounds) and has **zero ability to isolate primary speaker speech from background talkers**.
3. **The Voice Agent Failure Mode**:
   - Ambient non-speech noise (typing, air conditioners, fans) causes VAD to trigger false positives, interrupting the agent's turn.
   - Background human speech (coworkers in an open office, family in a living room, TV audio) is transcribed verbatim by Speech-to-Text (STT), polluting the LLM prompt context and leading to bizarre, hallucinated conversational turns.

### The Voice Pipes Opportunity
Nobody in the open-source voice runtime ecosystem ships an out-of-the-box, **free, offline, 100% pure-Rust** noise suppression filter (**DeepFilterNet3**) and **Primary Speaker Locking (Target Speaker Extraction / TSE)** built into the standalone binary.

By building this natively:
- **Zero Closed-Source Dependencies**: Dual-licensed MIT/Apache-2.0.
- **Pure Rust Memory Safety**: Uses `tract` (pure-Rust ONNX execution) for DeepFilterNet and `voice-pipes-onnx` for causal TSE, preserving `#![forbid(unsafe_code)]` across the entire binary.
- **Sub-500ms Glass-to-Glass Preservation**: Ultra-low algorithmic latency ($\le 10\text{ ms}$ for DeepFilterNet3 low-latency mode, $\approx 24\text{ ms}$ for causal TSE) with CPU compute overhead $< 5\text{ ms}$ per 20 ms audio chunk.
- **Pre-VAD Cleaning**: Audio is cleaned *before* reaching Voice Activity Detection (VAD) and STT, eliminating false barge-ins and secondary-speaker hallucinations at the source.

---

## 2. Architecture & Data Contracts

```text
Incoming 16 kHz Mono PCM (WebSocket / SIP Transport)
                         │
                         ▼
┌────────────────────────────────────────────────────────────────────────┐
│                   Audio Ingress Filter Pipeline                        │
│                (voice-pipes/src/pipeline/ingress.rs)                   │
│                                                                        │
│   ┌────────────────────────────────┐  ┌────────────────────────────┐   │
│   │     Stage 1: Denoise Filter    │  │  Stage 2: Target Speaker   │   │
│   │        (DeepFilterNet3)        │  │       Locking (TSE)        │   │
│   │                                │  │   (VoxLock / REAL-TSE)     │   │
│   │  • ERB-scale deep filtering    │  │  • 192-dim Cam++ embedding │   │
│   │  • Removes non-stationary      │  │  • Strips TV, cafeteria,   │   │
│   │    ambient noise & reverb      │  │    & bystander voices      │   │
│   └───────────────┬────────────────┘  └─────────────▲──────────────┘   │
│                   │                                 │                  │
│                   └─────────────────────────────────┤ (Clean audio)    │
│                                                     │                  │
└──────────────────────────────────────┬──────────────┴──────────────────┘
                                       │ (Filtered 16 kHz Mono PCM)
                                       ▼
┌────────────────────────────────────────────────────────────────────────┐
│                   Downstream Pipeline (Untouched)                      │
│                                                                        │
│     [Silero VAD]  ──►  [Smart Turn / VAP]  ──►  [Streaming STT]        │
└────────────────────────────────────────────────────────────────────────┘
```

### 2.1 Trait Definitions in `voice-pipes-sdk`

Add `voice-pipes-sdk/src/filter.rs`:

```rust
//! Ingress audio filter extension point.
//!
//! Audio filters process microphone PCM audio chunks *before* Voice Activity
//! Detection (VAD) and Speech-to-Text (STT). A filter may remove ambient
//! noise, cancel acoustic echo, or isolate a primary speaker's voice.

use std::fmt;
use std::sync::Arc;
use crate::component::Component;
use crate::config::ConfigError;

/// Runtime context passed to each chunk filtering invocation.
#[derive(Debug, Clone)]
pub struct FilterContext<'a> {
    pub session_id: &'a str,
    pub sample_rate: u32,
    /// Pre-enrolled target speaker embedding (if supplied via session metadata).
    pub enrolled_speaker_embedding: Option<&'a [f32]>,
}

/// Telemetry metrics returned by a filter execution on one audio chunk.
#[derive(Debug, Clone, Default)]
pub struct FilterMetrics {
    /// Processing duration inside the filter inference kernel.
    pub compute_duration: std::time::Duration,
    /// Estimated signal-to-noise ratio improvement (in dB), if calculated.
    pub snr_improvement_db: Option<f32>,
    /// Target speaker similarity score [0.0, 1.0] (for speaker locking filters).
    pub speaker_similarity: Option<f32>,
    /// Whether the filter is actively suppressing the chunk.
    pub is_suppressed: bool,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum FilterError {
    ModelLoadFailed(String),
    InferenceFailed(String),
    UnsupportedSampleRate { expected: u32, actual: u32 },
    BufferOverflow(String),
    ConfigError(String),
}

impl fmt::Display for FilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FilterError::ModelLoadFailed(msg) => write!(f, "audio filter model load failed: {msg}"),
            FilterError::InferenceFailed(msg) => write!(f, "audio filter inference failed: {msg}"),
            FilterError::UnsupportedSampleRate { expected, actual } => {
                write!(f, "audio filter expects {expected} Hz, received {actual} Hz")
            }
            FilterError::BufferOverflow(msg) => write!(f, "audio filter buffer overflow: {msg}"),
            FilterError::ConfigError(msg) => write!(f, "audio filter config error: {msg}"),
        }
    }
}

impl std::error::Error for FilterError {}

/// An audio filter operating sequentially on incoming client PCM audio chunks.
pub trait AudioFilter: Send {
    /// In-place process a chunk of 16-bit mono PCM samples at `sample_rate`.
    fn process_chunk(
        &mut self,
        samples: &mut [i16],
        ctx: &FilterContext<'_>,
    ) -> Result<FilterMetrics, FilterError>;

    /// Reset recurrent states (between callers, on reconnect, or after errors).
    fn reset(&mut self);

    /// Provider identifier for tracing and metrics.
    fn name(&self) -> &str;

    /// Health component reporting.
    fn as_component(&self) -> Option<Arc<dyn Component>> {
        None
    }
}

/// Factory trait for creating audio filters.
pub trait AudioFilterFactory: Send + Sync {
    fn name(&self) -> &str;
    fn aliases(&self) -> &[&str] { &[] }
    fn matches(&self, n: &str) -> bool {
        self.name() == n || self.aliases().contains(&n)
    }
    fn validate(&self, cfg: &crate::config::FilterStageConfig) -> Vec<ConfigError>;
    fn build(
        &self,
        cfg: &crate::config::FilterStageConfig,
        ctx: &crate::BuildContext<'_>,
    ) -> Result<Box<dyn AudioFilter>, crate::BuildError>;
}
```

---

## 3. DeepFilterNet3 Implementation Spec (`voice-pipes-filter-deepfilter`)

### 3.1 Model & Runtime Strategy
- **Engine**: Pure-Rust execution via `deep_filter` / `libdf` (backed by `tract-onnx` and `tract-core`).
- **Safety**: Fully safe Rust without C/C++ runtime or CUDA requirements; compiles under `#![forbid(unsafe_code)]`.
- **Model Files**: `DeepFilterNet3_ll_onnx.tar.gz` (Low-Latency variant: 10 ms lookahead, 20 ms frame window, ~2.5M params).
- **Embedded Asset**: Option to bundle model archive into the binary using `include_bytes!` or load from disk via `model_path = "/models/deepfilternet3_ll.tar.gz"`.
- **Resampling**: DeepFilterNet operates natively at 48 kHz. Ingress audio at 16 kHz is upsampled via `voice_pipes_sdk::audio::resample` to 48 kHz, filtered through DeepFilterNet, and downsampled back to 16 kHz for VAD/STT.

### 3.2 Attenuation & Post-Filter Tuning
- `atten_lim_db`: Attenuation limit in decibels ($20.0\text{--}100.0\text{ dB}$, default $30.0\text{ dB}$). Setting a limit prevents synthetic sounding vocal distortion during heavy background noise.
- `post_filter_beta`: Optional post-filter gain factor ($0.0\text{--}0.05$, default $0.02$) for crisp voice transients.

---

## 4. Primary Speaker Locking (TSE) Spec (`voice-pipes-filter-speakerlock`)

### 4.1 Target Speaker Extraction Mechanics
- **Model Architecture**:
  1. **Speaker Encoder (Voiceprint Extractor)**:
     - Model: Cam++ or WeSpeaker ResNet34-lite (ONNX format, ~15MB).
     - Input: 1.5s–3.0s clean 16 kHz speech.
     - Output: 192-dimensional unit L2-normalized embedding vector $\mathbf{e}_{\text{target}} \in \mathbb{R}^{192}$.
  2. **Causal Target Speaker Extractor (TSE)**:
     - Model: Fine-tuned causal REAL-TSE / VoxLock ONNX model (~27M parameters, ~24ms algorithmic latency).
     - Inputs: Current 20 ms audio frame $\mathbf{x}_t$, target embedding $\mathbf{e}_{\text{target}}$, recurrent hidden state $\mathbf{h}_{t-1}$.
     - Outputs: Cleaned audio frame $\hat{\mathbf{x}}_t$, next hidden state $\mathbf{h}_t$.

### 4.2 Auto-Lock & Dynamic Enrollment Protocol
1. **Unenrolled State (`SpeakerLockState::Searching`)**:
   - Audio passes through with noise suppression only.
   - Incoming audio chunks are accumulated in an enrollment ring buffer.
   - When VAD reports high-confidence speech ($> 0.85$) for $\ge 1.2\text{ seconds}$ without overlapping energy peaks, the encoder computes $\mathbf{e}_{\text{target}}$.
   - State transitions to `SpeakerLockState::Locked`.
2. **Locked State (`SpeakerLockState::Locked`)**:
   - The causal TSE model extracts only the locked speaker, attenuating secondary talkers by $18\text{--}30\text{ dB}$.
   - Every active turn evaluates cosine similarity $S_C(\mathbf{e}_{\text{current}}, \mathbf{e}_{\text{target}})$.
   - **Anti-Drift Protection**: If $S_C \ge 0.82$, adapt the embedding via exponential moving average:
     $$\mathbf{e}_{\text{target}} \leftarrow \text{normalize}(\alpha \mathbf{e}_{\text{target}} + (1 - \alpha) \mathbf{e}_{\text{current}}), \quad \alpha = 0.95$$
   - If $S_C < 0.60$ (secondary background speaker talking), adaptation is frozen, and TSE suppresses the audio.

---

## 5. Configuration Contract (`voice-pipes.toml`)

```toml
[audio.filter]
# Enabled filter plugins in declaration order
enabled = ["deepfilter", "speakerlock"]
# Filter mode: "pre_vad" (cleans for both VAD & STT) or "stt_only"
mode = "pre_vad"

[audio.filter.deepfilter]
model = "builtin:deepfilternet3-ll" # or path to .tar.gz
attenuation_limit_db = 30.0
post_filter = 0.02

[audio.filter.speakerlock]
model = "builtin:voxlock-causal"
encoder = "builtin:campp-192"
enrollment_strategy = "auto"         # "auto", "pre_enrolled", or "hybrid"
min_enrollment_speech_ms = 1200
drift_threshold = 0.82
suppression_db = 24.0
```

---

## 6. Implementation Phases

```
┌────────────────────────────────────────────────────────────────────────┐
│  Phase A: voice-pipes-sdk Extension Point                              │
│  - Add AudioFilter, AudioFilterFactory, FilterContext, FilterMetrics   │
│  - Add filter stage configuration types in config.rs                   │
│  - Registry voice_pipes_sdk::filter::register_filter_factory()         │
└──────────────────────────────────┬─────────────────────────────────────┘
                                   │
                                   ▼
┌────────────────────────────────────────────────────────────────────────┐
│  Phase B: Core Engine Ingress Hook (voice-pipes)                       │
│  - Implement FilterChain in voice-pipes/src/pipeline/filter.rs         │
│  - Wire FilterChain into voice-pipes/src/pipeline/vad.rs before VAD    │
│  - Add telemetry spans & timing metrics                                │
└──────────────────────────────────┬─────────────────────────────────────┘
                                   │
                                   ▼
┌────────────────────────────────────────────────────────────────────────┐
│  Phase C: DeepFilterNet3 Plugin (voice-pipes-filter-deepfilter)        │
│  - New crate plugins/filter/deepfilter with #![forbid(unsafe_code)]    │
│  - Pure-Rust tract inference runner with 16k <-> 48k resampling        │
│  - Embedded DeepFilterNet3-LL model artifact option                    │
└──────────────────────────────────┬─────────────────────────────────────┘
                                   │
                                   ▼
┌────────────────────────────────────────────────────────────────────────┐
│  Phase D: Speaker Lock Plugin (voice-pipes-filter-speakerlock)         │
│  - New crate plugins/filter/speakerlock with #![forbid(unsafe_code)]   │
│  - Cam++ 192-dim voiceprint extractor via voice-pipes-onnx             │
│  - Causal REAL-TSE/VoxLock streaming extractor with state cache        │
│  - Dynamic auto-enrollment state machine & anti-drift logic            │
└──────────────────────────────────┬─────────────────────────────────────┘
                                   │
                                   ▼
┌────────────────────────────────────────────────────────────────────────┐
│  Phase E: Admin Console Integration (voice-pipes-admin)                │
│  - Backend: Stream audio filter telemetry over fleet SSE               │
│  - Frontend: Show "Noise Suppressed" & "Speaker Locked" badges in      │
│    Session Inspector & Turn Cards                                      │
└──────────────────────────────────┬─────────────────────────────────────┘
                                   │
                                   ▼
┌────────────────────────────────────────────────────────────────────────┐
│  Phase F: Documentation & Reference Updates                            │
│  - Update metrics-glossary.md, config.md, plugin-sdk.md, providers.md │
│  - Update README.md and book documentation                             │
└────────────────────────────────────────────────────────────────────────┘
```

---

## 7. Test Suite & Verification Plan

Strict adherence to Voice Pipes testing standards:

### 7.1 Unit Tests
- **`filter_resample_roundtrip`**: Test 16 kHz $\to$ 48 kHz $\to$ 16 kHz identity preservation with minimal SNR distortion.
- **`deepfilter_chunking_alignment`**: Verify 20 ms (320 samples @ 16 kHz) chunks correctly pad and unpad across DeepFilterNet STFT hop boundaries without clicks or discontinuities.
- **`speaker_embedding_cosine_similarity`**: Test unit vector normalization and cosine distance calculations on known synthetic test vectors.
- **`anti_drift_logic`**: Verify that embeddings with similarity $< 0.82$ are rejected from updating the target model.
- **`filter_state_reset`**: Verify that calling `.reset()` clears all internal recurrent buffers and brings the filter back to initial state.

### 7.2 Integration Tests
- **`synthetic_noise_attenuation`**:
  Feed 16 kHz speech mixed with stationary white noise and non-stationary keyboard clicks (SNR = 0 dB). Verify output SNR is improved by $\ge 15\text{ dB}$ and speech segments remain intelligible.
- **`multi_speaker_crosstalk_isolation`**:
  Feed 2 overlapping speakers (Speaker A primary @ 0 dB, Speaker B secondary @ -3 dB). Verify Speaker B is attenuated by $\ge 18\text{ dB}$ after lock acquisition.
- **`auto_lock_acquisition_timing`**:
  Verify lock transitions from `Searching` to `Locked` within $1.2\text{--}1.5\text{ s}$ of valid speech.

### 7.3 Conformance Tests (Rule 7 Metric Integrity)
- **Deliberately Slow Fake Filter**:
  A test fake that delays processing by exactly $15\text{ ms}$ must prove that `audio_filter_compute_duration_ms` measures exactly $\ge 15\text{ ms}$ without charging time to VAD or STT.
- **Null on Bypass**:
  When filters are disabled, filter metrics in session reports must be `null`, never `0.0`.
- **Measurement Fault Flagging**:
  Impossible values (e.g., negative duration, NaN similarity) flag a measurement fault rather than silently passing.

### 7.4 Benchmarking & Performance Tests (`voice-pipes-bench`)
- **Latency Budget**: Total filter compute duration must stay $\le 5.0\text{ ms}$ per 20 ms chunk on x86_64 and ARM64 CPUs.
- **Real-Time Factor (RTF)**: RTF must stay $< 0.15$ on a single CPU thread.
- **WER Improvement**: Run `voice-pipes-bench` against noisy LibriSpeech (cafeteria noise @ 5 dB SNR). Verify STT Word Error Rate (WER) decreases by $\ge 30\%$ relative to unfiltered input.

### 7.5 Workspace Rules & Security Checks
- `#![forbid(unsafe_code)]` check across all new crates.
- `cargo clippy --workspace -- -D warnings` with zero warnings.
- `clippy::indexing_slicing`, `clippy::unwrap_used`, `clippy::expect_used` denials in non-test code.
- `cargo deny check` clean supply-chain audit.
- `cargo test -p voice-pipes-cli --test workspace_rules` passes (51 rules).

---

## 8. Documentation Update Plan

| File | Target Updates |
| :--- | :--- |
| `docs/reference/config.md` | Document `[audio.filter]`, `[audio.filter.deepfilter]`, and `[audio.filter.speakerlock]` TOML configurations, options, and defaults. |
| `docs/reference/metrics-glossary.md` | Add official metric entries for `audio_filter_compute_duration_ms`, `audio_filter_snr_gain_db`, `speaker_lock_confidence`, and `speaker_lock_state`. |
| `docs/reference/providers.md` | Add DeepFilterNet3 and VoxLock / REAL-TSE to the Audio Preprocessing / Filter catalog. |
| `docs/extending/plugin-sdk.md` | Add complete guide and example code for authoring custom `AudioFilter` and `AudioFilterFactory` plugins. |
| `docs/reference/vpo-metrics.md` | Add OpenTelemetry semantic conventions for audio filter spans (`voice_pipes.audio.filter.*`). |
| `docs/getting-started/quickstart.md` | Add a section demonstrating zero-configuration noise suppression in local CLI and WebSocket sessions. |
| `README.md` & `llms.txt` | Highlight native pure-Rust offline noise suppression and primary speaker locking as a core competitive differentiator. |

---

## 9. Acceptance Criteria Checklist

- [ ] `AudioFilter` and `AudioFilterFactory` traits defined in `voice-pipes-sdk`.
- [ ] Ingress audio filter hook integrated into `voice-pipes/src/pipeline/` before VAD and STT.
- [ ] `voice-pipes-filter-deepfilter` crate compiles with `#![forbid(unsafe_code)]` using pure-Rust `tract`.
- [ ] `voice-pipes-filter-speakerlock` crate delivers dynamic auto-enrollment and causal target speaker extraction.
- [ ] Per-chunk compute overhead is verified $< 5\text{ ms}$ on standard CPU.
- [ ] Rule 7 conformance test with deliberately slow fake proves metric clock accuracy.
- [ ] All 51 `workspace_rules` pass with zero warnings under `cargo clippy --workspace -- -D warnings`.
- [ ] All planned documentation files updated and verified.
