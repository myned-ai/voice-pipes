# Local GGUF LLM (in-process, candle)

**Status:** active
**Last verified:** 2026-10-17
**Owner:** Docs

## What it is

A text processor that runs a quantised GGUF model in-process with
Hugging Face's candle: no server, no network, no key. SmolLM2-360M, the
model `examples/minimal.toml` uses, makes a complete offline pipeline on any
CPU; larger models work on CUDA.

`voice-pipes models fetch` downloads three SmolLM2 sizes, each GGUF with its
`tokenizer.json` beside it. Measured on a laptop CPU (Q4_K_M, short spoken
replies):

| Name | Size | Speed | Replies |
|---|---|---|---|
| `smollm2-360m` (default) | ~270 MB | ~20 tokens/s | on topic, short |
| `smollm2-1.7b` | ~1 GB | ~9 tokens/s | best of the three; a long first sentence takes seconds |
| `smollm2-135m` | ~80 MB | fastest | rambles; for smoke tests |

## Cargo feature

`llm-gguf` on the **CLI crate**, where it is a default feature, so `cargo
install` and both worker images carry it; `cuda` runs the model on an NVIDIA
GPU. The processor lives in its own crate, `voice-pipes-llm-gguf`, which
takes candle and tokenizers with it. (Until 2026-09 both were engine
features; `-p voice-pipes --features llm-gguf` no longer exists.)

## Config

```toml
[[text_processor]]
type = "llm"
model = "models/smollm2-360m/SmolLM2-360M-Instruct-Q4_K_M.gguf"
# tokenizer_path = "models/smollm2-360m/tokenizer.json"  # only when tokenizer.json is not beside the model
system_prompt = "You are a helpful voice assistant. Be concise."
max_tokens = 128
temperature = 0.7
# top_p = 0.95
# repeat_penalty = 1.1
# seed = 42
# chat_template = "chatml"        # or "llama3", "gemma3", "gemma4"; must match the model's prompt format
# device = "cpu"                  # "cuda" with the cuda feature, "metal" on macOS
# prefill_cache = false           # reuse the system prompt's KV cache between turns; Llama-family
                                  # models (SmolLM2, Llama) can since candle 0.11, Gemma 3 cannot
                                  # (true only logs a warning there)
# history_max_turns = 20
```

Flat layout: every key after `type` is an option.

## Environment

None. `HF_TOKEN` only to download gated models such as Gemma.

## Capabilities

- Streaming token generation with sentence-boundary chunking to TTS.
- Prefill cache: the system prompt is ingested once per session, so time to
  first token does not pay for it on every turn.
- Conversation history with a rolling window.
- Time to first token and generation time in the per-turn metrics; with
  `RUST_LOG=voice_pipes=debug` the log shows KV-cache prefill and token
  generation timings.

## Limitations

- **CPU prompt ingestion is slow for anything beyond a tiny model.** For
  Gemma-class models the wizard deliberately routes through a `llama-server`
  sidecar with the [OpenAI-compatible](openai-compatible.md) processor
  instead of candle on CPU; do the same unless you have a GPU.
- No function calling; use the OpenAI-compatible processor for
  [client-side tools](../../tool-calling.md).
- `chat_template` must match the model: a Llama 3 model with the ChatML
  template produces garbage without an error. A name other than the four
  above is refused when the config is validated.

## Upstream

candle (`huggingface/candle`); GGUF models from their respective Hugging Face
repositories, for example `bartowski/SmolLM2-360M-Instruct-GGUF` and
`unsloth/gemma-4-E2B-it-GGUF`.
