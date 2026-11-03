# Voice Pipes Example Pipelines & Configurations

This directory contains production-ready and experimental configuration files for Voice Pipes.

---

## Directory Organization

### 1. Standalone Component Toolkits (No Full Voice Loop)
You can run any pipeline component independently without running a full conversational loop:

* **Text-to-Speech (TTS-only)** in [`tts/`](tts/):
  - [`tts/piper.toml`](tts/piper.toml): Lightweight, fast local voice generation on CPU with Piper.
  - [`tts/kokoro.toml`](tts/kokoro.toml): Studio-quality neural voice generation on CPU with Kokoro 82M.
  - [`tts/pocket-http.toml`](tts/pocket-http.toml): Low-latency Pocket TTS.
  ```sh
  voice-pipes --config examples/tts/piper.toml tts --text "Hello world" --output hello.wav
  ```

* **Speech-to-Text (STT-only)** in [`stt/`](stt/):
  - [`stt/parakeet.toml`](stt/parakeet.toml): Fast local offline transcription with Parakeet TDT on CPU.
  - [`stt/google.toml`](stt/google.toml): Cloud multilingual speech recognition with Google Cloud Speech (Chirp 3).
  - [`stt/assemblyai.toml`](stt/assemblyai.toml): Streaming cloud speech recognition with AssemblyAI.
  ```sh
  voice-pipes --config examples/stt/parakeet.toml stt --input speech.wav
  ```

* **Microphone Dictation (Listen-only)** in [`listen/`](listen/):
  - [`listen/parakeet.toml`](listen/parakeet.toml): Live microphone capture with Silero VAD filtering and Parakeet transcription directly to stdout (no LLM, no TTS).
  ```sh
  voice-pipes --config examples/listen/parakeet.toml listen
  ```

---

### 2. Conversational Voice Pipelines

* **Local & Cloud Cascades** in [`live/`](live/):
  - [`live/default.toml`](live/default.toml): Standard local cascade (Silero $\to$ Parakeet $\to$ SmolLM2 $\to$ Piper).
  - [`live/kokoro.toml`](live/kokoro.toml): Local cascade with Kokoro 82M neural voice.
  - [`live/moonshine.toml`](live/moonshine.toml): Local CUDA GPU cascade with Moonshine STT and VibeVoice.
  - [`live/openai-tts.toml`](live/openai-tts.toml): Cloud cascade using OpenAI GPT and OpenAI TTS.
  - [`live/gemini-live.toml`](live/gemini-live.toml): Cloud Speech-to-Speech using Google Gemini Live.
  ```sh
  # Run cascade:
  voice-pipes --config examples/live/default.toml live

  # Run speech-to-speech:
  voice-pipes --config examples/live/gemini-live.toml interactive
  ```

* **Avatar & Web Applications** in [`avatar/`](avatar/):
  - [`avatar/avatar-cascade.toml`](avatar/avatar-cascade.toml): Local cascade with real-time Wav2Arkit 3D facial blendshape annotations.
  - [`avatar/avatar-cascade-clienttools.toml`](avatar/avatar-cascade-clienttools.toml): Cascade pipeline with client-side function/tool execution.
  - [`avatar/avatar-realtime.toml`](avatar/avatar-realtime.toml): Full-duplex S2S streaming with avatar annotations.

* **Advanced Engine Features** in [`features/`](features/):
  - [`features/turn-vap.toml`](features/turn-vap.toml): Voice Activity Projection (VAP) anticipatory turn taking.
  - [`features/speculative-execution.toml`](features/speculative-execution.toml): Speculative LLM execution on partial transcripts.
  - [`features/adaptive-quality.toml`](features/adaptive-quality.toml): Dynamic model tier switching based on network conditions.

* **Fleet Orchestration & Distributed Workers** in [`orchestrate/`](orchestrate/):
  - [`orchestrate/orchestrator.toml`](orchestrate/orchestrator.toml): Fleet orchestrator configuration.
  - [`orchestrate/worker-standalone.toml`](orchestrate/worker-standalone.toml): Standalone worker serving WebSockets for browser & Admin Console.
  - [`orchestrate/worker.toml`](orchestrate/worker.toml): Fleet cluster worker configuration.

* **A/B Benchmarking & Variant Evaluation** in root [`examples/`](.):
  - [`bench-piper.toml`](bench/bench-piper.toml): Isolated TTS benchmark variant (STT $\to$ Echo $\to$ Piper TTS) evaluating voice latency without LLM jitter.
  - [`bench-pocket-http.toml`](bench/bench-pocket-http.toml): Pocket-TTS companion variant for side-by-side TTS shootout.
  - [`bench-openai.toml`](bench/bench-openai.toml): Cloud OpenAI GPT-4o-mini LLM benchmark variant for latency/quality comparison against local models.
  - [`bench-gemma4-concise.toml`](bench/bench-gemma4-concise.toml): Local GGUF Gemma 4 LLM benchmark variant.

---

For a complete recipe guide, see the [Examples & Feature Cookbook](../../docs/guides/examples-and-features.md).
