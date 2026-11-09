# Providers

**Status:** active
**Last verified:** 2026-09-23
**Owner:** Docs

Every pipeline stage is a provider you select in configuration. This page is
the support matrix; each provider has its own page with the Cargo feature, a
complete config block, environment variables, capabilities and measured
limitations. Everything listed here works today; what is planned is on the
[roadmap](../../roadmap.md).

Local providers run in-process on CPU (or GPU where noted) with no API key
and no audio leaving the machine.

## Speech-to-text

| Provider | `provider =` | Runs | Feature | Notes |
|---|---|---|---|---|
| [Parakeet TDT](stt/parakeet.md) | `parakeet-tdt` | local, ONNX, CUDA optional | `stt-parakeet` (default; CLI crate; plugin) | NVIDIA FastConformer + TDT, int8, word timestamps, the default |
| [Moonshine](stt/moonshine.md) | `moonshine` | local, ONNX, CUDA optional | `stt-moonshine` (CLI crate; plugin) | Small and fast; `stt-moonshine-cuda` for GPU |
| [Nemotron 3.5](stt/nemotron-3.5.md) | `nemotron-3.5` | local, ONNX, CUDA optional | `stt-nemotron35` (CLI crate; plugin) | True streaming RNNT, 40 locales, per-session language; `stt-nemotron35-cuda` for GPU |
| [AssemblyAI](stt/assemblyai.md) | `assemblyai` | cloud, WebSocket | `stt-assemblyai` (CLI crate; plugin) | Streaming, end-of-turn signal, word timestamps |
| [Google Cloud STT](stt/google.md) | `google` | cloud, gRPC | `stt-google` (CLI crate; plugin) | Chirp 3, 100+ languages, voice-activity events |
| [gRPC](stt/grpc.md) | `grpc` | remote component server | `stt-grpc` | Any provider behind the split-mode server |

## Text-to-speech

| Provider | `provider =` | Runs | Feature | Notes |
|---|---|---|---|---|
| [Piper](tts/piper.md) | `piper` | local, ONNX, CUDA optional | `tts-piper` (default; CLI crate; plugin) | Fastest; needs eSpeak NG |
| [Kokoro](tts/kokoro.md) | `kokoro` | local, ONNX | `tts-kokoro` (CLI crate; plugin) | 24 kHz, 26 voices, still faster than real time on CPU |
| [Pocket-TTS](tts/pocket.md) | `pocket-http` | local sidecar (HTTP) | `tts-pocket` (CLI crate; plugin) | Kyutai Pocket-TTS; multilingual variants |
| [VibeVoice](tts/vibevoice.md) | `openai` pointed at the sidecar | local sidecar (GPU) | `tts-openai` (CLI crate; plugin) | Quality-first; below real time, not for conversation |
| [OpenAI TTS](tts/openai.md) | `openai` | cloud or any compatible endpoint | `tts-openai` (CLI crate; plugin) | `/v1/audio/speech`, chunked streaming |
| [Gemini TTS](tts/gemini.md) | `gemini` | cloud | `tts-gemini` (CLI crate; plugin) | Generative-language REST API |
| [ElevenLabs TTS](tts/elevenlabs.md) | `elevenlabs` | cloud | `tts-elevenlabs` (CLI crate; plugin) | Streaming with word timestamps for karaoke captions |
| [gRPC](tts/grpc.md) | `grpc` | remote component server | `tts-grpc` | Any provider behind the split-mode server |

## LLM and text processors

| Processor | `type =` | Runs | Feature | Notes |
|---|---|---|---|---|
| [Local GGUF](llm/local-gguf.md) | `llm` | local, candle | `llm-gguf` (CLI crate; plugin; default) | Quantised GGUF models in-process; CPU or CUDA |
| [OpenAI-compatible](llm/openai-compatible.md) | `openai-compat` | cloud or local server | `llm-openai-compat` (default) | OpenAI, llama.cpp, Ollama, vLLM, LM Studio; function calling |
| [gRPC](llm/grpc.md) | `grpc` | remote component server | `text-grpc` | Any text processor behind the split-mode server |
| [Agent link](llm/agent-grpc.md) | `agent-grpc` | remote agent service | `text-grpc` | An external agent as the brain, over the `AgentService` protocol |
| Echo | `echo` | built-in | — | Returns the input; for testing the audio path |

## Speech-to-speech

One provider replaces STT, LLM and TTS: `[realtime_provider]` in place of
the cascade sections. Each provider is a plugin crate the CLI builds by
default.

| Provider | `provider =` | Notes |
|---|---|---|
| [OpenAI Realtime](s2s/openai-realtime.md) | `openai-realtime` | `gpt-realtime`, server VAD, tool calling |
| [Gemini Live](s2s/gemini-live.md) | `gemini-live` | native-audio models, thinking budget control |
| [ElevenLabs Agents](s2s/elevenlabs-agents.md) | `elevenlabs` | agent configured in the ElevenLabs dashboard |

## VAD and turn detection

Not providers in the factory sense but selected the same way:

- VAD: `silero` (feature `vad-silero`, default; CLI crate; plugin), `energy`, `passthrough`. [Guide](../vad.md).
- Turn detection: `silence`, `provider`, `layered`, and the plugins `smart` (CLI feature `turn-smart`) and `vap` (CLI feature `turn-vap`, experimental). [Guide](../turn-detection-and-interruptions.md).

## Choosing

- **No keys, no cloud:** Parakeet + local GGUF or `llama-server` + Kokoro.
- **Best latency with cloud budget:** AssemblyAI + a hosted LLM + ElevenLabs
  or OpenAI TTS, or a speech-to-speech provider.
- **Languages beyond English:** Google Cloud STT (Chirp 3) for recognition;
  Nemotron 3.5 locally where its tier for the language is good enough (it is
  not, yet, for Croatian); Pocket-TTS variants or a cloud TTS for synthesis.
- **Lip-sync or expressive output:** any TTS plus an
  [audio annotator](../audio-annotators.md); VibeVoice for offline quality work.

Each page's *Limitations* section records what was measured, so compare
pages by position.

## Endpoints and API keys

Every hosted provider connects through the same network policy: TLS is
`rustls`, requests have a connect and an overall timeout, and redirects are
not followed (a redirect to another host would otherwise carry a custom key
header such as `xi-api-key` along).

A provider whose `endpoint` (or `base_url`) you change refuses to start if
the API key would travel in plain `http://` or `ws://` to a host on the
internet. Local hosts are fine: loopback, private addresses, single-label
names such as a Docker service (`http://vibevoice:8091`), and private-use
names (`host.docker.internal`, `*.local`, `*.internal`, `*.home.arpa`,
`*.lan`). For any other plain-text host on a network you trust, set the
provider option:

```toml
[tts.options]
allow_insecure_endpoint = true
```

## Adding one

Out-of-tree: [Plugin SDK](../../extending/plugin-sdk.md); Nemotron 3.5 is
the in-repository example of that route. Built-in: the provider implements
the trait, registers a factory, gets a feature, a
[wizard descriptor](../../extending/wizard-descriptor.md) and a page here.
