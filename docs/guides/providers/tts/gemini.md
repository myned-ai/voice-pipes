# Gemini TTS (TTS, cloud)

**Status:** active
**Last verified:** 2026-09-27
**Owner:** Docs

## What it is

Google's Gemini text-to-speech through the generative-language REST API. A
prompt can steer the delivery style.

It is a plugin crate, `voice-pipes-tts-gemini`, built on `voice-pipes-sdk`
and `voice-pipes-net`; the engine contains no Gemini TTS code.

## Cargo feature

`tts-gemini` on the **CLI crate**:
`cargo build -p voice-pipes-cli --features tts-gemini`. Both worker images
carry it. (Until 2026-09 it was an engine feature; `-p voice-pipes
--features tts-gemini` no longer exists.)

## Config

```toml
[tts]
provider = "gemini"
model_name = "gemini-3.1-flash-tts-preview"
voice = "Kore"
# prompt = "Speak in a friendly, conversational tone."
# api_key = "…"                # else GOOGLE_API_KEY
```

## Environment

`GOOGLE_API_KEY`. Not `GEMINI_API_KEY`, which the Gemini *Live*
speech-to-speech provider reads; the two are read separately.

## Capabilities

| | |
|---|---|
| Output sample rate | 24 kHz |
| Streaming | One REST request per text fragment; audio is delivered to the pipeline in ~100 ms chunks, and the pipeline's prefetch overlaps the next fragment |
| Word timestamps | No |

## Limitations

- Per-fragment request latency is higher than a streaming socket; the
  per-turn `tts.ttfb` metric shows it.
- The default model id is a preview SKU and will change; it is a config
  value, and a test guards that an override reaches the request URL.

## Upstream

Gemini API speech generation documentation.
