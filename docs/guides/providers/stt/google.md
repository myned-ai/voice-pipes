# Google Cloud Speech-to-Text v2 (STT, cloud)

**Status:** active
**Last verified:** 2026-09-26
**Owner:** Docs

## What it is

Google's Speech-to-Text v2 `StreamingRecognize` over gRPC with TLS. The
`chirp_3` model covers 100+ languages in streaming, including ones no other
provider here handles (Croatian was the motivating case). Google performs
its own VAD and emits voice-activity events, which drive end-of-turn.

It is a plugin crate, `voice-pipes-stt-google`, built on `voice-pipes-sdk`;
the engine contains no Google code. The `voice-pipes` binary links it behind
a feature and registers it at start, so configs select it exactly as before.

## Cargo feature

`stt-google` on the **CLI crate**:
`cargo build -p voice-pipes-cli --features stt-google`. Both worker images
carry it. (Until 2026-09 it was an engine feature; `-p voice-pipes
--features stt-google` no longer exists.)

## Config

```toml
[stt]
provider = "google"                 # aliases: google-stt, gcp-speech, chirp
model_name = "chirp_3"              # chirp_3 | chirp_2 | long | short | telephony
sample_rate = 16000                 # 8000..=48000

[stt.options]
# project_id = "my-gcp-project"     # else GOOGLE_CLOUD_PROJECT, else the credential's
location = "eu"                     # chirp_3 streams from "us" and "eu" only
language_codes = ["hr-HR"]          # BCP-47; Chirp 3 accepts several
interim_results = true
enable_voice_activity_events = true # SPEECH_ACTIVITY_BEGIN/END → speech_started / end-of-turn
enable_automatic_punctuation = true
enable_word_time_offsets = false    # true → per-word timestamps
# enable_word_confidence = false
# profanity_filter = false
# endpointing_sensitivity = "standard"   # standard | short | supershort
# speech_start_timeout_ms = 5000
# speech_end_timeout_ms = 700
# credentials_path = "models/gcp/service-account.json"   # else Application Default Credentials
# stream_rotate_ms = 240000         # reopen before Google's 5-minute cap (must be < 300000)
# persistent_session = false        # one stream across utterances; needs continuous audio
# max_reconnect_attempts = 5        # consecutive failures before the session gives up
# reconnect_delay_ms = 1000         # first backoff; doubles up to reconnect_delay_max_ms
# reconnect_delay_max_ms = 30000

[vad]
mode = "passthrough"

[turn_detection]
mode = "provider"
```

## Environment

No API key. Authentication is Application Default Credentials: `gcloud auth
application-default login`, or `GOOGLE_APPLICATION_CREDENTIALS` pointing at a
service-account key, or the GCE/Cloud Run metadata server, or a logged-in
`gcloud` on `PATH`. `credentials_path` in the config overrides them.
`GOOGLE_CLOUD_PROJECT` names the project when the credential does not.
Enable `speech.googleapis.com` on the project first. In the Docker stack,
put a key under `models/` (mounted at `/app/models`) and reference it with a
relative path.

## Capabilities

| | |
|---|---|
| Input sample rate | configurable, 16 kHz recommended |
| Streaming | Yes, interim results |
| Word timestamps | With `enable_word_time_offsets` |
| End-of-turn signal | With `enable_voice_activity_events` (default on) |
| Language detection | Several `language_codes` at once |
| Built-in VAD | Yes |

## Reconnects

A stream that fails with a transient status (`UNAVAILABLE`,
`DEADLINE_EXCEEDED`, `RESOURCE_EXHAUSTED`, a dropped connection) is
reopened with exponential backoff, and audio Google had not acknowledged
is replayed on the new stream. The budget is bounded:

- `max_reconnect_attempts` counts **consecutive** failures. A stream that
  lived at least 10 seconds or delivered a transcript resets the count; a
  stream Google accepted and then failed on the first audio message does
  not, so a misconfiguration that only shows up after connect ends the
  session with an error instead of looping.
- At most 20 reconnects in any five-minute window.

When either bound is hit the worker ends with `ReconnectExhausted` (a
permanent error). Credential fetches are bounded to 30 seconds, and every
wait (connect, credentials, a server that accepts the call but neither
reads nor answers, a pipeline that has stopped taking transcripts) is
abandoned at once on session teardown. Audio the
server is not accepting queues locally up to the replay window and then
sheds its oldest part (logged once per session) rather than growing without
bound. An utterance
that ends while the previous stream is still draining is carried whole
into the next stream and half-closed there immediately.

## Limitations

- **Five-minute streams.** Google closes a stream at 5 minutes; the provider
  rotates proactively at 4 (`stream_rotate_ms`), preferring a moment with no
  speech active. Per-utterance streams (the default) rarely reach it.
- `chirp_3` is served only from the `us` and `eu` multi-regions; elsewhere it
  fails at connect, so the location is validated up front.
- The server withholds response headers until the first message, so a
  misconfigured stream fails on first audio, not on connect.
- Chirp bills per 15-second increment of audio; a live session streams
  continuously.
- `persistent_session` only makes sense with continuous audio (passthrough
  VAD): Google times out a stream that goes quiet.

## Upstream

Google Cloud Speech-to-Text v2 documentation; the Chirp 3 model page for
language coverage and regions.
