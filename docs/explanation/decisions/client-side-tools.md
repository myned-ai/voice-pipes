# Client-side tools

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs
**Decided:** 2026-07

## Context

An avatar or a device UI wants the agent to act on the client: navigate,
highlight, play media. Server-side tools cannot do that, and a spoken
"let me do that for you" filler before every command is wrong for UI
actions.

## Decision

The boundary rule: **the worker never executes a tool.** Server-side tools
go through the agent gateway; client-side tools are declared by the server
(inline `[[client_tool]]`, a catalog file, or per session in `configure`),
merged by name with later sources winning, and offered to the LLM as
function definitions. When the model calls one, the worker emits a
`trigger_action` message with a `call_id` and no spoken filler; the client
replies with `client_tool_result`; the outcome is injected into context and
spoken only if `speak_on_result` or the client asks.

Results are fenced to their turn and in-flight calls are bounded, so a
result arriving after the turn moved on is dropped rather than applied to
the wrong turn.

Both engines support it: speech-to-speech providers natively, the cascade
through function calling added to the OpenAI-compatible processor
(streamed `delta.tool_calls` accumulation).

## Consequences

- A local `llama-server` must run with `--jinja`; otherwise `tools` is
  silently ignored.
- ElevenLabs Agents need the same tools declared agent-side as type Client.
- The candle GGUF processor does not participate.
- `client_tools_path` is a top-level TOML key and must precede the first
  table header.

## Where in code

`voice-pipes/src/agent/client_tools.rs` (declarations, merge, fencing),
`src/transport/mod.rs` (`ClientControl::ClientToolResult`),
`src/transport/serve/realtime_runner/context/provider_handlers.rs`
(dispatch and resolve), `plugins/llm/openai-compat/` (function calling).
Guide: [Tool calling](../../guides/tool-calling.md).
