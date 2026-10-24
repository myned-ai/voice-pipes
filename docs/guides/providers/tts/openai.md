# OpenAI TTS and compatible endpoints (TTS, cloud or local)

**Status:** active
**Last verified:** 2026-09-27
**Owner:** Docs

## What it is

A client for the OpenAI `POST /v1/audio/speech` API with chunked PCM
streaming. Because the endpoint is configurable, the same provider drives any
server that speaks the same API, which is how the [VibeVoice](vibevoice.md)
GPU sidecar is reached.

It is a plugin crate, `voice-pipes-tts-openai`, built on `voice-pipes-sdk`
and `voice-pipes-net`; the engine contains no OpenAI TTS code. The live
voice works as before, and the setup wizard still offers both the cloud
service and the VibeVoice sidecar.

## Cargo feature

`tts-openai` on the **CLI crate**:
`cargo build -p voice-pipes-cli --features tts-openai`. Both worker images
carry it. (Until 2026-09 it was an engine feature; `-p voice-pipes
--features tts-openai` no longer exists.)

## Config

```toml
[tts]
provider = "openai"
model_name = "tts-1"
voice = "alloy"
# endpoint = "http://vibevoice:8091/v1"   # any OpenAI-compatible speech server
# api_key = "local"                       # literal key for local servers; else OPENAI_API_KEY

[tts.options]
# instructions = "Speak warmly and slowly."   # delivery-style prompt for models that accept one
```

## Environment

`OPENAI_API_KEY` for OpenAI. Local or third-party servers accept any literal
`api_key`.

## Capabilities

| | |
|---|---|
| Output sample rate | 24 kHz PCM |
| Streaming | Yes, progressive chunked transfer |
| Word timestamps | No |
| Model identity | Resolved from the server's `/v1/models` at session setup, so the session report names the real model behind a compatible endpoint |
| Warm-up | Probes the endpoint at session start; an unreachable server surfaces as a `TtsWarmupFailed` (4002) warning to the client and marks the component unhealthy |

## Limitations

- OpenAI's `tts-1` is fast and clean but not free; `tts-1-hd` is slower.
- A compatible server is only as real-time as its model; see the VibeVoice
  page for a sub-real-time example and how it shows up in the metrics.

## Upstream

OpenAI audio API reference.
