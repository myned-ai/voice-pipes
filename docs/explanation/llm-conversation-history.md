# LLM Conversation History — Implementation Spec

**Status**: Implemented  
**Created**: 2026-04-16  
**Scope**: In-memory, per-session conversation history for local and
OpenAI-compatible LLM backends.

---

## 1. Overview

Every voice-pipes session maintains a running conversation history so the LLM
can produce contextual, multi-turn responses. History is accumulated in-memory,
scoped to the lifetime of a single client connection, and automatically
discarded on disconnect.

Two parallel implementations exist, one per LLM pathway:

| Pathway | Struct | File | Lock |
|---------|--------|------|------|
| Local GGUF (candle) | `ConversationMemoryWrapper` | `voice-pipes-llm-gguf/src/memory.rs` | `RwLock<Vec<Message>>` |
| OpenAI-compat API | `OpenAiCompatProcessor` | `voice-pipes-llm-openai-compat/src/lib.rs` | `Mutex<Vec<ChatMessage>>` |

---

## 2. Architecture

```
Client connects
  │
  ▼
runner.rs  ──  builds fresh processor instances per session
  │             (each gets its own empty history vec)
  │
  ├── Local LLM path ──────────────────────────────────────┐
  │   LlmProcessor (inference)                             │
  │     └── wrapped in ConversationMemoryWrapper           │
  │           ├── history: RwLock<Vec<Message>>             │
  │           ├── template: Box<dyn ChatTemplate>           │
  │           └── system_prompt: String                     │
  │                                                        │
  │   Prompt assembly per turn:                            │
  │     [system_prompt] + [all history] + [new user msg]   │
  │     → ChatTemplate::format_messages()                  │
  │     → tokenise → inference → stream                    │
  │     → assistant response appended to history           │
  │                                                        │
  ├── OpenAI-compat path ──────────────────────────────────┐
  │   OpenAiCompatProcessor                                │
  │     ├── history: Mutex<Vec<ChatMessage>>                │
  │     └── config (base_url, model, api_key, …)           │
  │                                                        │
  │   Prompt assembly per turn:                            │
  │     if input is pre-formatted (contains chat tokens):  │
  │       → send as single user message (no double-wrap)   │
  │     else:                                              │
  │       → [system] + [history] + [user] as ChatMessages  │
  │     → HTTP POST to /chat/completions                   │
  │     → assistant response appended to history           │
  │                                                        │
Client disconnects → session dropped → history freed
```

### 2.1 Per-session instantiation

In `transport/serve/runner.rs`, every new client connection triggers a fresh
`build_processors()` call. This gives each session:

- Its own `ConversationMemoryWrapper` (or `OpenAiCompatProcessor`) instance
- An empty history `Vec`
- A dedicated audit/trace channel

No history is shared across sessions.

### 2.2 Chat templates (local LLM only)

`ChatTemplate` trait in `voice-pipes-llm-gguf/src/chat_template.rs` formats the
`[system, history…, user]` message array into the model-specific prompt string:

| Template | Format | Models |
|----------|--------|--------|
| `ChatMlTemplate` | `<\|im_start\|>role\ncontent<\|im_end\|>` | SmolLM2, Qwen, Mistral |
| `Llama3Template` | `<\|begin_of_text\|><\|start_header_id\|>role…` | Llama 3.x |

### 2.3 System-prompt KV-cache prefill

For local models, `voice-pipes-llm-gguf/src/prefill.rs` can pre-compute the system prompt
into the KV-cache at startup. On each turn `reset_cache()` restores the
prefill snapshot so only conversation tokens (history + new user message) need
processing. That needs a backend that takes a batch of tokens at an offset;
candle 0.9's quantized Llama and Gemma 3 do not, so the option is refused at
load with a warning and every turn is fed from position zero.

### 2.4 Pre-formatted prompt detection

When `OpenAiCompatProcessor` receives input that already contains chat-template
tokens (`<|im_start|>` or `<|begin_of_text|>`), it sends the entire string as
a single user message to avoid double-wrapping. This is the interop mechanism
when the processor is wrapped inside a `ConversationMemoryWrapper`.

---

## 3. Configuration

History itself has no dedicated config knobs. Adjacent settings that affect
prompt construction:

| Setting | Section | Default | Notes |
|---------|---------|---------|-------|
| `system_prompt` | `[text]` | `"You are a helpful assistant…"` | Prepended to every turn |
| `max_tokens` | `[text]` | `256` | Max **completion** tokens, not context window |
| `temperature` | `[text]` | `0.6` – `0.7` | Sampling temperature |
| `chat_template` | `[text]` | `"chatml"` | Template format (local only) |
| `prefill_cache` | `[text]` | `false` | Pre-cache the system prompt's KV entries. Needs a backend that continues a batch at an offset: Llama-family models do since candle 0.11; Gemma 3 does not, so there it warns and the prompt is computed each turn |

---

## 4. Known Limitations

| # | Limitation | Impact |
|---|-----------|--------|
| 1 | **Unbounded history** — both `Vec` stores grow without limit | Long sessions can exceed the model's context window, leading to truncated or degraded output |
| 2 | **No token counting** — no awareness of context-window size | Cannot proactively trim or summarise old turns |
| 3 | **No persistence** — history lives only in memory | Reconnecting starts a blank conversation |
| 4 | **Dual history bookkeeping** — when `OpenAiCompatProcessor` is wrapped in `ConversationMemoryWrapper`, both maintain separate histories | The pre-formatted detection heuristic works but is fragile |

---

## 5. Future Work

- **Sliding window / max-turns cap** — configurable `max_history_turns` or
  `max_context_tokens` to bound memory and stay within the context window.
- **Token-aware truncation** — count tokens per message and evict oldest turns
  when approaching the limit.
- **Persistent history** — optional storage backend (SQLite / file) for
  session resume.
- **Unified history layer** — single history implementation shared by both
  local and OpenAI-compat paths, eliminating the dual-bookkeeping issue.
