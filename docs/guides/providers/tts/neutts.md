# NeuTTS (TTS, local)

**Status:** active  
**Owner:** Docs  

## What it is

NeuTTS-Air is a zero-shot voice cloning and text-to-speech engine that pairs a quantized Qwen2 GGUF acoustic language model with an ONNX-based NeuCodec neural vocoder. It produces high-quality, expressive 24 kHz audio and runs completely in-process on CPU (or GPU) without any cloud dependencies.

It is implemented as a plugin crate, `voice-pipes-tts-neutts`, built on `voice-pipes-sdk`. The `voice-pipes` binary links it behind the `tts-neutts` Cargo feature.

## Cargo Feature

`tts-neutts` on the **CLI crate**:
```sh
cargo build -p voice-pipes-cli --features tts-neutts
```

For ONNX CUDA acceleration on the vocoder, enable `ort-cuda` on the CLI crate and run with `VP_ORT_EP=cuda`.

> [!NOTE]
> Building `tts-neutts` (or the default `llm-gguf` feature) compiles embedded `llama.cpp` through `llama-cpp-sys-2`. This requires **CMake 3.16+** and **LLVM / Clang (`libclang`)** on your host build environment.

## Config

```toml
[tts]
provider = "neutts"
model = "models/neutts/neutts-air-Q4_0.gguf"

[tts.options]
vocoder = "models/neutts/neucodec_decoder.onnx"
voice = "jo"           # built-in preset voice
threads = 4            # optional: CPU threads for acoustic token generation
pool_size = 1          # optional: concurrent synthesis session pool size
```

### Model Files

NeuTTS requires two files:
1. **Acoustic Backbone:** `neutts-air-Q4_0.gguf` (~400 MB)
2. **Neural Vocoder:** `neucodec_decoder.onnx` (~120 MB)

You can download them automatically via the configuration wizard (`launch-stack.ps1` / `launch-stack.sh` or `voice-pipes config-wizard`).

## Environment & Build Prerequisites

- **CMake 3.16+:** Required at build time to build embedded `llama.cpp`.
- **LLVM / libclang:** Required at build time by `bindgen` (ensure `LIBCLANG_PATH` is set on Windows if not detected automatically).
- **eSpeak NG:** Required at runtime for phonemization (`espeak-ng` on `PATH`).

## Custom Voice Cloning

In addition to built-in presets (e.g. `voice = "jo"`), you can clone any custom speaker by pointing `[tts.options]` to a clean 3–15 second reference WAV and its transcript:

```toml
[tts]
provider = "neutts"
model = "models/neutts/neutts-air-Q4_0.gguf"

[tts.options]
vocoder = "models/neutts/neucodec_decoder.onnx"
reference_audio = "models/neutts/custom_ref.wav"
reference_text = "The quick brown fox jumps over the lazy dog."
```

## Capabilities

| Capability | Specification |
|---|---|
| Native Output Sample Rate | 24 kHz (resampled to 16 kHz for pipeline) |
| Streaming | Token-by-token acoustic generation with chunked vocoding |
| Zero-shot Cloning | Yes (from 3–15s reference audio + transcript) |
| Runtime Engine | Embedded `llama.cpp` + ONNX Runtime vocoder |
| Word Timestamps | No |

## Limitations

- Synthesis latency is compute-dependent: multi-threading (`threads = 4..8`) is recommended on CPU for conversational real-time factor (RTF < 0.8).
- Requires English phonemization via eSpeak NG for the input text.
