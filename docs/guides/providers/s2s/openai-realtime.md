# OpenAI Realtime (speech-to-speech)

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs

## What it is

OpenAI's Realtime API over WebSocket: one model takes audio in and produces
audio out, with server-side VAD, transcripts and function calling. Voice
Pipes drives it as a speech-to-speech engine behind the same session
protocol, audit trail and metrics as the cascade.

## Cargo feature

`s2s-openai` on the **CLI crate**, where it is a default feature, so
`cargo install` and both worker images carry it. The provider lives in its
own crate, `voice-pipes-s2s-openai`, registered by the CLI at startup; it
needs the engine's speech-to-speech client, `realtime`, which the feature
turns on. (Until 2026-09 the engine's `realtime` feature built it in.)

## Config

```toml
[realtime_provider]
provider = "openai-realtime"
model = "gpt-realtime"
voice = "alloy"
vad = "server_vad"                  # or "none": the client drives turn boundaries
api_key_env = "OPENAI_API_KEY"
instructions = "You are a concise voice assistant. Speak English unless the user explicitly asks for another language."
# knowledge_base_path = "kb.md"      # appended to instructions at session start
# Configure.options keys a client may override (default: the vad_* knobs).
# Add "instructions" to let clients APPEND to the prompt; nothing replaces it.
# client_overrides = ["vad_threshold", "vad_silence_ms", "vad_prefix_padding_ms", "vad_eagerness"]

[realtime_provider.options]
output_emit_rate = 16000            # resample the provider's 24 kHz output, as one stream, before the pipeline
```

No `[stt]`, `[tts]` or `[[text_processor]]` sections; validation rejects
the combination.

## Environment

`OPENAI_API_KEY` (or whatever `api_key_env` names).

## How to run

### 1. Terminal voice loop (`interactive`)
```sh
export OPENAI_API_KEY="sk-..."
voice-pipes --config examples/avatar/avatar-realtime.toml interactive
```

### 2. Browser & Playground (`serve`)
```sh
voice-pipes --config examples/avatar/avatar-realtime.toml serve \
  --worker-config examples/orchestrate/worker-standalone.toml
```

## Capabilities

- Server VAD with explicit speech-started and speech-stopped events, which
  give barge-in and end-of-speech anchors directly from the provider.
- Input and output transcripts, streamed as `message` envelopes and as
  captions in `audio_meta`.
- Tool calling: client-side tools declared in the config are stamped into
  the session as tool specs; server-side tools through the agent link or a
  registered tool registry. See [Tool calling](../../tool-calling.md).
- Interruptions: a barge-in sends the provider a cancel and truncates the
  transcript at the offset the client actually played, including the tail
  after the provider reports the response done.
- Per-turn metrics: time to first audio byte (anchored on the first audio
  delta, not the first transcript), audio output duration, `provider_endpointing_ms`
  from vp's own end-of-speech observer.

## Limitations

- Model ids change. The dated `gpt-4o-realtime-preview-*` SKUs were retired
  with the GA launch and now fail at WebSocket connect, which the worker
  reports as a `RealtimeConnectFailed` (1004) error and closes the session.
  `gpt-realtime` is the current default; it is a config value.
- Provider cost is per minute of audio in both directions; a session streams
  continuously while connected.
- The provider's response latency is what it is; vp adds one channel hop.
  The metrics separate the provider's endpointing lag from its generation time
  so you can see which one you are paying for.

## Connection limits and reconnects

These apply to every realtime provider. Inbound WebSocket messages are
capped at 4 MiB, an audio field at 1 MiB of base64 and a tool call's
arguments at 64 KB; anything larger is reported as a recoverable error
and skipped. The worker pings the provider every 20 s and treats a
connection with no frame or pong for 60 s as closed. A transport error
(reset, TLS failure) is handled like a server close: the runner
classifies it and reconnects unless the reason is permanent (auth, quota,
close code 1008). Reconnects back off from 1 s, doubling to 16 s within
a streak; three closes within 15 s or more than ten over the session's
life end the session with `RealtimeConnectFailed`. A reconnect is
reported to the client as a warning. It is a fresh provider conversation
unless the provider can resume the old one: Gemini Live can (see
[Gemini Live](gemini-live.md)), `OpenAI` and `ElevenLabs` cannot.

## Upstream

OpenAI Realtime API documentation; model list and pricing on the OpenAI
platform pages.
