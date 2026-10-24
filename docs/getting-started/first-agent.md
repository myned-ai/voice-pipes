# Your first agent

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs

You have a working stack. This page changes it in the ways everyone changes it
first: a different LLM, a different voice, a persona, then serving it so a
client can connect. Everything is a config edit. This walkthrough uses the
native CLI; the same TOML drives the Docker worker.

## 1. Start from the minimal config

Copy `examples/minimal.toml` to `my-agent.toml` (its models come from
`voice-pipes models fetch`). The parts this page changes:

```toml
# my-agent.toml
[pipeline]
name = "my-agent"

[stt]
provider = "parakeet-tdt"
model_dir = "models/parakeet-tdt"

[tts]
provider = "piper"
model = "models/en_US-lessac-medium.onnx"

[[text_processor]]
type = "llm"
model = "models/smollm2-360m/SmolLM2-360M-Instruct-Q4_K_M.gguf"
system_prompt = "You are a helpful assistant. Respond briefly."
max_tokens = 128
```

```sh
voice-pipes --config my-agent.toml live
```

Speak; the pipeline answers when you pause. `Ctrl+C` stops it.

## 2. Swap the LLM

SmolLM2-360M is small. `voice-pipes models fetch smollm2-1.7b` gets a larger
one for the same processor, or point the chain at any OpenAI-compatible
endpoint instead:
llama.cpp's `llama-server`, Ollama, vLLM, LM Studio, or OpenAI itself.

```toml
[[text_processor]]
type = "openai-compat"
base_url = "http://localhost:8081/v1"     # llama-server; omit for api.openai.com
model = "gemma-4-E2B-it"                  # whatever the server calls the model
# api_key_env = "OPENAI_API_KEY"          # only for hosted endpoints
system_prompt = "You are a friendly voice assistant. Keep replies to one or two short sentences."
max_tokens = 256
temperature = 0.7
```

Start the server first, for example:

```sh
llama-server -m models/gemma4-e2b/gemma-4-E2B-it-Q4_K_M.gguf --port 8081 -ngl 99 -c 4096 --jinja
```

The `--jinja` flag matters if you will use [tool calling](../guides/tool-calling.md)
later; without it the server silently ignores tool definitions.

## 3. Change the voice

Kokoro sounds better than Piper and is still faster than real time on CPU.
Download the model and voice pack (see the [Kokoro page](../guides/providers/tts/kokoro.md)),
build with the feature, and change the TTS section:

```toml
[tts]
provider = "kokoro"
model = "models/kokoro-v1.0.onnx"
voices = "models/voices-v1.0.bin"
voice = "af_heart"
```

```sh
cargo build --release -p voice-pipes-cli --features tts-kokoro
```

Every provider page lists its Cargo feature, config block and limitations.

## 4. Tune turn-taking

Two settings decide how the agent feels in conversation. The silence timeout
is how long a pause has to be before the agent answers; barge-in is whether it
stops when you talk over it.

```toml
[turn_detection]
mode = "silence"
silence_timeout_ms = 700          # 1000 is the default; lower feels snappier, cuts off slow speakers

[barge_in]
enabled = true
min_chunks = 3                    # ~60 ms of sustained speech before interrupting
speech_threshold = 2000.0         # RMS; raise it in noisy rooms
```

Cloud STT providers that signal end-of-turn themselves (AssemblyAI, Google)
work best with `mode = "provider"` or `"layered"`. A semantic end-of-turn
model is available as `mode = "smart"`. Details in
[Turn detection and interruptions](../guides/turn-detection-and-interruptions.md).

## 5. Record what happened

```toml
[pipeline.audit]
enabled = true
captures_dir = "captures"
```

After a conversation, `captures/<session-id>/` holds the audio of both sides,
an event log and `metadata.json` with per-turn metrics. This is what the admin
console's session view renders. [Observability](../guides/observability.md)
explains every field.

## 6. Serve it

`live` uses your microphone and speakers. To let a client connect, enable the
transport and run the worker:

```toml
[transport]
enabled = true
bind = "0.0.0.0"
port = 8800
```

```sh
voice-pipes --config my-agent.toml serve --worker-config examples/orchestrate/worker-standalone.toml
```

The worker now accepts WebSocket sessions on `ws://localhost:8800/ws` and
answers `GET http://localhost:8800/health`. The admin console's Playground can
connect to it directly, or write your own client from
[Building a client](../clients/building-a-client.md). To manage several
workers and get aggregated history, add an orchestrator: [Fleet](../guides/fleet.md).

## 7. Go speech-to-speech instead

If you would rather use one model for the whole turn, replace `[stt]`, `[tts]`
and the text processor chain with a realtime provider:

```toml
[realtime_provider]
provider = "openai-realtime"          # or "gemini-live" / "elevenlabs"
model = "gpt-realtime"
voice = "alloy"
vad = "server_vad"
api_key_env = "OPENAI_API_KEY"
instructions = "You are a concise voice assistant."
```

Run with `interactive`:
```sh
voice-pipes --config my-agent.toml interactive
```

Build with `--features voice-pipes/realtime`. The session protocol, the
Playground, the audit trail and the metrics are the same; only the engine
changes. See the [speech-to-speech provider pages](../guides/providers/README.md#speech-to-speech).

## Next

- [Example pipelines & features](../guides/examples-and-features.md) — recipe cookbook for every feature and example configuration.
- [Configuration](../guides/configuration.md) — every section of the file.
- [Providers](../guides/providers/README.md) — what else you can plug in.
- [Tool calling](../guides/tool-calling.md) — let the agent act.
