# Moonshine (STT, local)

**Status:** active
**Last verified:** 2026-09-25
**Owner:** Docs

## What it is

Useful Sensors' Moonshine Base, a compact encoder-decoder speech model
optimised for low-latency transcription, run in-process through ONNX Runtime
with a CUDA build available. Smaller and quicker than Parakeet; a good fit
for GPU-accelerated local pipelines.

## Cargo feature

`stt-moonshine` on the CLI crate, which links the plugin crate
`voice-pipes-stt-moonshine`; `stt-moonshine-cuda` adds the ONNX Runtime CUDA
execution provider (select it at runtime with `VP_ORT_EP=cuda`). Its wizard
entry lists second in the recogniser menu, after Parakeet.

## Config

```toml
[stt]
provider = "moonshine"
model_dir = "models/moonshine"

[stt.options]
# pool_size = 2               # ONNX session sets loaded; one per concurrent session
# threads = 4                 # intra-op threads per session; 0 lets ONNX Runtime choose
# initial_buffer_ms = 200     # audio buffered before the first inference pass
# inference_step_ms = 80      # pacing between passes
# max_new_tokens = 128        # decoder cap per pass
```

`model_dir` holds `encoder_model.onnx`, `decoder_model_merged.onnx` and
`tokenizer.json` from `onnx-community/moonshine-base-ONNX`:

```sh
python voice-pipes/scripts/download_moonshine.py
```

The wizard downloads them too.

`decoder_model_merged.onnx` decodes with a key/value cache: each token costs
the same however long the transcript already is. An older `model_dir` with
only `decoder_model.onnx` still works, re-running the whole transcript for
every token; both give the same words (checked token for token on recorded
sessions), the merged one in less than half the decoding time. Add the merged
file to an existing directory to get the speed-up.

The worker warms every inference set once at start, so the first caller does
not pay ONNX Runtime's start-up: before, a worker's first utterance took
longer to transcribe than it took to say.

## Environment

`VP_ORT_EP=cuda` runs the sessions on the GPU in a build with
`stt-moonshine-cuda`; asked for in a build without it, the sessions stay on
the CPU and the device report says so.

## Capabilities

| | |
|---|---|
| Input sample rate | 16 kHz |
| Streaming | Pseudo-streaming over a growing buffer, interim results |
| Word timestamps | No |
| End-of-turn signal | No; use a real VAD with `silence` or `smart` turn detection |
| Language | English |

## Limitations

- English only.
- A batch model: in pipelines it transcribes each utterance once turn
  detection ends it. Its pseudo-streaming mode re-encodes the growing buffer
  on every pass, so long utterances cost more per pass there.
- Very short utterances ("hello" said quickly) are the most often misheard.
- The GPU build needs an ONNX Runtime CUDA build with CUDA 12 and cuDNN at
  runtime; see [GPU and CUDA](../../gpu-and-cuda.md).

## Upstream

`github.com/usefulsensors/moonshine`; ONNX export at
`huggingface.co/onnx-community/moonshine-base-ONNX`.
