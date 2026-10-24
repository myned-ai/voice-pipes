# Example Pipelines & Feature Cookbook

**Status:** active  
**Owner:** Docs & Developer Experience  
**Audience:** Developers evaluating Voice Pipes features and configuring pipelines

Voice Pipes ships with dozens of ready-to-run example pipeline configurations under `voice-pipes/examples/`. This cookbook provides a feature-to-pipeline selection matrix and step-by-step recipes to try each feature in seconds.

---

## 1. Feature Selection Matrix

Find the feature you want to explore and use the matching example configuration:

| Feature / Use Case | Recommended Pipeline TOML | Hardware / Cloud Requirements | Command to Try |
|---|---|---|---|
| **Zero-setup Local CPU** | [`examples/minimal.toml`](../../voice-pipes/examples/minimal.toml) | Any modern CPU (x86_64/ARM64), no keys | `voice-pipes --config examples/minimal.toml live` |
| **Standalone Voice Generation (TTS-only)** | [`examples/tts/piper.toml`](../../voice-pipes/examples/tts/piper.toml) or [`kokoro.toml`](../../voice-pipes/examples/tts/kokoro.toml) | CPU; Piper or Kokoro model | `voice-pipes --config examples/tts/piper.toml tts --text "Hello!" --output hello.wav` |
| **Standalone Audio Transcription (STT-only)** | [`examples/stt/parakeet.toml`](../../voice-pipes/examples/stt/parakeet.toml) | CPU; Parakeet TDT model | `voice-pipes --config examples/stt/parakeet.toml stt --input hello.wav` |
| **Microphone Dictation (Listen-only)** | [`examples/listen/parakeet.toml`](../../voice-pipes/examples/listen/parakeet.toml) | CPU; Parakeet + Silero VAD | `voice-pipes --config examples/listen/parakeet.toml listen` |
| **High-Fidelity Neural Voice on CPU** | [`examples/live/kokoro.toml`](../../voice-pipes/examples/live/kokoro.toml) | CPU; Kokoro 82M model (~350 MB) | `voice-pipes --config examples/live/kokoro.toml live` |
| **Ultra-Low Latency Local GPU** | [`examples/live/moonshine.toml`](../../voice-pipes/examples/live/moonshine.toml) | NVIDIA GPU (CUDA 12), VibeVoice sidecar | `voice-pipes --config examples/live/moonshine.toml live` |
| **Cloud Cascade (Hosted STT+LLM+TTS)** | [`examples/live/openai-tts.toml`](../../voice-pipes/examples/live/openai-tts.toml) | `OPENAI_API_KEY` | `voice-pipes --config examples/live/openai-tts.toml live` |
| **Cloud Multilingual STT (Chirp 3)** | [`examples/live/google-croatian.toml`](../../voice-pipes/examples/live/google-croatian.toml) | `GOOGLE_APPLICATION_CREDENTIALS` | `voice-pipes --config examples/live/google-croatian.toml live` |
| **Hosted Speech-to-Speech (Gemini Live)** | [`examples/live/gemini-live.toml`](../../voice-pipes/examples/live/gemini-live.toml) | `GEMINI_API_KEY` (0 MB downloads) | `voice-pipes --config examples/live/gemini-live.toml interactive` |
| **Speech-to-Speech + Avatar (S2S Streaming)** | [`examples/avatar/avatar-realtime.toml`](../../voice-pipes/examples/avatar/avatar-realtime.toml) | `OPENAI_API_KEY` (or Gemini / ElevenLabs) | `voice-pipes --config examples/avatar/avatar-realtime.toml serve ...` |
| **Client-Side Tool Calling** | [`examples/avatar/avatar-cascade-clienttools.toml`](../../voice-pipes/examples/avatar/avatar-cascade-clienttools.toml) | Cloud or local LLM with Jinja tool support | Connect via Playground or browser client |
| **3D Avatar Lip-Sync Annotations** | [`examples/avatar/avatar-cascade.toml`](../../voice-pipes/examples/avatar/avatar-cascade.toml) | Wav2Arkit ONNX model (~15 MB) | Connect Playground with Avatar extension |
| **Distributed gRPC Split-Mode** | [`examples/grpc/pipeline-remote-tts.toml`](../../voice-pipes/examples/grpc/pipeline-remote-tts.toml) | gRPC component server on host/network | Start worker + start pipeline |
| **Anticipatory Turn Detection (VAP)** | [`examples/features/turn-vap.toml`](../../voice-pipes/examples/features/turn-vap.toml) | VAP ONNX model | `voice-pipes --config examples/features/turn-vap.toml live` |
| **Speculative Turn Execution** | [`examples/features/speculative-execution.toml`](../../voice-pipes/examples/features/speculative-execution.toml) | Local or fast cloud LLM | `voice-pipes --config examples/features/speculative-execution.toml live` |
| **Multi-Worker Fleet Orchestration** | [`examples/orchestrate/orchestrator.toml`](../../voice-pipes/examples/orchestrate/orchestrator.toml) | Orchestrator + 1..N workers | Start orchestrator, then start workers |
| **Automated A/B Benchmarking** | [`examples/bench-piper.toml`](../../voice-pipes/examples/bench-piper.toml) vs [`bench-pocket-http.toml`](../../voice-pipes/examples/bench-pocket-http.toml) | Orchestrator + 2 workers | `pwsh scripts/run-benchmark-multi.ps1` |

---

## 2. Step-by-Step Feature Recipes

All commands below are run from the `voice-pipes/` directory.

### Recipe 0: Standalone Component Utilities (Modular Toolkit Mode)
* **What it proves:** You can use Voice Pipes purely as a high-performance audio synthesis, speech recognition, or dictation tool without running an end-to-end voice loop.
* **Stack:** Individual stage configs under `examples/tts/`, `examples/stt/`, and `examples/listen/`.

```sh
cd voice-pipes

# 1. Synthesize a voice WAV from text using local Piper (or Kokoro):
voice-pipes --config examples/tts/piper.toml tts \
  --text "Welcome to Voice Pipes modular speech processing." \
  --output welcome.wav

# 2. Transcribe an audio file using local Parakeet TDT:
voice-pipes --config examples/stt/parakeet.toml stt \
  --input welcome.wav

# 3. Live microphone dictation directly to stdout (STT only, no LLM or TTS reply):
voice-pipes --config examples/listen/parakeet.toml listen

# 4. Offline batch re-synthesis (WAV -> STT -> TTS -> WAV):
voice-pipes --config examples/minimal.toml pipe \
  --input welcome.wav \
  --output resynthesized.wav
```

---

### Recipe 1: Pure Local CPU Baseline (No API Keys, 100% Offline)
* **What it proves:** Fast speech-to-text, text-to-speech, and language modeling running fully in-process on CPU with calibrated local SLAs.
* **Stack:** Silero VAD $\to$ Parakeet TDT STT $\to$ SmolLM2-360M GGUF $\to$ Piper TTS.

```sh
cd voice-pipes
voice-pipes models fetch
voice-pipes --config examples/minimal.toml live
```
*Speak into your mic. The agent answers when you pause. Press `Ctrl+C` to stop.*

---

### Recipe 2: Natural Neural Voice on CPU with Kokoro
* **What it proves:** Studio-quality natural voices (82M parameter model) synthesizing faster than real time without a GPU.
* **Stack:** Parakeet STT $\to$ SmolLM2 $\to$ Kokoro TTS (`af_heart` voice).

```sh
cd voice-pipes
# Build CLI with Kokoro TTS support:
cargo build --release -p voice-pipes-cli --features tts-kokoro

# Fetch Kokoro model & voicepack:
voice-pipes models fetch kokoro

# Run the live pipeline:
voice-pipes --config examples/live/kokoro.toml live
```

---

### Recipe 3: Full Cloud Cascade Stack (OpenAI / Hosted LLM)
* **What it proves:** Cloud scale, multi-turn reasoning, and natural voice using standard `/v1/chat/completions` endpoints.
* **Stack:** AssemblyAI (or Parakeet) $\to$ OpenAI GPT-4o-mini $\to$ OpenAI TTS (or Piper).

```sh
export OPENAI_API_KEY="sk-..."
voice-pipes --config examples/live/openai-tts.toml live
```

---

### Recipe 4: Hosted Speech-to-Speech with Gemini Live (Zero Model Downloads)
* **What it proves:** Unified end-to-end speech-in/speech-out neural model running in cloud with sub-400ms end-to-end latency behind the identical Voice Pipes session protocol, metrics, and audit trail.
* **Stack:** Direct bidirectional WebSocket audio streaming to Google Gemini Live (`gemini-3.8-live`).

```sh
# 1. Set your Gemini API key (from Google AI Studio):
export GEMINI_API_KEY="AIzaSy..."

# 2. Run terminal voice loop (zero model downloads needed):
voice-pipes --config examples/live/gemini-live.toml interactive

# Or serve as a WebSocket worker for the Admin Console Playground:
voice-pipes --config examples/live/gemini-live.toml serve \
    --worker-config examples/orchestrate/worker-standalone.toml
```
*Note: For OpenAI Realtime, use `examples/avatar/avatar-realtime.toml` with `OPENAI_API_KEY`.*

---

### Recipe 5: Client-Side Tool Calling (Actions & Function Execution)
* **What it proves:** The voice agent asks the client application to execute a local function (e.g. check battery, toggle lights, change screen color), receives the result, and speaks the confirmation seamlessly.
* **Configuration:** [`examples/avatar/avatar-cascade-clienttools.toml`](../../voice-pipes/examples/avatar/avatar-cascade-clienttools.toml) defines tools:
  ```toml
  [[client_tool]]
  name = "get_weather"
  description = "Get current weather for a city"
  parameters = { type = "object", properties = { city = { type = "string" } }, required = ["city"] }
  ```
* **Try it:** Run in the Admin Console Playground. When you say *"What's the weather in Paris?"*, the Playground UI displays the client tool invocation card and fulfills it automatically.

---

### Recipe 6: 3D Avatar Lip-Sync & Audio Annotations
* **What it proves:** Real-time 60 FPS blendshapes (ARKit-compatible 52 facial expressions) extracted concurrently off the audio critical path without adding a single millisecond of voice delay.
* **Configuration:** [`examples/avatar/avatar-cascade.toml`](../../voice-pipes/examples/avatar/avatar-cascade.toml) includes:
  ```toml
  [[audio_annotator]]
  name = "wav2arkit"
  model = "models/wav2arkit/wav2arkit.onnx"
  ```
* **Try it:** Open the Admin Console Playground with the **Avatar** extension enabled to watch the 3D face speak in exact sync with synthesized audio.

---

### Recipe 7: Distributed gRPC Split-Mode (Microservice Stages)
* **What it proves:** Decoupling a heavy model (e.g. GPU TTS or remote Python LLM) into a separate process or remote server using high-performance gRPC protobuf contracts.
* **Try it in 2 terminal tabs:**

**Tab 1 (Component Server hosting TTS):**
```sh
voice-pipes component-server --config examples/grpc/worker-tts.toml
```

**Tab 2 (Pipeline Worker connecting to remote TTS):**
```sh
voice-pipes --config examples/grpc/pipeline-remote-tts.toml live
```

---

### Recipe 8: Testing Headless Without a Microphone
* **What it proves:** Validates that every model, audio device, and token quota works cleanly without speaking into a physical mic.
* **Command:**

```sh
voice-pipes --config examples/minimal.toml check
```
*Outputs stage-by-stage timings: what STT heard, the LLM's response, and TTS synthesis speed.*

---

### Recipe 9: Automated A/B Benchmarking & Pipeline Comparison
* **What it proves:** Replays identical audio scenarios across two different pipeline variants to evaluate TTFB, TTFT, caller wait p90, and Word Error Rate with statistical aggregation and visual diffs.
* **Configurations:**
  - Variant A: [`examples/bench-piper.toml`](../../voice-pipes/examples/bench-piper.toml) (Piper TTS)
  - Variant B: [`examples/bench-pocket-http.toml`](../../voice-pipes/examples/bench-pocket-http.toml) (Pocket-TTS)
  - Or LLM comparison: [`examples/bench-gemma4-concise.toml`](../../voice-pipes/examples/bench-gemma4-concise.toml) vs [`examples/bench-openai.toml`](../../voice-pipes/examples/bench-openai.toml)
* **Run comparison:**
  ```powershell
  # Compare two local or cloud workers:
  .\scripts\run-benchmark-multi.ps1

  # Compare Speech-to-Speech models (Gemini Live vs OpenAI Realtime):
  .\scripts\bench-s2s-compare.ps1
  ```
* **View results:** Open `http://localhost:3000/benchmarks` in the Admin Console to view side-by-side latency bar charts, p50/p90 distributions, and diff tables. Full guide in [Benchmarking & Evals](benchmarking-and-evals.md).

---

## 3. Serving Any Example to the Browser / Admin Console

To connect the Admin Console (Playground, visual karaoke, session inspector) to any example pipeline:

```sh
voice-pipes --config <path-to-example.toml> serve \
    --worker-config examples/orchestrate/worker-standalone.toml
```

Then navigate to:
- **Playground:** `http://localhost:3000` (click Connect).
- **Session Details:** After speaking, inspect the timeline, audio tracks, and click **Quick Summary** for the diagnostic breakdown.
