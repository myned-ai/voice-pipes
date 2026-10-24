# AssemblyAI (STT, cloud)

**Status:** active
**Last verified:** 2026-09-27
**Owner:** Docs

## What it is

AssemblyAI's streaming speech-to-text (v3 API) over WebSocket. It runs its
own VAD and signals end-of-turn, which makes it a good fit for
`provider`-mode turn detection.

It is a plugin crate, `voice-pipes-stt-assemblyai`, built on
`voice-pipes-sdk` and `voice-pipes-net`; the engine contains no AssemblyAI
code. It stays the setup wizard's preselected recogniser in a binary that
links it.

## Cargo feature

`stt-assemblyai` on the **CLI crate**:
`cargo build -p voice-pipes-cli --features stt-assemblyai`. Both worker
images carry it. (Until 2026-09 it was an engine feature; `-p voice-pipes
--features stt-assemblyai` no longer exists.)

## Config

```toml
[stt]
provider = "assemblyai"
model_name = "universal-streaming-english"   # or universal-streaming-multilingual, universal-3-5-pro; u3-rt-pro still accepted
sample_rate = 16000

[stt.options]
# min_turn_silence = 400                 # ms before a turn may end
# max_turn_silence = 1200                # ms before a turn is forced to end
# end_of_turn_confidence_threshold = 0.7
# format_turns = true
# keyterms_prompt = ["Voice Pipes", "Kokoro"]   # up to 100 terms, 50 chars each
# prompt = "Technical conversation about audio software."
# language_detection = false
# max_reconnect_attempts = 5             # consecutive failures before the session gives up
# reconnect_delay_ms = 1000              # first backoff; doubles up to reconnect_delay_max_ms
# reconnect_delay_max_ms = 30000
# persistent_session = false             # keep one session across utterances

[vad]
mode = "passthrough"                     # AssemblyAI does its own VAD

[turn_detection]
mode = "provider"
```

## Environment

`ASSEMBLYAI_API_KEY` (a literal `api_key` in the file also works).

## Capabilities

| | |
|---|---|
| Input sample rate | 16 kHz |
| Streaming | Yes, interim results |
| Word timestamps | Yes |
| End-of-turn signal | Yes (`is_end_of_turn`), tunable with the silence and confidence options |
| Language detection | Optional |
| Custom vocabulary | `keyterms_prompt` and free-form `prompt` |

## Reconnects

A session that drops with a transient error (network, a server-side
error frame, the concurrency cap) is reopened with exponential backoff
(`reconnect_delay_ms`, doubling up to `reconnect_delay_max_ms`). The budget
is bounded in two ways:

- `max_reconnect_attempts` counts **consecutive** failures. A session that
  lived at least 10 seconds or delivered a transcript resets the count; a
  session that was accepted (`Begin`) and then rejected on the first audio
  frame does not, so a deterministic rejection surfaces as an error after
  the configured attempts instead of looping.
- At most 20 reconnects in any five-minute window, whatever the sessions
  in between looked like.

When either bound is hit the worker ends with `ReconnectExhausted` (a
permanent error); the pipeline's own worker restart then applies its
backoff. The WebSocket connect itself is bounded to 10 seconds and gives
up at once on session teardown.

## Limitations

- **Concurrency limits are account limits.** Hitting the streaming
  concurrency cap returns a structured 1008 error, which the provider now
  classifies as capacity (not auth) and backs off with a capped delay
  instead of reconnecting in a tight loop. By default each utterance gets
  its own session, opened when its audio arrives and terminated when it
  ends, so none is open between turns; `persistent_session = true` keeps
  one open and sends `ForceEndpoint` at end of utterance instead, reducing
  connection churn at the cost of an idle session.
- Every session is terminated or given a deadline on every exit path, so a
  worker teardown cannot leak sessions on the account; a hard process kill
  still leaves sessions to expire server-side. Teardown is honoured within
  the 5-second termination deadline even while audio waits on a server
  that has stopped reading, or a transcript waits on a pipeline that has
  stopped taking them.
- Billing is per second of streamed audio; a live session streams
  continuously.

## Upstream

AssemblyAI streaming API reference. The provider validates `model_name`
against the ids that reference lists (`universal-3-5-pro`,
`universal-streaming-english`, `universal-streaming-multilingual`) plus the
older aliases `u3-rt-pro` and `whisper-rt`.
