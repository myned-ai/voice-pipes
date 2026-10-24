# Quickstart from Source

**Status:** active  
**Audience:** Developers new to Voice Pipes  
**Estimated Time:** ~5 minutes

Welcome to Voice Pipes! Voice Pipes is an ultra-low-latency, modular voice AI runtime in Rust. It lets you run full-duplex conversational voice pipelines with instant interruption (barge-in), stream to web browsers and mobile apps, or use individual components (TTS, STT, Dictation) as standalone CLI tools.

You have two easy ways to start:
- **Option A (Local Offline Cascade):** Runs 100% on your machine on CPU. Zero API keys, zero cloud costs.
- **Option B (Hosted Speech-to-Speech):** Streams directly to Google Gemini Live. Zero model downloads needed.

> [!TIP]
> **Prefer Docker?** If you don't want to install Rust or C++ toolchains on your host machine, see the [Docker Quickstart](quickstart-docker.md) to launch the entire stack (Orchestrator, Worker, and Web Admin Console) with a single script (`./launch-stack.sh` or `.\launch-stack.ps1`).

---

## 1. Prerequisites

Before building from source, ensure you have:

- **Rust 1.91 or newer** from [rustup.rs](https://rustup.rs).
- **A C/C++ toolchain:**
  - **Windows:** Visual Studio 2022 Build Tools with the *Desktop development with C++* workload (`winget install Microsoft.VisualStudio.2022.BuildTools`).
  - **Linux (Ubuntu/Debian):** `sudo apt update && sudo apt install -y build-essential libasound2-dev pkg-config`.
  - **macOS:** Xcode command-line tools (`xcode-select --install`).
- **eSpeak NG** (used by the local Piper voice at runtime for phonemization):
  - **Windows:** `winget install espeak-ng.espeak-ng` (open a new terminal after installation so it is on `PATH`).
  - **Linux:** `sudo apt install -y espeak-ng`.
  - **macOS:** `brew install espeak-ng`.

---

## 2. Install the CLI

Clone the repository and install the `voice-pipes` binary:

```sh
# Clone repository
git clone https://github.com/myned-ai/voice-pipes.git
cd vp

# Build and install the voice-pipes binary to ~/.cargo/bin (takes 2-3 minutes the first time)
cargo install --locked --path voice-pipes-cli
```

Verify the installation:
```sh
voice-pipes --version
```

> [!NOTE]
> All subsequent commands should be run from the **`voice-pipes/`** engine subdirectory where example configurations and model directories reside:
> ```sh
> cd voice-pipes
> ```

---

## 3. Choose Your Quickstart Path

### Path A: Local Offline Cascade (100% Private, CPU, No API Keys)

Download the lightweight demo models (~950 MB once) and start the live microphone loop:

```sh
# 1. Fetch demo models (Silero VAD, Parakeet TDT, SmolLM2-360M, Piper TTS):
voice-pipes models fetch

# 2. Start the interactive voice loop:
voice-pipes --config examples/minimal.toml live
```

**Try it:** Speak into your microphone. When you finish speaking, the agent answers back through your speakers. If you speak while the agent is answering, it cuts off immediately (instant barge-in interruption). Press `Ctrl+C` to exit.

---

### Path B: Hosted Speech-to-Speech with Gemini Live (Zero Model Downloads)

If you prefer testing an end-to-end cloud voice model without downloading local model files, Voice Pipes includes native Gemini Live support out of the box:

```sh
# 1. Set your Gemini API key (from Google AI Studio):
export GEMINI_API_KEY="your-gemini-api-key"
# On Windows PowerShell: $env:GEMINI_API_KEY="your-gemini-api-key"

# 2. Start the interactive terminal voice session:
voice-pipes --config examples/live/gemini-live.toml interactive
```

This streams audio directly to Google's Gemini Live model with native server VAD and interruption handling behind the exact same session protocol.

---

## 4. What Just Happened?

In [`examples/minimal.toml`](../../voice-pipes/examples/minimal.toml), Voice Pipes configured and orchestrated a complete conversational loop:

```
Microphone ──> [Silero VAD] ──> [Parakeet STT] ──> [SmolLM2 LLM] ──> [Piper TTS] ──> Speaker
                     │                                                      │
                     └───────────── Barge-in Interruption ──────────────────┘
```

- **Silero VAD:** Detects speech boundaries and filters out silence.
- **Parakeet TDT STT:** Transcribes speech streaming on CPU in ~150 ms.
- **SmolLM2-360M GGUF:** Generates conversational text token-by-token in-process.
- **Piper TTS:** Synthesizes natural speech audio frames faster than real time.
- **Barge-in Monitor:** Continuously monitors your microphone while the agent is speaking; the moment you interrupt, it aborts in-flight generation and immediately silences the speaker.

---

## 5. Test Components Independently (Modular Toolkit)

You don't have to run an end-to-end voice loop. Voice Pipes stages can be used standalone as fast, modular CLI tools:

- **Generate speech audio from text (TTS):**
  ```sh
  voice-pipes --config examples/minimal.toml tts \
    --text "Welcome to Voice Pipes modular runtime!" \
    --output welcome.wav
  ```
- **Transcribe an audio file (STT):**
  ```sh
  voice-pipes --config examples/minimal.toml stt --input welcome.wav
  ```
- **Live microphone dictation (STT only, no LLM or TTS reply):**
  ```sh
  voice-pipes --config examples/minimal.toml listen
  ```
- **Headless pipeline check without speaking (Sanity check):**
  ```sh
  voice-pipes --config examples/minimal.toml check
  ```
  Runs each stage against known test fixtures and prints stage-by-stage timings.

---

## 6. Serve to the Browser & Admin Console Playground

You can serve any pipeline over WebSockets so browser apps, mobile devices, or the Admin Console can connect:

```sh
voice-pipes --config examples/minimal.toml serve \
  --worker-config examples/orchestrate/worker-standalone.toml
```

This starts a WebSocket worker on `ws://localhost:8800/ws` with a health check endpoint at `http://localhost:8800/health`.

### Launch the Admin Console (Web UI)
To launch the full visual dashboard, live microphone Playground, and session inspector:

```sh
# In a separate terminal from repository root:
./launch-stack.sh      # Linux / macOS
.\launch-stack.ps1     # Windows PowerShell
```

Then open **`http://localhost:3000`** in your browser:
1. Go to **Playground** and click **Connect**.
2. Talk to the agent and watch real-time **karaoke transcript reveals** synchronized to the audio.
3. Open **Sessions** and click on your conversation to inspect the turn timeline, audio tracks, and click **Quick Summary** for the diagnostic breakdown.

---

## 7. Benchmark and Compare Pipeline Variants (A/B Testing)

Voice Pipes includes an automated benchmarking harness to compare two or more pipeline configurations side-by-side using deterministic audio replay and metric diffing:

```powershell
# Run automated A/B comparison across two worker variants:
.\scripts\run-benchmark-multi.ps1
```

- **What it does:** Starts two workers with different configurations (e.g. Variant A: Piper TTS vs Variant B: Pocket-TTS, or Local GGUF vs OpenAI), replays audio scenarios across multiple repetitions, and scores latency (TTFB, TTFT, caller wait p90) and accuracy.
- **View visual diffs:** Open `http://localhost:3000/benchmarks` in the Admin Console to view side-by-side bar charts, latency distributions, and p50/p90 diff tables.
- **Compare Speech-to-Speech:** To benchmark cloud S2S models (OpenAI Realtime vs Gemini Live), run:
  ```powershell
  .\scripts\bench-s2s-compare.ps1
  ```
- For custom scenarios, complex multi-worker setups, and CI evaluation gates, see the [Benchmarking & Evals Guide](../guides/benchmarking-and-evals.md).

---

## 8. Troubleshooting & Common Pitfalls

- **`command not found: voice-pipes`:** Ensure Cargo's bin directory (`~/.cargo/bin` on Linux/macOS or `%USERPROFILE%\.cargo\bin` on Windows) is in your system's `PATH`.
- **`phonemization failed: espeak not found`:** eSpeak NG is missing or not detected on `PATH`. Install it via your package manager and restart your terminal.
- **`model not found`:** Ensure you ran `voice-pipes models fetch` from inside the `voice-pipes/` directory.
- **Speech-to-Speech vs Cascade commands:**
  - Cascade pipelines (`STT -> LLM -> TTS`) use **`voice-pipes live`**.
  - Speech-to-Speech pipelines (`[realtime_provider]`) use **`voice-pipes interactive`**.
- **Linker errors on Windows:** Verify that the C++ build tools are installed via the Visual Studio Installer with the MSVC v143 toolset and Windows SDK.

---

## 9. Next Steps

- **[Examples & Feature Cookbook](../guides/examples-and-features.md):** Ready-to-run recipes for 3D avatars, VAP turn-taking, speculative execution, and cloud providers.
- **[Building Your First Agent](first-agent.md):** Step-by-step customization of system prompts, personalities, and custom voices.
- **[Configuration Reference](../guides/configuration.md):** Complete TOML schema for all pipeline stages.
- **[CLI Reference](../reference/cli.md):** Full list of subcommands, arguments, and diagnostic flags.
