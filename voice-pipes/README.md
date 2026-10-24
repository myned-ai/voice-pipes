# voice-pipes (engine crate)

The engine library behind Voice Pipes: the streaming pipeline, every built-in
provider, the WebSocket transport, the worker and the orchestrator. The
`voice-pipes` binary in `voice-pipes-cli/` is a thin composition root over
this crate.

```
Audio in ──→ [VAD] ──→ [STT provider] ──→ [Turn detector] ──→ [Text processor chain] ──→ [TTS provider] ──→ Audio out
                                                                         ↑
                                                            Barge-in monitor (cancels on user speech)

Audio in ──→ [Realtime provider: OpenAI Realtime · Gemini Live · ElevenLabs Agents] ──→ Audio out
```

**Documentation lives in [`../docs/`](../docs/README.md).** Start with the
[quickstart from source](../docs/getting-started/quickstart-source.md); the
[provider matrix](../docs/guides/providers/README.md) lists every STT, TTS,
LLM and speech-to-speech provider with its Cargo feature and config block.

## Build

```sh
# The binary, with the batteries (Parakeet, Piper, Silero, the local and
# OpenAI-compatible LLMs, speech-to-speech, telemetry):
cargo build --release -p voice-pipes-cli
# Exactly the providers named, nothing else:
cargo build --release -p voice-pipes-cli --no-default-features \
  --features stt-assemblyai,llm-openai-compat,tts-elevenlabs
# This crate alone, as a library: no provider unless you name one.
cargo build --release -p voice-pipes --features batteries
```

The engine has no default features; the CLI's defaults are the batteries
(`batteries` here) plus the plugin crates named in its `default-plugins` list. The full list with what each pulls in is in
[Cargo features](../docs/reference/cargo-features.md).
Rust 1.91+ (2024 edition); `protoc` is vendored; Piper and Kokoro need
`espeak-ng` at runtime.

## Layout

| Path | What |
|---|---|
| `src/pipeline/` | Streaming pipeline: tasks, channels, cancellation, barge-in, metrics, events, post-audio chain, annotators |
| `src/stt/`, `src/tts/`, `src/text/`, `src/vad/` | Providers and their factory registries |
| `src/realtime/` | Speech-to-speech client and tool dispatch |
| `src/transport/` | Session gateway, WebSocket codec, runners for both engines |
| `src/api/` | Orchestrator admin API, worker debug API, internal API, auth |
| `src/agent/` | Agent gRPC link, client-side tools |
| `src/commands/` | CLI subcommands including the config wizard |
| `src/audit/`, `src/telemetry.rs` | Capture artefacts, OpenTelemetry |
| `proto/` | gRPC contracts: STT, TTS, text processor, audio post-processor, agent |
| `examples/` | Example pipeline configurations used throughout the docs |
| `deploy/docker/` | The compose stack the launcher drives |

## Tests

```sh
cargo test --lib --features "text-grpc,realtime"
cargo test --test transport_integration --features "text-grpc,realtime"
```

Suites that need model files are `#[ignore]`d and say which files. See
[Testing](../docs/contributing/testing.md).

## License

Apache-2.0; see [`../LICENSE`](../LICENSE) and [`../NOTICE`](../NOTICE).
