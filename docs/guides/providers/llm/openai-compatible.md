# OpenAI-compatible LLM (cloud or local server)

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs

## What it is

A text processor for any endpoint that implements `/v1/chat/completions`
with server-sent-event streaming: OpenAI, llama.cpp's `llama-server`,
Ollama, vLLM, LM Studio and others. The usual LLM stage for a cascade
pipeline, with conversation history, retries, cancellation and function
calling.

## Cargo feature

`llm-openai-compat` on the **CLI crate**, where it is a default feature, so
`cargo install` and both worker images carry it. The provider lives in its own
crate, `voice-pipes-llm-openai-compat`, registered by the CLI at startup.
(Until 2026-09 it was an engine feature; `-p voice-pipes --features
llm-openai-compat` no longer exists.)

## Config

```toml
[[text_processor]]
type = "openai-compat"
base_url = "http://localhost:8081/v1"    # omit for https://api.openai.com/v1
model = "gemma-4-E2B-it"
# api_key = ""                           # literal; or:
# api_key_env = "OPENAI_API_KEY"         # env var name; OPENAI_API_KEY is the fallback anyway
system_prompt = "You are a helpful voice assistant. Be concise."
max_tokens = 256
temperature = 0.7
# top_p = 0.95
# presence_penalty = 0.0
# frequency_penalty = 0.0
# seed = 42
# stop = ["\n\n"]
# timeout_secs = 30                      # longest wait for the next byte (headers, then each chunk)
# max_retries = 2                        # 5xx / 429 / timeouts; capped at 5
# response_format = "text"               # or "json_object"
# tool_choice = "auto"                   # "auto" | "none" | "required", when client tools are declared
# history_max_turns = 20                 # rolling window of user/assistant pairs

# Fields the server takes beyond the common API, sent as given at the top
# level of every request. The keys the options above set (`model`,
# `max_tokens`, `tools`, ...) are refused here.
# [text_processor.extra_body]
# chat_template_kwargs = { enable_thinking = false }
```

Flat layout: every key after `type` is passed to the processor.

## Thinking models

A model that reasons before it answers (Gemma 4, Qwen 3) streams the
reasoning as `reasoning_content`, which the pipeline does not speak, and
spends `max_tokens` on it first: with the default budget every turn came
back as 128 tokens of thought and no words. Turn thinking off through the
server's template switch, with llama-server
`chat_template_kwargs = { enable_thinking = false }` under
`[text_processor.extra_body]` as above (or start it with
`--reasoning-budget 0`). A hosted API has its own field for the same thing;
`extra_body` carries whatever it is.

## Environment

Key resolution order: `api_key` in the file, then the variable named by
`api_key_env`, then `OPENAI_API_KEY`, then empty (valid for local servers).

## Capabilities

- Streaming with sentence-boundary chunking so TTS starts on the first
  sentence.
- Rolling conversation history per session, capped by `history_max_turns`
  so the prompt cannot grow until latency spikes.
- Retry with exponential backoff (500 ms doubling, at most 8 s per wait,
  at most 5 retries) on 5xx, 429 and timeouts; cancellation on barge-in
  closes the stream immediately, including mid-backoff.
- Function calling for [client-side tools](../../tool-calling.md): `tools`
  and `tool_choice` are sent, streamed `delta.tool_calls` are accumulated.
- Sampling parameters are live-editable from the admin console's parameter
  editor.
- Time to first token (`first_token_ms`, stamped on the first non-empty
  `delta.content` token so an initial role-only empty delta never stamps TTFT
  early), winning HTTP attempt header latency (`connect_ms`), retry wait time
  (`retry_wait_ms`, `retries`), and generation time per turn in `llm_detail`.

## Timeouts and limits

`timeout_secs` is not a whole-request budget. It is the longest the
processor waits for the next byte from the server: first the response
headers, then each streamed chunk. A long answer that keeps arriving is
never cut; a stall is. The TCP connect has its own 5 s budget.

What the server sends is bounded per turn:

| Item | Limit | On overflow |
|---|---|---|
| One SSE line (no newline seen) | 1 MiB | turn fails |
| Response text | 256 KB | turn ends with what was generated |
| Tool-call arguments (one call) | 64 KB | that call is dropped |
| `tool_calls[].index` | 64 | fragment ignored |
| Non-streaming response body | 1 MiB | turn fails |

A stream ends at the first `finish_reason` (one further chunk is read for
a trailing usage record) or at `[DONE]`, whichever comes first, so a
server that never sends `[DONE]` cannot hold the turn open.

An upstream error response is logged on the worker with the first 256
bytes of its body. The client receives only the status code and its
class (`upstream returned HTTP 401 (authentication rejected)`), never the
body. The `Debug` form of the processor config redacts the API key.

The input is always sent as the user (or, for agent-generated turns,
system) message on top of the operator's system prompt and the history.
Text that looks like a chat template (`<|im_start|>`, `<|begin_of_text|>`)
is content like any other and cannot replace the system prompt.

## Limitations

- A local `llama-server` must run with `--jinja` for tool calling; without
  it the `tools` field is silently ignored.
- Reasoning models add a pre-speech delay; prefer instruction-tuned models
  for conversation.
- The endpoint's own model naming applies (`model` is passed through).

## Upstream

OpenAI chat completions API; llama.cpp server documentation for the local
case.
