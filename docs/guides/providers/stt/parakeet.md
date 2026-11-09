# Parakeet TDT (STT, local)

**Status:** active
**Last verified:** 2026-10-13
**Owner:** Docs

## What it is

NVIDIA Parakeet TDT 0.6B v2, a FastConformer encoder with a
token-and-duration transducer decoder, run in-process through ONNX Runtime
from an int8-quantised export. The default STT provider: accurate English
recognition with word-level timestamps and no cloud dependency.

## Cargo feature

`stt-parakeet` (in the default set), which links the plugin crate
`voice-pipes-stt-parakeet`. The binary registers it as the recogniser a
pipeline with no `[stt]` section runs. GPU: add `ort-cuda` and run
with `VP_ORT_EP=cuda`.

## Config

```toml
[stt]
provider = "parakeet-tdt"
model_dir = "models/parakeet-tdt"

[stt.options]
# precision = "int8"          # which export to load: "int8" (default) or "fp32"
# language = "en-US"          # the language reported on results (a label, not a switch)
# pool_size = 2               # concurrent inference sessions for multi-session workers
# initial_buffer_ms = 200     # audio buffered before the first inference pass
# inference_step_ms = 80      # pacing between passes
# streaming_window_ms = 8000  # bounded re-decode window
```

`model_dir` holds `nemo128.onnx`, `encoder-model.int8.onnx`,
`decoder_joint-model.int8.onnx` and `vocab.txt`, from the
`istupakov/parakeet-tdt-0.6b-v2-onnx` Hugging Face repository. The wizard
downloads them.

`precision = "fp32"` loads `encoder-model.onnx` and
`decoder_joint-model.onnx` instead of the int8 pair. The fp32 encoder keeps
its weights in `encoder-model.onnx.data` beside the graph, which ONNX
Runtime finds on its own; keep the two together. The preprocessor and
vocabulary are the same files for both exports.

`language` is what results carry as their language tag; a request that
names a language overrides it. The model has no language head, so this is
the operator's statement of what the export was trained on, not a switch.
For `el` (Greek) the provider also writes the Greek question mark `;` where
the vocabulary's Latin `?` came out.

## Environment

`VP_ORT_EP=cuda` runs the sessions on the GPU in a build with `ort-cuda`;
asked for in a build without it, the sessions stay on the CPU and the
device report says so.

## Capabilities

| | |
|---|---|
| Input sample rate | 16 kHz (the pipeline resamples) |
| Streaming | Pseudo-streaming: an incrementally grown buffer is re-decoded per inference pass; interim results |
| Word timestamps | Yes, from the TDT duration tokens |
| End-of-turn signal | No; pair with a real VAD and `silence` or `smart` turn detection |
| Pooling | `pool_size` sessions behind a semaphore |
| Language | English (v2, the default export); 25 European languages with the v3 export below; Greek with the fine-tune below |

## Other languages: the v3 export

`istupakov/parakeet-tdt-0.6b-v3-onnx` is the multilingual checkpoint (25
European languages, including Croatian) in the same four-file layout, so it
needs no code change — download it into its own directory and point
`model_dir` at it. It is a larger vocabulary (8193 tokens against 1025), and
its int8 encoder is 652 MB.

Accuracy depends heavily on the recording. Measured with the corpus scorer
([benchmarking guide](../../benchmarking-and-evals.md#recognition-accuracy-on-a-corpus))
on a 9-entry Croatian corpus:

| Audio | v3 mean WER | v2 mean WER |
|---|---|---|
| Clean synthesised speech | 0.13 | 1.00 |
| Real browser-microphone turns | 1.85 | 1.06 |

So v3 does speak the language, and a close, clean signal is not optional: on
laptop-microphone audio captured through a browser (energy concentrated
below 1 kHz) it returns empty or nonsense text where a large cloud model
still transcribes correctly. Test your own recordings with the corpus scorer
before choosing it over a cloud provider.

## Greek: the fine-tuned export

`myned-ai/parakeet-tdt-0.6b-v3-greek` is v3 fine-tuned on 2,087 hours of
Greek, in the same layout plus the fp32 export. Its model card measures the
int8 export at 4 to 8 WER points worse than fp32 on Greek with no speed
gain on CPU, so load it at full precision:

```toml
[stt]
provider  = "parakeet-tdt"
model_dir = "models/parakeet-tdt-v3-greek"

[stt.options]
precision = "fp32"
language  = "el"
pool_size = 1   # each fp32 session set holds about 2.5 GB of weights
```

`voice-pipes models fetch parakeet-greek` downloads the five files:
`nemo128.onnx`,
`encoder-model.onnx`, `encoder-model.onnx.data`, `decoder_joint-model.onnx`
and `vocab.txt`, all from that repository. The vocabulary is the v3 one plus
33 Greek pieces (8,226 entries including `<blk>`); the decoder reads its
size from the file.

A ready-made Docker cascade, Greek in and out, is
`deploy/docker/docker-compose.greek.yml` with
`config/pipeline-greek-parakeet.toml`: this recogniser, local Gemma 4 and
the Greek Pocket-TTS (`myned-ai/pocket-tts-greek`) served on the host.

Reported WER on the model card, lower is better:

| Export | FLEURS Greek | Common Voice Greek | Cypriot |
|---|---|---|---|
| fp32 | 11.4% | 8.9% | 25.9% |
| int8 | 18.7% | 13.1% | 33.8% |
| base v3, no fine-tune | 33.6% | 22.9% | 65.0% |

## Limitations

- The default export is English only. For other languages use the v3 export
  or the Greek fine-tune above, [Google Cloud STT](google.md), or
  [Nemotron 3.5](nemotron-3.5.md) where its language list fits.
- Pseudo-streaming re-decodes the growing utterance buffer, so the cost of
  an inference pass grows with utterance length; a bounded window keeps it
  in check. For long dictation-style input prefer a true streaming model.
- Runs comfortably on CPU for one or two concurrent sessions; raise
  `pool_size` on larger machines or move it behind [gRPC split mode](../../../extending/grpc-split-mode.md)
  on a GPU.

## Upstream

NVIDIA's model card for `nvidia/parakeet-tdt-0.6b-v2`; the ONNX export by
`istupakov`.
