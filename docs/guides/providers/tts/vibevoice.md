# VibeVoice (TTS, GPU sidecar)

**Status:** active
**Last verified:** 2026-06-20 (measurements) · 2026-09-06 (page)
**Owner:** Docs

Microsoft VibeVoice-Realtime-0.5B served by a Python sidecar on a GPU and reached through the `openai` TTS provider. Quality-first, long-form; **not** a low-latency conversational model. Feature: `tts-openai`. Wizard option: *VibeVoice-Realtime-0.5B (host GPU sidecar)*.
## Identity
| | |
|---|---|
| **Stage** | TTS (text → speech) |
| **Vendor** | Microsoft |
| **Family** | VibeVoice-Realtime-0.5B (diffusion-based neural TTS) |
| **License/source** | vendored at `voice-pipes/VibeVoice/` |
| **Runs** | **Separate process** (Python sidecar), GPU — as the `vibevoice` Docker service, or standalone on the host |
| **Engine provider** | `openai` TTS provider, pointed at the sidecar's OpenAI-compatible endpoint |

VibeVoice is a **quality-first, long-form** TTS. It is **not** a low-latency
conversational model — see [Performance](#performance-measured).

## How it runs
The sidecar (`voice-pipes/scripts/vibevoice/server.py`) loads the model and
exposes an **OpenAI-compatible** API:
- `POST /v1/audio/speech` — synthesis (streams raw PCM when `response_format: "pcm"`).
- `GET  /v1/models` — advertises the model **id + runtime version** (so the
  engine records the real model in the session report — see
  [`ModelIdentity`](../../../../voice-pipes/src/component/mod.rs)).
- `GET  /health`.

The voice-pipes engine reaches it through the standard `openai` TTS provider
(the `voice-pipes-tts-openai` plugin crate) with the endpoint set to the sidecar. From the engine's
point of view it *is* "openai" — the actual model identity is resolved at
session setup via `/v1/models`.

### Launch
**Docker (default).** Selecting VibeVoice in the config wizard writes the
`.vibevoice-enabled` flag; `launch-stack.ps1` then starts the `vibevoice` compose
service (`--profile vibevoice`) built from `Dockerfile.vibevoice`. It reserves an
nvidia device, binds `:8091`, and volume-mounts `models/` + `VibeVoice/demo/voices`
— the entrypoint (`deploy/docker/scripts/vibevoice-entrypoint.sh`) downloads the
weights + voices on first boot. The worker reaches it at `http://vibevoice:8091`
on the shared compose network. Requires Docker GPU pass-through (probed by the
launcher); flash-attn is not in the image, so it runs on `sdpa`.

**Host (standalone, no Docker).** Still supported for local dev — set up
`.venv-vibevoice` per `scripts/vibevoice/README.md` and run:
```
.venv-vibevoice\Scripts\python.exe scripts/vibevoice/server.py \
    --device cuda --model-path models/vibevoice
```
(45 voice aliases load from `VibeVoice/demo/voices/streaming_model/`.)

### Engine config (TOML, TTS section)
```toml
[tts]
provider   = "openai"
endpoint   = "http://vibevoice:8091/v1"   # Docker service; host standalone: http://localhost:8091/v1
model_name = "VibeVoice-Realtime-0.5B"   # cosmetic; real id comes from /v1/models
voice      = "en-Carter_man"             # else defaults to en-Carter_man
```

## Performance (measured)
Measured on a local CUDA GPU, **sdpa** attention (no flash-attn), `ddpm_steps=5`.

| Metric | Cold turn (turn 0) | Warm turn | Target |
|---|---|---|---|
| TTFB (first audio byte) | ~8.2 s | **~1.4 s** (first byte ~0.5 s after LLM) | ≤400 ms |
| RTF (`synthesis/audio`) | 3.5 | 2.5 | ≤0.6 |
| RTFx (`audio/synthesis`) | 0.28 | 0.39 (0.7 seen on a very short clip) | ≥1.0 |

**The decisive fact: VibeVoice synthesizes at ~0.3–0.4× realtime here.** With
streaming, the *first byte* arrives fast (~0.5 s after the LLM), but the stream
**cannot sustain realtime** — e.g. a 5.2 s utterance took **12.6 s of wall-clock
to play** (chunks arrive slower than they're consumed → choppy/stretched
playback). So a low TTFB is necessary but **not sufficient**: sustained RTFx ≥ 1
is what makes streaming TTS smooth, and VibeVoice on this setup doesn't reach it
for anything beyond a very short utterance.

**Cold start:** the first synthesis after load pays a one-time warm-up (CUDA
kernels, first DDPM run) — ~8 s. Mitigate with a startup dummy synthesis
(see [Gotchas](#known-issues--gotchas)); ignore turn 0 in averages.

## Streaming
The sidecar streams PCM **during** generation (VibeVoice `AudioStreamer`):
`generate()` runs on a worker thread feeding a queue; the endpoint yields each
chunk as it's produced (chunked transfer, no `Content-Length`). The engine's
`openai` TTS client consumes `bytes_stream()` directly. This is what dropped
TTFB from ~9.5 s (buffer-whole-clip) to ~1.4 s.

## Known issues & gotchas
1. **Sub-realtime synthesis (the main limitation).** RTFx ~0.3–0.4 on sdpa →
   streaming stutters for non-trivial utterances. Not a bug; a model/hardware
   limit. See [When to use](#when-to-use--alternatives).
2. **flash-attn not installed → falls back to `sdpa`.** Log: *"Failed to load
   with flash_attention_2. Falling back to sdpa."* flash-attn would help
   (~1.3–2×) but is painful to install on Windows and likely still leaves it
   near/below 1× realtime. sdpa is functionally fine.
3. **Diffusion scheduler is NOT thread-safe.** The DPM solver keeps `step_index`
   on the shared model; two overlapping `generate()` calls race it →
   `IndexError: index N out of bounds for size N` in `dpm_solver.py`.
   **Fixed in `server.py`** via (a) a process-wide generation lock, (b) closing
   the generator on client disconnect/barge-in so the worker thread is reliably
   stopped, and (c) `set_ddpm_inference_steps()` per generation. Overlaps happen
   because the agent **barges in / cancels TTS** mid-stream — don't reintroduce
   unsynchronized generation.
4. **Startup warning: "Some weights … newly initialized" (acoustic_tokenizer
   encoder).** Expected for the streaming *inference* class — TTS generation uses
   the decode path; the encoder weights are unused for synthesis. Not an error.
5. **Tokenizer class mismatch warning (`Qwen2Tokenizer`).** Harmless.
6. **Voice default.** If the request sends no voice, the sidecar falls back to
   `en-Carter_man.pt` (logged). Set `voice` in the TTS config for a specific
   speaker.
7. **`ddpm_steps = 5`** (set in `server.py`). Fewer = faster but lower quality.

## When to use / alternatives
- **Use VibeVoice for**: quality/expressive or offline synthesis where latency
  isn't the constraint.
- **Do NOT use for responsive conversation.** For a real-time agent, point the
  TTS at a **faster-than-realtime** provider — **Piper** (fastest) or **Kokoro**
  (fast, good quality), both already in the engine. Keep VibeVoice available for
  quality work. (Real OpenAI cloud `tts-1` is also sub-second if cloud is OK.)

## Diagnostics signature
In a session report, VibeVoice-as-`openai` looks like:
`tts.rtf_high` on every turn (rtf 2.5–3.5), a cold-turn `tts.ttfb_high`, and a
large gap between `e2e_perceived_ms` (good, first byte) and the
`playback_start → playback_end` span (bad, stretched). If you see that, it's the
sub-realtime ceiling, not the streaming/buffering bug (that's fixed).

## Changelog
- **2026-07-01** — Containerized the sidecar as the `vibevoice` compose service
  (`Dockerfile.vibevoice`, `--profile vibevoice`), reached at `http://vibevoice:8091`.
  Weights + voices are volume-mounted and downloaded on first boot; no host venv
  required. Host standalone launch still supported.
- **2026-06-20** — Made the sidecar stream first-byte (TTFB 9.5 s → ~1.4 s);
  fixed the scheduler concurrency `IndexError` (gen lock + generator close on
  barge-in); added `/v1/models` model-identity reporting. Documented sub-realtime
  RTFx as the remaining limitation.
