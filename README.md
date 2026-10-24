# Voice Pipes

[![core-tests](https://img.shields.io/badge/core--tests-passing-brightgreen)](https://github.com/myned-ai/voice-pipes/actions/workflows/core-tests.yml)
[![docs](https://img.shields.io/badge/docs-read-2456d6)](docs/README.md)
[![license](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![rust](https://img.shields.io/badge/rust-1.91%2B-orange)](docs/getting-started/quickstart-source.md)

A modular, observable real-time voice AI runtime in Rust. Plug any STT, LLM, and
TTS provider into a streaming pipeline with neural VAD, barge-in, and per-turn
metrics, or connect a speech-to-speech provider (OpenAI Realtime, Gemini Live,
ElevenLabs Agents) behind the same session protocol. Ships with local
ONNX/GGUF providers that run entirely on your machine with no cloud keys.

<!-- TODO(launch): demo GIF here — ~20s of the Playground: talk, karaoke
     captions reveal, then barge in mid-sentence and it stops instantly.
     Second visual: session-detail per-turn latency screenshot. -->

```
Audio in ──→ [VAD] ──→ [STT provider] ──→ [Turn detector] ──→ [Text processor chain] ──→ [TTS provider] ──→ Audio out
                                                                         ↑
                                                            Barge-in monitor (cancels on user speech)
```

## Install

**Docker, no host dependencies** (the launcher runs a setup wizard, downloads
the models you pick, and starts the orchestrator, a worker and the admin
console):

```sh
./launch-stack.sh     # Linux / macOS / WSL
.\launch-stack.ps1    # Windows
```

**From source** (Rust 1.91+; on Linux the default build needs the ALSA
headers, `libasound2-dev`; Piper needs `espeak-ng` at runtime):

```sh
cargo install --locked --git https://github.com/myned-ai/voice-pipes voice-pipes-cli
```

Prebuilt binaries and container images accompany tagged releases; see the
[release process](docs/contributing/release-process.md).

## A complete local pipeline in twelve lines

```toml
# my-agent.toml — no API keys, nothing leaves the machine
[stt]
provider = "parakeet-tdt"
model_dir = "models/parakeet-tdt"

[[text_processor]]
type = "llm"
model = "models/smollm2-360m/SmolLM2-360M-Instruct-Q4_K_M.gguf"
system_prompt = "You are a helpful assistant. Respond briefly."

[tts]
provider = "piper"
model = "models/en_US-lessac-medium.onnx"
```

```sh
voice-pipes models fetch                     # these models, once (~950 MB)
voice-pipes --config my-agent.toml live      # microphone → speaker, Ctrl+C to stop
```

Swapping a provider is a config edit: change `provider = "…"` and its options.
The [quickstart](docs/getting-started/quickstart-source.md) is the
three-command version; [your first agent](docs/getting-started/first-agent.md) changes the
LLM, the voice and the turn-taking.

## Use components independently (TTS, STT, Dictation)

Voice Pipes isn't only an end-to-end voice loop. Every component can also be run standalone as a high-performance CLI tool without writing boilerplate code:

```sh
# Generate speech WAV from text using Piper, Kokoro, ElevenLabs, or OpenAI TTS:
voice-pipes --config examples/tts/piper.toml tts --text "Welcome to Voice Pipes!" --output welcome.wav

# Transcribe an audio file using Parakeet TDT, Nemotron, or Moonshine:
voice-pipes --config examples/stt/parakeet.toml stt --input welcome.wav

# Live mic dictation to terminal (STT only, no LLM or spoken replies):
voice-pipes --config examples/listen/parakeet.toml listen

# Round-trip offline audio file re-synthesis (WAV -> STT -> TTS -> WAV):
voice-pipes --config examples/minimal.toml pipe --input speech.wav --output resynthesized.wav
```

## Why Voice Pipes?

- **Rust, not Python.** Every pipeline stage runs on native threads with
  channel hand-off; there is no interpreter lock to block the audio path while
  a model runs. A worker is a single binary.
- **Local-first.** The default stack (STT, LLM, TTS, VAD) runs fully offline:
  no per-minute API costs, no keys to provision, no audio leaving your
  machine. Cloud and speech-to-speech providers are opt-in.
- **Observability built in.** Every conversation records per-turn,
  per-component latency, the audio of both sides, the events between them,
  what the client actually played, and OpenTelemetry traces. When a reply
  feels slow you can see which stage spent the milliseconds.
- **Interruption done properly.** A dedicated barge-in monitor cancels
  in-flight LLM and TTS the moment the user speaks, with word-exact playback
  truncation, so the agent never talks over people.
- **Apache-2.0**, with a semver-disciplined plugin SDK: new providers live in
  external crates against `voice-pipes-sdk`, no fork required.

## Providers

Everything listed works today; each links to a page with the Cargo feature,
config block and measured limitations. What is planned lives on the
[roadmap](docs/roadmap.md).

| Stage | Local, no keys | Cloud |
|---|---|---|
| **STT** | [Parakeet TDT](docs/guides/providers/stt/parakeet.md), [Moonshine](docs/guides/providers/stt/moonshine.md), [Nemotron streaming](docs/guides/providers/stt/nemotron.md) | [AssemblyAI](docs/guides/providers/stt/assemblyai.md), [Google Cloud STT (Chirp 3, 100+ languages)](docs/guides/providers/stt/google.md) |
| **LLM** | [GGUF in-process](docs/guides/providers/llm/local-gguf.md), any [OpenAI-compatible server](docs/guides/providers/llm/openai-compatible.md) (llama.cpp, Ollama, vLLM), an [external agent over gRPC](docs/guides/providers/llm/agent-grpc.md) | OpenAI, Gemini, anything with `/v1/chat/completions` |
| **TTS** | [Piper](docs/guides/providers/tts/piper.md), [Kokoro](docs/guides/providers/tts/kokoro.md), [Pocket-TTS](docs/guides/providers/tts/pocket.md), [VibeVoice](docs/guides/providers/tts/vibevoice.md) (GPU) | [OpenAI](docs/guides/providers/tts/openai.md), [Gemini](docs/guides/providers/tts/gemini.md), [ElevenLabs](docs/guides/providers/tts/elevenlabs.md) (word timestamps) |
| **Speech-to-speech** | — | [OpenAI Realtime](docs/guides/providers/s2s/openai-realtime.md), [Gemini Live](docs/guides/providers/s2s/gemini-live.md), [ElevenLabs Agents](docs/guides/providers/s2s/elevenlabs-agents.md) |
| **VAD / turn detection** | Silero, energy; silence, provider signal, layered, [Smart Turn](docs/guides/turn-detection-and-interruptions.md) (VAP anticipatory detection is [experimental](docs/guides/turn-detection-and-interruptions.md#vap-anticipatory-experimental)) | |

Any stage can also run in another process over [gRPC](docs/extending/grpc-split-mode.md).
Full matrix: [Providers](docs/guides/providers/README.md).

## More than talking

- **Tool calling** — client-side tools the LLM invokes and your app fulfils,
  and server-side tools through an agent link. [Guide](docs/guides/tool-calling.md)
- **Audio annotators** — lip-sync blendshapes, visemes, phonemes or any
  per-audio metadata from a sidecar that runs off the critical path and can
  never slow the voice down. [Guide](docs/guides/audio-annotators.md)
- **Admin console** — a live voice playground with karaoke captions, a
  session explorer with per-turn breakdowns and health verdicts, and A/B
  benchmarking of pipeline variants. [Guide](docs/guides/admin-console.md)
- **Fleet** — an orchestrator with worker registry, heartbeats,
  least-connections dispatch and drain; or one worker standalone.
  [Guide](docs/guides/fleet.md)
- **Standalone component utilities** — Synthesise audio files from text (`voice-pipes tts`), transcribe speech WAVs (`voice-pipes stt`), or run live terminal dictation (`voice-pipes listen`) without writing boilerplate code.
- **Benchmarks and evals** — two pipeline variants against the same scenarios
  with a synthetic caller, deterministic scoring and diffable results.
  [Guide](docs/guides/benchmarking-and-evals.md)

## Examples

| Example | What it shows |
|---|---|
| [`voice-pipes/examples/minimal.toml`](voice-pipes/examples/minimal.toml) | The fully local cascade above |
| [`voice-pipes/examples/live/openai-tts.toml`](voice-pipes/examples/live/openai-tts.toml) | Cloud stack: AssemblyAI + a hosted LLM + OpenAI TTS |
| [`voice-pipes/examples/live/moonshine.toml`](voice-pipes/examples/live/moonshine.toml) | Local GPU stack: Moonshine on CUDA, Smart Turn, VibeVoice |
| [`voice-pipes/examples/stt/google.toml`](voice-pipes/examples/stt/google.toml) | Croatian recognition with Google Chirp 3 |
| [`voice-pipes/examples/avatar/`](voice-pipes/examples/avatar/) | Avatar workers with client-side tools, cascade and speech-to-speech |
| [`voice-pipes/examples/grpc/`](voice-pipes/examples/grpc/) | TTS in a separate process over gRPC |
| [`examples/reference-agent/`](examples/reference-agent/) | A protocol-complete Python agent over the `AgentService` gRPC contract |

See the [Example Pipelines Cookbook](docs/guides/examples-and-features.md) for the complete feature-to-pipeline selection matrix and step-by-step recipes to try each feature.

## Your first conversation (2 minutes)

1. Open the admin console at http://localhost:3000 and go to **Playground**.
2. Click **Connect** and grant microphone access.
3. Say something. The agent transcribes, thinks and answers out loud, with
   the transcript revealing word by word in sync with the audio.
4. **Interrupt it mid-sentence.** It stops immediately. That is the barge-in
   monitor.
5. Open **Sessions**, click the conversation you just had, and look at the
   per-turn breakdown: how many milliseconds VAD, STT, the LLM and TTS each
   spent on every reply.

Cloud providers read API keys from the environment by name
(`OPENAI_API_KEY`, `GEMINI_API_KEY` / `GOOGLE_API_KEY`, `ELEVENLABS_API_KEY`,
`ASSEMBLYAI_API_KEY`); none is required for the local stack. Copy
[`voice-pipes/.env.example`](voice-pipes/.env.example) to `voice-pipes/.env`
or let the wizard write it.

## How it compares

Honest, verifiable differences against the frameworks people usually evaluate
alongside; the [discussion](docs/explanation/comparison.md) has the longer
table.

| | **Voice Pipes** | Pipecat | LiveKit Agents | Vocode |
|---|---|---|---|---|
| Language | **Rust** | Python | Python | Python |
| License | Apache-2.0 | BSD-2 | Apache-2.0 | MIT |
| Fully local STT+LLM+TTS (no cloud keys) | **Yes** | TTS only | No | Whisper.cpp only |
| Barge-in / interruption | Yes | Yes | Yes | Partial |
| Per-turn latency audit trail | **Built-in** | No | No | No |
| A/B pipeline benchmarking | **Built-in** | No | No | No |
| Telephony (PSTN/SIP) | Not yet | Via Daily/Twilio | Built-in SIP | Twilio |
| Client SDKs | WebSocket (any language) | JS/React/Swift/Kotlin/… | Extensive | React |

## Documentation

[docs/README.md](docs/README.md) is the front door, with three paths: build
an agent, connect a client, write a provider. Highlights:

- [Concepts](docs/getting-started/concepts.md) · [Configuration](docs/guides/configuration.md) · [Turn detection and interruptions](docs/guides/turn-detection-and-interruptions.md) · [Observability](docs/guides/observability.md) · [Deployment](docs/guides/deployment.md)
- [Building a client](docs/clients/building-a-client.md) · [Wire protocol](docs/clients/protocol.md)
- [Plugin SDK](docs/extending/plugin-sdk.md) · [Design decisions](docs/explanation/decisions/README.md)
- [CLI](docs/reference/cli.md) · [Configuration reference](docs/reference/config.md) · [HTTP API](docs/reference/http-api.md)

AI coding agents: start from [docs/llms.txt](docs/llms.txt).

## Repository layout

| Path | What it is |
|---|---|
| [`voice-pipes/`](voice-pipes/) | Engine library: pipeline, providers, transport, orchestrator |
| [`voice-pipes-sdk/`](voice-pipes-sdk/) | Stable plugin SDK, the semver-disciplined trait boundary for external providers |
| [`voice-pipes-cli/`](voice-pipes-cli/) | The `voice-pipes` command-line binary |
| [`voice-pipes-bench/`](voice-pipes-bench/) | Benchmarking and automated voice-testing extension |
| [`voice-pipes-admin/`](voice-pipes-admin/) | Admin console: React frontend + FastAPI backend |
| [`examples/`](examples/) | Standalone integrations that talk to a worker from outside |
| [`docs/`](docs/) | Documentation |

Container images: `voice-pipes/Dockerfile` (CPU worker and orchestrator),
`Dockerfile.cuda` (GPU build), `Dockerfile.vibevoice` (VibeVoice GPU TTS
sidecar), `voice-pipes-admin/{frontend,backend}/Dockerfile` (admin console),
all orchestrated by `voice-pipes/deploy/docker/docker-compose.yml` through the
launcher.

## Contributing

Bug reports, providers, docs and fixes are welcome; see
[CONTRIBUTING.md](CONTRIBUTING.md), the
[development setup](docs/contributing/development-setup.md) and the
[roadmap](docs/roadmap.md). Security issues go to the address in
[SECURITY.md](SECURITY.md).

## License

Copyright 2026 Myned AI. Licensed under the [Apache License, Version 2.0](LICENSE).
Third-party attributions, including the vendored Microsoft VibeVoice package,
are listed in [NOTICE](NOTICE).
