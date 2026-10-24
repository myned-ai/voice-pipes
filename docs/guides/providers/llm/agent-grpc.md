# Agent link (external agent over gRPC)

**Status:** active
**Last verified:** 2026-10-16
**Owner:** Docs

## What it is

Not an LLM but a way to make an external service the conversation's brain (on
cascade pipelines) or tool backend (on speech-to-speech pipelines). Your agent
implements `AgentService` from `voice-pipes/proto/agent.proto`: streamed turn
replies, smart-filler pause words with out-of-turn speech, cancel-by-words,
idle re-engagement, spoken-prefix barge-in ground truth, and a tool backend.
Voice Pipes handles audio, turn-taking, barge-in and the client protocol; the
agent decides what to say.

## Cargo feature

`text-grpc`.

## Config

Cascade, the agent is the brain (see
[`voice-pipes/examples/features/agent-grpc-idle.toml`](../../../../voice-pipes/examples/features/agent-grpc-idle.toml)):

```toml
[[text_processor]]
type = "agent-grpc"
endpoint = "http://127.0.0.1:50061"     # required
turn_timeout_ms = 30000                 # optional watchdog per turn (default 30000)

[agent]                                 # optional: out-of-turn Say & idle re-engagement
endpoint = "http://127.0.0.1:50061"
idle_timeout_ms = 8000
idle_max_prompts = 2
idle_message = "Are you still there?"
```

Speech-to-speech, the agent is the tool backend while the provider model
stays the brain (see
[`voice-pipes/examples/features/realtime-s2s-endpoint-observer.toml`](../../../../voice-pipes/examples/features/realtime-s2s-endpoint-observer.toml)):

```toml
[realtime_provider]
provider = "openai-realtime"
# …

[agent]
endpoint = "http://127.0.0.1:50061"
idle_timeout_ms = 10000
idle_max_prompts = 2
```

## Capabilities

- **Streamed replies** (`TurnFragment`) with sentence chunking to TTS.
- **Smart-filler flow (pause words) & out-of-turn speech (`Say`)**: speak an
  immediate acknowledgement (*"Sure — let me look into that. One moment."*),
  finish the turn so the caller can keep talking, and push the result later as
  an out-of-turn `Say` (cascade) or `CompleteTool` (speech-to-speech).
- **Cancel-by-words**: because the turn ends right after the filler, the caller
  can say *"cancel"* or *"never mind"* on a subsequent turn while the background
  task is still running.
- **Barge-in spoken-prefix ground truth (`Interrupted`)**: when the caller
  interrupts, Voice Pipes sends the exact client-reported spoken prefix in
  `Interrupted.spoken_text` (or `turn_cancelled = true` if interrupted before
  any audio played) so your agent's conversation history matches what the caller
  actually heard.
- **Idle re-engagement (`UserIdle`)**: after `idle_timeout_ms` of silence while
  listening, Voice Pipes sends `UserIdle(silence_ms, prompt_count)` so the agent
  can emit a custom `Say` prompt (or fall back to `idle_message`).
- **Tool backend for speech-to-speech providers** (`ListTools`, `ToolCall` →
  `ToolResult`, and async `Pending` → `CompleteTool`).

## Pause words & the smart-filler flow

When a request takes several seconds (database lookup, external API, slow
reasoning), holding the turn open in silence feels broken. Voice Pipes supports
non-blocking filler speech in both pipeline architectures:

### 1. Cascade (`type = "agent-grpc"`) — `TurnFragment` + late `Say`

1. On receiving `UserTurn`, immediately emit a `TurnFragment` with the pause
   phrase (for example, `"Sure — let me look into that. One moment."`) and
   `is_end_of_response = true`.
2. Voice Pipes synthesizes the filler and **ends the turn**, returning the
   session to `Listening` while your background task continues.
3. If the caller speaks while the task is running:
   - If they say *"cancel"* / *"never mind"*, abort the background task and
     confirm cancellation (**cancel-by-words**).
   - If they ask for status, reply with an interim `TurnFragment` (*"Still
     checking on that — I'll let you know as soon as it's ready."*) while the
     task keeps running.
4. When the background task finishes, push an out-of-turn `Say` frame
   (`scheduling = SCHEDULING_WHEN_IDLE`). Voice Pipes speaks it as soon as the
   session is in `Listening` (or immediately if already idle) and records the
   spoken text in history.

### 2. Speech-to-speech (`[agent]` tool backend) — `pending = true` + `CompleteTool`

1. When the S2S model emits a `ToolCall`, immediately reply with a `ToolResult`
   setting `pending = true` and a brief instruction payload (for example,
   `{"status": "working", "instruction": "briefly tell the user you're checking"}`).
2. Voice Pipes forwards the ack and kicks the S2S model so it **speaks a
   natural filler in its own voice** right away.
3. When your slow backend completes, send `CompleteTool(call_id, result,
   scheduling = SCHEDULING_WHEN_IDLE)`. Voice Pipes injects the tool result into
   the S2S session and triggers the final spoken answer.

### Scheduling modes (`Scheduling`)

Both `Say` and `CompleteTool` carry a `Scheduling` policy:

| Policy | Behaviour |
|---|---|
| `SCHEDULING_WHEN_IDLE` *(default)* | Queued while a turn is active; spoken as soon as the session returns to `Listening`. |
| `SCHEDULING_INTERRUPT` | Cancels any in-flight agent speech immediately (`clear_audio`) and speaks the new message at the front of the queue. |
| `SCHEDULING_SILENT` | Appends the context to conversation history without triggering TTS speech. |

## Limitations

- Auto-reconnect of the agent link is not implemented; a dropped link ends
  the agent's participation for that session.
- Agent-initiated interruption of the *user* while the user is speaking is
  deliberately not supported pending a policy decision (`SCHEDULING_INTERRUPT`
  preempts in-flight *agent* speech, not user speech).

## Reference implementation

The Python [reference agent](../../../../examples/README.md)
([`examples/reference-agent/reference_agent.py`](../../../../examples/reference-agent/reference_agent.py))
implements the full protocol — streamed replies, smart-filler + late `Say`,
cancel-by-words, `pending = true` + `CompleteTool`, `Interrupted` history
truncation, and `UserIdle` re-engagement — with canned replies so you can test
every flow without an LLM.
