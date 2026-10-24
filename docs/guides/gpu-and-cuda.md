# GPU and CUDA

**Status:** active
**Last verified:** 2026-10-02
**Owner:** Docs

How GPU acceleration works across the engine: the ONNX Runtime CUDA execution provider for Parakeet, Piper, both Nemotrons, Kokoro and Moonshine (`ort-cuda`, `stt-nemotron-cuda`, `stt-moonshine-cuda`; select at runtime with `VP_ORT_EP=cuda`; Silero runs on the CPU), candle on CUDA for the local GGUF LLM (`cuda`), and the GPU sidecars (VibeVoice, `llama-server`). The Docker launcher probes for NVIDIA Container Toolkit pass-through and picks the CUDA worker image automatically; CUDA features do not build on public CI runners.

The CUDA and VibeVoice images run as an unprivileged user like every other
image; the NVIDIA container runtime exposes `/dev/nvidia*` to any uid, so no
extra group is needed. See [Deployment › Images](deployment.md#images) for
the bind-mount ownership rule on Linux.

The rest of this page is the engineering record of the Nemotron streaming decoder and its CUDA execution provider, which is where most of the measured numbers come from.

---

## Nemotron-Speech-Streaming — true cache-aware streaming + optional CUDA EP

This document captures two related changes to the local Nemotron STT
provider:

1. Replacing the buffered "fake-stream" `transcribe_stream` with a real
   cache-aware streaming loop that amortizes encoder compute during the
   user's speech.
2. Adding an opt-in CUDA execution provider for the ONNX Runtime
   sessions (encoder, decoder, joint, mel).

Code lives under [voice-pipes-stt-nemotron/src/](../../voice-pipes-stt-nemotron/src/).

---

## 1. Why the live UX was bad

Symptoms (live mic playground):

- Short utterances ("hi", "hello") often required repeating twice.
- Per-turn `processing_ms` reported by the diagnostic was 4–19 s for
  utterances of well under one second.
- Diagnostic reported `stt mode: "batch"` even though the provider's
  capability advertised streaming.

Root cause: the `SttProvider::transcribe_stream` impl in
[lib.rs](../../voice-pipes-stt-nemotron/src/lib.rs) buffered the entire utterance into
a `Vec<f32>` and only on `is_last` invoked the batch
`transcribe_buffered`. So all encoder/decoder work happened **after**
endpointing — wall time ≈ `audio_seconds × RTF` of the *full* VAD-tail
window (typically 1.5 s of trailing silence included).

The 7-10-7 cache-aware export already supports per-chunk encoding with
shared cache state (left=7, chunk=10, right=7 subsampled frames →
80 ms → 800 ms audio chunk + 560 ms right context). The infra to use it
(`streaming::StreamingEngine`) was already implemented but the provider
trait method just wasn't calling it.

## 2. The fix — true cache-aware streaming

[streaming::StreamingEngine](../../voice-pipes-stt-nemotron/src/streaming.rs) owns:

- `NemotronSessions` (encoder + decoder + joint + mel)
- `CacheTensors` (mutated in place across chunks)
- `DecoderState` (RNNT decoder; advances per encoder output frame)
- A rolling `sample_buffer` and `sample_pos` cursor

It exposes:

- `feed_samples(&mut self, samples: &[f32]) -> Result<Vec<u32>, _>` —
  appends to the buffer, drains as many full
  `chunk_samples + right_samples + WIN_LENGTH` windows as available,
  returns any newly emitted RNNT tokens.
- `flush(&mut self) -> Result<Vec<u32>, _>` — at end-of-utterance:
  zero-pads remaining audio to a chunk window and runs final
  encoder/decoder steps so the right context lookahead is drained.

[lib.rs::transcribe_stream](../../voice-pipes-stt-nemotron/src/lib.rs) now:

1. Checks out `NemotronSessions` from the pool, constructs
   `StreamingEngine`.
2. On every `audio_rx.recv()`:
   - Coalesces queue backlog (`try_recv` loop) into one `feed_samples`
     call.
   - Runs the call inside `tokio::task::spawn_blocking` (ONNX
     `Session::run` is sync-blocking).
   - On `is_last` (or channel close) also calls `flush()`.
   - Tracks chunk drains via `engine_chunk_progress(&engine)` (reads
     `engine.sample_pos / chunk_samples`) so `inference_count` reflects
     real encoder steps.
3. Emits a `TranscriptionResult { is_final: false }` whenever the
   detokenized text changes (BPE-safe re-render + dedupe), and one
   `TranscriptionResult { is_final: true }` at end.
4. Returns sessions to the pool on every exit path (success, error,
   channel-closed-without-final).

**Expected impact**: post-endpointing wall time drops from
`audio_seconds × RTF` to **flush-only** = one chunk-sized encoder run
plus decoder drain — roughly the configured `chunk_ms` (≈ 560 ms with
the 7-10-7 export) plus a small constant.

### Files touched

| File | Change |
|---|---|
| [voice-pipes-stt-nemotron/src/lib.rs](../../voice-pipes-stt-nemotron/src/lib.rs) | Rewrote `transcribe_stream` end-to-end. Added `engine_chunk_progress` helper. Added `StreamingEngine` to the import set. |
| [voice-pipes-stt-nemotron/src/streaming.rs](../../voice-pipes-stt-nemotron/src/streaming.rs) | Made `StreamingEngine.sample_pos` `pub` so the helper can read chunk progress without a new method. |
| [voice-pipes-admin/backend/app/main.py](../../voice-pipes-admin/backend/app/main.py) | Added `"nemotron-streaming"` to `_STREAMING_STT_PROVIDERS` so the diagnostic flips from `mode: "batch"` to `mode: "streaming"` and stops flagging `processing_ms` as an anomaly. |

### What was intentionally dropped

The previous implementation contained two debugging features tied to
its buffered nature:

- Per-utterance audio dump gated by `NEMOTRON_DUMP_DIR`.
- Per-utterance RMS / peak / leading-zero / trailing-zero logging.

These were removed because there is no longer a single
end-of-utterance buffer to mirror. If they're needed again, add a
sample mirror inside `StreamingEngine` (write tail to a per-utterance
WAV in `flush`) — small change, just not a default.

The pipeline-side "flush trick" (500 ms zero-pad on the final chunk in
[pipeline/stt.rs](../../voice-pipes/src/pipeline/stt.rs)) is preserved at the layer
above; the new engine handles those zeros via its right-context flush.

### Compatibility

- `transcribe()` (one-shot batch) still calls `transcribe_buffered`
  unchanged.
- `expected_sample_rate()`, `name()`, `capabilities()`,
  `as_component()` unchanged.
- Sessions pool semantics preserved (single-slot pool today; nothing
  blocking a multi-slot pool later).

### Validation

```powershell
cd voice-pipes
cargo check -p voice-pipes-stt-nemotron
cargo test  -p voice-pipes-stt-nemotron
# 4 tests pass: preemphasis, reflect_pad, log_mel_spectrogram_sine,
# raw_mel_distribution_unnormalized
```

End-to-end live validation in the admin playground is the next step
(not yet performed in the same run).

### Related follow-ups

- Drop `worker.toml` `silence_ms` from 1500 → ~700 ms now that STT
  responds quickly post-endpointing. Independent change; not in this
  patch.
- The `with_execution_providers` block is now the shared
  `voice_pipes_onnx::open_session`; Silero does not use it yet and stays on
  the CPU (see §3).

---

## 3. Optional CUDA execution provider

`ort` 2.0 supports per-session execution providers. Until now, every
ONNX session built by `streaming::open_session` was CPU-only. The
existing workspace `cuda` cargo feature applies to `candle` (LLM), not
to `ort`.

### Cargo features

[Cargo.toml](../../voice-pipes/Cargo.toml) now exposes:

```toml
# Enable CUDA execution provider for the ONNX (ort) sessions used by
# Parakeet/Nemotron/Piper/Silero. Requires ONNX Runtime CUDA build +
# CUDA 12 / cuDNN at runtime.
ort-cuda           = ["ort/cuda"]
stt-nemotron-cuda  = ["stt-nemotron", "ort-cuda"]
```

`ort-cuda` is generic so other ort-using providers can opt in by
combining it with their own feature gate.

### Runtime opt-in

Even with `ort-cuda` compiled in, sessions stay on CPU unless the
runtime env var is set:

```powershell
$env:VP_ORT_EP = "cuda"
```

This was deliberate: a missing CUDA install on a laptop should not
silently break the binary. With the env var set, registration failures
**propagate as `SttError::ModelLoadFailed`** — fail loud, not silent.

### Code shape

Every plugin opens its sessions through one function in `voice-pipes-onnx`
(feature `session`; its `cuda` feature is what a plugin's `ort-cuda`
switches on), so the policy lives in one place: CUDA when asked for and
compiled in, a registration failure an error, and a request the build
cannot honour run on the CPU with a warning.

```rust
use voice_pipes_onnx::{ExecutionTarget, SessionOptions, open_session};

let options = SessionOptions::new(threads).with_target(ExecutionTarget::from_env());
let session = open_session(&path, &options)?;
```

Kokoro follows `VP_ORT_EP` too, unless its own `execution_provider`
option is set, which then picks its target instead; the two turn
detectors pass none and stay on the CPU.
`ExecutionTarget::effective()` says where a session actually runs, for the
provider's device report. The engine's own models (Silero, Moonshine)
still open theirs directly until they move into plugin crates; Silero
builds CPU sessions.

### Build

```powershell
cd voice-pipes
cargo build --release -p voice-pipes-cli --features stt-nemotron-cuda
```

Verified: clean build from `cargo clean` finishes with exit 0
(~2m 30s on the dev box). CPU-default build (`stt-nemotron`) also
unaffected.

### Runtime requirements (Windows)

`ort` 2.0 dynamically loads the ONNX Runtime DLLs at startup. For CUDA
you need the **GPU build** of ONNX Runtime, plus matching CUDA + cuDNN.

| Component | Version | Notes |
|---|---|---|
| CUDA Toolkit | 12.x | `12.8` confirmed via `build-cuda.ps1` on the dev box. |
| cuDNN | 9.x for CUDA 12 | NVIDIA Developer download. Extract `bin\*.dll` to a `PATH` folder, e.g. `C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v12.8\bin`. |
| ONNX Runtime GPU | 1.20.x | https://github.com/microsoft/onnxruntime/releases — `onnxruntime-win-x64-gpu-1.20.1.zip`. Extract `lib\onnxruntime.dll`, `lib\onnxruntime_providers_shared.dll`, `lib\onnxruntime_providers_cuda.dll` to a `PATH` folder. ort `2.0.0-rc.12` expects the 1.20 ABI. |

By default `ort` will try to download a prebuilt binary at first
load. To force a specific copy:

```powershell
$env:ORT_DYLIB_PATH = "C:\path\to\onnxruntime.dll"
```

### Runtime requirements (the CUDA container image)

The Linux image (`voice-pipes/Dockerfile.cuda`) links ONNX Runtime
statically, but the CUDA execution provider is still two shared libraries
loaded at run time, `libonnxruntime_providers_shared.so` and
`libonnxruntime_providers_cuda.so`, looked up beside the process's working
directory (`/app`). The build gathers them from the runtime it fetched and
fails if they are missing; an image without them reports "Failed to load
library /app/libonnxruntime_providers_shared.so" on the first session and
every ONNX recogniser or voice on it is unusable under `VP_ORT_EP=cuda`.
`VP_ORT_EP=cpu` on the container is the way to keep such a worker running
on the CPU until the image is rebuilt.

The runtime that ort `2.0.0-rc.12` fetches is ONNX Runtime 1.20, which has
no kernels for Blackwell cards (compute capability 12.0, the RTX 50 series):
the provider loads and the first run fails with
`cudaErrorNoKernelImageForDevice`. On such a card the ONNX providers stay on
the CPU (`VP_ORT_EP=cpu`, which is what the Croatian Nemotron worker's
compose entry sets) until the ort dependency moves to a runtime built for
them; llama-server and candle are unaffected.

### Run

```powershell
$env:VP_ORT_EP    = "cuda"
$env:ORT_DYLIB_PATH = "C:\path\to\onnxruntime.dll"  # if needed
.\target\release\voice-pipes.exe live --worker-config worker.toml
```

Expected log line **four times** at startup (encoder, decoder, joint,
mel):

```
ort: CUDA EP registered model=...\encoder.onnx
```

If absent, that's a DLL/PATH issue, not a build issue.

### Caveats

- **Mel session on GPU is wasteful.** It's a tiny graph; dispatching
  it to CUDA pays host↔device transfer overhead with no compute win.
  If profiling shows it dominating, change `open_session` to accept a
  "register CUDA" bool and pass `false` for the mel session only.
- **First inference is slow** (~1–3 s) because cuDNN autotunes
  convolution algorithms on the first call with a given shape. Steady
  state is what to measure — discard the first few turns.
- `with_intra_threads` still applies to whatever ops the CUDA EP
  doesn't claim (some graph nodes fall back to CPU). Leave it as-is.
- The gate covers every session opened through the shared opener
  (Parakeet, Piper, both Nemotrons, Kokoro). Silero uses its own session
  builder in the engine and stays on the CPU until it moves onto it.

### Related files

- [Cargo.toml](../../voice-pipes/Cargo.toml) — feature gates.
- [voice-pipes-stt-nemotron/src/streaming.rs](../../voice-pipes-stt-nemotron/src/streaming.rs) — `open_session`.
- [build-cuda.ps1](../build-cuda.ps1) — existing CUDA build helper for the candle/LLM path. Not directly required here.

---

## 4. Summary table

| Concern | Before | After |
|---|---|---|
| `transcribe_stream` body | Buffer all → batch on `is_last` | Per-chunk cache-aware encoder + decoder, partial emissions |
| Post-endpointing wall time (short utterance) | ~audio_seconds × RTF (4–19 s observed) | ~chunk_ms + flush (~hundreds of ms) |
| Diagnostic `stt mode` | `batch` | `streaming` |
| ONNX execution provider | CPU only | CPU default; CUDA opt-in via `--features stt-nemotron-cuda` + `VP_ORT_EP=cuda` |
| Cargo build (`stt-nemotron`) | green | green |
| Cargo build (`stt-nemotron-cuda`) | n/a | green |
| Tests (`cargo test --lib stt::nemotron`) | 4 pass | 4 pass |

