# Tool calling

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs

An agent that can only talk is a demo. Voice Pipes supports two kinds of
tools, split by one rule: **the worker never executes a tool itself.**

| Kind | Who runs it | Declared where | Result flows |
|---|---|---|---|
| **Client-side tools** | the connected client (browser, app, device) | `[[client_tool]]`, a catalog file, or per session by the client | LLM → `trigger_action` message → client → `client_tool_result` → back into the conversation |
| **Server-side tools** | an external agent service over gRPC, or Rust code you register when embedding the engine | `[agent] endpoint`, or `register_tool_registry_factory` | LLM → agent link → result → back into the conversation |

Both work with both engines: the cascade (STT → LLM → TTS, with an
OpenAI-compatible LLM that supports function calling) and speech-to-speech
providers, which recognise tool calls natively.

## Client-side tools

Use these when the action happens on the user's device: navigate a screen,
highlight an element, play media, read a sensor.

### Declare

Inline in the pipeline file:

```toml
[[client_tool]]
name = "navigate"
description = "Navigate the UI to a named screen."
speak_on_result = false          # default: the result is injected silently
[client_tool.parameters]         # JSON Schema for the arguments
type = "object"
properties = { screen = { type = "string", description = "Screen id." } }
required = ["screen"]
```

Or in a catalog file (JSON array or TOML `[[client_tool]]` tables) named by
a top-level key that must precede the first `[table]` header:

```toml
client_tools_path = "commands/client-tools.json"
```

```json
[
  {
    "name": "highlight",
    "description": "Highlight a UI element on the client screen.",
    "parameters": {
      "type": "object",
      "properties": { "selector": { "type": "string" } },
      "required": ["selector"]
    }
  }
]
```

Or per session: the client sends `options.client_tools` in its `configure`
handshake. The three sources merge by name; later sources win, with one
exception: a tool the client injects at runtime may not take the name of a
server-side tool (one the agent's `ListTools` or a registered Rust handler
answers to). Such a tool is dropped at session setup, logged on the worker,
and reported to the client with a `warning`-severity `error` control
(`code` 1007, `ClientToolRefused`) naming the refused tools. Operator-declared
client tools (inline or catalog file) keep their precedence over server tools
of the same name, since both are the operator's choice.

### The round trip

1. The LLM decides to call `highlight`. The worker emits a `message` envelope
   with `kind: "trigger_action"` and payload `{ call_id, name, arguments }`.
   No spoken filler is generated.
2. The client executes it and replies with a `client_tool_result` control
   message carrying the same `call_id`, a success flag and an optional
   result payload or error.
3. The worker injects the outcome into the conversation context. If
   `speak_on_result` is set, or the client's result asks for it, the agent
   says something about it; otherwise it stays silent.

Results are fenced to the turn that requested them, in-flight calls are
bounded, and a result that arrives after the turn moved on is dropped rather
than confusing a later turn.

### Engine notes

- **Speech-to-speech.** OpenAI Realtime and Gemini Live receive the tool
  specs at session start and emit function calls natively. With ElevenLabs
  Agents the tools must also be defined on the agent (type *Client*) with
  matching names; see [ElevenLabs Agents](providers/s2s/elevenlabs-agents.md).
- **Cascade.** The `openai-compat` processor sends `tools` and `tool_choice`
  and accumulates streamed `delta.tool_calls`. Hosted GPT and Gemini work;
  a local `llama-server` must run with `--jinja` or it silently ignores the
  `tools` field, and a thinking model (Gemma 4) needs thinking turned off
  (`[text_processor.extra_body]` with `chat_template_kwargs = { enable_thinking = false }`,
  see [OpenAI-compatible](providers/llm/openai-compatible.md)) or every turn
  is spent reasoning. Gemma 4 E4B on llama-server calls `play_media` from a
  spoken request in under 3 s. The local candle GGUF processor does not do
  function calling.
- The prompt matters: tell the model that some tools run silently on the
  user's device and it should not narrate them.

The `examples/avatar/` configs and `examples/avatar/commands/client-tools.json`
are complete working examples for both engines.

## Server-side tools

### An external agent over gRPC

`voice-pipes/proto/agent.proto` defines `AgentService`: streamed turn replies,
smart-filler pause words with out-of-turn speech, cancel-by-words, idle
re-engagement, spoken-prefix barge-in feedback, and a tool backend. Point the
worker at your implementation (see
[`voice-pipes/examples/features/agent-grpc-idle.toml`](../../voice-pipes/examples/features/agent-grpc-idle.toml)
for cascade and
[`voice-pipes/examples/features/realtime-s2s-endpoint-observer.toml`](../../voice-pipes/examples/features/realtime-s2s-endpoint-observer.toml)
for speech-to-speech):

```toml
[agent]
endpoint = "http://127.0.0.1:50061"
idle_timeout_ms = 8000           # optional idle re-engagement
idle_max_prompts = 2
idle_message = "Are you still there?"
```

- On **speech-to-speech** sessions the provider model stays the brain; the
  agent is the **tool backend**. Its `ListTools` populate the provider
  session and every function call is forwarded over the link.
- On **cascade** sessions the agent can be the brain: mount it as the text
  processor with `[[text_processor]] type = "agent-grpc"` (Cargo feature
  `text-grpc`; see [Agent link](providers/llm/agent-grpc.md)).

#### Pause words & the smart-filler flow

When a tool or backend lookup takes several seconds, holding the turn open in
silence feels unresponsive. Voice Pipes supports non-blocking pause words
(**smart fillers**) in both pipeline modes so the caller hears an immediate
acknowledgement (*"Sure — let me look into that. One moment."*) and can keep
talking while the work runs in the background:

1. **Cascade (`[[text_processor]] type = "agent-grpc"`)**:
   - The agent responds to `UserTurn` immediately with a `TurnFragment`
     containing the filler phrase and `is_end_of_response = true` (plus an
     optional `Message(kind="tool_call", record_in_history=false)` for client
     UI activity indicators).
   - Because `is_end_of_response = true` ends the turn right after the filler
     is synthesized, the session returns to `Listening`. If the caller speaks
     before the background task completes, the agent receives a normal
     `UserTurn` and can handle **cancel-by-words** (*"cancel"*, *"never mind"*)
     or give an interim status update.
   - When the background job finishes, the agent pushes an out-of-turn
     `Say(text="...", scheduling=SCHEDULING_WHEN_IDLE)` frame, which the worker
     synthesizes as soon as the session is idle.
2. **Speech-to-speech (`[agent] endpoint = "..."`)**:
   - When `ToolCall` arrives, the agent immediately returns
     `ToolResult(call_id, pending=true, output='{"status":"working","instruction":"briefly tell the user you are checking"}')`.
   - The worker forwards that acknowledgement and kicks the S2S provider so the
     model **speaks a natural filler in its own voice** right away, while
     continuing to stream audio and handle barge-in.
   - When the slow job finishes, the agent sends
     `CompleteTool(call_id, result="...", scheduling=SCHEDULING_WHEN_IDLE)`,
     and the worker injects the real output so the model speaks the final answer.

#### Out-of-turn scheduling & idle re-engagement

Out-of-turn `Say` and `CompleteTool` frames take one of three `Scheduling`
policies:

- `SCHEDULING_WHEN_IDLE` *(default)* — queued while a turn is active and spoken
  as soon as the session returns to `Listening`.
- `SCHEDULING_INTERRUPT` — cancels any in-flight agent speech (`clear_audio`)
  and speaks the new message next.
- `SCHEDULING_SILENT` — appends the result to conversation history without
  triggering TTS speech.

When `[agent] idle_timeout_ms` is set and the caller remains silent in
`Listening` for that duration, the worker sends `UserIdle(silence_ms,
prompt_count)` (up to `idle_max_prompts` times per silence stretch; reset on
user speech). A connected gRPC agent can reply with a custom `Say` prompt; when
no agent is connected, the worker speaks `idle_message`.

What the worker accepts from the agent is bounded:

| Item | Limit | On overflow |
|---|---|---|
| One gRPC message from the agent | 1 MiB | the stream errors and the link closes |
| Any text field (`TurnFragment.text`, `Say.text`, `CompleteTool.result`, `ToolResult.output`, `Message.text` / `payload_json`) | 64 KB | the frame is dropped and logged |
| Out-of-turn speech (`Say`; on cascade also `CompleteTool`) | 4 per 10 s | extra frames are dropped |
| Agent events waiting for the session to go idle | 16 | the oldest queued event is dropped |
| `CompleteTool` | only for a call acked as Pending, once, within 5 min of the ack | dropped |
| `Message.kind` | not `trigger_action` or `client_tool_result` | the frame is dropped |

The last row keeps the client-tool protocol in the engine's hands: an agent
cannot send the client a command that bypasses the client-tool allowlist,
in-flight cap and audit trail. Transparency kinds (`tool_call`, `card`,
`text`, ...) stay open.

The Python [reference agent](../../examples/README.md)
([`examples/reference-agent/reference_agent.py`](../../examples/reference-agent/reference_agent.py))
implements the whole protocol with canned replies so you can see every message
without an LLM.

### Registering tools in Rust

If you embed the engine, register a tool registry factory once at startup,
the same idiom as registering a provider:

```rust
voice_pipes::realtime::register_tool_registry_factory(|handle| {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(MyTool::new(handle)));
    reg
});
```

Handlers are `async`, dispatched on their own task so a slow tool never
blocks audio, and can return `ToolOutcome::Pending { ack }` for long jobs.

## Observability

Tool calls and results appear in the session's event log and in the admin
console's transcript, and the health model re-anchors post-tool latency so a
slow tool is not blamed on TTS. The per-turn metrics carry a
`client_tools` component when client tools were used.
