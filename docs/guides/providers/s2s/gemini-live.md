# Gemini Live (speech-to-speech)

**Status:** active
**Last verified:** 2026-09-30
**Owner:** Docs

## What it is

Google's Gemini Live API: native-audio Gemini models that take audio in and
speak back over a bidirectional WebSocket, with server VAD and function
calling. Voice Pipes drives it as a speech-to-speech engine.

## Cargo feature

`s2s-gemini` on the **CLI crate**, where it is a default feature, so
`cargo install` and both worker images carry it. The provider lives in its
own crate, `voice-pipes-s2s-gemini`, registered by the CLI at startup; it
needs the engine's speech-to-speech client, `realtime`, which the feature
turns on. (Until 2026-09 the engine's `realtime` feature built it in.)

## Config

```toml
[realtime_provider]
provider = "gemini-live"
model = "gemini-3.8-live"
voice = "Puck"
vad = "server_vad"
api_key_env = "GEMINI_API_KEY"
# The reply language can only be set in the prompt; see "Reply language".
instructions = "You are a concise voice assistant. RESPOND IN ENGLISH. YOU MUST RESPOND UNMISTAKABLY IN ENGLISH, even if the user's speech sounds like another language or you are unsure what they said. If you did not understand, ask them in English to repeat."
# Configure.options keys a client may override (default: the vad_* knobs).
# Add "instructions" to let clients APPEND to the prompt; nothing replaces it.
# client_overrides = ["vad_threshold", "vad_silence_ms", "vad_prefix_padding_ms", "vad_eagerness"]

[realtime_provider.options]
# Thinking: see "Models" below. gemini-3.8-live takes none.
# thinking_budget = 0          # 2.5 native-audio only
# thinking_level = "minimal"   # 3.1 Flash Live, 3.8 Live Extended Thinking
# The language the caller is transcribed in (BCP-47; several may be listed,
# comma-separated or as an array). Default "en-US"; "auto" lets Gemini
# detect per utterance.
# input_transcription_language = "en-US"
```

## Environment

`GEMINI_API_KEY`. Note this is a different variable from `GOOGLE_API_KEY`,
which the Gemini *TTS* provider reads.

## How to run

### 1. Terminal voice loop (`interactive`)
```sh
export GEMINI_API_KEY="your-gemini-api-key"
voice-pipes --config examples/live/gemini-live.toml interactive
```

### 2. Browser & Playground (`serve`)
```sh
voice-pipes --config examples/live/gemini-live.toml serve \
  --worker-config examples/orchestrate/worker-standalone.toml
```
Open `http://localhost:3000` to interact with live waveform visualizations, metrics, and karaoke transcripts in the Admin Console.

## Models

| Model | Thinking option | Async tool calls (`NON_BLOCKING`, `scheduling`) |
|---|---|---|
| `gemini-3.8-live` (recommended) | none | yes, by default |
| `gemini-3.8-live-extended-thinking` | `thinking_level` = low / medium / high | not by default (it rejects `scheduling`) |
| `gemini-3.1-flash-live-preview` | `thinking_level`, `"minimal"` is fastest | no (synchronous only) |
| `gemini-2.5-flash-native-audio-*` | `thinking_budget = 0` | yes, by default |

`thinking_budget` is only sent to 2.x models; on a 3.x model the provider
logs a warning and leaves it out. `async_function_calling = true | false`
overrides the tool-call column.

Prefer `gemini-3.8-live`. The 2.5 native-audio models drop whole spans of
clear speech from the user transcript: a clearly spoken sentence came back
as `" ."` while the model itself understood and answered it. Replaying the
same audio gave the same result on all three 2.5 versions, with or without a
language hint, and a correct transcript on `gemini-3.8-live`. Google lists
3.8 as the replacement for 3.1 Flash Live and the 2.5 native-audio models.

## Capabilities

- Server VAD and input/output transcription.
- Tool calling: client-side tools from the config, server-side through the
  agent link. See [Tool calling](../../tool-calling.md).
- Barge-in with transcript truncation at the played offset.
- Per-turn metrics anchored on vp's own end-of-speech detection.

## Running gemini-3.8-live

### What vp handles for you

Nothing below needs configuring; it is listed so the behaviour is not a
surprise.

- **Audio framing.** Audio goes out as `realtimeInput.audio`. 3.8 accepts
  the older `mediaChunks` framing without an error and then never hears it.
- **No thinking configuration** is sent to 3.8, which takes none.
- **Tool calls run non-blocking.** The model keeps the conversation going
  while a client tool runs. A tool with `speak_on_result = false` stays
  silent; one with `true` is spoken about once the client reports its
  result.
- **Tool-call turns.** 3.8 answers a request like "open the settings" with
  a tool call and nothing else, then speaks any follow-up in a separate
  turn. vp gives the tool call a turn of its own, so the session history
  keeps what the user said and which tool it triggered.

### Latency to expect

From the end of the caller's speech to the first reply audio at the client,
Playground sessions on 3.8 measured roughly **0.8–1.4 s**, and the first
turn of a session slower. The session report grades speech-to-speech
sessions against a 1200 ms target for both the end of speech and the first
audio unless the pipeline's `[grading]` says otherwise, so an ordinary 3.8
turn grades fair at worst; judge it against your own expectation of a
conversational reply.

**When Gemini does not end the turn.** Gemini decides when the caller has
finished; vp's own uplink VAD only measures. Once in a test Gemini left a
clean request followed by silence unanswered for 21 s (nothing in the
worker log, audio flowing the whole time) until the caller spoke again,
and since the caller's transcript arrives with the turn's end, the text
appeared late too. Two options make it close turns sooner, and the wizard
now writes them for Gemini:

```toml
[realtime_provider.options]
end_sensitivity = "high"   # endOfSpeechSensitivity
vad_silence_ms = 700       # silenceDurationMs; also a client override
```

With them the next session's tool call came 4 s after the end of speech and
every spoken turn closed within about a second. If Gemini starts cutting
callers off mid-sentence, raise `vad_silence_ms` first.

For a spoken tool result, the wait also contains the client's own tool time
(a screen loading, media starting). The report separates the two: the tool
round-trip is shown on the tool-call turn, and the reply's latency is
measured from the tool result.

### The user transcript

The words shown for the caller come from Gemini's own transcription, which
is separate from what the model understands. Plan around these properties:

- **It arrives with the reply**, in one piece, not while the caller speaks.
  A client cannot show a live, word-by-word caption of the caller.
- **It can differ from what the model understood.** The model can answer a
  question correctly while the transcript shows other words. This is widely
  reported for Gemini Live, not specific to vp.
- **Short utterances are misheard most.** "Who made you?" came back as
  "promedio", "whole media" or "ho medio" on replay; "I would say no" once
  came back as a different question. Longer sentences were transcribed
  correctly.
- **It is occasionally missing** for an utterance the model did answer.

If the transcript matters (analytics, compliance, a visible chat log), take
it from a separate recogniser rather than from the model.

### Reply language

The native-audio models have no reply-language setting
(`speechConfig.languageCode` is not supported), so state it as a hard rule
in `instructions`, without an "unless the user asks" exception. With "Speak
English unless the user explicitly asks for another language",
`gemini-3.8-live` answered a misheard English turn in Portuguese four times
out of four; with the wording in the config example above it answered in
English every time. For another language, name it in the same form
("RESPOND IN CROATIAN. …") and set `input_transcription_language` to match.

A hard rule fixes the language of the reply, not the recognition: a misheard
question is still misheard, but the model asks for clarification in the
right language.

### Interruptions and echo

- **Barge-in works.** Talking over a reply stopped playback 0.2–0.4 s after
  the caller started, and the model answered the new question rather than
  resuming the old one.
- **The model may finish its thought.** Gemini keeps in its history what it
  had *sent*, which is more than the caller heard, because audio arrives
  faster than it plays. If the interruption invites it ("I don't know that
  one" in the middle of a joke), the model delivers the rest.
- **Use headphones or echo cancellation.** Proactive audio is always on in
  3.8 and cannot be disabled, and the model's own voice picked up by the
  microphone can interrupt it. Google's guidance is to use headphones.

### Reading the session report

- On 3.8, turns are timed from vp's end-of-speech detection
  (`ttfb_anchor_source: vp_vad`), and `provider_endpointing_ms` is not
  reported: Gemini's transcript arrives with the reply, so it cannot say
  when the caller stopped.
- A request answered only by a tool call is a *tool-call leg* with no
  latency of its own; the spoken result after it is its *follow-up*, timed
  from the tool result.
- Sessions recorded before these measurements changed keep the numbers they
  were recorded with.

### What did not help

- **`input_transcription_language`.** The hint is sent, but Google does not
  document which Live models honour it, and it made no measurable difference
  to transcript accuracy in our tests.
- **`vad_prefix_padding_ms`.** Raising it to 400 ms fixed a misheard short
  question when the audio was sent straight to Google, but not through a
  live session, and it made another short phrase worse. Leave it unset.

### Testing with recorded or synthetic audio

3.8 is slow to notice the end of speech when the audio after it is exact
digital silence (all-zero samples). A scripted caller that pads with zeros
measures longer waits than a real microphone, whose background noise never
reaches zero. Pad with low-level noise, or test with a real microphone.

## Limitations

- **No explicit speech-stopped event.** Gemini does not send one, so vp runs
  its own end-of-speech observer on the uplink (`[realtime_provider.endpoint_observer]`,
  on by default) to anchor the metrics honestly. It never gates or delays
  audio. Keep it on for 3.8: without it, turns have no end-of-speech anchor.
- **First-audio buffering (2.5).** Gemini 2.5 flushes its opening response
  as one 640–960 ms audio blob after the opening transcription, then 40 ms
  pieces; its first audio reached the client 1.5–2.0 s after the true end
  of speech.
- **Thinking delay (2.5).** Leave `thinking_budget = 0` for conversational
  use; dynamic thinking adds an unpredictable pause before speech.
- **Language hint.** The provider sends `en-US` as
  `inputAudioTranscription.languageCodes` unless
  `input_transcription_language` says otherwise (`"auto"` sends none).
- Uplink chunk size is not a lever: larger client frames were consistently
  slower to reach Gemini's end-of-speech than the browser's small frames.
- **Session resumption.** vp turns resumption on in every session, so
  Gemini keeps issuing tokens, and a reconnect presents the latest: the
  conversation carries over (both 2.5 native audio and 3.8 remember what
  was said before the reconnect). A token Gemini no longer holds is
  refused at setup (1007 "Invalid session handle" on 2.5, 1008 "Requested
  entity was not found." on 3.x, and 1008 "BidiGenerateContent session not
  found or expired" when the server itself dropped the previous connection
  with a 1011 internal error). vp reads that close, or any close of a
  resumed connection before `setupComplete`, as a refusal: it starts a
  fresh session and tells the client its context was reset.
- **`goAway`.** Gemini says a connection is closing before its lifetime
  runs out (2.5 native audio: 540 s into a connection, "50s" left). vp
  moves the session to a resumed connection at the next quiet moment (no
  response, no turn, nothing queued), so the close cuts nothing and the
  client sees no warning. In probes Gemini did not close a quiet connection
  at the stated time, and 3.8 sent no `goAway` in 25 minutes.
- **Tool calls withdrawn by barge-in.** When the caller speaks over a
  pending call, 2.5 native audio withdraws it (`toolCallCancellation`) and
  usually calls again with the new context; a server tool that finishes
  after the withdrawal has its result dropped rather than answered. 3.8 was
  not seen to withdraw calls.
- Frame, audio-field and tool-argument caps, the 60 s liveness check and
  the reconnect budget are shared by every realtime provider; see
  [OpenAI Realtime › Connection limits and reconnects](openai-realtime.md#connection-limits-and-reconnects).
  Client-supplied `vad_prefix_padding_ms` and `vad_silence_ms` are clamped
  to 5000 ms before they reach Gemini.

## Upstream

- [Gemini 3.8 Live model page](https://ai.google.dev/gemini-api/docs/models/gemini-3.8-live)
- [Live API capabilities](https://ai.google.dev/gemini-api/docs/live-api/capabilities)
  and [tool use](https://ai.google.dev/gemini-api/docs/live-api/tools)
- [Live API reference](https://ai.google.dev/api/live)
- [Google's Live API examples](https://github.com/google-gemini/gemini-live-api-examples)
