# ElevenLabs Agents — Realtime S2S Provider

Status: Implemented 2026-07-12, live-verified 2026-07-13
Owner: Backend
Related: [protocol reference](../../../clients/protocol.md) (client-side tool policy)

How voice-pipes integrates ElevenLabs' realtime speech-to-speech offering as a
third `[realtime_provider]` backend next to OpenAI Realtime and Gemini Live.

---

## 1. What "ElevenLabs S2S" actually is

ElevenLabs does not ship a standalone fused speech-to-speech model. Their
realtime speech-in/speech-out product is **ElevenLabs Agents** (formerly
"Conversational AI" / convai): a hosted agent — cascaded ASR → LLM → TTS behind
one WebSocket:

```
wss://api.elevenlabs.io/v1/convai/conversation?agent_id=<id>
```

This shapes the whole integration. Unlike OpenAI/Gemini, the conversational
brain — persona prompt, LLM choice, voice, VAD/turn-taking, and **tool
definitions** — lives in the *agent's* configuration (elevenlabs.io → Agents),
not in our TOML. The socket is a thin conversation surface: audio in, audio +
transcripts out, tool calls, interruptions.

| Concern            | OpenAI / Gemini                  | ElevenLabs                            |
|--------------------|----------------------------------|---------------------------------------|
| System prompt      | our `instructions`               | agent dashboard (override = opt-in)   |
| Model / voice      | our `model` / `voice`            | agent dashboard (override = opt-in)   |
| VAD / turn-taking  | our `vad` + tuning options       | agent dashboard                       |
| Tool definitions   | declared over the socket per session | agent dashboard (names must match) |
| Tool execution     | our runner / client              | our runner / client (client tools)    |

## 2. Quick start

1. Create an agent at elevenlabs.io → Agents. In its settings:
   - set input/output audio format to a **PCM** rate (default `pcm_16000` is fine);
   - add Client tools matching your `[[client_tool]]` names (§6);
   - enable any overrides you plan to send (Security tab, §5.2).
2. Set `ELEVENLABS_API_KEY` (not needed for public agents).
3. Point a worker at it — minimal config:

```toml
[realtime_provider]
provider    = "elevenlabs"          # aliases: elevenlabs-agents, 11labs
vad         = "server_vad"          # turn-taking runs on ElevenLabs' side
api_key_env = "ELEVENLABS_API_KEY"

[realtime_provider.options]
agent_id = "agent_XXXXXXXXXXXXXXXX"
```

Full annotated example: [`config/worker-realtime-elevenlabs.toml`](../../../../voice-pipes/config/worker-realtime-elevenlabs.toml).
The config wizard (`launch-stack.ps1` / `.sh` → S2S mode → "ElevenLabs Agents")
generates the same config and stores the API key; the launchers load stored
keys from `voice-pipes/.env` and docker-compose passes `ELEVENLABS_API_KEY`
into every worker service. The terminal client also works:
`voice-pipes interactive --provider elevenlabs --config <toml-with-agent-id>`.

## 3. Code map

| Piece | File |
|---|---|
| Protocol (envelopes + event parsing) | `voice-pipes-s2s-elevenlabs/src/lib.rs` |
| Factory (config → protocol, validation) | `voice-pipes-s2s-elevenlabs/src/factory.rs` (`ElevenLabsFactory`) |
| JSON ping→pong keepalive hook | the SDK's `RealtimeProtocol::build_auto_reply` + the engine's `src/realtime/client.rs` reader |
| Wizard menu entry | `voice-pipes-s2s-elevenlabs/src/wizard.rs` |
| Terminal client | `voice-pipes/src/commands/interactive.rs` |
| Tests | `voice-pipes-s2s-elevenlabs/src/tests/` |

The crate is a plugin on the SDK (`voice_pipes_sdk::s2s`); the CLI feature
`s2s-elevenlabs` (a default) links and registers it.

Everything else (WS lifecycle, resampling, chunk coalescing, the runner's turn
accounting, client-tool dispatch) is the shared realtime stack — the provider
is a pure wire-protocol mapper under the realtime engine's
protocol/client split.

## 4. Wire protocol mapping

### 4.1 Outbound (us → ElevenLabs)

| RealtimeClient call | Envelope |
|---|---|
| `apply_session_config` | `conversation_initiation_client_data` (+ opt-in overrides, §5.2) |
| `send_audio(pcm)` | `{"user_audio_chunk": "<b64 pcm16>"}` |
| `send_text(text)` | `{"type":"user_message","text":...}` |
| `send_function_output(...)` | `{"type":"client_tool_result","tool_call_id":...,"result":...,"is_error":false}` |
| `send_context_message(text, respond=false)` | `{"type":"contextual_update","text":...}` (never triggers speech) |
| `send_context_message(text, respond=true)` | `user_message` with a `[background context — not spoken by the user]` prefix (triggers a spoken response) |
| `create_response()` / `cancel()` / `truncate_item()` | none — the agent auto-responds, barge-in is server-side (§4.4), no per-item history truncation |

### 4.2 Inbound (ElevenLabs → us)

| ElevenLabs event | RealtimeEvent |
|---|---|
| `conversation_initiation_metadata` | `SessionUpdated` + records negotiated audio rates (§4.5) |
| `audio` | `ResponseStarted` (first of a turn) + `ResponseAudioDelta` (incl. word-level `alignment`, see below) |
| `agent_response` | `ResponseStarted` (first of a turn) + one `ResponseTextDelta` (full turn text arrives as a block, not a token stream) |
| `user_transcript` | `InputTranscript { is_final: true }` (+ closes the open agent turn, §4.3) |
| `interruption` | `ResponseCancelled` (+ arms the stale-audio filter, §4.4) |
| `client_tool_call` | `FunctionCall { call_id, name, arguments }` |
| `client_error` | `Error { recoverable: true }` (fatal conditions arrive as a WS close frame → `Closed`) |
| `ping` | none — auto-replied with `{"type":"pong","event_id":N}` via the reader-task hook |
| `agent_chat_response_part`, `vad_score`, `internal_*` previews, `agent_response_correction`, `agent_tool_*`, `mcp_*`, `guardrail_triggered` | ignored (`agent_response` is the single authoritative transcript source — surfacing the streamed chat parts too duplicates turn text; the rest are telemetry with no pipeline mapping) |

**Alignment**:
`audio_event.alignment` (char-level speech timing, chunk-relative) is
grouped into word timings and rides `ResponseAudioDelta`. The runner uses
it for **word-exact barge-in truncation** (`RealtimeTurn::aligned_words` →
`truncate_aligned`, replacing the all-or-nothing single-entry timeline
ElevenLabs would otherwise get) and emits it to clients as
`CaptionMeta.word_timings` — the same contract the cascade TTS path uses,
so karaoke highlighting is exact. The admin SDK evicts estimate-seeded
timing tables when the first authoritative caption arrives and suppresses
duplicate caption text for text-delta-fed transcripts.
`agent_response_correction` is logged (ElevenLabs' own truncation view vs
our PlaybackReport-based one) for cross-checking, not used for truncation.

### 4.3 Turn boundaries are inferred (important quirk)

The docs list `agent_response_complete` and a final-audio flag, but the
**production socket sends neither** (live-verified 2026-07-13; without a close
signal every agent utterance accumulated into one never-ending turn and no
turn records were persisted). Both events are still honoured if they ever
appear, but `ResponseCompleted` is normally *inferred* from the strictly
turn-based flow:

- **`user_transcript` closes the open agent turn** — the user spoke next
  (voice flow; a mid-speech barge-in arrives as `interruption` instead).
- **A second `agent_response` closes the open turn and starts the next one.**
  The full turn text arrives exactly once per turn, so a repeat means a new
  turn. This is the only closer for *typed* input: `user_message` turns
  produce no `user_transcript`.
- Audio and text of the same turn arrive in either order without splitting
  the turn (tracked by a `response_has_text` flag).
- **Idle timeout** — the primary closer in practice: the protocol declares
  `RealtimeProtocol::inferred_completion_idle_ms` (default 1500 ms, option
  `completion_idle_ms`) and the runner treats a response with no new
  audio/text/tool-call for that long as completed. Without it the *final*
  reply of an exchange never closes: the session sticks in "speaking", the
  turn only persists at teardown, and queued agent events don't drain.
  Raise the window if long inter-sentence generation gaps split a reply
  into two turns; lower it for snappier end-of-turn detection.

Backstop: the runner still flushes a still-open turn at session teardown
(`flush_open_turn(terminated=true)` in
`realtime_runner/context/provider_handlers.rs`) so nothing is lost if the
socket dies inside the idle window.

**Re-delivery guards.** Live-observed: the server can re-stream a response's
content mid-conversation (a turn's transcript *and* audio arrived twice).
Because a duplicate must not be mistaken for a new turn, the protocol drops:

- an `agent_response` whose text equals the open response's already-delivered
  text, provided no new user input occurred in between (the memory clears on
  `user_message`, `user_transcript`, interruption, and turn close — so a
  legitimately repeated identical answer to a repeated question passes);
- audio events whose `event_id` goes *backwards* (strictly-less-than, so
  equal-id chunk trains are unaffected);
- `agent_chat_response_part` entirely (same text as `agent_response`).

### 4.4 Barge-in / interruption

There is no client→server cancel. We keep uplinking mic audio continuously;
ElevenLabs' server VAD detects overlap and emits `interruption`. On that we:

1. emit `ResponseCancelled` (runner flushes client playback as usual), and
2. record the interruption's `event_id` — **audio events with
   `event_id <=` that id are dropped**. They are stale in-flight chunks of the
   interrupted response; the official SDKs apply the same filter to prevent
   "audio ghosts" after a barge-in.

Provider-side history truncation (`conversation.item.truncate`-style) does not
exist; `agent_response_correction` tells us what ElevenLabs itself truncated
the turn text to, and is currently ignored.

### 4.5 Audio formats

Formats are fixed per *agent*, not per session. The handshake reply
(`conversation_initiation_metadata`) reports `user_input_audio_format` /
`agent_output_audio_format`; the protocol stores both in atomics. The engine
reads the input rate before each uplinked frame and converts the microphone
to it as one stream, starting over when the rate changes, so an agent's
format applies from the frame after the handshake (until then it assumes the
ElevenLabs default, `pcm_16000`). Downlink audio carries its rate with each
delta and is resampled to `output_emit_rate` (default 16 kHz) as one stream. `ulaw_8000`
(telephony) agents are rejected with a fatal, actionable error — this pipeline
only decodes PCM. The announced rate sizes resampler buffers, so only
8000, 16000, 22050, 24000, 44100 and 48000 Hz are accepted; any other
`pcm_*` value is rejected the same way.

### 4.6 Keepalive

ElevenLabs sends an application-level JSON `ping` (distinct from WS ping
frames) that must be echoed as `pong` with the same `event_id`. Since protocol
impls are pure parsers, this is handled by a dedicated trait hook —
`RealtimeProtocol::build_auto_reply` — which the shared reader task calls for
every frame, writing the reply straight back through the writer queue
(loss-tolerant `try_send`; a full queue means the socket is stalled anyway and
the server re-pings). Independently of that, the shared client sends a
WebSocket ping every 20 s and treats 60 s without any frame as a closed
connection; the frame and field caps and the reconnect budget are described in
[OpenAI Realtime › Connection limits and reconnects](openai-realtime.md#connection-limits-and-reconnects).

## 5. Configuration reference

### 5.1 `[realtime_provider]`

| Key | Meaning for ElevenLabs |
|---|---|
| `provider` | `"elevenlabs"` (aliases `elevenlabs-agents`, `11labs`) |
| `model` | optional `agent.prompt.llm` override (e.g. `"gpt-4o"`) — needs the LLM override enabled agent-side |
| `voice` | optional `tts.voice_id` override — needs the Voice override enabled agent-side |
| `vad` | informational; turn-taking always runs on ElevenLabs' side (use the agent's VAD settings to tune) |
| `instructions` | **not sent by default** — see `override_prompt` below |
| `api_key_env` | default `ELEVENLABS_API_KEY` |

### 5.2 `[realtime_provider.options]`

| Option | Effect |
|---|---|
| `agent_id` (required) | the agent to converse with |
| `public_agent = true` | connect without an API key (public agents): no key is sent even when one is set, and the validate-time key check is skipped |
| `endpoint` | regional base, e.g. `"wss://api.eu.residency.elevenlabs.io"` |
| `output_emit_rate` | the rate the engine resamples the agent's audio to, 8000–48000 (default 16000); applies to every provider |
| `completion_idle_ms` | inferred end-of-response idle window, 200–10000 (default 1500) — see §4.3 |
| `override_prompt = true` | send merged `instructions` (+ knowledge base + client-tool convention) as the agent's prompt override |
| `language` | `agent.language` override |
| `first_message` | `agent.first_message` override |
| `dynamic_variables` (table) | fills `{{placeholders}}` in the agent's prompt |
| `user_id` | attached to the conversation initiation |

**Override gating.** ElevenLabs rejects any `conversation_config_override`
field not explicitly enabled in the agent's **Security** tab, so we only send
deliberately-set values. `instructions` is opt-in (`override_prompt = true`)
rather than presence-based because the runner auto-appends client-tool
guidance to otherwise-empty instructions — non-empty does not mean the
operator wants an override. With `override_prompt = true` the dashboard
prompt is **replaced**, so manage the whole persona from TOML or not at all.

### 5.3 Authentication

Server-side connections send the key as an `xi-api-key` header on the WS
upgrade. Public agents connect anonymously: with `public_agent = true` no key
is sent, even when `ELEVENLABS_API_KEY` is set. Config validation requires the
key env var unless `public_agent = true`. Key delivery paths: shell env →
wizard-stored `voice-pipes/.env` (loaded by the launch scripts, shell wins) →
docker-compose passthrough on every worker service.

## 6. Tool calling

Boundary rule (the same as for every provider): vp never executes a
tool; client tools go to the client, server tools to the registered handler.
The ElevenLabs twist: **tool definitions cannot be registered over the
socket.** Define tools on the agent (dashboard → Tools, type **Client**) with
names *exactly* matching (case-sensitive) your `[[client_tool]]` entries /
server tool registry. `build_session_update` logs the advertised names per
session so a mismatch is diagnosable from the worker log. (For which other
engines/processors support client tools, see `specs/05-TEXT-PROCESSORS.md`
§ "Client-Side Tools — Support Matrix".)

Flow:

1. The agent's LLM decides to call a tool → socket delivers
   `client_tool_call` → our `FunctionCall` event.
2. The runner checks the client-tool allowlist first; on match it immediately
   pushes a `trigger_action` `PipelineMessage` to the client
   (`{call_id, name, arguments, speak_on_result}`) — this is the earliest
   "agent recognised the intent" signal ElevenLabs exposes, and it also lands
   in the session audit as a `client_tool_call` trace event. Names not on the
   allowlist fall through to the server tool registry.
3. The provider is acked instantly with the standard dispatch payload
   (`"status": "dispatched_to_client"` + do-not-narrate instruction).
4. The client reports `ClientToolResult`; the result is injected as context —
   `contextual_update` (silent) or a background-tagged `user_message` (spoken).

Tools hosted on ElevenLabs' own side (webhooks / MCP) execute entirely on
their infrastructure; the socket's `agent_tool_request` / `agent_tool_response`
notifications are currently ignored (candidates for a future `tool_status`
message if visibility is needed).

### 6.1 Controlling audio around a tool call

| Stage | Lever | Where |
|---|---|---|
| Result spoken? (default: silent) | `speak_on_result = true` per `[[client_tool]]` | our TOML |
| Result spoken? (per call) | `speak` flag on the client's `ClientToolResult` | client event |
| Narration *while* calling | enable **Wait for response** on the agent tool → our instant ack carries the do-not-narrate instruction; plus put the standing silence rule in the agent's dashboard prompt (the auto-appended convention only reaches ElevenLabs with `override_prompt = true`) | ElevenLabs dashboard |

Unlike OpenAI (withheld response kick) and Gemini (`scheduling: SILENT`),
ElevenLabs has no per-result protocol-level silence flag — invocation-time
silence is prompt-enforced; result-time audio is fully deterministic on our
side.

## 7. Testing & troubleshooting

- Unit tests pin every envelope/parse shape and the boundary inference:
  `cargo test -p voice-pipes-s2s-elevenlabs`.
- Wire tracing: `RUST_LOG=voice_pipes::realtime::client=trace` logs every
  inbound frame, each tagged `provider = "elevenlabs"`. First diagnostic for
  "turns don't separate" / "agent silent" class issues.
- Common failures:
  - **Upgrade rejected / immediate close** — bad `agent_id`, or private agent
    without `ELEVENLABS_API_KEY`.
  - **`client_error` about overrides** — you sent `voice`/`model`/
    `override_prompt`/`language`/`first_message` without enabling that
    override in the agent's Security tab. Non-fatal: the session continues.
  - **Fatal `unsupported_audio_format`** — agent configured for `ulaw_8000`;
    switch it to a PCM format.
  - **Tool calls never arrive** — tool not defined on the agent, wrong type
    (must be *Client*), or name mismatch (case-sensitive). Check the worker
    log line listing advertised tool names.
  - **Agent narrates client tools** — see §6.1; the silence convention must
    live in the agent's dashboard prompt unless `override_prompt = true`.
  - **Turn transcript/audio duplicated** — server-side re-stream; the
    re-delivery guards (§4.3) drop exact-duplicate `agent_response` text and
    backwards `event_id` audio. If a duplicate still gets through, capture
    the frame sequence with reader tracing and check which event carried it.
  - **Agent says it "can't do X" instead of calling the tool** — the tool is
    missing from the *agent's* config (tools cannot ride the socket, §6);
    our `[[client_tool]]` entry alone only tells vp how to route a call that
    the agent never makes.
  - **Audible break a few seconds into each reply** — not a wire defect:
    ElevenLabs paces delivery at ~1× realtime (unlike the bursting
    providers), so a zero-cushion playback client underruns on the first
    delivery hiccup. The worker forwards audio unmodified; the fix is a
    client-side jitter buffer — the admin playground ships one
    (`AudioPlaybackService` `prebufferMs`, default 200 ms, settings knob
    "Playback Prebuffer"), and other clients should hold each utterance's
    (re)start until ~200 ms of audio is queued.
