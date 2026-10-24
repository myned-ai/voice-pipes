# ElevenLabs TTS (TTS, cloud)

**Status:** active
**Last verified:** 2026-09-27
**Owner:** Docs

## What it is

ElevenLabs' streaming text-to-speech over REST
(`/v1/text-to-speech/{voice}/stream`), with the `with-timestamps` variant
that returns per-character timing. The first TTS provider here with word
timestamps, which drive exact karaoke captions instead of estimated ones.
Distinct from the [ElevenLabs Agents](../s2s/elevenlabs-agents.md)
speech-to-speech provider; the same API key serves both.

It is a plugin crate, `voice-pipes-tts-elevenlabs`, built on
`voice-pipes-sdk` and `voice-pipes-net`; the engine contains no ElevenLabs
TTS code. Word timestamps and the live voice work as before.

## Cargo feature

`tts-elevenlabs` on the **CLI crate**:
`cargo build -p voice-pipes-cli --features tts-elevenlabs`. Both worker
images carry it. (Until 2026-09 it was an engine feature; `-p voice-pipes
--features tts-elevenlabs` no longer exists.)

## Config

```toml
[tts]
provider = "elevenlabs"
model_name = "eleven_flash_v2_5"
voice = "21m00Tcm4TlvDq8ikWAM"          # a voice id; this one is the public "Rachel"

[tts.options]
# word_timestamps = true               # default on; uses the with-timestamps endpoint
# language_code = "en"
# stability = 0.5
# similarity_boost = 0.75
# style = 0.0
# use_speaker_boost = true
# speed = 1.0
# sample_rate = 24000
# request_timeout_ms = 30000
```

## Environment

`ELEVENLABS_API_KEY` (a literal `api_key` also works).

## Capabilities

| | |
|---|---|
| Output sample rate | 24 kHz PCM |
| Streaming | Yes |
| Word timestamps | Yes, aligned from the character timings; captions highlight exactly |
| Voices | any voice id on the account |

## Limitations

- Per-character billing.
- The timestamp endpoint carries a small overhead versus plain streaming;
  turn `word_timestamps` off if captions do not matter.
- Voice ids are opaque strings; the wizard defaults to the public Rachel
  voice.

## Upstream

ElevenLabs text-to-speech API reference.
