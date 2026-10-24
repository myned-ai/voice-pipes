# Nemotron streaming (STT, local)

**Status:** active — the more accurate Nemotron for English. For other
languages use [Nemotron 3.5](nemotron-3.5.md).
**Last verified:** 2026-09-25
**Owner:** Docs

## What it is

NVIDIA's `nemotron-speech-streaming-en-0.6b`, a cache-aware streaming
FastConformer with an RNNT decoder, run in-process through ONNX Runtime. The
only built-in STT that streams natively frame by frame rather than
re-decoding a buffer, so its latency does not grow with utterance length.
Requires a one-time model export.

### Against Nemotron 3.5, on English

Measured 2026-09-25 on 160 LibriSpeech utterances (80 from test-clean, 80
from the harder test-other), both models streamed through the same
`transformers` harness at their shipped settings, with 3.5 also at this
model's latency:

| | clean | other | all | CPU RTF |
|---|---|---|---|---|
| **this model**, 560 ms | **2.3%** | **4.5%** | **3.3%** | 0.47 |
| Nemotron 3.5 `en-US`, 320 ms | 3.1% | 7.8% | 5.3% | 0.67 |
| Nemotron 3.5 `en-US`, 560 ms | 3.3% | 7.0% | 5.0% | 0.55 |

At equal latency this model is better on 39 utterances and worse on 9, a
gap of 1.0–2.4 WER points (95% bootstrap interval), and cheaper on CPU. The
multilingual model pays for its 40 locales in English accuracy; for an
English-only deployment this is the Nemotron to run.

## Cargo feature

`stt-nemotron` on the **CLI crate** (plugin crate `voice-pipes-stt-nemotron`);
`stt-nemotron-cuda` adds the CUDA execution provider (`VP_ORT_EP=cuda` at
runtime). Both worker images carry it. (Until 2026-09 it was an engine
feature; `-p voice-pipes --features stt-nemotron` no longer exists.)

## Config

```toml
[stt]
provider = "nemotron-streaming"             # alias: nemotron (what the wizard writes)
model_dir = "models/nemotron-streaming/7-10-7"

[stt.options]
threads = 4                                 # per ONNX session; 0 lets ONNX Runtime choose
blank_id = 1024
max_tokens_per_step = 16                    # denser emission for fast, connected speech
# pool_size = 2                             # ONNX session sets loaded; one per concurrent session

[vad]
mode = "silero"
model = "models/silero_vad.onnx"
min_speech_ms = 500
min_silence_ms = 900                        # brief in-sentence pauses should not finalise early
```

## Model export

The ONNX files are not distributed pre-exported. Download the `.nemo`
checkpoint from Hugging Face (`nvidia/nemotron-speech-streaming-en-0.6b`,
no longer gated: no token or license click needed as of 2026-09) and run the
export once:

```sh
# see voice-pipes/scripts/README-export-nemotron.md
pwsh voice-pipes/scripts/export-nemotron.ps1          # or export_nemotron_streaming.py / Dockerfile.nemo-export
```

The result is `encoder.onnx`, `decoder.onnx`, `joint.onnx`,
`export_metadata.json` and `tokenizer.vocab` under `model_dir`. The wizard
points at that directory but cannot fetch it.

## Environment

- `VP_ORT_EP=cuda` runs the sessions on the GPU in a build with
  `stt-nemotron-cuda`; asked for in a build without it, the sessions stay on
  the CPU and the device report says so.
- `NEMOTRON_NORMALIZED_MEL=1` (or `true`) switches the mel front end to the
  legacy normalised, `pad_to = 16` path. Off by default (raw log-mel). Read
  once per process, so set it before the worker starts.

## Capabilities

| | |
|---|---|
| Input sample rate | 16 kHz |
| Streaming | True streaming, chunked encoder with cache |
| Word timestamps | No |
| End-of-turn signal | No |
| Language | English |

## Limitations

- The export is a one-time manual step in a NeMo environment. The
  checkpoint now also ships a `transformers` config, the route the
  Nemotron 3.5 export takes, so an export without NeMo is possible; it is
  not built yet.
- FP32 export; a self-quantised int4/int8 encoder is experimental.
- Each session holds one set of ONNX sessions for its whole life. The
  provider loads `pool_size` sets (default 2, the same as Parakeet); a
  session beyond that waits up to 5 s for a free set and then fails with an
  error that says so (raise `pool_size` or run fewer sessions per worker). Each set is
  a full copy of the model in memory, so size the pool to the sessions per
  worker, not higher.
- CPU inference is heavier than Parakeet's; the CUDA build is the intended
  deployment. [GPU and CUDA](../../gpu-and-cuda.md) covers the execution
  provider, streaming decode details and measured numbers.

## Upstream

NVIDIA NeMo; model card `nvidia/nemotron-speech-streaming-en-0.6b`.
