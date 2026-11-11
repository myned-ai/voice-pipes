# Concepts

**Status:** active
**Last verified:** 2026-09-19
**Owner:** Docs

The vocabulary the rest of the documentation assumes. Each term links to the
page that goes deeper.

## Pipeline

A **pipeline** is the chain that turns incoming audio into outgoing audio.
Voice Pipes has two engines behind the same session protocol:

```
Cascade:   Audio in → Audio filter → VAD → STT → Turn detector → Text processor chain → TTS → Audio out
                                                                       ↑
                                                      Barge-in monitor (cancels on user speech)

Speech-to-speech:   Audio in → Realtime provider (OpenAI Realtime, Gemini Live, ElevenLabs Agents) → Audio out
```

A pipeline is described by one TOML file. Which engine runs is decided by the
file: a `[realtime_provider]` section selects speech-to-speech and is mutually
exclusive with `[stt]` and `[tts]`. See [Configuration](../guides/configuration.md).

## Provider, factory, feature

A **provider** is an implementation of one pipeline stage: an STT model, a
TTS model, an LLM endpoint, a realtime model. Providers implement a trait
(`SttProvider`, `TtsProvider`, `TextProcessor`) and are constructed by a
**factory** that validates configuration. Providers are compiled in behind
Cargo **features** (`stt-parakeet`, `tts-kokoro`, `realtime`, …) so a binary
only carries the inference runtimes it needs. External providers implement the
same traits in their own crate through the [plugin SDK](../extending/plugin-sdk.md).

Swapping a provider is a config edit: change `provider = "…"` and its options.
The [providers guide](../guides/providers/README.md) has one page per provider.

## Text processor chain

Between STT and TTS runs an ordered chain of **text processors** declared as
`[[text_processor]]` tables. The usual chain is a single LLM (`llm` for a local
GGUF model, `openai-compat` for any OpenAI-compatible endpoint), but a
processor can be anything text-to-text: a translator, a filter, a router. The
chain streams: sentence-sized fragments flow to TTS as the LLM produces them.

## Session

A **session** is one client connection to a worker. It starts with a
handshake (`configure` or `resume`), negotiates sample rates and providers,
and then carries audio and control messages until either side closes it. A
session can be resumed after a disconnect with a session token. See
[Building a client](../clients/building-a-client.md).

## Turn and turn state

A **turn** is one exchange: the user speaks, the agent answers. The gateway
tracks each session in one of four states, and every transition is sent to
the client as a `state_changed` control message:

```
Listening → Processing → Speaking → Draining → Listening
```

- **Listening** — waiting for the user.
- **Processing** — an utterance was committed; STT is final and the LLM is working.
- **Speaking** — the first assistant audio has been sent.
- **Draining** — the worker has finished sending; the client is still playing its buffered tail.

The transitions are guarded by a state machine and pinned by golden fixtures.
The full model is in [The interruption model](../explanation/interruption-model.md).

## Utterance

An **utterance** is one contiguous stretch of user speech that the pipeline
commits as input. Every utterance gets a numeric `utterance_id` that tags all
audio, captions, metrics and annotations produced in response to it.

## Audio filter, VAD, turn detection, barge-in

Four different questions, four different components:

- **Audio filter** (`[[audio_filter]]`) cleans or gates incoming microphone PCM
  in place *before* VAD and STT see it. The built-in `deepfilter` plugin
  suppresses stationary background noise, mains hum, and fan rumble using
  decision-directed ERB Wiener filtering (`8..96 kHz`).
  [Guide](../guides/vad.md#ingress-noise-suppression-audio_filter).
- **VAD** (voice activity detection) answers *is this audio speech?* per chunk.
  Silero (neural, ONNX) is the default; an energy threshold is the fallback;
  `passthrough` hands everything to a provider that has its own VAD.
  [Guide](../guides/vad.md).
- **Turn detection** answers *has the user finished?* Modes: `silence`
  (timeout), `provider` (the STT's end-of-turn signal), `layered` (provider
  first, silence fallback), `smart` (a semantic end-of-turn model) and the
  experimental `vap` (a two-channel model that predicts the end of the turn
  before it happens).
  [Guide](../guides/turn-detection-and-interruptions.md).
- **Barge-in** answers *did the user start talking while the agent was
  speaking?* A dedicated monitor cancels in-flight LLM and TTS work, the
  client reports how much audio it actually played, and the transcript is
  truncated to what was heard.

## Worker and orchestrator

A **worker** is one `voice-pipes serve` process: it loads a pipeline, accepts
WebSocket sessions on `/ws` (port 8800 by default) and exposes a local debug
API. A worker can run standalone.

An **orchestrator** (`voice-pipes orchestrate`) manages a fleet of workers:
registration, heartbeats, least-connections dispatch, drain, aggregated
session history and metrics. The admin console talks only to the
orchestrator. [Guide](../guides/fleet.md).

## Audit trail, captures, per-turn metrics

With `[pipeline.audit] enabled = true`, every session writes a capture
directory: WAV audio for each side, an event log, and `metadata.json` with
per-turn, per-component metrics (STT latency, LLM time to first token, TTS
time to first byte, throughput, playback quality reported by the client). The
same metrics are sent to the client as `turn_metrics` and exported through
OpenTelemetry. [Guide](../guides/observability.md).

## Client protocol

Clients talk to a worker over one WebSocket. Binary frames carry PCM audio
with a 6-byte header; JSON text frames carry three envelopes: `control`
(handshake, state, errors, playback reports), `message` (transcripts, tool
calls, structured content) and `audio_meta` (captions with word timings sent
just before the audio they describe). [Wire protocol](../clients/protocol.md).

## Component

Anything that implements the `Component` trait: every provider, the VAD, the
output stage, each audio post-processor. Components report health, rolling
latency and error rates, appear on `/health` and in the admin console, and can
opt into the per-turn metrics table with a descriptor.
[Custom component](../extending/custom-component.md).
