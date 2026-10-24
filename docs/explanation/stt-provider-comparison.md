# Real-Time STT Provider Comparison for Voice Agent Pipelines

**Status:** active
**Last verified:** 2026-09-06 (moved into the public tree; content unchanged)
**Owner:** Docs

> **Research date:** July 2025  
> **Methodology:** All data sourced from official public documentation and pricing pages.  
> **Scope:** 8 STT providers evaluated for real-time voice agent use cases.

---

## Table of Contents

1. [Provider Comparison Matrix](#1-provider-comparison-matrix)
2. [Detailed Provider Profiles](#2-detailed-provider-profiles)
3. [Silero VAD ONNX Model Technical Details](#3-silero-vad-onnx-model-technical-details)
4. [Semantic Endpointing Techniques](#4-semantic-endpointing-techniques)
5. [Recommendations for Voice Agent Pipelines](#5-recommendations-for-voice-agent-pipelines)

---

## 1. Provider Comparison Matrix

### 1.1 Connection & Protocol

| Provider | Protocol | Streaming Support | Connection Type |
|---|---|---|---|
| **OpenAI Whisper API** | REST (HTTP) | SSE via `stream=True` (gpt-4o models only; not whisper-1) | File upload / SSE stream |
| **OpenAI Realtime API** | WebSocket / WebRTC / SIP | Yes — native real-time streaming | `wss://api.openai.com/v1/realtime?intent=transcription` |
| **Google Gemini 2.0 Live API** | WebSocket (WSS) | Yes — native real-time streaming | WSS bidirectional |
| **Deepgram Nova-3** | WebSocket | Yes — native real-time streaming | `wss://api.deepgram.com/v1/listen` |
| **AssemblyAI** | WebSocket | Yes — native real-time streaming | `wss://streaming.assemblyai.com/v3/ws` |
| **Speechmatics** | WebSocket | Yes — native real-time streaming | WSS with `StartRecognition` message |
| **Azure Speech Services** | SDK / REST / WebSocket | Yes — via Speech SDK real-time | Speech SDK (gRPC under the hood) |
| **Google Cloud STT v2** | gRPC | Yes — via `StreamingRecognize` RPC | gRPC bidirectional streaming |

### 1.2 Input Audio Format & Encoding

| Provider | Preferred Format | Supported Encodings | Sample Rates |
|---|---|---|---|
| **OpenAI Whisper API** | File upload | mp3, mp4, mpeg, mpga, m4a, wav, webm, flac, ogg | Auto-detected from file; 25MB limit |
| **OpenAI Realtime API** | Raw PCM | `pcm16` (16-bit signed LE) | Not explicitly documented; assumes 24kHz based on output |
| **Gemini 2.0 Live API** | Raw PCM | 16-bit PCM, little-endian | Input: 16kHz; Output: 24kHz |
| **Deepgram Nova-3** | Raw PCM or containerized | linear16, linear32, flac, alaw, mulaw, amr-nb, amr-wb, opus, ogg-opus, speex, g729 | Configurable via `sample_rate` param (commonly 8kHz, 16kHz) |
| **AssemblyAI** | Raw PCM | PCM16 (16-bit signed LE) | 16000 Hz (required) |
| **Speechmatics** | Raw or File | Raw: `pcm_f32le`, `pcm_s16le`, `mulaw`; File: auto-detected | Configurable via `sample_rate` field (raw) |
| **Azure Speech Services** | WAV (PCM) default | WAV (LINEAR16/MULAW default); compressed via GStreamer: MP3, OPUS/OGG, FLAC, ALAW, MULAW, MP4 | Default: 16kHz or 8kHz, 16-bit mono PCM |
| **Google Cloud STT v2** | LINEAR16 or FLAC preferred | LINEAR16, FLAC, MULAW, AMR, AMR_WB, OGG_OPUS, SPEEX_WITH_HEADER_BYTE, WEBM_OPUS, MP3 (beta) | Model-dependent; 16kHz recommended for speech |

### 1.3 Endpointing & VAD

| Provider | Built-in VAD | Endpointing | Configurable Silence Duration | Key Signals |
|---|---|---|---|---|
| **OpenAI Whisper API** | No | No (file-based) | N/A | N/A |
| **OpenAI Realtime API** | Yes — `server_vad` | Yes | `silence_duration_ms`, `threshold`, `prefix_padding_ms` | Server VAD events; configurable |
| **Gemini 2.0 Live API** | Yes | Yes — supports barge-in | Not documented in detail | Barge-in support; voice activity events |
| **Deepgram Nova-3** | Yes — internal VAD | Yes — `endpointing` param | Default 10ms; configurable via `endpointing=<ms>` | `speech_final=true`, `is_final`, `speech_started`, `utterance_end` |
| **AssemblyAI** | Yes | Yes — turn-based | Not documented as configurable ms | `end_of_turn` flag in v3 WebSocket API |
| **Speechmatics** | Yes | Yes — via `max_delay` | `max_delay` controls latency/accuracy tradeoff | Partials (`enable_partials`), finals |
| **Azure Speech Services** | Yes | Yes | Configurable via SDK | Intermediate results, final results |
| **Google Cloud STT v2** | Yes | Yes — single utterance mode | Voice activity events and timeouts configurable | `END_OF_SINGLE_UTTERANCE` event; interim results |

### 1.4 Interim/Partial Results

| Provider | Interim Results | Word-Level Timestamps | Confidence Scores |
|---|---|---|---|
| **OpenAI Whisper API** | Only via `stream=True` (SSE, gpt-4o models) | Yes (whisper-1 only, `verbose_json` format) | Yes (per-segment) |
| **OpenAI Realtime API** | Yes | Not documented for transcription sessions | Not documented |
| **Gemini 2.0 Live API** | Yes — audio transcriptions of both input/output | Not documented | Not documented |
| **Deepgram Nova-3** | Yes — `interim_results=true` | Yes — `start`, `end`, `confidence` per word | Yes — per word |
| **AssemblyAI** | Yes — partial transcripts during turn | Yes (word-level in final transcripts) | Yes — per word |
| **Speechmatics** | Yes — `enable_partials` | Yes — word-level timing | Yes — per word |
| **Azure Speech Services** | Yes — intermediate results via SDK events | Yes | Yes |
| **Google Cloud STT v2** | Yes — interim results in streaming | Yes — `word_confidence` feature | Yes — per word |

### 1.5 Speaker Diarization

| Provider | Diarization Support | Max Speakers | Notes |
|---|---|---|---|
| **OpenAI Whisper API** | Yes — `gpt-4o-transcribe-diarize` model only | Not documented | Specific model required |
| **OpenAI Realtime API** | Not documented | - | - |
| **Gemini 2.0 Live API** | Not documented for STT | - | - |
| **Deepgram Nova-3** | Yes — `diarize=true` | Not limited (documented) | Add-on: $0.0020/min PAYG |
| **AssemblyAI** | Yes — add-on | Not documented | Pre-recorded: $0.02/hr; Streaming: $0.12/hr |
| **Speechmatics** | Yes | Not documented | Available on all tiers |
| **Azure Speech Services** | Yes | Up to 35 speakers | Enhanced add-on: $0.30/hr real-time |
| **Google Cloud STT v2** | Yes — `multiple-voices` feature | Not documented | Available in v2 API |

### 1.6 Language Support

| Provider | Languages | Language Detection |
|---|---|---|
| **OpenAI Whisper API** | 57+ languages | Yes — automatic |
| **OpenAI Realtime API** | Not documented separately | Not documented |
| **Gemini 2.0 Live API** | 70+ languages | Yes — multilingual support |
| **Deepgram Nova-3** | 45+ languages | Yes — `detect_language=true` |
| **AssemblyAI** | Universal-2: 99 languages; Universal-3 Pro Streaming: 6 (en, es, de, fr, pt, it) | Yes (pre-recorded) |
| **Speechmatics** | 55+ languages | Yes — automatic language detection |
| **Azure Speech Services** | 100+ languages/locales | Yes — continuous language identification |
| **Google Cloud STT v2** | 125+ languages (Chirp models) | Yes — `multiple-languages` feature |

### 1.7 Pricing (Streaming/Real-Time STT)

| Provider | Streaming Price | Unit | Free Tier |
|---|---|---|---|
| **OpenAI Whisper API** | gpt-4o-transcribe: $0.006/min ($0.36/hr); gpt-4o-mini-transcribe: $0.003/min ($0.18/hr) | Per minute (file) / per token (streaming) | None documented |
| **OpenAI Realtime API** | gpt-realtime-1.5: Audio $32/1M input tokens, $64/1M output; gpt-realtime-mini: Audio $10/1M input, $20/1M output | Per 1M tokens | None documented |
| **Gemini 2.0 Live API** | Included in Gemini model pricing (token-based) | Per token | Free tier via Google AI Studio |
| **Deepgram Nova-3** | Monolingual: $0.0077/min ($0.462/hr); Multilingual: $0.0092/min ($0.552/hr) | Per minute | $200 free credit |
| **AssemblyAI** | U3 Pro Streaming: $0.45/hr; Universal-Streaming: $0.15/hr; Multilingual: $0.15/hr | Per hour (session duration) | $50 free credit (~333 hrs streaming) |
| **Speechmatics** | Real-time standard: $0.24/hr; Real-time enhanced: $0.40/hr | Per hour | 480 min/month free |
| **Azure Speech Services** | Standard: $1.00/hr; Custom: $1.20/hr | Per hour (billed per second) | 5 audio hours/month free (F0) |
| **Google Cloud STT v2** | Standard: $0.016/min ($0.96/hr) first 500K min; then $0.01/min ($0.60/hr) | Per minute | 60 min/month free (v1) |

### 1.8 Latency Characteristics

| Provider | Claimed Latency | Notes |
|---|---|---|
| **OpenAI Whisper API** | Not specified for streaming | File-based; SSE streaming adds overhead |
| **OpenAI Realtime API** | Low-latency (designed for real-time conversation) | Optimized for voice agents |
| **Gemini 2.0 Live API** | Low-latency (designed for live conversation) | Bidirectional real-time |
| **Deepgram Nova-3** | Sub-300ms typical | Endpointing default 10ms; highly optimized |
| **AssemblyAI** | Sub-300ms claimed for Universal-3 Pro | Turn-based detection |
| **Speechmatics** | Configurable via `max_delay` | Tradeoff: lower delay = less accuracy |
| **Azure Speech Services** | ~200-500ms typical | SDK-dependent |
| **Google Cloud STT v2** | Not specified | gRPC streaming |

### 1.9 Special Features

| Provider | Notable Features |
|---|---|
| **OpenAI Whisper API** | Noise reduction (`near_field`/`far_field`); diarization model; streaming SSE for gpt-4o models |
| **OpenAI Realtime API** | WebRTC & SIP support; server VAD with configurable params; noise reduction; speech-to-speech in same session |
| **Gemini 2.0 Live API** | Barge-in; proactive audio; affective dialog; transcription of both input and output audio; multimodal (text+audio+video) |
| **Deepgram Nova-3** | UtteranceEnd event; speech_started event; keyterm prompting; smart formatting; redaction; multichannel |
| **AssemblyAI** | Turn-based `end_of_turn` detection; keyterm prompting; medical mode; PII redaction; LLM Gateway |
| **Speechmatics** | Custom dictionary; max_delay tuning; enhanced vs standard models; on-device deployment; translation (69 pairs) |
| **Azure Speech Services** | Custom speech models; phrase lists; pronunciation assessment; speech translation; connected/disconnected containers |
| **Google Cloud STT v2** | Chirp 2/3 models; model adaptation; spoken punctuation/emoji; dynamic batch; on-premises option |

### 1.10 Compliance & Security

| Provider | Certifications |
|---|---|
| **OpenAI Whisper API** | SOC 2; data residency options (10% uplift) |
| **OpenAI Realtime API** | Same as OpenAI platform |
| **Gemini 2.0 Live API** | Google Cloud compliance (SOC 1/2/3, ISO 27001, HIPAA BAA) |
| **Deepgram Nova-3** | SOC 2 Type 1 & 2; HIPAA (BAA available); GDPR; CCPA; PCI; EU data residency (`api.eu.deepgram.com`) |
| **AssemblyAI** | SOC 2 Type 2; HIPAA; GDPR; PCI DSS; ISO 27001; EU data residency |
| **Speechmatics** | ISO 27001; SOC 2; GDPR; HIPAA; multi-region (US, EU, Australia) |
| **Azure Speech Services** | SOC 1/2/3; ISO 27001; HIPAA BAA; FedRAMP; GDPR; global Azure regions |
| **Google Cloud STT v2** | SOC 1/2/3; ISO 27001; HIPAA BAA; FedRAMP; GDPR; regional endpoints |

---

## 2. Detailed Provider Profiles

### 2.1 OpenAI Whisper API

- **Models:** `whisper-1`, `gpt-4o-transcribe`, `gpt-4o-mini-transcribe`, `gpt-4o-transcribe-diarize`
- **Streaming:** `stream=True` returns SSE events (not available for `whisper-1`). For true real-time mic input, use the Realtime API instead.
- **Word timestamps:** Available for `whisper-1` only via `timestamp_granularities=["word"]` with `response_format=verbose_json`.
- **File size limit:** 25MB per request.
- **Best for:** Batch/file transcription, not ideal for real-time voice agents.

### 2.2 OpenAI Realtime API

- **Connection:** WebSocket (`wss://api.openai.com/v1/realtime?intent=transcription`), WebRTC, or SIP.
- **Input format:** `input_audio_format: "pcm16"`.
- **VAD:** Server-side VAD with configurable `threshold`, `prefix_padding_ms`, `silence_duration_ms`.
- **Noise reduction:** `near_field` (close-talking mic) or `far_field` modes.
- **Models for transcription sessions:** `whisper-1`, `gpt-4o-transcribe`, `gpt-4o-mini-transcribe`.
- **Pricing:** Token-based; `gpt-realtime-mini` is significantly cheaper ($10/1M input audio tokens vs $32/1M for `gpt-realtime-1.5`).
- **Best for:** Full voice agent pipelines where OpenAI handles both STT and LLM.

### 2.3 Google Gemini 2.0 Live API

- **Protocol:** WebSocket (WSS) bidirectional.
- **Input:** Raw 16-bit PCM, 16kHz, little-endian.
- **Output audio:** Raw 16-bit PCM, 24kHz.
- **Features:** Barge-in support, audio transcriptions of both input/output, affective dialog, proactive audio, 70+ languages.
- **Best for:** Multimodal agents using Gemini as both STT and LLM in a unified session.

### 2.4 Deepgram Nova-3

- **Protocol:** WebSocket streaming.
- **Encoding:** Extremely flexible — supports linear16, linear32, flac, alaw, mulaw, amr-nb, amr-wb, opus, ogg-opus, speex, g729.
- **Endpointing:** Default 10ms (very aggressive), configurable via `endpointing=<ms>`. Uses internal VAD. Returns `speech_final=true` when endpoint detected.
- **Key events:** `speech_started`, `utterance_end` (configurable via `utterance_end_ms`), `is_final`, `speech_final`.
- **Interim results:** Enabled via `interim_results=true`.
- **Pricing:** $0.0077/min monolingual PAYG — competitive for high-volume usage.
- **Best for:** Dedicated STT in voice agent pipelines; excellent real-time streaming with fine-grained endpointing control.

### 2.5 AssemblyAI

- **Protocol:** WebSocket at `wss://streaming.assemblyai.com/v3/ws`.
- **Input:** PCM16, 16kHz, `speech_model` parameter required.
- **Models:** `u3-rt-pro` (highest quality, 6 languages), `universal-streaming` (English, fastest), `universal-streaming-multilingual` (6 languages), `whisper-streaming` (99+ languages).
- **Turn detection:** v3 API provides `end_of_turn` flag — next-gen turn detection built into the API.
- **Pricing:** Universal-Streaming at $0.15/hr is very cost-effective; U3 Pro Streaming at $0.45/hr for premium quality.
- **Session billing:** Charged for total session duration (connection open time), not just audio.
- **Concurrency:** Auto-scaling — starts at 100 new streams/min, scales up 10% every 60s at 70%+ utilization.
- **Best for:** Voice agents needing built-in turn detection; medical/specialized use cases with Medical Mode.

### 2.6 Speechmatics

- **Protocol:** WebSocket with `StartRecognition` / `AddAudio` / `EndOfStream` message protocol.
- **Input:** Raw (`pcm_s16le`, `pcm_f32le`, `mulaw`) or file (auto-detected).
- **Accuracy tiers:** Enhanced (best accuracy) and Standard (lower cost/faster turnaround).
- **Endpointing:** `max_delay` controls how long the system waits before emitting results (latency vs accuracy tradeoff).
- **Partials:** `enable_partials: true` for interim results.
- **Pricing:** $0.24/hr standard, $0.40/hr enhanced (Pro tier). 20% volume discount above 500 hrs/month.
- **Free tier:** 480 minutes/month, 2 concurrent real-time sessions.
- **Best for:** Multi-language enterprise deployments; on-device/on-premises requirements.

### 2.7 Azure Speech Services

- **Protocol:** Speech SDK (recommended), REST API, or CLI.
- **Input:** Default WAV 16kHz/8kHz 16-bit mono PCM. Compressed formats via GStreamer (MP3, OPUS/OGG, FLAC, etc.).
- **Diarization:** Up to 35 speakers.
- **Custom speech:** Train custom models for domain-specific vocabulary.
- **Pricing:** $1.00/hr standard real-time; enhanced add-ons (diarization, language ID) at $0.30/hr each. Commitment tiers available (down to $0.50/hr at 50K hr tier).
- **Free tier:** 5 audio hours/month (F0).
- **Best for:** Enterprise Azure-integrated pipelines; custom pronunciation/domain models; high speaker count diarization.

### 2.8 Google Cloud Speech-to-Text v2

- **Protocol:** gRPC bidirectional streaming (`StreamingRecognize`).
- **Input:** LINEAR16, FLAC preferred. Also supports MULAW, AMR, AMR_WB, OGG_OPUS, SPEEX, WEBM_OPUS.
- **Models:** Standard, Chirp 2, Chirp 3 (latest). `auto_decoding_config` can auto-detect encoding.
- **Pricing:** $0.016/min ($0.96/hr) for first 500K min/month; drops to $0.004/min ($0.24/hr) above 2M min. Dynamic batch at $0.003/min.
- **Features:** Model adaptation, spoken punctuation/emoji, single utterance mode, voice activity events and timeouts, word confidence.
- **25KB limit** per message in streaming requests.
- **Best for:** Google Cloud-integrated pipelines; very high-volume workloads (aggressive volume discounts).

---

## 3. Silero VAD ONNX Model Technical Details

**Source:** [snakers4/silero-vad](https://github.com/snakers4/silero-vad) — MIT License

### 3.1 Model Specifications

| Property | Value |
|---|---|
| **Latest version** | v6 |
| **Model formats** | ONNX, PyTorch JIT |
| **ONNX model size** | ~2MB (JIT ~2MB) |
| **License** | MIT |
| **Supported sample rates** | 8000 Hz, 16000 Hz (or multiples of 16000, downsampled automatically) |
| **Training data** | 6000+ languages |
| **Quality (ROC-AUC)** | 0.97 on Multi-Domain Validation (31.25ms segments) |

### 3.2 ONNX Model Input/Output Tensors

From the `OnnxWrapper` class in `utils_vad.py`:

| Tensor | Name | Shape | Type | Notes |
|---|---|---|---|---|
| **Input: audio** | `input` | `[batch, num_samples + context_size]` | float32 | `num_samples`: 512 @ 16kHz, 256 @ 8kHz. `context_size`: 64 @ 16kHz, 32 @ 8kHz. Context is prepended automatically. |
| **Input: state** | `state` | `[2, batch, 128]` | float32 | Hidden state, initialized to zeros. Persisted across calls. |
| **Input: sample rate** | `sr` | scalar | int64 | Must be 8000 or 16000 |
| **Output: probability** | `out` | `[batch, 1]` | float32 | Speech probability (0.0–1.0) |
| **Output: state** | `state` | `[2, batch, 128]` | float32 | Updated hidden state for next call |

### 3.3 Chunk Processing

| Sample Rate | Chunk Size (samples) | Chunk Duration | Context Size (samples) | Total Input Samples |
|---|---|---|---|---|
| **16000 Hz** | 512 | 32ms | 64 | 576 |
| **8000 Hz** | 256 | 32ms | 32 | 288 |

- **Processing time:** < 1ms per chunk on a single CPU thread (for 30ms+ chunks)
- **Minimum chunk:** Must satisfy `sr / chunk_samples <= 31.25` (i.e., chunk must be ≥ 32ms)
- **Validation:** Raises `ValueError` if chunk is too short or sample rate unsupported

### 3.4 Key Parameters (get_speech_timestamps)

| Parameter | Default | Description |
|---|---|---|
| `threshold` | 0.5 | Probability above this = speech |
| `neg_threshold` | `threshold - 0.15` | Probability below this while speaking = silence |
| `min_speech_duration_ms` | 250 | Chunks shorter than this are discarded |
| `max_speech_duration_s` | inf | Maximum speech chunk length before forced split |
| `min_silence_duration_ms` | 100 | Silence required to end a speech segment |
| `speech_pad_ms` | 30 | Padding added to each side of detected speech |

### 3.5 VADIterator (Streaming)

For real-time use, `VADIterator` provides a stateful streaming interface:

```python
vad_iterator = VADIterator(model, threshold=0.5, sampling_rate=16000,
                           min_silence_duration_ms=100, speech_pad_ms=30)

# Feed 512-sample chunks:
result = vad_iterator(audio_chunk)
# Returns: {'start': sample_idx} when speech starts
#          {'end': sample_idx} when speech ends
#          None otherwise
```

### 3.6 Dependencies (ONNX-only deployment)

- `onnxruntime >= 1.16.1`
- Python 3.8+
- ~1GB RAM
- Must implement own audio I/O (torchaudio not needed)

### 3.7 voice-pipes Implementation (`SileroVad`)

Our `SileroVad` in `src/stt/vad.rs` implements the official protocol:

- **Window size:** 512 samples at 16 kHz (32 ms)
- **Context prefix:** 64 samples from the previous window, prepended to each inference call (576 total input)
- **Resampling:** 48 kHz mic audio is downsampled to 16 kHz via a windowed-sinc resampler with anti-aliasing (not point-sampling decimation)
- **State management:** recurrent state `[2, 1, 128]` and context buffer persisted across calls, zeroed on `reset()`
- **Config fields:** `speech_prob` (threshold, default 0.5) and `energy_threshold` (for energy mode, default 200.0) replace the ambiguous `threshold` field
- **Onset confirmation:** 3 consecutive speech frames (60 ms) required before speech is confirmed, rejecting noise spikes
- **No gain:** raw mic samples are used without pre-amplification to preserve formant structure

---

## 4. Semantic Endpointing Techniques

### 4.1 Overview

Traditional endpointing relies purely on **silence duration** — after N milliseconds of silence, the system concludes the user has finished speaking. This approach has fundamental problems for voice agents:

- **Too short:** Cuts off users during natural pauses (thinking, mid-sentence breath)
- **Too long:** Creates unnatural delays before the agent responds

**Semantic endpointing** (also called "smart turn detection") goes beyond silence detection by analyzing **what was said** to determine if the user's turn is complete.

### 4.2 Approaches

#### 4.2.1 VAD + Silence Threshold (Baseline)

The simplest approach used by most STT providers:
- Silero VAD detects speech/non-speech
- After configurable silence duration (e.g., 300–1000ms), turn is marked complete
- **Pros:** Low latency, simple, no additional compute
- **Cons:** Cannot distinguish intentional pauses from turn completion

**Used by:** Deepgram (`endpointing` param), OpenAI Realtime (`silence_duration_ms`), Azure Speech Services

#### 4.2.2 ML-Based Turn Detection (Pipecat Smart Turn)

**Pipecat** (by Daily.co) implements `Smart Turn Detection v3` — an ONNX model that analyzes audio to determine turn completion:

- **Architecture:** ONNX model (~local inference), not LLM-based
- **How it works:**
  1. VAD detects a pause in speech (recommended: 200ms `stop_secs`)
  2. Smart Turn model analyzes up to 8 seconds of the user's audio turn
  3. Classifies turn as **complete** or **incomplete** based on intonation patterns and linguistic signals
  4. Fallback: if classified incomplete but silence exceeds `stop_secs` (default 3.0s), marks complete anyway
- **Performance:** Inference under 100ms on CPU; GPU via `onnxruntime-gpu` for faster
- **Language support:** 23 languages
- **Model:** Available on HuggingFace (`pipecat-ai/smart-turn-v3`)
- **Key params:**
  - `stop_secs` (3.0): Silence timeout fallback
  - `pre_speech_ms` (0.0): Pre-speech audio buffer
  - `max_duration_secs` (8.0): Max segment for analysis (rolling window beyond this)

#### 4.2.3 LLM-Based Turn Detection

Some voice agent frameworks use an LLM to analyze the text transcript and determine if the turn is complete:
- Feed the latest transcript to a small/fast LLM with a prompt like "Is this a complete thought?"
- **Pros:** Understands semantic completeness (e.g., "Can you tell me about..." is clearly incomplete)
- **Cons:** Adds latency (LLM inference); requires the STT transcript to be available first; cost per call

#### 4.2.4 Hybrid: STT Provider End-of-Turn Signals

Some providers are building semantic understanding directly into their STT:
- **AssemblyAI v3:** `end_of_turn` flag — the API itself determines when the user's turn is complete using "next-gen end-of-turn detection" (per their docs)
- **Deepgram:** Combination of `speech_final` (VAD-based) and `utterance_end` (longer timeout for aggregation)
- **OpenAI Realtime:** Server VAD with tunable parameters, but no documented semantic analysis

### 4.3 Recommended Architecture for Voice Agents

```
Microphone → [VAD: Silero] → [STT: Streaming Provider] → [Turn Detector] → LLM
                                                              ↑
                                               Smart Turn ML model (e.g., Pipecat)
                                               OR provider end_of_turn signal
                                               OR LLM-based classification
                                               OR fallback silence timeout
```

**Layered approach:**
1. **Layer 1 — VAD:** Silero VAD for immediate speech/non-speech detection (< 1ms)
2. **Layer 2 — STT partials:** Get interim transcripts flowing immediately
3. **Layer 3 — Provider endpointing:** Use `speech_final` / `end_of_turn` from the STT provider (10–500ms configurable)
4. **Layer 4 — Semantic analysis:** Smart Turn model or LLM classifier for ambiguous cases
5. **Layer 5 — Fallback timeout:** Hard stop after N seconds of silence regardless (e.g., 3s)

---

## 5. Recommendations for Voice Agent Pipelines

### For a Rust-based pipeline (voice-pipes):

| Criterion | Top Pick | Runner-Up | Rationale |
|---|---|---|---|
| **Best real-time streaming** | Deepgram Nova-3 | AssemblyAI | WebSocket, fine-grained endpointing, rich event signals |
| **Best cost (high volume)** | Google Cloud STT v2 | Deepgram | $0.24/hr at 2M+ min/month; Deepgram at $0.462/hr |
| **Best cost (low volume)** | AssemblyAI | Speechmatics | $0.15/hr Universal-Streaming; Speechmatics $0.24/hr with 480 min free |
| **Best accuracy** | AssemblyAI U3 Pro | Speechmatics Enhanced | Purpose-built models; medical mode available |
| **Best for unified STT+LLM** | OpenAI Realtime API | Gemini 2.0 Live | Single connection for both STT and generation |
| **Best endpointing control** | Deepgram | OpenAI Realtime | Configurable ms, multiple event types, utterance_end |
| **Best language coverage** | Google Cloud STT v2 | Azure Speech | 125+ languages; Azure 100+ |
| **Best enterprise/compliance** | Azure Speech Services | Google Cloud | FedRAMP, containers, custom models, 35-speaker diarization |
| **Best for self-hosting** | Speechmatics | — | On-device deployment option; data stays on-premises |
| **Best semantic endpointing** | AssemblyAI (built-in) | Deepgram | AssemblyAI `end_of_turn`; Deepgram `speech_final` + `utterance_end` |

### Key Tradeoffs:

- **Deepgram** offers the most granular control over endpointing and the widest encoding support — ideal for a Rust pipeline where you control the audio format.
- **AssemblyAI** is strong on built-in turn detection (`end_of_turn`) but charges for full session duration (idle time costs money).
- **OpenAI Realtime API** is expensive (token-based audio pricing) but provides the tightest integration if you're already using OpenAI for the LLM.
- **Speechmatics** and **Azure** are the best cloud options if on-premises/self-hosted deployment is required.
- **Google Cloud STT v2** has the best volume pricing but requires gRPC (not WebSocket).
